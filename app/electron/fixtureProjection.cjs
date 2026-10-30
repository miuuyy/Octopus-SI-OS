const crypto = require("node:crypto");
const DAY_MS = 86_400_000;
const { parseProjectionReadRequest } = require("./projectionReadContract.cjs");
const { operatorActivityRows } = require("./operatorActivity.cjs");
const { createLabCatalog } = require("./labCatalog.cjs");
const { connectionContext, connectionInput, createApplicationConnectionHost } = require("./applicationConnection.cjs");
const fixtureConnectionModule = require("./fixtureConnectionModule.cjs");
const { resolveJsonPointer } = require("./projection/privateConfiguration.cjs");

const FIXTURE_ROOT_EE_ID = "lab_ee_home";
const FIXTURE_PRIMARY_PROJECT_ID = "lab_project_lumen";
const FIXTURE_RESEARCH_PROJECT_ID = "lab_project_research";
const FIXTURE_OPERATIONS_PROJECT_ID = "lab_project_operations";
const FIXTURE_WORKSPACE_ROOT = "/workspace/lumen";
const LAB_CONTEXT_WINDOW = 272_000;
const LAB_RUNTIME_ENDPOINT = "lab://isolated";
const LAB_CONNECTION_APPLICATION_ID = "world_lumen_mail";
const LAB_CHAT_APPLICATIONS = {
  lab_run_2: ["world_mcp_github"],
  lab_run_4: ["world_slack", "world_cron"],
  lab_run_5: ["world_slack"],
};
const LAB_DEFAULT_APPLICATIONS = {
  [FIXTURE_PRIMARY_PROJECT_ID]: ["world_mcp_github"],
  [FIXTURE_OPERATIONS_PROJECT_ID]: ["world_slack", "world_cron"],
};
const LAB_DRIVER_PROJECTS = {
  browser_session: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_RESEARCH_PROJECT_ID],
  process_session: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_OPERATIONS_PROJECT_ID],
  web_search: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_RESEARCH_PROJECT_ID, FIXTURE_OPERATIONS_PROJECT_ID],
};
const LAB_DRIVER_HOST_SLOTS = {
  browser_session: { browser_profile_root: "/workspace/.browser-profile" },
  filesystem_workspace: { workspace_root: FIXTURE_WORKSPACE_ROOT },
  git_worktree: { workspace_root: FIXTURE_WORKSPACE_ROOT },
  process_session: { workspace_root: FIXTURE_WORKSPACE_ROOT },
};

const FIXTURE_CHAT_ACTIVITY = [
  { delay: 320, suffix: "turn-started", kind: "turn_started" },
  { delay: 780, suffix: "reasoning-1", kind: "reasoning", summary: ["**Inspecting the demo workspace**"] },
  { delay: 980, suffix: "plan", kind: "commentary", text: "I'll read the header first, then make one focused change and run its check." },
  { delay: 1_180, suffix: "list", kind: "tool", tool: "coder.list_dir", args: { path: "src" } },
  { delay: 1_720, suffix: "list", kind: "tool", tool: "coder.list_dir", args: { path: "src" }, body: { entries: ["components", "styles"] } },
  { delay: 2_160, suffix: "read", kind: "tool", tool: "coder.read_file", args: { path: "src/components/ProjectHeader.tsx" } },
  { delay: 2_760, suffix: "read", kind: "tool", tool: "coder.read_file", args: { path: "src/components/ProjectHeader.tsx" }, body: { bytes: 1842 } },
  { delay: 3_240, suffix: "reasoning-2", kind: "reasoning", summary: ["**Preparing a focused change**"] },
  { delay: 3_760, suffix: "change", kind: "file_change", changes: [{ added: 12, deleted: 3, kind: "update", path: "src/components/ProjectHeader.tsx" }] },
  { delay: 4_280, suffix: "command", kind: "tool", tool: "coder.build", args: { command: "npm test -- ProjectHeader" } },
  { delay: 5_620, suffix: "command", kind: "tool", tool: "coder.build", args: { command: "npm test -- ProjectHeader" }, body: { output: "1 test passed" } },
];

function freshenScheduledChats(chats) {
  const now = Date.now();
  for (const chat of chats) {
    if (!chat.next_scheduled_at_ms) continue;
    while (chat.next_scheduled_at_ms <= now) chat.next_scheduled_at_ms += DAY_MS;
  }
  return chats;
}

