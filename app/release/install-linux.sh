#!/bin/sh
set -eu

REPOSITORY="${OCTOPUS_REPOSITORY:-miuuyy/Octopus-SI-OS}"
VERSION="${OCTOPUS_VERSION:-}"
BASE_URL_OVERRIDE="${OCTOPUS_RELEASE_BASE_URL:-}"
NODE_BIN="${OCTOPUS_NODE:-node}"
MIN_FREE_MB="${OCTOPUS_MIN_FREE_MB:-1536}"

if [ "$(id -u)" -eq 0 ]; then
  DEFAULT_PREFIX="/usr/local/lib/octopus"
  DEFAULT_BIN_DIR="/usr/local/bin"
else
  DEFAULT_PREFIX="${HOME:-/root}/.local/share/octopus"
  DEFAULT_BIN_DIR="${HOME:-/root}/.local/bin"
fi

PREFIX="${OCTOPUS_INSTALL_DIR:-$DEFAULT_PREFIX}"
BIN_DIR="${OCTOPUS_BIN_DIR:-$DEFAULT_BIN_DIR}"
CHECK_ONLY=0
JSON_OUTPUT=0
NO_SYMLINK=0

usage() {
  cat <<'EOF'
Usage: install-linux.sh [OPTIONS]

Install or preflight-check the headless Octopus CLI release on Linux x86_64.

Options:
  --check            Read-only host preflight check (mutates nothing)
  --json             Emit machine-readable JSON output
  --prefix <dir>     Install directory (env: OCTOPUS_INSTALL_DIR)
                     Default: $HOME/.local/share/octopus (non-root)
                              /usr/local/lib/octopus (root)
  --bin-dir <dir>    Directory for the octopus CLI symlink (env: OCTOPUS_BIN_DIR)
                     Default: $HOME/.local/bin (non-root)
                              /usr/local/bin (root)
  --version <ver>    Release version to install, e.g. 0.1.0 (env: OCTOPUS_VERSION)
  --no-symlink       Do not create or update the octopus symlink in --bin-dir
  -h, --help         Show this help message

Environment variables:
  OCTOPUS_INSTALL_DIR        Override --prefix
  OCTOPUS_BIN_DIR            Override --bin-dir
  OCTOPUS_VERSION            Override --version
  OCTOPUS_REPOSITORY         GitHub repo owner/name (default: miuuyy/Octopus-SI-OS)
  OCTOPUS_RELEASE_BASE_URL   Base URL for release assets (mirrors / offline testing)
  OCTOPUS_NODE               Node.js 24+ binary path or name (default: node)
  OCTOPUS_MIN_FREE_MB        Minimum free MiB required on target filesystem (default: 1536)
EOF
}

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

json_escape() {
  printf '%s' "$1" | awk '
    {
      if (NR > 1) printf "\\n"
      n = length($0)
      for (i = 1; i <= n; i++) {
        c = substr($0, i, 1)
        if (c == "\\") printf "\\\\"
        else if (c == "\"") printf "\\\""
        else if (c == "\r") printf "\\r"
        else if (c == "\t") printf "\\t"
        else printf "%s", c
      }
    }'
}

normalize_path() {
  p="$1"
  [ -n "$p" ] || fail "Path option cannot be empty."
  case "$p" in
    /*) ;;
    *) p="$(pwd)/$p" ;;
  esac
  while [ "$p" != "/" ]; do
    case "$p" in
      */) p="${p%/}" ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$p"
}

nearest_existing_dir() {
  p="$1"
  while [ -n "$p" ] && [ "$p" != "/" ] && [ ! -e "$p" ] && [ ! -L "$p" ]; do
    p="$(dirname -- "$p")"
  done
  [ -n "$p" ] || p="/"
  printf '%s\n' "$p"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check)
      CHECK_ONLY=1
      shift
      ;;
    --json)
      JSON_OUTPUT=1
      shift
      ;;
    --no-symlink)
      NO_SYMLINK=1
      shift
      ;;
    --prefix)
      [ "$#" -ge 2 ] || { printf '%s\n' "Missing value for --prefix" >&2; exit 2; }
      PREFIX="$2"
      shift 2
      ;;
    --prefix=*)
      PREFIX="${1#--prefix=}"
      shift
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || { printf '%s\n' "Missing value for --bin-dir" >&2; exit 2; }
      BIN_DIR="$2"
      shift 2
      ;;
    --bin-dir=*)
      BIN_DIR="${1#--bin-dir=}"
      shift
      ;;
    --version)
      [ "$#" -ge 2 ] || { printf '%s\n' "Missing value for --version" >&2; exit 2; }
      VERSION="$2"
      shift 2
      ;;
    --version=*)
      VERSION="${1#--version=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown option: %s\n\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

