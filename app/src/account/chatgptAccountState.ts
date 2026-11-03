import type { OctopusChatgptAccount, OctopusChatgptAccountSessions } from "../electron";

export type ChatgptAccountBridge = Pick<NonNullable<Window["octopusTransport"]>,
  "chatgptAccount" | "chatgptAccountSessions" | "chatgptSwitchWorkspace" | "chatgptSignInStart" | "chatgptSignInStatus"
  | "chatgptSignInCancel" | "chatgptLogout" | "onChatgptAccountChanged">;

export type ChatgptAccountState = {
  account: OctopusChatgptAccount | null;
  loading: boolean;
  action: "idle" | "starting" | "waiting" | "cancelling" | "signing-out" | "switching";
  error: string;
  signInStatusError: string;
  signInUrl: string;
  sessions: OctopusChatgptAccountSessions | null;
  sessionsLoading: boolean;
  sessionsError: string;
};

export type ChatgptWorkspaceSwitchOutcome = "switched" | "browser" | "failed";

export const EMPTY_CHATGPT_ACCOUNT_STATE: ChatgptAccountState = { account: null, loading: true, action: "idle", error: "", signInStatusError: "", signInUrl: "",
  sessions: null, sessionsLoading: false, sessionsError: "" };

export function chatgptAccountIdentity(account: OctopusChatgptAccount | null): string | null {
  return account?.signedIn ? JSON.stringify([account.credentialHome, account.email, account.workspaceName ?? null]) : null;
}

