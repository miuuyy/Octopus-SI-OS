const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { readClientAgentCatalog } = require("../electron/clientAgentCatalog.cjs");
const { mutateOctopusOsWorkspace } = require("../electron/projection.cjs");
const { operatorChatContextProjection } = require("../electron/projection/operatorChatContext.cjs");

async function main(rootScope = false) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-agent-template-test-"));
  try {
    const octopusHomeRoot = path.join(temporary, "home");
    const registryRoot = path.join(octopusHomeRoot, "registry");
    const interfaceRoot = path.join(registryRoot, "interface");
    const eeTemplateRoot = path.join(registryRoot, "ee_template");
    const eeInstanceRoot = path.join(octopusHomeRoot, "state", "ee_instances");
    const sourceWorkerInterfaceId = "system_worker_octopus_development";
    const homeInterfaceId = "world_operator_chat_home";
    const rootEeId = "ee_root";
    const projectEeId = rootScope ? rootEeId : "ee_octopus";
    await fs.mkdir(path.join(interfaceRoot, sourceWorkerInterfaceId), { recursive: true });
    await fs.mkdir(path.join(interfaceRoot, homeInterfaceId), { recursive: true });
    const sourceInterface = {
      id: sourceWorkerInterfaceId,
      interface_type: "system",
      role: "worker",
      direction: "bidirectional",
      source_names: ["octopus_development"],
    };
    const sourcePolicy = {
      id: sourceWorkerInterfaceId,
      role_artifacts: ["artifact-octopus-project-role"],
      ceiling: {
        interfaces: ["world_provider_codex", "system_exec_env"],
        tools: ["exec.run"],
        wek: ["process_session"],
        mark_outcomes: [],
        hard_blocked_tools: [],
      },
    };
    const taskProfile = {
      ee_id: projectEeId,
      worker_choice_interface: sourceWorkerInterfaceId,
      door_host_slots: {},
    };
    const homeSidecarPath = path.join(interfaceRoot, homeInterfaceId, "octopus_os.json");
    const agentCatalogPath = path.join(octopusHomeRoot, "client", "agents", "catalog.json");
    const rootTemplatePath = path.join(eeTemplateRoot, "operator_space", "ee_template.json");
    const projectTemplatePath = rootScope ? rootTemplatePath : path.join(eeTemplateRoot, "project", "ee_template.json");
    const rootEePath = path.join(eeInstanceRoot, `${rootEeId}.json`);
    const projectEePath = path.join(eeInstanceRoot, `${projectEeId}.json`);
    await writeJson(path.join(interfaceRoot, sourceWorkerInterfaceId, "interface.json"), sourceInterface);
    await writeJson(path.join(interfaceRoot, sourceWorkerInterfaceId, "policy.json"), sourcePolicy);
    await writeJson(rootTemplatePath, { id: "operator_space", allowed_interfaces: [sourceWorkerInterfaceId] });
    if (!rootScope) await writeJson(projectTemplatePath, { id: "project", allowed_interfaces: [sourceWorkerInterfaceId] });
    await writeJson(rootEePath, {
      ee_id: rootEeId,
      template_name: "operator_space",
      parent_ee_id: "",
      goal: "existence",
      config: {},
      policy_narrowing: { allowed_interfaces: [sourceWorkerInterfaceId] },
    });
    if (!rootScope) await writeJson(projectEePath, {
      ee_id: projectEeId,
      template_name: "project",
      parent_ee_id: rootEeId,
      goal: "Octopus Development",
      config: {},
      policy_narrowing: { allowed_interfaces: [sourceWorkerInterfaceId] },
    });
    await writeJson(homeSidecarPath, {
      schema_version: 1,
      workspace: "octopus_os",
      interface: homeInterfaceId,
      owner_interface: homeInterfaceId,
      component: "client",
      surface_role: "home",
      provider_choice_interface: "world_provider_codex",
      worker_choice_interface: "system_worker_home",
      authoring_profile: {
        ee_id: rootEeId,
        worker_choice_interface: "system_worker_home",
        door_host_slots: {},
      },
      task_authoring_profiles: [taskProfile],
      presentation: {
        name: "Octopus OS",
        projects: rootScope ? [] : [{ ee_id: projectEeId, name: "Octopus", icon: "code", color: "violet" }],
      },
    });
    await writeJson(agentCatalogPath, {
      schema: "octopus.client.agent_catalog",
      schema_version: 1,
      project_sources: [{ project_ee_id: projectEeId, source_worker_interface: sourceWorkerInterfaceId }],
      agents: [],
    });
    const scope = await projectedScope({
      agentCatalogPath,
      homeSidecarPath,
      homeInterfaceId,
      projectEeId,
      projectEePath,
      rootEeId,
      rootEePath,
      taskProfiles: [taskProfile],
    });
    const request = {
      action: "create_agent_template",
      operationId: "agent_template_12345678",
      workspace: {
        rootEeId,
        homeRunId: "run_home",
        homeInterfaceId,
        expectedHomeSidecarDigest: scope.home_sidecar_digest,
        expectedAgentCatalogDigest: scope.agent_catalog_digest,
      },
      projectEeId,
      sourceWorkerInterfaceId,
      name: "Octopus Development",
      icon: "glasses",
      providerInterfaceId: "world_provider_codex",
      providerModel: "catalog-model-a",
      reasoningEffort: "high",
      applicationIds: ["world_terminal"],
      driverNames: ["process_session"],
    };
    const chatContexts = {
      contexts: [{
        ee_id: projectEeId,
        workers: [{
          id: sourceWorkerInterfaceId,
          authoring_profile: true,
          provider_interface_ids: ["world_provider_codex"],
          interface_policy: { world_provider_codex: { models: ["catalog-model-a", "catalog-model-b"] } },
          applications: [{ id: "world_terminal" }],
          drivers: [{ name: "process_session" }],
        }],
      }],
    };
    const commitClientAuthoring = async (plan) => {
      const writes = [...plan.writes].sort((left, right) => (
        left.file === plan.publishBarrierPath ? 1 : right.file === plan.publishBarrierPath ? -1 : 0
      ));
      for (const write of writes) {
        assert.equal(await optionalDigest(write.file), write.beforeDigest);
        if (write.next === null) await fs.rm(write.file);
        else await writeJson(write.file, write.next);
      }
      return { cleanupWarnings: [] };
    };
    await assert.rejects(
      mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, scope, { ...request, projectEeId: "ee_foreign" }, { commitClientAuthoring, chatContexts }),
      /outside the workspace scope/,
    );
    await assert.rejects(
      mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, scope, { ...request, providerModel: "not-admitted" }, { commitClientAuthoring, chatContexts }),
      /not admitted by worker template/,
    );
    await assert.rejects(
      mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, scope, { ...request, providerModel: "" }, { commitClientAuthoring, chatContexts }),
      /explicit Agent provider model/,
    );
    const created = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      scope,
      request,
      { commitClientAuthoring, chatContexts },
    );
    assert.equal(created.action, "create_agent_template");
    assert.equal(created.changed, true);
    if (rootScope) assert.deepEqual(await fs.readdir(eeInstanceRoot), [`${rootEeId}.json`]);
    assert.match(created.workerInterfaceId, /^system_worker_template_[a-f0-9]{32}$/);
    assert.match(created.agentId, /^agent_[a-f0-9]{32}$/);
    const clonedInterface = await readJson(path.join(interfaceRoot, created.workerInterfaceId, "interface.json"));
    const clonedPolicy = await readJson(path.join(interfaceRoot, created.workerInterfaceId, "policy.json"));
    const clonedClientTemplatePath = path.join(interfaceRoot, created.workerInterfaceId, "client_template.json");
    assert.deepEqual(clonedInterface, { ...sourceInterface, id: created.workerInterfaceId, copy_of: sourceInterface.copy_of ?? sourceWorkerInterfaceId });
    assert.deepEqual(clonedPolicy, { ...sourcePolicy, id: created.workerInterfaceId });
    assert.deepEqual(await readJson(clonedClientTemplatePath), {
      schema_version: 1,
      worker_interface: created.workerInterfaceId,
      project_ee_id: projectEeId,
      provider_interface_id: "world_provider_codex",
      provider_model: "catalog-model-a",
      reasoning_effort: "high",
      application_ids: ["world_terminal"],
      worker_environment_kinds: ["process_session"],
    });
    for (const file of [rootTemplatePath, projectTemplatePath]) {
      assert.deepEqual((await readJson(file)).allowed_interfaces, [sourceWorkerInterfaceId]);
    }
    for (const file of [rootEePath, projectEePath]) {
      assert.deepEqual((await readJson(file)).policy_narrowing.allowed_interfaces, [sourceWorkerInterfaceId]);
    }
    let catalog = await readClientAgentCatalog(octopusHomeRoot);
    assert.deepEqual(catalog.document.agents, [{
      id: created.agentId,
      name: "Octopus Development",
      icon: "glasses",
      project_ee_id: projectEeId,
      worker_interface: created.workerInterfaceId,
      created_at_ms: catalog.document.agents[0].created_at_ms,
      updated_at_ms: catalog.document.agents[0].updated_at_ms,
      deleted_at_ms: 0,
    }]);
    assert.equal(created.agentCatalogDigest, catalog.digest);
    const authoredHome = await readJson(homeSidecarPath);
    const clonedProfile = authoredHome.task_authoring_profiles.at(-1);
    assert.equal(clonedProfile.worker_choice_interface, created.workerInterfaceId);
    chatContexts.contexts[0].workers.push({
      ...chatContexts.contexts[0].workers[0],
      id: created.workerInterfaceId,
    });
    const nextScope = await projectedScope({
      agentCatalogPath,
      agentPresentations: catalog.document.agents,
      homeSidecarPath,
      homeInterfaceId,
      projectEeId,
      projectEePath,
      rootEeId,
      rootEePath,
      taskProfiles: authoredHome.task_authoring_profiles,
    });
    request.workspace.expectedHomeSidecarDigest = nextScope.home_sidecar_digest;
    request.workspace.expectedAgentCatalogDigest = nextScope.agent_catalog_digest;
    const repeated = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      nextScope,
      request,
      { commitClientAuthoring, chatContexts },
    );
    assert.equal(repeated.changed, false);
    clonedPolicy.ceiling.tools.push("unexpected.tool");
    await writeJson(path.join(interfaceRoot, created.workerInterfaceId, "policy.json"), clonedPolicy);
    await assert.rejects(
      mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, nextScope, request, { commitClientAuthoring, chatContexts }),
      /belongs to another authoring operation/,
    );
    clonedPolicy.ceiling.tools.pop();
    await writeJson(path.join(interfaceRoot, created.workerInterfaceId, "policy.json"), clonedPolicy);
    const configureRequest = {
      action: "configure_agent_template",
      agentId: created.agentId,
      projectEeId,
      workerInterfaceId: created.workerInterfaceId,
      name: "Octopus Core Development",
      icon: "hardhat",
      providerInterfaceId: "world_provider_codex",
      providerModel: "catalog-model-b",
      reasoningEffort: "xhigh",
      applicationIds: [],
      driverNames: [],
      expectedClientTemplateDigest: created.clientTemplateDigest,
      expectedAgentCatalogDigest: catalog.digest,
    };
    const configured = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      nextScope,
      configureRequest,
      { commitClientAuthoring, chatContexts },
    );
    assert.equal(configured.changed, true);
    assert.equal((await readJson(clonedClientTemplatePath)).provider_model, "catalog-model-b");
    catalog = await readClientAgentCatalog(octopusHomeRoot);
    assert.equal(catalog.document.agents[0].name, "Octopus Core Development");
    assert.equal(catalog.document.agents[0].icon, "hardhat");
    const configuredScope = { ...nextScope, agent_catalog_digest: catalog.digest, agent_presentations: catalog.document.agents };
    const repeatedConfiguration = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      configuredScope,
      {
        ...configureRequest,
        expectedClientTemplateDigest: configured.clientTemplateDigest,
        expectedAgentCatalogDigest: catalog.digest,
      },
      { commitClientAuthoring, chatContexts },
    );
    assert.equal(repeatedConfiguration.changed, false);
    const agentPolicyPath = path.join(interfaceRoot, created.workerInterfaceId, "policy.json");
    const projectedAgent = () => {
      const contexts = projectedChatContexts(configuredScope, projectEeId, [{ ...sourceInterface, worker_policy: sourcePolicy }, {
        ...clonedInterface,
        worker_policy: clonedPolicy,
        client_worker_template: { project_ee_id: projectEeId, worker_interface: created.workerInterfaceId },
      }]);
      return { contexts, row: contexts.contexts[0].workers.find((worker) => worker.id === created.workerInterfaceId) };
    };
    assert.deepEqual(projectedAgent().row.unavailable_applications, [{ id: "world_mail", reason: "Not in this Agent's capability ceiling" }]);
    sourcePolicy.ceiling.interfaces.push("world_mail");
    await writeJson(path.join(interfaceRoot, sourceWorkerInterfaceId, "policy.json"), sourcePolicy);
    const widened = projectedAgent();
    assert.deepEqual(widened.row.applications, [{ id: "world_mail", selected_by_default: false }]);
    const reformed = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      configuredScope,
      {
        ...configureRequest,
        applicationIds: ["world_mail"],
        expectedClientTemplateDigest: configured.clientTemplateDigest,
        expectedAgentCatalogDigest: catalog.digest,
      },
      { commitClientAuthoring, chatContexts: widened.contexts },
    );
    assert.equal(reformed.changed, true);
    assert.deepEqual(await readJson(agentPolicyPath), { ...sourcePolicy, id: created.workerInterfaceId });
    assert.deepEqual((await readJson(clonedClientTemplatePath)).application_ids, ["world_mail"]);
    catalog = await readClientAgentCatalog(octopusHomeRoot);
    const deleted = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHomeRoot,
      { ...configuredScope, agent_catalog_digest: catalog.digest, agent_presentations: catalog.document.agents },
      {
        action: "delete_agent_template",
        agentId: created.agentId,
        projectEeId,
        workerInterfaceId: created.workerInterfaceId,
        expectedClientTemplateDigest: reformed.clientTemplateDigest,
        expectedAgentCatalogDigest: catalog.digest,
      },
      { commitClientAuthoring, chatContexts },
    );
    assert.equal(deleted.changed, true);
    assert.equal(await optionalDigest(clonedClientTemplatePath), null);
    catalog = await readClientAgentCatalog(octopusHomeRoot);
    assert.ok(catalog.document.agents[0].deleted_at_ms > 0);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

