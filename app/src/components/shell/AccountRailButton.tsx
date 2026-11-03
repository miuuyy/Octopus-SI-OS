import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import type { RuntimeHostView } from "../../runtimeHost";
import { MessageTooltip } from "../chat/MessageTooltip";
import { useChatgptAccount } from "../../account/useChatgptAccount";
import { useChatgptUsage } from "../../account/useChatgptUsage";
import { accountPlanLabel, accountRemainingLabel, accountUsageGroups } from "../../account/chatgptUsageModel";
import { activeChatgptSession, chatgptWorkspaceLabel } from "../../account/chatgptWorkspaces";
import "../account/account.css";

export function AccountRailButton({ accessProfile, runtimeHost, onLogOut, onOpenSettings }: {
  accessProfile: "live" | "lab";
  runtimeHost: RuntimeHostView;
  onLogOut: () => void;
  onOpenSettings: (section?: string) => void;
}) {
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const state = useChatgptAccount(accessProfile === "live");
  const { account } = state;
  const trigger = useRef<HTMLButtonElement>(null);
  const openingSettings = useRef(false);
  const [open, setOpen] = useState(false);
  const usage = useChatgptUsage(account, accessProfile === "live" && open);
  const { loadSessions, sessions, sessionsLoading, sessionsError } = state;
  const listAccounts = accessProfile === "live" && open && account?.signedIn === true;
  const listedThisOpening = useRef(false);
  useEffect(() => {
    if (!listAccounts) { listedThisOpening.current = false; return; }
    if (listedThisOpening.current || sessions || sessionsLoading) return;
    listedThisOpening.current = true;
    void loadSessions?.();
  }, [listAccounts, loadSessions, sessions, sessionsLoading]);
  const session = activeChatgptSession(sessions);
  useLayoutEffect(() => {
    if (open || !openingSettings.current) return;
    openingSettings.current = false;
    document.getElementById("settings-search")?.focus({ preventScroll: true });
  }, [open]);
  const name = accessProfile === "lab" ? "Test" : "Administrator";
  const accountLabel = accessProfile !== "live" ? "Octopus OS" : state.loading && !account ? "Checking ChatGPT…" : account?.signedIn
    ? `ChatGPT${account.planType ? ` ${accountPlanLabel(account.planType)}` : ""}${account.workspaceName ? ` · ${account.workspaceName}` : ""}`
    : account ? "ChatGPT disconnected" : "ChatGPT unavailable";
  const usageWindow = usage.value ? accountUsageGroups(usage.value)[0]?.windows.find((row) => row.reported && row.remaining !== null) : undefined;
  const openSettings = (section?: string) => { openingSettings.current = true; trigger.current?.focus(); onOpenSettings(section); };
  const accountBusy = state.action !== "idle" || state.loading;
  async function switchWorkspace(sessionId: string, accountId: string) {
    setOpen(false);
    const outcome = await state.switchWorkspace?.(sessionId, accountId);
    if (outcome === "browser" || outcome === "failed") onOpenSettings("account");
  }
  function addAccount() {
    openSettings("account");
    void state.signIn?.();
  }
  const storageBlocked = accessProfile === "live" && runtimeHost.storageHealth && runtimeHost.storageHealth.status !== "writable";
  const connected = runtimeHost.available && runtimeHost.ready && !storageBlocked;
  const settingsShortcut = formatKeyboardShortcuts(shortcuts.open_settings ?? []);
  const avatar = <span className="navigation-rail-avatar" aria-hidden="true">{name.slice(0, 1)}</span>;

  return (
    <div className="navigation-rail-account">
      <MessageTooltip label="Account" side="right" delayMs={300} disabled={open}>
        <button
          aria-expanded={open}
          aria-haspopup="menu"
          aria-label={`Account: ${name}`}
          className="oc-octopus-icon-control no-drag"
          data-appearance="rail"
          data-state={open ? "open" : "closed"}
          onClick={() => setOpen(!open)}
          onKeyDown={(event) => {
            if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
            event.preventDefault();
            event.stopPropagation();
            setOpen(true);
          }}
          ref={trigger}
          type="button"
        >{avatar}</button>
      </MessageTooltip>
      <MessageTooltip asChild label={storageBlocked ? runtimeHost.storageHealth!.message || "Home storage unavailable" : connected ? "Runtime connected" : runtimeHost.error || runtimeHost.phase} side="right" delayMs={300}><span
        aria-label={storageBlocked ? "Home storage unavailable" : connected ? "Runtime connected" : "Runtime unavailable"}
        className={`sidebar-runtime-dot navigation-rail-account-status${connected ? " is-ready" : storageBlocked || runtimeHost.phase === "degraded" ? " is-error" : ""}`}
        role="status"
      /></MessageTooltip>
      <OcDropdown align="end" alignOffset={-4} anchorRef={trigger} aria-label="Account" className="account-menu" contentWidth="sidebar" open={open} onOpenChange={setOpen} onCloseAutoFocus={(event) => {
        if (openingSettings.current) event.preventDefault();
      }} side="right">
        <div className="account-menu-identity">{avatar}<div><strong>{name}</strong><small>{accountLabel}</small></div></div>
        {listAccounts && sessionsLoading && !session ? <OcMenuItem disabled label="Loading accounts…" role="menuitem" /> : null}
        {listAccounts && !sessionsLoading && sessionsError && !session ? <OcMenuItem descriptionAllowWrap disabled description={sessionsError} label="Workspaces unavailable" role="menuitem" /> : null}
        {listAccounts && session && session.workspaces.length > 1 ? session.workspaces.map((workspace) => <OcMenuItem key={workspace.accountId}
          aria-checked={workspace.accountId === session.selectedWorkspaceId} disabled={accountBusy} label={chatgptWorkspaceLabel(workspace)} role="menuitemradio"
          selected={workspace.accountId === session.selectedWorkspaceId}
          onClick={() => { if (workspace.accountId !== session.selectedWorkspaceId) void switchWorkspace(session.sessionId, workspace.accountId); }} />) : null}
        {accessProfile === "live" && account?.signedIn ? <OcMenuItem disabled={accountBusy} label="Add account" leadingIcon="plus" onClick={addAccount} role="menuitem" /> : null}
        <div className="account-menu-separator" role="separator" />
        <OcMenuItem label="Settings" leadingIcon="settings" onClick={() => openSettings()} role="menuitem" trailing={settingsShortcut ? <span className="text-xs text-token-description-foreground">{settingsShortcut}</span> : undefined} />
        {accessProfile === "live" && account?.signedIn ? <OcMenuItem label="Usage" leadingIcon="usage" onClick={() => openSettings("account")} role="menuitem"
          trailing={usageWindow ? <span className="whitespace-nowrap text-token-description-foreground">{accountRemainingLabel(usageWindow.remaining)}</span> : undefined} /> : null}
        <div className="account-menu-separator" role="separator" />
        <OcMenuItem label="Lock Octopus" leadingIcon="logOut" onClick={onLogOut} role="menuitem" />
      </OcDropdown>
    </div>
  );
}
