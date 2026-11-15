const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const net = require("node:net");
const vm = require("node:vm");
const crypto = require("node:crypto");
const { promisify } = require("node:util");
const { execFile } = require("node:child_process");
const { setTimeout: delay } = require("node:timers/promises");
const { resolveEnvironment, createEnvironment } = require("../cli/environment.cjs");
const { compileSetupPlan, setupReference, ensureSignIn } = require("../cli/setup.cjs");
const { createClientSetupStore } = require("../electron/clientSetup.cjs");
const { readWorkspaceSetupState } = require("../electron/workspaceSetup.cjs");
const { recoverWorkspaceTransactions } = require("../electron/workspaceSetup.cjs");
const { commitProjectionClientAuthoring } = require("../electron/clientWorkspaceAuthoring.cjs");
const { readProviderInterfaceDescriptors } = require("../electron/projection.cjs");
const { createClientProviderMetadata } = require("../electron/clientProviderMetadata.cjs");
const { installLocalPackages, inspectLocalPackageInstallation } = require("../electron/localPackages.cjs");

const execute = promisify(execFile);

async function main() {
  const root = await fs.realpath(await fs.mkdtemp("/tmp/octo-cli-"));
  let runtime = null;
  let verified = false;
  try {
    await resolution(root);
    await fixtureRuntime(root);
    await setupEvidence(root);
    await signInContract();
    if (process.argv[2]) {
      assert.equal(process.argv[2], "--runtime-root");
      assert.ok(path.isAbsolute(process.argv[3]));
      runtime = createEnvironment({ home: path.join(root, "real"), installationRoot: process.argv[3] });
      await genuineSetup(runtime);
    }
    verified = true;
    console.log(`CLI setup contracts passed${runtime ? "; genuine isolated Home setup and endpoint verified" : ""}.`);
  } finally {
    if (runtime) runtime.close();
    if (runtime && verified && process.argv.includes("--keep-home")) console.log(`Retained isolated Home: ${runtime.home}`);
    else { if (runtime) await stop(runtime); await fs.rm(root, { recursive: true, force: true }); }
  }
}

async function resolution(root) {
  const userHome = path.join(root, "user"), selected = path.join(root, "selected"), explicit = path.join(root, "explicit");
  await fs.mkdir(path.join(userHome, ".octopus"), { recursive: true });
  await fs.writeFile(path.join(userHome, ".octopus", ".env"), `OCTOPUS_HOME=${selected}\nOCTOPUS_CLIENT_ADMIN_SECRET=private-test-administrator\n`, { mode: 0o600 });
  const options = { userHome, env: {}, repoRoot: path.resolve(__dirname, "../..") };
  const resolved = resolveEnvironment(options);
  assert.equal(resolved.home, selected);
  assert.equal(resolved.installationRoot, path.join(options.repoRoot, "app/build/cli/runtime"));
  assert.equal(resolved.packageRoots.sourceRegistryRoot, path.join(resolved.installationRoot, "local-packages/registry"));
  assert.equal(resolved.codexBinary, path.join(resolved.installationRoot, "bin/codex"));
  assert.ok(!JSON.stringify(resolved).includes("private-test-administrator"));
  await fs.mkdir(explicit, { recursive: true });
  await fs.writeFile(path.join(explicit, ".env"), `OCTOPUS_HOME=${selected}\nCODEX_HOME=${path.join(root, "auth-owner")}\n`, { mode: 0o600 });
  assert.equal(resolveEnvironment({ ...options, home: explicit }).home, explicit);
  assert.equal(resolveEnvironment({ ...options, home: explicit }).codexHome, path.join(root, "auth-owner"));
  assert.equal(resolveEnvironment({ ...options, home: "relative", cwd: root }).home, path.join(root, "relative"));
  assert.equal(resolveEnvironment({ ...options, bundle: path.join(root, "Octopus.app") }).sourceRegistry,
    path.join(root, "Octopus.app/Contents/Resources/app/registry"));
  assert.throws(() => resolveEnvironment({ ...options, runtimeBinary: path.join(root, "target/debug/octopus") }), /release runtime/);
  assert.throws(() => resolveEnvironment({ ...options, env: { CODEX_HOME: "relative" } }), /CODEX_HOME must be an absolute/);
  assert.throws(() => resolveEnvironment({ ...options, home: `/${"a".repeat(140)}` }), /shorter --home/);
  await fs.chmod(path.join(explicit, ".env"), 0o644);
  assert.throws(() => resolveEnvironment({ ...options, home: explicit }), /0600/);
  await fs.chmod(path.join(explicit, ".env"), 0o600);
  const environment = createEnvironment({ ...options, home: explicit });
  assert.equal(environment.accountClient(), environment.accountClient());
  environment.close();
}