function createFixtureProjectionStore(options = {}) {
  const data = buildFixtureProjection();
  const catalog = createLabCatalog(options);
  const openedAtMs = Date.now();
  let appliedCatalog = null;

  async function withCatalog() {
    const lab = await catalog.load();
    if (appliedCatalog !== lab) {
      data.providers = lab.providers;
      data.catalogErrors = [...lab.errors];
      data.applicationSources = new Map([...lab.applications, labFixtureConnectionApplication()].map((entry) => [entry.manifest.id, entry]));
      data.privateValues = labSeedPrivateValues(data.applicationSources, data.catalogErrors);
      data.applications = labApplicationRows([...data.applicationSources.values()], data);
      data.drivers = labDriverRows(lab.drivers, data);
      const preferred = lab.providers[0];
      for (const chat of data.chats) {
        const provider = lab.providers.find((candidate) => candidate.id === chat.provider_interface_id) ?? preferred;
        if (provider && !provider.models.some((model) => model.model === chat.provider_model)) chat.provider_model = provider.model;
      }
      appliedCatalog = lab;
    }
    return lab;
  }
  const onChange = typeof options.onChange === "function" ? options.onChange : () => {};
  let generation = 1;
  const pendingChatReplies = new Map();

  async function read(request) {
    const { method, params } = parseProjectionReadRequest(request);
    switch (method) {
      case "projection.runtime.snapshot":
        return { projection: data.projection };
      case "projection.runtime.topology": {
        const includeLedger = params.include_ledger !== false;
        const runs = limitRows(data.runs, { limit: 1_000 });
        const focused = new Set(Array.isArray(params.focused_run_ids) ? params.focused_run_ids.map(String) : []);
        for (const run of runs.slice(0, 12)) focused.add(String(run.run_id));
        const runDetailsRaw = Object.fromEntries([...focused].flatMap((runId) => {
          const detail = data.runDetails[runId];
          return detail ? [[runId, {
            detail,
            trace: includeLedger
              ? { events: limitRows(data.envelopes.filter((row) => row.run_id === runId), { limit: 80 }) }
              : { envelopes: [] },
          }]] : [];
        }));
        return {
          generation,
          snapshotRaw: { projection: data.projection },
          eeRaw: { ee_instances: limitRows(data.eeInstances, { limit: 300 }) },
          runsRaw: { runs, invalid_run_records: [] },
          runCheckpointsRaw: {
            run_checkpoints: runs.map((run) => ({
              run_id: run.run_id,
              checkpoints: Array.isArray(data.runDetails[run.run_id]?.checkpoints)
                ? data.runDetails[run.run_id].checkpoints
                : [],
            })),
          },
          interfacesRaw: { interfaces: data.interfaces },
          bindingsRaw: { bindings: data.bindings },
          artifactsRaw: { artifacts: includeLedger ? limitRows(data.artifacts, { limit: 300 }) : [] },
          marksRaw: { marks: includeLedger ? limitRows(data.marks, { limit: 300 }) : [] },
          workerEnvironmentsRaw: { worker_environments: limitRows(data.workerEnvironments, { limit: 1_000 }) },
          movementsRaw: includeLedger
            ? { movements: limitRows(data.movements, { limit: 1_000 }), total: data.movements.length }
            : { movements: [], total: 0 },
          runDetailsRaw,
        };
      }
      case "projection.ee_instances.list":
        return { ee_instances: limitRows(data.eeInstances, params) };
      case "projection.runs.list":
        return { runs: limitRows(data.runs, params) };
      case "projection.runs.checkpoints.list":
        return {
          run_checkpoints: limitRows(data.runs, params).map((run) => ({
            run_id: run.run_id,
            checkpoints: Array.isArray(data.runDetails[run.run_id]?.checkpoints)
              ? data.runDetails[run.run_id].checkpoints
              : [],
          })),
        };
      case "projection.runs.get": {
        const runId = requiredString(params.run_id, "run_id");
        const detail = data.runDetails[runId];
        if (!detail) throw new Error(`lab run does not exist: ${runId}`);
        return detail;
      }
      case "projection.trace.recent": {
        const runId = requiredString(params.run_id, "run_id");
        return { events: limitRows(data.envelopes.filter((row) => row.run_id === runId), params) };
      }
      case "projection.interfaces.list":
        return { interfaces: limitRows(filterString(data.interfaces, "interface_type", params.interface_type), params) };
      case "projection.interfaces.artifact_transfer.targets":
        return { artifact_transfer_targets: limitRows(filterTargets(data.transferTargets, params), params) };
      case "projection.bindings.list":
        return { bindings: limitRows(data.bindings, params) };
      case "projection.artifacts.list": {
        const source = typeof params.source === "string" ? params.source.trim() : "";
        if (source && !["ledger", "registry"].includes(source)) {
          throw new Error("Test artifact list source must be ledger or registry");
        }
        if (params.previewable_only !== undefined && typeof params.previewable_only !== "boolean") {
          throw new Error("Test artifact list previewable_only must be a boolean");
        }
        const limit = params.limit === undefined ? 1_000 : Number(params.limit);
        if (!Number.isSafeInteger(limit) || limit < 1 || limit > 5_000) {
          throw new Error("Test artifact list limit must be an integer between 1 and 5000");
        }
        const matching = source === "registry" ? [] : filterString(data.artifacts, "label", params.label)
          .map((row) => ({
            ...row,
            previewable: false,
            source: "ledger",
          }));
        const selected = params.previewable_only === true ? [] : matching;
        const artifacts = selected.slice(0, limit);
        return {
          artifacts,
          total: matching.length,
          previewable_total: 0,
          unavailable_total: matching.length,
          returned: artifacts.length,
          limit,
          truncated: selected.length > artifacts.length,
        };
      }
      case "projection.marks.list":
        return { marks: limitRows(data.marks, params) };
      case "projection.worker_environments.list":
        return { worker_environments: limitRows(data.workerEnvironments, params) };
      case "projection.worker_environment_pools.list":
        return { worker_environment_pools: limitRows(data.workerEnvironmentPools, params) };
      case "projection.envelopes.list":
        return { envelopes: limitRows(filterString(data.envelopes, "kind", params.kind), params) };
      case "projection.interfaces.pending.list":
        return { pending_artifacts: limitRows(data.pendingArtifacts, params) };
      case "projection.goals.snapshot":
        return { goals: limitRows(data.goals, params) };
      case "projection.ee_templates.list":
        return { ee_templates: limitRows(data.eeTemplates, params) };
      case "projection.workers.list":
        return { workers: limitRows(data.workers, params) };
      case "projection.tools.list":
        return { tools: limitRows(data.tools, params) };
      case "projection.verifiers.list":
        return { verifiers: limitRows(data.verifiers, params) };
      case "projection.policy.ee.list":
        return { policy: limitRows(data.policy, params) };
      case "projection.descriptors.inventory.list":
        return { descriptors: data.descriptorInventory };
      case "projection.descriptors.kinds.list":
        return { descriptors: data.descriptorKinds };
      case "projection.descriptors.artifact_labels.list":
        return { descriptors: limitRows(data.artifactLabels, params) };
      case "projection.descriptors.mark_kinds.list":
        return { descriptors: limitRows(data.markKinds, params) };
      case "projection.descriptors.tool.list":
        return { descriptors: limitRows(data.tools, params) };
      case "projection.descriptors.worker_environment_kind.list":
        return { descriptors: limitRows(data.workerEnvironmentKinds, params) };
      case "projection.descriptors.ee.list":
        return { descriptors: limitRows(data.eeTemplates, params) };
      case "projection.operator.chats.list": {
        await withCatalog();
        const chats = limitRows(freshenScheduledChats(data.chats), params);
        if (params.include_activity !== true) return { chats };
        const contexts = fixtureChatExecutionContexts(data);
        const activity = [];
        const presented = chats.map((chat) => {
          const worker = contexts.find((context) => context.ee_id === chat.ee_id)?.workers
            .find((worker) => worker.id === chat.worker_choice_interface_id);
          const agent = worker?.client_agent_id ? { id: worker.client_agent_id, name: worker.client_agent_name, icon: worker.client_agent_icon } : null;
          const color = agent ? data.projectPresentations.find((presentation) => presentation.ee_id === chat.ee_id)?.color || null : null;
          const messages = data.chatMessages[chat.run_id] ?? [];
          const deliveries = messages.filter((message) => message.event_kind === "message" && message.terminal === true
            && message.error !== true && message.direction === "out" && message.interface_id === chat.chat_interface_id)
            .map((message) => ({ artifact_id: message.artifact_id, seq: message.movement_seq, movement_id: `fixture:${message.movement_seq}` }));
          const responses = messages.filter((message) => message.event_data?.resolved === true && message.event_data?.request_id)
            .map((message) => ({ key: `${message.event_kind}:${message.event_data.request_id}`, seq: message.movement_seq + 1 }));
          activity.push(...operatorActivityRows({ chat: { ...chat, agent_color: color }, messages, deliveries, responses, agent }).map(fixtureRow));
          return { ...chat, agent_id: agent?.id ?? null, agent_name: agent?.name ?? null, agent_icon: agent?.icon ?? null, agent_color: color };
        });
        return { chats: presented, activity };
      }
      case "projection.operator.chats.status": {
        const runIds = new Set(Array.isArray(params.run_ids) ? params.run_ids.map(String) : []);
        return {
          chats: freshenScheduledChats(data.chats)
            .filter((chat) => runIds.has(String(chat.run_id)))
            .map((chat) => ({
              run_id: chat.run_id,
              turn_open: chat.turn_open === true,
              turn_state: chat.turn_state,
              updated_at_ms: chat.updated_at_ms,
              last_movement_seq: chat.last_movement_seq,
            })),
          ledger_cursor_seq: data.chats.reduce(
            (latest, chat) => Math.max(latest, Number(chat.last_movement_seq) || 0),
            0,
          ),
        };
      }
      case "projection.operator.os.scope": {
        const home = data.chats.find((chat) => chat.client_surface_role === "home") ?? null;
        const rootEnvironment = data.eeInstances.find((environment) => environment.ee_id === FIXTURE_ROOT_EE_ID) ?? null;
        const projects = data.eeInstances.filter((environment) => environment.parent_ee_id === FIXTURE_ROOT_EE_ID);
        const profiles = fixtureTaskAuthoringProfiles(data.eeInstances);
        return {
          status: home ? "ready" : "awaiting_home",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          home_run_id: String(home?.run_id || ""),
          home_interface_id: String(home?.chat_interface_id || ""),
          space_name: "Lab",
          home_sidecar_digest: fixtureDigest("lab-home-sidecar"),
          agent_catalog_path: "lab://client/agents/catalog.json",
          agent_catalog_digest: "lab-agent-catalog",
          root_environment: rootEnvironment,
          projects,
          environments: data.eeInstances,
          ee_templates: data.eeTemplates,
          project_presentations: data.projectPresentations,
          task_authoring_profiles: profiles,
          client_interface_ids: data.chats.map((chat) => chat.chat_interface_id),
          client_runs: data.chats.map((chat) => ({
            run_id: chat.run_id,
            ee_id: chat.ee_id,
            record_path: `lab://state/runs/${chat.run_id}.json`,
            record_digest: chat.record_digest,
            interface_id: chat.chat_interface_id,
            surface_role: chat.client_surface_role,
            archived: chat.archived === true,
            provider_choice_interface: chat.provider_interface_id,
            worker_choice_interface: chat.worker_choice_interface_id,
            authoring_profile: profiles.find((profile) => profile.ee_id === chat.ee_id) ?? null,
          })),
          read_only_runs: [],
          errors: [],
        };
      }
      case "projection.operator.os.workspace_setup":
        return { status: "not_configured", receipt: null, errors: [] };
      case "projection.operator.os.local_packages":
        return (await withCatalog()).localPackages;
      case "projection.operator.os.applications":
        await withCatalog();
        return {
          status: "ready",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          applications: data.applications,
          projects: data.eeInstances.map(fixtureApplicationProject),
          errors: data.catalogErrors,
        };
      case "projection.operator.os.drivers":
        await withCatalog();
        return {
          status: "ready",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          host_root_refs: data.hostRootRefs,
          full_access_root: "/",
          source_folders: [FIXTURE_WORKSPACE_ROOT],
          host_manifest_ready: true,
          drivers: data.drivers,
          projects: data.eeInstances.map(fixtureApplicationProject),
          errors: [],
        };
      case "projection.operator.run.activity": {
        const runId = requiredString(params.run_id, "run_id");
        return fixtureRunActivityPage(runId, data.chatMessages[runId] || [], params);
      }
      case "projection.operator.chat.contexts":
        await withCatalog();
        return {
          contexts: fixtureChatExecutionContexts(data),
          application_catalog: data.applications.map((application) => ({
            id: application.id,
            name: application.name,
            description: application.description,
            category: application.category,
            icon: application.icon,
            logo_data_url: application.logo_data_url,
            logo_dark_data_url: application.logo_dark_data_url,
            interfaces: application.interfaces,
            artifact_bindings: application.artifact_bindings,
            task_capabilities: application.task_capabilities ?? {},
          })),
        };
      case "projection.operator.chat.messages":
        return { messages: data.chatMessages[requiredString(params.run_id, "run_id")] || [] };
      case "projection.operator.providers.list":
        return { providers: (await withCatalog()).providers };
      case "projection.operator.providers.status":
        return { providers: fixtureProviderStatus(data.chats) };
      case "projection.movements.list":
        return fixtureMovementPage(data.movements, params);
      case "projection.movements.payload":
        return { resolved: false, blob_path: String(params.blob_path || ""), size: 0, text: "", truncated: false };
      case "projection.client_surfaces.list":
        return { generation, root: "artifact://lab", documents: [], errors: [] };
      case "projection.client_actions.list":
        return { home: params.home, run_id: params.run_id, interface_id: params.interface_id, actions: [] };
      case "projection.interface_artifacts.list":
        return { artifacts: [] };
      case "projection.scheduled_interfaces.list":
        return {
          scheduled_interfaces: data.scheduledInterfaces,
          scope_options: data.eeInstances.map((environment) => ({
            ee_id: environment.ee_id,
            name: data.projectPresentations.find((presentation) => presentation.ee_id === environment.ee_id)?.name
              || (environment.ee_id === FIXTURE_ROOT_EE_ID ? "Home" : environment.goal),
          })),
        };
      case "projection.operator.chat.search":
        return fixtureChatSearch(data, params);
      case "projection.artifact.bytes.read":
        return labArtifactBytes(data, params);
      case "projection.interface_media.read":
        throw new Error("Test voice media is synthetic and keeps no recording.");
      default:
        throw new Error(`lab projection method is not implemented: ${method}`);
    }
  }

  async function submitChatTurn(request) {
    const text = requiredString(request?.text, "text");
    const providerInterfaceId = requiredString(request?.providerInterfaceId, "providerInterfaceId");
    if (!(await withCatalog()).providers.some((provider) => provider.id === providerInterfaceId)) {
      throw new Error(`Test provider does not exist: ${providerInterfaceId}`);
    }
    const now = Date.now();
    const runId = typeof request?.runId === "string" && request.runId.trim()
      ? request.runId.trim()
      : `lab_run_${now.toString(36)}`;
    let chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) {
      const chatInterfaceId = `lab_world_operator_chat_${runId}`;
      chat = fixtureRow({
        run_id: runId,
        title: text.slice(0, 72),
        chat_interface_id: chatInterfaceId,
        client_surface_role: request?.clientSurfaceRole === "home" ? "home" : "task",
        writable: true,
        os_root_ee_id: FIXTURE_ROOT_EE_ID,
        turn_open: false,
        ee_id: String(request?.eeId || data.eeInstances[0]?.ee_id || ""),
        ee_goal: String(data.eeInstances.find((environment) => environment.ee_id === request?.eeId)?.goal || ""),
        ee_template_name: String(data.eeInstances.find((environment) => environment.ee_id === request?.eeId)?.template_name || ""),
        worker_interface_id: String(request?.workerInterfaceId || data.workers[0]?.worker || ""),
        worker_choice_interface_id: String(request?.workerInterfaceId || data.workers[0]?.worker || ""),
        door_host_slots: request?.doorHostSlots || {},
        provider_interface_id: providerInterfaceId,
        provider_bound_interface_id: `${providerInterfaceId}_${runId}`,
        provider_model: String(request?.providerModel || data.providers?.[0]?.model || ""),
        provider_models_refreshable: false,
        reasoning_effort: String(request?.reasoningEffort || "medium"),
        created_at_ms: now,
        updated_at_ms: now,
        message_count: 0,
        pinned_artifact_ids: [],
        continued_from: request?.continuation || null,
      });
      data.chats.unshift(chat);
      data.chatMessages[runId] = [];
    }
    if (chat.turn_open === true || pendingChatReplies.has(runId)) {
      throw new Error(`Test run '${runId}' already has an in-flight reply; queue or steer the message instead.`);
    }
    const messages = data.chatMessages[runId];
    const seq = nextFixtureChatSeq(data);
    const messageId = String(request?.messageId || `lab_message_${now.toString(36)}`);
    const events = labChatEvents(chat);
    const turnId = events.turnId(messageId);
    messages.push(events.user({ id: messageId, text, createdAtMs: now, seq, attachments: fixtureChatAttachments(request?.attachments) }));
    chat.turn_open = true;
    chat.updated_at_ms = now;
    chat.message_count = messages.length;
    generation += 1;
    onChange(generation);
    const scheduleActivity = (activity) => {
      const timer = setTimeout(() => {
        if (!pendingChatReplies.has(runId)) return;
        const createdAtMs = Date.now();
        const id = `${messageId}-${activity.suffix}`;
        const base = { id, turnId, createdAtMs, seq: nextFixtureChatSeq(data) };
        messages.push(activity.kind === "turn_started"
          ? events.turnStarted(base)
          : activity.kind === "reasoning"
            ? events.reasoning({ ...base, summary: activity.summary })
            : activity.kind === "commentary"
              ? events.commentary({ ...base, text: activity.text })
              : activity.kind === "file_change"
                ? events.fileChange({ ...base, changes: activity.changes })
                : events.tool({ ...base, tool: activity.tool, args: activity.args, body: activity.body }));
        chat.updated_at_ms = createdAtMs;
        chat.message_count = messages.length;
        generation += 1;
        onChange(generation);
      }, activity.delay);
      timer.unref?.();
    };
    for (const activity of FIXTURE_CHAT_ACTIVITY) scheduleActivity(activity);
    const replyTimer = setTimeout(() => {
      pendingChatReplies.delete(runId);
      const steers = messages.filter((message) => message.event_kind === "steering" && message.movement_seq > seq);
      const replyAt = Date.now();
      const finalText = steers.length
        ? `I incorporated ${steers.length} in-flight ${steers.length === 1 ? "message" : "messages"} and completed the demo change.`
        : "Done. I inspected the demo workspace, updated `src/components/ProjectHeader.tsx`, and the focused check passes.";
      const previous = messages.findLast((message) => message.event_data?.context_window_usage);
      const contextTokens = (Number(previous?.event_data?.context_window_usage?.used_tokens) || 18_000) + 7_400;
      messages.push(events.finalAnswer({ id: `${messageId}-final`, turnId, text: finalText, createdAtMs: replyAt, seq: nextFixtureChatSeq(data), contextTokens, latencyMs: replyAt - now }));
      messages.push(events.turnCompleted({ id: `${messageId}-turn-completed`, turnId, startedAtMs: now, finalId: `${messageId}-final`, finalText, createdAtMs: replyAt + 1, seq: nextFixtureChatSeq(data) }));
      chat.turn_open = false;
      chat.updated_at_ms = replyAt;
      chat.message_count = messages.length;
      generation += 1;
      onChange(generation);
    }, 6_520);
    replyTimer.unref?.();
    pendingChatReplies.set(runId, replyTimer);
    return {
      runId,
      chatInterfaceId: chat.chat_interface_id,
      modelInterfaceId: providerInterfaceId,
      workerInterfaceId: chat.worker_interface_id,
      clientSurfaceRole: chat.client_surface_role,
      label: "operator.message",
      messageId,
      payloadPath: `lab://artifacts/${messageId}`,
      contentHash: `lab${messageId}`,
      attachmentCount: Array.isArray(request?.attachments) ? request.attachments.length : 0,
      clientWrites: [],
      messageRecovered: false,
      messageVerdict: { admitted: { seq } },
      outboundCursor: { offset: 0, seq: null },
    };
  }

  async function submitInterfaceArtifact(request) {
    const runId = requiredString(request?.runId, "runId");
    const interfaceId = requiredString(request?.interfaceId, "interfaceId");
    const label = requiredString(request?.label, "label");
    const kind = { "operator.approval.response": "approval", "operator.user_input.response": "user_input", "operator.elicitation.response": "elicitation" }[label];
    if (!kind) throw new Error(`The Test profile answers operator requests only; '${label}' is not accepted.`);
    const chat = data.chats.find((item) => item.run_id === runId && item.chat_interface_id === interfaceId);
    if (!chat) throw new Error(`Test run '${runId}' has no chat interface '${interfaceId}'.`);
    const response = request?.payload?.[`operator_${kind}_response`];
    const requestId = typeof response?.request_id === "string" ? response.request_id : "";
    const messages = data.chatMessages[runId] || [];
    const pending = messages.find((message) => message.event_kind === kind && message.event_data?.request_id === requestId && message.event_data?.resolved !== true);
    if (!pending) throw new Error(`Test run '${runId}' has no pending ${kind} request '${requestId}'.`);
    const now = Date.now();
    const turnId = String(pending.event_data.turn_id || "");
    const declined = kind === "approval" && response.decision === "decline";
    pending.event_data = { ...pending.event_data, resolved: true, response };
    const events = labChatEvents(chat);
    const finalText = kind === "approval"
      ? declined ? "Understood. I left the tests unrun, and the change is ready for your own check." : "`npm test -- ProjectHeader` passes. The connection badge is ready."
      : "Thanks. I'll compare the set you picked and come back with the summary.";
    messages.push(events.resolved({ id: `${requestId}-resolved`, turnId, requestId, text: declined ? "Declined" : "Answered", createdAtMs: now, seq: nextFixtureChatSeq(data) }));
    const previous = messages.findLast((message) => message.event_data?.context_window_usage);
    const contextTokens = (Number(previous?.event_data?.context_window_usage?.used_tokens) || 18_000) + 3_200;
    messages.push(events.finalAnswer({ id: `${requestId}-final`, turnId, text: finalText, createdAtMs: now + 1, seq: nextFixtureChatSeq(data), contextTokens, latencyMs: now - pending.created_at_ms }));
    messages.push(events.turnCompleted({ id: `${requestId}-turn-completed`, turnId, startedAtMs: pending.created_at_ms, finalId: `${requestId}-final`, finalText, createdAtMs: now + 2, seq: nextFixtureChatSeq(data) }));
    chat.turn_open = false;
    chat.turn_state = "idle";
    chat.updated_at_ms = now;
    chat.message_count = messages.length;
    generation += 1;
    onChange(generation);
    return { interfaceId, runId, label, contentHash: `lab${requestId}response`, verdict: { admitted: { seq: pending.movement_seq + 1 } } };
  }

  async function submitChatSteer(request) {
    const runId = requiredString(request?.runId, "runId");
    const text = typeof request?.text === "string" ? request.text.trim() : "";
    const attachments = fixtureChatAttachments(request?.attachments);
    if (!text && attachments.length === 0) throw new Error("Test steering message must carry text or an attachment.");
    const chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) throw new Error(`Test chat does not exist: ${runId}`);
    if (chat.turn_open !== true || !pendingChatReplies.has(runId)) {
      throw new Error(`Test run '${runId}' has no in-flight reply to steer.`);
    }
    const interfaceId = chat.chat_interface_id;
    const label = "operator.message";
    const messageId = String(request?.messageId || `lab_steer_${Date.now().toString(36)}`);
    const seq = nextFixtureChatSeq(data);
    const createdAtMs = Date.now();
    data.chatMessages[runId].push(fixtureRow({
      id: messageId,
      run_id: runId,
      role: "user",
      label,
      direction: "in",
      event_kind: "steering",
      event_data: { text, message_id: messageId, steering_status: "accepted" },
      text,
      created_at_ms: createdAtMs,
      movement_seq: seq,
      artifact_id: messageId,
      content_hash: `lab${messageId}`,
      interface_id: interfaceId,
      attachments,
      error: false,
      terminal: true,
    }));
    chat.updated_at_ms = createdAtMs;
    chat.message_count = data.chatMessages[runId].length;
    generation += 1;
    onChange(generation);
    return {
      interfaceId,
      runId,
      label,
      messageId,
      attachmentCount: attachments.length,
      contentHash: `lab${messageId}`,
      messageRecovered: false,
      verdict: { admitted: { seq } },
    };
  }

  async function updateChatPins(request) {
    const runId = requiredString(request?.runId, "runId");
    const artifactId = requiredString(request?.artifactId, "artifactId");
    if (typeof request?.pinned !== "boolean") throw new Error("pinned must be a boolean");
    const chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) throw new Error(`Test chat does not exist: ${runId}`);
    const messages = data.chatMessages[runId] || [];
    const source = messages.find((message) => message.artifact_id === artifactId);
    const currentItems = Array.isArray(chat.pinned_context_items) ? chat.pinned_context_items : [];
    if (!source && !currentItems.some((item) => item.artifact_id === artifactId)) {
      throw new Error(`Test chat artifact does not exist: ${artifactId}`);
    }
    const current = Array.isArray(chat.pinned_artifact_ids) ? chat.pinned_artifact_ids : [];
    const pinnedId = request.pinned && request.excerpt
      ? `operator.pinned_excerpt:lab-${artifactId}-${request.excerpt.start}-${request.excerpt.end}`
      : artifactId;
    chat.pinned_artifact_ids = request.pinned
      ? Array.from(new Set([...current, pinnedId]))
      : current.filter((id) => id !== artifactId);
    chat.pinned_context_items = request.pinned && request.excerpt
      ? [...currentItems, {
          artifact_id: pinnedId,
          source_artifact_id: artifactId,
          source_message_id: request.excerpt.sourceMessageId,
          text: request.excerpt.text,
          start: request.excerpt.start,
          end: request.excerpt.end,
          role: request.excerpt.role,
        }]
      : currentItems.filter((item) => item.artifact_id !== artifactId);
    generation += 1;
    onChange(generation);
    return {
      runId,
      pinnedArtifactIds: chat.pinned_artifact_ids,
      pinnedContextItems: chat.pinned_context_items.map((item) => ({
        artifactId: item.artifact_id,
        sourceArtifactId: item.source_artifact_id,
        sourceMessageId: item.source_message_id,
        text: item.text,
        start: item.start,
        end: item.end,
        role: item.role,
      })),
      clientWrites: [],
    };
  }

  function mutateScheduledInterface(request) {
    const action = requiredString(request?.action, "action");
    const rows = data.scheduledInterfaces;
    const find = (interfaceId) => {
      const row = rows.find((candidate) => candidate.interface_id === interfaceId);
      if (!row) throw new Error(`Test trigger does not exist: ${interfaceId}`);
      return row;
    };
    const current = (row, digest) => {
      if (row.activation_digest !== digest) throw new Error("This trigger changed since it was opened. Reopen it and try again.");
    };
    const fromDraft = (row, draft) => {
      const schedule = draft.type === "interval"
        ? { type: "interval", interval_seconds: Number(draft.intervalSeconds) }
        : draft.type === "once"
          ? { type: "once", due_at: String(draft.dueAt) }
          : draft.type === "inactivity"
            ? { type: "inactivity", seconds: Number(draft.inactivity?.seconds), interface_match: draft.inactivity?.interface_match ?? {}, exclude_interface_match: draft.inactivity?.exclude_interface_match ?? null, ...(draft.inactivity?.within_ee ? { within_ee: draft.inactivity.within_ee } : {}) }
            : { type: "cron", cron: String(draft.cron) };
      return labScheduledRow({
        ...row,
        status: row.status === "template" ? "enabled" : row.status,
        template: false,
        schedule,
        timezone: String(draft.timezone || "UTC"),
        fire_on_start: draft.fireOnStart === true,
        instructions: String(draft.instructions ?? ""),
        next_run_at_ms: Date.now() + DAY_MS,
        next_run_source: "schedule",
      });
    };
    let outcome;
    if (action === "create") {
      const source = find(requiredString(request.sourceInterfaceId, "sourceInterfaceId"));
      current(source, request.sourceActivationDigest);
      const interfaceId = requiredString(request.interfaceId, "interfaceId");
      if (rows.some((row) => row.interface_id === interfaceId)) throw new Error(`Test trigger already exists: ${interfaceId}`);
      const created = fromDraft({ ...source, interface_id: interfaceId, display_name: interfaceId.replace(/^lab_world_|^world_/, "").replace(/_/g, " "), deliveries: [] }, request.draft ?? {});
      rows.push(created);
      outcome = { action, interfaceId, activationDigest: created.activation_digest };
    } else if (action === "update") {
      const row = find(requiredString(request.interfaceId, "interfaceId"));
      current(row, request.expectedDigest);
      const updated = fromDraft(row, request.draft ?? {});
      rows.splice(rows.indexOf(row), 1, updated);
      outcome = { action, interfaceId: row.interface_id, activationDigest: updated.activation_digest };
    } else if (action === "set_enabled") {
      const row = find(requiredString(request.interfaceId, "interfaceId"));
      current(row, request.expectedDigest);
      const updated = labScheduledRow({ ...row, status: request.enabled === true ? "enabled" : "disabled", next_run_at_ms: Date.now() + DAY_MS, next_run_source: "schedule" });
      rows.splice(rows.indexOf(row), 1, updated);
      outcome = { action, interfaceId: row.interface_id, activationDigest: updated.activation_digest };
    } else if (action === "delete") {
      const row = find(requiredString(request.interfaceId, "interfaceId"));
      current(row, request.expectedDigest);
      rows.splice(rows.indexOf(row), 1);
      outcome = { action, interfaceId: row.interface_id, deleted: true };
    } else if (action === "undo_changes") {
      const runId = requiredString(request.runId, "runId");
      const delivery = rows.flatMap((row) => row.deliveries).find((candidate) => candidate.run_id === runId && candidate.journal);
      if (!delivery || delivery.journal.digest !== request.expectedJournalDigest) throw new Error("This change journal changed since it was opened.");
      delivery.journal = {
        ...delivery.journal,
        undone: true,
        undoable: false,
        changes: delivery.journal.changes.map((change) => change.kind === "file" ? { ...change, status: "undone" } : change),
      };
      outcome = { action, runId, restored: delivery.journal.changes.flatMap((change) => change.kind === "file" ? [change.path] : []) };
    } else {
      throw new Error(`Unsupported Test trigger action: ${action}`);
    }
    generation += 1;
    onChange(generation);
    return outcome;
  }

  function labApplicationSource(applicationId, interfaceId) {
    const source = data.applicationSources?.get(applicationId);
    const row = data.applications.find((candidate) => candidate.id === applicationId);
    if (!source || row?.installed !== true) throw new Error(`Test application '${applicationId}' is not installed.`);
    if (interfaceId !== null && !source.manifest.interfaces.includes(interfaceId)) {
      throw new Error(`Application '${applicationId}' does not include interface '${interfaceId}'.`);
    }
    return { source, row };
  }

  function labWritePrivateValues(applicationId, interfaceId, names, changes) {
    const { source, row } = labApplicationSource(applicationId, interfaceId);
    const current = { ...(data.privateValues[interfaceId] ?? {}) };
    for (const [name, value] of Object.entries(changes ?? {})) {
      if (!names.has(name)) throw new Error(`interface '${interfaceId}' does not declare private value '${name}'`);
      if (value === null) delete current[name];
      else if (typeof value === "string" && value.length > 0 && !value.includes("\0")) current[name] = value;
      else throw new Error(`interface '${interfaceId}' private value '${name}' must be a non-empty string or null`);
    }
    data.privateValues[interfaceId] = current;
    row.configuration = labApplicationConfiguration(source.manifest, source.manifest.descriptors, true, data.privateValues);
    return new Map(Object.entries(current));
  }

  const labConnections = createApplicationConnectionHost({
    resolveTargets: async (applicationId, interfaceId) => {
      await withCatalog();
      const { source, row } = labApplicationSource(applicationId, interfaceId);
      const interfaceIds = interfaceId === null ? row.connection_interfaces : [interfaceId];
      return interfaceIds.map((id) => {
        if (!row.connection_interfaces.includes(id)) throw new Error(`Interface '${id}' declares no client connection module.`);
        const names = new Set(labSetupFields(source.manifest, source.manifest.descriptors, id).map(({ name }) => name));
        return {
          interfaceId: id,
          module: fixtureConnectionModule,
          source: "Test connection module",
          createContext: async ({ kind, input, signal, progress }) => connectionContext({
            descriptor: source.manifest.descriptors[id],
            interfaceId: id,
            values: data.privateValues[id] ?? {},
            input: kind === "connect" ? connectionInput(input, names, id) : {},
            privateDirectory: `/lab/interfaces/${id}`,
            signal,
            progress,
            saveValues: async (changes) => labWritePrivateValues(applicationId, id, names, changes),
            launchEnvironment: (name) => { throw new Error(`The Test profile has no runtime launch environment for '${String(name)}'.`); },
            openExternal: async () => { throw new Error("The Test profile does not open external links."); },
          }),
        };
      });
    },
  });

  async function mutateApplicationPrivateConfiguration(request) {
    await withCatalog();
    const applicationId = requiredString(request?.applicationId, "applicationId");
    const interfaceId = requiredString(request?.interfaceId, "interfaceId");
    const { source } = labApplicationSource(applicationId, interfaceId);
    const names = new Set(labSetupFields(source.manifest, source.manifest.descriptors, interfaceId).map(({ name }) => name));
    if ((labPrivateValuesDigest(data.privateValues[interfaceId]) || null) !== (request?.expectedDigest ?? null)) {
      throw new Error(`application private configuration '${interfaceId}' changed before the edit could be committed`);
    }
    const updates = Object.entries(request?.updates ?? {});
    const clears = Array.isArray(request?.clears) ? request.clears : [];
    if (!updates.length && !clears.length) throw new Error("application private configuration mutation contains no changes");
    const values = labWritePrivateValues(applicationId, interfaceId, names, {
      ...Object.fromEntries(clears.map((name) => [name, null])),
      ...Object.fromEntries(updates),
    });
    const digest = labPrivateValuesDigest(data.privateValues[interfaceId]);
    return { interfaceId, documentStatus: digest ? "valid" : "absent", documentDigest: digest || null, configuredFields: values.size };
  }

  function runtimeHostState() {
    return {
      phase: "ready",
      ready: true,
      ownership: "none",
      pid: null,
      home: "",
      sourceRegistry: "",
      runtimeBinary: "",
      repoRoot: "",
      launchCwd: "",
      launchCommand: "",
      launchEnv: {},
      endpoint: LAB_RUNTIME_ENDPOINT,
      error: null,
      updatedAtMs: openedAtMs,
    };
  }

  return {
    read,
    runtimeHostState,
    interfaceDescriptor: (interfaceId) => {
      const row = data.interfaces.find((candidate) => candidate.rule_id === interfaceId);
      if (!row) return null;
      const { projection_source: _source, artifact_transfer_protocol_status: _status, artifact_transfer_target_count: _count, artifact_transfer_ready_targets: _ready, artifact_transfer_target_reason: _reason, ...descriptor } = row;
      return descriptor;
    },
    workspaceFiles: () => Object.fromEntries(fixtureWorkspaceEntries().filter((entry) => entry.kind === "file").map((entry) => [entry.path, entry.text])),
    mutateScheduledInterface,
    mutateApplicationPrivateConfiguration,
    applicationConnectionProbe: (request) => labConnections.probe(request),
    applicationConnectionConnect: (request, options) => labConnections.connect(request, options),
    applicationConnectionCancel: (request) => labConnections.cancel(request),
    applicationConnectionDisconnect: (request) => labConnections.disconnect(request),
    submitChatTurn,
    submitChatSteer,
    submitInterfaceArtifact,
    updateChatPins,
    generation: () => generation,
    invalidate: () => {},
    ensureWatchers: () => {},
  };
}

