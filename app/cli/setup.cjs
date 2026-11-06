const fs = require("node:fs/promises");
const path = require("node:path");
const { createHash, randomUUID } = require("node:crypto");
const { setTimeout: delay } = require("node:timers/promises");
const { createClientSetupStore } = require("../electron/clientSetup.cjs");
const { initializeWorkspace, readWorkspaceSetupState, workspaceInitializeRequestDigest } = require("../electron/workspaceSetup.cjs");
const { applyLocalApplicationPack, readLocalPackageCatalog, withLocalPackageInstallation, withInstalledAgentPreset } = require("../electron/localPackages.cjs");
const { createOperatorChatClientLoader } = require("../electron/operatorChatClientContract.cjs");
const { createClientProviderMetadata, validateProviderModelSelection } = require("../electron/clientProviderMetadata.cjs");
const { readProviderInterfaceDescriptors, mutateOctopusOsWorkspace, setupTriggerEntries, setupTriggerProof, armSetupTriggers, setupTriggerTaskPlanner } = require("../electron/projection.cjs");
const { projectionReadRequest } = require("../electron/projectionReadContract.cjs");
const { planClientAgentTemplate } = require("../electron/clientAgentCatalog.cjs");
const { commitProjectionClientAuthoring } = require("../electron/clientWorkspaceAuthoring.cjs");
const { taskRetirementBarrier, projectionStore, requireDistribution } = require("./environment.cjs");

const DIGEST = /^[a-f0-9]{64}$/;

async function setupEnvironment(environment, options = {}) {
  if (options.workspaceRoot !== undefined) throw new Error("Workspace paths belong to Project/Agent driver configuration. Setup does not accept workspaceRoot.");
  const progress = typeof options.onProgress === "function" ? options.onProgress : () => {};
  checkCancelled(options.signal);
  await environment.start({ onProgress: progress, timeoutMs: options.timeoutMs, signal: options.signal });
  requireDistribution(environment);
  const registryRoot = environment.packageRoots.activeRegistryRoot;
  const roots = { octopusHomeRoot: environment.home, registryRoot,
    publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) };
  const owners = createSetupOwners(environment, options);
  const store = createClientSetupStore({ root: path.join(environment.home, "client", "setup-state"), owners });
  try {
    const workspace = await readWorkspaceSetupState(roots);
    if (workspace.status === "degraded") throw new Error(`Workspace setup needs repair: ${workspace.errors.join("; ")}`);
    const current = await store.readCurrent();
    if (current?.status === "in_progress") {
      if (options.operationId && current.operationId !== options.operationId) throw new Error(`Setup '${current.operationId}' is in progress. Resume it before choosing another plan.`);
      if (workspace.status === "configured" && workspace.receipt.operation_id !== current.operationId) throw new Error("The unfinished setup and Home receipt belong to different operations.");
      assertSavedChoices(options, current.plan);
      progress({ phase: "resuming_setup", operationId: current.operationId });
      await store.resume(current.operationId, { onProgress: progress });
      return finishSetup(environment, store, current.operationId, options);
    }
    if (workspace.status === "configured") {
      if (current && (current.operationId !== workspace.receipt.operation_id || current.plan.workspace.request_digest !== workspace.receipt.request_digest)) throw new Error("Completed client setup does not match Home's workspace receipt.");
      if (current) assertSavedChoices(options, current.plan);
      else if (hasNewChoices(options)) throw new Error("This Home is already configured. Existing setup choices cannot be replaced by setup.");
      if (options.signIn) await ensureSignIn(environment, options);
      return {
        status: "configured", changed: false, home: environment.home,
        workspace, setup: current,
        runtime: await environment.status({ interfaceId: workspace.receipt.home_interface_id }),
      };
    }
    if (current) throw new Error("The completed client setup has no matching configured Home. Repair its evidence before continuing.");
    const catalog = await readLocalPackageCatalog(environment.packageRoots);
    const pack = await choosePack(catalog, options);
    const choices = await collectChoices(environment, pack, options);
    const plan = compileSetupPlan(pack, choices);
    await verifySetupPlan(environment, plan);
    progress({ phase: "setup_plan_ready", operationId: choices.operationId, pack: pack.name, workspace: choices.workspaceName });
    await store.begin({ operationId: choices.operationId, plan });
    await store.resume(choices.operationId, { onProgress: progress });
    return finishSetup(environment, store, choices.operationId, options);
  } finally { owners.close(); }
}