export function createChatgptAccountState(bridge: ChatgptAccountBridge, home?: string) {
  let state = EMPTY_CHATGPT_ACCOUNT_STATE;
  let disposed = false;
  let readGeneration = 0;
  let pendingRead: Promise<OctopusChatgptAccount | null> | null = null;
  let actionGeneration = 0;
  let loginId = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let observedIdentity: string | null = null;
  let sessionsGeneration = 0;
  let pendingSessions: Promise<OctopusChatgptAccountSessions | null> | null = null;
  const listeners = new Set<() => void>();
  const message = (error: unknown) => error instanceof Error ? error.message : String(error);
  function update(patch: Partial<ChatgptAccountState>) {
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  }
  function stopPolling() {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    actionGeneration += 1;
  }
  function observeAccount(account: OctopusChatgptAccount | null): Partial<ChatgptAccountState> {
    const identity = chatgptAccountIdentity(account);
    if (identity === observedIdentity) return {};
    observedIdentity = identity;
    sessionsGeneration += 1;
    pendingSessions = null;
    return { sessions: null, sessionsLoading: false, sessionsError: "" };
  }
  function requireHome(observation: { octopusHome: string | null }) {
    if (home !== undefined && observation.octopusHome !== home) throw new Error("The provider account observation belongs to another Home.");
  }
  function loadSessions(): Promise<OctopusChatgptAccountSessions | null> {
    if (disposed || !state.account?.signedIn) return Promise.resolve(null);
    if (pendingSessions) return pendingSessions;
    const generation = ++sessionsGeneration;
    update({ sessionsLoading: true, sessionsError: "" });
    const request: Promise<OctopusChatgptAccountSessions | null> = Promise.resolve().then(() => bridge.chatgptAccountSessions()).then((sessions) => {
      requireHome(sessions);
      if (generation !== sessionsGeneration) return state.sessions;
      update({ sessions, sessionsLoading: false });
      return sessions;
    }, (error) => {
      if (generation !== sessionsGeneration) return state.sessions;
      update({ sessions: null, sessionsLoading: false, sessionsError: message(error) });
      return null;
    }).finally(() => { if (pendingSessions === request) pendingSessions = null; });
    pendingSessions = request;
    return request;
  }
  function refresh(): Promise<OctopusChatgptAccount | null> {
    if (disposed) return Promise.resolve(null);
    const generation = ++readGeneration;
    update({ account: null, loading: true, error: "" });
    const request: Promise<OctopusChatgptAccount | null> = Promise.resolve().then(() => bridge.chatgptAccount()).then((account) => {
      requireHome(account);
      return account;
    }).then((account) => {
      if (generation !== readGeneration) return pendingRead !== request ? pendingRead : state.account;
      update({ account, loading: false, ...observeAccount(account) });
      return account;
    }, (error) => {
      if (generation !== readGeneration) return pendingRead !== request ? pendingRead : state.account;
      update({ account: null, loading: false, error: message(error), ...observeAccount(null) });
      return null;
    });
    pendingRead = request;
    return request;
  }
  async function poll(generation: number) {
    if (disposed) return;
    let terminal = false;
    try {
      const result = await bridge.chatgptSignInStatus(loginId);
      if (generation !== actionGeneration || state.action !== "waiting") return;
      update({ signInStatusError: "" });
      if (result.state === "pending") {
        timer = setTimeout(() => void poll(generation), 1000);
        return;
      }
      terminal = true;
      if (result.state !== "completed") throw new Error(result.error || "ChatGPT sign-in did not complete.");
      const account = await refresh();
      if (generation !== actionGeneration) return;
      if (!account?.signedIn) throw new Error(state.error || "Sign in with ChatGPT to continue.");
      loginId = "";
      update({ action: "idle", signInUrl: "" });
    } catch (error) {
      if (generation !== actionGeneration) return;
      if (terminal) loginId = "";
      update(terminal
        ? { action: "idle", error: message(error), signInStatusError: "", signInUrl: "" }
        : { action: "waiting", signInStatusError: message(error) });
    }
  }
  return {
    getSnapshot: () => state,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    refresh,
    loadSessions,
    dispose() {
      disposed = true;
      readGeneration += 1;
      pendingRead = null;
      stopPolling();
      state = EMPTY_CHATGPT_ACCOUNT_STATE;
      listeners.clear();
    },
    async retrySignInStatus() {
      if (disposed || state.action !== "waiting" || !loginId || !state.signInStatusError) return;
      stopPolling();
      update({ signInStatusError: "" });
      await poll(actionGeneration);
    },
    connect() { return bridge.onChatgptAccountChanged(() => { void refresh(); }); },
    async signIn() {
      if (disposed || state.action !== "idle") return;
      stopPolling();
      const generation = actionGeneration;
      update({ action: "starting", error: "", signInStatusError: "", signInUrl: "" });
      try {
        const result = await bridge.chatgptSignInStart();
        if (generation !== actionGeneration) return;
        loginId = result.loginId;
        update({ action: "waiting", signInUrl: result.authUrl });
        void poll(generation);
      } catch (error) {
        if (generation === actionGeneration) update({ action: "idle", error: message(error) });
      }
    },
    async cancel() {
      if (disposed || state.action !== "waiting" || !loginId) return;
      stopPolling();
      update({ action: "cancelling", error: "", signInStatusError: "" });
      try {
        await bridge.chatgptSignInCancel(loginId);
        loginId = "";
        update({ action: "idle", signInUrl: "" });
        await refresh();
      } catch (error) {
        update({ action: "waiting", error: message(error) });
        void poll(actionGeneration);
      }
    },
    async logout() {
      if (disposed || state.action !== "idle") return;
      stopPolling();
      readGeneration += 1;
      update({ action: "signing-out", error: "", signInStatusError: "", signInUrl: "" });
      try {
        const account = await bridge.chatgptLogout();
        if (disposed) return;
        if (account.signedIn) throw new Error("ChatGPT logout was not confirmed.");
        readGeneration += 1;
        update({ account, loading: false, action: "idle", ...observeAccount(account) });
      } catch (error) {
        if (disposed) return;
        await refresh();
        update({ action: "idle", error: message(error) });
      }
    },
    async switchWorkspace(sessionId: string, accountId: string): Promise<ChatgptWorkspaceSwitchOutcome | null> {
      if (disposed || state.action !== "idle" || !state.account?.signedIn) return null;
      stopPolling();
      const generation = actionGeneration;
      update({ action: "switching", error: "", signInStatusError: "", signInUrl: "" });
      try {
        const result = await bridge.chatgptSwitchWorkspace({ sessionId, accountId });
        if (disposed || generation !== actionGeneration) return null;
        if (result.state === "browser") {
          loginId = result.loginId;
          update({ action: "waiting", signInUrl: result.authUrl });
          void poll(generation);
          return "browser";
        }
        requireHome(result.account);
        requireHome(result.sessions);
        if (!result.account.signedIn) throw new Error("ChatGPT switched workspaces without an authorized account.");
        readGeneration += 1;
        const observed = observeAccount(result.account);
        sessionsGeneration += 1;
        pendingSessions = null;
        update({ ...observed, account: result.account, loading: false, sessions: result.sessions, sessionsLoading: false, sessionsError: "", action: "idle" });
        return "switched";
      } catch (error) {
        if (disposed || generation !== actionGeneration) return null;
        await refresh();
        update({ action: "idle", error: message(error) });
        return "failed";
      }
    },
  };
}
