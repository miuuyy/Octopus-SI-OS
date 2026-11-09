# Troubleshooting

Octopus fails closed. When something is wrong you should see an error, not a
quiet fallback. This page says where to look.

## The first five minutes

In the desktop app, start with **Help → Report a Problem…**. Describe what
you expected and what happened, review the diagnostic preview, then save the
report. **Open GitHub draft** opens an issue containing your description
and build details; review it, attach the saved report if appropriate, and
submit it yourself. The app does not upload the report or post the issue.

This window is independent of the main renderer and works during setup.
Unlock as Administrator before opening it to include setup and client logs;
otherwise it contains application facts only. It observes existing evidence
without booting, repairing or restarting the runtime. A long-running setup
step remains pending; 100% is reserved for a sealed setup receipt.

By default, the report contains technical metadata: setup and client events,
selected ledger movements, matching calls and results, context generations,
pending handoffs and failed interfaces. Private identities are replaced
consistently within each report. Only technical fields are exported from verified
artifact data; conversation text, tool arguments and results, project files,
credential files and environment variables are excluded. **Include error text
and log excerpts** adds bounded diagnostics that may contain private data;
review them before sharing. Reports are bounded observations, not a health
verdict or a complete runtime trace. Missing or unreadable sources are named
in the report. Use **Refresh** to capture a later snapshot.

For a deeper local investigation:

1. Run the doctor against the Home you are using. With the installed app:

   ```sh
   /Applications/Octopus.app/Contents/Resources/app/bin/octopus doctor
   ```

   For a build from source, use `./app/build/Octopus.app` from the checkout
   root. For a custom Home, pass the same `OCTOPUS_HOME` you launch with.
2. Read the error the app shows and use its own retry or sign-in action.
3. Do not delete the Home or replace its descriptors to make an error go away.
   To start clean, launch with a separate Home instead:

   ```sh
   open -n --env OCTOPUS_HOME="$HOME/.octopus-preview" /Applications/Octopus.app
   ```

If the failed Home was already moved aside, open **Help → Report a Problem…**
in the updated app and click **Choose saved Home…**. Select the preserved
Octopus Home folder, enable **Include error text and log excerpts** if needed,
review the preview and click **Save report…**. The picker shows hidden folders.
This reads the selected Home without switching to it, starting its runtime or
changing its files. **Use current Home** returns to the active Home's diagnostics.
For a saved Home, build details identify the app collecting the report; include
the original report or revision when reporting a failure from an older build.

## Where the evidence is

| You want to know | Look at |
| --- | --- |
| Why setup stopped | `<home>/client/setup-state/operations/*.json` - the `last_error` field |
| Why an interface is not answering | `<home>/interfaces/<interface-id>/interface-life.stderr.*.log` |
| What moved, and what was refused | `<home>/ledger/movements.jsonl` - look for `DeliveryRefused` and `VerificationFailed` |
| What a run is waiting for | `<home>/state/runs/<run>.json` - its `active_checkpoints` |

The screen shows a short message. The files above carry the exact reason.

## Compare source, installation, and local JSON

For a package/configuration bug, collect a scoped report before changing the
Home. From the checkout root, set the Home you actually use:

```sh
OCTOPUS_DIAG_HOME="$HOME/.octopus"
OCTOPUS_DIAG_SOURCE="$PWD/registry"
OCTOPUS_DIAG_LIBRARY="$PWD/app/distribution/local-packages"

node app/electron/clientDoctor.cjs \
  "$OCTOPUS_DIAG_SOURCE" "$OCTOPUS_DIAG_HOME/registry" \
  "$OCTOPUS_DIAG_LIBRARY" "$OCTOPUS_DIAG_HOME/client/package-state" \
  --package world_provider_codex > package-diagnostic.json
```

`--package` takes the local package id. To inspect one descriptor instead,
replace `--package world_provider_codex` with:

```sh
--registry-file interface/world_provider_codex/interface.json
```

Use the source belonging to the build being diagnosed. For a packaged app,
set `OCTOPUS_DIAG_SOURCE` to its
`Contents/Resources/app/local-packages/registry` and `OCTOPUS_DIAG_LIBRARY` to
`Contents/Resources/app/local-packages`. A checkout is useful for comparing
against a proposed update, but it is not evidence of what an older app shipped.

The report compares three things: the selected source, the last installation
receipt or baseline, and the active files. For packages, it distinguishes
package-owned content from manifest-declared configuration fields. A changed
field is not automatically corruption, and a descriptor created only in Home
does not need a source package owner. `null` means there is not enough evidence
for that comparison.

The versioned output is `octopus.configuration_diagnostic`, version `1`.
By default it omits names, paths, JSON values, active-content hashes, logs,
credentials, and environment variables. `--include-paths` adds relative names
and configuration pointers for a more precise local investigation; review
those names before sharing. Values stay omitted. Nothing is uploaded, repaired,
installed, or booted by this command.

Exit `0` means the observation completed, not that the installation is healthy.
Exit `2` means inspection could not complete. The report is bounded to 1 MiB
and 256 files per package member, prioritizing differences; omitted counts are
explicit. It is not an atomic snapshot or an edit history. Package inspection
still hashes the selected owner folders, so a large package can take time.

