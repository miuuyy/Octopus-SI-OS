#!/bin/sh
set -eu

REPOSITORY="${OCTOPUS_REPOSITORY:-miuuyy/Octopus-SI-OS}"
VERSION="${OCTOPUS_VERSION:-}"
INSTALL_DIR="${OCTOPUS_APPLICATIONS_DIR:-}"

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

OS="$(uname -s)"
if [ "$OS" = "Linux" ]; then
  SCRIPT_DIR=""
  case "${0:-}" in
    ""|sh|-sh|*/sh|dash|-dash|*/dash|bash|-bash|*/bash) ;;
    *)
      if [ -f "$0" ]; then
        SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
      fi
      ;;
  esac
  if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/install-linux.sh" ]; then
    exec sh "$SCRIPT_DIR/install-linux.sh" "$@"
  fi

  printf '%s\n' "$REPOSITORY" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || fail "Invalid GitHub repository: $REPOSITORY"

  LINUX_VERSION="$VERSION"
  prev_arg=""
  for arg in "$@"; do
    if [ "$prev_arg" = "--version" ]; then
      LINUX_VERSION="$arg"
      prev_arg=""
      continue
    fi
    case "$arg" in
      --version) prev_arg="--version" ;;
      --version=*) LINUX_VERSION="${arg#--version=}" ;;
    esac
  done

  if command -v curl >/dev/null 2>&1; then
    LINUX_DL="curl"
  elif command -v wget >/dev/null 2>&1; then
    LINUX_DL="wget"
  else
    fail "Could not fetch install-linux.sh: neither curl nor wget is available."
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    LINUX_SHA="sha256sum"
  elif command -v shasum >/dev/null 2>&1; then
    LINUX_SHA="shasum"
  else
    fail "Could not verify install-linux.sh: neither sha256sum nor shasum is available."
  fi

  if [ -n "${OCTOPUS_RELEASE_BASE_URL:-}" ]; then
    LINUX_BASE_URL="${OCTOPUS_RELEASE_BASE_URL%/}"
  else
    if [ -z "$LINUX_VERSION" ]; then
      if [ "$LINUX_DL" = "curl" ]; then
        LATEST="$(curl -fsSLI --retry 3 --connect-timeout 15 --max-time 60 -o /dev/null -w '%{url_effective}' \
          "https://github.com/$REPOSITORY/releases/latest")" || fail "Could not reach the releases of $REPOSITORY."
      else
        LATEST="$(wget --spider --max-redirect=10 -S "https://github.com/$REPOSITORY/releases/latest" 2>&1 \
          | awk '/^[[:space:]]*[Ll]ocation:/ { loc = $2 } END { gsub(/\r/, "", loc); print loc }')"
        [ -n "$LATEST" ] || fail "Could not reach the releases of $REPOSITORY."
      fi
      LINUX_VERSION="${LATEST##*/tag/}"
      [ "$LINUX_VERSION" != "$LATEST" ] || fail "Could not resolve the latest Octopus release of $REPOSITORY."
    fi
    LINUX_VERSION="${LINUX_VERSION#v}"
    case "$LINUX_VERSION" in
      ''|*[!0-9.]*) fail "Invalid Octopus release version: $LINUX_VERSION" ;;
    esac
    LINUX_BASE_URL="https://github.com/$REPOSITORY/releases/download/v$LINUX_VERSION"
  fi

  LINUX_TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/octopus-linux-dispatch.XXXXXX")"
  cleanup_linux() {
    rm -rf "$LINUX_TEMP_DIR" || true
  }
  trap cleanup_linux EXIT
  trap 'exit 1' HUP INT TERM

  if [ "$LINUX_DL" = "curl" ]; then
    curl -fsSL --retry 3 --connect-timeout 15 --max-time 60 "$LINUX_BASE_URL/checksums.txt" -o "$LINUX_TEMP_DIR/checksums.txt" \
      || fail "Could not download checksums.txt from $LINUX_BASE_URL."
    curl -fsSL --retry 3 --connect-timeout 15 --max-time 60 "$LINUX_BASE_URL/install-linux.sh" -o "$LINUX_TEMP_DIR/install-linux.sh" \
      || fail "Could not download install-linux.sh from $LINUX_BASE_URL."
  else
    wget -q --tries=3 --timeout=30 -O "$LINUX_TEMP_DIR/checksums.txt" "$LINUX_BASE_URL/checksums.txt" \
      || fail "Could not download checksums.txt from $LINUX_BASE_URL."
    wget -q --tries=3 --timeout=30 -O "$LINUX_TEMP_DIR/install-linux.sh" "$LINUX_BASE_URL/install-linux.sh" \
      || fail "Could not download install-linux.sh from $LINUX_BASE_URL."
  fi

  EXPECTED_LINUX="$(awk '{ name = $2; sub(/^\*/, "", name); if (name == "install-linux.sh") { print $1; exit } }' "$LINUX_TEMP_DIR/checksums.txt" | LC_ALL=C tr 'A-F' 'a-f')"
  printf '%s\n' "$EXPECTED_LINUX" | grep -Eq '^[0-9a-f]{64}$' \
    || fail "Release checksums.txt has no valid SHA-256 entry for install-linux.sh."
  if [ "$LINUX_SHA" = "sha256sum" ]; then
    ACTUAL_LINUX="$(sha256sum "$LINUX_TEMP_DIR/install-linux.sh" | awk '{ print $1 }' | LC_ALL=C tr 'A-F' 'a-f')"
  else
    ACTUAL_LINUX="$(shasum -a 256 "$LINUX_TEMP_DIR/install-linux.sh" | awk '{ print $1 }' | LC_ALL=C tr 'A-F' 'a-f')"
  fi
  [ "$ACTUAL_LINUX" = "$EXPECTED_LINUX" ] || fail "SHA-256 verification failed for install-linux.sh."

  if [ -n "$LINUX_VERSION" ] && [ -z "${OCTOPUS_VERSION:-}" ]; then
    OCTOPUS_VERSION="$LINUX_VERSION" sh "$LINUX_TEMP_DIR/install-linux.sh" "$@"
  else
    sh "$LINUX_TEMP_DIR/install-linux.sh" "$@"
  fi
  exit $?
