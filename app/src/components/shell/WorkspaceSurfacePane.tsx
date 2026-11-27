import { useId, useLayoutEffect, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { OcMenuItem } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { OcDropdown } from "../../design-system/OcDropdown";
import {
  PaneTabStrip,
  type PaneTabContextAction,
  type PaneTabPresentation,
} from "./PaneTabStrip";
import { PaneHeaderButton } from "./PaneHeaderButton";
import { SurfaceRenderBoundary } from "./SurfaceBoundary";
import { WorkspaceCompactTabProvider, type WorkspaceCompactTabDrag } from "./WorkspaceCompactTabContext";
import { WorkspacePaneHeaderActionsProvider } from "./WorkspacePaneHeaderActions";
import { secondaryPaneUsesTabStrip } from "./paneChromeOwnership";
import { useWindowPaneControlsHost } from "./WindowPaneControls";
import { useWorkspacePanePresentation } from "./WorkspacePaneManager";
import { ShellTooltip } from "./ShellTooltip";
import "./workspacePaneLauncher.css";

export type WorkspacePaneTab = PaneTabPresentation & {
  launcher?: boolean;
  showWhenSingle?: boolean;
};

export type WorkspacePaneAddAction = {
  description?: string;
  glyph: GlyphName;
  group?: { id: string; title: string; glyph: GlyphName };
  id: string;
  moreOnly?: boolean;
  section?: string;
  shortcut?: string;
  title: string;
};

type Props = {
  controlsInWindowHeader?: boolean;
  activeTabId: string;
  addActions: WorkspacePaneAddAction[];
  canNativeDrag?: (tab: WorkspacePaneTab) => boolean;
  focused: boolean;
  handoffPending?: boolean;
  tabs: WorkspacePaneTab[];
  contextActions?: (tab: WorkspacePaneTab) => PaneTabContextAction[];
  onAddAction: (actionId: string) => void;
  onNewTab?: () => void;
  onClose: () => void;
  onCloseOtherTabs: (tabId: string) => void;
  onCloseTabsToRight: (tabId: string) => void;
  onFocusedChange: (focused: boolean) => void;
  onContextAction?: (actionId: string, tab: WorkspacePaneTab) => void;
  onTabClose: (tabId: string) => void;
  onTabWindowPointerDown?: (tab: WorkspacePaneTab, event: PointerEvent<HTMLElement>) => void;
  onTabReorder: (activeTabId: string, overTabId: string) => void;
  onTabSelect: (tabId: string) => void;
  children: ReactNode;
};

export function WorkspaceSurfacePane({
  controlsInWindowHeader = false,
  activeTabId,
  addActions,
  canNativeDrag,
  focused,
  handoffPending,
  tabs,
  contextActions,
  onAddAction,
  onNewTab,
  onClose,
  onCloseOtherTabs,
  onCloseTabsToRight,
  onFocusedChange,
  onContextAction,
  onTabClose,
  onTabWindowPointerDown,
  onTabReorder,
  onTabSelect,
  children,
}: Props) {
  const windowControlsHost = useWindowPaneControlsHost();
  const { compactOverlay, managerId } = useWorkspacePanePresentation();
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [headerActionsHost, setHeaderActionsHost] = useState<HTMLDivElement | null>(null);
  const addButtonRef = useRef<HTMLButtonElement>(null);
  const launcherRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const showLauncher = tabs.length === 0 || tabs.some((tab) => tab.id === activeTabId && tab.launcher);

  useLayoutEffect(() => {
    if (!showLauncher) return;
    launcherRef.current?.querySelector<HTMLButtonElement>("button")?.focus({ preventScroll: true });
  }, [activeTabId, showLauncher]);

  const closeLauncher = () => {
    if (tabs.length === 0) onClose();
    else {
      onTabClose(activeTabId);
      window.requestAnimationFrame(() => contentRef.current?.focus({ preventScroll: true }));
    }
  };
  const selectAction = (actionId: string) => {
    setAddMenuOpen(false);
    onAddAction(actionId);
  };

  const compactTab = tabs.length === 1 ? tabs[0] : null;
  const tabStripVisible = secondaryPaneUsesTabStrip(tabs);
  const showTabStrip = tabStripVisible || showLauncher;
  const compactTabDrag: WorkspaceCompactTabDrag | null = compactTab && !tabStripVisible ? {
    tabId: compactTab.id,
    onPointerDown: onTabWindowPointerDown && (canNativeDrag?.(compactTab) ?? true) ? (event) => onTabWindowPointerDown(compactTab, event) : undefined,
  } : null;
  const addControl = onNewTab ? (
    <div className="workspace-pane-tab-add no-drag">
      <PaneHeaderButton
        glyph="plus"
        label="New tab"
        buttonRef={addButtonRef}
        onClick={onNewTab}
        onContextMenu={(event) => { event.preventDefault(); setAddMenuOpen(true); }}
      />
    </div>
  ) : null;
  const paneControls = (
    <div className="workspace-pane-tab-controls no-drag">
      {!compactOverlay ? <PaneHeaderButton
        className="workspace-surface-focus"
        glyph={focused ? "restore" : "expand"}
        label={focused ? "Restore split workspace" : "Focus this workspace"}
        onClick={() => onFocusedChange(!focused)}
      /> : null}
      <PaneHeaderButton
        className="workspace-surface-close"
        glyph={controlsInWindowHeader ? "splitPane" : "close"}
        label="Close secondary pane"
        selected={controlsInWindowHeader}
        onClick={onClose}
      />
    </div>
  );
  const header = (
    <header className={`workspace-surface-pane-header draggable${controlsInWindowHeader ? " is-window-header" : ""}`}
      data-pane-role="secondary" data-pane-manager-controls="true" data-pane-manager-id={managerId} inert={handoffPending}>
      {showTabStrip ? <PaneTabStrip
        afterSticky={addControl}
        activeTabId={activeTabId}
        ariaLabel="Secondary workspace tabs"
        canNativeDrag={canNativeDrag}
        contextActions={contextActions}
        onCloseOtherTabs={onCloseOtherTabs}
        onCloseTabsToRight={onCloseTabsToRight}
        onContextAction={onContextAction}
        onTabClose={onTabClose}
        onTabWindowPointerDown={onTabWindowPointerDown}
        onTabReorder={onTabReorder}
        onTabSelect={onTabSelect}
        stretch
        tabs={tabs}
      /> : <div className="workspace-pane-compact-header-spacer" />}
      <div className="workspace-pane-header-actions no-drag" hidden={showLauncher} ref={setHeaderActionsHost} />
      {!showTabStrip ? addControl : null}
      {paneControls}
    </header>
  );

  return (
    <WorkspacePaneHeaderActionsProvider host={controlsInWindowHeader || tabStripVisible ? headerActionsHost : null}>
      <div
        className={`workspace-surface-pane @container/app-shell-detail-panel flex h-full min-h-0 flex-col${showTabStrip ? " has-tab-strip" : " has-compact-chrome"}${controlsInWindowHeader ? " has-window-controls" : ""}${focused ? " is-focused" : ""}`}
        data-app-shell-tabs={showTabStrip || undefined}
        data-pane-role="secondary"
        inert={handoffPending}
      >
        {!controlsInWindowHeader && showTabStrip ? header : !controlsInWindowHeader ? (
          <div className="workspace-surface-pane-compact-chrome">
            {addControl}
            {paneControls}
          </div>
        ) : null}
        {showLauncher ? <div className="workspace-pane-launcher-host" ref={launcherRef} onKeyDown={(event) => {
          if (event.key !== "Escape" || event.defaultPrevented || !showLauncher) return;
          event.preventDefault();
          event.stopPropagation();
          closeLauncher();
        }}><WorkspacePaneLauncher actions={addActions} onAction={selectAction} /></div> : null}
        <div className="workspace-surface-pane-content relative flex min-h-0 flex-1 flex-col overflow-hidden" hidden={showLauncher} inert={showLauncher || undefined} ref={contentRef} tabIndex={-1}>
          <WorkspaceCompactTabProvider value={compactTabDrag}>
            <SurfaceRenderBoundary resetKey={activeTabId || "launcher"}>
              {children}
            </SurfaceRenderBoundary>
          </WorkspaceCompactTabProvider>
        </div>

        {controlsInWindowHeader && windowControlsHost ? createPortal(header, windowControlsHost) : null}

        <OcDropdown anchorRef={addButtonRef} align="end" aria-label="New tab" onOpenChange={setAddMenuOpen} open={addMenuOpen && addActions.length > 0} style={{ width: 280 }}>
          <WorkspacePaneActionMenu actions={addActions} onAction={selectAction} />
        </OcDropdown>
      </div>
    </WorkspacePaneHeaderActionsProvider>
  );
}

export function WorkspacePaneLauncher({ actions, onAction }: {
  actions: WorkspacePaneAddAction[];
  onAction: (actionId: string) => void;
}) {
  const headingId = useId();
  const [moreOpen, setMoreOpen] = useState(false);
  const moreButtonRef = useRef<HTMLButtonElement>(null);
  const tools = actions.filter((action) => !action.moreOnly && !action.group);
  const additionalActions = actions.filter((action) => action.moreOnly || action.group);
  const visibleActions = tools.slice(0, tools.length > 6 || additionalActions.length ? 5 : 6);
  const moreActions = [...tools.slice(visibleActions.length), ...additionalActions];
  return (
    <div className="workspace-pane-launcher">
      <section className="workspace-pane-launcher-section" aria-labelledby={headingId}>
        <h3 id={headingId}>Tools</h3>
        <div className="workspace-pane-launcher-list">
        {actions.length === 0 ? (
          <span className="workspace-pane-launcher-empty">No tabs are available for this task</span>
        ) : visibleActions.map((action) => (
            <ShellTooltip className="min-w-0" key={action.id} label={action.title} shortcut={action.shortcut}>
              <button
                aria-label={action.title}
                className="workspace-pane-launcher-item"
                onClick={() => onAction(action.id)}
                type="button"
              >
                <Glyph className="icon-xs shrink-0" name={action.glyph} size={16} />
                <span>{action.title}</span>
                {action.shortcut ? <kbd className="workspace-pane-shortcut">{action.shortcut}</kbd> : null}
              </button>
            </ShellTooltip>
        ))}
        {moreActions.length ? <button
          aria-expanded={moreOpen}
          aria-haspopup="menu"
          className="workspace-pane-launcher-item"
          onClick={() => setMoreOpen((open) => !open)}
          ref={moreButtonRef}
          type="button"
        >
          <Glyph name="more" size={16} />
          <span>More tools…</span>
          <Glyph name="chevronDown" size={14} />
        </button> : null}
        </div>
        <OcDropdown anchorRef={moreButtonRef} align="end" aria-label="More tools" onOpenChange={setMoreOpen} open={moreOpen} style={{ width: 280 }}>
          <WorkspacePaneActionMenu actions={moreActions} onAction={(id) => { setMoreOpen(false); onAction(id); }} />
        </OcDropdown>
      </section>
    </div>
  );
}

function WorkspacePaneActionMenu({ actions, onAction }: {
  actions: WorkspacePaneAddAction[];
  onAction: (actionId: string) => void;
}) {
  const [groupId, setGroupId] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const previousGroupRef = useRef<string | null>(null);
  const groups = new Map<string, WorkspacePaneAddAction[]>();
  const entries: WorkspacePaneAddAction[] = [];
  for (const action of actions) {
    if (!action.group) { entries.push(action); continue; }
    const existing = groups.get(action.group.id);
    if (existing) existing.push(action);
    else {
      groups.set(action.group.id, [action]);
      entries.push({ ...action.group, id: `group:${action.group.id}`, group: action.group, section: action.section });
    }
  }
  const selected = groupId ? groups.get(groupId) : undefined;
  const group = selected?.[0].group;
  const visible = selected ?? entries;

  useLayoutEffect(() => {
    const current = group?.id ?? null;
    const previous = previousGroupRef.current;
    if (previous === current) return;
    previousGroupRef.current = current;
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
    const target = current ? items[0] : items.find((item) => item.dataset.actionId === `group:${previous}`);
    target?.focus({ preventScroll: true });
  }, [group?.id]);

  return <div ref={menuRef} onKeyDown={(event) => {
    if (!group || !["Escape", "ArrowLeft"].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    setGroupId(null);
  }}>
    {group ? <>
      <OcMenuItem aria-label="Back to tools" label={group.title} leadingIcon="navigateBack" role="menuitem" onClick={(event) => {
        event.preventDefault();
        setGroupId(null);
      }} />
      <div aria-hidden="true" className="workspace-pane-tab-menu-separator" />
    </> : null}
    {visible.map((action, index) => <div key={action.id}>
      {!group && index > 0 && action.section !== visible[index - 1].section ? <div aria-hidden="true" className="workspace-pane-tab-menu-separator" /> : null}
      <OcMenuItem label={action.title} description={group ? action.description : undefined} leadingIcon={action.glyph} role="menuitem"
        data-action-id={action.id} data-text-value={action.title} title={action.description ? `${action.title}\n${action.description}` : action.title}
        onClick={(event) => {
          if (!group && action.group) { event.preventDefault(); setGroupId(action.group.id); }
          else onAction(action.id);
        }}
        trailing={!group && action.group ? <><span>{groups.get(action.group.id)?.length}</span><Glyph className="-rotate-90" name="chevronDown" size={14} /></>
          : action.shortcut ? <kbd className="workspace-pane-shortcut">{action.shortcut}</kbd> : undefined} />
    </div>)}
  </div>;
}