// The fixture runtime writes its instance record first and binds its control socket 300 ms
// later, as the real runtime does in that order, so every start probes the gap between them.
async function fixtureRuntime(root) {
  const installationRoot = path.join(root, "distribution"), home = path.join(root, "fixture");
  for (const directory of ["bin", "registry/interface", "local-packages/registry/interface", "local-packages/packages", "local-packages/packs"]) {
    await fs.mkdir(path.join(installationRoot, directory), { recursive: true });
  }
  for (const file of ["registry/.octopus-source.json", "local-packages/registry/.octopus-source.json"]) await json(path.join(installationRoot, file), { schema_version: 1, id: "test-source" });
  const binary = path.join(installationRoot, "bin/octopus");
  await fs.writeFile(binary, `#!${process.execPath}\nconst fs=require('node:fs'),net=require('node:net'),path=require('node:path'),crypto=require('node:crypto');
if(process.argv[2]==='project-ee-policies'){console.log(JSON.stringify({environments:[],issues:[]}));process.exit(0)}
if(process.argv[2]!=='up')process.exit(64);
const home=process.env.OCTOPUS_HOME, state=path.join(home,'state/runtime-instance'), now=Date.now();
fs.mkdirSync(state,{recursive:true});fs.appendFileSync(path.join(home,'starts'),'start\\n');
let phase='starting';function writeState(){fs.writeFileSync(path.join(state,'state.json'),JSON.stringify({schema_version:3,pid:process.pid,phase,started_at_unix_ms:now,updated_at_unix_ms:Date.now(),launch_owner:null}))}writeState();
const folder=path.join(home,'app-server-control');fs.mkdirSync(folder,{recursive:true});
const endpoint=path.join(folder,crypto.createHash('sha256').update('fixture_home').digest('hex').slice(0,24)+'.sock');
const server=net.createServer(socket=>socket.end());
const control=net.createServer(socket=>{let input='';socket.on('data',bytes=>{input+=bytes;if(!input.includes('\\n'))return;let response;try{const request=JSON.parse(input);if(Object.keys(request).join(',')!=='request'||request.request!=='status')throw new Error('Fixture accepts only a strict status request');response={outcome:'status',pid:process.pid,phase,started_at_unix_ms:now,stopping:null,interface_failures:[]}}catch(error){response={outcome:'refused',error:error.message}}socket.end(JSON.stringify(response)+'\\n')})});
setTimeout(()=>control.listen(path.join(state,'control.sock'),()=>server.listen(endpoint,()=>setTimeout(()=>{phase='ready';writeState()},100))),300);
process.on('SIGTERM',()=>{let closed=0;const finish=()=>{if(++closed===2){fs.rmSync(state,{recursive:true,force:true});process.exit(0)}};server.close(finish);control.close(finish)});
`, { mode: 0o700 });
  const options = { home, installationRoot, env: { PATH: process.env.PATH } };
  const blank = createEnvironment({ ...options, home: path.join(root, "blank") });
  const blankProgress = [];
  try {
    await blank.start({ interfaceId: "fixture_home", timeoutMs: 5000, onProgress: (row) => blankProgress.push(row) });
    await assert.rejects(fs.access(path.join(blank.home, "registry/interface")), { code: "ENOENT" });
  } finally {
    const current = await blank.status({ interfaceId: "fixture_home" });
    if (current.pid) { process.kill(current.pid, "SIGTERM"); await waitStopped(blank); }
    blank.close();
  }
  const first = createEnvironment(options), second = createEnvironment(options);
  const source = path.join(first.packageRoots.sourceRegistryRoot, "interface", "world_cli_fixture", "interface.json");
  const active = path.join(first.packageRoots.activeRegistryRoot, "interface", "world_cli_fixture", "interface.json");
  const original = { id: "world_cli_fixture", interface_type: "world", role: "provider", direction: "bidirectional", source_names: ["fixture"], config: { model: "default" }, contract: 1 };
  await json(source, original);
  await json(path.join(first.packageRoots.libraryRoot, "packages", "cli.fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "cli.fixture", version: "1.0.0", product_kind: "application", product_id: original.id, release_status: "optional",
    display: { name: "Fixture", description: "CLI installed package isolation", developer: "Octopus", category: "provider" },
    members: [{ registry_kind: "interface", owner: original.id, primary: true, configuration: [{ path: "interface.json", fields: [{ pointer: "/config/model", schema: { type: "string" } }] }] }],
  });
  const packageState = () => inspectLocalPackageInstallation({ ...first.packageRoots, packageId: "cli.fixture" });
  await fs.mkdir(first.packageRoots.activeRegistryRoot, { recursive: true });
  first.packageRoots.publication = second.packageRoots.publication = require("./support/native-publication.cjs").nativePublication();
  await installLocalPackages({ ...first.packageRoots, operationId: "cli_fixture_install_001", packageIds: ["cli.fixture"], expectedSourceDigests: { "cli.fixture": (await packageState()).source_digest } });
  await json(active, { ...original, config: { model: "selected" } });
  await json(source, { ...original, contract: 2 });
  const pendingRecord = path.join(home, "state/runs/private_task/run.json");
  await json(pendingRecord, { pending: "preserve" });
  try {
    assert.equal((await first.status()).runtimeReady, false);
    const results = await Promise.all([first.start({ interfaceId: "fixture_home", timeoutMs: 5000 }), second.start({ interfaceId: "fixture_home", timeoutMs: 5000 })]);
    assert.equal(results[0].pid, results[1].pid);
    assert.equal(results[0].clientReady, true);
    assert.equal(await fs.readFile(path.join(home, "starts"), "utf8"), "start\n");
    assert.deepEqual(JSON.parse(await fs.readFile(active)), { ...original, config: { model: "selected" } });
    assert.equal((await packageState()).receipt_state, "valid");
    assert.deepEqual(JSON.parse(await fs.readFile(pendingRecord)), { pending: "preserve" });
    await json(source, { ...original, contract: 3 });
    await first.start({ interfaceId: "fixture_home", timeoutMs: 5000 });
    assert.equal(JSON.parse(await fs.readFile(active)).contract, 1);
    first.close(); second.close();
    assert.equal((await first.status({ interfaceId: "fixture_home" })).runtimeReady, true);
    const absent = await first.status({ interfaceId: "unbound_home" });
    assert.equal(absent.runtimeReady, true);
    assert.equal(absent.clientReady, false);
    await assert.rejects(first.start({ requireEndpoint: true, interfaceId: "unbound_home", timeoutMs: 200 }), /not accepting.*Runtime log/);
    await assert.rejects(first.setup({ workspaceRoot: root }), /does not accept workspaceRoot/);
  } finally {
    const current = await first.status({ interfaceId: "fixture_home" });
    if (current.pid) { process.kill(current.pid, "SIGTERM"); await waitStopped(first); }
  }
  await json(active, { ...original, config: { model: "selected" }, contract: 999 });
  try {
    await first.start({ interfaceId: "fixture_home", timeoutMs: 5000 });
    assert.equal(await fs.readFile(path.join(home, "starts"), "utf8"), "start\nstart\n");
    assert.equal(JSON.parse(await fs.readFile(active)).contract, 999);
  } finally {
    const current = await first.status({ interfaceId: "fixture_home" });
    if (current.pid) { process.kill(current.pid, "SIGTERM"); await waitStopped(first); }
  }
  const missing = createEnvironment({ ...options, home: path.join(root, "missing"), runtimeBinary: path.join(installationRoot, "bin/missing") });
  await assert.rejects(missing.start(), /node app\/scripts\/package-cli.cjs/);
  const broken = path.join(installationRoot, "bin/broken");
  await fs.writeFile(broken, "#!/bin/sh\nexit 7\n", { mode: 0o700 });
  await assert.rejects(createEnvironment({ ...options, home: path.join(root, "failed"), runtimeBinary: broken }).start({ timeoutMs: 3000 }), /exit 7.*Runtime log/);
  const orphanHome = path.join(root, "orphan");
  const orphan = createEnvironment({ ...options, home: orphanHome });
  await fs.mkdir(path.dirname(orphan.endpoint.slice(7)), { recursive: true });
  const server = net.createServer((socket) => socket.end());
  await new Promise((resolve) => server.listen(orphan.endpoint.slice(7), resolve));
  try { await assert.rejects(orphan.start(), /without coherent live runtime-instance proof/); }
  finally { await new Promise((resolve) => server.close(resolve)); }
}