async function finishSetup(environment, store, operationId, options) {
  const setup = await store.read(operationId);
  const workspace = await readWorkspaceSetupState({ octopusHomeRoot: environment.home, registryRoot: environment.packageRoots.activeRegistryRoot,
    publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) });
  if (setup?.status !== "complete" || workspace.status !== "configured"
    || workspace.receipt.operation_id !== operationId || workspace.receipt.request_digest !== setup.plan.workspace.request_digest) {
    throw new Error("Setup did not produce matching client and workspace receipts.");
  }
  const runtime = await environment.start({ requireEndpoint: true, interfaceId: workspace.receipt.home_interface_id,
    timeoutMs: options.timeoutMs, signal: options.signal, onProgress: options.onProgress });
  return { status: "configured", changed: true, home: environment.home, workspace, setup, runtime };
}

function createSetupOwners(environment, options = {}) {
  const registryRoot = environment.packageRoots.activeRegistryRoot;
  const roots = { octopusHomeRoot: environment.home, registryRoot,
    publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) };
  const loader = createOperatorChatClientLoader({
    packageRoots: () => environment.packageRoots,
    clientPath: () => path.join(registryRoot, "interface", "world_operator_chat", "client.cjs"),
  });
  let projection = null;
  const getProjection = () => projection ??= projectionStore(environment);
  const metadata = createClientProviderMetadata({ registryRoot: () => registryRoot,
    sourceRegistryRoot: () => environment.packageRoots.sourceRegistryRoot,
    accountClient: () => environment.accountClient(), reference: setupReference });

  async function planSurface({ eeInstancesRoot, request }) {
    checkCancelled(options.signal);
    const client = await loader.load();
    if (typeof client.planOperatorChatSurface !== "function") throw new Error("Installed operator chat package has no topology planner.");
    return client.planOperatorChatSurface({
      octopusHome: () => environment.home, installationRoot: () => environment.installationRoot,
      registryRoot: () => registryRoot, eeInstancesRoot: () => eeInstancesRoot,
    }, request);
  }

  async function packagePlanner(packageId, owner, relativeFile, exported, request) {
    return withLocalPackageInstallation({ ...environment.packageRoots, packageId }, async (installation) => {
      const member = installation.member_states?.find((row) => row.registry_kind === "worker_environment_kind" && row.owner === owner);
      if (!installation.available || !installation.installed || installation.receipt_state !== "valid"
        || !DIGEST.test(installation.receipt_digest) || !member?.materialized) {
        throw new Error(`Setup requires the current installed '${packageId}' package and valid receipt.`);
      }
      const file = path.join(registryRoot, "worker_environment_kind", owner, relativeFile);
      if (await fs.realpath(file) !== file) throw new Error("Installed capability setup helper must have a canonical path.");
      const helper = require(file);
      if (typeof helper[exported] !== "function") throw new Error(`Installed '${packageId}' has no '${exported}' setup helper.`);
      return helper[exported](request);
    });
  }

  async function agentPlanner({ choice, projectEeId, operationId, eeInstancesRoot }) {
    return withInstalledAgentPreset(environment.packageRoots, { ...choice, home: environment.home }, async (source) => {
      const token = hash(`${operationId}\0${projectEeId}\0${choice.presetId}`).slice(0, 32);
      const surface = await planSurface({ eeInstancesRoot, request: {
        chatId: `agent_preflight_${token}`, cycle: `agent_preflight_${token}`,
        eeId: projectEeId, clientSurfaceRole: "task", workerInterfaceId: source.recipe.worker_interface_id,
        providerInterfaceId: choice.providerInterfaceId, providerModel: choice.providerModel,
        reasoningEffort: choice.reasoningEffort, serviceTier: choice.serviceTier, doorHostSlots: {},
        contextArtifactBindings: source.recipe.context_artifact_bindings,
        selectedWorkerEnvironmentKinds: source.recipe.selected_worker_environment_kinds,
      } });
      if (surface.authoringProfile.ee_id !== projectEeId || surface.authoringProfile.worker_choice_interface !== source.sourceInterface.id
        || surface.providerModel !== choice.providerModel) throw new Error("Agent preset planning changed its Project, worker, or model.");
      if (choice.serviceTier && surface.serviceTier !== choice.serviceTier) throw new Error("Agent preset planning changed its selected service tier");
      if (source.recipe.selected_application_ids.length) throw new Error("The selected Agent preset needs an Application binding planner that this client does not provide.");
      return { ...planClientAgentTemplate({ operationId, projectEeId, sourceInterface: source.sourceInterface,
        sourcePolicy: source.sourcePolicy, sourceProfile: surface.authoringProfile,
        settings: { schema_version: 1, worker_interface: source.sourceInterface.id, project_ee_id: projectEeId,
          provider_interface_id: choice.providerInterfaceId, provider_model: choice.providerModel,
          reasoning_effort: choice.reasoningEffort, ...(choice.serviceTier ? { service_tier: choice.serviceTier } : {}), application_ids: source.recipe.selected_application_ids,
          worker_environment_kinds: source.recipe.selected_worker_environment_kinds },
        name: source.recipe.name, icon: source.recipe.icon }), verify: source.verify };
    });
  }

  async function workspaceProof(plan, context) {
    const state = await readWorkspaceSetupState(roots);
    if (state.status === "degraded") throw new Error(`Workspace evidence is degraded: ${state.errors.join("; ")}`);
    if (state.status !== "configured" || state.receipt.operation_id !== context.operationId
      || state.receipt.request_digest !== plan.workspace.request_digest) return null;
    const references = [setupReference("workspace_receipt", context.operationId, state.receipt)];
    for (const entry of plan.workspace.project_defaults) {
      const context = await projectDefaultContext(getProjection(), entry, state.receipt);
      if (!context.proven) return null;
      references.push(projectDefaultReference(entry, context));
    }
    const triggers = await setupTriggerProof(registryRoot, environment.home, await triggerEntries(plan));
    if (!triggers) return null;
    for (const trigger of triggers) references.push(setupReference("workspace_receipt", `trigger.${trigger.interface_id}`, trigger));
    return references;
  }

  async function triggerEntries(plan) {
    const catalog = await verifySetupPlan(environment, plan);
    return setupTriggerEntries(catalog.packs.find((row) => row.id === plan.pack.id), plan.pack.selected_recommended);
  }

  return {
    packages: {
      async probe(plan) {
        const catalog = await verifySetupPlan(environment, plan);
        const byId = new Map(catalog.packages.map((row) => [row.id, row]));
        const references = [];
        for (const expected of plan.packages) {
          const actual = byId.get(expected.id);
          if (!actual?.installed || actual.receipt_state !== "valid" || actual.source_digest !== expected.source_digest) return null;
          references.push(setupReference("package_install_evidence", expected.id, {
            installed_digest: actual.installed_digest, receipt_operation_id: actual.receipt_operation_id, source_digest: actual.source_digest,
          }));
        }
        return references;
      },
      async apply(plan, context) {
        checkCancelled(options.signal);
        await verifySetupPlan(environment, plan);
        await applyLocalApplicationPack({ ...environment.packageRoots, operationId: context.operationId,
          packId: plan.pack.id, selectedRecommended: plan.pack.selected_recommended,
          expectedSourceDigests: Object.fromEntries(plan.packages.map((row) => [row.id, row.source_digest])),
          assertMutable: loader.assertMutationAllowed, onProgress: options.onProgress });
        projection?.invalidate();
      },
    },
    extensions: {
      async probe(plan) { if (plan.extensions.length) throw new Error("This terminal setup does not install client Extensions. Resume this plan in the desktop client."); return []; },
      async apply() { throw new Error("Client Extensions require their owning desktop setup client."); },
    },
    workspace: {
      async probe(plan, context) {
        const client = await loader.load();
        if (typeof client.withOperatorWorkspaceTopologyLock !== "function") throw new Error("Installed operator chat package has no workspace topology lock.");
        return client.withOperatorWorkspaceTopologyLock(environment.home, () => workspaceProof(plan, context));
      },
      async apply(plan, context) {
        checkCancelled(options.signal);
        const client = await loader.load();
        if (typeof client.withOperatorWorkspaceTopologyLock !== "function") throw new Error("Installed operator chat package has no workspace topology lock.");
        await client.withOperatorWorkspaceTopologyLock(environment.home, async () => {
          const state = await readWorkspaceSetupState(roots);
          if (state.status === "not_configured") await initializeWorkspace({ ...roots, request: plan.workspace.request,
            planOperatorSurface: planSurface, planAgentPreset: agentPlanner,
            planBrowserNativeHost: (request) => packagePlanner("driver.browser_session", "browser_session",
              "packages/octopus-browser-session-env/native/setup-host.cjs", "planBrowserNativeHost", request),
            planDesktopApplicationPreferences: (request) => packagePlanner("driver.desktop_session", "desktop_session",
              "packages/octopus-system-gui-env/native/setup-preferences.cjs", "planDesktopApplicationPreferences", request),
            onProgress: options.onProgress });
          else if (state.status !== "configured" || state.receipt.operation_id !== context.operationId
            || state.receipt.request_digest !== plan.workspace.request_digest) throw new Error("Another setup already owns this Home, or workspace evidence is degraded.");
          const configured = await readWorkspaceSetupState(roots);
          for (const entry of plan.workspace.project_defaults) {
            const target = await projectDefaultContext(getProjection(), entry, configured.receipt);
            if (target.proven) continue;
            const request = entry.product_kind === "application"
              ? { action: "configure_application", applicationId: entry.product_id,
                projects: target.targets.map((row) => ({ eeId: row.eeId, expectedDigest: row.recordDigest,
                  available: true, selectedByDefault: entry.default_for_new_tasks })) }
              : { action: "configure_driver", driverName: entry.product_id,
                projects: target.targets.map((row) => ({ eeId: row.eeId, expectedDigest: row.recordDigest,
                  available: true, selectedByDefault: entry.default_for_new_tasks, hostSlots: row.hostSlots })) };
            await mutateOctopusOsWorkspace(registryRoot, environment.home, target.scope, request, {
              commitClientAuthoring: (request) => commitProjectionClientAuthoring(environment.home, registryRoot, request,
                { publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) }),
            });
            getProjection().invalidate();
          }
          const entries = await triggerEntries(plan);
          await armSetupTriggers(registryRoot, environment.home, entries, {
            commitClientAuthoring: (request) => commitProjectionClientAuthoring(environment.home, registryRoot, request,
              { publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) }),
            planTriggerTask: setupTriggerTaskPlanner(planSurface, {
              eeInstancesRoot: path.join(environment.home, "state", "ee_instances"),
              rootEeId: configured.receipt.root_ee_id,
              home: plan.workspace.request?.home,
            }),
          });
          getProjection().invalidate();
          if (!await setupTriggerProof(registryRoot, environment.home, entries)) throw new Error("Setup triggers were armed without read-back proof.");
        });
      },
    },
    provider: {
      probe: metadata.setupOwner.probe,
      async apply(plan) {
        checkCancelled(options.signal);
        await ensureSignIn(environment, options);
        if (!await metadata.setupOwner.probe(plan)) throw new Error("ChatGPT sign-in completed without a readable account.");
      },
    },
    close() { projection?.close(); },
  };
}

