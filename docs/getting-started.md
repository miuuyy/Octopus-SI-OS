# Getting started

The research preview runs locally on macOS. Install the released app, or build
it from this repository; either way you finish setup inside the app. Codex is
the first supported provider, and sign-in is **Sign in with ChatGPT** in the
app - no separate provider CLI login.

For the terminal client and its independent build command, see
[Terminal client](cli.md). Both clients use the same Home and runtime.

## Install the app

Releases are published on
[GitHub Releases](https://github.com/miuuyy/Octopus-SI-OS/releases) as a disk
image for Apple Silicon, `Octopus-<version>-macos-arm64.dmg`, with
`checksums.txt` and `install.sh`.

Requirements:

- macOS 14 or later on Apple Silicon.
- Command Line Tools for Xcode (`xcode-select --install`). Octopus runs
  `python3` and `git` from them; a Mac without them offers the install the
  first time something calls `python3`.
- A ChatGPT account with access to the model you select.

Install the latest release from Terminal:

```sh
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/latest/download/install.sh | sh
```

The installer checks macOS and the Command Line Tools, verifies the disk
image against `checksums.txt`, and copies `Octopus.app` to `/Applications`
(`~/Applications` when `/Applications` is not writable). Set
`OCTOPUS_VERSION=<version>` to install a specific release, or
`OCTOPUS_APPLICATIONS_DIR` to choose the folder.

To install by hand, download the disk image, drag Octopus to Applications,
and run this once before the first launch:

```sh
xattr -dr com.apple.quarantine /Applications/Octopus.app
```

The build is ad-hoc signed, not notarized by Apple. A browser marks the
download as quarantined, and macOS then refuses to open the app. **Open
Anyway** in System Settings would admit only the app itself: Octopus copies its
tools into your Home when it installs packages, the copies keep the quarantine
mark, and macOS would block them one by one. The installer above downloads
with `curl`, so nothing it installs is quarantined. Do not run Octopus from the
mounted disk image.

To update, quit Octopus and its runtime, then install the new release the same
way. Octopus is a research preview: releases with incompatible persisted formats
require a fresh Home through setup. Existing Homes are never migrated or repaired
on startup; keep your sign-in credentials when resetting a disposable Home.
To uninstall, delete `Octopus.app`; your Home (`~/.octopus` by default) and the
Codex sign-in in `~/.codex` stay until you remove them.

## Build from source

Building is for development and for architectures without a published
release.

### Prerequisites

- macOS 14 or later, with the checkout on an APFS volume.
- Xcode Command Line Tools (`xcode-select --install`), including Clang, Swift,
  the macOS SDK, and `/usr/bin/python3`.
- [Node.js 24 LTS](https://nodejs.org/en/download) and npm. Node 20 does not
  support the required native rebuild and SQLite checks.
- Current stable [Rust](https://www.rust-lang.org/tools/install) and Cargo.
  The workspace and locked dependencies require at least Rust 1.91.
  Use current stable for the preview build.
- Internet access to fetch dependencies and the pinned provider distribution.
  A ChatGPT account with access to the selected model is needed for Home.

The packager builds for the architecture of the Node process running it. Use a
native Node installation on Apple Silicon. Intel packaging has a source path but
is not established as a tested preview distribution here.

### Build

```sh
git clone https://github.com/miuuyy/Octopus-SI-OS.git
cd Octopus-SI-OS
npm ci --prefix app
```

The native presentation build requires the headers of the Node inside the
pinned Electron, independently of the Node version running npm.
`app/native/runtime.json` pins Electron 45.2.1, its Node 24.22.1, the sha256 of
the headers archive (from the [Node.js release manifest](https://nodejs.org/download/release/v24.22.1/SHASUMS256.txt))
and the sha256 of the Electron archives. Fetch and verify the headers:

```sh
node app/native/runtime.cjs headers
```

Electron no longer downloads its binary on install; packaging fetches the
checksum-pinned Electron archive itself (`node app/native/runtime.cjs electron`
does the same by hand). Then package and open the app:

```sh
OCTOPUS_NATIVE_NODE_HEADERS="$PWD/app/build/node-v24.22.1/include/node" \
  npm --prefix app run package:mac
open -n ./app/build/Octopus.app
```

Packaging builds the renderer, release runtime, capability executables, and
provider bridge; it includes the managed Codex distribution and verifies an
ad-hoc code signature. The output is `app/build/Octopus.app`, built for this
machine. It is not a notarized installer or a universal binary. Repackaging
replaces that bundle in place.

## First setup

Follow the client screens. The actions depend on the setup already present on
your machine:

- **Sign in with ChatGPT** opens browser authentication. Complete it in the
  browser and return to the client.
- The access screen lets you configure or enter the local administrator password.
- The embedded launch terminal prepares a command ending in `octopus up`.
  Press Enter to run it and wait for the runtime's ready state.
- Workspace setup asks for a starting structure, project names, and capabilities.
  Review the choices and select **Set up Octopus**.

![Local client lock screen asking for the Administrator or Test password](images/client/client-lock.png)

The local access password is separate from provider sign-in. Later,
**avatar → Log out** returns to this gate without removing your Home or
provider credentials.

When the client reaches Home, send a short message. A model reply checks the
connection; exercising a task's granted tools checks that task's capability setup.

For a new task, use **Create agent** if the Agent list is empty. Choose a Project
and an offered model, review capabilities, and create the Agent. Then choose its
Project and select the Agent in the task composer.

The [desktop client guide](client.md) explains Home, Tasks, voice, gestures,
and how Applications and Drivers become available to a project.

Model availability and usage limits belong to the provider account. Additional
applications and voice capabilities can require their own credentials and macOS
permissions.

## Codex CLI

The app carries its own Codex CLI: a pinned upstream release, checked against
its SHA-256 when the app is packaged. You do not need to install Codex, and a
Codex CLI you installed yourself is neither used nor updated by Octopus.

The sign-in is shared. **Sign in with ChatGPT** keeps its credential in the
machine's Codex home, `~/.codex` unless `CODEX_HOME` says otherwise - the same
place Codex CLI uses. So:

- If Codex CLI on this Mac is already signed in with ChatGPT, Octopus starts
  signed in.
- Signing in or out in either one changes it for both.
- Octopus needs a ChatGPT sign-in. If `~/.codex` holds an API-key login, setup
  asks you to sign in with ChatGPT, and doing so replaces that login for Codex
  CLI as well.

To keep them apart, launch Octopus once with its own Codex home:

```sh
open -n --env OCTOPUS_HOME="$HOME/.octopus" --env CODEX_HOME="$HOME/.octopus/codex" /Applications/Octopus.app
```

Octopus remembers this pair in `~/Library/Application Support/Octopus/launch.json`
and uses it for later launches from the Dock or Finder.

## Local data and diagnostics

By default, the app uses `~/.octopus` as its Octopus Home. The active registry,
run and EE records, ledger, interface state, and client setup data live there.
Provider sign-in uses the machine's Codex authentication boundary, normally
under `~/.codex`; authentication is not contained entirely in the Octopus Home.
Treat both locations as private.

The commands below use the installed app at `/Applications/Octopus.app`; for a
build from source, use `./app/build/Octopus.app` from the checkout root.

For a separate preview Home, launch the app with the environment variable:

```sh
open -n --env OCTOPUS_HOME="$HOME/.octopus-preview" /Applications/Octopus.app
```

Keep the Home path short: macOS Unix sockets have a path-length limit. This
separates Octopus state, but does not create a separate provider account (see
[Codex CLI](#codex-cli)).

Inspect the selected default Home without booting:

```sh
/Applications/Octopus.app/Contents/Resources/app/bin/octopus doctor
```

For a custom Home, pass the same `OCTOPUS_HOME` to diagnostics. A healthy boot or
doctor report does not prove that every provider or tool works. If setup fails,
retain the displayed error and use the client's explicit retry/sign-in action;
do not reset the Home or replace its descriptors to make an error disappear.