PREFIX="$(normalize_path "$PREFIX")"
BIN_DIR="$(normalize_path "$BIN_DIR")"
[ "$PREFIX" != "/" ] || fail "Refusing to use '/' as --prefix."
[ "$BIN_DIR" != "/" ] || fail "Refusing to use '/' as --bin-dir."

case "$MIN_FREE_MB" in
  ''|*[!0-9]*) fail "Invalid OCTOPUS_MIN_FREE_MB: $MIN_FREE_MB" ;;
esac

# Detect host tools without mutating anything.
HOST_OS="$(uname -s 2>/dev/null || printf 'unknown')"
HOST_ARCH="$(uname -m 2>/dev/null || printf 'unknown')"
HOST_KERNEL="$(uname -r 2>/dev/null || printf 'unknown')"
RELEASE_ARCH=""
case "$HOST_ARCH" in
  x86_64|amd64) RELEASE_ARCH="x64" ;;
esac

DOWNLOADER=""
if command -v curl >/dev/null 2>&1; then
  DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER="wget"
fi

SHA256_TOOL=""
if command -v sha256sum >/dev/null 2>&1; then
  SHA256_TOOL="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHA256_TOOL="shasum"
fi

HARD_FAILURES=0
WARNINGS=0
CHECKS_JSON=""
TEXT_REPORT=""

record_check() {
  chk_id="$1"
  chk_level="$2"
  chk_status="$3"
  chk_detail="$4"
  chk_remediation="$5"

  if [ "$chk_status" = "fail" ] && [ "$chk_level" = "required" ]; then
    HARD_FAILURES=$((HARD_FAILURES + 1))
  elif [ "$chk_status" = "warn" ]; then
    WARNINGS=$((WARNINGS + 1))
  fi

  case "$chk_status" in
    ok) tag="OK" ;;
    warn) tag="WARN" ;;
    *) tag="FAIL" ;;
  esac
  line="$(printf '[%s] (%s) %s: %s' "$tag" "$chk_level" "$chk_id" "$chk_detail")"
  if [ -n "$chk_remediation" ]; then
    line="$(printf '%s\n       -> %s' "$line" "$chk_remediation")"
  fi
  if [ -z "$TEXT_REPORT" ]; then
    TEXT_REPORT="$line"
  else
    TEXT_REPORT="$(printf '%s\n%s' "$TEXT_REPORT" "$line")"
  fi

  entry="$(printf '{"id":"%s","level":"%s","status":"%s","detail":"%s","remediation":"%s"}' \
    "$(json_escape "$chk_id")" \
    "$(json_escape "$chk_level")" \
    "$(json_escape "$chk_status")" \
    "$(json_escape "$chk_detail")" \
    "$(json_escape "$chk_remediation")")"
  if [ -z "$CHECKS_JSON" ]; then
    CHECKS_JSON="$entry"
  else
    CHECKS_JSON="$CHECKS_JSON,$entry"
  fi
}

check_symlink_target_safe() {
  link_path="$1"
  if [ -L "$link_path" ]; then
    target="$(readlink "$link_path" 2>/dev/null || true)"
    case "$target" in
      "$PREFIX/octopus"|*/octopus|octopus) return 0 ;;
      *) return 1 ;;
    esac
  fi
  if [ -e "$link_path" ]; then
    return 1
  fi
  return 0
}