function nextFixtureChatSeq(data) {
  let maximum = 0;
  for (const rows of Object.values(data.chatMessages)) {
    for (const row of rows) maximum = Math.max(maximum, Number(row.movement_seq) || 0);
  }
  return maximum + 1;
}

function fixtureChatAttachments(value) {
  return (Array.isArray(value) ? value : []).map((input, index) => ({
    id: String(input?.id || `${String(input?.name || "attachment")}:${index}`),
    name: String(input?.name || `Attachment ${index + 1}`),
    size: Number(input?.size) || Number(input?.bytes?.byteLength) || 0,
    type: String(input?.type || "application/octet-stream"),
    label: String(input?.label || "operator.file"),
    last_modified: Number(input?.lastModified) || 0,
  }));
}

function buildFixtureProjection() {
  const now = Date.now();
  const at = (days, hours = 0) => new Date(now - days * DAY_MS - hours * 3_600_000).toISOString();
  const interfaces = fixtureInterfaces();
  const eeInstances = fixtureEnvironments(at);
  const workerEnvironments = fixtureWorkerEnvironments(eeInstances);
  const runs = fixtureRuns(at, eeInstances, workerEnvironments);
  for (const [index, environment] of workerEnvironments.entries()) {
    const run = runs[index];
    environment.run_id = run?.run_id || "";
    environment.worker = run?.worker || "";
    environment.lifecycle = environment.status === "released" ? "released" : "active";
    environment.last_activity_movement_seq = 1_000 - index;
  }
  const runDetails = {};
  const bindings = [];
  for (const [index, run] of runs.entries()) {
    const checkpoints = fixtureCheckpoints(run, index, at);
    const runBindings = checkpoints.flatMap((checkpoint) => [
      fixtureRow({
        binding_id: `${checkpoint.checkpoint_id}:in`,
        run_id: run.run_id,
        ee_id: run.ee_id,
        worker: run.worker,
        checkpoint_id: checkpoint.checkpoint_id,
        interface_id: checkpoint.interface,
        direction: "inbound",
        status: checkpoint.status,
      }),
      ...checkpoint.out_interfaces.map((interfaceId) => fixtureRow({
        binding_id: `${checkpoint.checkpoint_id}:out:${interfaceId}`,
        run_id: run.run_id,
        ee_id: run.ee_id,
        worker: run.worker,
        checkpoint_id: checkpoint.checkpoint_id,
        interface_id: interfaceId,
        direction: "outbound",
        status: checkpoint.status,
      })),
    ]);
    bindings.push(...runBindings);
    runDetails[run.run_id] = fixtureRow({
      run,
      bindings: runBindings,
      checkpoints,
      worker_environment: workerEnvironments.find((item) => item.worker_environment_id === run.worker_environment_id),
      artifacts: [],
      marks: [],
    });
  }

  const artifacts = Array.from({ length: 58 }, (_, index) => {
    const run = runs[index % runs.length];
    const labels = ["context.reference", "llm.request", "interface.episode.output", "worker.native.summary", "run.start", "operator.message"];
    return fixtureRow({
      artifact_id: `lab_artifact_${String(index + 1).padStart(3, "0")}`,
      label: labels[index % labels.length],
      route_label: index % 4 === 0 ? "system.worker" : "",
      run_id: run.run_id,
      ee_id: run.ee_id,
      worker: run.worker,
      status: index % 13 === 0 ? "pending" : "recorded",
      created_at: at((index * 7) % 30, index % 18),
      size: 4_096 + (index % 11) * 3_277,
    });
  }).sort((left, right) => Date.parse(right.created_at) - Date.parse(left.created_at));

  const minutesAgo = (minutes) => new Date(now - minutes * 60_000).toISOString();
  const markSources = {
    task: { run_id: "lab_run_2", ee_id: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", worker_original: "system_worker_operator_task" },
    review: { run_id: "lab_run_3", ee_id: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", worker_original: "system_worker_operator_task" },
    home: { run_id: "lab_run_1", ee_id: FIXTURE_ROOT_EE_ID, worker: "lab_system_worker_planner", worker_original: "system_worker_operator_home" },
    memory: { run_id: "lab_run_memory_observer", ee_id: FIXTURE_ROOT_EE_ID, worker: "lab_system_worker_memory_observer_3c5c96af8a1b2c7902af96c2", worker_original: "system_worker_memory_observer" },
    projection: { run_id: "lab_run_memory_projection", ee_id: FIXTURE_ROOT_EE_ID, worker: "lab_system_worker_memory_projection_3c5c96af8a1b2c7902af96c2", worker_original: "system_worker_memory_projection" },
  };
  const markPlan = [
    ["task", "tool.call.allowed", 2], ["task", "tool.call.allowed", 3], ["task", "tool.call.allowed", 4], ["task", "tool.call.allowed", 6],
    ["task", "tool.result.corresponds", 7], ["task", "coder.build.ok", 9],
    ["memory", "memory.experience.admitted", 14], ["projection", "memory.capture.completed", 15],
    ["home", "task.summary.received", 38], ["home", "operator.approval", 52],
    ["review", "tool.call.allowed", 95], ["review", "tool.call.allowed", 97], ["review", "tool.result.corresponds", 99],
    ["memory", "memory.experience.admitted", 26 * 60], ["projection", "memory.capture.completed", 26 * 60 + 2],
    ["home", "operator.input.provided", 30 * 60], ["task", "context.recipe.valid", 6 * 24 * 60],
  ];
  const marks = markPlan.map(([source, name, minutes], index) => {
    const origin = markSources[source];
    return fixtureRow({
      movement_id: `lab_mark_movement_${index + 1}`,
      mark_name: name,
      status: "admitted",
      run_id: origin.run_id,
      ee_id: origin.ee_id,
      worker: origin.worker,
      worker_original: origin.worker_original,
      checkpoint_id: `${origin.run_id}:checkpoint:${(index % 2) + 1}`,
      interface_id: "",
      artifact_id: artifacts[index % artifacts.length].artifact_id,
      created_at: minutesAgo(minutes),
      seq: 900 + markPlan.length - index,
      source: "ledger",
    });
  }).sort((left, right) => Date.parse(right.created_at) - Date.parse(left.created_at));

  const movements = fixtureMovements(now, runs, interfaces, artifacts);
  const envelopes = movements.slice(0, 180).map((movement, index) => fixtureRow({
    envelope_id: `lab_envelope_${String(index + 1).padStart(3, "0")}`,
    kind: ["InterfaceArrival", "ArtifactRecorded", "InterfaceOutbound", "MarkOutcomeAdmitted(llm.output.ok)"][index % 4],
    run_id: movement.raw_run_id,
    trace_id: movement.trace,
    ts: new Date(movement.recorded_at_unix_ms).toISOString(),
    recorded_at_unix_ms: movement.recorded_at_unix_ms,
    invocation_metadata: {
      runtime: {
        run: movement.raw_run_id,
        ee: movement.raw_ee_id,
        worker: movement.raw_worker,
        checkpoint: `${movement.raw_run_id}:checkpoint:${(index % 2) + 1}`,
      },
    },
    artifact_id: movement.artifact_id,
    artifact: { artifact_id: movement.artifact_id, label: movement.artifact_label },
  }));

  for (const detail of Object.values(runDetails)) {
    detail.artifacts = artifacts.filter((artifact) => artifact.run_id === detail.run.run_id);
    detail.marks = marks.filter((mark) => mark.run_id === detail.run.run_id);
  }

  const goals = eeInstances.map((environment, index) => fixtureRow({
    goal_id: `lab_goal_${index + 1}`,
    status: environment.status === "done" ? "complete" : "active",
    kind: environment.template_name,
    title: environment.goal,
    ee_id: environment.ee_id,
    created_at: environment.created_at,
  }));
  const eeTemplates = eeInstances.map((environment) => fixtureRow({
    descriptor_id: `ee_template:${environment.template_name}`,
    id: environment.template_name,
    name: environment.template_name,
    goal: environment.goal,
    source: "lab.registry",
    allowed_interfaces: environment.policy.allowed_interfaces,
    allowed_tools: environment.policy.allowed_tools,
    allowed_worker_environment_kinds: environment.policy.allowed_worker_environment_kinds,
  }));
  const tools = [
    "coder.list_dir",
    "coder.read_file",
    "coder.grep",
    "coder.apply_patch",
    "coder.build",
    "browser.navigate",
    "artifact_handoff",
  ].map((name, index) => fixtureRow({
    descriptor_id: `tool:${name}`,
    id: name,
    name,
    interface_ref: interfaces[index % interfaces.length].rule_id,
    effect_categories: index < 3 ? ["read"] : ["world_effect"],
    is_destructive: name === "coder.apply_patch" || name === "coder.build",
    supports_progress: name.startsWith("coder."),
  }));
  const verifiers = ["artifact_schema", "policy_grant", "mark_admission"].map((name) => fixtureRow({
    name,
    handler_name: `verify_${name}`,
    source: "lab.registry",
  }));
  const workers = interfaces.filter((item) => item.interface_type === "System" && item.role === "worker").map((item, index) => fixtureRow({
    worker: item.rule_id,
    status: "registered",
    ceiling_interfaces_count: 4 + index,
    ceiling_tools_count: 3 + index,
    ceiling_worker_environment_kinds_count: 2,
    ceiling_mark_outcomes_count: 3,
    policy_path: `lab://registry/interface/${item.rule_id}/worker_policy.json`,
  }));
  const policy = eeTemplates.map((template) => fixtureRow({
    name: template.name,
    allowed_interfaces_count: template.allowed_interfaces.length,
    allowed_tools_count: template.allowed_tools.length,
    allowed_worker_environment_kinds_count: template.allowed_worker_environment_kinds.length,
    source: "lab.registry",
  }));
  const artifactLabels = [...new Set(artifacts.map((item) => item.label))].map((label) => fixtureRow({
    label,
    lifecycle: "artifact",
    allowed_writers: ["lab_system_worker_planner"],
    finalize_recommended: label.includes("output"),
  }));
  const markKinds = [...new Set(marks.map((item) => item.mark_name))].map((name) => fixtureRow({
    name,
    lifecycle: "registered",
    class: "checkpoint",
    package: "lab.verifiers",
  }));
  const descriptorKinds = ["interface", "ee_template", "worker_environment_kind", "tool", "verifier", "artifact", "mark"].map((kind) => fixtureRow({
    kind,
    identity_field: kind === "mark" ? "name | id | kind" : "id | name",
    descriptor_filename: `${kind}.json`,
    source: "lab.projection",
  }));
  const descriptorInventory = descriptorKinds.map((row, index) => fixtureRow({
    registry: row.kind,
    count: [interfaces.length, eeTemplates.length, workerEnvironments.length, tools.length, verifiers.length, artifactLabels.length, markKinds.length][index],
    runtime_count: index === 0 ? 2 : 0,
    warning_count: 0,
    descriptor_filename: row.descriptor_filename,
  }));
  const runtimeDescriptors = [fixtureRow({ descriptor_id: "lab_runtime_voice_widget", kind: "client_surface", name: "voice_status", created_by_ee: eeInstances[0].ee_id, accepted_at: at(1) })];
  const descriptorMutations = [fixtureRow({ mutation_id: "lab_mutation_1", status: "accepted", kind: "client_surface", name: "voice_status", descriptor_id: runtimeDescriptors[0].descriptor_id })];
  const transferTargets = bindings.filter((item) => item.direction === "inbound").map((binding) => fixtureRow({
    target_id: `${binding.interface_id}:${binding.run_id}:${binding.checkpoint_id}`,
    interface_id: binding.interface_id,
    status: "ready",
    run_id: binding.run_id,
    ee_id: binding.ee_id,
    worker: binding.worker,
    checkpoint_id: binding.checkpoint_id,
    reason: "lab checkpoint target",
  }));
  const pendingArtifacts = artifacts.filter((item) => item.status === "pending").map((artifact) => fixtureRow({
    pending_id: `pending:${artifact.artifact_id}`,
    template_name: "lab_world_operator_chat",
    artifact_id: artifact.artifact_id,
    trace_id: `lab_trace_${artifact.artifact_id}`,
    created_at: artifact.created_at,
  }));
  const workerEnvironmentPools = [fixtureRow({ pool_id: "lab_pool_local", worker_environment_kind: "git_worktree", status: "active", allocation_policy: "per_run", worker_environment_count: 3 })];
  const workerEnvironmentKinds = Array.from(new Map(workerEnvironments.map((environment) => [environment.worker_environment_kind, fixtureRow({
    id: environment.worker_environment_kind,
    name: environment.worker_environment_kind,
    worker_environment_kind_id: environment.worker_environment_kind,
    worker_environment_kind: environment.worker_environment_kind,
    substrate_family: environment.substrate_family,
    capabilities: environment.capabilities,
    supported_executor_kinds: environment.substrate_family === "browser" ? ["browser"] : ["exec"],
    boundary_defaults: { network_ceiling: environment.substrate_family === "filesystem" ? "denied" : "restricted" },
    lifecycle: { requires_release: environment.worker_environment_kind.includes("session") },
    descriptor_path: environment.descriptor_path,
    source: "lab.registry",
  })])).values());
  const projectPresentations = [
    fixtureRow({ ee_id: FIXTURE_PRIMARY_PROJECT_ID, name: "Lumen", icon: "code", color: "violet" }),
    fixtureRow({ ee_id: FIXTURE_RESEARCH_PROJECT_ID, name: "Research", icon: "globe", color: "teal" }),
    fixtureRow({ ee_id: FIXTURE_OPERATIONS_PROJECT_ID, name: "Operations", icon: "briefcase", color: "orange" }),
  ];
  const applications = [];
  const drivers = [];
  const hostRootRefs = {
    host_filesystem_root: "/",
    workspace_root: FIXTURE_WORKSPACE_ROOT,
    browser_profile_root: "/workspace/.browser-profile",
  };
  const scheduledArrival = new Date(now + 2 * 60 * 60 * 1_000);
  const scheduledFollowup = new Date(scheduledArrival.getTime() + 2 * 60 * 60 * 1_000);
  const chatSeeds = [
    { runId: "lab_run_1", title: "Home", surfaceRole: "home", eeId: FIXTURE_ROOT_EE_ID, worker: "lab_system_worker_planner", updatedDaysAgo: 0.08, createdDaysAgo: 18, nextScheduledAtMs: 0 },
    { runId: "lab_run_2", title: "Add connection status to project header", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 0.03, createdDaysAgo: 2, nextScheduledAtMs: 0 },
    { runId: "lab_run_3", title: "Review onboarding empty states", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 0.4, createdDaysAgo: 3, nextScheduledAtMs: 0 },
    { runId: "lab_run_4", title: "Prepare weekly project pulse", surfaceRole: "task", eeId: FIXTURE_OPERATIONS_PROJECT_ID, worker: "lab_system_worker_planner", updatedDaysAgo: 0.9, createdDaysAgo: 6, nextScheduledAtMs: scheduledArrival.getTime() },
    { runId: "lab_run_5", title: "Compare onboarding patterns", surfaceRole: "task", eeId: FIXTURE_RESEARCH_PROJECT_ID, worker: "lab_system_worker_research", updatedDaysAgo: 1.4, createdDaysAgo: 5, nextScheduledAtMs: scheduledFollowup.getTime() },
    { runId: "lab_run_6", title: "Polish settings navigation", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 2.1, createdDaysAgo: 8, nextScheduledAtMs: 0 },
    { runId: "lab_run_7", title: "Prepare release notes", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 3.2, createdDaysAgo: 9, nextScheduledAtMs: 0 },
    { runId: "lab_run_8", title: "Investigate preview reconnect", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 4.1, createdDaysAgo: 11, nextScheduledAtMs: 0, archived: true },
  ];
  const chats = chatSeeds.map((seed, index) => {
    const chatEnvironment = eeInstances.find((environment) => environment.ee_id === seed.eeId) ?? eeInstances[0];
    return fixtureRow({
      run_id: seed.runId,
      title: seed.title,
      chat_interface_id: `lab_world_operator_chat_${seed.runId}`,
      client_surface_role: seed.surfaceRole,
      writable: true,
      os_root_ee_id: FIXTURE_ROOT_EE_ID,
      turn_open: false,
      turn_state: "idle",
      ee_id: chatEnvironment.ee_id,
      ee_goal: chatEnvironment.goal,
      ee_template_name: chatEnvironment.template_name,
      provider_interface_id: "world_provider_codex",
      provider_bound_interface_id: `world_provider_codex_${seed.runId}`,
      provider_model: "",
      provider_models_refreshable: false,
      worker_interface_id: seed.worker,
      worker_choice_interface_id: seed.worker,
      door_host_slots: {},
      reasoning_effort: index === 1 ? "high" : "medium",
      created_at_ms: now - Math.round(seed.createdDaysAgo * DAY_MS),
      updated_at_ms: now - Math.round(seed.updatedDaysAgo * DAY_MS),
      next_scheduled_at_ms: seed.nextScheduledAtMs || 0,
      message_count: 0,
      pinned_artifact_ids: [],
      pinned_context_items: [],
      continued_from: {},
      archived: seed.archived === true,
      voice_context: seed.surfaceRole === "home"
        ? { control_interface_id: "lab_world_voice_control", provider_id: "world_provider_realtime", delegation_interface_id: "lab_world_voice_delegation", run_id: "lab_run_voice" }
        : null,
      service_tier: null,
      record_digest: fixtureDigest(`lab-run-record:${seed.runId}`),
      ...fixtureChatAgent(chatEnvironment.ee_id, projectPresentations),
      connected_application_ids: LAB_CHAT_APPLICATIONS[seed.runId] ?? [],
    });
  });
  let chatMovementSeq = 1;
  const chatMessages = Object.fromEntries(chats.map((chat, index) => {
    const messages = fixtureChatHistory(chat, index, () => chatMovementSeq++);
    if (messages.some((message) => ["approval", "user_input", "elicitation"].includes(message.event_kind) && message.event_data?.resolved !== true)) {
      chat.turn_open = true;
      chat.turn_state = "waiting";
    }
    chat.message_count = messages.length;
    chat.last_movement_seq = messages.at(-1)?.movement_seq || 0;
    chat.updated_at_ms = Math.max(chat.updated_at_ms, messages.at(-1)?.created_at_ms || 0);
    const terminal = messages.filter((message) => (
      message.role === "assistant"
      && message.direction === "out"
      && message.event_kind === "message"
      && message.terminal === true
      && message.interface_id === chat.chat_interface_id
      && message.artifact_id
    )).at(-1);
    chat.last_terminal_message = terminal
      ? {
          artifact_id: terminal.artifact_id,
          created_at_ms: terminal.created_at_ms,
          interface_id: terminal.interface_id,
          movement_seq: terminal.movement_seq,
          text: terminal.text.replace(/\s+/g, " ").trim().slice(0, 240),
        }
      : {};
    return [chat.run_id, messages];
  }));
  const badge = chats.find((chat) => chat.run_id === "lab_run_2");
  const badgeAnswer = chatMessages.lab_run_2?.findLast((message) => message.event_kind === "message" && message.terminal && message.role === "assistant");
  const excerpt = "Reused the existing `StatusBadge` component and semantic tones.";
  if (badge && badgeAnswer && badgeAnswer.text.includes(excerpt)) {
    const start = badgeAnswer.text.indexOf(excerpt);
    const pinnedId = `operator.pinned_excerpt:lab-${badgeAnswer.artifact_id}-${start}-${start + excerpt.length}`;
    badge.pinned_artifact_ids = [pinnedId];
    badge.pinned_context_items = [{
      artifact_id: pinnedId,
      source_artifact_id: badgeAnswer.artifact_id,
      source_message_id: badgeAnswer.id,
      text: excerpt,
      start,
      end: start + excerpt.length,
      role: "assistant",
    }];
  }
  const notes = chats.find((chat) => chat.run_id === "lab_run_7");
  const settingsAnswer = chatMessages.lab_run_6?.findLast((message) => message.event_kind === "message" && message.terminal && message.role === "assistant");
  if (notes && settingsAnswer) notes.continued_from = { run_id: "lab_run_6", artifact_id: settingsAnswer.artifact_id };

  return {
    projection: fixtureRow({
      generated_at: new Date(now).toISOString(),
      generation: 1,
      mode: "lab",
      repo_root: "lab://octopus-client",
      durable_root: "lab://octopus-client/ledger",
      descriptor_counts: Object.fromEntries(descriptorInventory.map((row) => [row.registry, row.count])),
      ledger_rows: movements.length,
      warnings: [],
    }),
    eeInstances,
    runs,
    runDetails,
    interfaces,
    transferTargets,
    bindings,
    artifacts,
    marks,
    workerEnvironments,
    workerEnvironmentPools,
    workerEnvironmentKinds,
    envelopes,
    pendingArtifacts,
    goals,
    eeTemplates,
    workers,
    tools,
    verifiers,
    policy,
    descriptorInventory,
    runtimeDescriptors,
    descriptorMutations,
    descriptorKinds,
    artifactLabels,
    markKinds,
    projectPresentations,
    applications,
    drivers,
    hostRootRefs,
    chats,
    chatMessages,
    movements,
    scheduledInterfaces: fixtureScheduledInterfaces(now, chats, eeInstances),
  };
}

function fixtureScheduledInterfaces(now, chats, environments) {
  const nextWeekday = (hour, weekday = null) => {
    const next = new Date(now);
    next.setHours(hour, 0, 0, 0);
    while (next.getTime() <= now || (weekday === null ? [0, 6].includes(next.getDay()) : next.getDay() !== weekday)) {
      next.setDate(next.getDate() + 1);
    }
    return next.getTime();
  };
  const chatInterface = (runId) => chats.find((chat) => chat.run_id === runId)?.chat_interface_id ?? "";
  const delivery = (seq, daysAgo, runId, journal = null) => ({
    seq,
    at_ms: now - daysAgo * DAY_MS,
    run_id: runId,
    chat_interface_id: chatInterface(runId),
    journal,
  });
  const reviewJournal = {
    digest: fixtureDigest("lab-system-review-journal"),
    outcome: "changed",
    summary: "Tightened the onboarding empty state copy and removed an unused settings toggle.",
    checks: [
      { check: "Client type check", result: "passed", evidence: "tsc --noEmit" },
      { check: "Settings navigation contract", result: "passed", evidence: "test-client-navigation" },
    ],
    changes: [
      { kind: "file", path: "app/src/components/onboarding/EmptyState.tsx", action: "modified", reason: "Shorter first-run copy.", status: "applied", restorable: true, unrestorable_reason: "" },
      { kind: "file", path: "app/src/components/settings/legacyToggle.tsx", action: "deleted", reason: "The toggle had no reader.", status: "applied", restorable: true, unrestorable_reason: "" },
    ],
    undoable: true,
    undone: false,
    error: "",
  };
  const rows = [
    {
      interface_id: "lab_world_daily_brief",
      display_name: "Daily brief",
      status: "enabled",
      schedule: { type: "cron", cron: "0 8 * * 1-5" },
      instructions: "Summarize recent work, active runs, blockers, and the priorities that need attention today.",
      ee_id: FIXTURE_OPERATIONS_PROJECT_ID,
      next_run_at_ms: nextWeekday(8),
      next_run_source: "schedule",
      deliveries: [delivery(910, 1, "lab_run_4"), delivery(842, 2, "lab_run_4")],
    },
    {
      interface_id: "lab_world_weekly_review",
      display_name: "Weekly review",
      status: "disabled",
      schedule: { type: "cron", cron: "0 16 * * 5" },
      instructions: "Review this week's completed and active runs and draft a concise status update.",
      ee_id: FIXTURE_PRIMARY_PROJECT_ID,
      next_run_at_ms: null,
      next_run_source: "",
      deliveries: [delivery(615, 6, "lab_run_7")],
    },
    {
      interface_id: "lab_world_system_review",
      display_name: "System review",
      status: "enabled",
      schedule: {
        type: "inactivity",
        seconds: 3_600,
        interface_match: { interface_type: "world", role: "user" },
        exclude_interface_match: { interface_driver_ref: "cron_life" },
      },
      instructions: "Review recent work in Octopus. If something would make the system work better, make one small, reversible change and record it in the change journal.",
      ee_id: FIXTURE_ROOT_EE_ID,
      next_run_at_ms: now + 42 * 60_000,
      next_run_source: "driver_state",
      inactivity_state: { status: "waiting", current: true, last_activity_at_ms: now - 18 * 60_000 },
      deliveries: [delivery(1024, 0.6, "lab_run_3", reviewJournal)],
    },
    {
      interface_id: "lab_world_cron",
      display_name: "Trigger",
      status: "template",
      template: true,
      schedule: { type: "cron", cron: "0 9 * * 1-5" },
      instructions: "",
      ee_id: FIXTURE_ROOT_EE_ID,
      next_run_at_ms: null,
      next_run_source: "",
      deliveries: [],
    },
  ];
  const known = new Set(environments.map((environment) => environment.ee_id));
  return rows.filter((row) => known.has(row.ee_id)).map((row) => labScheduledRow(row));
}

function labScheduledRow(row) {
  const deliveries = row.deliveries ?? [];
  const activation = {
    schedule: row.schedule,
    timezone: row.timezone ?? "UTC",
    fire_on_start: row.fire_on_start === true,
    instructions: row.instructions,
    status: row.status,
  };
  return fixtureRow({
    interface_id: row.interface_id,
    display_name: row.display_name,
    template: row.template === true,
    status: row.status,
    valid: true,
    activation_digest: fixtureDigest(JSON.stringify(activation)),
    activation_path: `lab://registry/interface/${row.interface_id}/activation.json`,
    descriptor_path: `lab://registry/interface/${row.interface_id}/interface.json`,
    schedule: row.schedule,
    timezone: activation.timezone,
    fire_on_start: activation.fire_on_start,
    instructions: row.instructions,
    reminder_env: "",
    run_template: {
      run_id: `lab_run_template_${row.interface_id}`,
      ee_id: row.ee_id,
      worker_interface_id: fixtureEnvironmentWorker(row.ee_id),
      checkpoint_count: 3,
      outbound_interface_ids: [],
      artifact_binding_count: 0,
    },
    clone_eligible: true,
    last_run_at_ms: deliveries[0]?.at_ms ?? null,
    last_movement_seq: deliveries[0]?.seq ?? null,
    next_run_at_ms: row.status === "enabled" ? row.next_run_at_ms : null,
    next_run_source: row.status === "enabled" ? row.next_run_source : "",
    inactivity_state: row.schedule.type === "inactivity" ? row.inactivity_state ?? { status: "unstarted", current: true, last_activity_at_ms: null } : null,
    deliveries,
    ee_id: row.ee_id,
    error: "",
  });
}

function labArtifactBytes(data, params) {
  const runId = requiredString(params.run_id, "run_id");
  if (Object.hasOwn(params, "call_id")) throw new Error("The tool result for this call is not admitted in this run.");
  const artifactId = requiredString(params.artifact_id, "artifact_id");
  const interfaceId = requiredString(params.interface_id, "interface_id");
  if (Object.hasOwn(params, "attachment_id")) throw new Error("Test attachments keep only their name and size.");
  const message = (data.chatMessages[runId] ?? []).find((candidate) => candidate.artifact_id === artifactId);
  const artifact = message ? null : data.artifacts.find((candidate) => candidate.artifact_id === artifactId && candidate.run_id === runId);
  if (!message && !artifact) throw new Error("The requested artifact is not admitted for this run and interface.");
  if (message && message.interface_id !== interfaceId) throw new Error("The requested artifact is not admitted for this run and interface.");
  const label = message ? message.label : artifact.label;
  const payload = message
    ? Object.fromEntries(Object.entries(message.event_data ?? {}).filter(([key]) => !key.startsWith("_")))
    : { label: artifact.label, run_id: artifact.run_id, ee_id: artifact.ee_id, worker: artifact.worker, status: artifact.status, created_at: artifact.created_at };
  const bytes = Buffer.from(`${JSON.stringify(payload, null, 2)}\n`, "utf8");
  return {
    artifact_id: artifactId,
    run_id: runId,
    interface_id: interfaceId,
    filename: `${label}.json`,
    mime_type: "application/json",
    content_hash: fixtureDigest(bytes),
    size: bytes.byteLength,
    base64: bytes.toString("base64"),
  };
}

function fixtureDigest(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function fixtureChatAgent(eeId, projectPresentations) {
  if (eeId === FIXTURE_ROOT_EE_ID) return { agent_id: null, agent_name: null, agent_icon: null, agent_color: null };
  return {
    agent_id: `lab_agent_${eeId}`,
    agent_name: eeId === FIXTURE_PRIMARY_PROJECT_ID ? "Octopus Development" : "Research Development",
    agent_icon: eeId === FIXTURE_PRIMARY_PROJECT_ID ? "glasses" : "curious",
    agent_color: projectPresentations.find((presentation) => presentation.ee_id === eeId)?.color || null,
  };
}

function labChatEvents(chat) {
  const threadId = `lab-thread-${chat.run_id}`;
  const provider = chat.provider_bound_interface_id || chat.provider_interface_id;
  const worker = chat.worker_interface_id;
  const envelope = (turnId, itemId, lifecycle, startedAtMs = null, completedAtMs = null, durationMs = null) => ({
    turn_id: turnId,
    item_id: itemId,
    lifecycle,
    started_at_ms: startedAtMs,
    completed_at_ms: completedAtMs,
    duration_ms: durationMs,
  });
  const row = ({ id, role = "assistant", label, eventKind, eventData, text, createdAtMs, seq, durable = true, interfaceId = provider, latencyMs = 0, terminal = false, attachments = [] }) => {
    const transport = durable
      ? {
          _movement: {
            seq,
            trace_id: "",
            recorded_at_unix_ms: createdAtMs,
            from: { Interface: role === "user" ? chat.chat_interface_id : provider },
            to: { Interface: worker },
            metadata: { runtime: { run: chat.run_id, ee: chat.ee_id, worker, checkpoint: "cp_provider_output", interface: role === "user" ? chat.chat_interface_id : provider, epoch: 0, trace_id: "" } },
          },
        }
      : { _stream: { stream_id: `lab-stream-${chat.run_id}`, stream_seq: seq, byte_start_offset: 0, byte_end_offset: 0, source_movement_seq: seq, recorded_at_unix_ms: createdAtMs } };
    const origin = role === "user" ? {} : { _activity_origin: { interface_id: provider, interface_role: "provider" }, _run_activity: true };
    return fixtureRow({
      id: role === "user" ? id : `${chat.run_id}:${id}`,
      run_id: chat.run_id,
      role,
      label,
      direction: role === "user" ? "in" : "out",
      event_kind: eventKind,
      event_data: { ...eventData, ...transport, ...origin },
      text,
      created_at_ms: createdAtMs,
      movement_seq: seq,
      artifact_id: durable ? (role === "user" ? id : `${label}:${chat.run_id}:${id}`) : "",
      content_hash: durable ? fixtureDigest(`${chat.run_id}:${id}:${text}`) : "",
      interface_id: interfaceId,
      latency_ms: latencyMs,
      attachments,
      error: false,
      terminal,
    });
  };
  return {
    turnId: (id) => `lab-turn-${chat.run_id}-${id}`,
    user: ({ id, text, createdAtMs, seq, attachments = [] }) => row({
      id,
      role: "user",
      label: "operator.message",
      eventKind: "message",
      eventData: { text, message_id: id, attachments, _event: envelope("", "", "") },
      text,
      createdAtMs,
      seq,
      interfaceId: chat.chat_interface_id,
      terminal: true,
      attachments,
    }),
    turnStarted: ({ id, turnId, createdAtMs, seq }) => row({
      id,
      label: "turn.started",
      eventKind: "turn_started",
      eventData: {
        label: "turn.started",
        threadId,
        turn: { completedAt: null, durationMs: null, error: null, id: turnId, items: [], itemsView: "notLoaded", startedAt: Math.floor(createdAtMs / 1000), status: "inProgress" },
        _event: envelope(turnId, "", "started", Math.floor(createdAtMs / 1000)),
      },
      text: "Turn started",
      createdAtMs,
      seq,
      durable: false,
    }),
    reasoning: ({ id, turnId, summary, createdAtMs, seq }) => {
      const itemId = `rs_lab_${chat.run_id}_${id}`;
      return row({
        id,
        label: "turn.reasoning",
        eventKind: "reasoning",
        eventData: { completed_at_ms: createdAtMs, content: [], item_id: itemId, label: "turn.reasoning", lifecycle: "completed", summary, thread_id: threadId, turn_id: turnId, _event: envelope(turnId, itemId, "completed", null, createdAtMs) },
        text: summary.join("\n"),
        createdAtMs,
        seq,
      });
    },
    commentary: ({ id, turnId, text, createdAtMs, seq }) => {
      const itemId = `msg_lab_${chat.run_id}_${id}`;
      return row({
        id,
        label: "turn.agent_message",
        eventKind: "commentary",
        eventData: { completed_at_ms: createdAtMs, item_id: itemId, label: "turn.agent_message", lifecycle: "completed", phase: "commentary", text, thread_id: threadId, turn_id: turnId, _event: envelope(turnId, itemId, "completed", null, createdAtMs) },
        text,
        createdAtMs,
        seq,
      });
    },
    tool: ({ id, turnId, tool, args, body, createdAtMs, seq }) => {
      const callId = `lab-call-${chat.run_id}-${id}`;
      const completed = body !== undefined;
      return row({
        id: `${id}-${completed ? "completed" : "started"}`,
        label: "turn.dynamic_tool_call",
        eventKind: "dynamic_tool",
        eventData: {
          arguments: args,
          call_id: callId,
          item_id: callId,
          label: "turn.dynamic_tool_call",
          lifecycle: completed ? "completed" : "started",
          namespace: "formed_interfaces",
          status: completed ? "completed" : "started",
          thread_id: threadId,
          tool,
          turn_id: turnId,
          ...(completed ? { body, error: null, ok: true, success: true } : {}),
          _event: envelope(turnId, callId, completed ? "completed" : "started"),
        },
        text: tool,
        createdAtMs,
        seq,
        durable: false,
      });
    },
    fileChange: ({ id, turnId, changes, diff = "", createdAtMs, seq }) => {
      const itemId = `lab-patch-${chat.run_id}-${id}`;
      return row({
        id,
        label: "turn.file_change",
        eventKind: "file_change",
        eventData: { changes, ...(diff ? { diff } : {}), item_id: itemId, label: "turn.file_change", lifecycle: "completed", status: "completed", thread_id: threadId, turn_id: turnId, _event: envelope(turnId, itemId, "completed") },
        text: `Updated ${changes.map((change) => change.path.split("/").at(-1)).join(", ")}`,
        createdAtMs,
        seq,
      });
    },
    finalAnswer: ({ id, turnId, text, createdAtMs, seq, contextTokens, latencyMs }) => {
      const itemId = `msg_lab_${chat.run_id}_${id}`;
      const outputTokens = Math.round(text.length / 3.6) + 180;
      return row({
        id,
        label: "interface.artifact.output",
        eventKind: "message",
        eventData: {
          context_window_usage: { context_window: LAB_CONTEXT_WINDOW, used_tokens: contextTokens },
          item_id: itemId,
          label: "interface.artifact.output",
          lifecycle: "completed",
          phase: "final_answer",
          provider_token_usage: {
            context_window: LAB_CONTEXT_WINDOW,
            cumulative: { cached_input_tokens: contextTokens * 4, input_tokens: contextTokens * 5, output_tokens: outputTokens * 3, reasoning_output_tokens: outputTokens, total_tokens: contextTokens * 5 + outputTokens * 3, uncached_input_tokens: contextTokens },
            current_context: { cached_input_tokens: contextTokens - 2_400, input_tokens: contextTokens - outputTokens, output_tokens: outputTokens, reasoning_output_tokens: Math.round(outputTokens / 2), total_tokens: contextTokens, uncached_input_tokens: 2_400 },
          },
          text,
          thread_id: threadId,
          turn_id: turnId,
          _event: envelope(turnId, itemId, "completed", null, createdAtMs),
        },
        text,
        createdAtMs,
        seq,
        interfaceId: chat.chat_interface_id,
        latencyMs,
        terminal: true,
      });
    },
    request: ({ id, turnId, kind, data, text, createdAtMs, seq }) => row({
      id,
      label: `operator.${kind}.request`,
      eventKind: kind,
      eventData: { ...data, label: `operator.${kind}.request`, thread_id: threadId, turn_id: turnId, _event: envelope(turnId, data.request_id, "started") },
      text,
      createdAtMs,
      seq,
      interfaceId: chat.chat_interface_id,
      terminal: true,
    }),
    resolved: ({ id, turnId, requestId, text, createdAtMs, seq }) => row({
      id,
      label: "turn.operator_request.resolved",
      eventKind: "operator_request_resolved",
      eventData: { label: "turn.operator_request.resolved", requestId, thread_id: threadId, turn_id: turnId, _event: envelope(turnId, requestId, "completed") },
      text,
      createdAtMs,
      seq,
      interfaceId: chat.chat_interface_id,
    }),
    turnCompleted: ({ id, turnId, startedAtMs, finalId, finalText, createdAtMs, seq }) => row({
      id,
      label: "turn.completed",
      eventKind: "turn_completed",
      eventData: {
        label: "turn.completed",
        threadId,
        turn: {
          completedAt: Math.floor(createdAtMs / 1000),
          durationMs: createdAtMs - startedAtMs,
          error: null,
          id: turnId,
          items: [{ id: `msg_lab_${chat.run_id}_${finalId}`, phase: "final_answer", text: finalText, type: "agentMessage" }],
          itemsView: "summary",
          startedAt: Math.floor(startedAtMs / 1000),
          status: "completed",
        },
        _event: envelope(turnId, "", "completed", Math.floor(startedAtMs / 1000), Math.floor(createdAtMs / 1000), createdAtMs - startedAtMs),
      },
      text: "Turn completed",
      createdAtMs,
      seq,
    }),
  };
}

function fixtureChatHistory(chat, index, nextSeq) {
  const rows = [];
  const events = labChatEvents(chat);
  let contextTokens = 18_400 + index * 2_300;
  const userMessage = (id, text, createdAtMs) => rows.push(events.user({ id: `msg_${chat.run_id}_${id}`, text, createdAtMs, seq: nextSeq() }));
  const turn = (id, startAtMs, durationMs, steps, finalText) => {
    const turnId = events.turnId(id);
    rows.push(events.turnStarted({ id: `${id}-turn-started`, turnId, createdAtMs: startAtMs + 600, seq: nextSeq() }));
    for (const step of steps) rows.push(step(turnId, startAtMs));
    const finalAtMs = startAtMs + durationMs;
    contextTokens += 6_800 + finalText.length * 3;
    rows.push(events.finalAnswer({ id: `${id}-final`, turnId, text: finalText, createdAtMs: finalAtMs, seq: nextSeq(), contextTokens, latencyMs: durationMs }));
    rows.push(events.turnCompleted({ id: `${id}-turn-completed`, turnId, startedAtMs: startAtMs, finalId: `${id}-final`, finalText, createdAtMs: finalAtMs + 200, seq: nextSeq() }));
  };
  const reasoning = (id, offsetMs, summary) => (turnId, startAtMs) => events.reasoning({ id, turnId, summary, createdAtMs: startAtMs + offsetMs, seq: nextSeq() });
  const commentary = (id, offsetMs, text) => (turnId, startAtMs) => events.commentary({ id, turnId, text, createdAtMs: startAtMs + offsetMs, seq: nextSeq() });
  const toolCall = (id, startOffsetMs, endOffsetMs, tool, args, body) => [
    (turnId, startAtMs) => events.tool({ id, turnId, tool, args, createdAtMs: startAtMs + startOffsetMs, seq: nextSeq() }),
    (turnId, startAtMs) => events.tool({ id, turnId, tool, args, body, createdAtMs: startAtMs + endOffsetMs, seq: nextSeq() }),
  ];
  const fileChange = (id, offsetMs, changes, diff) => (turnId, startAtMs) => events.fileChange({ id, turnId, changes, diff, createdAtMs: startAtMs + offsetMs, seq: nextSeq() });
  const pendingRequest = (id, askAtMs, prompt, kind, text, data) => {
    const turnId = events.turnId(id);
    userMessage(id, prompt, askAtMs - 9_000);
    rows.push(events.turnStarted({ id: `${id}-turn-started`, turnId, createdAtMs: askAtMs - 8_400, seq: nextSeq() }));
    rows.push(events.reasoning({ id: `${id}-reasoning`, turnId, summary: [`**${text}**`], createdAtMs: askAtMs - 4_000, seq: nextSeq() }));
    rows.push(events.request({ id: `${id}-request`, turnId, kind, data, text, createdAtMs: askAtMs, seq: nextSeq() }));
  };

  if (chat.run_id === "lab_run_1") {
    const firstAt = chat.created_at_ms + 45_000;
    userMessage("overview-request", "Give me a concise overview of the active projects and what needs attention.", firstAt);
    turn("overview", firstAt + 400, 38_000, [
      reasoning("overview-reasoning", 4_000, ["**Reading active project state**"]),
      ...toolCall("overview-runs", 6_000, 9_500, "projection.runs.list", { limit: 20 }, { runs: 8 }),
    ], [
      "You have three active areas:",
      "",
      "- **Lumen** — the current product pass is healthy; the connection-status task is ready for review.",
      "- **Research** — the onboarding comparison is complete and pinned for later use.",
      "- **Operations** — the weekly project pulse is scheduled for later today.",
      "",
      "Nothing is blocked. The most useful next step is to review the small Lumen UI change, then decide whether to include it in the next release.",
    ].join("\n"));
    const secondAt = chat.updated_at_ms - 48_000;
    userMessage("today-request", "What should I focus on next?", secondAt);
    turn("today", secondAt + 400, 47_000, [
      reasoning("today-reasoning", 5_000, ["**Ranking open work by impact**"]),
    ], "Review **Add connection status to project header**. It is a contained change with a passing focused check, so you can inspect the file diff without opening any private project data.");
    return rows;
  }

  if (chat.run_id === "lab_run_2") {
    const turnStart = chat.updated_at_ms - 128_000;
    userMessage("request", "Add a compact connection-status badge to the project header. Reuse the existing visual language and run a focused check.", turnStart);
    turn("badge", turnStart + 400, 127_000, [
      reasoning("reasoning-inspect", 6_000, ["**Inspecting the existing project header**"]),
      commentary("plan", 9_000, "I'll look at the header and the existing status primitive first, then add the badge without changing the layout."),
      ...toolCall("list", 11_000, 15_000, "coder.list_dir", { path: "src/components" }, { entries: ["ProjectHeader.tsx", "StatusBadge.tsx"] }),
      ...toolCall("read-header", 20_000, 28_000, "coder.read_file", { path: "src/components/ProjectHeader.tsx" }, { bytes: 1_842 }),
      ...toolCall("read-badge", 32_000, 39_000, "coder.read_file", { path: "src/components/StatusBadge.tsx" }, { bytes: 816 }),
      reasoning("reasoning-implementation", 46_000, ["**Reusing the existing status primitive**"]),
      commentary("found", 50_000, "`StatusBadge` already has the semantic tones I need, so the header only needs a small slot for it."),
      ...toolCall("search", 53_000, 61_000, "coder.grep", { path: "src", query: "connectionStatus" }, { matches: 3 }),
      fileChange("header-change", 79_000, [{ added: 12, deleted: 3, kind: "update", path: "src/components/ProjectHeader.tsx" }], fixtureProjectHeaderDiff()),
      reasoning("reasoning-verify", 92_000, ["**Verifying the focused interaction**"]),
      ...toolCall("check", 99_000, 120_000, "coder.build", { command: "npm test -- ProjectHeader" }, { output: "PASS src/components/ProjectHeader.test.tsx\n1 test passed" }),
    ], [
      "Added the connection-status badge to the project header.",
      "",
      "- Reused the existing `StatusBadge` component and semantic tones.",
      "- Kept the project title layout stable at narrow widths.",
      "- Added an accessible status label for screen readers.",
      "- `npm test -- ProjectHeader` passes.",
    ].join("\n"));
    pendingRequest("tests", Date.now() - 3 * 60_000, "Also run the full header suite before you finish.", "approval", "Run the header tests?", {
      request_id: "lab-approval-header-tests",
      kind: "exec",
      method: "item/commandExecution/requestApproval",
      title: "Run the header tests?",
      detail: { command: "npm test -- ProjectHeader", cwd: FIXTURE_WORKSPACE_ROOT, reason: "Checks the new connection badge before handing the change back." },
    });
    return rows;
  }

  const prompts = [
    "",
    "",
    "Review the empty states in onboarding and identify the smallest useful polish pass.",
    "Prepare a short project pulse for the end of the week.",
    "Compare three strong onboarding patterns and summarize the useful differences.",
    "Make settings navigation feel calmer without changing its information architecture.",
    "Draft release notes from the completed work in this project.",
    "Find why the preview occasionally reconnects after becoming active.",
  ];
  const progress = [
    "",
    "",
    "I'll walk the onboarding screens and note where the empty states disagree.",
    "I'll collect this week's finished and active runs before writing the pulse.",
    "I'll compare the three patterns on first action, optional setup, and the way back.",
    "I'll keep the section structure and only adjust spacing, copy weight, and focus.",
    "I'll group the completed work by outcome before drafting.",
    "I'll reproduce the reconnect and check the preview identity across tab resume.",
  ];
  const responses = [
    "",
    "",
    "The empty states now use one action hierarchy and consistent copy. I kept the existing flow intact and recorded the remaining illustration work separately.",
    "The weekly pulse is ready: one completed UI improvement, one research summary, and no blocked work. The scheduled task will refresh it later today.",
    "The strongest pattern is progressive disclosure: show one clear next action, keep optional setup nearby, and preserve a visible way back. I saved a concise comparison for the project.",
    "Settings navigation now keeps section titles stable, uses quieter secondary copy, and preserves keyboard focus across section changes.",
    "Release notes are drafted around outcomes rather than implementation details and are ready for a final wording pass.",
    "The reconnect was caused by a stale preview identity after the tab resumed. The diagnosis is recorded; no private endpoint or machine detail is present in this demo.",
  ];
  const requestAt = chat.updated_at_ms - 58_000;
  userMessage("request", prompts[index] || chat.title, requestAt);
  turn("work", requestAt + 400, 57_000, [
    reasoning("reasoning", 5_000, [`**${chat.title}**`]),
    commentary("progress", 8_000, progress[index] || "Working on it."),
  ], responses[index] || "Done.");
  if (chat.run_id === "lab_run_5") {
    pendingRequest("scope", Date.now() - 18 * 60_000, "Go deeper on this and include tools outside our category.", "user_input", "Which products should I compare?", {
      request_id: "lab-question-compare-scope",
      method: "item/tool/requestUserInput",
      message: "Which products should I compare?",
      questions: [{
        id: "scope",
        header: "Scope",
        question: "Which products should I compare?",
        options: [
          { label: "Five closest", description: "A quick comparison of the tools most like ours." },
          { label: "Wider set of twelve", description: "Takes longer and covers mobile apps too." },
        ],
      }],
    });
  }
  return rows;
}

function fixtureWorkspaceEntries() {
  return [
    { path: "README.md", kind: "file", text: "# Lumen\n\nA small demo project used by the isolated Client Lab profile.\n" },
    { path: "package.json", kind: "file", text: JSON.stringify({ name: "lumen-demo", private: true, scripts: { test: "vitest run" } }, null, 2) },
    { path: "src", kind: "directory" },
    { path: "src/components", kind: "directory" },
    { path: "src/components/ProjectHeader.tsx", kind: "file", text: fixtureProjectHeaderSource() },
    { path: "src/components/ProjectHeader.test.tsx", kind: "file", text: fixtureProjectHeaderTestSource() },
    { path: "src/components/StatusBadge.tsx", kind: "file", text: fixtureStatusBadgeSource() },
    { path: "src/styles", kind: "directory" },
    { path: "src/styles/tokens.css", kind: "file", text: ":root {\n  --status-online: #3fb950;\n  --status-offline: #8b949e;\n}\n" },
  ];
}

function fixtureProjectHeaderDiff() {
  return [
    "diff --git a/src/components/ProjectHeader.tsx b/src/components/ProjectHeader.tsx",
    "--- a/src/components/ProjectHeader.tsx",
    "+++ b/src/components/ProjectHeader.tsx",
    "@@ -1,10 +1,19 @@",
    " import { StatusBadge } from \"./StatusBadge\";",
    " ",
    "-export function ProjectHeader({ name }: { name: string }) {",
    "+type Props = { name: string; online: boolean };",
    "+",
    "+export function ProjectHeader({ name, online }: Props) {",
    "   return (",
    "     <header className=\"project-header\">",
    "       <h1>{name}</h1>",
    "+      <StatusBadge",
    "+        label={online ? \"Connected\" : \"Offline\"}",
    "+        tone={online ? \"success\" : \"neutral\"}",
    "+      />",
    "     </header>",
    "   );",
    " }",
  ].join("\n");
}

function fixtureProjectHeaderSourceBefore() {
  return [
    "export function ProjectHeader({ name }: { name: string }) {",
    "  return (",
    "    <header className=\"project-header\">",
    "      <h1>{name}</h1>",
    "    </header>",
    "  );",
    "}",
    "",
  ].join("\n");
}

function fixtureProjectHeaderSource() {
  return [
    "import { StatusBadge } from \"./StatusBadge\";",
    "",
    "type Props = { name: string; online: boolean };",
    "",
    "export function ProjectHeader({ name, online }: Props) {",
    "  return (",
    "    <header className=\"project-header\">",
    "      <h1>{name}</h1>",
    "      <StatusBadge",
    "        label={online ? \"Connected\" : \"Offline\"}",
    "        tone={online ? \"success\" : \"neutral\"}",
    "      />",
    "    </header>",
    "  );",
    "}",
    "",
  ].join("\n");
}

function fixtureStatusBadgeSource() {
  return [
    "type Props = { label: string; tone: \"success\" | \"neutral\" };",
    "",
    "export function StatusBadge({ label, tone }: Props) {",
    "  return <span aria-label={`Project status: ${label}`} data-tone={tone}>{label}</span>;",
    "}",
    "",
  ].join("\n");
}

function fixtureProjectHeaderTestSource() {
  return [
    "import { render, screen } from \"@testing-library/react\";",
    "import { ProjectHeader } from \"./ProjectHeader\";",
    "",
    "it(\"labels the current connection state\", () => {",
    "  render(<ProjectHeader name=\"Lumen\" online />);",
    "  expect(screen.getByLabelText(\"Project status: Connected\")).toBeVisible();",
    "});",
    "",
  ].join("\n");
}

function fixtureInterfaces() {
  const rows = [
    ["lab_system_worker_planner", "System", "bidirectional", "worker"],
    ["lab_system_worker_coder", "System", "bidirectional", "worker"],
    ["lab_system_worker_research", "System", "bidirectional", "worker"],
    ["lab_system_browser", "System", "bidirectional", "service"],
    ["lab_system_exec", "System", "bidirectional", "service"],
    ["lab_world_operator_chat", "World", "inbound", "user"],
    ["world_provider_codex", "World", "bidirectional", "provider"],
    ["lab_world_web_search", "World", "bidirectional", "service"],
    ["lab_world_calendar", "World", "bidirectional", "service"],
    ["lab_world_email", "World", "bidirectional", "service"],
    ["lab_world_files", "World", "bidirectional", "service"],
  ];
  return rows.map(([ruleId, interfaceType, direction, role], index) => fixtureRow({
    id: ruleId,
    rule_id: ruleId,
    interface_type: interfaceType,
    direction,
    role,
    mode: index < 6 ? "active" : "registered",
    tool_ref: role === "service" ? ruleId.replace("lab_", "") : "",
    source: "lab.registry",
    artifact_transfer_protocol_status: "ready",
    artifact_transfer_target_count: 1,
    artifact_transfer_ready_targets: 1,
    artifact_transfer_target_reason: "lab target is ready",
  }));
}

function fixtureEnvironments(at) {
  const rows = [
    [FIXTURE_ROOT_EE_ID, "existence", "active", "", "Coordinate Home, projects, scheduled work, and operator requests"],
    [FIXTURE_PRIMARY_PROJECT_ID, "self_improvement", "active", FIXTURE_ROOT_EE_ID, "Develop and improve the Lumen product"],
    [FIXTURE_RESEARCH_PROJECT_ID, "operator_interaction", "active", FIXTURE_ROOT_EE_ID, "Research product patterns and preserve useful evidence"],
    [FIXTURE_OPERATIONS_PROJECT_ID, "operator_interaction", "idle", FIXTURE_ROOT_EE_ID, "Prepare recurring project updates and operational summaries"],
  ];
  return rows.map(([eeId, template, status, parent, goal], index) => fixtureRow({
    ee_id: eeId,
    template_name: template,
    status,
    goal_id: `lab_goal_${index + 1}`,
    goal,
    parent_ee_id: parent,
    created_at: at(28 - index * 3),
    record_digest: `lab-ee-digest-${index + 1}`,
    allowed_child_ee_templates: index === 0 ? ["self_improvement", "operator_interaction"] : [],
    policy: {
      allowed_interfaces: index === 0
        ? ["lab_system_worker_planner", "lab_world_operator_chat", "world_provider_codex", "lab_world_calendar"]
        : ["lab_system_worker_coder", "lab_system_worker_research", "lab_system_exec", "lab_world_operator_chat", "world_provider_codex", "lab_world_web_search", "lab_world_files"],
      allowed_tools: index === 0
        ? ["artifact_handoff"]
        : ["coder.list_dir", "coder.read_file", "coder.grep", "coder.apply_patch", "coder.build"],
      allowed_worker_environment_kinds: index === 0
        ? ["system_room"]
        : ["git_worktree", "browser_session", "process_session", "web_search"],
    },
  }));
}

function fixtureWorkerEnvironments(environments) {
  const kinds = [
    "system_room",
    "git_worktree",
    "browser_session",
    "process_session",
    "desktop_session",
    "code_mode",
    "remote_exec_server",
    "container_session",
    "mcp_stdio_session",
    "web_search",
    "obsidian_vault",
  ];
  const environmentIds = [
    FIXTURE_ROOT_EE_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_OPERATIONS_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_OPERATIONS_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_ROOT_EE_ID,
  ];
  const now = Date.now();
  return kinds.map((kind, index) => fixtureRow({
    worker_environment_id: `lab_we_${index + 1}`,
    worker_environment_kind: kind,
    kind,
    status: fixtureWorkerEnvironmentStatus(index),
    live_available: fixtureWorkerEnvironmentStatus(index) === "active",
    opened_at_unix_ms: now - (index + 1) * 3_600_000,
    last_activity_at_unix_ms: fixtureWorkerEnvironmentStatus(index) === "released" ? now - (index + 1) * 1_800_000 : now - (index + 1) * 240_000,
    opened_at_movement_seq: 40 + index * 7,
    description: kind === "git_worktree" ? "Isolated worktree for the Lumen project." : `${kind.split("_").join(" ")} for lab tasks.`,
    projection_kind: "worker_environment",
    ee_id: environmentIds[index] || environments[index % environments.length].ee_id,
    display_name: kind === "git_worktree" ? "Lumen worktree" : kind.split("_").join(" "),
    root: kind.includes("worktree") ? FIXTURE_WORKSPACE_ROOT : "",
    session_id: kind.includes("session") ? `lab_session_${index + 1}` : "",
    transport: fixtureWorkerEnvironmentTransport(kind),
    substrate_family: kind.includes("browser")
      ? "browser"
      : kind.includes("container")
        ? "container_runtime"
        : kind.includes("mcp")
          ? "mcp_stdio_room"
          : kind === "code_mode"
            ? "code_mode"
            : "filesystem",
    door: "lab_system_exec",
    descriptor_path: `lab://registry/worker_environment_kind/${kind}.json`,
    capabilities: ["read", index % 2 ? "execute" : "project"],
    tools: index % 2 ? ["shell_exec"] : ["read_projection"],
    client_surfaces: fixtureWorkerEnvironmentSurfaces(kind, index),
  }));
}

function fixtureWorkerEnvironmentSurfaces(kind, index) {
  const surface = (id, surfaceKind, label, data = {}, extra = {}) => fixtureRow({
    id,
    kind: surfaceKind,
    label,
    mode: "fixture",
    status: "available",
    read_only: true,
    data,
    ...extra,
  });
  const terminal = surface(`${kind}_terminal`, "terminal", "Terminal", {
    lines: [
      `$ octopus environment inspect lab_we_${index + 1}`,
      `kind=${kind}`,
      "status=ready",
      "Demo environment ready. No host process was executed.",
    ],
  }, { protocol: "xterm.fixture" });
  const files = surface(`${kind}_files`, "files", "Files", {
    entries: [
      { path: "README.md", kind: "file", text: `# ${kind}\n\nIsolated Worker Environment fixture.\n` },
      { path: "src", kind: "directory" },
      { path: "src/environment.json", kind: "file", text: JSON.stringify({ kind, status: "ready" }, null, 2) },
    ],
  }, { root: `fixture://${kind}` });
  const connection = surface(
    `${kind}_connection`,
    "connection",
    "Connection",
    fixtureWorkerEnvironmentConnection(kind, index),
    { protocol: `connection.${kind}.fixture` },
  );
  if (kind === "browser_session") {
    return [connection, surface("browser_primary", "browser", "Browser", {
      url: "https://example.test/octopus",
      title: "Octopus Test Browser",
      heading: "Browser session",
      body: "A deterministic browser room fixture. Live mode requires a projected viewer endpoint.",
      pages: [
        { url: "https://example.test/octopus", title: "Octopus Test Browser", heading: "Browser session", body: "A deterministic browser room fixture. Live mode requires a projected viewer endpoint." },
        { url: "https://example.test/artifacts", title: "Artifact viewer", heading: "Artifact projection", body: "This page is isolated demo state and has no network authority." },
        { url: "https://example.test/status", title: "Room status", heading: "Browser room ready", body: "Navigation, history, and reload are deterministic inside Test." },
      ],
    }, { protocol: "browser.fixture", viewer_url: "fixture://browser" })];
  }
  if (kind === "desktop_session") {
    return [connection, surface("desktop_primary", "desktop", "Desktop", {
      title: "Desktop session",
      detail: "Isolated remote desktop fixture",
    }, { protocol: "desktop.fixture", viewer_url: "fixture://desktop" })];
  }
  if (kind === "git_worktree") {
    const worktreeFiles = surface("worktree_files", "files", "Files", {
      entries: fixtureWorkspaceEntries(),
    }, { root: `fixture://${FIXTURE_WORKSPACE_ROOT}` });
    return [
      surface("worktree_status", "worktree_status", "Worktree", {
        phase: "worktree-ready",
        label: "lumen",
        description: "Persistent project worktree",
        branch: "feature/project-status",
        worktree_git_root: FIXTURE_WORKSPACE_ROOT,
        worktree_workspace_root: FIXTURE_WORKSPACE_ROOT,
        local_environment_config_path: ".octopus/environment.json",
        worktree_output_text: `Prepared isolated demo worktree at ${FIXTURE_WORKSPACE_ROOT}`,
        setup_output_text: "Resolved project dependencies\nPrepared preview and review surfaces",
      }, { protocol: "worktree.status.fixture" }),
      connection,
      surface("worktree_review", "review", "Review", {
        branch: "feature/project-status",
        base: "main",
        filesChanged: 1,
        additions: 12,
        deletions: 3,
        files: ["src/components/ProjectHeader.tsx"],
        diffs: [
          {
            path: "src/components/ProjectHeader.tsx",
            before: fixtureProjectHeaderSourceBefore(),
            after: fixtureProjectHeaderSource(),
          },
        ],
      }),
      terminal,
      worktreeFiles,
      surface("worktree_preview", "preview", "Preview", {
        url: "http://127.0.0.1:4174",
        title: "Lumen preview",
        detail: "Isolated demo preview; no network request is made.",
      }, { protocol: "browser.fixture", viewer_url: "fixture://preview" }),
    ];
  }
  if (kind === "container_session") {
    return [
      connection,
      terminal,
      files,
      surface("container_runtime", "container", "Runtime", {
        name: "octopus-worker-7f2a",
        image: "ubuntu:24.04 · linux/arm64",
        state: "Running",
        cpu: "18%",
        memory: "612 MB / 4 GB",
        uptime: "36m",
        network: "12.4 MB ↓ · 3.1 MB ↑",
        processes: [
          { pid: "1", command: "octopus-room", cpu: "2.4%", memory: "94 MB" },
          { pid: "28", command: "python worker.py", cpu: "14.8%", memory: "486 MB" },
          { pid: "77", command: "bash", cpu: "0.1%", memory: "7 MB" },
        ],
        ports: [
          { port: "3000/tcp", service: "Preview" },
          { port: "9222/tcp", service: "Browser debugging" },
        ],
        mounts: [
          { target: "/workspace", source: "git_worktree" },
          { target: "/artifacts", source: "artifact cache · read only" },
        ],
      }),
      surface("container_preview", "preview", "Preview", {
        url: "https://preview.example.test/container",
        title: "Container preview",
        detail: "An isolated preview fixture with no network or runtime authority.",
      }, { protocol: "browser.fixture", viewer_url: "fixture://container-preview" }),
    ];
  }
  if (kind === "code_mode") {
    return [connection, surface("code_mode_runtime", "runtime", "Runtime activity", {
      active_cells: ["cell_plan", "cell_patch"],
      pending_tool_calls: [
        { id: "call_17", tool: "artifact.read", status: "pending" },
        { id: "call_18", tool: "exec.run", status: "queued" },
      ],
      detail: "Deterministic Test live-state; no tool process is running.",
    }, { protocol: "worker_environment.live_state.fixture" })];
  }
  if (kind === "mcp_stdio_session") {
    return [connection, surface("mcp_stdio_runtime", "runtime", "MCP server", {
      server: {
        pid: 4217,
        command: "octopus-mcp-stdio-room-tool",
        pending_events: 2,
        protocol_version: "2025-06-18",
      },
      event_log: "initialize → ready\ntools/list → 6 tools\nresources/list → 3 resources",
      stderr_log: "",
    }, { protocol: "mcp_stdio_room.fixture" })];
  }
  if (kind === "web_search") {
    return [connection, surface("web_search_runtime", "runtime", "Search room", {
      detail: "Search results are deterministic Test evidence; no network request is made.",
      server: { provider: "fixture", queries: 3, artifacts: 7 },
    }, { protocol: "web_search.fixture" })];
  }
  if (kind === "obsidian_vault") {
    return [connection, surface("vault_files", "files", "Vault", {
      entries: [
        { path: "README.md", kind: "file", text: "# Test vault\n\nA projected file snapshot for UI development.\n" },
        { path: "Architecture", kind: "directory" },
        { path: "Architecture/Worker Environments.md", kind: "file", text: "# Worker Environments\n\nViews arrive through explicit surface descriptors.\n" },
        { path: "Daily", kind: "directory" },
        { path: "Daily/2026-08-07.md", kind: "file", text: "# 2026-08-07\n\nDemo daily note.\n" },
      ],
    }, { root: "fixture://obsidian-vault" })];
  }
  if (kind === "system_room") return [connection, files];
  return [connection, terminal, files];
}

function fixtureWorkerEnvironmentConnection(kind, index) {
  const suffix = String(index + 1).padStart(2, "0");
  const state = fixtureWorkerEnvironmentStatus(index);
  const common = {
    state,
    sync: state === "released" ? "Final snapshot" : "Current",
    health: state === "released" ? "Released · evidence retained" : state === "idle" ? "Idle · ready to resume" : "Healthy · checked 4s ago",
    capabilities: ["inspect", "attach", "read projected state"],
    events: [
      { state, label: state === "released" ? "Environment released" : "View endpoint ready", detail: "Projected by the Test Worker Environment fixture", time: "now" },
      { state: "active", label: "Environment formed", detail: "Door, kind, and policy assembled for this run", time: "18s" },
      { state: "completed", label: "Initial state synchronized", detail: "Fixture state only; no host files were read", time: "22s" },
    ],
  };
  if (kind === "browser_session") return {
    ...common,
    name: `browser-room-${suffix}`,
    provider: "Chromium browser room",
    address: `cdp+test://browser-room-${suffix}`,
    transport: "CDP viewer",
    workspace: "Ephemeral browser profile",
    persistence: "Run lifetime",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "navigate", "screenshot"],
  };
  if (kind === "desktop_session") return {
    ...common,
    name: `desktop-room-${suffix}`,
    provider: "Remote desktop room",
    address: `vnc+test://desktop-room-${suffix}`,
    transport: "Desktop viewer",
    workspace: "Isolated desktop session",
    persistence: "Run lifetime",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "observe desktop", "input preview"],
  };
  if (kind === "git_worktree") return {
    ...common,
    name: "lumen-worktree",
    provider: "Git worktree",
    address: `worktree+fixture://${FIXTURE_WORKSPACE_ROOT}`,
    transport: "Local filesystem room",
    workspace: FIXTURE_WORKSPACE_ROOT,
    persistence: "Persistent branch",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "review diff", "application preview"],
  };
  if (kind === "remote_exec_server") return {
    ...common,
    name: `remote-worker-${suffix}`,
    provider: "Remote exec room",
    address: `wss://test-worker-${suffix}.invalid/exec`,
    transport: "remote_exec_server",
    workspace: "/home/octopus/workspace",
    persistence: "Reusable connection",
    region: "eu-west · Demo",
    capabilities: [...common.capabilities, "terminal", "file sync"],
  };
  if (kind === "container_session") return {
    ...common,
    name: `octopus-worker-${suffix}`,
    provider: "Local container",
    address: `docker+test://octopus-worker-${suffix}`,
    transport: "Container process room",
    workspace: "/workspace",
    persistence: "Persistent filesystem",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "terminal", "files", "runtime metrics"],
  };
  return {
    ...common,
    name: `${kind.replaceAll("_", "-")}-${suffix}`,
    provider: kind === "process_session" ? "Local process room" : "Execution room",
    address: `room+test://${kind}/${suffix}`,
    transport: kind === "process_session" ? "Local process" : "Projected room",
    workspace: "/workspace",
    persistence: kind === "process_session" ? "Run lifetime" : "Descriptor-defined",
    region: "Local Test boundary",
  };
}

