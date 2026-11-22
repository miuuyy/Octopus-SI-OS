const fs = require("node:fs/promises");
const path = require("node:path");
const { randomUUID, createHash } = require("node:crypto");
const { promisify } = require("node:util");
const { execFile } = require("node:child_process");
const { createProjectionStore, mutateOctopusOsWorkspace } = require("../electron/projection.cjs");
const { projectionReadRequest } = require("../electron/projectionReadContract.cjs");
const { createClientProviderMetadata, validateProviderModelSelection } = require("../electron/clientProviderMetadata.cjs");
const { createOperatorChatClientLoader, assertOperatorChatArtifactBatch } = require("../electron/operatorChatClientContract.cjs");
const { handInArtifact } = require("../electron/interfaceTransport.cjs");
const { readContentAddressedArtifactPayload, readContentAddressedArtifactProjection, prepareContentAddressedPayloadBindings } = require("../electron/contentAddressedArtifact.cjs");
const { createClientAttachmentStagingStore, stageClientAttachmentPaths } = require("../electron/clientAttachments.cjs");
const { submitClientInterfaceArtifact } = require("../electron/clientArtifacts.cjs");
const { commitProjectionClientAuthoring } = require("../electron/clientWorkspaceAuthoring.cjs");
const { mutateWorkspaceProject } = require("../electron/workspaceSetup.cjs");
const { taskRetirementBarrier, requireDistribution } = require("./environment.cjs");
const shared = require("../build/cli/shared.cjs");

const execute = promisify(execFile);