run_preflight() {
  # 1. OS
  if [ "$HOST_OS" = "Linux" ]; then
    record_check "os" "required" "ok" "Linux ($HOST_KERNEL)" ""
  else
    record_check "os" "required" "fail" "Detected $HOST_OS (expected Linux)" \
      "Run app/release/install.sh on macOS, or run install-linux.sh on a Linux x86_64 host."
  fi

  # 2. Architecture
  if [ -n "$RELEASE_ARCH" ]; then
    record_check "arch" "required" "ok" "$HOST_ARCH (release arch: $RELEASE_ARCH)" ""
  else
    record_check "arch" "required" "fail" "Unsupported architecture: $HOST_ARCH" \
      "Prebuilt Linux headless releases currently target x86_64/amd64; build from source on other architectures (see docs/linux.md)."
  fi

  # 3. tar
  if command -v tar >/dev/null 2>&1; then
    record_check "tar" "required" "ok" "$(command -v tar)" ""
  else
    record_check "tar" "required" "fail" "tar not found on PATH" \
      "Install tar (e.g. sudo apt-get install tar)."
  fi

  # 4. curl or wget
  if [ -n "$DOWNLOADER" ]; then
    record_check "downloader" "required" "ok" "$DOWNLOADER ($(command -v "$DOWNLOADER"))" ""
  else
    record_check "downloader" "required" "fail" "Neither curl nor wget found on PATH" \
      "Install curl or wget (e.g. sudo apt-get install curl)."
  fi

  # 5. sha256sum or shasum
  if [ -n "$SHA256_TOOL" ]; then
    record_check "sha256" "required" "ok" "$SHA256_TOOL ($(command -v "$SHA256_TOOL"))" ""
  else
    record_check "sha256" "required" "fail" "Neither sha256sum nor shasum found on PATH" \
      "Install coreutils (sha256sum) or perl (shasum)."
  fi

  # 6. Node.js 24+ (required at runtime: Linux tarball ships pure-JS CLI and delegates to host node)
  if command -v "$NODE_BIN" >/dev/null 2>&1; then
    node_raw="$("$NODE_BIN" --version 2>/dev/null | head -n 1 || true)"
    node_ver="${node_raw#v}"
    node_major="${node_ver%%.*}"
    case "$node_major" in
      ''|*[!0-9]*)
        record_check "node" "required" "fail" "Could not parse Node.js version from '$NODE_BIN --version' (got '${node_raw:-empty}')" \
          "Install Node.js 24+ and ensure it is on PATH or set OCTOPUS_NODE."
        ;;
      *)
        if [ "$node_major" -ge 24 ]; then
          record_check "node" "required" "ok" "$node_raw ($(command -v "$NODE_BIN"))" ""
        else
          record_check "node" "required" "fail" "Node.js $node_raw is below required v24+" \
            "Install Node.js 24+ (note: Ubuntu 26.04 apt nodejs ships v22; use nodejs.org, NodeSource, or a version manager and set OCTOPUS_NODE if outside PATH)."
        fi
        ;;
    esac
  else
    record_check "node" "required" "fail" "Node.js executable '$NODE_BIN' not found" \
      "Install Node.js 24+ (required by the Linux headless CLI) and ensure it is on PATH or set OCTOPUS_NODE."
  fi

  # 7. Writable --prefix (and its parent for same-filesystem atomic swap)
  prefix_parent="$(dirname -- "$PREFIX")"
  prefix_parent_anc="$(nearest_existing_dir "$prefix_parent")"
  if [ -e "$PREFIX" ] && [ ! -d "$PREFIX" ]; then
    record_check "prefix_writable" "required" "fail" "$PREFIX exists and is not a directory" \
      "Choose a different --prefix or remove the conflicting file."
  elif [ ! -d "$prefix_parent_anc" ] || [ ! -w "$prefix_parent_anc" ]; then
    record_check "prefix_writable" "required" "fail" "Target parent $prefix_parent_anc is not writable for $PREFIX" \
      "Use a writable --prefix (e.g. --prefix \"\$HOME/.local/share/octopus\") or fix directory permissions."
  elif [ -d "$PREFIX" ] && [ ! -w "$PREFIX" ]; then
    record_check "prefix_writable" "required" "fail" "Existing install directory $PREFIX is not writable" \
      "Fix permissions on $PREFIX or choose a writable --prefix."
  else
    record_check "prefix_writable" "required" "ok" "$PREFIX (parent ancestor $prefix_parent_anc is writable)" ""
  fi

  # 8. Writable --bin-dir & non-clobbering symlink check
  if [ "$NO_SYMLINK" -eq 1 ]; then
    record_check "bin_dir" "required" "ok" "Skipped (--no-symlink)" ""
  else
    bin_anc="$(nearest_existing_dir "$BIN_DIR")"
    if [ -e "$BIN_DIR" ] && [ ! -d "$BIN_DIR" ]; then
      record_check "bin_dir" "required" "fail" "$BIN_DIR exists and is not a directory" \
        "Choose a different --bin-dir or pass --no-symlink."
    elif [ ! -d "$bin_anc" ] || [ ! -w "$bin_anc" ]; then
      record_check "bin_dir" "required" "fail" "Target bin directory ancestor $bin_anc is not writable for $BIN_DIR" \
        "Use a writable --bin-dir (e.g. --bin-dir \"\$HOME/.local/bin\") or pass --no-symlink."
    elif ! check_symlink_target_safe "$BIN_DIR/octopus"; then
      record_check "bin_dir" "required" "fail" "$BIN_DIR/octopus already exists and is not an Octopus symlink" \
        "Move the existing $BIN_DIR/octopus, choose a different --bin-dir, or pass --no-symlink."
    else
      record_check "bin_dir" "required" "ok" "$BIN_DIR (writable; octopus link slot available)" ""
    fi
  fi

  # 9. Free disk space on target filesystem
  free_kb="$(df -Pk "$prefix_parent_anc" 2>/dev/null | awk 'NR==2 { print $4 }' || true)"
  case "$free_kb" in
    ''|*[!0-9]*)
      record_check "disk_space" "required" "fail" "Could not determine free disk space for $prefix_parent_anc" \
        "Verify filesystem health and df availability on $prefix_parent_anc."
      ;;
    *)
      free_mb=$((free_kb / 1024))
      if [ "$free_mb" -ge "$MIN_FREE_MB" ]; then
        record_check "disk_space" "required" "ok" "${free_mb} MiB available on $prefix_parent_anc (minimum ${MIN_FREE_MB} MiB)" ""
      else
        record_check "disk_space" "required" "fail" "Only ${free_mb} MiB available on $prefix_parent_anc (minimum ${MIN_FREE_MB} MiB)" \
          "Free at least ${MIN_FREE_MB} MiB on the target filesystem before installing (or adjust OCTOPUS_MIN_FREE_MB)."
      fi
      ;;
  esac

  # Optional / recommended checks (never fail --check on their own)
  # 10. Kernel 6.5+ (recommended for AppArmor 4.0 + SO_PEERPIDFD confinement)
  k_major="${HOST_KERNEL%%.*}"
  k_rest="${HOST_KERNEL#*.}"
  k_minor="${k_rest%%.*}"
  k_minor="${k_minor%%-*}"
  case "$k_major:$k_minor" in
    :*|*:|*[!0-9]*:*|*:*[!0-9]*)
      record_check "kernel" "recommended" "warn" "Could not parse Linux kernel version '$HOST_KERNEL'" \
        "Linux 6.5+ is recommended for enforced AppArmor 4.0 + cgroup v2 process isolation."
      ;;
    *)
      if [ "$k_major" -gt 6 ] || { [ "$k_major" -eq 6 ] && [ "$k_minor" -ge 5 ]; }; then
        record_check "kernel" "recommended" "ok" "Linux $HOST_KERNEL (>= 6.5)" ""
      else
        record_check "kernel" "recommended" "warn" "Linux $HOST_KERNEL is older than 6.5" \
          "Upgrade to Linux 6.5+ for enforced AppArmor 4.0 + SO_PEERPIDFD process isolation."
      fi
      ;;
  esac

  # 11. systemd availability
  if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    record_check "systemd" "recommended" "ok" "systemd active ($(command -v systemctl))" ""
  elif command -v systemctl >/dev/null 2>&1; then
    record_check "systemd" "recommended" "warn" "systemctl is installed, but systemd is not PID 1 (/run/systemd/system absent)" \
      "Headless CLI runs manually without systemd; use a systemd-booted host for service supervision (see docs/linux.md)."
  else
    record_check "systemd" "recommended" "warn" "systemctl not found" \
      "Optional for install; needed for systemd service supervision and octopus-process-authority.service (see docs/linux.md)."
  fi

  # 12. AppArmor + cgroup v2 host prerequisites
  aa_missing=""
  if [ ! -r /sys/module/apparmor/parameters/enabled ] || [ "$(cat /sys/module/apparmor/parameters/enabled 2>/dev/null || true)" != "Y" ]; then
    aa_missing="${aa_missing}AppArmor kernel module, "
  fi
  if [ ! -x /usr/sbin/apparmor_parser ]; then
    aa_missing="${aa_missing}/usr/sbin/apparmor_parser, "
  fi
  if [ ! -f /etc/apparmor.d/abi/4.0 ]; then
    aa_missing="${aa_missing}/etc/apparmor.d/abi/4.0, "
  fi
  if [ ! -f /sys/fs/cgroup/cgroup.controllers ]; then
    aa_missing="${aa_missing}cgroup v2, "
  fi
  if [ -z "$aa_missing" ]; then
    record_check "apparmor" "recommended" "ok" "AppArmor enabled with abi/4.0, apparmor_parser, and cgroup v2" ""
  else
    aa_missing="${aa_missing%, }"
    record_check "apparmor" "recommended" "warn" "Missing for enforced isolation: $aa_missing" \
      "Optional for install; for enforced process isolation install AppArmor 4+ on a cgroup v2 host (e.g. sudo apt-get install apparmor)."
  fi

  # 13. octopus-process-authority service status
  user_in_authority_group=0
  if [ "$(id -u)" -eq 0 ] || id -Gn 2>/dev/null | tr ' ' '\n' | grep -qx "octopus-process"; then
    user_in_authority_group=1
  fi
  authority_service_active=0
  if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet octopus-process-authority.service >/dev/null 2>&1; then
    authority_service_active=1
  fi
  authority_dir_inaccessible=0
  if [ -d /run/octopus-process-authority ] && { [ ! -r /run/octopus-process-authority ] || [ ! -x /run/octopus-process-authority ]; }; then
    if { command -v getent >/dev/null 2>&1 && getent group octopus-process >/dev/null 2>&1; } \
      || grep -q '^octopus-process:' /etc/group 2>/dev/null; then
      authority_dir_inaccessible=1
    fi
  fi

  if [ -S /run/octopus-process-authority/control.sock ] && [ -x /usr/libexec/octopus/octopus-linux-process-authority ]; then
    if [ "$user_in_authority_group" -eq 1 ]; then
      record_check "process_authority" "recommended" "ok" "octopus-process-authority socket active and accessible" ""
    else
      record_check "process_authority" "recommended" "warn" "octopus-process-authority is running, but current user lacks access to /run/octopus-process-authority/control.sock (not in group 'octopus-process')" \
        "Add your user to octopus-process (sudo usermod -aG octopus-process \"\$USER\") and start a new login session, or re-run: sudo sh \"$PREFIX/linux-process-authority/install-linux.sh\" \"$PREFIX/app/build/cli/runtime/bin/octopus-linux-process-authority\" \"\$USER\""
    fi
  elif [ "$authority_service_active" -eq 1 ] || [ "$authority_dir_inaccessible" -eq 1 ]; then
    if [ "$user_in_authority_group" -eq 0 ]; then
      record_check "process_authority" "recommended" "warn" "octopus-process-authority is running, but current user lacks access to /run/octopus-process-authority/control.sock (not in group 'octopus-process')" \
        "Add your user to octopus-process (sudo usermod -aG octopus-process \"\$USER\") and start a new login session, or re-run: sudo sh \"$PREFIX/linux-process-authority/install-linux.sh\" \"$PREFIX/app/build/cli/runtime/bin/octopus-linux-process-authority\" \"\$USER\""
    else
      record_check "process_authority" "recommended" "warn" "octopus-process-authority.service is active, but /run/octopus-process-authority/control.sock or /usr/libexec/octopus/octopus-linux-process-authority is unavailable" \
        "Check 'systemctl status octopus-process-authority.service' or re-run: sudo sh \"$PREFIX/linux-process-authority/install-linux.sh\" \"$PREFIX/app/build/cli/runtime/bin/octopus-linux-process-authority\" \"\$USER\""
    fi
  else
    record_check "process_authority" "recommended" "warn" "octopus-process-authority is not installed/running" \
      "Optional for install; after installing, enable enforced isolation with: sudo sh \"$PREFIX/linux-process-authority/install-linux.sh\" \"$PREFIX/app/build/cli/runtime/bin/octopus-linux-process-authority\" \"\$USER\""
  fi
}