async function setupEvidence(root) {
  const home = path.join(root, "evidence"), registryRoot = path.join(home, "registry");
  for (const directory of [path.join(registryRoot, "interface"), path.join(registryRoot, "tool"), path.join(home, "state/runs"), path.join(home, "state/ee_instances")]) await fs.mkdir(directory, { recursive: true });
  assert.equal((await readWorkspaceSetupState({ octopusHomeRoot: home, registryRoot })).status, "not_configured");
  const pack = { id: "test-pack", name: "Test Pack", manifest_digest: "a".repeat(64), components: [
    { package_id: "provider", source_digest: "b".repeat(64), selection: "included", product_kind: "application", product_id: "provider" },
  ], workspace_setup: { home: { provider_interface_id: "provider", worker_interface_id: "system_worker_test", context_artifact_bindings: [], selected_worker_environment_kinds: [] },
    task: { worker_interface_id: "system_worker_task_test", context_artifact_bindings: [], selected_worker_environment_kinds: [] }, project_defaults: [{ package_id: "provider", default_for_new_tasks: true }] } };
  const plan = compileSetupPlan(pack, { operationId: "cli_fixture_12345678", workspaceName: "Work", workspaceGoal: "Work",
    projects: [{ name: "First Project" }], model: "fixture-model", reasoningEffort: "low", serviceTier: "priority", recommended: [] });
  assert.equal(plan.workspace.request.home.serviceTier, "priority");
  assert.equal(plan.workspace.request.home.providerInterfaceId, "provider");
  assert.equal(plan.workspace.request.task.workerInterfaceId, "system_worker_task_test");
  assert.notEqual(plan.workspace.request.task.workerInterfaceId, plan.workspace.request.home.workerInterfaceId);
  assert.deepEqual(plan.workspace.project_defaults[0].project_keys, ["project-1"]);
  const owners = Object.fromEntries(["packages", "extensions", "workspace", "provider"].map((step) => [step, {
    probe: async () => step === "extensions" ? [] : null,
    apply: async () => { throw new Error("fixture owner requires a real helper"); },
  }]));
  const store = createClientSetupStore({ root: path.join(home, "client/setup-state"), owners });
  const begun = await store.begin({ operationId: plan.workspace.request.operationId, plan });
  assert.equal(begun.status, "in_progress");
  await assert.rejects(store.resume(begun.operationId), /real helper/);
  assert.equal((await store.readCurrent()).lastError.step, "packages");
  await assert.rejects(store.begin({ operationId: begun.operationId, plan: { ...plan, provider: { ...plan.provider, interface_id: "other" } } }), /another immutable plan/);
}