async function projectDefaultContext(store, entry, receipt) {
  store.invalidate();
  const scope = await store.read(projectionReadRequest("projection.operator.os.scope"));
  if (scope.status !== "ready" || scope.root_ee_id !== receipt.root_ee_id || scope.home_run_id !== receipt.home_run_id
    || scope.home_interface_id !== receipt.home_interface_id) throw new Error("Project defaults require the exact configured Home scope.");
  const projectIds = new Map(receipt.project_ee_ids.map((row) => [row.key, row.ee_id]));
  const ids = [receipt.root_ee_id, ...entry.project_keys.map((key) => {
    if (!projectIds.has(key)) throw new Error(`Project default references unknown Project '${key}'.`);
    return projectIds.get(key);
  })];
  const environments = new Map([scope.root_environment, ...(scope.projects ?? [])].filter(Boolean).map((row) => [row.ee_id, row]));
  const projection = await store.read(projectionReadRequest(entry.product_kind === "application"
    ? "projection.operator.os.applications" : "projection.operator.os.drivers"));
  const capability = (entry.product_kind === "application" ? projection.applications : projection.drivers)
    .find((row) => (entry.product_kind === "application" ? row.id : row.name) === entry.product_id);
  if (!capability?.installed) throw new Error(`Project default '${entry.product_id}' is not installed.`);
  const states = new Map((capability.project_states ?? []).map((row) => [row.ee_id, row]));
  const targets = ids.map((eeId) => {
    const environment = environments.get(eeId), state = states.get(eeId);
    if (!environment || !state || state.record_digest !== environment.record_digest) throw new Error("Project default state is stale or missing.");
    return { eeId, recordDigest: environment.record_digest, policyAllowed: state.policy_allowed === true,
      selectedByDefault: state.selected_by_default === true, hostSlots: entry.product_kind === "driver" ? state.host_slots ?? {} : {} };
  });
  return { scope, targets, proven: targets.every((row) => row.policyAllowed && row.selectedByDefault === entry.default_for_new_tasks) };
}