print_preflight_result() {
  if [ "$JSON_OUTPUT" -eq 1 ]; then
    ok_bool="false"
    [ "$HARD_FAILURES" -eq 0 ] && ok_bool="true"
    sym_bool="true"
    [ "$NO_SYMLINK" -eq 1 ] && sym_bool="false"
    printf '{"ok":%s,"mode":"check","os":"%s","arch":"%s","release_arch":"%s","prefix":"%s","bin_dir":"%s","symlink_enabled":%s,"hard_failures":%d,"warnings":%d,"checks":[%s]}\n' \
      "$ok_bool" \
      "$(json_escape "$HOST_OS")" \
      "$(json_escape "$HOST_ARCH")" \
      "$(json_escape "$RELEASE_ARCH")" \
      "$(json_escape "$PREFIX")" \
      "$(json_escape "$BIN_DIR")" \
      "$sym_bool" \
      "$HARD_FAILURES" \
      "$WARNINGS" \
      "$CHECKS_JSON"
  else
    printf 'Octopus Linux headless host preflight\n'
    printf '%s\n' "$TEXT_REPORT"
    if [ "$HARD_FAILURES" -eq 0 ]; then
      printf '\nPreflight passed (%d optional warning(s)). Target prefix: %s\n' "$WARNINGS" "$PREFIX"
    else
      printf '\nPreflight failed with %d blocking issue(s) (%d optional warning(s)).\n' "$HARD_FAILURES" "$WARNINGS" >&2
    fi
  fi
}

