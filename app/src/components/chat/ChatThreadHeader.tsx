import { Fragment, useSyncExternalStore, type ReactNode } from "react";
import type { ChatRun } from "../../chatRuns";
import { dispatchClientCommand } from "../../clientActions";
import { OcPopover } from "../../design-system/components";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import { dispatchOpenClientTaskActionsMenu } from "../../shell/taskActionsMenu";
import { clientTaskTitle, useClientTaskOrganization } from "../../shell/clientWorkspaces";
import { getShellSecondaryPaneOpen, subscribeShellSecondaryPaneOpen } from "../../shell/shellMetrics";
import { Glyph } from "../IconGlyphs";
import { ProjectLogo } from "../project/ProjectLogo";
import { useWorkspaceCompactTab } from "../shell/WorkspaceCompactTabContext";
import { ShellTooltip } from "../shell/ShellTooltip";

type Props = {
  actionsVisible?: boolean;
  accessProfile: "live" | "lab";
  activeRun: ChatRun | null;
  edgeScrolled: boolean;
  pinnedContextCount: number;
  pinnedContextOpen: boolean;
  projectContext?: { color?: string; icon?: string } | null;
  secondaryPaneOpen?: boolean;
  summaryDisplayMode: "overlay" | "shift" | "gutter";
  summaryOpen: boolean;
  summaryPopoverContent: ReactNode;
  mode: "window" | "pane";
  onPinnedContextOpenChange: (open: boolean) => void;
  onSummaryOpenChange: (open: boolean) => void;
};


export function ChatThreadHeader({
  actionsVisible = true,
  accessProfile,
  activeRun,
  edgeScrolled,
  pinnedContextCount,
  pinnedContextOpen,
  projectContext = null,
  secondaryPaneOpen,
  summaryDisplayMode,
  summaryOpen,
  summaryPopoverContent,
  mode,
  onPinnedContextOpenChange,
  onSummaryOpenChange,
}: Props) {
  const compactTab = useWorkspaceCompactTab();
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const taskOrganization = useClientTaskOrganization(accessProfile);
  const shellSecondaryPaneOpen = useSyncExternalStore(
    subscribeShellSecondaryPaneOpen,
    getShellSecondaryPaneOpen,
    () => false,
  );
  const resolvedSecondaryPaneOpen = secondaryPaneOpen ?? shellSecondaryPaneOpen;
  const splitLabel = resolvedSecondaryPaneOpen ? "Close secondary pane" : "Open secondary pane";
  const activeTitle = activeRun ? clientTaskTitle(activeRun.runId, activeRun.title, taskOrganization) : "";
  return (
    <header
      className={`app-header-tint pointer-events-none absolute ${mode === "pane" ? "chat-thread-header-pane" : "chat-thread-header-window"} inset-x-0 top-0 z-30 flex h-toolbar min-w-0 items-center`}
      data-app-shell-header-edge-scroll="true"
    >
      <div
        aria-hidden="true"
        className={`pointer-events-none relative h-full shrink-0 [container-type:inline-size]${mode === "pane" ? " chat-thread-header-pane-leading" : ""}`}
        style={{ width: mode === "pane" ? undefined : "12px", minWidth: 0 }}
      />
      <div className="pointer-events-none relative ms-2 flex h-full min-w-0 flex-1 isolate items-center gap-1.5 overflow-hidden [contain:layout_paint] pe-1.5">
        <div className="pointer-events-none flex min-w-0 flex-1 [&_button]:pointer-events-auto [&_input]:pointer-events-auto">
          <div
            className="mr-3 line-clamp-1 flex max-w-full min-w-0 items-center gap-2 truncate electron:h-toolbar"
            style={{ viewTransitionName: "header-title" }}
          >
            {activeRun ? (
              <>
                <ShellTooltip className="min-w-0 max-w-full" label={activeTitle}>
                  <button
                    aria-label={`Open recent tasks. Current task: ${activeTitle}`}
                    className="thread-header-title-button flex min-w-0 flex-1 items-center gap-2 truncate !px-0 !py-0 text-left text-sm text-token-foreground hover:opacity-80 electron:font-medium"
                    data-thread-title
                    onPointerDown={mode === "pane" ? compactTab?.onPointerDown : undefined}
                    onClick={() => dispatchClientCommand("open_command_menu", "surface")}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget, { x: event.clientX, y: event.clientY });
                    }}
                    type="button"
                  >
                    {projectContext ? (
                      <ProjectLogo color={projectContext.color || ""} icon={projectContext.icon || ""} iconSize={14} size={18} />
                    ) : activeRun.clientSurfaceRole === "task" && mode === "window" ? (
                      <Glyph className="icon-xs shrink-0 text-token-description-foreground" name="folder" size={15} />
                    ) : null}
                    <span className="thread-header-title-text text-fade-truncate block min-w-0">{activeTitle}</span>
                  </button>
                </ShellTooltip>
              </>
            ) : null}
          </div>
        </div>
      </div>
      {actionsVisible ? <div
        className="chat-thread-header-actions pointer-events-none relative flex h-full shrink-0 items-center gap-1"
        style={{ paddingInlineEnd: mode === "pane" ? "var(--workspace-pane-compact-controls-width, calc(3 * var(--spacing-token-button-composer) + 12px))" : "12px" }}
      >
        <ChatThreadHeaderActions
          activeRun={activeRun}
          pinnedContextCount={pinnedContextCount}
          pinnedContextOpen={pinnedContextOpen}
          summaryDisplayMode={summaryDisplayMode}
          summaryOpen={summaryOpen}
          summaryPopoverContent={summaryPopoverContent}
          onPinnedContextOpenChange={onPinnedContextOpenChange}
          onSummaryOpenChange={onSummaryOpenChange}
        />
        {mode === "window" ? (
          <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
            <ShellTooltip label={splitLabel} shortcut={formatKeyboardShortcuts(shortcuts.toggle_split ?? [])}>
              <button
                aria-label="Toggle secondary pane"
                aria-pressed={resolvedSecondaryPaneOpen}
                className="app-titlebar-action no-drag"
                data-state={resolvedSecondaryPaneOpen ? "open" : "closed"}
                onClick={() => dispatchClientCommand("toggle_split", "surface")}
                type="button"
              >
                <Glyph className="icon-xs" name="splitPane" size={16} />
              </button>
            </ShellTooltip>
          </div>
        ) : null}
      </div> : null}
      <div
        aria-hidden="true"
        className="chat-thread-top-fade"
        data-app-shell-main-content-top-fade={edgeScrolled ? "visible" : undefined}
      />
    </header>
  );
}