function fixtureRuns(at, environments, workerEnvironments) {
  const workers = [
    "lab_system_worker_planner",
    "lab_system_worker_coder",
    "lab_system_worker_coder",
    "lab_system_worker_planner",
    "lab_system_worker_research",
    "lab_system_worker_coder",
  ];
  return Array.from({ length: workerEnvironments.length }, (_, index) => {
    const workerEnvironment = workerEnvironments[index];
    const environment = environments.find((candidate) => candidate.ee_id === workerEnvironment.ee_id)
      ?? environments[index % environments.length];
    const workerEnvironmentStatus = fixtureWorkerEnvironmentStatus(index);
    return fixtureRow({
      run_id: `lab_run_${index + 1}`,
      status: workerEnvironmentStatus === "released" ? "completed" : workerEnvironmentStatus,
      worker: workers[index % workers.length],
      ee_id: environment.ee_id,
      goal: environment.goal,
      created_at: at(Math.max(0, 28 - index * 2), index),
      worker_environment_id: workerEnvironment.worker_environment_id,
      source: "lab.run_record",
      meta: { record_path: `lab://runs/lab_run_${index + 1}.json` },
    });
  });
}

function fixtureWorkerEnvironmentStatus(index) {
  return ["active", "released", "active", "idle", "released", "active"][index % 6];
}

