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

## Headless Linux Quickstart (Bring Any API Key)

After installing the Linux `x86_64` bundle above (or
[building the CLI](cli.md#build-and-open)), setup can run without a desktop,
macOS, or ChatGPT OAuth. Choose one supported API-key provider below, set its
key privately in your shell environment, and use a model available to that
account. These examples use IDs in the bundled catalogue; provider access and
billing are separate, and catalogue entries can change.

Node.js **24+** is required. [Ubuntu 26.04's apt](https://packages.ubuntu.com/resolute/nodejs) package ships Node 22: install
a supported Node separately and, if it is not first on `PATH`, point directly
at it. Keep this environment when running setup, doctor, and the runtime:

```sh
export OCTOPUS_NODE=/usr/local/bin/node # replace with your Node 24+ executable
"$OCTOPUS_NODE" --version
```

`codex` remains the desktop-capable default pack. The dedicated `headless` pack
supplies Home, task authoring, context, memory, code mode, filesystem and
isolated process/git rooms, with OpenRouter as its default API provider. It
does not install browser, desktop, audio or realtime voice drivers. Gemini and
OpenCode Go are available explicitly via `--provider`; their package closure is
installed transactionally and selected in Home and Project
`application_defaults`. Headless-compatible sensors are optional; installing one
does not arm a trigger. The shared Codex adapter package is still required by
API providers, but no ChatGPT sign-in is needed.

Run **one** of these alternatives against a fresh Home. Do not paste real keys
into command-line flags or commit them to a setup file.

```sh
# Gemini: first set OCTOPUS_PROVIDER_GEMINI_API_KEY privately.
octopus setup --non-interactive --no-start --pack headless \
  --workspace-name "My space" --provider world_provider_gemini \
  --model gemini-3.5-flash

# OpenRouter: first set OCTOPUS_PROVIDER_OPENROUTER_API_KEY privately.
octopus setup --non-interactive --no-start --pack headless \
  --workspace-name "My space" --provider world_provider_openrouter \
  --model anthropic/claude-sonnet-5

# OpenCode Go: first set OCTOPUS_PROVIDER_OPENCODE_GO_API_KEY privately.
octopus setup --non-interactive --no-start --pack headless \
  --workspace-name "My space" --provider world_provider_opencode_go \
  --model beam-free
```

### Explicit OpenRouter models and privacy

`--provider world_provider_openrouter` reads
`OCTOPUS_PROVIDER_OPENROUTER_API_KEY` and saves it only in the selected Home's
`interfaces/world_provider_openrouter/environment.json` with mode `0600`.
Credentials do not belong in flags, setup receipts or logs. Setup does not test a
billable provider call.

The shipped default model and named catalogue entries are unchanged; no `:free`
model is selected automatically. For a model outside that catalogue, an explicit
`--model vendor/model-id` opts into the provider's declared `openrouter-model`
alias. Setup saves the upstream ID as `OCTOPUS_PROVIDER_OPENROUTER_MODEL` in the
same private environment file. Alternatively, set that variable privately and
pass `--model openrouter-model`. A missing alias target, malformed model ID or
provider without an override contract is rejected. The exact alias remains the
policy model; setup never adds a wildcard or silently substitutes another model.

An explicitly selected `:free` model is allowed, but may not support ZDR. An ID
alone is not a privacy guarantee: verify the chosen upstream route's retention,
privacy settings, availability and tool support before sending sensitive data.
Custom aliases use a conservative text/tool catalogue; setup does not discover
or verify a custom model's capabilities. A configured Home keeps its saved
provider/model choices; rerunning setup is not a provider switch.

`--home <path>` selects a different Home; use it consistently on subsequent
commands. Setup preserves an already configured Home rather than replacing its
provider. API-key setup reads the provider's environment variable (or an
existing private provider environment file) and saves credentials privately.
`--no-start` provisions the Home without launching a detached daemon:

```sh
octopus doctor
octopus up --foreground
# From another terminal, a real provider reply checks more than diagnostics:
octopus exec "Reply with a short hello."
```

Setup and doctor do not prove a successful provider call. This is a research
preview, and Linux does not gain macOS-only integrations. For unattended use,
configure the [systemd service](#run-as-a-systemd-service) instead of leaving
an unmanaged background process. Enforced process isolation additionally needs
the host authority service described below.

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
Environment=PATH=/home/operator/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
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
processes, appends `$HOME/.local/bin` to `PATH` when that directory exists, and
forwards `SIGTERM`, `SIGINT`, and `SIGHUP` to the runtime. It
fails closed if a runtime is already running for that Home (stop any manually
started instance with `./octopus down` before starting the unit) and cannot be
combined with `--json` or `--timeout`. Variables in `<OCTOPUS_HOME>/.env` are
loaded automatically, with unit `Environment=` or `EnvironmentFile=` values
taking precedence (note that `systemd` always supplies a default `PATH`, so set
`Environment=PATH=...` in the unit when exposing additional user toolchain
directories). With `Restart=on-failure`, an explicit `./octopus down` exits
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

## Install optional bundled packages

The bundled catalog includes packages outside the setup pack. Use the package
manager from the checkout or standalone release (it always prints JSON):

```sh
node app/scripts/safe-self-update.cjs packages available --json
node app/scripts/safe-self-update.cjs packages install world_watch world_provider_openrouter --dry-run --json
node app/scripts/safe-self-update.cjs packages install world_watch world_provider_openrouter --json
```

For standalone releases add `--platform linux --install-dir /opt/octopus/current`;
use `--home /home/operator/.octopus` to select the service user's Home.
Use exact package ids from `packages available` (including for GitHub sensors).
Installation reviews the dependency closure and pins its source digests, then
uses the same transactional installer and receipts as setup. Installed packages,
local configuration and credentials are preserved; ambiguous ownership requires
explicit repair. A dry run lists new and preserved packages without writing.
Installation does not bind sensors to tasks, configure provider credentials, or
restart the runtime. `packages status` / `packages sync` audit / repair packages
already installed; they do not install optional packages.

Before an upgrade, inspect installed state without creating locks, caches,
transactions, directories or starting the runtime:

```sh
octopus packages check --home /home/operator/.octopus
octopus packages check --home /home/operator/.octopus --json
# Same report in the standalone updater (always JSON):
node app/scripts/safe-self-update.cjs packages check --platform linux --install-dir /opt/octopus/current --home /home/operator/.octopus --json
```

The check lists installed receipts, managed-file differences (package, registry
owner, relative path and reason), available updates, and ready/blocked ownership
transfers. Reasons include `modified`, `missing`, `added` and `mode_changed`;
invalid receipts, unreceipted owners, duplicate ownership and unfinished
transactions require review. The reserved `.octopus-publication` directory is
publication bookkeeping, not a pending package transaction; malformed metadata
and actual transaction entries still require review. Declared mutable data
additions and allowed configuration values are not managed drift. File contents
and configuration values are never printed.

This offline check accepts Homes beyond the Unix socket path-length limit;
commands that use the local daemon still require a socket-compatible Home.

Exit **0** means no detected issues (updates/clean transfers may still be
available); **1** means drift or issues. Terminal CLI invocation errors also use
1; updater invocation errors use 2. JSON has the versioned shape
`{schema_version:1, ok, packages, transfers, issues}`. Each package has
`id, receipt_state, update_available, files, issues`; `files` lists differences,
not every unchanged file. Each file has `registry_kind, owner, path, reason`;
each transfer has `registry_kind, owner, from, to, state, reason`; issues have
`code, member`. `update_available` is null when the target cannot be inspected.
A check is an observation, not authorization to write: sync must still revalidate
its own preimages and selection, and never recovers a pending transaction as
part of this check.

Sync repairs the selected installed packages in one checked transaction, including
member ownership transfers between their manifests. Both the old and new owner
packages must be selected (including with `--only`); sync refuses duplicate
ownership, transferred-file drift, and removals without a receiving package.
If one selected repair is refused, the batch does not proceed independently.

## Browser rooms on Linux (CDP)

`driver.browser_session` is an optional bundled package and can be installed on
headless Linux with the package manager above (`driver.browser_session`). Its
default `system_browser/interface.json` declares `"wire": "browser.native.v1"`
for the desktop app's native browser host and fails closed when
`browser_native_host_root` is absent.

To run `system_browser` on headless Linux without the desktop app, start a local
Chromium or Chrome process bound to loopback with a dedicated profile directory:

```sh
chromium --headless=new --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=9222 --user-data-dir=/home/operator/.cache/octopus-chromium
```

Then replace `interface_driver_config.private_config.browser_room.transport` in
`<OCTOPUS_HOME>/registry/interface/system_browser/interface.json` with a `cdp`
transport:

```json
{
  "wire": "cdp",
  "discovery_url": "http://127.0.0.1:9222"
}
```

`driver.browser_session` declares this transport choice as allowed local
configuration, so `octopus packages check` treats it as valid configuration
rather than drift. Plaintext `http://` discovery URLs are restricted to loopback
(`127.0.0.1`, `[::1]`, `localhost`); non-loopback endpoints require `https://`
and `wss://` and support `auth_required` / `auth_env_key` and
`discovery_url_env`. To expose multiple CDP endpoints side by side, author a
separate door interface with `copy_of: "system_browser"`.

Each browser room creates and disposes its own isolated Chromium
`BrowserContext` via `Target.createBrowserContext`. Under AppArmor,
`system_browser/life.json` keeps `browser_native_host_root` optional and
`allow_network: false` (validating CDP config statically before delegating),
while CDP-capable `browser.*` tools declare `allow_network: true` and mark
`secrets/browser-native-host.auth` optional. Navigation, accessibility
snapshots, click/type/key/scroll/hover/select/drag, evaluation, screenshots,
PDF, cookies, tabs, console, dialogs, and polling waits work over CDP;
native-host-only tools (`browser.download_*`, `browser.filechooser_*`,
`browser.history*`, `browser.locator_*`, `browser.dom_*`, `browser.clipboard_*`,
`browser.tab_mark`, `browser.visibility_*`, and retained `browser.wait` phases)
fail closed.

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
