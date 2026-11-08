const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const ts = require("typescript");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");

function source(relativePath) {
  return fs.readFileSync(path.join(appRoot, relativePath), "utf8");
}

function sourceBetween(value, startMarker, endMarker) {
  const start = value.indexOf(startMarker);
  assert.notEqual(start, -1, `missing source marker: ${startMarker}`);
  const end = value.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `missing source marker: ${endMarker}`);
  return value.slice(start, end);
}

function typescriptModule(relativePath, mocks = {}) {
  const filename = path.join(appRoot, relativePath);
  const compiled = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
    compilerOptions: { esModuleInterop: true, module: ts.ModuleKind.CommonJS, resolveJsonModule: true, target: ts.ScriptTarget.ES2022 },
    fileName: filename,
  }).outputText;
  const module = { exports: {} };
  const localRequire = createRequire(filename);
  Function("module", "exports", "require", compiled)(module, module.exports, (specifier) => (
    Object.prototype.hasOwnProperty.call(mocks, specifier)
      ? mocks[specifier]
      : localRequire(specifier)
  ));
  return module.exports;
}

const turnActivity = source("src/components/chat/TurnActivity.tsx");
const messageList = source("src/components/chat/ChatMessageList.tsx");
const iconNotices = JSON.parse(source("licenses/icon-assets/notices.json"));
const glyphAsset = (name) => iconNotices.glyphs.find((entry) => entry.name === name);
const toolActivityPresentationCatalog = JSON.parse(source("src/components/chat/toolActivityPresentationCatalog.json"));
const assistantDirectives = typescriptModule("src/components/chat/assistantDirectives.ts");
const toolPresentation = typescriptModule("src/components/chat/toolPresentation.ts");
const reasoningSummary = typescriptModule("src/components/chat/reasoningSummary.ts");
const chatTurnControl = typescriptModule("src/components/chat/chatTurnControl.ts");
const paneChromeOwnership = typescriptModule("src/components/shell/paneChromeOwnership.ts");

assert.equal(
  reasoningSummary.reasoningSummaryHeading("**Gathering full architecture documentation**\n\n**Planning parallel document retrieval**"),
  "Planning parallel document retrieval",
);
assert.equal(reasoningSummary.reasoningSummaryHeading("<!-- partial -->\n### Mapping action blocks"), "Mapping action blocks");
assert.equal(reasoningSummary.reasoningSummaryHeading("<!--"), "");

function turnControlMessage({
  error = false,
  eventData = {},
  eventKind,
  movementSeq,
  role = "assistant",
  terminal = true,
}) {
  return {
    id: `message-${movementSeq}`,
    runId: "run-resume",
    role,
    label: "test",
    direction: role === "user" ? "in" : "out",
    text: "test",
    createdAtMs: movementSeq,
    movementSeq,
    artifactId: `artifact-${movementSeq}`,
    contentHash: "a".repeat(64),
    interfaceId: "world_operator_chat_resume",
    eventKind,
    eventData,
    latencyMs: 0,
    attachments: [],
    error,
    terminal,
  };
}

const successfulTurn = turnControlMessage({ eventKind: "message", movementSeq: 1 });
const failedTurn = turnControlMessage({ error: true, eventKind: "error", movementSeq: 2 });
const interruptedTurn = turnControlMessage({ eventKind: "interrupted_result", movementSeq: 3, eventData: { turn_interrupted: true, native_terminal_status: "interrupted" } });
const failedResume = turnControlMessage({
  error: true,
  eventData: { action: "resume", control_id: "control-failed", status: "failed" },
  eventKind: "turn_control",
  movementSeq: 4,
  terminal: false,
});
const acknowledgedResume = turnControlMessage({
  eventData: { action: "resume", control_id: "control-ack", status: "acknowledged" },
  eventKind: "turn_control",
  movementSeq: 5,
  terminal: false,
});
assert.equal(chatTurnControl.operatorTurnResumeAvailable([successfulTurn], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([failedTurn], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([{ ...interruptedTurn, eventData: { native_terminal_status: "failed", turn_interrupted: false } }], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn], false), true);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([{ ...interruptedTurn, eventData: { native_terminal_status: "completed", turn_interrupted: false } }], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn, failedResume], false), true);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn, acknowledgedResume], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([failedTurn], true), false);
assert.equal(chatTurnControl.operatorTurnResumeAcknowledgedOpen([interruptedTurn, acknowledgedResume]), true);
assert.equal(chatTurnControl.operatorTurnResumeAcknowledgedOpen([
  interruptedTurn,
  acknowledgedResume,
  { ...failedTurn, movementSeq: 6, createdAtMs: 6 },
]), false);
const chatActivityProjectionModule = typescriptModule("src/components/chat/chatActivityProjection.ts", {
  "../../chatRuns": { normalizeChatRunEventProjection: (eventKind, eventData, text) => ({ eventData, eventKind, text }) },
  "./toolPresentation": { clientToolActivityPresentation: () => null },
});
const conversationTraceModule = typescriptModule("src/components/chat/conversationTrace.ts", {
  "./toolPresentation": { dynamicToolPresentation: () => ({ conversationVisible: true }) },
});
let chatRunsProjection = {};
const chatRunsModule = typescriptModule("src/chatRuns.ts", { "./projectionClient": { formatProjectionError: String, readProjection: async () => chatRunsProjection },
  "./components/chat/taskAccess": typescriptModule("src/components/chat/taskAccess.ts") });
const threadVirtualizer = typescriptModule("src/components/chat/threadVirtualizer.ts");
const clientWindowModel = typescriptModule("src/shell/clientWindowModel.ts", {
  "../components/files/artifactFilesTarget": typescriptModule("src/components/files/artifactFilesTarget.ts"),
  "../components/files/filesPaneSnapshot": typescriptModule("src/components/files/filesPaneSnapshot.ts", {
    "./artifactFilesTarget": typescriptModule("src/components/files/artifactFilesTarget.ts"),
  }),
});
const {
  assertOperatorChatArtifactBatch,
  requireMaterializedOperatorChatClientOwner,
} = require("../electron/operatorChatClientContract.cjs");

const previousThreadLayout = threadVirtualizer.buildThreadVirtualizerLayout({
  entries: [{ turnKey: "turn-a" }, { turnKey: "turn-b" }],
  gapPx: 10,
  measuredHeightsByKey: { "turn-a": 100, "turn-b": 100 },
});
const nextThreadLayout = threadVirtualizer.buildThreadVirtualizerLayout({
  entries: [{ turnKey: "turn-a" }, { turnKey: "turn-b" }, { turnKey: "turn-c" }],
  gapPx: 10,
  measuredHeightsByKey: { "turn-a": 100, "turn-b": 100, "turn-c": 50 },
});
assert.equal(threadVirtualizer.distanceFromBottomAnchoredToTurn({
  anchorKey: "turn-a",
  distanceFromBottomPx: 40,
  nextLayout: nextThreadLayout,
  previousLayout: previousThreadLayout,
}), 100);

