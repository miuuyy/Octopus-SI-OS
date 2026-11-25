# Process isolation on Linux

Octopus's Linux backend uses AppArmor to enforce a process's declared read, write, execute,
network, local IPC and signal authority. The same launch path serves verifier
scripts, interface lives and tool processes. A missing platform service or a
refused profile stops the launch before the target executes. Full-host grants
and an explicitly disabled sandbox keep their declared behavior.

## Headless release installer and host preflight

For prebuilt Linux `x86_64` headless CLI releases, `app/release/install-linux.sh`
provides a read-only host preflight check and a checksum-verified installer (and
`app/release/install.sh` dispatches to `install-linux.sh` automatically on
Linux).

Run the read-only host preflight first (mutates nothing; add `--json` for
machine-readable output):

```sh
sh app/release/install-linux.sh --check
# or from a published release that ships install-linux.sh:
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/latest/download/install-linux.sh | sh -s -- --check
```

`--check` distinguishes **hard blockers** (Linux `x86_64`, `tar`, `curl`/`wget`,
`sha256sum`/`shasum`, host **Node.js 24+**, writable `--prefix` / `--bin-dir`,
non-conflicting `octopus` link slot, and free disk space on the target
filesystem) from **optional/recommended host features** (Linux 6.5+, active
`systemd`, AppArmor 4.0+ with `/etc/apparmor.d/abi/4.0` and `cgroup v2`, and
`octopus-process-authority.service`), exiting `0` when ready and non-zero with
remediation steps when a required prerequisite is missing.

Install or upgrade a published Linux `x86_64` release:

```sh
sh app/release/install-linux.sh --version 0.1.0
# or, from a release that publishes install-linux.sh alongside install.sh:
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/latest/download/install.sh | sh
```

Defaults and options:

- `--prefix <dir>` / `OCTOPUS_INSTALL_DIR`: install root (default
  `$HOME/.local/share/octopus` for non-root, `/usr/local/lib/octopus` for
  `root`).
- `--bin-dir <dir>` / `OCTOPUS_BIN_DIR`: symlink directory for `octopus`
  (default `$HOME/.local/bin` for non-root, `/usr/local/bin` for `root`). Pass
  `--no-symlink` to skip linking; the installer refuses to overwrite an
  unrelated binary at `<bin-dir>/octopus`.
- `--version <ver>` / `OCTOPUS_VERSION` and `OCTOPUS_RELEASE_BASE_URL` (for
  mirrors or offline testing).
- Before touching `--prefix`, the installer verifies the exact SHA-256 of
  `Octopus-<ver>-linux-x64.tar.gz` against `checksums.txt` (or `.sha256`),
  extracts into a sibling staging directory on the **same filesystem** so
  hardlinks in the deduplicated tarball are preserved, checks `./octopus
  --version`, and performs an atomic rename swap with rollback on failure.

After installing, initialize a Home without leaving an unmanaged daemon running
and run diagnostics:

```sh
octopus setup --no-start
octopus doctor
```

### Upgrading an existing Linux host

AppArmor profiles are rendered inside the privileged host daemon
`/usr/libexec/octopus/octopus-linux-process-authority`, not by the unprivileged
CLI. After upgrading Octopus on a host that already runs
`octopus-process-authority.service`, re-run the bundled authority installer so
the daemon picks up updated AppArmor profile rules:

```sh
sudo sh "<prefix>/linux-process-authority/install-linux.sh" \
  "<prefix>/app/build/cli/runtime/bin/octopus-linux-process-authority" "$USER"
```

AppArmor retains pathname permissions across atomic file replacement. Exact
invocation paths remain exact: the publication protocol receives its own
directory and filename permissions without granting neighboring files. Before
exec, the native launcher creates only the required ancestor and publication
directories, as the ordinary user and without following symlinks. The resulting
profile grants directory reads and locks; creation never becomes permission to
delete or change an otherwise ungranted parent. Preparation failure may leave
the directories it already created, but never executes the target. Runtime
libraries, executable interpreters and operating-system resources are included
in the process's runtime closure.

## Build prerequisites