async function signInContract() {
  let signedIn = false, starts = 0, observedUrl = null;
  const account = { account: async () => ({ signedIn }), start: async () => { starts += 1; return { loginId: "login" }; },
    status: () => { signedIn = true; return { state: "completed" }; }, cancel: async () => {} };
  const environment = { accountClient: (request) => { if (request?.openExternal) observedUrl = request.openExternal; return account; } };
  await assert.rejects(ensureSignIn(environment, { nonInteractive: true }), /not signed in/);
  assert.equal(starts, 0);
  await ensureSignIn(environment, { ui: { choose: async () => "sign_in", openExternal: async () => {} } });
  assert.equal(starts, 1);
  assert.equal(typeof observedUrl, "function");
  await ensureSignIn(environment, { nonInteractive: true });
  assert.equal(starts, 1);
}

async function genuineSetup(environment) {
  const launcher = `const {createEnvironment}=require(process.argv[1]);const env=createEnvironment({home:process.argv[2],installationRoot:process.argv[3]});env.start({timeoutMs:60000}).then(value=>{console.log(JSON.stringify({pid:value.pid,runtimeReady:value.runtimeReady}));env.close()}).catch(error=>{console.error(error.message);env.close();process.exitCode=1});`;
  const processes = await Promise.all([0, 1].map(() => execute(process.execPath, ["-e", launcher,
    path.resolve(__dirname, "../cli/environment.cjs"), environment.home, environment.installationRoot], { timeout: 90_000, maxBuffer: 1024 * 1024 })));
  const starts = processes.map(({ stdout }) => JSON.parse(stdout));
  assert.equal(starts[0].runtimeReady, true);
  assert.equal(starts[0].pid, starts[1].pid);
  const start = await environment.start({ timeoutMs: 60_000 });
  assert.equal(start.runtimeReady, true);
  assert.equal(start.clientReady, false);
  console.log(`isolated Home ${environment.home}; runtime PID ${start.pid}`);
  assert.equal((await environment.accountClient().account()).signedIn, true, "Genuine setup requires existing native sign-in; this test never starts login.");
  const providers = await readProviderInterfaceDescriptors(environment.packageRoots.sourceRegistryRoot);
  const metadata = createClientProviderMetadata({ registryRoot: () => environment.packageRoots.sourceRegistryRoot,
    sourceRegistryRoot: () => environment.packageRoots.sourceRegistryRoot,
    accountClient: () => environment.accountClient(), reference: setupReference });
  const [provider] = await metadata.catalog([providers.find((row) => row.id === "world_provider_codex")]);
  assert.ok(!provider.modelCatalogError, provider.modelCatalogError);
  const model = provider.models.find((row) => row.isDefault && row.showInPicker) ?? provider.models.find((row) => row.showInPicker);
  assert.ok(model);
  const questions = [];
  const choices = process.argv.includes("--keep-home") ? { ui: {
    ask: async ({ id }) => { questions.push(id); return id === "workspace_name" ? "Terminal fixture" : "First project"; },
    choose: async ({ id, defaultValue }) => { questions.push(id); return id === "model" ? model.model : defaultValue; },
  } } : { nonInteractive: true, packId: "codex", workspaceName: "Terminal fixture", projectName: "First project",
    model: model.model, reasoningEffort: model.defaultReasoningEffort };
  const result = await environment.setup({ ...choices, recommended: [], timeoutMs: 60_000,
    onProgress: (event) => { if (event.step || event.phase === "complete") console.log(`isolated setup: ${event.step ?? event.phase} ${event.phase ?? ""}`); } });
  if (process.argv.includes("--keep-home")) assert.deepEqual(questions.slice(0, 4), ["pack", "workspace_name", "project_name", "model"]);
  assert.equal(result.status, "configured");
  assert.equal(result.runtime.clientReady, true);
  const repeated = await environment.setup({ recommended: [] });
  assert.equal(repeated.changed, false);
  assert.equal(repeated.workspace.receipt.home_run_id, result.workspace.receipt.home_run_id);
  await assert.rejects(environment.setup({ workspaceName: "replace" }), /Saved setup already chose/);
  const reused = await createEnvironment({ home: environment.home, installationRoot: environment.installationRoot }).start();
  assert.equal(reused.pid, start.pid);
  assert.equal(reused.clientReady, true);
  console.log(`isolated Home verified: ${result.workspace.receipt.project_ee_ids.length} Project; client receipt ${result.setup.status}; shared PID reused`);
}

async function waitStopped(environment) {
  for (let i = 0; i < 100; i += 1) { if (!(await environment.status()).pid) return; await delay(50); }
  throw new Error("Isolated test runtime did not stop.");
}
async function stop(environment) {
  await execute(environment.runtimeBinary, ["down"], { env: { ...process.env, ...environment.launchEnv }, timeout: 10_000 }).catch(() => {});
  await waitStopped(environment);
}
async function json(file, value) { await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, `${JSON.stringify(value)}\n`); }

main().catch((error) => { console.error(error.stack); process.exitCode = 1; });