function projectDefaultReference(entry, context) {
  return setupReference("workspace_receipt", `default.${entry.product_kind}.${entry.product_id}`, {
    package_id: entry.package_id, product_kind: entry.product_kind, product_id: entry.product_id,
    default_for_new_tasks: entry.default_for_new_tasks, project_keys: entry.project_keys,
    targets: context.targets.map((row) => ({ ee_id: row.eeId, policy_allowed: row.policyAllowed,
      selected_by_default: row.selectedByDefault, record_digest: row.recordDigest })),
  });
}

async function choosePack(catalog, options) {
  if (catalog.errors.length) throw new Error(`The package library is degraded: ${catalog.errors.join("; ")}`);
  const packs = catalog.packs.filter((pack) => pack.workspace_setup?.home);
  if (!packs.length) throw new Error("The terminal distribution has no Application Pack that can create Home.");
  const id = await choose(options, "pack", "Choose your applications", packs.map((pack) => ({ value: pack.id, label: pack.name,
    description: pack.description })), options.packId, packs[0].id);
  const pack = packs.find((row) => row.id === id);
  if (!pack) throw new Error(`Application Pack '${id}' is unavailable.`);
  return pack;
}

async function collectChoices(environment, pack, options) {
  if (options.nonInteractive && !options.model) throw new Error("Noninteractive setup requires --model.");
  const recommended = uniqueStrings(options.recommended ?? [], "recommended packages");
  const recommendedIds = new Set(pack.components.filter((row) => row.selection === "recommended").map((row) => row.package_id));
  if (recommended.some((id) => !recommendedIds.has(id))) throw new Error("A selected optional package is not recommended by this Application Pack.");
  const selected = pack.components.filter((row) => row.selection === "included" || recommended.includes(row.package_id));
  for (const row of selected) if (!row.available || !DIGEST.test(row.source_digest)) throw new Error(`Package '${row.name}' is unavailable: ${row.error || "its source proof is missing"}`);
  const workspaceName = await ask(options, "workspace_name", "Name your workspace", options.workspaceName, "My space");
  let projects = options.projects;
  if (projects === undefined) {
    const projectName = options.nonInteractive ? options.projectName ?? "" : await ask(options, "project_name", "Name your first project (leave empty to start with Home)", options.projectName, "Project", true);
    projects = projectName ? [{ name: projectName, goal: options.projectGoal ?? projectName }] : [];
  }
  if (!Array.isArray(projects) || projects.length > 24) throw new Error("Setup accepts up to 24 Projects.");
  projects = projects.map((row) => typeof row === "string" ? { name: row, goal: row } : row);
  const providers = await readProviderInterfaceDescriptors(environment.packageRoots.sourceRegistryRoot);
  const provider = providers.find((row) => row.id === pack.workspace_setup.home.provider_interface_id);
  if (!provider) throw new Error("The Application Pack's Home provider is absent from the source package registry.");
  const metadata = createClientProviderMetadata({ registryRoot: () => environment.packageRoots.sourceRegistryRoot,
    sourceRegistryRoot: () => environment.packageRoots.sourceRegistryRoot,
    accountClient: () => environment.accountClient(), reference: setupReference });
  const [discovered] = await metadata.catalog([provider]);
  if (discovered.modelCatalogError) throw new Error(discovered.modelCatalogError);
  const models = discovered.models.filter((row) => row.showInPicker);
  if (!models.length) throw new Error("Home's provider returned no selectable models.");
  const model = await choose(options, "model", "Choose a model for Home", models.map((row) => ({ value: row.model, label: row.displayName })),
    options.model, models.find((row) => row.isDefault)?.model ?? models[0].model);
  const modelRow = models.find((row) => row.model === model);
  if (!modelRow) throw new Error("Choose a model from the provider's current catalogue.");
  const reasoningEffort = options.reasoningEffort ?? (modelRow.supportedReasoningEfforts.length
    ? options.nonInteractive ? modelRow.defaultReasoningEffort : await choose(options, "reasoning", "Choose a reasoning level",
      modelRow.supportedReasoningEfforts.map((value) => ({ value, label: value })), undefined, modelRow.defaultReasoningEffort)
    : "");
  const serviceTier = options.serviceTier;
  validateProviderModelSelection([discovered], { providerInterfaceId: provider.id, providerModel: model, reasoningEffort, serviceTier });
  return { operationId: options.operationId ?? randomUUID(), workspaceName, workspaceGoal: options.workspaceGoal ?? workspaceName,
    projects, model, reasoningEffort, serviceTier, recommended, agentPresets: options.agentPresets };
}