run_preflight

if [ "$CHECK_ONLY" -eq 1 ]; then
  print_preflight_result
  [ "$HARD_FAILURES" -eq 0 ] && exit 0 || exit 1
fi

if [ "$HARD_FAILURES" -ne 0 ]; then
  print_preflight_result
  exit 1
fi

download_file() {
  url="$1"
  dest="$2"
  max_time="${3:-3600}"
  if [ "$DOWNLOADER" = "curl" ]; then
    curl -fsSL --retry 3 --connect-timeout 15 --max-time "$max_time" "$url" -o "$dest"
  else
    wget -q --tries=3 --timeout=30 -O "$dest" "$url"
  fi
}

compute_sha256() {
  if [ "$SHA256_TOOL" = "sha256sum" ]; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

resolve_latest_version() {
  if [ "$DOWNLOADER" = "curl" ]; then
    latest="$(curl -fsSLI --retry 3 --connect-timeout 15 --max-time 60 -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPOSITORY/releases/latest")" || fail "Could not reach the releases of $REPOSITORY."
  else
    latest="$(wget --spider --max-redirect=10 -S "https://github.com/$REPOSITORY/releases/latest" 2>&1 \
      | awk '/^[[:space:]]*[Ll]ocation:/ { loc = $2 } END { gsub(/\r/, "", loc); print loc }')"
    [ -n "$latest" ] || fail "Could not reach the releases of $REPOSITORY."
  fi
  resolved="${latest##*/tag/}"
  [ "$resolved" != "$latest" ] || fail "Could not resolve the latest Octopus release of $REPOSITORY."
  printf '%s\n' "$resolved"
}

