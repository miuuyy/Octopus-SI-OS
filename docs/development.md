# Development

Use [Getting started](getting-started.md) for the macOS build,
[Architecture](architecture.md) for the picture, and the
[contracts](contracts/README.md) for what each primitive guarantees.

## Source map

| Path | What lives there |
| --- | --- |
| [crates/](../crates/) | The kernel and its generic hosts. Nothing here knows a provider, a channel, or a tool by name. |
| [registry/interface/](../registry/interface/) | Every interface descriptor, every worker pin, and each interface's own driver code. |
| [registry/worker_environment_kind/](../registry/worker_environment_kind/) | Room descriptors with their executors and tool binaries. |
| [registry/tool/](../registry/tool/) | Tool descriptors pointing at a room's executables. |
| [registry/ee_template/](../registry/ee_template/) | Goals and their policy. |
| [registry/artifact/](../registry/artifact/) | Declared context artifacts: roles, grounding, skills. |
| [registry/mark/](../registry/mark/), [registry/verifier/](../registry/verifier/) | Outcome vocabulary and check packages. |
| [schemas/](../schemas/) | JSON Schemas for run records, movements, grants, verdicts, marks, verifiers. |
| [app/src/](../app/src/), [app/electron/](../app/electron/) | The desktop client: React presentation and Electron host operations. |
| [app/cli/](../app/cli/) | Terminal presentation over the shared client projections, authoring, setup, and artifact transports. |
| [app/distribution/](../app/distribution/) | The baseline registry, local application packs, generated-output declarations. |
| [app/schemas/](../app/schemas/) | Client-side JSON contracts: setup, packages, surfaces, extensions. |

### The kernel crates

| Crate | Role |
| --- | --- |
| `octopus-app` | The `octopus` binary: `up`, `down`, `doctor`. Boots a Home and runs the arrival loop. |
| `octopus-app-server` | The composition root: registry snapshot, artifact arrival handling, and the worker-policy wiring. |
| `octopus-runtime` | Grant formation per arrival, admission, and outbound transfers. |
| `octopus-run-record` | Run and checkpoint records, grant epochs, and the run-records index. Rooms and interfaces read and author runs through it without linking the runtime. |
| `octopus-policy` | Policy shapes and their reduction: worker-interface ceilings and interface policy declarations. |
| `octopus-ee` | EE templates, instances, and the EE tree. |
| `octopus-interfaces` | Interface descriptors and worker-pin policy loading. |
| `octopus-interface-transport` | Interface life processes, hand-in, activation, and live reconciliation. |
| `octopus-interface-projection` | What an interface is handed: binding, affordance, metadata, and registry projections. |
| `octopus-descriptors` | The one strict JSON gate every descriptor crosses, and descriptor hashing. |
| `octopus-tool-registry` | Tool descriptor loading and argument validation. |
| `octopus-tool-handlers` | The passive tool-call and tool-result contract. No executable family lives here. |
| `octopus-worker-env` | Room descriptors, leasing, and the authorization gate. Room behavior lives in kind packages. |
| `octopus-marks` | Mark descriptors and the verification input and verdict types. |
| `octopus-verifiers` | The blind verifier runner: pins a package and executes its script. |
| `octopus-ledger`, `octopus-ledger-store` | Movement types and the append-only log. |
| `octopus-ledger-index` | The in-memory arrival index and the disposable SQLite index. |
| `octopus-schemas` | The published JSON Schemas. |

Adding a capability over an existing driver or room is descriptors and
bindings. A genuinely new execution family is code inside its concrete owner
folder under `registry/`. Do not put provider names, artifact semantics, or
product behavior into the crates' decision paths. See
[Extending Octopus](contracts/EXTENSION_SURFACES.md).

The [file observer example](../registry/interface/world_file_observer/examples/binding_repair.py)
demonstrates selected files becoming immutable input artifacts, a checkpoint
waiting for two independently verified snapshots, and catch-up after repairing
an exact interface binding. Its `--read-root` argument grants an explicit
existing directory; the command prints the proposed files and steps without
installing them. The observer is an interface package, while waiting and
replay use existing runtime mechanisms. Use `./octopus inspect <run-id> --json`
to inspect the resulting checkpoint and movement evidence.

## Source and Home

JSON is part of the program here. A worker ceiling or interface binding can
change behavior as much as a source edit, so a commit alone does not describe
what a user's installation is running.

![Optional clients connect to an independent runtime; the runtime reads and records Home state, while repository packages are installed into the Home registry](images/architecture/client-runtime-home.png)