const primaryShellSelection = { runId: "", surfaceId: "voice", workspaceKey: "octopus-os" };
const secondaryChatTarget = {
  kind: "surface",
  runId: "run-secondary",
  surfaceId: "chat",
  workspaceKey: "octopus-os",
};
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "primary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-secondary",
    },
  }),
  { ...primaryShellSelection, pane: "primary", ready: true },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-secondary",
    },
  }),
  { runId: "run-secondary", surfaceId: "chat", workspaceKey: "octopus-os", pane: "secondary", ready: true },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-workspace-ghost",
    },
  }),
  { runId: "", surfaceId: "chat", workspaceKey: "octopus-os", pane: "secondary", ready: false },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: { kind: "files", workspaceKey: "dashboard" },
      workspaceActiveRunId: "run-ignored",
    },
  }),
  { runId: "", surfaceId: "", workspaceKey: "dashboard", pane: "secondary", ready: true },
);
const taskSourcesTab = clientWindowModel.workspaceTaskSourcesPaneTab("octopus-os", {
  runId: "run-sources",
  schemaVersion: 1,
  sources: [],
});
assert.equal(taskSourcesTab.id, "task-sources:run-sources");
assert.equal(clientWindowModel.transferableWorkspacePaneTarget(taskSourcesTab.target), null);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    { x: 899, y: 799 },
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  false,
);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    { x: 900, y: 400 },
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  true,
);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    null,
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  false,
);
assert.equal(clientWindowModel.clientWindowChatStorageScope("main", "secondary"), "secondary");
assert.equal(clientWindowModel.clientWindowChatStorageScope("window-a", "secondary"), "w_window-a_secondary");
assert.match(
  clientWindowModel.clientWindowChatStorageScope("window-12345678-1234-1234-1234-123456789abc", "secondary"),
  /^[A-Za-z][A-Za-z0-9_-]{0,31}$/,
);
assert.equal(paneChromeOwnership.paneChromeOwner("primary", []), "window-titlebar");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", []), "pane-launcher");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{}]), "pane-content-header");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{ showWhenSingle: true }]), "pane-tab-strip");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{}, {}]), "pane-tab-strip");
{
  const saved = new Map();
  const previousWindow = globalThis.window;
  globalThis.window = { localStorage: { getItem: (key) => saved.get(key) ?? null, setItem: (key, value) => saved.set(key, String(value)) } };
  try {
    const storage = typescriptModule("src/components/chat/storage.ts", {
      "./constants": typescriptModule("src/components/chat/constants.ts"), "./taskAccess": typescriptModule("src/components/chat/taskAccess.ts"),
    });
    const draft = (key, messageId, messageHadAttachments) => ({ key, messageId, messageHadAttachments, text: "", agentPresetId: "", eeId: "", workerInterfaceId: "",
      providerInterfaceId: "", providerModel: "", reasoningEffort: "", applicationIds: [], driverNames: [], accessMode: "default", continuation: null, updatedAtMs: 1 });
    storage.saveChatDrafts("live", [draft("exact", "msg_exact", true), draft("forged", "not-a-message-id", true)]);
    assert.ok([...saved.keys()].some((key) => key.includes("chatDrafts")));
    assert.deepEqual(storage.loadChatDrafts("live").map(({ key, messageId, messageHadAttachments }) => [key, messageId, messageHadAttachments]), [["exact", "msg_exact", true]]);
  } finally {
    globalThis.window = previousWindow;
  }
}
assert.equal(clientWindowModel.clientWindowStateKey("main", "surface"), "surface");
assert.equal(clientWindowModel.clientWindowStateKey("window-a", "surface"), "octopus.client-window.window-a.surface");
const paneDrag = {
  schema: "octopus.client-pane-transfer",
  schemaVersion: 1,
  sourceWindowId: "window-a",
  transferId: "transfer-a",
};
assert.deepEqual(
  clientWindowModel.parseClientPaneDragPayload(JSON.stringify(paneDrag)),
  paneDrag,
);
assert.equal(clientWindowModel.parseClientPaneDragPayload(JSON.stringify({ ...paneDrag, transferId: "bad\ntransfer" })), null);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({
  kind: "terminal",
  terminalId: "terminal-a",
  title: "Terminal",
  workspaceKey: "dashboard",
}), null);
const filesSnapshot = {
  version: 1, id: "editor-a", revision: 7, rootId: "", rootPath: "",
  file: null, draft: "", view: { scrollTop: 24, scrollLeft: 0 },
};
const transferredFiles = { kind: "files", workspaceKey: "octopus-os", snapshot: filesSnapshot };
assert.deepEqual(clientWindowModel.normalizeTransferableWorkspacePaneTarget(transferredFiles), transferredFiles);
assert.equal(clientWindowModel.workspacePaneTabForTarget(transferredFiles).id, "files:editor-a");
assert.notEqual(clientWindowModel.workspacePaneTabForTarget(transferredFiles).id, clientWindowModel.workspacePaneTabForTarget({ kind: "files", workspaceKey: "octopus-os" }).id);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({ ...transferredFiles, snapshot: { ...filesSnapshot, revision: -1 } }), null);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({
  kind: "surface",
  runId: "ignored",
  surfaceId: "drivers",
  workspaceKey: "octopus-os",
}).runId, "");

const exactPackageDigest = "a".repeat(64);
assert.deepEqual(requireMaterializedOperatorChatClientOwner({
  packages: [{
    id: "world_operator_chat",
    available: true,
    installed: false,
    materialized: false,
    partially_materialized: true,
    receipt_state: "missing",
    member_states: [
      {
        registry_kind: "interface",
        owner: "world_operator_chat",
        materialized: true,
        source_digest: exactPackageDigest,
        installed_digest: exactPackageDigest,
      },
      {
        registry_kind: "interface",
        owner: "unrelated_package_member",
        materialized: false,
        source_digest: exactPackageDigest,
        installed_digest: "",
      },
    ],
  }],
}), {
  installedDigest: exactPackageDigest,
  owner: "world_operator_chat",
  registryKind: "interface",
  sourceDigest: exactPackageDigest,
});
assert.throws(
  () => requireMaterializedOperatorChatClientOwner({ packages: [] }),
  (error) => error?.code === "operator_chat_client_contract_mismatch",
);
assert.doesNotThrow(
  () => requireMaterializedOperatorChatClientOwner({
    packages: [{
      id: "world_operator_chat",
      available: true,
      member_states: [{
        registry_kind: "interface",
        owner: "world_operator_chat",
        materialized: true,
        source_digest: exactPackageDigest,
        installed_digest: "b".repeat(64),
      }],
    }],
  }),
  "source equality is not a client contract",
);
assert.throws(
  () => assertOperatorChatArtifactBatch({
    artifacts: [{ from_interface_id: "world_provider_codex_exact", from_interface_role: "" }],
    cursor: { offset: 0, seq: null },
  }),
  (error) => error?.code === "operator_chat_client_contract_mismatch",
);

const directiveProjection = assistantDirectives.projectAssistantDirectives([
  "Answer body.",
  '::code-comment{title="[P1] Bound path" body="Keep the exact room root." file="/repo/src/a.ts" start=4 end=6 priority=P1}',
  '::codex-inline-vis{file="reports/result.html"}',
].join("\n"));
assert.equal(directiveProjection.markdown, "Answer body.");
assert.deepEqual(directiveProjection.codeComments.map(({ directiveIndex, ...comment }) => comment), [{
  body: "Keep the exact room root.",
  endLine: 6,
  path: "/repo/src/a.ts",
  priority: 1,
  startLine: 4,
  title: "[P1] Bound path",
}]);
assert.deepEqual(directiveProjection.inlineVisualizations, [{ directiveIndex: 1, path: "reports/result.html" }]);
assert.equal(assistantDirectives.directivePathWithinRoot("/repo/reports/result.html", "/repo"), "reports/result.html");
assert.equal(assistantDirectives.directivePathWithinRoot("../outside.html", "/repo"), null);
const literalDirective = "```text\n::code-comment{title=literal}\n```\n::code-comment{title=malformed}";
assert.equal(assistantDirectives.projectAssistantDirectives(literalDirective).markdown, literalDirective);
const projectedCodeComments = assistantDirectives.projectAssistantCodeComments([
  {
    id: "assistant-a",
    role: "assistant",
    runId: "run-a",
    text: 'Answer.\n::code-comment{title="Bound path" body="Keep the exact root." file="src/a.ts" start=4}',
    createdAtMs: 1,
    terminal: true,
  },
  {
    id: "assistant-a-duplicate",
    role: "assistant",
    runId: "run-a",
    text: '::code-comment{title="Bound path" body="Keep the exact root." file="src/a.ts" start=4}',
    createdAtMs: 2,
    terminal: true,
  },
  {
    id: "assistant-other-run",
    role: "assistant",
    runId: "run-b",
    text: '::code-comment{title="Wrong run" body="Do not project." file="src/b.ts"}',
    createdAtMs: 3,
    terminal: true,
  },
], "run-a");
assert.deepEqual(projectedCodeComments.map(({ id, sourceMessageId, title }) => ({ id, sourceMessageId, title })), [{
  id: "assistant-a:code-comment:0",
  sourceMessageId: "assistant-a",
  title: "Bound path",
}]);

const registryToolIds = fs.readdirSync(path.join(repoRoot, "registry", "tool"))
  .map((entry) => path.join(repoRoot, "registry", "tool", entry, "tool.json"))
  .filter((descriptorPath) => fs.existsSync(descriptorPath))
  .map((descriptorPath) => JSON.parse(fs.readFileSync(descriptorPath, "utf8")))
  .map((descriptor) => descriptor.id)
  .filter((toolId) => typeof toolId === "string")
  .sort();