export function ChatThreadHeaderActions({
  activeRun,
  pinnedContextCount,
  pinnedContextOpen,
  summaryDisplayMode,
  summaryOpen,
  summaryPopoverContent,
  onPinnedContextOpenChange,
  onSummaryOpenChange,
}: Pick<Props,
  | "activeRun"
  | "pinnedContextCount"
  | "pinnedContextOpen"
  | "summaryDisplayMode"
  | "summaryOpen"
  | "summaryPopoverContent"
  | "onPinnedContextOpenChange"
  | "onSummaryOpenChange"
>) {
  const summaryLabel = summaryDisplayMode === "overlay" ? "Toggle summary" : "Toggle pinned summary";
  const summaryTrigger = activeRun ? (
    <ShellTooltip disabled={summaryOpen} label={summaryLabel}>
      <button
        aria-label={summaryLabel}
        aria-pressed={summaryOpen}
        className="app-titlebar-action no-drag"
        data-state={summaryOpen ? "open" : "closed"}
        data-task-summary-trigger="true"
        onClick={() => onSummaryOpenChange(!summaryOpen)}
        type="button"
      >
        <Glyph className="icon-xs" name="taskSummary" size={16} />
      </button>
    </ShellTooltip>
  ) : null;

  return (
    <Fragment>
      {activeRun?.clientSurfaceRole === "task" ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          <ShellTooltip label="Task actions">
            <button
              aria-haspopup="menu"
              aria-label="Task actions"
              className="app-titlebar-action no-drag"
              data-task-actions-run-id={activeRun.runId}
              onClick={(event) => dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget)}
              onKeyDown={(event) => {
                if (event.key !== "ArrowDown") return;
                event.preventDefault();
                dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget);
              }}
              type="button"
            >
              <Glyph className="icon-xs" name="more" size={16} />
            </button>
          </ShellTooltip>
        </div>
      ) : null}
      {pinnedContextCount > 0 ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          <ShellTooltip label={`${pinnedContextCount} pinned context ${pinnedContextCount === 1 ? "item" : "items"}`}>
            <button
              aria-label={pinnedContextOpen ? "Close pinned context" : "Open pinned context"}
              aria-pressed={pinnedContextOpen}
              className="app-titlebar-action no-drag"
              data-state={pinnedContextOpen ? "open" : "closed"}
              onClick={() => onPinnedContextOpenChange(!pinnedContextOpen)}
              type="button"
            >
              <Glyph className="icon-xs" name="pin" size={16} />
            </button>
          </ShellTooltip>
        </div>
      ) : null}
      {summaryTrigger ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          {summaryDisplayMode === "overlay" ? (
            <OcPopover
              align="end"
              anchor={summaryTrigger}
              className="task-summary-popover"
              label="Task summary"
              onDismiss={() => onSummaryOpenChange(false)}
              open={summaryOpen}
              panelClassName="task-summary-popover-panel"
              placement="bottom"
              sideOffset={8}
            >
              <div data-task-summary-popover="true">{summaryPopoverContent}</div>
            </OcPopover>
          ) : summaryTrigger}
        </div>
      ) : null}
    </Fragment>
  );
}