The diagram separates running code from persistent files. Repository packages
reach Home through the [distribution and installation path](client.md#where-packages-come-from);
client preferences and provider credentials have the separate locations below.

| Location | Owner and purpose |
| --- | --- |
| `registry/`, `app/distribution/`, schemas | Versioned source: reusable code, descriptors, package definitions, and validation rules. |
| `Octopus.app` | A built snapshot: client, runtime, startup baseline, and optional local packages. |
| `<home>/registry/` | Installed, active descriptors and package code, including local configuration and authorized mutations. |
| Other Home files | Machine roots, runs, EEs, ledger, blobs, installation receipts, and private interface state. |
| `<home>/client/` / provider state | The client profile of that Home, including preferences and Extensions / provider-owned credentials, which `OCTOPUS_HOME` does not isolate. |

The normal loop is **edit the owning source or descriptor → run focused checks
→ build the changed distribution → exercise it against an explicitly selected
test Home → inspect the run and movement evidence**. For client presentation,
the renderer preview is the shorter loop described below. An edit in the
checkout does not automatically update a packaged app or an already-installed
package in a Home.

Octopus is a research preview. Incompatible persisted formats require a fresh
Home through normal setup. There are no schema migrations, predecessor tables,
automatic package adoption, or startup repairs of missing fields in old state.
The bundled registry initializes a new Home once; later starts use the installed
registry. Explicit package installation and recovery of interrupted operations
in the current format remain supported. Preserve sign-in credentials when
resetting a disposable Home; do not transplant old runs, checkpoints or ledgers.

Authorized local changes to descriptors, role artifacts and package code do
not invalidate an installed package. Doctor validates active contracts and
runs declared probes; it does not compare installed bytes with the bundle.

A reusable fix belongs in the source package and its checks. A user's token,
machine path, run, or personal context belongs in private state. Reproduce a
bug with the smallest relevant package/configuration and recorded evidence;
do not turn a user's Home into a source fixture. See
[Troubleshooting](../TROUBLESHOOTING.md) for collecting diagnostics and
[the client guide](client.md#where-packages-come-from) for the installation path.

## Discipline

- **Fix the contract, not the symptom.** Failures here are almost always
  architectural - the wrong data shown to the model, a missing descriptor
  field - not a capability gap. Hardcoding around one hides a problem in the system behind a workaround
  for the model.
- **No branch on identity.** Not on a worker pin, a provider, a model string,
  or an artifact label.
- **Fail closed.** A clear error beats a fallback that almost works. No silent
  semantic fallback, no placeholder success.
- **Leave evidence.** A new side effect produces an artifact and a movement.
- **Tests follow the framework.** Add a contract test when a primitive is
  stable. Keep test code well under half of a change, and do not pin
  implementation details.
- **Comments describe documented architecture or nothing.** No promises, no
  migration notes, no explanations of why a change is correct.

## Checks

Install dependencies and use the Node/Rust toolchains in the build guide. From
the repository root, run the checks relevant to a change:

```sh
cargo check --locked --workspace --all-targets
cargo test --locked -p octopus-policy --lib
cargo test --locked -p octopus-runtime --lib
cargo test --locked -p octopus-ledger-index --lib
```

For client setup and presentation contracts:

```sh
cd app
npx tsc --noEmit --incremental false
npm run check:design-system
npm run test:workspace-setup
npm run test:client-setup
npm run test:chat-trace-contracts
```

The [quality workflow](../.github/workflows/quality.yml) lists the broader checks
and platform dependencies. It runs Rust contracts through `cargo nextest`, one
process per test; [`.config/nextest.toml`](../.config/nextest.toml) runs the
Code Mode, process session and file publication tests one at a time.
Source/type checks and contract tests do not prove a
fresh packaged setup, a real model reply, or physical device permissions.

For the terminal client, `npm --prefix app run test:cli` checks transcript,
interaction, transport, packaging, and setup contracts. `npm --prefix app run
build:cli` builds its release distribution; `build:cli-client` rebuilds only the
shared TypeScript client functions. Exercise the resulting CLI with an explicit
test Home for real task and tool acceptance. The terminal imports the existing
client libraries and installed chat interface; it does not create a second task
index or runtime control API.

Run the Doomsday latency budget separately in release mode:

```sh
cargo test --locked --release -p octopus-app-server --test doomsday -- --ignored --nocapture
```

The budget is **1,000 ms of framework overhead** for a reference 5,000 ms model
response. It measures 20 artifact round trips per scenario through policy,
durable storage and verified marks: one project EE, then 1,000 project EEs and
background run records with a maximum depth of 30. Cold and warm exchanges must
both fit. Provider replies are boundary fixtures; model latency, client work and
initial setup are outside the timed exchange. No user Home or live provider is used.

### CI triggers and local results

`quality` runs when a pull request is opened, updated with new commits,
reopened, or marked ready for review. It also runs on pushes, except changes
limited to `docs/` and the root `README.md`, and supports **Run workflow**.
A newer run cancels an older quality run for the same pull request, branch or
tag. Pull request jobs use read-only repository permissions; GitHub may require
maintainer approval before running a fork's contribution. `doomsday` is manual
only. `release` runs for a pushed `v*` tag and on **Run workflow**; see
[Releases](#releases). None of them has a schedule or an automatic retry loop.

Local checks use no GitHub-hosted runner minutes. When publishing a local
commit status, name the checks actually run, attach it to the exact tested
commit, and retain the command results. A local check does not establish that
the Linux and macOS hosted jobs passed. Billing-blocked jobs have not tested
the code and must not be relabeled as successful tests.

From the repository root, check publication inputs with:

```sh
node app/scripts/check-public-source.cjs
```

That check reads the selected Git blobs and declared package inputs. Required
public files must be selected, and the publication controls must match the
checked files. Use it before publishing a checkout; `--worktree` skips the
staged-file inventory. It does not establish asset provenance or runtime acceptance.

## Releases

A release is the packaged app as a disk image,
`Octopus-<version>-macos-<arch>.dmg`, with `install.sh` and `checksums.txt`.
The version is `version` in `app/package.json`; the packager writes it into
the bundle, and the release tag is `v<version>`.

After `npm --prefix app run package:mac`, build the release assets locally with:

```sh
node app/scripts/release-macos.cjs
```

The script checks the bundle (version, identifier, minimum macOS,
architecture, managed Codex receipt, code signature), then builds the disk
image on an APFS sparse image in which byte-identical files are clones of one
copy. The bundle carries the same executables in several package owners, and
the clones keep the compressed image close to the size of the unique content.
It mounts the result, compares every path, mode, and SHA-256 with the bundle,
verifies the signature inside the image, and writes `app/build/release/` and
`app/build/release-notes.md` from [app/release](../app/release). Nothing is
uploaded.

The `release` workflow does the same on a hosted macOS runner. **Run workflow**
is a dry run whose assets stay on the run for seven days. Pushing a tag that
matches `app/package.json` builds the assets and creates a **draft** GitHub
release; it becomes public only when a maintainer publishes the draft. Asset
digests reported by GitHub are checked against `checksums.txt`.

Quit the client and runtime before replacing the release. If the release changes
persisted contracts, initialize a fresh Home and retain only sign-in credentials.
Existing runs and ledgers are not upgraded.

## Client iteration and acceptance

For a browser renderer preview, from the repository root:

```sh
npm --prefix app run dev
```

Vite serves the renderer on the local address it prints. Electron-only operations
and runtime-backed flows require the desktop app; the browser preview cannot
stand in for their acceptance.

Test the built `app/build/Octopus.app` with its bundled release runtime and
capability executables. Use a dedicated test Home and the actual onboarding
flow. Keep real user Homes, credentials, and accepted UI behavior intact. An
integration needing login or macOS permissions must be exercised separately;
missing credentials are an explicit limit on the result.

The fresh packaged onboarding runner documents its modes with:

```sh
npm --prefix app run dev:onboarding -- --help
```

After preparing the build prerequisites, run both modes from the repository root:

```sh
OCTOPUS_NATIVE_NODE_HEADERS="$PWD/app/build/node-v24.15.0/include/node" \
  npm --prefix app run dev:onboarding -- --mode unauthenticated

npm --prefix app run dev:onboarding -- --mode reused-auth \
  --auth-file /absolute/path/auth.json --target task --no-build
```

Each run creates empty, isolated client, runtime, and provider Homes. The first
mode builds and signs the app, walks welcome, opens browser sign-in, and cancels
it. It does not claim a completed login. The second copies only the selected
credential, completes setup, and requires rendered replies in Home and a new
task. It removes the credential copy afterward.

`--no-build` requires a previous successful harness build whose source and bundle
hashes still match. Reports and screenshots stay in ignored
`.private/onboarding/`. Read the report's requested boundary and result before
calling onboarding verified; the runner does not certify every capability or
macOS permission.

Change schema-owned data explicitly and validate it at its owner. Do not hide
malformed old data behind startup migrations, fallback models, or a degraded
success path. Report any source/documentation mismatch and what remains
unverified in the change description.