Linux workspace checks and the [terminal build](cli.md#build-and-open) require
Rust, **Node.js 24 or newer**, a C toolchain, pkg-config, and the OpenSSL,
SQLite and ALSA development headers. On Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libssl-dev libsqlite3-dev libasound2-dev
```

Check `node --version` before building or running the CLI: on Ubuntu 26.04 LTS,
the default `apt` `nodejs` package ships Node.js 22 (`v22.x`), which is below
Octopus's Node.js 24+ requirement. Install Node.js 24+ from the official
Node.js binary distribution, NodeSource, or a version manager (and set
`OCTOPUS_NODE` if it is outside `systemd`'s default `PATH`).

Standard POSIX `sh` and `bash` work out of the box on stock Debian/Ubuntu
servers. `zsh` is declared in `optional_executable_commands`: it is permitted
when installed, and not required to boot `system_workspace_env` or pass
`octopus doctor`.

ALSA headers are needed by the audio capability built with the workspace and
CLI distribution, even when the runtime is used headlessly. These build
prerequisites are separate from the process-isolation service below.

## Install the platform component

The backend requires Linux 6.5 or later, AppArmor 4 or later, the AppArmor 4.0
policy ABI, systemd and cgroup v2. It has no distribution-specific execution
branch. The installation must provide `/usr/sbin/apparmor_parser`,
`/etc/apparmor.d/abi/4.0` and `apparmor.service`. Enforced launches are unavailable
on systems without these prerequisites.

Install the distribution's AppArmor package as an administrator. For example,
on Ubuntu:

```sh
sudo apt-get install apparmor
```

The Linux CLI runtime distribution includes the service as
`bin/octopus-linux-process-authority`. To build the service separately from source:

```sh
cargo build --locked --release -p octopus-linux-process-authority
```

Install it for the Unix account that will run Octopus, replacing `operator`
with that account's login:

```sh
sudo sh registry/worker_environment_kind/process_session/packages/octopus-linux-process-authority/install-linux.sh \
  "$PWD/target/release/octopus-linux-process-authority" operator
```

Start a new login session so the account receives its `octopus-process` group.
Run Octopus as that account. The service does not start Octopus, execute tool
commands or change their Unix user. Its socket is available only to root and
members of that group. Group membership authorizes use of the platform service;
Octopus still forms every run's grant from its own declarations.

The service owns the profile compiler and a delegated cgroup subtree. Compiler
warnings are errors, including a rule the kernel cannot enforce. A launch retains
its preparation connection until attachment. The service checks kernel-reported
Unix credentials and a peer pidfd, the preparing process's identity and current
confinement. It briefly holds the single-threaded launcher in a ptrace stop while
attaching it to its cgroup, so the PID cannot be recycled during the attachment.
Attachment admits only the preparing process itself or its actual child. The
launcher applies the profile before executing the declared program. Ordinary confined
processes cannot request new profiles; only a process whose declaration grants
sandbox delegation may do so. Requests from another user namespace are refused.

A process without network or local IPC authority may still create unnamed
stream socket pairs, which language runtimes use to wake their own event loops.
It cannot bind or connect an abstract socket. AppArmor mediates a pathname
socket through the file rule of its path, so such a process reaches one only
where its declared file authority already lets it write that path. The service's
control socket is granted by path to a process with sandbox delegation and
denied by path to every other process.

Library tests that launch confined processes (including `octopus-verifiers`) also
require this service to be installed and running. Without it they fail closed;
installing the distribution AppArmor package alone is not sufficient.

## Process lifetime and recovery

A profile stays installed while its preparation connection is open or its
cgroup contains processes. Descendants remain in that cgroup across forks,
executions and new process groups. This includes a command keeper that outlives
the tool process which created it. The service unloads the profile only after
both conditions have ended. A failed cleanup retains the profile and reports
the error.

Service restart preserves existing profiles, cgroups and their root-owned
records under `/run/octopus-process-authority/leases`. It resumes collection
from those records without restarting commands. Stopping the service prevents
new confined launches; it does not remove confinement from existing processes.
Do not remove its retained state while those processes are alive.

Trusted workspace executors invoke the same installed executable in its
unprivileged `exec` mode. It projects the formed room's readable and writable
roots, operating-system runtime resources, protected read-only subpaths and
network ceiling into another AppArmor profile, then replaces itself with the
command. The command keeps its PID and streams and receives no sandbox-delegation
authority. Protected paths remain protected when absent or atomically replaced;
their ancestor directories cannot be renamed or removed to evade that protection.
No filesystem mount or second sandbox backend is involved.

## Run as a systemd service

On a headless Linux host, supervise the runtime with `systemd` using
`octopus up --foreground` (from a built [terminal distribution](cli.md#build-and-open)).
Run the service as the ordinary Unix account that owns the Octopus Home, not as
`root`. Prebuilt Linux CLI archives and `package-cli.cjs` set `0755` directory
permissions and world-readable runtime files (with normalized `0:0` archive
ownership), so unpacking the distribution as `root` under `/opt/...` and running
the unit as a dedicated non-root user works without `EACCES`. Initialize the
Home first with `--no-start` so setup provisions packages, provider credentials,
and workspace records without leaving a detached daemon behind for `systemd` to
collide with (`./octopus setup --home /home/operator/.octopus --non-interactive --no-start ...`),
so its client configuration and provider credentials are already in place, and
install `octopus-process-authority.service` above if runs use enforced process
isolation.

Create `/etc/systemd/system/octopus.service`, replacing `operator`,
`/opt/octopus-os`, and `/home/operator/.octopus` with your Unix account,
checkout (or CLI distribution) path, and Home path:

```ini
[Unit]
Description=Octopus runtime
After=network-online.target octopus-process-authority.service
Wants=network-online.target octopus-process-authority.service

[Service]
Type=simple
User=operator
Group=operator
SupplementaryGroups=octopus-process
WorkingDirectory=/home/operator
Environment=OCTOPUS_HOME=/home/operator/.octopus
# Uncomment if Node.js 24+ is not on systemd's default PATH:
# Environment=OCTOPUS_NODE=/usr/local/bin/node
ExecStart=/opt/octopus-os/octopus --home /home/operator/.octopus up --foreground
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
```

If enforced process isolation is required on the host, change
`Wants=network-online.target octopus-process-authority.service` to include a
separate `Requires=octopus-process-authority.service` line; omit
`SupplementaryGroups=octopus-process` only when running without the process
authority service. To supervise a standalone bundle outside the checkout, pass
`--bundle <path>` before `up --foreground`.

`octopus up --foreground` runs stopped-transaction recovery, starts the release
runtime attached in the foreground with stdout and stderr inherited (so
`journald` captures runtime logs), preserves `OCTOPUS_NODE` on `PATH` for child
processes, and forwards `SIGTERM`, `SIGINT`, and `SIGHUP` to the runtime. It
fails closed if a runtime is already running for that Home (stop any manually
started instance with `./octopus down` before starting the unit) and cannot be
combined with `--json` or `--timeout`. Variables in `<OCTOPUS_HOME>/.env` are
loaded automatically, with unit `Environment=` or `EnvironmentFile=` values
taking precedence. With `Restart=on-failure`, an explicit `./octopus down` exits
`0` and leaves the service stopped until started again, while an unexpected
runtime exit restarts it.

Enable, start, and inspect the service:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now octopus.service
systemctl status octopus.service
journalctl -u octopus.service -f
/opt/octopus-os/octopus --home /home/operator/.octopus status --json
/opt/octopus-os/octopus --home /home/operator/.octopus doctor
```

Run `octopus doctor` after initial Home setup (once `<OCTOPUS_HOME>/registry`
is populated).

## Staged self-update for systemd installations

`app/scripts/safe-self-update.cjs` supports headless Linux CLI installations
supervised by `systemd`, covering both checkout builds (`app/build/cli`) and
standalone extracted release directories (such as `/opt/octopus/current`). On
Linux it selects the CLI layout automatically; pass `--install-dir=<path>` for
a standalone installation outside the checkout, `--service=<unit>` (default
`octopus.service`), `--authority-service=<unit>` (default
`octopus-process-authority.service`), `--user-service` for `systemctl --user`,
`--timeout=<seconds>` (default `300`) to bound successor readiness waiting, and
`--require-authority` to refuse restarts unless
`octopus-process-authority.service` is active. Both `--flag=value` and
space-separated `--flag value` forms are accepted, and standalone Linux release
tarballs bundle `app/scripts/safe-self-update.cjs` so operators can run it
directly from `/opt/octopus/current/app/scripts/safe-self-update.cjs`.

```sh
# Checkout-based installation (stages into app/build/cli.staging)
node app/scripts/safe-self-update.cjs status --home /home/operator/.octopus
node app/scripts/safe-self-update.cjs build --home /home/operator/.octopus
node app/scripts/safe-self-update.cjs verify --home /home/operator/.octopus
node app/scripts/safe-self-update.cjs restart --swap --wait-quiet --home /home/operator/.octopus

# Standalone release installation under /opt/octopus/current
# (stages into /opt/octopus/current.staging, keeps /opt/octopus/current.prev)
node app/scripts/safe-self-update.cjs status \
  --install-dir /opt/octopus/current --home /home/operator/.octopus
node app/scripts/safe-self-update.cjs verify \
  --install-dir /opt/octopus/current --home /home/operator/.octopus
node app/scripts/safe-self-update.cjs restart --swap --wait-quiet \
  --install-dir /opt/octopus/current --home /home/operator/.octopus --service octopus.service
```

Key safety invariants for Linux headless self-updates:

- **`build`** runs the disk-space preflight (`--min-free-gb`, default `15` GiB)
  and stages via `build-cli.cjs --runtime` with `OCTOPUS_CLI_BUILD_DIR` set to
  the `.staging` directory (for standalone `--install-dir` targets, it stages
  `<staging>/app/build/cli` and populates the matching standalone `./octopus`
  launcher, `app/cli/`, and `app/scripts/safe-self-update.cjs`), verifying the
  live `runtime-build-receipt.json` is untouched. Run `build` as the unprivileged
  `systemd` service user when possible; if you stage a build (or unpack a
  release) as `root`, align ownership of the staged and parent install
  directories (for example `sudo chown -R <user>:<group> /opt/octopus/current.staging`)
  before running `restart --swap` as the service user so the swap and sibling
  `.prev`/`.failed` directories remain writable by that account.
- **`verify`** validates `runtime-build-receipt.json` (`platform: "linux"`),
  `shared.cjs`, `0755` directory and executable permissions, every receipt
  binary's byte size and SHA-256 digest (including `bin/octopus` and
  `bin/octopus-linux-process-authority`), layout parity with the live
  installation and `systemd` `ExecStart`, the real launcher/runtime CLI
  invocation contract, and `octopus doctor` when `<OCTOPUS_HOME>/registry`
  exists.