fi

[ "$OS" = "Darwin" ] || fail "Unsupported operating system: $OS. Octopus supports macOS 14+ and Linux x86_64."
case "$(uname -m)" in
  arm64) ARCH="arm64" ;;
  x86_64)
    if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then ARCH="arm64"; else ARCH="x64"; fi
    ;;
  *) fail "Unsupported Mac architecture: $(uname -m)" ;;
esac
MACOS_VERSION="$(sw_vers -productVersion)"
[ "${MACOS_VERSION%%.*}" -ge 14 ] || fail "Octopus needs macOS 14 or later; this Mac runs macOS $MACOS_VERSION."
xcode-select -p >/dev/null 2>&1 || fail "Octopus needs the Command Line Tools for Xcode, which provide python3 and git.
Install them with:  xcode-select --install
Then run this installer again."

printf '%s\n' "$REPOSITORY" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || fail "Invalid GitHub repository: $REPOSITORY"
if [ -z "$VERSION" ]; then
  LATEST="$(curl -fsSLI --retry 3 --connect-timeout 15 --max-time 60 -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPOSITORY/releases/latest")" || fail "Could not reach the releases of $REPOSITORY."
  VERSION="${LATEST##*/tag/}"
  [ "$VERSION" != "$LATEST" ] || fail "Could not resolve the latest Octopus release of $REPOSITORY."
fi
VERSION="${VERSION#v}"
case "$VERSION" in
  ''|*[!0-9.]*) fail "Invalid Octopus release version: $VERSION" ;;
esac

ASSET="Octopus-$VERSION-macos-$ARCH.dmg"
BASE_URL="${OCTOPUS_RELEASE_BASE_URL:-https://github.com/$REPOSITORY/releases/download/v$VERSION}"
TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/octopus-install.XXXXXX")"
MOUNT_DIR="$TEMP_DIR/volume"
MOUNTED=""
STAGE_APP=""
cleanup() {
  if [ -n "$MOUNTED" ]; then hdiutil detach -quiet "$MOUNT_DIR" || hdiutil detach -quiet -force "$MOUNT_DIR" || true; fi
  if [ -n "$STAGE_APP" ] && [ -e "$STAGE_APP" ]; then rm -rf "$STAGE_APP"; fi
  rm -rf "$TEMP_DIR"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

curl -fsSL --retry 3 --connect-timeout 15 --max-time 60 "$BASE_URL/checksums.txt" -o "$TEMP_DIR/checksums.txt" \
  || fail "Could not download the checksums of Octopus $VERSION."
EXPECTED="$(awk -v asset="$ASSET" '$2 == asset { print $1 }' "$TEMP_DIR/checksums.txt")"
[ -n "$EXPECTED" ] || fail "Release $VERSION has no $ASSET. See https://github.com/$REPOSITORY/releases/tag/v$VERSION"
printf 'Downloading %s\n' "$ASSET"
curl -fL --retry 3 --connect-timeout 15 --max-time 3600 "$BASE_URL/$ASSET" -o "$TEMP_DIR/$ASSET" \
  || fail "Could not download $ASSET."
ACTUAL="$(shasum -a 256 "$TEMP_DIR/$ASSET" | awk '{ print $1 }')"
[ "$ACTUAL" = "$EXPECTED" ] || fail "SHA-256 verification failed for $ASSET."

mkdir "$MOUNT_DIR"
hdiutil attach -quiet -nobrowse -readonly -noautoopen -mountpoint "$MOUNT_DIR" "$TEMP_DIR/$ASSET"
MOUNTED="yes"
SOURCE_APP="$MOUNT_DIR/Octopus.app"
[ -x "$SOURCE_APP/Contents/MacOS/Octopus" ] || fail "The disk image does not contain Octopus.app."

if [ -z "$INSTALL_DIR" ]; then
  INSTALL_DIR="/Applications"
  if [ ! -w "$INSTALL_DIR" ]; then
    INSTALL_DIR="$HOME/Applications"
    mkdir -p "$INSTALL_DIR"
  fi
fi
[ -d "$INSTALL_DIR" ] && [ -w "$INSTALL_DIR" ] || fail "Cannot write to $INSTALL_DIR."
TARGET_APP="$INSTALL_DIR/Octopus.app"
if pgrep -f "$TARGET_APP/Contents/" >/dev/null 2>&1; then
  fail "Quit Octopus and stop its runtime before updating it."
fi

STAGE_APP="$INSTALL_DIR/.Octopus.app.installing.$$"
printf 'Copying Octopus.app to %s\n' "$INSTALL_DIR"
ditto "$SOURCE_APP" "$STAGE_APP"
if [ -e "$TARGET_APP" ]; then
  PREVIOUS_APP="$TEMP_DIR/Octopus.previous.app"
  mv "$TARGET_APP" "$PREVIOUS_APP"
  if ! mv "$STAGE_APP" "$TARGET_APP"; then
    mv "$PREVIOUS_APP" "$TARGET_APP"
    fail "Could not replace $TARGET_APP; the previous version was kept."
  fi
else
  mv "$STAGE_APP" "$TARGET_APP"
fi
STAGE_APP=""

printf 'Installed Octopus %s at %s\n' "$VERSION" "$TARGET_APP"
printf 'Open it from Applications, or run: open "%s"\n' "$TARGET_APP"