printf '%s\n' "$REPOSITORY" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || fail "Invalid GitHub repository: $REPOSITORY"

if [ -z "$VERSION" ]; then
  if [ -n "$BASE_URL_OVERRIDE" ]; then
    fail "OCTOPUS_RELEASE_BASE_URL requires --version or OCTOPUS_VERSION."
  fi
  VERSION="$(resolve_latest_version)"
fi
VERSION="${VERSION#v}"
case "$VERSION" in
  ''|*[!0-9.]*) fail "Invalid Octopus release version: $VERSION" ;;
esac

STEM="Octopus-$VERSION-linux-$RELEASE_ARCH"
ASSET="$STEM.tar.gz"
if [ -n "$BASE_URL_OVERRIDE" ]; then
  BASE_URL="${BASE_URL_OVERRIDE%/}"
else
  BASE_URL="https://github.com/$REPOSITORY/releases/download/v$VERSION"
fi

TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/octopus-linux-install.XXXXXX")"
STAGE_DIR=""
TEMP_LINK=""
cleanup() {
  if [ -n "$TEMP_LINK" ] && [ -L "$TEMP_LINK" ]; then rm -f "$TEMP_LINK" || true; fi
  if [ -n "$STAGE_DIR" ] && [ -e "$STAGE_DIR" ]; then rm -rf "$STAGE_DIR" || true; fi
  rm -rf "$TEMP_DIR" || true
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

EXPECTED=""
if download_file "$BASE_URL/checksums.txt" "$TEMP_DIR/checksums.txt" 60 2>/dev/null; then
  EXPECTED="$(awk -v asset="$ASSET" '{ name = $2; sub(/^\*/, "", name); if (name == asset) { print $1; exit } }' "$TEMP_DIR/checksums.txt")"
fi
if [ -z "$EXPECTED" ] && download_file "$BASE_URL/$ASSET.sha256" "$TEMP_DIR/$ASSET.sha256" 60 2>/dev/null; then
  EXPECTED="$(awk -v asset="$ASSET" '{ name = $2; sub(/^\*/, "", name); if (name == asset) { print $1; exit } }' "$TEMP_DIR/$ASSET.sha256")"
fi
EXPECTED="$(printf '%s' "$EXPECTED" | LC_ALL=C tr 'A-F' 'a-f')"
printf '%s\n' "$EXPECTED" | grep -Eq '^[0-9a-f]{64}$' \
  || fail "Could not obtain a valid SHA-256 checksum for $ASSET from $BASE_URL."

if [ "$JSON_OUTPUT" -eq 0 ]; then
  printf 'Downloading %s\n' "$ASSET"
fi
download_file "$BASE_URL/$ASSET" "$TEMP_DIR/$ASSET" 3600 \
  || fail "Could not download $ASSET from $BASE_URL."

ACTUAL="$(compute_sha256 "$TEMP_DIR/$ASSET" | LC_ALL=C tr 'A-F' 'a-f')"
[ "$ACTUAL" = "$EXPECTED" ] \
  || fail "SHA-256 verification failed for $ASSET (expected $EXPECTED, got $ACTUAL). Existing installation was not modified."

PREFIX_PARENT="$(dirname -- "$PREFIX")"
mkdir -p "$PREFIX_PARENT"
STAGE_DIR="$(mktemp -d "$PREFIX_PARENT/.octopus-install-stage.XXXXXX")"

if [ "$JSON_OUTPUT" -eq 0 ]; then
  printf 'Extracting %s into staging directory on %s\n' "$ASSET" "$PREFIX_PARENT"
fi
tar -xzf "$TEMP_DIR/$ASSET" -C "$STAGE_DIR" \
  || fail "Failed to extract $ASSET. Existing installation was not modified."

STAGE_PAYLOAD="$STAGE_DIR/$STEM"
[ -d "$STAGE_PAYLOAD" ] || fail "Archive $ASSET does not contain expected top-level directory $STEM."
[ -x "$STAGE_PAYLOAD/octopus" ] || fail "Archive $ASSET is missing executable octopus launcher."
[ -f "$STAGE_PAYLOAD/app/cli/index.cjs" ] || fail "Archive $ASSET is missing app/cli/index.cjs."
[ -f "$STAGE_PAYLOAD/app/build/cli/runtime/runtime-build-receipt.json" ] || fail "Archive $ASSET is missing runtime-build-receipt.json."
[ -x "$STAGE_PAYLOAD/app/build/cli/runtime/bin/octopus" ] || fail "Archive $ASSET is missing runtime binary."

if ! OCTOPUS_NODE="$NODE_BIN" "$STAGE_PAYLOAD/octopus" --version >/dev/null 2>&1; then
  fail "Staged launcher failed '$STAGE_PAYLOAD/octopus --version'; existing installation was not modified."
fi

if [ "$NO_SYMLINK" -eq 0 ]; then
  check_symlink_target_safe "$BIN_DIR/octopus" \
    || fail "$BIN_DIR/octopus already exists and is not an Octopus symlink; pass --no-symlink or a different --bin-dir."
fi

# Atomic swap on the same filesystem with rollback on failure.
if [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; then
  BACKUP_PARENT="$(mktemp -d "$PREFIX_PARENT/.octopus-install-backup.XXXXXX")"
  BACKUP_DIR="$BACKUP_PARENT/previous"
  mv "$PREFIX" "$BACKUP_DIR" || {
    rm -rf "$BACKUP_PARENT" || true
    fail "Could not move existing installation $PREFIX aside."
  }
  if ! mv "$STAGE_PAYLOAD" "$PREFIX"; then
    mv "$BACKUP_DIR" "$PREFIX" || true
    rm -rf "$BACKUP_PARENT" || true
    fail "Could not replace $PREFIX; previous installation was restored."
  fi
  rm -rf "$BACKUP_PARENT"
else
  mv "$STAGE_PAYLOAD" "$PREFIX"
fi
rm -rf "$STAGE_DIR"
STAGE_DIR=""

LINKED_PATH=""
if [ "$NO_SYMLINK" -eq 0 ]; then
  mkdir -p "$BIN_DIR"
  TEMP_LINK="$BIN_DIR/.octopus.link.$$"
  rm -f "$TEMP_LINK"
  ln -s "$PREFIX/octopus" "$TEMP_LINK"
  mv -f "$TEMP_LINK" "$BIN_DIR/octopus"
  TEMP_LINK=""
  LINKED_PATH="$BIN_DIR/octopus"
fi

if [ "$JSON_OUTPUT" -eq 1 ]; then
  printf '{"ok":true,"mode":"install","version":"%s","asset":"%s","sha256":"%s","prefix":"%s","launcher":"%s","symlink":"%s"}\n' \
    "$(json_escape "$VERSION")" \
    "$(json_escape "$ASSET")" \
    "$(json_escape "$ACTUAL")" \
    "$(json_escape "$PREFIX")" \
    "$(json_escape "$PREFIX/octopus")" \
    "$(json_escape "$LINKED_PATH")"
else
  printf 'Installed Octopus %s (Linux %s) at %s\n' "$VERSION" "$RELEASE_ARCH" "$PREFIX"
  if [ -n "$LINKED_PATH" ]; then
    printf 'Linked CLI at %s\n' "$LINKED_PATH"
    case ":${PATH:-}:" in
      *":$BIN_DIR:"*) ;;
      *) printf 'Note: add %s to PATH to run `octopus` directly.\n' "$BIN_DIR" ;;
    esac
  fi
  printf '\nNext steps:\n'
  if [ ! -S /run/octopus-process-authority/control.sock ]; then
    printf '  1. (Recommended) Install AppArmor process isolation:\n'
    printf '     sudo sh "%s/linux-process-authority/install-linux.sh" "%s/app/build/cli/runtime/bin/octopus-linux-process-authority" "$USER"\n' "$PREFIX" "$PREFIX"
    printf '     (Then open a new login session so the octopus-process group takes effect.)\n'
  fi
  printf '  - Initialize Home (headless, without starting a detached daemon):\n'
  if [ -n "$LINKED_PATH" ]; then
    printf '    %s setup --no-start\n' "$LINKED_PATH"
    printf '  - Verify installation and descriptors:\n'
    printf '    %s doctor\n' "$LINKED_PATH"
  else
    printf '    "%s/octopus" setup --no-start\n' "$PREFIX"
    printf '  - Verify installation and descriptors:\n'
    printf '    "%s/octopus" doctor\n' "$PREFIX"
  fi
  printf '  - Configure systemd supervision (octopus up --foreground): see docs/linux.md\n'
fi
