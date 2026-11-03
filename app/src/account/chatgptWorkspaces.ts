import type { OctopusChatgptAccountSession, OctopusChatgptAccountSessions, OctopusChatgptWorkspace } from "../electron";

export function chatgptWorkspaceLabel(workspace: OctopusChatgptWorkspace) {
  return workspace.kind === "personal" ? "Personal account" : workspace.name?.trim() || "Workspace";
}

export function activeChatgptSession(sessions: OctopusChatgptAccountSessions | null): OctopusChatgptAccountSession | null {
  return sessions?.sessions.find((session) => session.sessionId === sessions.activeSessionId) ?? null;
}

export function selectedChatgptWorkspace(session: OctopusChatgptAccountSession | null): OctopusChatgptWorkspace | null {
  return session?.workspaces.find((workspace) => workspace.accountId === session.selectedWorkspaceId) ?? null;
}
