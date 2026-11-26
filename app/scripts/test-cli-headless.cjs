const assert = require("node:assert/strict");
const { existsSync } = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { createHash } = require("node:crypto");
const { execFileSync, spawnSync } = require("node:child_process");
const { before, after, test } = require("node:test");
const { createRuntimePackager } = require("./runtime-package.cjs");
const { readLocalPackageCatalog, inspectLocalPackageInstallation } = require("../electron/localPackages.cjs");

const repo = path.resolve(__dirname, "../..");
const targetRoot = process.env.CARGO_TARGET_DIR || path.join(repo, "target");
const forbidden = ["driver.desktop_session", "driver.audio_device_session", "driver.browser_session", "world_provider_realtime"];
const keyName = "OCTOPUS_PROVIDER_OPENROUTER_API_KEY";
const modelName = "OCTOPUS_PROVIDER_OPENROUTER_MODEL";
const dummyKey = "offline-synthetic-key-not-a-real-credential";
let root, bundle;
const readJson = async (file) => JSON.parse(await fs.readFile(file, "utf8"));

before(async () => {
  execFileSync("cargo", ["build", "--locked", "--workspace", "--bins"], { cwd: repo, stdio: "pipe", timeout: 600_000 });
  const provider = "registry/interface/world_provider_codex/packages/octopus-provider-codex";
  const bridge = path.join(repo, provider, "opencodex-bridge");
  if (!existsSync(path.join(bridge, "node_modules"))) {
    execFileSync("npm", ["ci"], { cwd: bridge, stdio: "pipe", timeout: 120_000 });
  }
  execFileSync("npm", ["run", "build"], { cwd: bridge, stdio: "pipe", timeout: 120_000 });
  const tmpBase = process.platform === "darwin" ? "/private/tmp" : os.tmpdir();
  root = await fs.realpath(await fs.mkdtemp(path.join(tmpBase, "oct-headless-")));
  bundle = path.join(root, "bundle");
  const builder = createRuntimePackager({ repoRoot: repo, appRoot: path.join(repo, "app"), outputRoot: bundle });
  builder.copyRegistryBaseline(path.join(repo, "registry"), path.join(bundle, "registry"), path.join(repo, "app/distribution/system-baseline.json"));
  builder.copyRegistrySurface(path.join(repo, "registry"), path.join(bundle, "local-packages/registry"));
  builder.copyLocalPackageDefinitions(path.join(repo, "app/distribution/local-packages"), path.join(bundle, "local-packages"));
  builder.bundleManagedCodex([path.join(bundle, "local-packages/registry")]);
  await fs.mkdir(path.join(root, "Library/Application Support"), { recursive: true });
  await fs.mkdir(path.join(bundle, "target/debug"), { recursive: true });
  for (const entry of await fs.readdir(path.join(targetRoot, "debug"), { withFileTypes: true })) {
    const source = path.join(targetRoot, "debug", entry.name);
    if (entry.isFile() && entry.name.startsWith("octopus") && (await fs.stat(source)).mode & 0o111) {
      await fs.link(source, path.join(bundle, "target/debug", entry.name));
    }
  }
  await fs.mkdir(path.join(bundle, "bin"), { recursive: true });
  await fs.link(path.join(targetRoot, "debug/octopus"), path.join(bundle, "bin/octopus"));
  const sha = async (file) => createHash("sha256").update(await fs.readFile(file)).digest("hex");
  const owner = path.join(bundle, "local-packages/registry/interface/world_provider_codex/packages/octopus-provider-codex");
  for (const [subdir, binary] of [["", "octopus-provider-opencodex-life"], ["intake", "octopus-input-intake-life"]]) {
    const directory = path.join(owner, subdir);
    await fs.rm(path.join(directory, "life-bin"), { recursive: true, force: true });
    const life = path.join(directory, "life-bin", binary);
    await fs.mkdir(path.dirname(life), { recursive: true });
    await fs.copyFile(path.join(targetRoot, "debug", binary), life);
    const receipt = { schema_version: 1, executable: "life-bin/" + binary,
      executable_sha256: await sha(life), executable_bytes: (await fs.stat(life)).size,
      inputs: { "Cargo.lock": await sha(path.join(repo, "Cargo.lock")),
        "Cargo.toml": await sha(path.join(repo, provider, "Cargo.toml")) } };
    await fs.writeFile(path.join(directory, "build-receipt.json"), JSON.stringify(receipt) + "\n");
  }
  require("esbuild").buildSync({ entryPoints: [path.join(repo, "app/electron/clientDoctor.cjs")],
    outfile: path.join(bundle, "client-doctor.cjs"), bundle: true, platform: "node", target: "node24", format: "cjs" });
});
after(async () => { if (root) await fs.rm(root, { recursive: true, force: true }); });