function compileSetupPlan(pack, choices) {
  const selected = pack.components.filter((row) => row.selection === "included" || choices.recommended.includes(row.package_id));
  const projectKeys = choices.projects.map((row, index) => row.key ?? `project-${index + 1}`);
  if (new Set(projectKeys).size !== projectKeys.length) throw new Error("Project keys must be unique.");
  const recipe = pack.workspace_setup.home;
  const taskRecipe = pack.workspace_setup.task;
  if (!taskRecipe) throw new Error("The Application Pack has no declared Task recipe.");
  const projects = choices.projects.map((row, index) => ({
    key: projectKeys[index], parentKey: row.parentKey ?? "root", templateName: "project",
    name: humanText(row.name, "Project name", 80), goal: humanText(row.goal ?? row.name, "Project goal", 240),
    presentation: { icon: row.icon ?? "folder", color: row.color ?? "violet" },
  }));
  const chosenPresets = choices.agentPresets ?? (pack.workspace_setup.agent_presets ?? []).map((row) => row.id);
  const agentPresets = chosenPresets.flatMap((id) => {
    const preset = pack.workspace_setup.agent_presets?.find((row) => row.id === id);
    if (!preset) throw new Error(`Agent preset '${id}' is not in this Application Pack.`);
    return !projects.length || !selected.some((row) => row.package_id === preset.component_package_id) ? [] : [{
      packId: pack.id, presetId: id, expectedPackManifestDigest: pack.manifest_digest,
      projectKeys: [...projectKeys].sort(), providerInterfaceId: preset.provider_interface_id,
      providerModel: choices.model, reasoningEffort: choices.reasoningEffort,
      ...(choices.serviceTier ? { serviceTier: choices.serviceTier } : {}),
    }];
  });
  const request = {
    action: "initialize_workspace",
    operationId: choices.operationId,
    root: { key: "root", templateName: "existence", name: humanText(choices.workspaceName, "Workspace name", 80), goal: humanText(choices.workspaceGoal, "Workspace goal", 240) },
    home: { name: "Home", providerInterfaceId: recipe.provider_interface_id, providerModel: choices.model,
      workerInterfaceId: recipe.worker_interface_id, reasoningEffort: choices.reasoningEffort,
      ...(choices.serviceTier ? { serviceTier: choices.serviceTier } : {}), doorHostSlots: structuredClone(recipe.door_host_slots ?? {}),
      contextArtifactBindings: recipe.context_artifact_bindings, selectedWorkerEnvironmentKinds: recipe.selected_worker_environment_kinds },
    task: { workerInterfaceId: taskRecipe.worker_interface_id, contextArtifactBindings: taskRecipe.context_artifact_bindings,
      selectedWorkerEnvironmentKinds: taskRecipe.selected_worker_environment_kinds },
    projects, ...(agentPresets.length ? { agentPresets } : {}),
  };
  const defaults = pack.workspace_setup.project_defaults.flatMap((entry) => {
    const component = selected.find((row) => row.package_id === entry.package_id);
    if (!component) return [];
    if (!["application", "driver"].includes(component.product_kind)) throw new Error("Project default has no Application or Driver owner.");
    return [{ package_id: component.package_id, product_kind: component.product_kind, product_id: component.product_id,
      default_for_new_tasks: entry.default_for_new_tasks, project_keys: [...projectKeys].sort() }];
  });
  return {
    pack: { id: pack.id, manifest_digest: pack.manifest_digest, selected_recommended: [...choices.recommended].sort() },
    packages: selected.map((row) => ({ id: row.package_id, source_digest: row.source_digest, desired_state: "installed" })).sort(byId),
    extensions: [], provider: { interface_id: recipe.provider_interface_id, required_state: "account_readable" },
    workspace: { request_digest: workspaceInitializeRequestDigest(request), request,
      project_defaults: defaults.sort((a, b) => a.package_id.localeCompare(b.package_id)) },
  };
}