function fixtureWorkerEnvironmentTransport(kind) {
  if (kind === "browser_session") return "browser.fixture";
  if (kind === "desktop_session") return "desktop.fixture";
  if (kind === "remote_exec_server") return "xterm.websocket.fixture";
  if (kind === "container_session") return "container_runtime.fixture";
  if (kind === "mcp_stdio_session") return "mcp_stdio_room.fixture";
  if (kind === "code_mode") return "code_mode.live_state.fixture";
  if (kind === "process_session") return "terminal.fixture";
  return "projection.fixture";
}

function fixtureCheckpoints(run, runIndex, at) {
  return Array.from({ length: runIndex % 2 ? 2 : 3 }, (_, index) => {
    const projected = index === 2 && run.status === "active";
    return fixtureRow({
      checkpoint_id: `${run.run_id}:checkpoint:${index + 1}`,
      label: ["Receive context", "Execute task", "Verify output"][index],
      status: projected ? "projected" : "made",
      interface: index === 0 ? "lab_world_operator_chat" : "lab_system_worker_planner",
      out_interfaces: index === 2 ? ["lab_world_operator_chat"] : ["world_provider_codex"],
      tool_name: index === 1 ? "shell_exec" : "artifact_handoff",
      artifact_label: ["context.reference", "llm.request", "interface.episode.output"][index],
      artifact_bindings: index + 1,
      interface_bindings: 2,
      recorded_at_unix_ms: Date.parse(at(Math.max(0, 20 - runIndex * 2 - index))),
      required_verified_artifacts: index === 2 ? [{ mark: "llm.output.ok", artifact_label: "interface.episode.output", verifier: "artifact_schema", outcome: projected ? "declared" : "closed" }] : [],
    });
  });
}

