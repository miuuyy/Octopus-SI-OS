import { useEffect, useSyncExternalStore } from "react";
import { createChatgptAccountState, EMPTY_CHATGPT_ACCOUNT_STATE } from "./chatgptAccountState";
import { recordChatgptSignIn } from "../onboarding/chatgptSignIn";

type AccountConnection = {
  controller: ReturnType<typeof createChatgptAccountState>;
  home?: string;
  connections: number;
  disconnect: (() => void) | null;
};
let connection: AccountConnection | null = null;
const unavailable = { ...EMPTY_CHATGPT_ACCOUNT_STATE, loading: false, error: "The native ChatGPT account connection is unavailable." };
const noopSubscribe = () => () => {};

function accountConnection(home?: string) {
  const bridge = window.octopusTransport;
  if (!bridge?.chatgptAccount) return null;
  if (!connection || home !== undefined && connection.home !== home) {
    connection?.controller.dispose();
    recordChatgptSignIn(false);
    connection = { controller: createChatgptAccountState(bridge, home), home, connections: 0, disconnect: null };
  }
  return connection;
}

export function useChatgptAccount(enabled = true, home?: string) {
  const scoped = enabled || home !== undefined ? accountConnection(home) : null;
  const current = enabled ? scoped?.controller : null;
  const state = useSyncExternalStore(current?.subscribe ?? noopSubscribe,
    current?.getSnapshot ?? (() => enabled ? unavailable : EMPTY_CHATGPT_ACCOUNT_STATE));
  useEffect(() => {
    if (!current || !scoped) return;
    scoped.connections += 1;
    if (scoped.connections === 1) {
      const stopChanges = current.connect();
      const onFocus = () => { void current.refresh(); };
      window.addEventListener("focus", onFocus);
      scoped.disconnect = () => { stopChanges(); window.removeEventListener("focus", onFocus); };
      void current.refresh();
    }
    return () => {
      scoped.connections -= 1;
      if (scoped.connections === 0) { scoped.disconnect?.(); scoped.disconnect = null; }
    };
  }, [current, scoped]);
  useEffect(() => {
    if (!enabled || scoped !== connection) return;
    recordChatgptSignIn(state.account?.signedIn === true);
  }, [enabled, scoped, state.account]);
  return { ...state, refresh: current?.refresh, loadSessions: current?.loadSessions, switchWorkspace: current?.switchWorkspace,
    retrySignInStatus: current?.retrySignInStatus, signIn: current?.signIn, cancel: current?.cancel, logout: current?.logout };
}