assert.deepEqual(
  Object.keys(toolActivityPresentationCatalog).sort(),
  registryToolIds,
  "every registry tool must have an exact conversation presentation",
);
assert.equal(toolActivityPresentationCatalog["file.read"].kind, "read");
assert.equal(toolActivityPresentationCatalog["file.list"].kind, "list");
assert.deepEqual(toolActivityPresentationCatalog["workspace.read_file"].target_arguments, ["path"]);
assert.deepEqual(toolActivityPresentationCatalog["workspace.exec"].target_arguments, ["command"]);
assert.deepEqual(toolActivityPresentationCatalog["workspace.grep"].query_arguments, ["query"]);
assert.equal(toolActivityPresentationCatalog["workspace.apply_patch"].kind, "file_change");
const workspaceToolSuffix = "ffc1f8a5d06b5d4c8f5df13b40f1aeab";
const projectedWorkspaceRead = toolPresentation.dynamicToolPresentation({
  eventData: {
    arguments: { path: "src/life.rs" },
    tool: `workspace.read_file__${workspaceToolSuffix}`,
  },
}, {});
const projectedWorkspaceExec = toolPresentation.dynamicToolPresentation({
  eventData: {
    arguments: { command: "git status --short" },
    tool: `workspace.exec__${workspaceToolSuffix}`,
  },
}, {});
const projectedRoomRead = toolPresentation.dynamicToolPresentation({
  eventData: {
    arguments: { path: "/home/registry/interface/world_operator_chat/activation.json" },
    namespace: "formed_interfaces",
    tool: `file.read__${workspaceToolSuffix}`,
  },
}, {});
assert.equal(toolPresentation.dynamicToolReadLabel(projectedRoomRead, false), "Read activation.json");
assert.equal(projectedWorkspaceRead.family, "read");
assert.equal(toolPresentation.dynamicToolReadLabel(projectedWorkspaceRead, false), "Read life.rs");
assert.equal(projectedWorkspaceExec.family, "exec");
assert.equal(projectedWorkspaceExec.target, "git status --short");
const projectedBrowserTabs = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `browser.tabs__${workspaceToolSuffix}` },
}, {});
const projectedContainerStatus = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `container.status__${workspaceToolSuffix}` },
}, {});
assert.equal(toolPresentation.dynamicToolOperationLabel(projectedBrowserTabs), "Used the browser");
assert.equal(toolPresentation.dynamicToolOperationLabel(projectedContainerStatus), "Container status");
assert.equal(toolPresentation.dynamicToolGlyph(projectedBrowserTabs), "browserUse");
assert.equal(toolPresentation.dynamicToolGlyph(projectedWorkspaceRead), "read");
assert.equal(toolPresentation.dynamicToolGlyph(projectedWorkspaceExec), "terminal");
const namedChatSuffix = `system_review_${workspaceToolSuffix}`;
const projectedNamedExec = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: { command: "git status --short" }, tool: `workspace.exec__${namedChatSuffix}` },
}, {});
const projectedAuthoringWrite = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: { path: "run.json" }, tool: `file.write__authoring_${namedChatSuffix}` },
}, {});
const projectedOctopusMcp = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `client.mcp__${namedChatSuffix}` },
}, {});
const projectedIntegrationCall = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `mcp.call_tool__${namedChatSuffix}` },
}, {});
assert.equal(projectedNamedExec.family, "exec");
assert.equal(projectedNamedExec.target, "git status --short");
assert.equal(projectedAuthoringWrite.conversationVisible, false);
assert.equal(toolPresentation.dynamicToolGlyph(projectedOctopusMcp), "plugins");
assert.equal(toolPresentation.dynamicToolGlyph(projectedIntegrationCall), "plugins");

const exactProviderLiveTool = chatActivityProjectionModule.liveArtifactMessage({
  source: "movement",
  movement_seq: 10,
  recorded_at_unix_ms: 1_700_000_000_000,
  run_id: "run_exact",
  interface_id: "world_operator_chat_exact",
  checkpoint_id: "cp_provider_output",
  from_interface_id: "world_provider_codex_exact",
  from_interface_role: "provider",
  artifact_id: "turn.dynamic_tool_call:exact",
  content_hash: "a".repeat(64),
  label: "turn.dynamic_tool_call",
  event_kind: "dynamic_tool",
  event_data: { call_id: "call_exact", status: "started", tool: "workspace.read_file__exact" },
  text: "workspace.read_file__exact",
  error: false,
  terminal: false,
  will_retry: false,
}, new Map());
assert.equal(exactProviderLiveTool.eventData._activity_origin.interface_role, "provider");
assert.equal(exactProviderLiveTool.eventData._movement.seq, 10);
assert.equal(exactProviderLiveTool.eventData._stream, undefined);
assert.equal(conversationTraceModule.conversationTraceAdmission(exactProviderLiveTool, {}), "visible");
const exactProviderLiveStream = chatActivityProjectionModule.liveArtifactMessage({
  source: "stream",
  stream_id: "b".repeat(64),
  stream_seq: 7,
  byte_start_offset: 100,
  byte_end_offset: 180,
  source_movement_seq: 10,
  recorded_at_unix_ms: 1_700_000_000_007,
  run_id: "run_exact",
  interface_id: "world_operator_chat_exact",
  from_interface_id: "world_provider_codex_exact",
  from_interface_role: "provider",
  label: "turn.reasoning.delta",
  event_kind: "reasoning_delta",
  event_data: { delta: "proof", _movement: { seq: 999 } },
  text: "proof",
  error: false,
  terminal: false,
  will_retry: false,
}, new Map());
assert.equal(exactProviderLiveStream.eventData._movement, undefined);
assert.equal(exactProviderLiveStream.eventData._stream.stream_seq, 7);
assert.equal(exactProviderLiveStream.eventData._stream.source_movement_seq, 10);
assert.equal(exactProviderLiveStream.artifactId, "");
assert.equal(exactProviderLiveStream.contentHash, "");
const durableBetweenStreamRows = {
  ...exactProviderLiveTool,
  id: "durable-between-stream-rows",
  createdAtMs: 1_700_000_000_005,
  movementSeq: 500,
};
assert.deepEqual(
  chatActivityProjectionModule.mergeChatRunMessages(
    [],
    [exactProviderLiveStream, durableBetweenStreamRows],
  ).map((message) => message.id),
  ["durable-between-stream-rows", exactProviderLiveStream.id],
);
assert.doesNotThrow(() => assertOperatorChatArtifactBatch({
  artifacts: [{
    source: "stream",
    stream_id: "b".repeat(64),
    stream_seq: 7,
    byte_start_offset: 100,
    byte_end_offset: 180,
    source_movement_seq: 10,
    recorded_at_unix_ms: 1_700_000_000_007,
    run_id: "run_exact",
    interface_id: "world_operator_chat_exact",
    from_interface_id: "world_provider_codex_exact",
    from_interface_role: "provider",
    label: "turn.reasoning.delta",
    event_kind: "reasoning_delta",
    event_data: {},
    text: "proof",
    error: false,
    terminal: false,
    will_retry: false,
  }],
  cursor: { offset: 0, seq: null },
}));

const duplicateProjection = Array.from({ length: 2_000 }, (_, index) => ({
  id: `projected-${index}`,
  role: "assistant",
  text: `fragment ${index}`,
  createdAtMs: index,
  eventKind: index % 7 === 0 ? "message" : "reasoning_delta",
}));
const deduplicatedProjection = chatActivityProjectionModule.mergeRenderedMessages(
  duplicateProjection,
  duplicateProjection.map((message) => ({ ...message })),
  [],
  true,
);
assert.equal(deduplicatedProjection.length, duplicateProjection.length);
assert.deepEqual(deduplicatedProjection.map((message) => message.id), duplicateProjection.map((message) => message.id));
const repeatedInput = { id: "repeat-before", runId: "home", interfaceId: "home-client", role: "user", text: "hi!", createdAtMs: 1, eventKind: "message" };
const repeatedPending = { ...repeatedInput, id: "repeat-now", createdAtMs: 3, pending: true };
assert.deepEqual(chatActivityProjectionModule.mergeRenderedMessages([repeatedInput], [repeatedPending], [], true)
  .map(message => message.id), ["repeat-before", "repeat-now"]);
assert.deepEqual(chatActivityProjectionModule.mergeRenderedMessages([repeatedPending], [repeatedPending], [], true)
  .map(message => message.id), ["repeat-now"]);
const pendingSteer = Object.freeze({ id: "steer-message", runId: "run-a", role: "user", text: "exact steering text",
  createdAtMs: 1, eventKind: "steering", pending: true,
  eventData: Object.freeze({ steering_status: "submitted", steering_provider_interface: "provider-exact" }) });
const steerReceipt = (status, sequence, extra = {}) => chatActivityProjectionModule.toRender(chatActivityProjectionModule.liveArtifactMessage({
  source: "movement", run_id: "run-a", artifact_id: `steer-receipt-${sequence}`, movement_seq: sequence,
  recorded_at_unix_ms: sequence, interface_id: "operator-chat", from_interface_id: "provider-exact", from_interface_role: "provider",
  label: `turn.control.${status}`, event_kind: "turn_control", text: "", terminal: false, error: status === "failed",
  event_data: { action: "steer", control_id: pendingSteer.id, status }, ...extra,
}, new Map()));
const mergedSteer = (receipts, message = pendingSteer) => chatActivityProjectionModule.mergeRenderedMessages(receipts, [], [message], false)
  .find((message) => message.id === pendingSteer.id);