function projectedChatContexts(scope, projectEeId, workers) {
  const application = { id: "world_mail", valid: true, interfaces: ["world_mail"], task_capabilities: { tools: [] }, project_states: [{ ee_id: projectEeId, policy_allowed: true }] };
  const provider = { id: "world_provider_codex", interface_type: "world", role: "provider", interface_driver_config: { private_config: { client_projection_output_labels: ["final", "error"] } } };
  const formedWorkers = workers.map((worker) => ({ worker_interface_id: worker.id, interface_policy: { world_provider_codex: { models: ["catalog-model-b"] } } }));
  return { application_catalog: [application], contexts: operatorChatContextProjection({
    operatorOsScope: () => scope,
    operatorOsApplications: () => ({ applications: [application] }),
    operatorOsDrivers: () => ({ drivers: [] }),
  }).operatorChatContextRows({
    operatorChatProtocol: { terminalOutputLabel: "final", errorOutputLabel: "error" },
    descriptors: { interface: [...workers, provider], ee_template: [] },
    eeInstances: [{ ee_id: projectEeId }],
    eePolicyProjection: { environments: [{ ee_id: projectEeId, formed_envelope: {}, workers: formedWorkers }] },
    structureMemo: new Map(), ledgerMemo: new Map(), runRecords: [], ledgerRows: [],
  }) };
}