async function verifySetupPlan(environment, plan) {
  const catalog = await readLocalPackageCatalog(environment.packageRoots);
  const pack = catalog.packs.find((row) => row.id === plan.pack.id);
  if (!pack || pack.manifest_digest !== plan.pack.manifest_digest) throw new Error("Application Pack changed after the setup choices were saved.");
  const request = plan.workspace.request;
  const expected = compileSetupPlan(pack, {
    operationId: request.operationId, workspaceName: request.root.name, workspaceGoal: request.root.goal,
    projects: request.projects.map((row) => ({ ...row, ...row.presentation })),
    model: request.home.providerModel, reasoningEffort: request.home.reasoningEffort, serviceTier: request.home.serviceTier,
    recommended: plan.pack.selected_recommended, agentPresets: (request.agentPresets ?? []).map((row) => row.presetId),
  });
  if (stableJson(plan) !== stableJson(expected)) throw new Error("Saved setup choices no longer match the Application Pack's recipe and source files.");
  return catalog;
}

async function ensureSignIn(environment, options) {
  const client = environment.accountClient();
  if ((await client.account()).signedIn) return;
  let allowed = options.signIn === true;
  if (!allowed && !options.nonInteractive) {
    allowed = await choose(options, "sign_in", "Sign in with ChatGPT to continue", [
      { value: "sign_in", label: "Sign in with ChatGPT" }, { value: "cancel", label: "Cancel setup" },
    ], undefined, "sign_in") === "sign_in";
  }
  if (!allowed) throw new Error("ChatGPT is not signed in. Run interactive setup or explicitly allow sign-in with an address handler.");
  const openExternal = options.ui?.openExternal;
  if (typeof openExternal !== "function") throw new Error("Sign-in requires ui.openExternal(url) to display the native sign-in address.");
  environment.accountClient({ openExternal });
  const { loginId } = await client.start();
  const deadline = Date.now() + (options.signInTimeoutMs ?? 300_000);
  try {
    do {
      checkCancelled(options.signal);
      const observed = client.status(loginId);
      if (observed.state === "completed") {
        if (!(await client.account()).signedIn) throw new Error("Native sign-in completed without a readable account.");
        return;
      }
      if (observed.state === "failed") throw new Error(observed.error || "ChatGPT sign-in failed.");
      await delay(300, undefined, { signal: options.signal });
    } while (Date.now() < deadline);
    throw new Error("ChatGPT sign-in was not completed in time.");
  } catch (error) { await client.cancel(loginId); throw error; }
}

