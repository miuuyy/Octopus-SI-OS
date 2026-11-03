import type { OctopusChatgptAccount, OctopusChatgptUsage } from "../electron";

export type ChatgptUsageState = { identity: string | null; value: OctopusChatgptUsage | null; loading: boolean; error: string };
export const EMPTY_CHATGPT_USAGE_STATE: ChatgptUsageState = { identity: null, value: null, loading: false, error: "" };

export function accountUsageIdentity(account: OctopusChatgptAccount | null): string | null {
  return account?.signedIn ? JSON.stringify([account.credentialHome, account.email, account.workspaceName ?? null, account.planType]) : null;
}

export function createChatgptUsageState(bridge: Pick<NonNullable<Window["octopusTransport"]>, "chatgptRateLimits">) {
  let state = EMPTY_CHATGPT_USAGE_STATE;
  let account: OctopusChatgptAccount | null = null;
  let generation = 0;
  let pending: Promise<OctopusChatgptUsage | null> | null = null;
  const listeners = new Set<() => void>();
  function update(patch: Partial<ChatgptUsageState>) {
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  }
  function refresh(): Promise<OctopusChatgptUsage | null> {
    if (state.identity === null) return Promise.resolve(null);
    if (pending) return pending;
    const currentGeneration = ++generation;
    update({ loading: true, error: "" });
    const request = Promise.resolve().then(() => bridge.chatgptRateLimits()).then((value) => {
      if (currentGeneration !== generation) return null;
      update({ value, loading: false });
      return value;
    }, (error) => {
      if (currentGeneration !== generation) return null;
      update({ value: null, loading: false, error: error instanceof Error ? error.message : String(error) });
      return null;
    }).finally(() => { if (pending === request) pending = null; });
    pending = request;
    return request;
  }
  return {
    getSnapshot: () => state,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    refresh,
    observe(nextAccount: OctopusChatgptAccount | null) {
      const identity = accountUsageIdentity(nextAccount);
      if (identity === null || identity !== state.identity) {
        generation += 1;
        pending = null;
        update({ ...EMPTY_CHATGPT_USAGE_STATE, identity, loading: identity !== null });
      }
      if (identity === null) { account = null; return Promise.resolve(null); }
      if (account === nextAccount) return pending ?? Promise.resolve(state.value);
      account = nextAccount;
      return refresh();
    },
  };
}