async function projectedScope({
  agentCatalogPath,
  agentPresentations = [],
  homeSidecarPath,
  homeInterfaceId,
  projectEeId,
  projectEePath,
  rootEeId,
  rootEePath,
  taskProfiles,
}) {
  return {
    status: "ready",
    root_ee_id: rootEeId,
    home_run_id: "run_home",
    home_interface_id: homeInterfaceId,
    home_sidecar_digest: await digest(homeSidecarPath),
    agent_catalog_path: agentCatalogPath,
    agent_catalog_digest: await digest(agentCatalogPath),
    agent_project_sources: [{ project_ee_id: projectEeId, source_worker_interface: "system_worker_octopus_development" }],
    agent_presentations: agentPresentations,
    environments: [
      {
        ee_id: rootEeId,
        parent_ee_id: "",
        template_name: "operator_space",
        record_path: rootEePath,
        record_digest: await digest(rootEePath),
      },
      ...(projectEeId === rootEeId ? [] : [{
        ee_id: projectEeId,
        parent_ee_id: rootEeId,
        template_name: "project",
        record_path: projectEePath,
        record_digest: await digest(projectEePath),
      }]),
    ],
    client_runs: [],
    task_authoring_profiles: taskProfiles,
    read_only_runs: [],
  };
}

async function writeJson(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

async function digest(file) {
  return crypto.createHash("sha256").update(await fs.readFile(file)).digest("hex");
}

async function optionalDigest(file) {
  try {
    return await digest(file);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

main().then(() => main(true)).then(() => {
  process.stdout.write("agent worker-template contracts passed\n");
}).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