async function ask(options, id, prompt, value, defaultValue, emptyAllowed = false) {
  if (value === undefined) {
    if (options.nonInteractive || typeof options.ui?.ask !== "function") throw new Error(`Setup requires an explicit ${id.replaceAll("_", " ")} in noninteractive mode.`);
    value = await options.ui.ask({ id, prompt, defaultValue });
  }
  checkCancelled(options.signal);
  if (emptyAllowed && value === "") return "";
  return humanText(value, prompt, 80);
}

async function choose(options, id, prompt, choices, value, defaultValue) {
  if (value === undefined) {
    if (options.nonInteractive || typeof options.ui?.choose !== "function") throw new Error(`Setup requires an explicit ${id.replaceAll("_", " ")} in noninteractive mode.`);
    value = await options.ui.choose({ id, prompt, choices, defaultValue });
  }
  checkCancelled(options.signal);
  if (!choices.some((row) => row.value === value)) throw new Error(`Invalid ${id.replaceAll("_", " ")} selection.`);
  return value;
}

function hasNewChoices(options) { return ["packId", "workspaceName", "workspaceGoal", "projectName", "projects", "model", "reasoningEffort", "serviceTier", "agentPresets"].some((key) => options[key] !== undefined) || options.recommended?.length > 0; }
function assertSavedChoices(options, plan) {
  const expected = { packId: plan.pack.id, workspaceName: plan.workspace.request.root.name,
    workspaceGoal: plan.workspace.request.root.goal, model: plan.workspace.request.home.providerModel,
    reasoningEffort: plan.workspace.request.home.reasoningEffort, serviceTier: plan.workspace.request.home.serviceTier };
  for (const [field, value] of Object.entries(expected)) {
    if (options[field] !== undefined && options[field] !== value) throw new Error(`Saved setup already chose ${field}. Rerun without replacement choices.`);
  }
  if (options.projectName !== undefined && (plan.workspace.request.projects.length !== 1
    || plan.workspace.request.projects[0].name !== options.projectName)) throw new Error("Saved setup has a different Project selection.");
  if (options.recommended?.length && stableJson([...options.recommended].sort()) !== stableJson(plan.pack.selected_recommended)) throw new Error("Saved setup has a different optional package selection.");
  if (options.projects !== undefined && stableJson(options.projects.map((row) => typeof row === "string" ? row : row.name))
    !== stableJson(plan.workspace.request.projects.map((row) => row.name))) throw new Error("Saved setup has different Projects.");
  if (options.agentPresets !== undefined && stableJson([...options.agentPresets].sort())
    !== stableJson((plan.workspace.request.agentPresets ?? []).map((row) => row.presetId).sort())) throw new Error("Saved setup has different Agent presets.");
}
function humanText(value, name, max) { if (typeof value !== "string" || !value.trim() || value.trim() !== value || value.length > max || /[\x00-\x1f\x7f]/.test(value)) throw new Error(`${name} must be nonempty text of at most ${max} characters.`); return value; }
function uniqueStrings(value, label) { if (!Array.isArray(value) || value.some((row) => typeof row !== "string" || !row) || new Set(value).size !== value.length) throw new Error(`${label} must be a list of unique names.`); return value; }
function checkCancelled(signal) { if (signal?.aborted) { const error = new Error("Workspace setup cancelled. Rerun setup to resume its saved choices."); error.code = "ABORT_ERR"; throw error; } }
function byId(a, b) { return a.id.localeCompare(b.id); }
function hash(value) { return createHash("sha256").update(value).digest("hex"); }
function stableJson(value) { if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`; if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`; return JSON.stringify(value); }
function setupReference(kind, id, evidence) { return { kind, id, digest: hash(stableJson(evidence)), evidence }; }

module.exports = { setupEnvironment, setup: setupEnvironment, createSetupOwners, compileSetupPlan, setupReference, ensureSignIn };
