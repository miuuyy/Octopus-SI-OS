import { normalizeArtifactFilesTarget } from "../files/artifactFilesTarget";
import { clientCommands, type ClientCommandId } from "../../clientActions";
import type { SelectionRef } from "../../data/topology";
import type { OctopusClientAccessProfile } from "../../electron";
import type { useRuntimeTopology } from "../../runtimeTopology";
import {
  profileStorageKey,
  surfaceBelongsToWorkspace,
  type ClientWorkspaceKey,
} from "../../shell/clientWorkspaces";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import { activeDestinationStorageKey } from "../../shell/navigation";
import {
  clientWindowStateKey,
  clientWindowStateStorage,
  workspaceSurfacePaneTab,
  type WorkspacePaneTabState,
  type WorkspacePaneTarget,
} from "../../shell/clientWindowModel";
import type { FilesOpenTarget } from "../files/types";
import { normalizeFilesPaneSnapshot } from "../files/filesPaneSnapshot";
import type { WorkerEnvironmentPaneTarget } from "../worker-environment/workerEnvironmentPane";
import type { WorkerEnvironmentPanelKind } from "../worker-environment/surfaceRegistry";
import { restorePaneTabs } from "./paneTabs";

export type SecondaryPaneTabState = WorkspacePaneTabState;

export type WorkspacePaneLayout = {
  activePane: "primary" | "secondary";
  focused: boolean;
  sidebarOpen: boolean;
  sidebarWidth: number;
  widthPercent: number;
};

export type StoredSecondaryPaneState = {
  version: 3;
  open: boolean;
  activeTabId: string;
  tabs: SecondaryPaneTabState[];
  layout?: WorkspacePaneLayout;
};

const SECONDARY_PANE_STATE_STORAGE_KEY = "octopus.home.secondary-pane.v1";

export function isSidebarOnlySurface(surface: Pick<ClientSurfaceDescriptor, "id">): boolean {
  return false;
}

export function secondaryPaneTab(
  workspaceKey: ClientWorkspaceKey,
  surfaceId: string,
  runId: string,
): SecondaryPaneTabState {
  return workspaceSurfacePaneTab(workspaceKey, surfaceId, runId);
}

export type SurfaceNavigation = {
  entries: ClientDestination[];
  index: number;
};

export type ClientDestination = {
  runId?: string;
  surfaceId: string;
};

export function clientDestination(surfaceId: string, runId?: string): ClientDestination {
  return surfaceId === "chat" ? { surfaceId, runId: runId ?? "" } : { surfaceId };
}

export function clientWindowInitialDestination(target: WorkspacePaneTarget): ClientDestination {
  if (target.kind === "surface") return clientDestination(sanitizePrimarySurfaceId(target.surfaceId), target.runId);
  return clientDestination(target.workspaceKey === "dashboard" ? "dashboard" : "voice");
}

export function sameClientDestination(left: ClientDestination, right: ClientDestination): boolean {
  return left.surfaceId === right.surfaceId && left.runId === right.runId;
}

function sanitizePrimarySurfaceId(surfaceId: string | undefined, fallback = "voice"): string {
  const normalized = surfaceId?.trim() ?? "";
  return !normalized || normalized === "inbox" ? fallback : normalized;
}

export function initializeSurfaceNavigation(
  startupSurface: string,
  profile: OctopusClientAccessProfile,
  startupRunId = "",
  windowScope = "main",
): SurfaceNavigation {
  const storage = clientWindowStateStorage(windowScope);
  const storedSurface = storage.getItem(clientWindowStateKey(windowScope, activeDestinationStorageKey(profile)))?.trim();
  const resolved = sanitizePrimarySurfaceId(storedSurface, sanitizePrimarySurfaceId(startupSurface));
  return { entries: [clientDestination(resolved, startupRunId)], index: 0 };
}

