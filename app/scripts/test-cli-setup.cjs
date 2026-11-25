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
const { main: runCli, argumentsFor, HELP, formatSetupProgress } = require("../cli/index.cjs");
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
    await setupNoStartAndProgressContract(root);
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

async function setupNoStartAndProgressContract(root) {
  assert.match(HELP, /--no-start/);
  const parsedArgs = argumentsFor(["setup", "--no-start", "--non-interactive", "--json"]);
  assert.deepEqual(parsedArgs.positionals, ["setup"]);
  assert.equal(parsedArgs.values["no-start"], true);
  assert.equal(parsedArgs.values["non-interactive"], true);
  assert.equal(parsedArgs.values.json, true);
  assert.equal(formatSetupProgress({ packageId: "core.pkg", phase: "staging_package", percent: 40 }), "[setup] packages: staging package core.pkg (40%)");
  assert.equal(formatSetupProgress({ step: "workspace", phase: "running" }), "[setup] workspace: running");
  assert.equal(formatSetupProgress({ phase: "setup_plan_ready" }), "[setup] setup plan ready");

  const captureIo = () => {
    let stdout = "", stderr = "";
    return {
      io: {
        stdout: { write: (chunk) => { stdout += String(chunk); }, isTTY: false },
        stderr: { write: (chunk) => { stderr += String(chunk); }, isTTY: false },
        stdin: { isTTY: false },
      },
      read: () => ({ stdout, stderr }),
    };
  };
  const badFlag = captureIo();
  const badExit = await runCli(["status", "--no-start"], badFlag.io);
  assert.equal(badExit, 1);
  assert.match(badFlag.read().stderr, /--no-start is only supported with 'setup'/);

  // Offline empty-Home setup --no-start --json + progress + idempotent repeat run.
  const installationRoot = path.join(root, "offline-dist");
  const emptyHome = path.join(root, "offline-home");
  const nativePubPath = path.resolve(__dirname, "support/native-publication.cjs");
  for (const dir of [
    "bin", "registry/interface", "registry/tool", "registry/ee_template/existence", "registry/ee_template/project",
    "local-packages/registry/interface/world_operator_chat", "local-packages/registry/interface/world_provider_fixture",
    "local-packages/registry/interface/system_worker_home_fixture", "local-packages/registry/interface/system_worker_task_fixture",
    "local-packages/packages", "local-packages/packs",
  ]) {
    await fs.mkdir(path.join(installationRoot, dir), { recursive: true });
  }
  for (const file of ["registry/.octopus-source.json", "local-packages/registry/.octopus-source.json"]) {
    await json(path.join(installationRoot, file), { schema_version: 1, id: "offline-fixture" });
  }
  const repoRoot = path.resolve(__dirname, "../..");
  for (const template of ["existence", "project"]) {
    await fs.copyFile(
      path.join(repoRoot, "registry/ee_template", template, "ee_template.json"),
      path.join(installationRoot, "registry/ee_template", template, "ee_template.json"),
    );
  }
  await json(path.join(installationRoot, "local-packages/registry/interface/world_operator_chat/interface.json"), {
    id: "world_operator_chat", interface_type: "world", role: "channel", direction: "bidirectional", source_names: ["operator_chat"],
  });
  await json(path.join(installationRoot, "local-packages/registry/interface/world_operator_chat/life.json"), {
    processes: [{ bin: "owner://bin/client" }],
  });
  await json(path.join(installationRoot, "local-packages/registry/interface/world_operator_chat/client.json"), {
    schema_version: 2, interface: "world_operator_chat",
    run: { ee: "ee_existence", worker: "system_worker_home_fixture", context_artifact_bindings: [] },
  });
  await fs.writeFile(path.join(installationRoot, "local-packages/registry/interface/world_operator_chat/client.cjs"), `
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { nativePublication } = require(${JSON.stringify(nativePubPath)});
exports.loadStoragePublisher = () => nativePublication();
exports.withOperatorWorkspaceTopologyLock = async (_home, fn) => fn();
exports.loadOperatorTaskLifecycle = async () => ({
  retirementBarrier: { verifyBefore: async () => {}, withHeldProof: async (_ctx, fn) => fn() },
});
exports.planOperatorChatSurface = async ({ octopusHome, registryRoot, eeInstancesRoot }, request) => {
  const homeRoot = octopusHome(), regRoot = registryRoot(), eeRoot = eeInstancesRoot();
  const rootRecord = JSON.parse(await fs.readFile(path.join(eeRoot, \`\${request.eeId}.json\`), "utf8"));
  const clientSurfaceRole = request.clientSurfaceRole ?? "home";
  if (clientSurfaceRole === "home") {
    assert.equal(rootRecord.parent_ee_id, null);
    const tpl = JSON.parse(await fs.readFile(path.join(regRoot, "ee_template", rootRecord.template_name, "ee_template.json"), "utf8"));
    assert.equal(tpl.bootstrap_root, true);
  }
  const runId = \`run_operator_chat_\${request.chatId}\`;
  const chatInterfaceId = \`world_operator_chat_\${request.chatId}\`;
  const modelInterfaceId = \`\${request.providerInterfaceId}_\${request.chatId}\`;
  const workerChoiceInterfaceId = request.workerInterfaceId;
  const workerInterfaceId = \`\${workerChoiceInterfaceId}_\${request.chatId}\`;
  const toolId = \`file.write__authoring_\${request.chatId}\`;
  const pid = crypto.createHash("sha256").update(request.chatId).digest("hex").slice(0, 24);
  const intakeId = \`system_service_input_\${pid}\`, writerId = \`system_input_writer_\${pid}\`, inputToolId = \`file.write.input_\${pid}\`;
  const requiredInterfaceIds = [chatInterfaceId, modelInterfaceId, workerInterfaceId, intakeId, writerId].sort();
  const requiredToolIds = [toolId, inputToolId].sort();
  const authoringProfile = { ee_id: request.eeId, worker_choice_interface: workerChoiceInterfaceId, door_host_slots: request.doorHostSlots ?? {} };
  const ifaceDir = path.join(regRoot, "interface", chatInterfaceId);
  const sidecar = { schema_version: 1, workspace: "octopus_os", interface: chatInterfaceId, owner_interface: chatInterfaceId,
    component: "client", surface_role: clientSurfaceRole, provider_choice_interface: request.providerInterfaceId,
    worker_choice_interface: workerChoiceInterfaceId, authoring_profile: authoringProfile };
  const bind = (id) => ({ interface: id, config: { artifact_policy: { mode: "all" } } });
  const cp = (id, inbound, outbound) => ({ id, run: runId, trace: \`trace:\${runId}:\${id}\`, epoch: 0,
    interface_binding: bind(inbound), outbound_interface_bindings: outbound.map(bind), artifact_bindings: [], required_verified_artifacts: [] });
  const checkpoints = [cp("cp_home_input", chatInterfaceId, [modelInterfaceId]), cp("cp_worker_input", workerInterfaceId, [modelInterfaceId]),
    cp("cp_home_output", modelInterfaceId, [chatInterfaceId]), cp("cp_intake_input", intakeId, [modelInterfaceId]),
    cp("cp_intake_call", intakeId, [writerId]), cp("cp_intake_result", writerId, [intakeId])];
  checkpoints[4].required_verified_artifacts = [{ artifact_label: "tool.call", mark: "tool.call.allowed", verifier: "tool_call_allowed",
    params: { tools: [inputToolId], metadata_matches: [{ payload: "/arguments/expected_before_hash", metadata: "/runtime/run_record_hash" }] } }];
  const runRecord = { id: runId, ee: request.eeId, worker: workerInterfaceId, trace: \`trace:\${runId}\`, status: "running",
    interface_state: { [chatInterfaceId]: { client_surface_role: clientSurfaceRole, provider_choice_interface: request.providerInterfaceId,
      provider_runtime_interface: modelInterfaceId, worker_choice_interface: workerChoiceInterfaceId } },
    checkpoints, active_checkpoints: checkpoints.map((row) => row.id) };
  const w = (filePath, content, tag) => ({ filePath, content, tag, expectedBeforeHash: null });
  return {
    runId, chatInterfaceId, modelInterfaceId, providerModel: request.providerModel, workerInterfaceId, clientSurfaceRole,
    reasoningEffort: request.reasoningEffort ?? "low", serviceTier: request.serviceTier ?? "", authoringProfile, requiredInterfaceIds, requiredToolIds,
    writes: [
      w(path.join(ifaceDir, "interface.json"), JSON.stringify({ id: chatInterfaceId }) + "\\n", \`interface:\${chatInterfaceId}\`),
      w(path.join(ifaceDir, "octopus_os.json"), JSON.stringify(sidecar) + "\\n", \`octopus-os:\${chatInterfaceId}\`),
      w(path.join(regRoot, "interface", modelInterfaceId, "interface.json"), JSON.stringify({ id: modelInterfaceId, interface_driver_config: { private_config: { model: request.providerModel } } }) + "\\n", \`interface:\${modelInterfaceId}\`),
      w(path.join(regRoot, "interface", workerInterfaceId, "interface.json"), JSON.stringify({ id: workerInterfaceId }) + "\\n", \`interface:\${workerInterfaceId}\`),
      w(path.join(regRoot, "interface", workerInterfaceId, "policy.json"), JSON.stringify({ id: workerInterfaceId, ceiling: { interfaces: requiredInterfaceIds, tools: requiredToolIds } }) + "\\n", \`policy:\${workerInterfaceId}\`),
      w(path.join(regRoot, "interface", intakeId, "interface.json"), JSON.stringify({ id: intakeId, role: "service" }) + "\\n", \`interface:\${intakeId}\`),
      w(path.join(regRoot, "interface", intakeId, "life.json"), "{}\\n", \`life:\${intakeId}\`),
      w(path.join(regRoot, "interface", writerId, "interface.json"), JSON.stringify({ id: writerId, role: "system" }) + "\\n", \`interface:\${writerId}\`),
      w(path.join(regRoot, "tool", toolId, "tool.json"), JSON.stringify({ id: toolId, interface: writerId }) + "\\n", \`tool:\${toolId}\`),
      w(path.join(regRoot, "tool", inputToolId, "tool.json"), JSON.stringify({ id: inputToolId, interface: writerId }) + "\\n", \`tool:\${inputToolId}\`),
      w(path.join(homeRoot, "state", "runs", \`\${runId}.json\`), JSON.stringify(runRecord) + "\\n", \`run:\${runId}\`),
    ],
    ledgerOffset: 0,
  };
};
exports.readOperatorChatArtifacts = async () => ({ artifacts: [] });
exports.submitOperatorTurn = async () => ({});
`);
  await json(path.join(installationRoot, "local-packages/registry/interface/world_provider_fixture/interface.json"), {
    id: "world_provider_fixture", interface_type: "world", role: "provider", direction: "bidirectional", source_names: ["fixture"],
    interface_policy: { variables: { models: { default: ["fixture-model"] } } },
    interface_driver_config: {
      transport: { auth_env_key: "OCTOPUS_FIXTURE_API_KEY" },
      private_config: {
        model: "fixture-model",
        model_catalog: [{ model: "fixture-model", display_name: "Fixture Model", default_reasoning_effort: "low", supported_reasoning_efforts: ["low"], show_in_picker: true }],
      },
    },
  });
  await json(path.join(installationRoot, "local-packages/registry/interface/world_provider_fixture/application.json"), {
    schema_version: 3, id: "world_provider_fixture", name: "Fixture Provider", description: "Fixture provider",
    category: "provider", developer: "Octopus", version: "1.0.0", icon: "sparkles", logo: "application-logo.svg",
    interfaces: ["world_provider_fixture"], setup: { fields: [] },
  });
  await fs.writeFile(path.join(installationRoot, "local-packages/registry/interface/world_provider_fixture/application-logo.svg"), '<svg xmlns="http://www.w3.org/2000/svg"/>\n');
  for (const workerId of ["system_worker_home_fixture", "system_worker_task_fixture"]) {
    await json(path.join(installationRoot, "local-packages/registry/interface", workerId, "interface.json"), {
      id: workerId, interface_type: "system", role: "worker", direction: "bidirectional", source_names: [workerId],
    });
    await json(path.join(installationRoot, "local-packages/registry/interface", workerId, "policy.json"), {
      id: workerId, ceiling: { interfaces: [], tools: [] },
    });
  }
  await json(path.join(installationRoot, "local-packages/packages/world_operator_chat.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "world_operator_chat", version: "1.0.0",
    product_kind: "application", product_id: "world_operator_chat", release_status: "optional",
    display: { name: "Operator Chat", description: "Fixture", developer: "Octopus", category: "core" },
    members: [{ registry_kind: "interface", owner: "world_operator_chat", primary: true }],
  });
  await json(path.join(installationRoot, "local-packages/packages/world_provider_fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "world_provider_fixture", version: "1.0.0",
    product_kind: "application", product_id: "world_provider_fixture", release_status: "optional",
    display: { name: "Fixture Provider", description: "Fixture", developer: "Octopus", category: "provider" },
    members: [
      { registry_kind: "interface", owner: "world_provider_fixture", primary: true },
      { registry_kind: "interface", owner: "system_worker_home_fixture", primary: false },
      { registry_kind: "interface", owner: "system_worker_task_fixture", primary: false },
    ],
  });
  await json(path.join(installationRoot, "local-packages/packs/fixture.json"), {
    schema: "octopus.local_application_pack", schema_version: 1, id: "fixture", version: "1.0.0",
    name: "Fixture Pack", description: "Minimal offline setup pack",
    components: [
      { package_id: "world_operator_chat", selection: "included", reason: "Chat client" },
      { package_id: "world_provider_fixture", selection: "included", reason: "Provider" },
    ],
    workspace_setup: {
      selection: "included",
      home: { provider_interface_id: "world_provider_fixture", worker_interface_id: "system_worker_home_fixture",
        default_model: "fixture-model", default_reasoning_effort: "low", context_artifact_bindings: [], selected_worker_environment_kinds: [] },
      task: { worker_interface_id: "system_worker_task_fixture", context_artifact_bindings: [], selected_worker_environment_kinds: [] },
      project_defaults: [],
    },
  });
  await fs.writeFile(path.join(installationRoot, "bin/octopus"), `#!${process.execPath}
const fs = require("node:fs"), path = require("node:path");
const home = process.env.OCTOPUS_HOME, srcReg = process.env.OCTOPUS_SOURCE_REGISTRY, cmd = process.argv[2];
if (cmd === "project-ee-policies") {
  const eeDir = path.join(home, "state/ee_instances"), environments = [];
  if (fs.existsSync(eeDir)) {
    for (const file of fs.readdirSync(eeDir).filter((n) => n.endsWith(".json")).sort()) {
      const rec = JSON.parse(fs.readFileSync(path.join(eeDir, file), "utf8"));
      const allowed = Array.isArray(rec.policy?.allowed_interfaces) ? [...new Set(rec.policy.allowed_interfaces)].sort() : ["world_operator_chat", "world_provider_fixture"];
      const env = { allowed_interfaces: allowed, allowed_tools: [], allowed_worker_environment_kinds: [], allowed_root_slots: null, allowed_artifact_verbs: ["change", "create", "delete", "read"], hard_blocked_tools: [] };
      environments.push({ ee_id: rec.ee_id, effective_policy: env, policy_ceiling: { ...env, allowed_interfaces: ["world_operator_chat", "world_provider_fixture"] }, workers: [] });
    }
  }
  console.log(JSON.stringify({ environments, issues: [] })); process.exit(0);
}
if (cmd === "bootstrap") {
  fs.mkdirSync(home, { recursive: true });
  fs.appendFileSync(path.join(home, "bootstrap-calls"), "bootstrap\\n");
  fs.cpSync(srcReg, path.join(home, "registry"), { recursive: true });
  for (const d of ["state/runs", "state/ee_instances", "state/interface-artifacts", "ledger/blobs"]) fs.mkdirSync(path.join(home, d), { recursive: true });
  fs.writeFileSync(path.join(home, "host.json"), JSON.stringify({ host_manifest_version: 1, root_refs: {} }) + "\\n");
  const ee = path.join(home, "state/ee_instances/ee_existence.json");
  if (!fs.existsSync(ee)) fs.writeFileSync(ee, JSON.stringify({ ee_id: "ee_existence", template_name: "existence", parent_ee_id: null, goal: "Existence" }) + "\\n");
  process.exit(0);
}
fs.mkdirSync(home, { recursive: true });
fs.appendFileSync(path.join(home, "starts"), (cmd || "unknown") + "\\n");
process.exit(99);
`, { mode: 0o700 });

  const prevKey = process.env.OCTOPUS_FIXTURE_API_KEY;
  process.env.OCTOPUS_FIXTURE_API_KEY = "fixture-secret";
  try {
    await assert.rejects(fs.access(emptyHome), { code: "ENOENT" });
    const baseArgs = [
      "setup", "--home", emptyHome, "--bundle", installationRoot, "--non-interactive",
      "--pack", "fixture", "--workspace-name", "Fresh Space", "--project-name", "Alpha",
      "--provider", "world_provider_fixture", "--model", "fixture-model", "--effort", "low", "--no-start",
    ];
    const cold = captureIo();
    const coldExit = await runCli([...baseArgs, "--json"], cold.io);
    const coldOut = cold.read();
    assert.equal(coldExit, 0, `cold setup failed: stdout=${coldOut.stdout} stderr=${coldOut.stderr}`);
    const stdoutLines = coldOut.stdout.trimEnd().split("\n");
    assert.equal(stdoutLines.length, 1);
    const parsed = JSON.parse(stdoutLines[0]);
    assert.equal(parsed.status, "configured");
    assert.equal(parsed.changed, true);
    assert.equal(parsed.runtime.started, false);
    assert.equal(parsed.runtime.skipped, true);
    assert.equal(parsed.runtime.runtimeReady, false);
    assert.equal(parsed.workspace.receipt.project_ee_ids.length, 1);
    const stderrLines = coldOut.stderr.trim().split("\n").filter(Boolean);
    assert.ok(stderrLines.length >= 2 && stderrLines.every((line) => line.startsWith("[setup] ")));
    assert.ok(stderrLines.includes("[setup] bootstrapping home"));
    assert.ok(stderrLines.includes("[setup] setup plan ready"));
    assert.equal(await fs.readFile(path.join(emptyHome, "bootstrap-calls"), "utf8"), "bootstrap\n");
    await assert.rejects(fs.access(path.join(emptyHome, "starts")), { code: "ENOENT" });
    await assert.rejects(fs.access(path.join(emptyHome, "state/runtime-instance")), { code: "ENOENT" });

    const repeat = captureIo();
    const repeatExit = await runCli([...baseArgs, "--json"], repeat.io);
    assert.equal(repeatExit, 0, repeat.read().stderr);
    const repeatParsed = JSON.parse(repeat.read().stdout.trim());
    assert.equal(repeatParsed.status, "configured");
    assert.equal(repeatParsed.changed, false);
    assert.equal(repeatParsed.runtime.skipped, true);
    assert.equal(await fs.readFile(path.join(emptyHome, "bootstrap-calls"), "utf8"), "bootstrap\n");

    const human = captureIo();
    const humanExit = await runCli(baseArgs, human.io);
    assert.equal(humanExit, 0, human.read().stderr);
    assert.match(human.read().stderr, /Runtime daemon not started \(--no-start\)/);
    await assert.rejects(fs.access(path.join(emptyHome, "starts")), { code: "ENOENT" });
  } finally {
    if (prevKey === undefined) delete process.env.OCTOPUS_FIXTURE_API_KEY;
    else process.env.OCTOPUS_FIXTURE_API_KEY = prevKey;
  }
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