assert.equal(mergedSteer([]), pendingSteer);
const acceptedSteer = mergedSteer([steerReceipt("acknowledged", 12)]);
assert.equal(acceptedSteer.eventData.steering_status, "acknowledged");
assert.equal(acceptedSteer.pending, false);
assert.equal(acceptedSteer.text, pendingSteer.text);
assert.equal(pendingSteer.pending, true);
for (const receipt of [
  steerReceipt("acknowledged", 12, { run_id: "run-b" }),
  steerReceipt("acknowledged", 12, { event_data: { action: "steer", control_id: "another-message", status: "acknowledged" } }),
  steerReceipt("acknowledged", 12, { event_data: { action: "stop", control_id: pendingSteer.id, status: "acknowledged" } }),
  steerReceipt("acknowledged", 12, { artifact_id: "" }),
  steerReceipt("acknowledged", 0),
  { id: "ordinary-completion", runId: "run-a", eventKind: "message", role: "assistant", text: "Done", terminal: true, createdAtMs: 12 },
]) assert.equal(mergedSteer([receipt]), pendingSteer);
const refusedSteer = mergedSteer([steerReceipt("failed", 14), steerReceipt("acknowledged", 12)]);
assert.equal(refusedSteer.eventData.steering_status, "failed");
assert.equal(mergedSteer([steerReceipt("acknowledged", 12)], refusedSteer), refusedSteer);
assert.equal(mergedSteer([steerReceipt("acknowledged", 15)], refusedSteer).eventData.steering_status, "acknowledged");
const inputRecorded = (extra = {}) => chatActivityProjectionModule.toRender(chatActivityProjectionModule.liveArtifactMessage({
  source: "stream", run_id: "run-a", stream_id: "a".repeat(64), stream_seq: 2,
  byte_start_offset: 200, byte_end_offset: 400, source_movement_seq: 10, recorded_at_unix_ms: 13,
  interface_id: "operator-chat", from_interface_id: "provider-exact", from_interface_role: "provider",
  label: "turn.stream", event_kind: "input_recorded", text: "", terminal: false, error: false,
  event_data: { type: "input_recorded", stage: "input_recorded_in_turn", seq: 2,
    provider_interface: "provider-exact", run: "run-a", thread_id: "thread-a", turn_id: "turn-a",
    item_id: "native-item", client_id: pendingSteer.id, source_event: "item/started", native_event_at_ms: 12, ...extra },
}, new Map()));
const recordedSteer = mergedSteer([inputRecorded()]);
assert.equal(recordedSteer.eventData.steering_status, "input_recorded");
assert.equal(recordedSteer.text, pendingSteer.text);
for (const status of ["acknowledged", "failed"]) {
  assert.equal(mergedSteer([steerReceipt(status, 15)], recordedSteer), recordedSteer);
  assert.equal(mergedSteer([steerReceipt(status, 15), inputRecorded()]).eventData.steering_status, "input_recorded");
}
for (const extra of [{ client_id: "another" }, { run: "run-b" }, { provider_interface: "foreign-provider" },
  { turn_id: "" }, { stage: "accepted" }, { source_event: "turn/steer" }, { native_event_at_ms: null }, { seq: 0 }]) {
  assert.equal(mergedSteer([inputRecorded(extra)]), pendingSteer);
}
assert.equal(chatActivityProjectionModule.mergeChatRunMessages(
  [inputRecorded()], [steerReceipt("acknowledged", 15)], "snapshot",
).some((message) => message.eventKind === "input_recorded"), true);
const displayEdit = { provider_interface: "provider", turn_id: "turn", item_id: "item", original_text: "original", text: "edited", edited_at_ms: 1 };
const editableReply = { id: "reply", runId: "run", interfaceId: "chat", role: "assistant", text: "edited", eventKind: "message", terminal: true, movementSeq: 10, createdAtMs: 10,
  eventData: { turn_id: "turn", item_id: "item", _activity_origin: { interface_id: "provider" }, _client_text_edit: displayEdit } };
const replayedReply = { ...editableReply, id: "replayed", text: "original", movementSeq: 20, eventData: { ...editableReply.eventData } };
delete replayedReply.eventData._client_text_edit;
const editedReplay = chatActivityProjectionModule.mergeChatRunMessages([editableReply], [replayedReply]);
assert(editedReplay.every(row => row.text === "edited"));
assert.equal(chatActivityProjectionModule.mergeRenderedMessages(editedReplay.map(chatActivityProjectionModule.toRender), [], [], false)[0].text, "edited");
assert.equal(chatActivityProjectionModule.mergeChatRunMessages(editedReplay, [replayedReply], "snapshot").at(-1).text, "original");
assert.equal(chatActivityProjectionModule.mergeChatRunMessages([editableReply], [{ ...replayedReply, runId: "other" }]).at(-1).text, "original");
const coalescedProjection = chatActivityProjectionModule.mergeRenderedMessages([
  {
    id: "delta-1",
    role: "assistant",
    text: "first ",
    createdAtMs: 1,
    eventKind: "reasoning_delta",
    eventData: { item_id: "reasoning-1", lifecycle: "streaming", summary_index: 0, turn_id: "turn-1" },
  },
  {
    id: "delta-2",
    role: "assistant",
    text: "second",
    createdAtMs: 2,
    eventKind: "reasoning_delta",
    eventData: { item_id: "reasoning-1", lifecycle: "completed", summary_index: 0, turn_id: "turn-1" },
  },
], [], [], false);
assert.equal(coalescedProjection.length, 1);
assert.equal(coalescedProjection[0].text, "first second");
assert.equal(coalescedProjection[0].eventData.lifecycle, "completed");
const streamDelta = (seq, text) => ({ id: `${"c".repeat(64)}:${seq}`, runId: "run-a", interfaceId: "chat", role: "assistant",
  createdAtMs: 100 + seq, eventKind: "message_delta", terminal: false, text,
  eventData: { turn_id: "turn-a", item_id: "answer", phase: "final_answer", lifecycle: "streaming",
    _activity_origin: { interface_id: "provider", interface_role: "provider" },
    _stream: { stream_id: "c".repeat(64), stream_seq: seq } } });
const liveDeltas = [streamDelta(3, "Hello"), streamDelta(4, " there,"), streamDelta(5, " friend.")];
const sealedFold = { ...liveDeltas[0], text: "Hello there, friend.", eventData: liveDeltas[2].eventData };
const mergeHistory = chatActivityProjectionModule.mergeChatRunMessages;
{
  const old = { id: "old-edit", runId: "edited", role: "user", artifactId: "old", contentHash: "01".repeat(32), createdAtMs: 1, movementSeq: 1 };
  const answer = { ...old, id: "old-answer", role: "assistant", artifactId: "answer", createdAtMs: 2, movementSeq: 2 };
  const edited = { ...old, id: "edit", artifactId: "edit", contentHash: "02".repeat(32), createdAtMs: 3, movementSeq: 3 };
  const receipt = { ...answer, id: "edit-receipt", label: "interface.context.updated", artifactId: "receipt", createdAtMs: 4, movementSeq: 4,
    eventData: { _activity_origin: { interface_role: "provider" }, phase: "message_revised", replaces_message: { id: "old", content: Array(32).fill(1) }, source_artifact: { id: "edit", content: Array(32).fill(2) } } };
  const revised = mergeHistory([old, answer, edited], [receipt]);
  assert.deepEqual(revised.map(row => row.id), ["edit", "edit-receipt"]);
  assert.deepEqual(mergeHistory(revised, [old, answer], "snapshot").map(row => row.id), ["edit", "edit-receipt"]);
}

const renderHistory = rows => chatActivityProjectionModule.mergeRenderedMessages(rows, [], [], false);
const finalSnapshot = { ...sealedFold, eventKind: "message", terminal: true, movementSeq: 20,
  eventData: { ...sealedFold.eventData, lifecycle: "completed", _stream: undefined } };
const completedItemReplay = { ...finalSnapshot, id: "durable-item-completed", terminal: false,
  createdAtMs: 110, movementSeq: 10 };
assert.equal(renderHistory(mergeHistory([finalSnapshot], [completedItemReplay]))[0].terminal, true,
  "replaying item completion under another row id must not revoke the final answer");
for (const snapshot of [sealedFold, { ...sealedFold, eventKind: "message", terminal: true, movementSeq: 20,
  eventData: { ...sealedFold.eventData, lifecycle: "completed", _stream: undefined } }]) {
  let replayed = [snapshot];
  for (const delta of liveDeltas) {
    replayed = mergeHistory(replayed, [{ ...delta, movementSeq: 10 }]);
    assert.equal(renderHistory(replayed)[0].text, snapshot.text, "each replay batch must preserve the loaded answer");
    assert.equal(renderHistory(replayed)[0].terminal, snapshot.terminal, "replay must not reopen a completed answer");
  }
}
assert.equal(renderHistory(mergeHistory([sealedFold], [streamDelta(7, " Bye.")]))[0].text, "Hello there, friend. Bye.");