export function normalizeSurfaceNavigation(
  current: SurfaceNavigation,
  surfaces: ClientSurfaceDescriptor[],
  chatRunIds?: ReadonlySet<string>,
): SurfaceNavigation {
  const allowedIds = new Set(surfaces.filter((surface) => surface.id !== "inbox").map((surface) => surface.id));
  const valid = (destination: ClientDestination) => allowedIds.has(destination.surfaceId) && (
    destination.surfaceId !== "chat"
    || !destination.runId
    || chatRunIds === undefined
    || chatRunIds.has(destination.runId)
  );
  const entries = current.entries.filter(valid);
  if (!entries.length) return { entries: [clientDestination("voice")], index: 0 };
  const index = Math.max(0, Math.min(entries.length - 1, current.entries.slice(0, current.index + 1).filter(valid).length - 1));
  return entries.length === current.entries.length && index === current.index ? current : { entries, index };
}

export function resolveSelection(
  selectedRef: SelectionRef | null,
  topology: ReturnType<typeof useRuntimeTopology>,
): SelectionRef | null {
  if (selectedRef?.kind === "environment" && topology.environments.some((environment) => environment.id === selectedRef.id)) {
    return selectedRef;
  }
  if (selectedRef?.kind === "run" && topology.environments.some((environment) => environment.runs.some((run) => run.id === selectedRef.id))) {
    return selectedRef;
  }
  if (selectedRef?.kind === "interface" && topology.interfaceIndex[selectedRef.id]) {
    return selectedRef;
  }
  const firstEnvironment = topology.environments[0];
  return firstEnvironment ? { kind: "environment", id: firstEnvironment.id } : null;
}

export function secondaryPaneStorageKey(profile: OctopusClientAccessProfile, homeRunId: string, windowScope = "main", paneScope = ""): string {
  return clientWindowStateKey(
    windowScope,
    profileStorageKey(`${SECONDARY_PANE_STATE_STORAGE_KEY}.${homeRunId.trim()}${paneScope ? `.${paneScope}` : ""}`, profile),
  );
}

export function readSecondaryPaneState(
  profile: OctopusClientAccessProfile,
  homeRunId: string,
  windowScope = "main",
  paneScope = "",
): StoredSecondaryPaneState {
  return readSecondaryPaneStateAtKey(windowScope, secondaryPaneStorageKey(profile, homeRunId, windowScope, paneScope));
}

export function readSecondaryPaneStateAtKey(windowScope: string, key: string): StoredSecondaryPaneState {
  const fallback: StoredSecondaryPaneState = { version: 3, open: false, activeTabId: "", tabs: [] };
  try {
    const raw = clientWindowStateStorage(windowScope).getItem(key);
    if (!raw) return fallback;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return fallback;
    const record = parsed as Record<string, unknown>;
    if (![1, 2, 3].includes(Number(record.version)) || !Array.isArray(record.tabs)) return fallback;
    const layout = record.layout === undefined ? undefined : normalizeWorkspacePaneLayout(record.layout);
    if (layout === null) return fallback;
    const restored = restorePaneTabs({
      activeTabId: typeof record.activeTabId === "string" ? record.activeTabId : "",
      storedTabs: record.tabs,
      resolve: (value) => {
        const tab = normalizeSecondaryPaneTab(value);
        return tab ? { id: tab.id, tab } : null;
      },
    });
    return {
      version: 3,
      open: record.open === true,
      activeTabId: restored.activeTabId,
      tabs: restored.tabs,
      ...(layout ? { layout } : {}),
    };
  } catch {
    return fallback;
  }
}

export function writeSecondaryPaneState(
  profile: OctopusClientAccessProfile,
  homeRunId: string,
  windowScope: string,
  state: Pick<StoredSecondaryPaneState, "activeTabId" | "open" | "tabs" | "layout">,
  paneScope = "",
): void {
  if (!homeRunId.trim()) return;
  writeSecondaryPaneStateAtKey(windowScope, secondaryPaneStorageKey(profile, homeRunId, windowScope, paneScope), state);
}