function fixtureApplicationProject(environment) {
  return fixtureRow({
    ee_id: environment.ee_id,
    name: fixtureEnvironmentDisplayName(environment.ee_id),
    goal: fixtureEnvironmentDisplayName(environment.ee_id),
    parent_ee_id: environment.parent_ee_id,
    record_digest: environment.record_digest,
  });
}

function fixtureEnvironmentDisplayName(eeId) {
  if (eeId === FIXTURE_ROOT_EE_ID) return "Home";
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return "Lumen";
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return "Research";
  if (eeId === FIXTURE_OPERATIONS_PROJECT_ID) return "Operations";
  return eeId;
}

function labFixtureConnectionApplication() {
  const id = LAB_CONNECTION_APPLICATION_ID;
  const digest = crypto.createHash("sha256").update(id).digest("hex");
  const logo = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="#2f6fed"/><path d="M15 21h34a3 3 0 0 1 3 3v17a3 3 0 0 1-3 3H15a3 3 0 0 1-3-3V24a3 3 0 0 1 3-3Z" fill="#fff"/><path d="m13 23 19 13 19-13" fill="none" stroke="#2f6fed" stroke-width="3" stroke-linejoin="round"/></svg>`;
  return {
    record: {
      id,
      product_id: id,
      installed: true,
      available: true,
      release_status: "optional",
      manifest_digest: digest,
      installed_digest: digest,
      source_digest: digest,
      source_file_count: 1,
      receipt_state: "valid",
      receipt_digest: digest,
      install_packages: [],
    },
    manifest: {
      schema_version: 3,
      id,
      name: "Lumen Mail",
      description: "Search, read and send email from the tasks you connect.",
      category: "connector",
      developer: "Lumen",
      version: "1.0.0",
      icon: "email",
      logo: "application-logo.svg",
      logo_dark: "",
      interfaces: [id],
      artifact_bindings: [],
      task_capabilities: { tools: [] },
      setup: {
        fields: [{
          interface: id,
          pointer: "/interface_driver_config/private_config/environment/address_env",
          label: "Email address",
          kind: "text",
          required: true,
          placeholder: "name@example.com",
          help: "Use the address you sign in with.",
        }],
      },
      connection: {
        disclosure: [
          "Lumen Mail reads and sends messages only in tasks you connect to it.",
          "Your sign-in stays in this Octopus Home. Disconnect anytime to remove it.",
        ],
        help: { label: "About Lumen Mail", url: "https://example.com/" },
      },
      logo_data_url: `data:image/svg+xml;base64,${Buffer.from(logo).toString("base64")}`,
      logo_dark_data_url: "",
      descriptors: {
        [id]: {
          id,
          interface_type: "world",
          role: "user",
          direction: "bidirectional",
          source_names: ["lumen_mail"],
          interface_driver_config: {
            private_config: {
              environment: { address_env: "LUMEN_MAIL_ADDRESS" },
              client_connection: { schema_version: 1, module: `interface://${id}/packages/connection.cjs` },
            },
          },
        },
      },
    },
  };
}