The ordinary Doctor and raw client logs remain local diagnostics and can
include private paths or details. They are not the scoped configuration
export. Inspect the scoped report first; if a maintainer needs a
specific setting afterward, share only the relevant sanitized value.

## macOS will not open Octopus, or blocks its tools

The release is ad-hoc signed and not notarized. A disk image downloaded with a
browser is quarantined, and so is everything copied out of it.

- **"Octopus" Not Opened, or "cannot be opened because Apple cannot check it"**
  - clear the quarantine of the installed app once:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Octopus.app
  ```

- **The app opened, but tools, provider lives, or `codex` fail to start, or
  macOS asks about them one by one** - the app was admitted with **Open
  Anyway** while still quarantined, and the packages it installed into your Home
  kept the mark. Quit Octopus and its runtime, then clear both:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Octopus.app
  xattr -dr com.apple.quarantine "$HOME/.octopus"
  ```

  Use your `OCTOPUS_HOME` if it is not the default.
- **The path in an error contains `AppTranslocation`** - Octopus was opened
  from the disk image or the Downloads folder. Move it to Applications, clear
  the quarantine as above, and open it from there.

The installer from [Getting started](docs/getting-started.md#install-the-app)
downloads with `curl` and avoids all three.

## Setup cannot run python3 or git

For Apple's `python3` and `git` shims, Octopus uses the active developer
directory reported by `xcode-select --print-path`. The installed Command Line
Tools or Xcode must provide the requested command, as described in
[Apple's command-line tools settings](https://developer.apple.com/documentation/xcode/configuring-command-line-tools-settings).
Check the selection without
changing it:

```sh
xcode-select --print-path
xcrun --no-cache --find git
xcrun --no-cache --find python3
```

If no developer tools are installed, use `xcode-select --install`. If the
selection names a missing installation, select the tools you actually installed
with `sudo xcode-select --switch <developer-directory>`. A missing
`/private/var/select/developer_dir` alone does not mean the tools are unavailable:
macOS can select its default installation without that symlink.

Doctor checks the runtime dependencies of life executables and their declared
commands and files even when a life has no diagnostic probe. A passing check
does not settle a previously interrupted tool call. Keep a failed Home closed
while collecting its saved evidence; do not delete continuity or pending-result
records to force a retry.

## The build fails

- **`Packaging native PiP requires explicit OCTOPUS_NATIVE_NODE_HEADERS`**  - 
  run `node app/native/runtime.cjs headers` and set the variable to the
  directory it prints, as shown in [Getting started](docs/getting-started.md#build).
  `Native headers must be exactly Node 24.22.1` means the variable points at
  headers of another Node.
- **Native rebuild or SQLite check errors** - you are on Node 20 or 22. Use
  Node 24.
- **Rust errors about the edition or a locked dependency** - update to current
  stable Rust.
- **The packager built the wrong architecture** - it builds for the
  architecture of the Node process running it. Use a native arm64 Node on Apple
  silicon.

## The launch terminal shows an error, or nothing happens after Enter

- **The Home path is too long.** macOS limits Unix socket paths to about 104
  bytes, and the runtime keeps its sockets inside the Home. Use a short path
  such as `~/.octopus` or `~/.octopus-preview`.
- **The Home path goes through a symlink.** `/tmp` is a symlink on macOS. Point
  `OCTOPUS_HOME` at a real path under your home directory.
- **Another runtime already holds this Home.** One runtime per Home. Quit the
  other app instance, or run `octopus down` with the same `OCTOPUS_HOME`.

## Sign in with ChatGPT does not complete

- Finish the flow in the browser window that opened, then return to the app.
- Cancel in the app and start again if the browser tab was closed.
- The account must have access to the model you select. Availability and usage
  limits belong to the provider account, not to Octopus.
- The sign-in is shared with Codex CLI through `~/.codex`. An API-key login
  there asks you to sign in with ChatGPT; see
  [Codex CLI](docs/getting-started.md#codex-cli) to keep the two apart.

## Setup says it needs attention

Open the newest file in `<home>/client/setup-state/operations/` and read
`last_error`. Use the app's retry after fixing what it names. Setup is a
transaction with recorded steps; retrying continues from the step that failed.

## Home does not reply

1. Check that the provider interface is alive: read the newest
   `interface-life.stderr.*.log` under the Home's provider interface folder in
   `<home>/interfaces/`.
2. Search the end of `ledger/movements.jsonl` for `DeliveryRefused`. The row
   names the reason - `policy`, `no_binding`, `checkpoint_spent`, and so on  - 
   and the run and checkpoint it happened on.
3. Send a short message again after the cause is fixed. A run is data; nothing
   needs to be restarted for it to continue.

## A tool call was refused

That is usually the system working. The refusal row in the ledger says whether
the route was outside the run's grant (`policy`), had no authored binding
(`no_binding`), or failed its verifier (`verification_failed`). Grant the
project the driver or application it needs in the app rather than editing the
run record by hand.

## Open a useful bug report

Include the commit, macOS version and architecture, the steps, what you
expected, and what happened. For installation/configuration issues, attach the
scoped report above and identify the public package/version separately. For a
runtime failure, include the relevant `last_error` or refusal row. Remove
credentials, sign-in URLs, private prompts, project paths, and artifact contents
from those raw excerpts. Do not attach the whole Home.