export function writeSecondaryPaneStateAtKey(
  windowScope: string,
  key: string,
  state: Pick<StoredSecondaryPaneState, "activeTabId" | "open" | "tabs" | "layout">,
): void {
  if (state.layout && !normalizeWorkspacePaneLayout(state.layout)) throw new Error("Invalid workspace pane layout");
  const tabs = state.tabs.filter((tab) => tab.target.kind !== "terminal" && tab.target.kind !== "task_sources" && tab.target.kind !== "client_browser");
  const activeTabId = tabs.some((tab) => tab.id === state.activeTabId)
    ? state.activeTabId
    : tabs[0]?.id ?? "";
  const value: StoredSecondaryPaneState = {
    version: 3,
    activeTabId,
    open: state.open && (state.tabs.length === 0 || tabs.length > 0),
    tabs,
    ...(state.layout ? { layout: state.layout } : {}),
  };
  clientWindowStateStorage(windowScope).setItem(
    key,
    JSON.stringify(value),
  );
}

function normalizeWorkspacePaneLayout(value: unknown): WorkspacePaneLayout | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.activePane !== "primary" && row.activePane !== "secondary") return null;
  if (typeof row.focused !== "boolean" || typeof row.sidebarOpen !== "boolean") return null;
  if (typeof row.sidebarWidth !== "number" || !Number.isFinite(row.sidebarWidth) || row.sidebarWidth <= 0) return null;
  if (typeof row.widthPercent !== "number" || !Number.isFinite(row.widthPercent) || row.widthPercent <= 0 || row.widthPercent > 100) return null;
  return { activePane: row.activePane, focused: row.focused, sidebarOpen: row.sidebarOpen, sidebarWidth: row.sidebarWidth, widthPercent: row.widthPercent };
}

function normalizeSecondaryPaneTab(value: unknown): SecondaryPaneTabState | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const id = boundedPaneText(row.id, 512);
  const target = normalizeWorkspacePaneTarget(row.target);
  return id && target ? { id, target } : null;
}

function normalizeWorkspacePaneTarget(value: unknown): SecondaryPaneTabState["target"] | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const workspaceKey = row.workspaceKey === "dashboard" || row.workspaceKey === "octopus-os"
    ? row.workspaceKey
    : null;
  if (!workspaceKey) return null;
  if (row.kind === "launcher") {
    let source: Extract<WorkspacePaneTarget, { kind: "launcher" }>["source"];
    if (row.source !== undefined) {
      if (!row.source || typeof row.source !== "object" || Array.isArray(row.source)) return null;
      const value = row.source as Record<string, unknown>;
      if (value.kind === "terminal") {
        const cwd = value.cwd === undefined ? undefined : boundedPaneText(value.cwd, 8192);
        if (cwd === "") return null;
        source = { kind: "terminal", ...(cwd ? { cwd } : {}) };
      } else if (value.kind === "worker_environment") {
        const target = normalizeWorkerEnvironmentPaneTarget(value.target);
        if (!target) return null;
        source = { kind: "worker_environment", target };
      } else return null;
    }
    return { kind: "launcher", runId: boundedPaneText(row.runId, 512), workspaceKey, ...(source ? { source } : {}) };
  }
  if (row.kind === "surface") {
    const surfaceId = boundedPaneText(row.surfaceId, 512);
    const runId = boundedPaneText(row.runId, 512);
    return surfaceId ? { kind: "surface", runId: surfaceId === "chat" ? runId : "", surfaceId, workspaceKey } : null;
  }
  if (row.kind === "files") {
    const openTarget = normalizeFilesOpenTarget(row.openTarget);
    if (row.openTarget !== undefined && !openTarget) return null;
    const snapshot = row.snapshot === undefined ? undefined : normalizeFilesPaneSnapshot(row.snapshot);
    if (row.snapshot !== undefined && !snapshot) return null;
    return { kind: "files", workspaceKey, ...(openTarget ? { openTarget } : {}), ...(snapshot ? { snapshot } : {}) };
  }
  if (row.kind === "terminal") {
    const terminalId = boundedPaneText(row.terminalId, 128);
    const title = boundedPaneText(row.title, 120);
    return terminalId ? { kind: "terminal", terminalId, title, workspaceKey } : null;
  }
  if (row.kind === "worker_environment") {
    const target = normalizeWorkerEnvironmentPaneTarget(row.target);
    return target ? { kind: "worker_environment", target, workspaceKey } : null;
  }
  return null;
}