for (const [projected, pending] of [
  [[sealedFold], liveDeltas],
  [[sealedFold, ...liveDeltas.slice(1)], liveDeltas],
  [[], [...liveDeltas, ...liveDeltas.slice(1)]],
]) {
  const rendered = chatActivityProjectionModule.mergeRenderedMessages(projected, pending, [], true);
  assert.deepEqual(rendered.map((message) => message.text), ["Hello there, friend."]);
}
assert.equal(chatActivityProjectionModule.mergeRenderedMessages([sealedFold], [...liveDeltas, streamDelta(7, " Bye.")], [], true)[0].text,
  "Hello there, friend. Bye.");
const assistantDelta = { id: "delta", runId: "run-a", interfaceId: "chat", role: "assistant", createdAtMs: 1,
  eventKind: "message_delta", terminal: false, text: "Progress", eventData: { turn_id: "turn-a", item_id: "item-a", phase: null, lifecycle: "streaming" } };
const assistantCompleted = { ...assistantDelta, id: "completed", createdAtMs: 3, eventKind: "commentary",
  eventData: { ...assistantDelta.eventData, phase: "commentary", lifecycle: "completed" } };
const reconcile = (messages) => chatActivityProjectionModule.mergeRenderedMessages(messages, [], [], false);
const reconciledItems = reconcile([assistantDelta, { id: "tool", role: "assistant", eventKind: "tool", text: "read", createdAtMs: 2 }, assistantCompleted]);
assert.deepEqual(reconciledItems.map(({ text, eventKind }) => [text, eventKind]), [["Progress", "commentary"], ["read", "tool"]]);
assert.equal(reconciledItems[0].id, reconcile([assistantDelta])[0].id);
assert.equal(reconciledItems[0].createdAtMs, 1);
assert.equal(reconcile([assistantDelta, assistantCompleted, { ...assistantDelta, id: "late", createdAtMs: 4 }])[0].text, "Progress");
for (const different of [{ runId: "run-b" }, { interfaceId: "another-chat" }, { eventData: { ...assistantDelta.eventData, item_id: "item-b" } }]) {
  assert.equal(reconcile([assistantDelta, { ...assistantDelta, ...different, id: "independent" }]).length, 2);
}
assert.equal(conversationTraceModule.isFinalAssistantResponse(assistantDelta), false);
assert.equal(conversationTraceModule.isFinalAssistantResponse(assistantCompleted), false);
assert.equal(conversationTraceModule.isFinalAssistantResponse({ ...assistantDelta, eventData: { ...assistantDelta.eventData, phase: "final_answer" } }), true);
assert.equal(conversationTraceModule.isFinalAssistantResponse({ ...assistantCompleted, eventKind: "message", terminal: true, eventData: { phase: "partial_answer" } }), false);
const anchoredProjection = chatActivityProjectionModule.mergeRenderedMessages([
  { id: "operator", role: "user", text: "prompt", createdAtMs: 1 },
], [
  { id: "activity-a", role: "assistant", text: "a", createdAtMs: 2, eventData: { anchor_message_id: "operator" } },
  { id: "activity-b", role: "assistant", text: "b", createdAtMs: 3, eventData: { anchor_message_id: "operator" } },
], [], true);
assert.deepEqual(anchoredProjection.map((message) => message.id), ["operator", "activity-a", "activity-b"]);
const mergeCost = (size) => {
  const projected = Array.from({ length: size }, (_, index) => ({ id: `operator-${index}`, role: "user", text: "prompt", createdAtMs: index * 3 }));
  const pending = Array.from({ length: size }, (_, index) => ({ id: `activity-${index}`, role: "assistant", text: "a", createdAtMs: index * 3 + 1,
    eventKind: "reasoning_delta", eventData: { anchor_message_id: `operator-${index % 64}`, item_id: `item-${index % 512}`, turn_id: "turn", lifecycle: "streaming" } }));
  let best = Infinity;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const started = process.hrtime.bigint();
    chatActivityProjectionModule.mergeRenderedMessages(projected, pending, [], true);
    best = Math.min(best, Number(process.hrtime.bigint() - started));
  }
  return best;
};
mergeCost(500);
assert.ok(mergeCost(8000) < mergeCost(2000) * 10, "rendered message merge must stay near-linear in projected and pending history");

for (const eventKind of ["turn_control", "interrupted_result"]) {
  assert.equal(conversationTraceModule.conversationTraceAdmission({ eventKind, eventData: {}, text: "" }, {}), "state");
  assert.equal(chatRunsModule.normalizeChatRunEventProjection(eventKind, {}, "").eventKind, eventKind);
}
assert.equal(conversationTraceModule.conversationTraceAdmission({ ...exactProviderLiveTool,
  eventData: { ...exactProviderLiveTool.eventData, _activity_origin: { interface_id: "system_room", interface_role: "system" } } }, {}), "hidden");
chatRunsProjection = { messages: [[3, 20, "late"], [9, 10, "earliest"], [2, 20, "tied earlier"]].map(([seq, time, id]) => ({ id, movement_seq: seq, created_at_ms: time, event_kind: "message" })) };
chatRunsModule.loadChatRunMessages("run").then((messages) => assert.deepEqual(messages.map((message) => message.id), ["earliest", "tied earlier", "late"]));
for (const [eventKind, field] of [["message_delta", "text"], ["commentary_delta", "text"], ["reasoning_delta", "delta"], ["plan_delta", "delta"], ["tool_output_delta", "delta"], ["file_change_delta", "delta"]]) {
  assert.equal(chatRunsModule.normalizeChatRunEventProjection(eventKind, { [field]: "fragment" }, "composed", "fragment").text, "fragment");
  assert.equal(chatRunsModule.normalizeChatRunEventProjection(eventKind, { [field]: "fragment" }, "composed").text, "composed");
  assert.equal(chatRunsModule.normalizeChatRunEventProjection(eventKind, {}, "composed").eventKind, "generic_activity");
}





assert.deepEqual(glyphAsset("users").shapes, ["users"]);


