import { useEffect, useState } from "react";
import { OcLoadingText } from "../../design-system/LoadingSkeleton";
import { chatgptAccountIdentity } from "../../account/chatgptAccountState";
import { useChatgptAccount } from "../../account/useChatgptAccount";
import { useChatgptUsage } from "../../account/useChatgptUsage";
import { accountPlanLabel } from "../../account/chatgptUsageModel";
import { activeChatgptSession, chatgptWorkspaceLabel, selectedChatgptWorkspace } from "../../account/chatgptWorkspaces";
import { OcButton, OcSettingsRow } from "../../design-system/components";
import { AccountLogoutDialog } from "../account/AccountLogoutDialog";
import { AccountSection, AccountUsageSections } from "../account/AccountUsage";
import { SettingsHeading } from "./SettingsHeading";
import { settingsTargetProps } from "./settingsCatalog";

export function AccountSettingsSection() {
  const state = useChatgptAccount();
  const account = state.account;
  const usage = useChatgptUsage(account);
  const [logoutOpen, setLogoutOpen] = useState(false);
  const busy = state.action !== "idle";
  const identity = chatgptAccountIdentity(account);
  const { loadSessions, sessions, sessionsLoading, sessionsError } = state;
  useEffect(() => {
    if (identity !== null && !sessions && !sessionsLoading && !sessionsError) void loadSessions?.();
  }, [identity, loadSessions, sessions, sessionsError, sessionsLoading]);
  const session = activeChatgptSession(sessions);
  const workspace = selectedChatgptWorkspace(session);
  const status = !account ? "Unavailable"
    : account.signedIn ? accountPlanLabel(account.planType) : "Signed out";
  const workspaceName = account?.workspaceName ?? (workspace ? chatgptWorkspaceLabel(workspace) : null);
  async function refresh() {
    const refreshed = await state.refresh?.();
    if (refreshed?.signedIn) await Promise.all([usage.refresh?.(), state.loadSessions?.()]);
  }
  return <section className="settings-section account-settings">
    <SettingsHeading title="Account" actions={<OcButton disabled={busy || state.loading || usage.loading} onClick={() => void refresh()}>Refresh</OcButton>} />
    {state.error ? <div className="account-inline-notice account-error" role="alert"><span>{state.error}</span>
      <OcButton disabled={busy || state.loading} onClick={() => void state.refresh?.()}>Retry account check</OcButton>
    </div> : null}
    <AccountSection title="Account" {...settingsTargetProps("account.identity")}>
      <div className="account-row-list">
        <OcSettingsRow label="Status" control={<span className="account-value">{state.loading && !account ? <OcLoadingText label="Checking account…">Account status</OcLoadingText> : status}</span>} />
        <OcSettingsRow label="Email" control={<span className="account-value">{state.loading && !account ? <OcLoadingText label="Loading account email…">Account email address</OcLoadingText> : account?.email ?? "Unavailable"}</span>} />
        {account?.signedIn ? <OcSettingsRow label="Workspace" control={<span className="account-value">{workspaceName
          ?? (sessionsLoading ? <OcLoadingText label="Loading workspace…">Workspace name</OcLoadingText> : "Unavailable")}</span>} /> : null}
      </div>
    </AccountSection>
    {account?.signedIn ? <AccountSection title="Workspaces" description="Octopus uses the selected workspace for new ChatGPT requests and reloads the model list after a switch."
      actions={state.action === "switching" ? <OcLoadingText label="Switching workspace…" /> : undefined} {...settingsTargetProps("account.workspace")}>
      {sessionsError ? <div className="account-inline-notice account-error" role="alert"><span>Workspaces are unavailable. {sessionsError}</span>
        <OcButton disabled={busy || sessionsLoading} onClick={() => void state.loadSessions?.()}>Retry</OcButton>
      </div> : null}
      {session ? <div className="account-row-list">
        {session.workspaces.map((option) => <OcSettingsRow key={option.accountId} label={chatgptWorkspaceLabel(option)}
          control={option.accountId === session.selectedWorkspaceId ? <span className="account-value">Current</span>
            : <OcButton disabled={busy || state.loading} onClick={() => void state.switchWorkspace?.(session.sessionId, option.accountId)}>Switch</OcButton>} />)}
      </div> : sessionsLoading ? <OcLoadingText label="Loading workspaces…">Workspaces</OcLoadingText>
        : !sessionsError ? <p className="account-section-footer" role="status">No workspaces were reported for this sign-in.</p> : null}
    </AccountSection> : null}
    <div {...settingsTargetProps("account.usage")} className="account-usage-sections">
      <AccountUsageSections state={usage} signedIn={Boolean(account?.signedIn)} accountUnavailable={!account && !state.loading} accountLoading={state.loading && !account} />
    </div>
    <AccountSection title="Sign-in" {...settingsTargetProps("account.logout")}>
      {state.signInStatusError ? <div className="account-inline-notice account-error" role="alert"><span>{state.signInStatusError}</span></div> : null}
      {state.action === "waiting" || state.action === "cancelling" ? <div className="account-section-actions">
        {state.signInStatusError ? <OcButton onClick={() => void state.retrySignInStatus?.()}>Retry sign-in check</OcButton> : null}
        <OcButton disabled={state.action === "cancelling"} onClick={() => void state.cancel?.()}>Cancel sign-in</OcButton>
      </div> : null}
      <div className="account-row-list">
        {account?.signedIn ? <OcSettingsRow label="Add account" description="Sign in with another ChatGPT account. Codex keeps one sign-in at a time, so the new account replaces this one."
          control={<OcButton disabled={busy || state.loading || !state.signIn} onClick={() => void state.signIn?.()}>{state.action === "starting" ? "Opening browser…" : "Add account"}</OcButton>} /> : null}
        <OcSettingsRow label={account?.signedIn ? "Log out of ChatGPT" : "Sign in with ChatGPT"} control={account?.signedIn
          ? <OcButton disabled={busy || state.loading} onClick={() => setLogoutOpen(true)}>{state.action === "signing-out" ? "Logging out…" : "Log out"}</OcButton>
          : <OcButton disabled={busy || state.loading || !state.signIn} onClick={() => void state.signIn?.()}>{state.action === "starting" ? "Opening browser…" : "Continue with ChatGPT"}</OcButton>} />
      </div>
      <p className="account-section-footer">This sign-in is shared with Codex on this computer. Adding an account, switching workspaces or logging out here also changes the account your own Codex CLI uses.</p>
    </AccountSection>
    <AccountLogoutDialog account={account} open={logoutOpen} busy={busy || state.loading} onClose={() => setLogoutOpen(false)} onConfirm={() => void state.logout?.()} />
  </section>;
}