function normalizeWorkerEnvironmentPaneTarget(value: unknown): WorkerEnvironmentPaneTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const workerEnvironmentId = boundedPaneText(row.workerEnvironmentId, 512);
  if (!workerEnvironmentId) return null;
  const surfaceId = boundedPaneText(row.surfaceId, 512);
  const filePath = boundedPaneText(row.filePath, 2048);
  const panelKind = normalizeWorkerEnvironmentPanelKind(row.panelKind);
  const clientSurface = normalizePaneClientSurface(row.clientSurface);
  if (row.clientSurface !== undefined && !clientSurface) return null;
  return {
    schemaVersion: 1,
    workerEnvironmentId,
    ...(surfaceId ? { surfaceId } : {}),
    ...(filePath ? { filePath } : {}),
    ...(panelKind ? { panelKind } : {}),
    ...(clientSurface ? { clientSurface } : {}),
  };
}

function normalizeFilesOpenTarget(value: unknown): FilesOpenTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.kind === "file") {
    const absolutePath = boundedPaneText(row.absolutePath, 4096);
    const path = boundedOptionalPaneText(row.path, 4096);
    const rootId = boundedPaneText(row.rootId, 512);
    if (!absolutePath && path == null && !rootId) return null;
    if (path != null !== Boolean(rootId)) return null;
    return {
      kind: "file",
      ...(absolutePath ? { absolutePath } : {}),
      ...(path != null ? { path, rootId } : {}),
    };
  }
  if (row.kind === "artifact") return normalizeArtifactFilesTarget(row);
  return null;
}

function normalizePaneClientSurface(value: unknown): NonNullable<WorkerEnvironmentPaneTarget["clientSurface"]> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const documentId = boundedPaneText(row.documentId, 512);
  const sourceDigest = boundedPaneText(row.sourceDigest, 128);
  const revision = Number(row.revision);
  if (!documentId || !sourceDigest || !Number.isSafeInteger(revision) || revision < 1) return null;
  return { documentId, revision, sourceDigest };
}

function normalizeWorkerEnvironmentPanelKind(value: unknown): WorkerEnvironmentPanelKind | "" {
  const candidate = boundedPaneText(value, 64);
  return [
    "browser",
    "connection",
    "container",
    "runtime",
    "worktree",
    "files",
    "file",
    "terminal",
    "changes",
    "desktop",
    "custom",
    "overview",
  ].includes(candidate) ? candidate as WorkerEnvironmentPanelKind : "";
}

function boundedOptionalPaneText(value: unknown, maximum: number): string | null {
  if (typeof value !== "string" || value.length > maximum || /[\u0000-\u001f\u007f]/.test(value)) return null;
  return value;
}

function boundedPaneText(value: unknown, maximum: number): string {
  const text = typeof value === "string" ? value.trim() : "";
  return text && text.length <= maximum && !/[\u0000-\u001f\u007f]/.test(text) ? text : "";
}

export function stringifyPreparedContext(value: unknown): string | null {
  const serialized = JSON.stringify(value, null, 2);
  return serialized.length <= 160_000 ? serialized : null;
}

export function isClientCommandId(value: unknown): value is ClientCommandId {
  return typeof value === "string" && clientCommands.some((command) => command.id === value);
}