function cli(home, args, extraEnv = {}) {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("OCTOPUS_") && key !== "CODEX_HOME"));
  const result = spawnSync(process.execPath, [path.join(repo, "app/cli/index.cjs"), ...args, "--home", home, "--bundle", bundle], {
    cwd: repo, encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024,
    env: { ...env, HOME: root, CODEX_HOME: path.join(root, "no-account"), OCTOPUS_NODE: process.execPath, ...extraEnv },
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.ok(!String(result.stdout).includes(dummyKey) && !String(result.stderr).includes(dummyKey), "credentials leaked into CLI output");
  if (result.status !== 0) {
    const error = new Error(result.stdout || result.stderr || result.error?.message || "CLI failed");
    error.stdout = result.stdout; error.stderr = result.stderr; throw error;
  }
  return result.stdout;
}
function roots(home) {
  return { sourceRegistryRoot: path.join(bundle, "local-packages/registry"), libraryRoot: path.join(bundle, "local-packages"),
    activeRegistryRoot: path.join(home, "registry"), stateRoot: path.join(home, "client/package-state") };
}
async function setup(name, pack = "headless", model = "anthropic/claude-sonnet-5", extraEnv = {}) {
  const home = path.join(root, name);
  const output = cli(home, ["setup", "--non-interactive", "--no-start", "--pack", pack, "--workspace-name", "Offline server",
    "--project-name", "Server", "--provider", "world_provider_openrouter", "--model", model, "--json"], { [keyName]: dummyKey, ...extraEnv });
  assert.ok(!output.includes(dummyKey));
  const result = JSON.parse(output);
  assert.equal(result.status, "configured"); assert.equal(result.runtime.skipped, true);
  await assert.rejects(fs.access(path.join(home, "state/runtime-instance")), { code: "ENOENT" });
  const file = path.join(home, "interfaces/world_provider_openrouter/environment.json");
  assert.equal((await fs.stat(file)).mode & 0o777, 0o600);
  assert.equal((await readJson(file)).values[keyName], dummyKey);
  const provider = await inspectLocalPackageInstallation({ ...roots(home), packageId: "world_provider_openrouter" });
  assert.equal(provider.receipt_state, "valid");
  for (const eeId of [result.workspace.receipt.root_ee_id, ...result.workspace.receipt.project_ee_ids.map((row) => row.ee_id)]) {
    const ee = await readJson(path.join(home, "state/ee_instances", eeId + ".json"));
    assert.ok(ee.config.octopus_os.application_defaults.includes("world_provider_openrouter"), "selected provider must be a Project application default");
  }
  async function checkNoLeak(dir) {
    for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (p === file) continue;
      if (entry.isDirectory()) await checkNoLeak(p);
      else if (entry.isFile() && /\.(json|jsonl|log)$/.test(entry.name)) assert.ok(!(await fs.readFile(p, "utf8")).includes(dummyKey), p);
    }
  }
  await checkNoLeak(home);
  return { home, result };
}

test("headless manifest validates and its complete optional closure excludes desktop/browser/audio/voice", async () => {
  const catalog = await readLocalPackageCatalog({ ...roots(path.join(root, "catalog")), packId: "headless" });
  assert.deepEqual(catalog.errors, []);
  const pack = catalog.packs.find((row) => row.id === "headless");
  assert.ok(pack);
  const ids = new Set(pack.components.map((row) => row.package_id));
  const packages = new Map(catalog.packages.map((row) => [row.id, row]));
  function closure(id, visited = new Set()) {
    if (visited.has(id)) return visited;
    visited.add(id); const pkg = packages.get(id); assert.ok(pkg?.available, pkg?.error || id);
    for (const dep of pkg.required_packages ?? []) closure(dep, visited);
    return visited;
  }
  for (const id of ids) for (const dep of closure(id)) assert.ok(!forbidden.includes(dep), dep);
  for (const target of [pack.workspace_setup.home, pack.workspace_setup.task]) {
    assert.ok(!target.selected_worker_environment_kinds.some((id) => ["browser_session", "desktop_session", "audio_device_session"].includes(id)));
  }
  assert.equal(pack.workspace_setup.home.voice, undefined);
  const descriptor = await readJson(path.join(bundle, "local-packages/registry/interface/world_provider_openrouter/interface.json"));
  assert.ok(!descriptor.interface_policy.variables.models.default.some((id) => id.endsWith(":free") || id === "*"));
  assert.equal(descriptor.interface_driver_config.private_config.model, "anthropic/claude-sonnet-5");
});