function createClient(environment, dependencies = {}) {
  const env = environment.config ?? environment;
  const registryRoot = path.join(env.home, "registry");
  const owner = `terminal_${randomUUID()}`;
  let closed = false;
  const store = dependencies.store ?? createProjectionStore({
    resolveRepoRoot: () => env.repoRoot,
    sourceRegistry: () => env.sourceRegistry,
    localPackageRegistry: () => env.packageRoots.sourceRegistryRoot,
    localPackageLibrary: () => env.packageRoots.libraryRoot,
    octopusHome: () => env.home,
    projectEePolicies: async () => {
      const { stdout } = await execute(env.runtimeBinary, ["project-ee-policies"], {
        env: { ...process.env, ...env.launchEnv },
        cwd: env.installationRoot,
        maxBuffer: 32 * 1024 * 1024,
        timeout: 120_000,
      });
      const result = JSON.parse(stdout);
      if (!Array.isArray(result.environments)) throw new Error("The runtime returned an invalid policy projection.");
      return result;
    },
  });
  const rawLoader = dependencies.loader ?? createOperatorChatClientLoader({
    packageRoots: () => env.packageRoots,
    clientPath: () => path.join(registryRoot, "interface", "world_operator_chat", "client.cjs"),
  });
  const loader = dependencies.loader ? rawLoader : {
    ...rawLoader,
    async load() {
      requireDistribution(env);
      return rawLoader.load();
    },
  };
  const metadata = dependencies.metadata ?? createClientProviderMetadata({
    sourceRegistryRoot: () => env.packageRoots.sourceRegistryRoot,
    registryRoot: () => registryRoot,
    accountClient: () => environment.accountClient(),
  });
  const staging = createClientAttachmentStagingStore({
    root: () => path.join(env.home, "client", "attachment-staging"),
  });
  const artifactsRoot = (interfaceId) => path.join(env.home, "interface-hand-in", interfaceId, "client");
  const readPayload = (input) => readContentAddressedArtifactPayload({
    ...input,
    blobsRoot: path.join(env.home, "ledger", "blobs"),
    maxPayloadBytes: input.label === "turn.stream" ? 256 * 1024 * 1024 : undefined,
  });
  const readActivityPayload = (input) => Object.hasOwn(input, "projection")
    ? readContentAddressedArtifactProjection({ ...input, blobsRoot: path.join(env.home, "ledger", "blobs") })
    : readPayload(input);
  const submitOptions = {
    clientArtifactsRoot: artifactsRoot,
    octopusHome: () => env.home,
    installationRoot: () => env.installationRoot,
    registryRoot: () => registryRoot,
    handInArtifact: (input) => handInArtifact({ ...input, octopusHome: env.home }),
    readArtifactPayload: readPayload,
  };

  async function read(method, params) {
    if (closed) throw new Error("This terminal client is closed.");
    return store.read(projectionReadRequest(method, params));
  }

  async function status() {
    const runtime = await environment.status();
    if (!(runtime.ready || runtime.runtimeReady)) return { ...runtime, home: env.home };
    const scope = await read("projection.operator.os.scope");
    return { ...runtime, home: env.home, workspace: scope.status, workspaceErrors: scope.errors ?? [] };
  }

  async function requireReady() {
    const runtime = await environment.status();
    if (!(runtime.ready || runtime.runtimeReady)) {
      throw Object.assign(new Error("Octopus is not running. Start it with 'octopus up' or open the interactive client."), { code: "runtime_stopped" });
    }
  }

  async function catalogue() {
    const { providers } = await read("projection.operator.providers.list");
    return metadata.catalog(providers);
  }

  async function contextData() {
    const raw = await read("projection.operator.chat.contexts");
    const contexts = shared.projectChatExecutionContexts(raw);
    const agents = shared.visibleClientAgents(shared.resolveClientAgents(contexts));
    return { raw, contexts, agents };
  }

  async function contexts() {
    const [{ contexts: all, agents }, providers] = await Promise.all([contextData(), catalogue()]);
    return {
      projects: all.map((context) => ({
        id: context.eeId,
        name: context.displayName || (context.isOsRoot ? "Home" : context.goal),
        isHome: context.isOsRoot,
        sourceWorkerId: context.workers.find((worker) => worker.agentSourceTemplate)?.id ?? "",
        agents: agents.filter((agent) => agent.projectEeId === context.eeId).map(presentAgent),
      })),
      providers,
    };
  }

  async function tasks(options = {}) {
    const result = await read("projection.operator.chats.list");
    const raw = new Map(result.chats.map((row) => [row.run_id, row]));
    const query = String(options.search ?? "").toLocaleLowerCase();
    return shared.projectChatRuns(result).filter((run) => run.chatInterfaceId).map((run) => ({
      ...run,
      id: run.runId, projectId: run.eeId,
      agentId: raw.get(run.runId)?.agent_id ?? "",
      agentName: raw.get(run.runId)?.agent_name ?? "",
      providerId: run.providerInterfaceId, model: run.providerModel,
      effort: run.reasoningEffort, state: run.turnState,
      isHome: run.clientSurfaceRole === "home",
    })).filter((task) => (
      (options.archived === undefined || task.archived === options.archived)
      && (!query || `${task.title} ${task.id}`.toLocaleLowerCase().includes(query))
    ));
  }

  async function task(selector) {
    const all = await tasks();
    if (selector === "home") return exactChoice(all.filter((row) => row.isHome), "Home");
    if (selector === "last") return exactChoice(all.filter((row) => !row.isHome && !row.archived).slice(0, 1), "recent task");
    return select(all, selector, "task", "title");
  }

  async function models(selection = {}) {
    const providers = await catalogue();
    let allowed = providers;
    if (selection.runId) {
      const current = await task(selection.runId);
      allowed = providers.filter((provider) => provider.id === current.providerId);
    } else if (selection.agentId || selection.projectId || selection.workerId) {
      const { contexts: all, agents } = await contextData();
      const agent = selection.agentId ? select(agents, selection.agentId, "Agent") : null;
      const context = all.find((row) => row.eeId === (agent?.projectEeId || selection.projectId));
      const worker = context?.workers.find((row) => row.id === (agent?.workerInterfaceId || selection.workerId || context.workers.find((row) => row.agentSourceTemplate)?.id));
      if (!worker) throw new Error("Select an available Project and Agent before choosing a model.");
      allowed = providers.filter((provider) => worker.providerInterfaceIds.includes(provider.id))
        .map((provider) => shared.providerForExecutionContext(provider, worker));
    }
    if (selection.providerId) allowed = allowed.filter((provider) => provider.id === selection.providerId);
    return allowed.flatMap((provider) => shared.providerModels(provider).map((model) => ({
      ...model, providerId: provider.id, providerLabel: provider.label,
      reasoningEfforts: model.supportedReasoningEfforts,
    })));
  }

  async function rawHistory(runId) {
    const current = await task(runId);
    const result = await read("projection.operator.chat.messages", { run_id: current.id });
    return result.messages;
  }

  async function history(runId) {
    return require("./transcript.cjs").normalizeHistory(await rawHistory(runId));
  }

  async function send(input) {
    await requireReady();
    const current = input.runId ? await task(input.runId) : null;
    if (current && (!current.writable || current.archived)) throw new Error("This task cannot accept messages. Restore it if archived.");
    if (current?.turnOpen) throw Object.assign(new Error("This task is still working. Follow it or stop it before sending another message."), { code: "task_busy" });
    if (current && await hasPendingStop(current.id)) {
      throw Object.assign(new Error("This task is stopping. Wait for the current operation to report its result before sending another message."), { code: "task_stopping" });
    }
    const { contexts: all, agents } = await contextData();
    const agent = current ? null : select(agents, input.agentId, "Agent");
    if (agent && input.projectId && input.projectId !== agent.projectEeId) throw new Error("The selected Agent belongs to another Project.");
    const projectId = current?.projectId || agent.projectEeId;
    const workerId = current ? current.workerChoiceInterfaceId || current.workerInterfaceId : agent.workerInterfaceId;
    const context = all.find((row) => row.eeId === projectId);
    const worker = context?.workers.find((row) => row.id === workerId);
    const providerId = input.providerId ?? current?.providerId ?? agent.providerInterfaceId;
    if (current && providerId !== current.providerId) throw new Error("Continue this task with its existing provider. Select another provider for a new task.");
    const model = input.model ?? current?.model ?? agent.providerModel;
    const effort = input.effort ?? current?.effort ?? agent.reasoningEffort;
    const serviceTier = input.serviceTier ?? current?.serviceTier ?? agent?.serviceTier;
    const providers = await catalogue();
    const provider = providers.find((row) => row.id === providerId);
    const admittedProvider = current ? provider : provider && worker?.providerInterfaceIds.includes(providerId)
      ? shared.providerForExecutionContext(provider, worker) : null;
    if (!admittedProvider) throw new Error("The selected provider is not available to this Agent.");
    validateProviderModelSelection([admittedProvider], { providerInterfaceId: providerId, providerModel: model, reasoningEffort: effort, serviceTier });
    const capabilities = current ? {} : shared.executionContextTaskCapabilities(
      all, projectId, workerId,
      input.applicationIds ?? agent.applicationIds,
      input.driverNames ?? agent.driverNames,
      input.access ?? context.defaultTaskAccessMode,
    );
    const selected = [];
    const consumed = [];
    let failure;
    try {
      if (input.files?.length) {
        const result = await stageClientAttachmentPaths(input.files.map((file) => path.resolve(file)), {
          stageFile: (request, file) => staging.stageFile(owner, request, file),
          stageBytes: (request, bytes) => staging.stageBytes(owner, request, bytes),
          discard: async (handle) => staging.discard(await staging.consume(owner, handle)),
        });
        selected.push(...result.attachments);
        for (const handle of selected) consumed.push(await staging.consume(owner, handle));
      }
      const chat = await loader.load();
      const result = await chat.submitOperatorTurn({
        ...submitOptions,
        preparePayloadArtifactBindings: ({ payloadArtifactBindings }) => prepareContentAddressedPayloadBindings({
          blobsRoot: path.join(env.home, "ledger", "blobs"), attachments: consumed,
          payloadArtifactBindings, revalidate: requireReady,
        }),
      }, {
        ...capabilities,
        runId: current?.id,
        messageId: input.messageId || `msg_${randomUUID().replaceAll("-", "")}`,
        providerInterfaceId: providerId, providerModel: model, reasoningEffort: effort, serviceTier,
        eeId: projectId, workerInterfaceId: workerId,
        clientSurfaceRole: current?.clientSurfaceRole || "task",
        text: input.text ?? "",
        attachments: consumed,
      });
      assertAdmission(result.messageVerdict);
      store.invalidate(!current, current ? "runs" : "structure");
      return { ...result, cursor: result.outboundCursor, afterSeq: result.messageVerdict.admitted.seq };
    } catch (error) {
      failure = error;
      throw error;
    } finally {
      const cleanup = await Promise.allSettled(consumed.map((file) => staging.discard(file)));
      const failed = cleanup.filter((row) => row.status === "rejected");
      if (failed.length && !failure) throw new AggregateError(failed.map((row) => row.reason), "The message was sent, but attachment staging cleanup failed.");
    }
  }

  async function* follow(runId, options = {}) {
    const current = await task(runId);
    if (!current.chatInterfaceId) throw new Error("This task has no operator interface.");
    const { createTranscriptReducer } = require("./transcript.cjs");
    const reducer = createTranscriptReducer({ history: await rawHistory(current.id) });
    const initial = await taskStatus(current.id);
    const afterSeq = options.afterSeq ?? initial.turn_opened_movement_seq;
    if (!Number.isSafeInteger(afterSeq) || afterSeq < 0) throw new Error("This task has no recorded turn to follow.");
    if (options.messageVerdict) {
      if (options.runId !== current.id || options.chatInterfaceId !== current.chatInterfaceId
        || options.modelInterfaceId !== current.providerBoundInterfaceId) throw new Error("The submission receipt belongs to a different task.");
      if (afterSeq !== options.messageVerdict.admitted?.seq) throw new Error("The follow cursor has a different admission frontier from its receipt.");
      const events = reducer.beginTurn(options);
      for (const entry of reducer.snapshot()) {
        const m = entry.metadata;
        if (m.direction === "out" && (m.source === "movement" ? m.movementSeq > afterSeq : m.sourceMovementSeq !== null && m.sourceMovementSeq >= afterSeq)) {
          if (entry.request && entry.status !== "resolved") {
            yield { type: "request", id: entry.id, kind: entry.request.kind, payload: entry.request.payload,
              requestId: entry.request.payload?.request_id, status: entry.status, entry, runId: current.id };
          } else yield { type: "message", entry, runId: current.id };
        }
      }
      for (const event of events) yield event;
      if (reducer.currentTurn().terminal) return;
    } else {
      const recorded = reducer.snapshot();
      const events = reducer.beginWatch({ runId: current.id, chatInterfaceId: current.chatInterfaceId,
        modelInterfaceId: current.providerBoundInterfaceId, afterSeq, messageId: options.messageId });
      if (reducer.currentTurn().terminal) {
        for (const entry of recorded) yield { type: "message", entry, runId: current.id };
        for (const event of events) yield event;
        return;
      }
      for (const event of events) yield event;
      for (const entry of reducer.snapshot()) {
        if (entry.request?.status === "pending" || entry.request && entry.status !== "resolved") {
          yield { type: "request", id: entry.id, kind: entry.request.kind, payload: entry.request.payload,
            requestId: entry.request.payload?.request_id, status: entry.status, entry, runId: current.id };
        }
      }
    }
    const chat = await loader.load();
    let cursor = options.cursor;
    let lastProbe = 0;
    for (;;) {
      if (options.signal?.aborted || closed) return;
      if (Date.now() - lastProbe > 5_000) { await requireReady(); lastProbe = Date.now(); }
      const batch = assertOperatorChatArtifactBatch(await chat.readOperatorChatArtifacts({
        octopusHome: () => env.home, readArtifactPayload: readActivityPayload,
      }, { runId: current.id, interfaceId: current.chatInterfaceId, afterSeq, cursor, waitMs: 500, limit: 256 }));
      cursor = batch.cursor;
      if (options.signal?.aborted || closed) return;
      const events = reducer.pushBatch(batch);
      for (const event of events) yield { ...event, ...(event.type === "request" ? { requestId: event.payload?.request_id } : {}), runId: current.id, cursor };
      if (events.some((event) => event.type === "status" && event.terminal && event.correlated)) return;
      if (reducer.currentTurn()?.correlationError) throw new Error(reducer.currentTurn().correlationError);
    }
  }

  async function taskStatus(runId) {
    const result = await read("projection.operator.chats.status", { run_ids: [runId] });
    return exactChoice(result.chats, "recorded task status");
  }

  async function stop(runId) {
    await requireReady();
    const current = await controlTarget(runId);
    if (!current.writable) throw new Error("This task cannot accept Stop.");
    const chat = await loader.load();
    const result = await chat.submitOperatorTurnControl(submitOptions, {
      interfaceId: current.chatInterfaceId, runId: current.id,
      action: "stop", controlId: `control_${randomUUID().replaceAll("-", "")}`, timeoutMs: 45_000,
    });
    assertAdmission(result.verdict);
    store.invalidate(false, "runs");
    return result;
  }

  async function controlTarget(selector) {
    if (typeof selector !== "string" || !selector.trim()) throw new Error("Choose a task.");
    const scope = await read("projection.operator.os.scope");
    if (scope.status !== "ready" || !Array.isArray(scope.client_runs)) throw new Error("Control target lookup requires the current valid client scope.");
    const ids = new Set();
    const targets = scope.client_runs.map(row => {
      if (!row || typeof row.run_id !== "string" || !row.run_id || typeof row.interface_id !== "string" || !row.interface_id
        || !["home", "task"].includes(row.surface_role) || typeof row.archived !== "boolean" || ids.has(row.run_id)) {
        throw new Error("The client scope contains an invalid control target.");
      }
      ids.add(row.run_id);
      return { id: row.run_id, chatInterfaceId: row.interface_id, isHome: row.surface_role === "home", archived: row.archived, writable: !row.archived };
    });
    if (selector === "home") return exactChoice(targets.filter(row => row.isHome && row.id === scope.home_run_id
      && row.chatInterfaceId === scope.home_interface_id), "Home");
    const exact = targets.find(row => row.id === selector);
    if (exact) return exact;
    const aliases = await read("projection.operator.chats.list", { control_targets_only: true, include_titles: selector !== "last" });
    if (!Array.isArray(aliases.chats)) throw new Error("Control target metadata is invalid.");
    const targetById = new Map(targets.map(row => [row.id, row]));
    const candidates = aliases.chats.map(row => {
      const target = targetById.get(row.run_id);
      if (!target || target.chatInterfaceId !== row.chat_interface_id || typeof row.writable !== "boolean") {
        throw new Error("Control target metadata differs from the current client scope.");
      }
      return { ...target, title: row.title, writable: target.writable && row.writable };
    });
    if (selector === "last") return exactChoice(candidates.filter(row => !row.isHome && !row.archived).slice(0, 1), "recent task");
    return exactChoice(candidates.filter(row => row.title === selector || row.id.startsWith(selector)), "task");
  }

  async function respond(runId, request, answer) {
    await requireReady();
    const current = await task(runId);
    if (!current.writable) throw new Error("This task cannot accept an answer.");
    const pending = await rawHistory(current.id);
    const requestId = typeof request === "string" ? request : request.payload?.request_id ?? request.entry?.request?.id ?? request.id;
    const target = exactChoice(pending.filter((row) => (
      ["approval", "elicitation", "user_input"].includes(row.event_kind)
      && row.event_data?.resolved !== true
      && (!request?.kind || row.event_kind === request.kind)
      && [row.id, row.event_data?.request_id].includes(requestId)
    )), "pending question");
    const kind = target.event_kind;
    const data = target.event_data;
    const interfaceId = target.interface_id;
    if (!data.request_id || interfaceId !== current.chatInterfaceId || target.run_id !== current.id) {
      throw new Error("The question has no exact task and client provenance.");
    }
    let payload;
    let label;
    if (kind === "approval") {
      const bound = ["application", "browser_file_upload", "browser_history_read"].includes(data.kind);
      const decision = bound
        ? shared.operatorPermissionAnswer(data, answer?.kind === "approval" || typeof answer?.decision === "object" ? answer.decision : answer, interfaceId)
        : { decision: answer.decision ?? answer };
      if (bound && !data.call_id) throw new Error("Permission request has no exact tool call.");
      payload = { operator_approval_response: { request_id: data.request_id, ...(bound ? { call_id: data.call_id } : {}), ...decision } };
      label = "operator.approval.response";
    } else if (kind === "elicitation") {
      payload = { operator_elicitation_response: { request_id: data.request_id, answer: answer.answer ?? answer } };
      label = "operator.elicitation.response";
    } else {
      payload = { operator_user_input_response: { request_id: data.request_id, answers: answer.answers ?? answer } };
      label = "operator.user_input.response";
    }
    const chat = await loader.load();
    const result = await submitClientInterfaceArtifact({
      ...submitOptions, projectionStore: () => store,
      authorizeSubmission: (input) => chat.assertGenericOperatorChatArtifactAllowed(input),
    }, { interfaceId, runId: current.id, label, payload, timeoutMs: 45_000 });
    store.invalidate(false, "runs");
    return result;
  }

  async function mutate(request) {
    await requireReady();
    store.invalidate();
    const [scope, { raw }] = await Promise.all([read("projection.operator.os.scope"), contextData()]);
    const chat = await loader.load();
    const { taskLifecycle, retirementBarrier } = await chat.loadOperatorTaskLifecycle(submitOptions);
    const result = await mutateOctopusOsWorkspace(registryRoot, env.home, scope, request, {
      chatContexts: raw,
      taskLifecycle,
      commitClientAuthoring: (plan) => commitProjectionClientAuthoring(env.home, registryRoot, plan,
        { publication: env.packageRoots.publication, retirementBarrier }),
      withTaskAuthoringLock: chat.withOperatorRunAuthoringLock,
      readTaskStatus: async (id) => {
        store.invalidate(false, "runs");
        return (await read("projection.operator.chats.status", { run_ids: [id] })).chats.find((row) => row.run_id === id);
      },
      releaseTaskWorkerEnvironments: (input) => chat.releaseOperatorTaskWorkerEnvironments(submitOptions, input),
      mutateWorkspaceProject: (scope, request) => chat.withOperatorWorkspaceTopologyLock(env.home, () => mutateWorkspaceProject({
        octopusHomeRoot: env.home, registryRoot, scope, request, publication: env.packageRoots.publication, retirementBarrier,
        planOperatorSurface: ({ eeInstancesRoot, request }) => chat.planOperatorChatSurface({
          ...submitOptions, eeInstancesRoot: () => eeInstancesRoot,
        }, request),
      })),
    });
    store.invalidate();
    return result;
  }

  async function createAgent(input) {
    const { contexts: all } = await contextData();
    const context = select(all.map((row) => ({ ...row, id: row.eeId, name: row.displayName || row.goal })), input.projectId, "Project");
    const source = context.workers.find((row) => row.agentSourceTemplate);
    if (!source) throw new Error("This Project has no Agent template source.");
    const providerId = input.providerId;
    const providers = await catalogue();
    const provider = providers.find((row) => row.id === providerId);
    if (!provider || !source.providerInterfaceIds.includes(providerId)) throw new Error("Choose a provider available to this Project.");
    validateProviderModelSelection([shared.providerForExecutionContext(provider, source)], {
      providerInterfaceId: providerId, providerModel: input.model, reasoningEffort: input.effort ?? "", serviceTier: input.serviceTier,
    });
    const scope = await read("projection.operator.os.scope");
    return mutate({
      action: "create_agent_template", operationId: randomUUID(), workspace: workspaceIdentity(scope, true),
      projectEeId: context.eeId, sourceWorkerInterfaceId: source.id,
      name: input.name, icon: "default", providerInterfaceId: providerId, providerModel: input.model,
      reasoningEffort: input.effort ?? "", serviceTier: input.serviceTier,
      applicationIds: input.applicationIds ?? source.applications.filter((row) => row.selectedByDefault).map((row) => row.id),
      driverNames: input.driverNames ?? source.drivers.filter((row) => row.selectedByDefault).map((row) => row.name),
    });
  }

  async function createProject(input) {
    const scope = await read("projection.operator.os.scope");
    const parentId = input.parentId || scope.root_ee_id;
    const parent = scope.environments.find((row) => row.ee_id === parentId);
    if (!parent) throw new Error("Choose an existing parent Project.");
    const policies = await projectPolicies(parentId);
    const policy = input.policy ? policies.find((row) => row.id === input.policy)
      : policies.find((row) => row.inheritsParentPolicy);
    if (!policy) throw new Error("Choose an installed policy allowed by this parent Project.");
    return mutate({
      action: "create_project", operationId: randomUUID(), workspace: workspaceIdentity(scope),
      parentEeId: parentId, expectedParentDigest: parent.record_digest,
      templateName: policy.id, name: input.name, goal: input.name,
      presentation: { icon: "folder", color: "gray" },
    });
  }

  async function setProjectFolders(projectId, folders) {
    const scope = await read("projection.operator.os.scope");
    const target = scope.environments.find((row) => row.ee_id === projectId);
    if (!target || projectId === scope.root_ee_id) throw new Error("Choose an existing Project.");
    const presentations = scope.project_presentations ?? [];
    if (!presentations.some((row) => row.ee_id === projectId)) throw new Error(`Project '${projectId}' has no presentation.`);
    return mutate({
      action: "update_project_presentation", operationId: randomUUID(), workspace: workspaceIdentity(scope),
      projects: presentations.map((row) => ({
        eeId: row.ee_id,
        ...(row.ee_id === projectId ? { sourceFolders: folders, expectedProjectDigest: target.record_digest } : {}),
        name: row.name, icon: row.icon, color: row.color,
      })),
    });
  }

  async function deleteProject(projectId) {
    const scope = await read("projection.operator.os.scope");
    const target = scope.environments.find((row) => row.ee_id === projectId);
    if (!target || projectId === scope.root_ee_id) throw new Error("Choose an existing Project.");
    return mutate({
      action: "delete_project", operationId: randomUUID(), workspace: workspaceIdentity(scope),
      eeId: projectId, expectedProjectDigest: target.record_digest,
    });
  }

  async function deleteAgent(agentId) {
    const agent = select((await contextData()).agents, agentId, "Agent");
    const scope = await read("projection.operator.os.scope");
    return mutate({
      action: "delete_agent_template", agentId: agent.id, projectEeId: agent.projectEeId,
      workerInterfaceId: agent.workerInterfaceId, expectedClientTemplateDigest: agent.clientTemplateDigest,
      expectedAgentCatalogDigest: scope.agent_catalog_digest,
    });
  }

  async function projectPolicies(parentId) {
    const scope = await read("projection.operator.os.scope");
    const parent = scope.environments.find((row) => row.ee_id === (parentId || scope.root_ee_id));
    if (!parent) throw new Error("Choose an existing parent Project.");
    const { ee_templates: templates } = await read("projection.ee_templates.list");
    return shared.allowedProjectPolicies(templates.map(shared.projectPolicyOption).filter(Boolean), parent.allowed_child_ee_templates);
  }

  async function rename(runId, name) {
    return mutate({ action: "rename_task", runId: (await task(runId)).id, name });
  }

  async function archive(runId, archived = true) {
    const current = await task(runId);
    const scope = await read("projection.operator.os.scope");
    const { expectedHomeSidecarDigest, ...workspace } = workspaceIdentity(scope);
    return mutate({ action: "set_task_archived", home: env.home, workspace, runId: current.id, expectedDigest: current.recordDigest, archived });
  }

  async function deleteTask(runId) {
    const current = await task(runId);
    if (current.isHome) throw new Error("Home cannot be deleted.");
    if (current.turnOpen || await hasPendingStop(current.id)) {
      throw new Error("Stop the task and wait for it to settle before deleting it.");
    }
    return mutate({ action: "delete_task", runId: current.id, expectedDigest: current.recordDigest });
  }

  async function hasPendingStop(runId) {
    const { pendingStop } = require("./transcript.cjs");
    const outcome = (await history(runId)).findLast((entry) => entry.terminal || pendingStop(entry));
    return pendingStop(outcome ?? {});
  }

  async function search(query, runId) {
    const candidates = runId ? [await task(runId)] : (await tasks()).filter((row) => !row.isHome);
    return read("projection.operator.chat.search", { query, task_run_ids: candidates.map((row) => row.id), limit: 30 });
  }

  async function saveAttachment(runId, attachmentId, destination) {
    const messages = await rawHistory(runId);
    const message = exactChoice(messages.filter((row) => row.attachments?.some((file) => file.id === attachmentId)), "attachment message");
    const result = await read("projection.artifact.bytes.read", {
      artifact_id: message.artifact_id, run_id: message.run_id,
      interface_id: message.interface_id, attachment_id: attachmentId,
    });
    const bytes = Buffer.from(result.base64, "base64");
    if (result.attachment_id !== attachmentId || result.content_hash !== attachmentId || bytes.length !== result.size
      || createHash("sha256").update(bytes).digest("hex") !== attachmentId) throw new Error("Attachment integrity verification failed.");
    const file = path.resolve(destination);
    await fs.writeFile(file, bytes, { flag: "wx", mode: 0o600 });
    return { path: file, size: bytes.length, contentHash: attachmentId };
  }

  async function close() {
    closed = true;
    store.close();
    await staging.abortOwner(owner);
  }

  return { env, status, contexts, tasks, task, models, history, rawHistory, send, follow, stop, respond, rename, archive,
    createAgent, deleteAgent, createProject, deleteProject, setProjectFolders, projectPolicies, deleteTask, search, saveAttachment, close, read, catalogue };
}