- **`restart --swap`** inspects the loaded `systemd` unit via `systemctl show`
  and fails closed (`systemd_service_mismatch` / `staging_layout_mismatch`) if
  its `OCTOPUS_HOME`, `ExecStart`/`--bundle`, or staged directory layout does
  not match the target Home and install directory. After the turn-frontier
  guard passes, it swaps `live -> .prev` and `.staging -> live`, runs
  `systemctl restart <unit>`, and waits for a ready successor (up to
  `--timeout`, short-circuiting promptly if `systemd` reports the unit
  `failed`, `auto-restart`, or a rising restart counter) with no new
  `interface_failures`. If the successor crashes, fails readiness, or
  regresses, it automatically rolls back (`live -> .failed`, `.prev -> live`),
  restarts the unit again, and refreshes the reported `systemd.mainPid` and
  unit state in its JSON output.
- **No automatic `sudo` and peer-UID `control.sock` check**:
  `safe-self-update.cjs` never invokes `sudo`. Run it as the same non-root Unix
  user that owns the service (for example `operator`), with the parent install
  directory (such as `/opt/octopus`) writable by that user. The runtime's
  `<OCTOPUS_HOME>/state/runtime-instance/control.sock` enforces an exact
  `SO_PEERCRED` peer-UID match against the runtime owner, so running
  `safe-self-update.cjs` as `root` against a non-root service fails closed on
  `control.sock` (`write EPIPE`). Grant the service user narrow non-interactive
  restart permission with a `polkit` rule such as
  `/etc/polkit-1/rules.d/50-octopus-self-update.rules`:

```js
polkit.addRule(function (action, subject) {
  if (
    action.id === "org.freedesktop.systemd1.manage-units" &&
    action.lookup("unit") === "octopus.service" &&
    action.lookup("verb") === "restart" &&
    subject.user === "operator"
  ) {
    return polkit.Result.YES;
  }
});
```

  If `systemctl restart` is refused for missing privileges, `safe-self-update.cjs`
  rolls back any swapped directory immediately and exits with
  `permission_denied: true`.
- **`prune`** reclaims `.staging`, `.failed`, Rust `target/`, and (with
  `--include-prev`) `.prev`, while always protecting the live CLI directory
  (even when `--include-cli` is supplied) and `<OCTOPUS_HOME>`. Running `prune`
  directly from an unpacked standalone install bundle (for example
  `node /opt/octopus/current/app/scripts/safe-self-update.cjs prune --install-dir /opt/octopus/current --home /home/operator/.octopus`)
  works without `--target-dir`: the default `rust_target` candidate is omitted
  when it would overlap a protected path or when no Cargo workspace is present.
  Pass `--target-dir <path>` only when you also want to prune a separate
  source-checkout Cargo `target/` directory (an explicit `--target-dir` that
  overlaps a protected path still fails closed).

## Diagnostics and scope

```sh
systemctl status octopus-process-authority.service
journalctl -u octopus-process-authority.service
```

The service reports profile compilation, peer identity, attachment and cleanup
errors. It does not substitute an unconfined launch when isolation is
unavailable. Distribution AppArmor policy and user-namespace restrictions remain
in force; they are not disabled by the installer.

This component supplies process isolation. It does not convert a macOS Home,
install Linux versions of its providers or make macOS-only integrations
available. Compose a fresh Home with packages and host paths for the target
machine; existing Homes and their execution evidence are not migrated.

The platform contracts are documented by
[AppArmor](https://apparmor.net/man/4.0/apparmor.d/), its
[policy loader](https://manpages.ubuntu.com/manpages/noble/man8/apparmor_parser.8.html)
and the Linux kernel's
[cgroup v2 interface](https://docs.kernel.org/admin-guide/cgroup-v2.html).