test("offline headless bootstrap has valid receipts and real doctor PASS without starting a daemon", async () => {
  const { home, result } = await setup("headless");
  for (const id of forbidden) assert.equal((await inspectLocalPackageInstallation({ ...roots(home), packageId: id })).installed, false);
  for (const pkg of result.setup.plan.packages) assert.equal((await inspectLocalPackageInstallation({ ...roots(home), packageId: pkg.id })).receipt_state, "valid", pkg.id);
  const run = await readJson(path.join(home, "state/runs", result.workspace.receipt.home_run_id + ".json"));
  assert.equal(run.interface_state[result.workspace.receipt.home_interface_id].voice_context, undefined);
  assert.ok(!(await fs.readdir(path.join(home, "registry/interface"))).some((id) => id.includes("home_voice_")));
  const doctor = cli(home, ["doctor"]);
  assert.match(doctor, /result\s+PASS/); assert.doesNotMatch(doctor, /WARN|unavailable interface target/i);
  const repeat = await setup("headless");
  assert.equal(repeat.result.changed, false);
});

test("explicit custom OpenRouter model uses its declared alias and private model environment", async () => {
  const { home, result } = await setup("explicit-model", "headless", "example/explicit-model:free");
  assert.equal(result.setup.plan.workspace.request.home.providerModel, "openrouter-model");
  assert.deepEqual(result.setup.plan.provider.model_override, { env_key: modelName, value: "example/explicit-model:free" });
  const file = path.join(home, "interfaces/world_provider_openrouter/environment.json");
  assert.equal((await readJson(file)).values[modelName], "example/explicit-model:free");
  assert.match(cli(home, ["doctor"]), /result\s+PASS/);
  assert.equal((await setup("explicit-model", "headless", "example/explicit-model:free")).result.changed, false);
});

test("OpenRouter alias accepts the explicit model environment and keeps its key private", async () => {
  const { home } = await setup("env-model", "headless", "openrouter-model", { [modelName]: "example/operator-chosen" });
  assert.equal((await readJson(path.join(home, "interfaces/world_provider_openrouter/environment.json"))).values[modelName], "example/operator-chosen");
});

test("codex pack still bootstraps with explicitly selected OpenRouter and preserved defaults", async () => {
  const { home } = await setup("codex-openrouter", "codex");
  for (const id of forbidden) assert.equal((await inspectLocalPackageInstallation({ ...roots(home), packageId: id })).receipt_state, "valid", id);
});

test("invalid model IDs, missing alias targets and unknown providers fail closed before Home bootstrap", async () => {
  for (const [name, provider, model] of [["bad-model", "world_provider_openrouter", "../unsafe"], ["no-alias-target", "world_provider_openrouter", "openrouter-model"], ["bad-provider", "world_provider_missing", "example/model"], ["catalogue-only", "world_provider_gemini", "example/custom-model"]]) {
    const home = path.join(root, name);
    let error;
    try { cli(home, ["setup", "--non-interactive", "--no-start", "--pack", "headless", "--workspace-name", "Invalid",
      "--provider", provider, "--model", model, "--json"], { [keyName]: dummyKey }); } catch (caught) { error = caught; }
    assert.ok(error);
    assert.match(String(error.stdout), name === "bad-provider" ? /no unique installable local package/ : name === "catalogue-only" ? /no upstream model override/ : /no valid declared model override/);
    assert.ok(!String(error.stdout).includes(dummyKey) && !String(error.stderr).includes(dummyKey));
    await assert.rejects(fs.access(path.join(home, "host.json")), { code: "ENOENT" });
  }
});