async function verifyTranscriptLayout() {
  const React = require("react"), { JSDOM } = require("jsdom"), { createRoot } = require("react-dom/client");
  const dom = new JSDOM('<!doctype html><div id="root"></div>');
  const previousNavigator = Object.getOwnPropertyDescriptor(global, "navigator");
  Object.defineProperty(global, "navigator", { configurable: true, value: dom.window.navigator });
  Object.assign(global, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  const frames = new Map(), completions = [], artifactReads = [], artifactErrors = [], filesOpened = [];
  let reducedMotion = true;
  let artifactResponse;
  let frameId = 0;
  window.requestAnimationFrame = (callback) => (frames.set(++frameId, callback), frameId);
  window.cancelAnimationFrame = (id) => frames.delete(id);
  const scope = {
    Element: dom.window.Element,
    ...React, ...toolPresentation, ...reasoningSummary, ...conversationTraceModule,
    ...typescriptModule("src/components/chat/subagentActivity.ts"), ...typescriptModule("src/components/chat/commandActivityModel.ts"),
    useShouldReduceMotion: () => reducedMotion,
    Glyph: ({ name }) => React.createElement("span", { "data-glyph": name }),
    ShimmerActivityScope: ({ children }) => children,
    CadencedShimmerText: ({ children, className, active }) => React.createElement("span", { className, "data-active": Boolean(active) }, children),
    MessageContent: ({ text, streaming, streamKey, variant }) => React.createElement("p", { "data-variant": variant, "data-streaming": streaming ? "true" : undefined, "data-stream-key": streamKey }, text),
    MessageTooltip: ({ children, label, onActivate }) => React.createElement("span", { "data-tooltip-target": label?.props?.children, "data-activates-file": Boolean(onActivate), onClick: onActivate }, children),
    readProjection: async (method, params) => { assert.equal(method, "projection.artifact.bytes.read"); artifactReads.push(params); return artifactResponse; },
    appToast: { warning: (error) => artifactErrors.push(error) },
    useOpenArtifactFiles: () => (reference) => filesOpened.push(reference),
    motion: { div: ({ animate, initial, transition, onAnimationComplete, ...props }) => {
      React.useEffect(() => { if (onAnimationComplete) completions.push(onAnimationComplete); }, [animate?.height]);
      return React.createElement("div", { ...props, style: { ...props.style, ...animate } });
    } },
  };
  function renderModule(value, names, dependencies) {
    const tree = ts.createSourceFile("layout.tsx", value, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const selected = tree.statements.filter((node) => !ts.isImportDeclaration(node) && (!names || names.includes(node.name?.text)));
    const input = selected.map((node) => node.getText(tree)).join("\n") + (names ? `\nmodule.exports = {${names.join(",")}};` : "");
    const compiled = ts.transpileModule(input, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const module = { exports: {} };
    Function("require", "module", "exports", ...Object.keys(dependencies), compiled)(require, module, module.exports, ...Object.values(dependencies));
    return module.exports;
  }
  const turn = renderModule(turnActivity, null, scope);
  const { parsePatchFiles, getSingularPatch } = await import("@pierre/diffs");
  const { parseWorkingPatch } = renderModule(source("src/components/chat/WorkingPatchDiff.tsx"), ["parseWorkingPatch"], { parsePatchFiles });
  for (const [kind, patch, additions, deletions] of [
    ["add", "@@ -0,0 +1,2 @@\n+report\n+total: 42\n", 2, 0],
    ["update", "@@ -1 +1 @@\n-blue\n+teal\n", 1, 1],
    ["delete", "@@ -1 +0,0 @@\n-old\n", 0, 1],
    ["update", "--- old.md\n+++ old.md\n@@ -1 +1 @@\n-old\n+new\n", 1, 1],
  ]) {
    const parsed = parseWorkingPatch(patch, "report with spaces.md", kind);
    assert.equal(parsed.error, null);
    assert.equal(parsed.fileDiff.hunks.reduce((n, hunk) => n + hunk.additionCount, 0), additions);
    assert.equal(parsed.fileDiff.hunks.reduce((n, hunk) => n + hunk.deletionCount, 0), deletions);
    assert.equal(getSingularPatch(parsed.patch).hunks.length, 1);
  }
  assert.match(parseWorkingPatch("not a diff", "report.md", "update").error, /one recorded file diff/);
  assert.ok(parseWorkingPatch("--- a\n+++ a\n@@ -1 +1 @@\n-old\n+new\n--- b\n+++ b\n@@ -1 +1 @@\n-old\n+new\n", "a", "update").error);
  const events = renderModule(messageList, ["AssistantEventContent", "ConversationNotice", "TraceReadTarget", "traceReadArtifactReference", "decodeJsonArtifactPayload", "WebSearchActivity", "StructuredActivity", "activityLifecycle", "activityLifecycleFromStatus", "eventString", "eventRecord", "eventArray", "eventJson", "dynamicToolResult", "reasoningBody"], { ...scope, ...turn,
    McpResourceSurface: ({ message }) => React.createElement("section", { "data-mcp-tool": message.eventData.tool }) });
  const fileActivity = renderModule(messageList, ["FileChangeActivity", "activityPathLabel"], {
    ...scope,
    ...renderModule(source("src/design-system/LoadingSkeleton.tsx"), null, scope),
    InlineUnifiedDiff: ({ diff }) => React.createElement("pre", null, diff),
  });
  const container = document.getElementById("root"), root = createRoot(container);
  const query = (selector) => container.querySelector(selector);
  const all = (selector) => [...container.querySelectorAll(selector)];
  const settle = async () => {
    while (frames.size || completions.length) await React.act(async () => {
      const batch = [...frames.values(), ...completions.splice(0)]; frames.clear(); batch.forEach((callback) => callback());
    });
  };
  let turnKey = 0;
  const render = async (activities, turnInProgress = true, response, statusBoundaryVisible = false, activityWindow = "all") => {
    if (!activities.length && !turnInProgress && !response) turnKey += 1;
    await React.act(async () => root.render(React.createElement(turn.TurnActivity, {
      key: turnKey,
      activities, activityWindow, turnInProgress, response, startedAtMs: 1, presentation: "conversation", statusBoundaryVisible, toolPresentations: {},
      renderActivity: (message) => React.createElement(events.AssistantEventContent, { message, toolPresentations: {} }),
    })));
  };
  const click = async (node) => { assert.ok(node); await React.act(async () => node.click()); await settle(); };
  const message = (id, eventKind, text, eventData = {}) => ({ id, eventKind, text, eventData, role: "assistant", createdAtMs: 1 });
  const commentary = message("commentary", "commentary", "Progress explained");
  const reasoning = message("reasoning", "reasoning_delta", "**Reviewing instructions and directory**", { summary_index: 0 });
  const operation = message("operation", "dynamic_tool", "", { tool: "run_list", call_id: "list", arguments: {}, body: { count: 2 }, status: "completed", _activity_origin: { interface_role: "provider" } });
  const read = message("read", "dynamic_tool", "", { ...operation.eventData, call_id: "read", tool: "workspace.read_file", arguments: { path: "src/life.rs" } });
  const rowText = () => all(".octopus-agent-activity-row").map((row) => row.textContent);
  try {
    const thinking = message("thinking", "reasoning_delta", "**Comparing the two files**", { summary_index: 0, lifecycle: "streaming", item_id: "thinking" });
    const nextNote = message("next-note", "commentary", "Checking the totals next");
    const nativeRead = message("cat", "tool", "", { call_id: "cat", status: "completed", command: "cat src/a.rs", parsed_cmd: { type: "read" } });
    const nativeCommand = message("build", "tool", "", { call_id: "build", status: "completed", command: "cargo build" });
    for (const [work, explores] of [[read, true], [nativeRead, true], [operation, false], [nativeCommand, false]]) {
      await render([], false); await render([work, thinking, nextNote], true);
      assert.equal(query(".octopus-thinking-fallback") === null, explores, `${work.id}: thinking after exploration stays in its group, after other work it shows on its own`);
      if (explores) assert.equal(query(".octopus-agent-activity-summary-value").textContent, "Comparing the two files");
      else assert.equal(query(".octopus-thinking-fallback").textContent, "Comparing the two files");
    }
    await render([commentary, operation], true, undefined, false, "latest-context");
    assert.match(container.textContent, /Progress explained/);
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Run list"]);
    await render([commentary, operation, message("new-context", "commentary", "Checking the result")], true, undefined, false, "latest-context");
    assert.match(container.textContent, /Checking the result/);
    assert.doesNotMatch(container.textContent, /Progress explained|Run list/);
    await render([operation], true, assistantDelta, true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "working");
    assert.match(query(".octopus-worked-for-boundary").textContent, /^Working/);
    await render([operation], true, { ...assistantDelta, eventData: { ...assistantDelta.eventData, phase: "final_answer" } }, true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "complete");
    await render([commentary, reasoning]);
    assert.equal(query(".octopus-turn-activity-content").children.length, 2, "commentary and Thinking have one gap owner, with no spacer child");
    assert.equal(query(".octopus-thinking-fallback").textContent, "Reviewing instructions and directory");
    const streamed = query(".octopus-turn-activity-content").textContent;
    await render([commentary, { ...reasoning, eventKind: "reasoning", eventData: { summary: [reasoning.text] } }, message("empty", "commentary_delta", " \n")]);
    assert.equal(query(".octopus-turn-activity-content").textContent, streamed);
    assert.equal(query(".octopus-turn-activity-content").children.length, 2, "empty stream wrappers cannot reserve a row or hide Thinking");
    await render([{ ...commentary, eventKind: "commentary_delta" }]);
    assert.ok(query(".octopus-thinking-fallback") === null, "streamed commentary already owns progress");
    await render([operation, message("empty", "commentary", ""), read], false);
    assert.equal(query(".octopus-turn-activity-content").children.length, 1);
    assert.ok(query(".octopus-agent-activity-body") === null, "settled groups mount collapsed");
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Run list", "Read life.rs"]);
    assert.ok(query('.structured-activity [data-glyph="plugins"]'), "Octopus interface operations carry the plugin mark");
    assert.ok(query(".structured-activity-body") === null, "closed operation details cannot retain padding");
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.match(query(".structured-activity-body").textContent, /Arguments\{\}Result[\s\S]*count/);
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.ok(query(".structured-activity-body") === null, "closed operation content unmounts after transition");
    const settledRows = rowText();
    await render([{ ...operation, id: "start", eventData: { ...operation.eventData, status: "started" } }, operation, read]);
    assert.deepEqual(rowText(), settledRows, "streamed and settled operations retain identical rows");
    assert.ok(query(".octopus-thinking-fallback") === null, "a trailing group owns Thinking without duplicate progress");
    await click(query(".octopus-turn-activity-content > .octopus-agent-activity .octopus-agent-activity-toggle"));
    await render([], false); await render([operation, read]);
    assert.ok(query(".octopus-agent-activity-body") === null, "streamed groups mount collapsed just like history");
    const collaboration = (id, tool, status, receivers) => message(id, "collaboration", "", { tool, status, receiverThreadIds: receivers, model: "gpt-test", prompt: `Prompt ${id}` });
    const subagent = (id, thread, kind, agentPath) => message(id, "subagent_activity", "", { agentThreadId: thread, kind, agentPath });
    await render([
      collaboration("spawn-a", "spawnAgent", "completed", ["aaaaaaaa-thread"]), collaboration("spawn-b", "spawnAgent", "completed", ["bbbbbbbb-thread"]),
      collaboration("message-a", "sendInput", "completed", ["aaaaaaaa-thread"]), collaboration("wait", "wait", "completed", ["aaaaaaaa-thread"]),
      subagent("sub-1", "t1", "started", "root/research_agent"), subagent("sub-2", "t2", "started", "root/writer"),
      subagent("sub-3", "t3", "started", "root/critic"), subagent("sub-4", "t4", "started", "root/editor"),
    ], false);
    assert.deepEqual(all(".octopus-agent-activity-toggle").map((node) => node.getAttribute("aria-label")), ["Created 2 agents", "Messaged an agent"]);
    assert.deepEqual(all(".octopus-subagent-chip-name").map((node) => node.textContent), ["Research agent", "Writer", "Critic"]);
    assert.equal(query(".octopus-subagent-group-status").textContent, "and 1 other subagent started working");
    assert.doesNotMatch(container.textContent, /Prompt wait/);
    await render([subagent("sub-1", "t1", "started", "root/a"), subagent("sub-2", "t2", "interacted", "root/b")], false);
    assert.equal(query(".octopus-subagent-group-status").textContent, "updated");
    await render([subagent("sub-1", "t1", "interacted", "root/a"), subagent("sub-2", "t2", "interrupted", "root/b")], false);
    assert.equal(query(".octopus-subagent-group-status").textContent, "interrupted");
    const privateReasoning = [message("reason", "reasoning", "", { summary: ["Checking files", "Then comparing"], content: ["RAW PRIVATE CHAIN"] }),
      message("raw-delta", "reasoning_delta", "RAW DELTA", { content_index: 0 }), message("unindexed-delta", "reasoning_delta", "RAW UNINDEXED", {})];
    await render(privateReasoning, true);
    assert.equal(query(".octopus-thinking-fallback-label").textContent, "Then comparing");
    assert.doesNotMatch(container.textContent, /RAW/);
    const renderLedger = (activities, turnInProgress) => React.act(async () => root.render(React.createElement(turn.TurnActivity, {
      activities, activityWindow: "all", turnInProgress, startedAtMs: 1, presentation: "ledger", statusBoundaryVisible: false, toolPresentations: {},
      renderActivity: (message) => React.createElement(events.AssistantEventContent, { message, toolPresentations: {} }),
    })));
    await renderLedger(privateReasoning, false);
    assert.deepEqual(all(".octopus-agent-activity-toggle").map((node) => [node.getAttribute("aria-label"), node.getAttribute("aria-expanded")]), [["Thought", "false"]]);
    await click(query(".octopus-agent-activity-toggle"));
    assert.equal(query(".octopus-reasoning-detail").textContent, "Then comparing");
    assert.ok(container.querySelector(".vertical-scroll-fade-mask") === null);
    assert.doesNotMatch(container.textContent, /RAW|Checking files/);
    await renderLedger([message("timed", "reasoning", "", { summary: ["Done"], started_at_ms: 1_000, completed_at_ms: 4_000 })], false);
    assert.match(query(".octopus-agent-activity-toggle").getAttribute("aria-label"), /^Thought for \S/);
    await renderLedger([message("live", "reasoning_delta", "Scanning", { summary_index: 0, lifecycle: "streaming" })], true);
    assert.equal(query(".octopus-agent-activity-summary-value").textContent, "Thinking");
    assert.equal(query(".octopus-reasoning-detail").textContent, "Scanning", "active reasoning opens without a click");
    await render([], false);
    await render([message("pending", "event", "", { work_status: "submitting" })], true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "starting");
    assert.equal(query('[role="status"][aria-live="polite"]').textContent, "Starting your task");
    await render([message("pending", "event", "", { work_status: "submitting" }), operation], true);
    assert.notEqual(query(".octopus-turn-activity").dataset.state, "starting");
    const finalResponse = { ...assistantDelta, eventData: { ...assistantDelta.eventData, phase: "final_answer" } };
    const historyToggle = () => query(".octopus-worked-for-toggle");
    const history = () => query(".octopus-turn-history");
    for (const motion of [true, false]) {
      reducedMotion = motion;
      await render([], false); await render([operation], false, finalResponse, true);
      assert.match(historyToggle().textContent, /^Worked for /);
      assert.equal(history(), null, "settled history mounts collapsed");
      await React.act(async () => historyToggle().click());
      assert.equal(historyToggle().getAttribute("aria-expanded"), "true");
      if (!motion) assert.equal(history().getAttribute("aria-hidden"), "true", "opening history stays hidden until its frame");
      await settle();
      assert.equal(history().getAttribute("aria-hidden"), "false");
      assert.equal(history().hasAttribute("inert"), false);
      await React.act(async () => historyToggle().click());
      assert.equal(historyToggle().getAttribute("aria-expanded"), "false");
      if (!motion) assert.equal(history().getAttribute("aria-hidden"), "true", "closing history animates out before it unmounts");
      await settle();
      assert.equal(history(), null);
    }
    reducedMotion = true;
    await render([], false); await render([message("thought", "reasoning", "", { summary: ["Comparing the two files"] }),
      message("content-thought", "reasoning", "", { summary: [], content: ["**Planning the merge**\n\nThe totals differ"] })], false, finalResponse, true);
    assert.match(historyToggle().textContent, /^Worked for /, "a turn that only thought still says how long it worked");
    await click(historyToggle());
    assert.deepEqual(all(".octopus-agent-activity-toggle").map((node) => node.getAttribute("aria-label")), ["Thought", "Thought"], "settled thoughts stay in the turn history");
    await click(all(".octopus-agent-activity-toggle")[1]);
    assert.equal(query(".octopus-reasoning-detail").textContent, "The totals differ", "reasoning projected only as content is shown");
    await render([], false); await render([], false, finalResponse, true);
    assert.equal(container.childElementCount, 0, "a turn without work or thought renders nothing");
    await render([operation, message("interrupted", "interrupted_result", "", {})], false, undefined, true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "stopped");
    assert.match(query(".octopus-worked-for-boundary").textContent, /^Interrupted after /);
    await render([operation, message("stop", "turn_control", "", { action: "stop", status: "acknowledged" })], false, undefined, true);
    assert.match(query(".octopus-worked-for-boundary").textContent, /^You stopped after /);
    const buffering = (id, showBufferingUi) => message(id, "warning", "Checking response safety", { source: "model_safety_buffering", showBufferingUi });
    await render([operation, buffering("quiet", false), buffering("shown", true)], true);
    assert.equal((container.textContent.match(/Checking response safety/g) ?? []).length, 1);
    assert.equal((container.textContent.match(/No action is required\. Octopus will keep waiting for the response\./g) ?? []).length, 1);
    await render([operation, buffering("shown", true)], false, finalResponse, false);
    assert.doesNotMatch(container.textContent, /Checking response safety/);
    const callUpdate = (index, failed) => message(`flaky-${index}`, "dynamic_tool", "", { tool: "run_list", call_id: "flaky", status: failed ? "failed" : "completed",
      ok: !failed, arguments: {}, _activity_origin: { interface_role: "provider" } });
    await render([{ ...callUpdate(0, true), error: true }, callUpdate(1, false)], false);
    assert.equal(query(".octopus-agent-activity").dataset.state, "error", "a later successful update keeps the call's failure tone");
    const updateCost = async (count) => {
      const updates = Array.from({ length: count }, (_, index) => callUpdate(index, index === 0));
      let best = Infinity;
      for (let attempt = 0; attempt < 3; attempt += 1) {
        await render([], false);
        const started = process.hrtime.bigint();
        await render(updates, false);
        best = Math.min(best, Number(process.hrtime.bigint() - started));
      }
      return best;
    };
    await updateCost(200);
    assert.ok(await updateCost(2_400) < await updateCost(600) * 10, "coalescing repeated updates of one call stays near-linear");
    await render([message("mcp", "mcp_tool", "", { server: "github", tool: "search", status: "completed" })], false);
    assert.ok(query('.octopus-agent-activity-header [data-glyph="plugins"]'), "MCP calls carry the plugin mark");
    await render([message("mcp-live", "mcp_tool", "", { server: "github", tool: "search", status: "in_progress" })], true);
    assert.ok(query('.octopus-agent-activity-header [data-glyph="plugins"]'), "an MCP call in flight carries the plugin mark");
    await render([message("mcp", "mcp_tool", "", { server: "github", tool: "search", status: "completed" })], false);
    await click(query(".octopus-agent-activity-toggle"));
    assert.ok(container.querySelector(".vertical-scroll-fade-mask") === null);
    assert.ok(query('[data-mcp-tool="search"]'), "MCP calls render through the MCP surface");
    assert.doesNotMatch(container.textContent, /Called|Calling/);
    for (const [kind, streaming] of [["reasoning_delta", true], ["reasoning", false]]) {
      await React.act(async () => root.render(React.createElement(events.AssistantEventContent, { message: message("thought", kind, "Comparing totals"), toolPresentations: {} })));
      const body = query('p[data-variant="reasoning"]');
      assert.deepEqual([body.textContent, body.dataset.streaming, body.dataset.streamKey], ["Comparing totals", streaming ? "true" : undefined, streaming ? "thought" : undefined], kind);
    }
    await React.act(async () => root.render(React.createElement(turn.TurnActivity, { activities: [{ ...operation, eventData: { ...operation.eventData, status: "started" } }],
      startedAtMs: 1, presentation: "conversation", toolPresentations: {}, renderActivity: (item) => React.createElement(events.AssistantEventContent, { message: item, toolPresentations: {} }) })));
    assert.equal(query(".octopus-turn-activity").dataset.state, "working", "an unstated turn follows its active work");
    await render([operation], false, finalResponse, false);
    const finalGap = query(".octopus-turn-activity").lastElementChild;
    assert.equal(finalGap.getAttribute("aria-hidden"), "true");
    assert.match(finalGap.getAttribute("style"), /--conversation-item-gap/);
    const artifactId = `docs.guide.md:${"a".repeat(64)}`;
    const artifactMessage = { ...message("artifact", "dynamic_tool", "", { tool: "artifact.read", call_id: "artifact", status: "completed", arguments: { id: artifactId }, _activity_origin: { interface_role: "provider" } }), artifactId: "receipt:exact-read", runId: "run-test", interfaceId: "provider-test" };
    const readResult = { tool: "artifact.read", correlation_id: "artifact", ok: true, body: { result: "artifact_read", id: artifactId, text: "Historical content", artifact_size_bytes: 18, offset_bytes: 0, bytes_read: 18, truncated: false } };
    artifactResponse = { artifact_id: artifactMessage.artifactId, run_id: artifactMessage.runId, interface_id: artifactMessage.interfaceId, base64: Buffer.from(JSON.stringify(readResult)).toString("base64") };
    await render([artifactMessage], false);
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Read artifact docs.guide.md"]);
    assert.equal(query("[data-tooltip-target]").getAttribute("data-tooltip-target"), artifactId);
    assert.equal(query("[data-tooltip-target]").getAttribute("data-activates-file"), "true");
    await click(query("[data-tooltip-target]"));
    assert.deepEqual(artifactReads, [{ call_id: "artifact", run_id: "run-test" }]);
    assert.equal(filesOpened.at(-1).inlinePayload, "Historical content");
    assert.equal(filesOpened.at(-1).artifactId, artifactMessage.artifactId);
    assert.equal(query('[role="dialog"]'), null, "an admitted artifact opens through Files without a modal");
    assert.deepEqual(artifactErrors, []);
    const artifactPresentation = toolPresentation.dynamicToolPresentation(artifactMessage, {});
    assert.throws(() => events.traceReadArtifactReference(artifactMessage, artifactPresentation, { ...artifactResponse, run_id: "another-run" }), /coordinates/);
    assert.throws(() => events.traceReadArtifactReference(artifactMessage, artifactPresentation, { ...artifactResponse, base64: Buffer.from(JSON.stringify({ ...readResult, body: { ...readResult.body, id: "different-artifact" } })).toString("base64") }), /target/);
    const entry = { id: "guide", content: { text: "Retained instructions" } }, revision = "b".repeat(64);
    const contextSource = { id: "library", content: Array(32).fill(1) };
    const contextMessage = { ...artifactMessage, eventData: { ...artifactMessage.eventData, tool: `context.read__${workspaceToolSuffix}`, arguments: { source: contextSource, entry_id: entry.id, revision } } };
    const contextPresentation = toolPresentation.dynamicToolPresentation(contextMessage, {});
    const contextResult = { ...readResult, tool: contextMessage.eventData.tool, body: { entry, pin: { source: contextSource, entry_id: entry.id, revision } } };
    const contextBytes = value => ({ ...artifactResponse, base64: Buffer.from(JSON.stringify(value)).toString("base64") });
    assert.deepEqual(JSON.parse(events.traceReadArtifactReference(contextMessage, contextPresentation, contextBytes(contextResult)).inlinePayload), entry);
    assert.throws(() => events.traceReadArtifactReference(contextMessage, contextPresentation, contextBytes({ ...contextResult, correlation_id: "another-call" })), /target/);
    for (const changed of [{ revision: "c".repeat(64) }, { source: { id: "another-library", content: contextSource.content } }, { source: { ...contextSource, content: Array(32).fill(2) } }, { entry_id: "another-entry" }]) {
      assert.throws(() => events.traceReadArtifactReference(contextMessage, contextPresentation, contextBytes({ ...contextResult, body: { ...contextResult.body, pin: { ...contextResult.body.pin, ...changed } } })), /source and revision/);
    }
    const navigation = message("navigate", "dynamic_tool", "", {
      tool: `browser.navigate__${workspaceToolSuffix}`, call_id: "navigate", status: "completed",
      arguments: { url: "https://example.org/path" }, ok: false,
      error: { message: "native host rendezvous was not published" },
      _activity_origin: { interface_role: "provider" },
    });
    await render([], false);
    await render([navigation], false);
    assert.ok(query('.octopus-agent-activity-header [data-glyph="browserUse"]'));
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Used the browser"]);
    assert.ok(query('.structured-activity [data-glyph="browserUse"]'));
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.match(query(".structured-activity-body").textContent, /https:\/\/example.org\/path/);
    assert.match(query(".structured-activity-body").textContent, /native host rendezvous/);
    for (const [receipt, expected] of [
      [{}, "Diff unavailable"],
      [{ added: 0 }, "Diff unavailable"],
      [{ added: 1, deleted: 0 }, "Diff unavailable"],
      [{ changeKind: "delete" }, "Diff unavailable"],
      [{ added: 0, deleted: 0, lifecycle: "error" }, "Diff unavailable"],
      [{ added: 0, deleted: 0 }, "No changes"],
      [{ detailsLoading: true }, "Loading full diff…"],
      [{ diff: "+recorded content" }, "+recorded content"],
    ]) {
      await React.act(async () => root.render(React.createElement(fileActivity.FileChangeActivity, {
        changeKind: "update", diff: "", lifecycle: "complete", path: "expense-report.md", onOpenFile: () => {}, ...receipt,
      })));
      const disclosure = query('[aria-label="Toggle diff for expense-report.md"]');
      if (disclosure.getAttribute("aria-expanded") === "false") await click(disclosure);
      assert.equal(receipt.detailsLoading ? query('.activity-file-body [role="status"]').getAttribute("aria-label") : query(".activity-file-body").textContent, expected);
    }
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    delete global.window; delete global.document; delete global.IS_REACT_ACT_ENVIRONMENT;
    if (previousNavigator) Object.defineProperty(global, "navigator", previousNavigator);
    else delete global.navigator;
  }
}

async function verifyPreparedDraftStorage() {
  const React = require("react"), { JSDOM } = require("jsdom"), { createRoot } = require("react-dom/client"), { build } = require("esbuild");
  const dom = new JSDOM('<!doctype html><div id="root"></div>', { url: "http://fixture.invalid" });
  Object.assign(global, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  const previousStorage = Object.getOwnPropertyDescriptor(global, "localStorage");
  Object.defineProperty(global, "localStorage", { configurable: true, value: dom.window.localStorage });
  const bundle = await build({ entryPoints: [path.join(appRoot, "src/components/chat/useChatDraftState.ts")], bundle: true, platform: "node", format: "cjs",
    write: false, logLevel: "silent", external: ["react", "react-dom"] });
  const module = { exports: {} };
  Function("require", "module", "exports", bundle.outputFiles[0].text)(require, module, module.exports);
  const root = createRoot(document.getElementById("root"));
  let draft;
  function Probe() {
    draft = module.exports.useChatDraftState({ accessProfile: "lab", activeRunId: "run-a", storageScope: "home", onError: () => {}, preserveSubmissionIntentOnEdit: () => false });
    return null;
  }
  try {
    await React.act(async () => root.render(React.createElement(Probe)));
    await React.act(async () => draft.setText("Prepared totals"));
    await React.act(async () => draft.persistSubmissionIntent("run-a", "msg_prepared", false));
    await React.act(async () => draft.setRawText("Recalled from history"));
    await React.act(async () => draft.saveCurrentDraft("run-a"));
    assert.deepEqual([draft.readDraft("run-a").messageId, draft.readDraft("run-a").text], ["msg_prepared", "Prepared totals"],
      "a prepared message keeps its stored payload under its identity while the composer shows other text");
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    delete global.window; delete global.document; delete global.IS_REACT_ACT_ENVIRONMENT;
    if (previousStorage) Object.defineProperty(global, "localStorage", previousStorage);
    else delete global.localStorage;
  }
}

verifyTranscriptLayout().then(verifyPreparedDraftStorage).then(() => process.stdout.write("chat trace source contracts and transcript layout fixtures passed\n"))
  .catch((error) => { console.error(error); process.exitCode = 1; });
