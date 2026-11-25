Octopus {{version}} includes:

- **macOS desktop app (`Octopus-{{version}}-macos-{{architecture}}.dmg`)** for macOS 14 or later ({{architecture}}), bundling the runtime, local packages, and pinned Codex CLI.
- **Linux headless CLI (`Octopus-{{version}}-linux-x64.tar.gz`)** for Linux `x86_64` (with `Octopus-{{version}}-linux-x64.tar.gz.sha256` and `install-linux.sh`), bundling the release runtime, `octopus-linux-process-authority`, local packages, pinned Codex CLI, and standalone terminal client.

## macOS install

In Terminal:

```sh
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/download/v{{version}}/install.sh | OCTOPUS_VERSION={{version}} sh
```

The installer checks the SHA-256 of the disk image against `checksums.txt` and copies `Octopus.app` to `/Applications` (or `~/Applications` when `/Applications` is not writable).

To install by hand, download `Octopus-{{version}}-macos-{{architecture}}.dmg`, drag Octopus to Applications, and before the first launch run:

```sh
xattr -dr com.apple.quarantine /Applications/Octopus.app
```

This build is ad-hoc signed and not notarized by Apple. Without that command macOS refuses to open it, and the tools Octopus copies into your Home would stay quarantined as well. Do not open Octopus from the disk image itself.

## Requirements

- macOS 14 or later.
- Command Line Tools for Xcode: `xcode-select --install`. Octopus runs `python3` and `git` from them.
- A ChatGPT account for Sign in with ChatGPT. If Codex CLI is already signed in on this Mac, Octopus uses the same sign-in in `~/.codex`.

## Linux headless CLI (`x86_64`)

Check host prerequisites (read-only) or install the verified headless CLI release with `install-linux.sh` (`install.sh` also dispatches to `install-linux.sh` on Linux):

```sh
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/download/v{{version}}/install-linux.sh | sh -s -- --check
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/download/v{{version}}/install-linux.sh | OCTOPUS_VERSION={{version}} sh
```

Or download `Octopus-{{version}}-linux-x64.tar.gz` and `Octopus-{{version}}-linux-x64.tar.gz.sha256`, verify the archive, and extract it manually:

```sh
sha256sum -c Octopus-{{version}}-linux-x64.tar.gz.sha256
tar -xzf Octopus-{{version}}-linux-x64.tar.gz
cd Octopus-{{version}}-linux-x64
./octopus --version
./octopus setup
```

Requires Node.js 24+ on the host (no repository checkout or `npm install` required). For AppArmor process isolation on Linux 6.5+ / AppArmor 4+, install the bundled process authority service:

```sh
sudo sh ./linux-process-authority/install-linux.sh "$PWD/app/build/cli/runtime/bin/octopus-linux-process-authority" "$USER"
```

When upgrading an existing Linux host, re-run the authority installer from your install `<prefix>` so `/usr/libexec/octopus/octopus-linux-process-authority` (which renders AppArmor profiles) picks up updated AppArmor profile rules:

```sh
sudo sh "<prefix>/linux-process-authority/install-linux.sh" \
  "<prefix>/app/build/cli/runtime/bin/octopus-linux-process-authority" "$USER"
```

See [Linux process isolation & systemd](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/docs/linux.md) and [Terminal client](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/docs/cli.md) for headless setup and `systemd` supervision.

Setup, Home location, and diagnostics: [Getting started](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/docs/getting-started.md). Known problems: [Troubleshooting](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/TROUBLESHOOTING.md).