function labSetupFields(manifest, descriptors, interfaceId = "") {
  return manifest.setup.fields.flatMap((field) => {
    if (interfaceId && field.interface !== interfaceId) return [];
    const name = resolveJsonPointer(descriptors[field.interface] ?? {}, field.pointer);
    if (typeof name !== "string" || !name) {
      throw new Error(`Test application '${manifest.id}' setup field '${field.label}' does not name an environment value`);
    }
    return [{ field, name }];
  });
}

function labSeedPrivateValues(sources, errors) {
  const inUse = new Set([...Object.values(LAB_CHAT_APPLICATIONS), ...Object.values(LAB_DEFAULT_APPLICATIONS)].flat());
  const values = {};
  for (const { record, manifest } of sources.values()) {
    try {
      for (const { field, name } of labSetupFields(manifest, manifest.descriptors)) {
        values[field.interface] ??= {};
        if (record.installed === true && inUse.has(manifest.id) && field.required) values[field.interface][name] = "test-profile-value";
      }
    } catch (error) {
      errors.push(String(error?.message ?? error));
    }
  }
  return values;
}

function labApplicationConfiguration(manifest, descriptors, installed, privateValues) {
  if (!installed) return { status: "not_installed", fields: [], documents: [], error: "" };
  let entries;
  try {
    entries = labSetupFields(manifest, descriptors);
  } catch (error) {
    return { status: "issue", fields: [], documents: [], error: String(error?.message ?? error) };
  }
  const fields = entries.map(({ field, name }) => {
    const saved = typeof privateValues[field.interface]?.[name] === "string";
    return {
      interface_id: field.interface,
      name,
      label: field.label,
      kind: field.kind,
      required: field.required,
      authored: true,
      placeholder: field.placeholder ?? "",
      help: field.help ?? "",
      configured: saved,
      private_saved: saved,
      source: saved ? "private" : "missing",
    };
  });
  const documents = [...new Set(fields.map((field) => field.interface_id))].map((interfaceId) => {
    const digest = labPrivateValuesDigest(privateValues[interfaceId]);
    return { interface_id: interfaceId, status: digest ? "valid" : "absent", digest };
  });
  return {
    status: fields.some((field) => field.required && !field.configured) ? "required" : "configured",
    fields,
    documents,
    error: "",
  };
}

function labPrivateValuesDigest(values) {
  const entries = Object.entries(values ?? {}).sort(([left], [right]) => left.localeCompare(right));
  return entries.length ? crypto.createHash("sha256").update(JSON.stringify(entries)).digest("hex") : "";
}

function labApplicationRows(entries, data) {
  return entries.map(({ record, manifest: { descriptors, ...manifest } }) => {
    const installed = record.installed === true;
    const managed = record.release_status === "system";
    const connected = installed
      ? data.chats.filter((chat) => (chat.connected_application_ids ?? []).includes(manifest.id)).map((chat) => chat.run_id)
      : [];
    return fixtureRow({
      ...manifest,
      configuration: labApplicationConfiguration(manifest, descriptors, installed, data.privateValues),
      connection_interfaces: installed && manifest.id === LAB_CONNECTION_APPLICATION_ID
        ? manifest.interfaces.filter((interfaceId) => Object.prototype.hasOwnProperty.call(descriptors[interfaceId]?.interface_driver_config?.private_config ?? {}, "client_connection"))
        : [],
      installed,
      materialized: installed,
      partially_materialized: false,
      valid: true,
      missing_interface_ids: [],
      descriptor_path: `lab://registry/interface/${manifest.id}/interface.json`,
      manifest_path: `lab://registry/interface/${manifest.id}/application.json`,
      manifest_digest: record.manifest_digest,
      package_id: record.id,
      installed_digest: record.installed_digest,
      source_available: record.available === true,
      source_kind: record.available === true ? "local" : "none",
      install_packages: record.install_packages,
      source_digest: record.source_digest,
      source_file_count: record.source_file_count,
      receipt_state: record.receipt_state,
      receipt_digest: record.receipt_digest,
      managed,
      install_block_reason: installed
        ? "Application is already installed."
        : record.available === true ? "" : "No verified package source is available.",
      removal_allowed: installed && !managed && connected.length === 0,
      removal_block_reason: !installed
        ? "Application is not installed."
        : managed
          ? "This application is supplied by the Octopus installation and cannot be removed independently."
          : connected.length
            ? `Disconnect this application from ${connected.length} task${connected.length === 1 ? "" : "s"} before removing it.`
            : "",
      connected_run_ids: connected,
      partially_connected_run_ids: [],
      project_states: data.eeInstances.map((environment) => fixtureRow({
        ee_id: environment.ee_id,
        is_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
        policy_allowed: installed,
        ceiling_allowed: true,
        selected_by_default: installed && (LAB_DEFAULT_APPLICATIONS[environment.ee_id] ?? []).includes(manifest.id),
        record_digest: environment.record_digest,
      })),
    });
  });
}