function workspaceIdentity(scope, agents = false) {
  if (scope.status !== "ready") throw new Error("Complete Octopus setup before changing Projects or Agents.");
  return {
    rootEeId: scope.root_ee_id, homeRunId: scope.home_run_id,
    homeInterfaceId: scope.home_interface_id, expectedHomeSidecarDigest: scope.home_sidecar_digest,
    ...(agents ? { expectedAgentCatalogDigest: scope.agent_catalog_digest } : {}),
  };
}

function presentAgent(agent) {
  return { ...agent, workerId: agent.workerInterfaceId, projectId: agent.projectEeId,
    providerId: agent.providerInterfaceId, model: agent.providerModel, effort: agent.reasoningEffort };
}

function exactChoice(rows, label) {
  if (rows.length === 1) return rows[0];
  throw new Error(rows.length ? `${label} is ambiguous. Use its exact ID.` : `No ${label} is available.`);
}

function select(rows, value, label, nameKey = "name") {
  const article = /^[aeiou]/i.test(label) ? "an" : "a";
  if (typeof value !== "string" || !value.trim()) throw new Error(`Choose ${article} ${label}.`);
  const exact = rows.find((row) => row.id === value);
  return exact || exactChoice(rows.filter((row) => row[nameKey] === value || row.id.startsWith(value)), label);
}

function assertAdmission(verdict) {
  if (verdict?.admitted && Number.isSafeInteger(verdict.admitted.seq)) return;
  throw Object.assign(new Error(verdict?.refused
    ? `Octopus refused the input: ${verdict.refused.reason}.`
    : "Octopus returned no verifiable admission receipt."), { code: "delivery_refused" });
}

module.exports = { createClient, select, assertAdmission };