function labDriverRows(entries, data) {
  return entries.map(({ record, descriptor, tools }) => {
    const name = record.product_id;
    const installed = record.installed === true;
    const activeInstances = data.workerEnvironments.filter((environment) => (
      environment.worker_environment_kind === name
      && environment.status !== "released"
    ));
    const hostSlots = LAB_DRIVER_HOST_SLOTS[name] ?? {};
    const allowed = installed ? LAB_DRIVER_PROJECTS[name] ?? [FIXTURE_PRIMARY_PROJECT_ID] : [];
    const projectStates = data.eeInstances.map((environment) => {
      const selected = installed && fixtureSelectedDriverNames(environment.ee_id).includes(name);
      const policyAllowed = selected || allowed.includes(environment.ee_id);
      return fixtureRow({
        ee_id: environment.ee_id,
        is_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
        policy_allowed: policyAllowed,
        ceiling_allowed: true,
        selected_by_default: selected,
        record_digest: environment.record_digest,
        host_slots: policyAllowed ? hostSlots : {},
      });
    });
    const projectConfigured = projectStates.some((state) => state.policy_allowed || state.selected_by_default);
    const boundary = descriptor.boundary_defaults ?? {};
    return fixtureRow({
      name,
      display_name: record.display?.name ?? name,
      description: record.display?.description ?? "",
      icon: record.display?.icon ?? "",
      package_id: record.id,
      installed,
      materialized: installed,
      partially_materialized: false,
      source_available: record.available === true,
      source_digest: record.source_digest,
      installed_digest: record.installed_digest,
      source_file_count: record.source_file_count,
      install_packages: record.install_packages,
      agent_presets: [],
      receipt_state: record.receipt_state,
      install_block_reason: installed ? "Driver is already installed." : "",
      removal_allowed: installed && activeInstances.length === 0 && !projectConfigured,
      removal_block_reason: !installed
        ? "Driver is not installed."
        : activeInstances.length > 0
          ? `Release ${activeInstances.length} active worker environment${activeInstances.length === 1 ? "" : "s"} before removing this Driver.`
          : projectConfigured
            ? "Remove this Driver from every project before removing its package."
            : "",
      substrate_family: descriptor.substrate_family ?? "",
      worker_consent: descriptor.worker_consent ?? "explicit_ceiling",
      capabilities: descriptor.capabilities ?? [],
      supported_executor_kinds: descriptor.supported_executor_kinds ?? [],
      instance_config_files: [],
      door_interfaces: [...new Set(tools.map((tool) => tool.interface).filter(Boolean))],
      door_driver_refs: descriptor.door_driver_refs ?? [],
      tool_ids: tools.map((tool) => tool.id),
      tool_bindings: tools.map((tool) => fixtureRow({ tool_id: tool.id, interface_id: tool.interface })),
      boundary_defaults: {
        read_root_refs: boundary.read_root_refs ?? [],
        write_root_refs: boundary.write_root_refs ?? [],
        protected_read_only_subpaths: boundary.protected_read_only_subpaths ?? [],
        network_ceiling: boundary.network_ceiling ?? "denied",
      },
      lifecycle: { requires_release: descriptor.lifecycle?.requires_release === true },
      descriptor_path: `lab://registry/worker_environment_kind/${name}/worker_environment_kind.json`,
      active_instances: activeInstances,
      project_states: projectStates,
    });
  });
}

function fixtureTaskAuthoringProfiles(environments) {
  return environments.map((environment) => {
    const worker = fixtureEnvironmentWorker(environment.ee_id);
    return fixtureRow({
      ee_id: environment.ee_id,
      worker_choice_interface: worker,
      door_host_slots: environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID ? { lab_system_exec: { workspace_root: FIXTURE_WORKSPACE_ROOT } } : {},
      external_inputs: [],
      external_relays: [],
      model_outputs: [],
      context_artifact_bindings: [{ id: "lab_artifact_project_brief" }],
      selected_worker_environment_kinds: fixtureSelectedDriverNames(environment.ee_id),
      selected_tools: [],
    });
  });
}

function fixtureChatExecutionContexts(data) {
  return data.eeInstances.map((environment) => {
    const workerId = fixtureEnvironmentWorker(environment.ee_id);
    const applicationChoices = data.applications.flatMap((application) => {
      const state = application.project_states.find((candidate) => candidate.ee_id === environment.ee_id);
      return state?.policy_allowed ? [{ id: application.id, selected_by_default: state.selected_by_default }] : [];
    });
    const driverChoices = data.drivers.flatMap((driver) => {
      const state = driver.project_states.find((candidate) => candidate.ee_id === environment.ee_id);
      if (!state?.policy_allowed) return [];
      return [{
        name: driver.name,
        capabilities: driver.capabilities,
        root_slots: driver.name === "filesystem_workspace"
          ? ["host_filesystem_root", "workspace_root"]
          : [],
        tool_ids: driver.tool_ids,
        door_interfaces: driver.door_interfaces,
        host_slots: state.host_slots,
        selected_by_default: state.selected_by_default,
      }];
    });
    const workspaceSlots = environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID
      ? { lab_system_exec: { workspace_root: FIXTURE_WORKSPACE_ROOT } }
      : {};
    const agent = environment.ee_id === FIXTURE_ROOT_EE_ID ? null : {
      id: `lab_agent_${environment.ee_id}`,
      name: environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID ? "Octopus Development" : "Research Development",
      icon: environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID ? "glasses" : "curious",
    };
    return fixtureRow({
      ee_id: environment.ee_id,
      agent_catalog_digest: "lab-agent-catalog",
      default_task_access_mode: "project_access",
      host_root_refs: data.hostRootRefs,
          full_access_root: "/",
          source_folders: [FIXTURE_WORKSPACE_ROOT],
      is_os_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
      display_name: data.projectPresentations.find((presentation) => presentation.ee_id === environment.ee_id)?.name
        || (environment.ee_id === FIXTURE_ROOT_EE_ID ? "Home" : environment.goal),
      goal: environment.goal,
      template_name: environment.template_name,
      parent_ee_id: environment.parent_ee_id,
      workers: [{
        id: workerId,
        label: workerId === "lab_system_worker_coder" ? "Coder" : workerId === "lab_system_worker_research" ? "Research" : "Operator",
        authoring_profile: true,
        agent_source_template: agent !== null,
        client_agent_id: agent?.id ?? "",
        client_agent_name: agent?.name ?? "",
        client_agent_icon: agent?.icon ?? "",
        client_agent_created_at_ms: agent ? 1 : 0,
        client_agent_updated_at_ms: agent ? 1 : 0,
        client_agent_deleted_at_ms: 0,
        client_template: environment.ee_id !== FIXTURE_ROOT_EE_ID,
        client_template_provider_interface_id: "world_provider_codex",
        client_template_provider_model: data.providers?.[0]?.model ?? "",
        client_template_reasoning_effort: "high",
        client_template_digest: "lab-client-template",
        client_template_application_ids: applicationChoices.map((application) => application.id),
        client_template_worker_environment_kinds: driverChoices.map((driver) => driver.name),
        provider_interface_ids: FIXTURE_WORKER_PROVIDER_IDS,
        interface_policy: fixtureWorkerInterfacePolicy(data.providers),
        role_artifact_ids: [],
        authoring_room_interface_id: "lab_system_exec",
        door_host_slots: workspaceSlots,
        external_inputs: [{ checkpoint_id: "cp_operator_message", interface_id: "lab_world_operator_chat" }],
        external_relays: [],
        model_outputs: [{
          checkpoint_id: "cp_provider_output",
          interface_id: "world_provider_codex",
          required_verified_artifact: {
            mark: "llm.output.ok",
            artifact_label: "interface.artifact.output",
            verifier: "artifact_schema",
            params: {},
          },
        }],
        context_artifact_bindings: [],
        selected_worker_environment_kinds: fixtureSelectedDriverNames(environment.ee_id),
        applications: applicationChoices,
        drivers: driverChoices,
      }],
    });
  });
}

const FIXTURE_WORKER_PROVIDER_IDS = ["world_provider_codex"];

function fixtureWorkerInterfacePolicy(providers) {
  return Object.fromEntries((providers ?? [])
    .filter((provider) => FIXTURE_WORKER_PROVIDER_IDS.includes(provider.id))
    .map((provider) => [provider.id, { models: provider.models.map((model) => model.model) }]));
}

function fixtureEnvironmentWorker(eeId) {
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return "lab_system_worker_coder";
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return "lab_system_worker_research";
  return "lab_system_worker_planner";
}

function fixtureSelectedDriverNames(eeId) {
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return ["git_worktree"];
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return ["browser_session"];
  return [];
}

function fixtureProviderStatus(chats) {
  const home = chats.find((chat) => chat.client_surface_role === "home");
  return [{
    provider_interface_id: "world_provider_codex",
    observed_at_ms: Date.now(),
    movement_seq: home?.last_movement_seq || 0,
    run_id: home?.run_id || "",
    rate_limits: {
      credits: null,
      individualLimit: null,
      limitId: "lab-demo",
      limitName: "Demo allowance",
      planType: "demo",
      primary: { resetsAt: null, usedPercent: 18, windowDurationMins: 300 },
      secondary: null,
    },
  }];
}

function fixtureMovements(now, runs, interfaces, artifacts) {
  const weights = Array.from({ length: 30 }, (_, day) => 1.2 + 5.2 * Math.exp(-((day - 15) ** 2) / 28) + 2.1 * Math.exp(-((day - 21) ** 2) / 9));
  const counts = distribute(243, weights);
  const rows = [];
  let index = 0;
  for (let day = 0; day < counts.length; day += 1) {
    for (let occurrence = 0; occurrence < counts[day]; occurrence += 1) {
      const run = runs[index % runs.length];
      const artifact = artifacts[index % artifacts.length];
      const boundary = interfaces[(index + 4) % interfaces.length];
      const inbound = index % 2 === 0;
      const timestamp = now - day * DAY_MS - ((occurrence * 71 + index * 17) % 1_200) * 60_000;
      const verifierPassed = index % 11 === 0;
      const verifierFailed = !verifierPassed && index % 29 === 0;
      const mark = ["coder.build.ok", "artifact.admitted", "run.checkpoint.closed"][index % 3];
      rows.push(fixtureRow({
        movement_id: `lab_movement_${String(index + 1).padStart(3, "0")}`,
        seq: 243 - index,
        lamport: 2_000 + index,
        trace: `lab_trace_${Math.floor(index / 3) + 1}`,
        kind: verifierPassed ? `MarkOutcomeAdmitted(${mark})` : verifierFailed ? "VerificationFailed" : inbound ? "InterfaceArrival" : "InterfaceOutbound",
        mark,
        verifier: mark === "coder.build.ok" ? "build_success" : mark === "artifact.admitted" ? "artifact_schema" : "mark_admission",
        recorded_at_unix_ms: timestamp,
        artifact_id: artifact.artifact_id,
        artifact_label: artifact.label,
        content_hash: `fixture${String(index).padStart(58, "0")}`.slice(0, 64),
        blob_path: "",
        blob_size: artifact.size,
        from: inbound
          ? { kind: "interface", id: boundary.rule_id }
          : { kind: "run", id: run.run_id, ee_id: run.ee_id },
        to: inbound
          ? { kind: "run", id: run.run_id, ee_id: run.ee_id }
          : { kind: "interface", id: boundary.rule_id },
        raw_run_id: run.run_id,
        raw_ee_id: run.ee_id,
        raw_worker: run.worker,
      }));
      index += 1;
    }
  }
  return rows.sort((left, right) => right.recorded_at_unix_ms - left.recorded_at_unix_ms);
}

function distribute(total, weights) {
  const sum = weights.reduce((value, weight) => value + weight, 0);
  const exact = weights.map((weight) => (weight / sum) * total);
  const values = exact.map(Math.floor);
  let remaining = total - values.reduce((value, count) => value + count, 0);
  const order = exact.map((value, index) => ({ index, remainder: value - Math.floor(value) })).sort((left, right) => right.remainder - left.remainder);
  for (let index = 0; index < remaining; index += 1) values[order[index].index] += 1;
  return values;
}

function filterTargets(rows, params) {
  return filterString(filterString(filterString(rows, "interface_id", params.interface_id ?? params.interface), "run_id", params.run_id), "status", params.status);
}

function filterString(rows, field, value) {
  const needle = typeof value === "string" ? value.trim().toLowerCase() : "";
  return needle ? rows.filter((row) => String(row[field] || "").toLowerCase() === needle) : rows;
}

function fixtureMovementPage(rows, params) {
  const beforeSeq = optionalSequence(params.before_seq);
  const afterSeq = optionalSequence(params.after_seq);
  const afterTime = optionalSequence(params.recorded_after_unix_ms);
  const beforeTime = optionalSequence(params.recorded_before_unix_ms);
  const query = typeof params.query === "string" ? params.query.trim().toLowerCase() : "";
  const limit = Math.min(1_000, Math.max(1, Number.isFinite(Number(params.limit)) ? Math.floor(Number(params.limit)) : 500));
  const matching = rows.filter((row) => {
    const seq = Number(row.seq);
    const timestamp = Number(row.recorded_at_unix_ms);
    if (beforeSeq !== null && seq >= beforeSeq) return false;
    if (afterSeq !== null && seq <= afterSeq) return false;
    if (afterTime !== null && timestamp < afterTime) return false;
    if (beforeTime !== null && timestamp > beforeTime) return false;
    if (!query) return true;
    return [row.kind, row.trace, row.artifact_id, row.artifact_label, row.from?.id, row.to?.id]
      .some((value) => String(value || "").toLowerCase().includes(query));
  }).sort((left, right) => Number(right.seq) - Number(left.seq));
  const movements = matching.slice(0, limit);
  const oldest = movements.length ? Math.min(...movements.map((row) => Number(row.seq))) : null;
  return {
    movements,
    total: matching.length,
    ...(oldest !== null && movements.length < matching.length ? { next_before_seq: oldest } : {}),
  };
}

function fixtureRunActivityPage(runId, rows, params) {
  const afterSeq = optionalSequence(params.after_seq);
  const beforeSeq = optionalSequence(params.before_seq);
  const fromEnd = params.from_end === true;
  const limit = Math.min(1_000, Math.max(1, Number.isFinite(Number(params.limit)) ? Math.floor(Number(params.limit)) : 100));
  if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
    throw new Error("run activity before_seq must be greater than after_seq");
  }
  const ordered = rows.slice().sort((left, right) => Number(left.movement_seq) - Number(right.movement_seq));
  const matching = ordered.filter((row) => {
    const seq = Number(row.movement_seq);
    return (afterSeq === null || seq > afterSeq) && (beforeSeq === null || seq < beforeSeq);
  });
  const selected = (fromEnd || beforeSeq !== null ? matching.slice(-limit) : matching.slice(0, limit)).map((message) => ({
    ...message,
    movement_kind: message.direction === "in" ? "InterfaceArrival" : "InterfaceOutbound",
    from: { kind: "interface", id: message.direction === "in" ? message.interface_id : message.event_data?._activity_origin?.interface_id || message.interface_id },
    to: { kind: "interface", id: message.event_data?._movement?.to?.Interface || message.interface_id },
    verification: "",
    payload_error: "",
  }));
  const oldestSeq = selected.length ? Number(selected[0].movement_seq) : null;
  const latestSeq = selected.length ? Number(selected[selected.length - 1].movement_seq) : null;
  const allOldest = ordered.length ? Number(ordered[0].movement_seq) : null;
  const allLatest = ordered.length ? Number(ordered[ordered.length - 1].movement_seq) : null;
  return {
    run_id: runId,
    total: matching.length,
    truncated: selected.length < matching.length,
    has_older: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
    has_newer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
    oldest_seq: oldestSeq,
    latest_seq: latestSeq,
    activity: selected,
  };
}

function optionalSequence(value) {
  if (value === undefined || value === null || value === "") return null;
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error("movement cursor and timestamp parameters must be unsigned integers");
  return number;
}

function limitRows(rows, params) {
  const limit = Number(params.limit);
  return Number.isFinite(limit) && limit >= 0 ? rows.slice(0, Math.floor(limit)) : rows.slice();
}

function fixtureRow(value) {
  return { ...value, projection_source: "lab" };
}

function requiredString(value, field) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text) throw new Error(`${field} is required`);
  return text;
}

function fixtureChatSearch(data, params) {
  const query = requiredString(params.query, "query").replace(/\s+/g, " ").trim();
  if (query.length < 2) throw new Error("task transcript search query must contain at least two characters");
  if (!Array.isArray(params.task_run_ids)) throw new Error("task_run_ids must be an array of exact projected task run ids");
  const runIds = [...new Set(params.task_run_ids.map(String))];
  const limit = Math.min(50, Math.max(1, Number(params.limit) || 12));
  const beforeSeq = Number.isSafeInteger(params.before_seq) ? params.before_seq : Number.MAX_SAFE_INTEGER;
  const needle = query.toLocaleLowerCase();
  const candidates = runIds.flatMap((runId) => (data.chatMessages[runId] ?? [])
    .filter((message) => message.event_kind === "message" && typeof message.text === "string" && message.movement_seq < beforeSeq)
    .map((message) => ({ runId, message })))
    .sort((left, right) => right.message.movement_seq - left.message.movement_seq);
  const matches = [];
  let lastSeq = -1;
  for (const { runId, message } of candidates) {
    const text = message.text.replace(/\s+/g, " ").trim();
    const index = text.toLocaleLowerCase().indexOf(needle);
    if (index < 0) continue;
    if (matches.length === limit) {
      return { matches, has_older: true, next_before_seq: lastSeq, index_watermark: fixtureSearchWatermark(data), scope: fixtureSearchScope(runIds) };
    }
    const start = Math.max(0, index - 60);
    const end = Math.min(text.length, index + needle.length + 60);
    matches.push({
      task_run_id: runId,
      source_run_id: runId,
      snippet: `${start > 0 ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}`,
      message,
    });
    lastSeq = message.movement_seq;
  }
  return { matches, has_older: false, next_before_seq: -1, index_watermark: fixtureSearchWatermark(data), scope: fixtureSearchScope(runIds) };
}

function fixtureSearchWatermark(data) {
  return Object.values(data.chatMessages).flat().reduce((latest, message) => Math.max(latest, Number(message.movement_seq) || 0), 0);
}

function fixtureSearchScope(runIds) {
  return {
    kind: "octopus_os_task_transcripts",
    task_count: runIds.length,
    run_count: runIds.length,
    message_classes: ["operator", "terminal_agent", "terminal_error"],
  };
}

module.exports = {
  createFixtureProjectionStore,
};
