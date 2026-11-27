import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  horizontalListSortingStrategy,
  useSortable,
} from "@dnd-kit/sortable";
import { CSS as DndCss } from "@dnd-kit/utilities";
import { AnimatePresence, motion, useIsPresent, useReducedMotion } from "motion/react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type ButtonHTMLAttributes,
  type HTMLAttributes,
  type PointerEvent,
} from "react";
import { createPortal } from "react-dom";
import { OcMenuItem, OcMenuSeparator, OcMenuSurface } from "../../design-system/components";
import { useFloatingMenu } from "../../design-system/useFloatingMenu";
import { Glyph, type GlyphName } from "../IconGlyphs";
import "./paneTabs.css";

const TAB_MIN_WIDTH = 90;
const TAB_MAX_WIDTH = 240;
const TAB_EXIT_DURATION_MS = 150;
const TAB_COLLAPSED = {
  minWidth: 0,
  maxWidth: 0,
};
const TAB_EXPANDED = {
  minWidth: TAB_MIN_WIDTH,
  maxWidth: TAB_MAX_WIDTH,
};
const TAB_TRANSITION = {
  duration: TAB_EXIT_DURATION_MS / 1000,
  ease: [0.23, 1, 0.32, 1] as [number, number, number, number],
};

export type PaneTabPresentation = {
  contextMenu?: boolean;
  draggable?: boolean;
  glyph: GlyphName;
  id: string;
  preview?: boolean;
  title: string;
};

export type PaneTabContextAction = {
  disabled?: boolean;
  glyph?: GlyphName;
  id: string;
  label: string;
};

type Props<Tab extends PaneTabPresentation> = {
  activeTabId: string;
  ariaLabel: string;
  afterSticky?: ReactNode;
  canNativeDrag?: (tab: Tab) => boolean;
  className?: string;
  contextActions?: (tab: Tab) => PaneTabContextAction[];
  onCloseOtherTabs: (tabId: string) => void;
  onCloseTabsToRight: (tabId: string) => void;
  onContextAction?: (actionId: string, tab: Tab) => void;
  onTabClose: (tabId: string) => void;
  onTabWindowPointerDown?: (tab: Tab, event: PointerEvent<HTMLElement>) => void;
  onTabPin?: (tabId: string) => void;
  onTabReorder: (activeTabId: string, overTabId: string) => void;
  onTabSelect: (tabId: string) => void;
  stretch?: boolean;
  tabs: Tab[];
};

type PositionedMenu = {
  tabId: string;
  x: number;
  y: number;
};

export function PaneTabStrip<Tab extends PaneTabPresentation>({
  activeTabId,
  ariaLabel,
  afterSticky,
  canNativeDrag,
  className,
  contextActions,
  onCloseOtherTabs,
  onCloseTabsToRight,
  onContextAction,
  onTabClose,
  onTabWindowPointerDown,
  onTabPin,
  onTabReorder,
  onTabSelect,
  stretch = false,
  tabs,
}: Props<Tab>) {
  const [tabMenu, setTabMenu] = useState<PositionedMenu | null>(null);
  const [lockedTabWidth, setLockedTabWidth] = useState<number | null>(null);
  const [dragging, setDragging] = useState(false);
  const [dragPreview, setDragPreview] = useState<{ tab: Tab; width: number } | null>(null);
  const [overlayHost, setOverlayHost] = useState<HTMLElement | null>(null);
  const [retainedTabCount, setRetainedTabCount] = useState(tabs.length);
  const scrollRef = useRef<HTMLDivElement>(null);
  const previousTabCount = useRef(tabs.length);
  const tabListRef = useRef<HTMLDivElement>(null);
  const tabMenuRef = useRef<HTMLDivElement>(null);
  const reducedMotion = useReducedMotion();
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));

  useEffect(() => { setOverlayHost(document.body); }, []);


  useLayoutEffect(() => {
    if (tabs.length > retainedTabCount) setRetainedTabCount(tabs.length);
    if (tabs.length > previousTabCount.current) setLockedTabWidth(null);
    previousTabCount.current = tabs.length;
  }, [retainedTabCount, tabs.length]);

  useLayoutEffect(() => {
    const selected = Array.from(tabListRef.current?.querySelectorAll<HTMLElement>("[data-pane-tab-id]") ?? [])
      .find((element) => element.dataset.paneTabId === activeTabId);
    selected?.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
  }, [activeTabId]);

  useEffect(() => {
    const root = scrollRef.current?.parentElement;
    if (!root) return;
    const observer = new ResizeObserver(() => setLockedTabWidth(null));
    observer.observe(root);
    return () => observer.disconnect();
  }, []);

  const closeTab = useCallback((tabId: string, restoreFocus = false) => {
    if (restoreFocus) setLockedTabWidth(null);
    onTabClose(tabId);
    if (restoreFocus) window.requestAnimationFrame(() => {
      tabListRef.current?.querySelector<HTMLButtonElement>('[role="tab"][aria-selected="true"]:not(:disabled)')?.focus({ preventScroll: true });
    });
  }, [onTabClose]);

  const navigateTabs = useCallback((tabId: string, destination: "previous" | "next" | "first" | "last") => {
    setLockedTabWidth(null);
    const index = tabs.findIndex((tab) => tab.id === tabId);
    if (index < 0 || tabs.length === 0) return;
    const targetIndex = destination === "first"
      ? 0
      : destination === "last"
        ? tabs.length - 1
        : (index + (destination === "next" ? 1 : -1) + tabs.length) % tabs.length;
    const target = tabs[targetIndex];
    if (!target) return;
    onTabSelect(target.id);
    window.requestAnimationFrame(() => {
      const element = Array.from(tabListRef.current?.querySelectorAll<HTMLElement>("[data-pane-tab-id]") ?? [])
        .find((candidate) => candidate.dataset.paneTabId === target.id);
      element?.querySelector<HTMLButtonElement>('[role="tab"]')?.focus();
    });
  }, [onTabSelect, tabs]);

  const reorderTabs = useCallback((event: DragEndEvent) => {
    setDragging(false);
    setDragPreview(null);
    if (!event.over || event.active.id === event.over.id) return;
    onTabReorder(String(event.active.id), String(event.over.id));
  }, [onTabReorder]);

  const tabIndex = tabMenu ? tabs.findIndex((tab) => tab.id === tabMenu.tabId) : -1;
  const menuTab = tabIndex >= 0 ? tabs[tabIndex] : null;
  useFloatingMenu(Boolean(tabMenu && menuTab), tabMenuRef, () => setTabMenu(null), tabMenu ? { left: tabMenu.x, top: tabMenu.y } : null);
  const extraActions = menuTab ? contextActions?.(menuTab) ?? [] : [];
  const hasOtherTabs = Boolean(menuTab && tabs.some((tab) => tab.id !== menuTab.id));
  const hasTabsToRight = tabIndex >= 0 && tabIndex < tabs.length - 1;
  const layoutTabCount = Math.max(tabs.length, retainedTabCount);
  const tabListWidth = lockedTabWidth == null
    ? `clamp(${layoutTabCount * TAB_MIN_WIDTH}px, 100%, ${layoutTabCount * TAB_MAX_WIDTH}px)`
    : `${layoutTabCount * lockedTabWidth}px`;
  const tabListStyle = {
    width: tabListWidth,
  } as CSSProperties;
  const motionEnabled = !reducedMotion && !dragging;

  return (
    <div
      className={`pane-tabs${stretch ? " is-stretch" : ""}${className ? ` ${className}` : ""}`}
      onPointerLeave={() => setLockedTabWidth(null)}
      style={{ "--pane-tabs-width": `${layoutTabCount * TAB_MAX_WIDTH}px` } as CSSProperties}
    >
      <DndContext
        collisionDetection={closestCenter}
        onDragCancel={() => { setDragging(false); setDragPreview(null); }}
        onDragEnd={reorderTabs}
        onDragStart={(event) => {
          setDragging(true);
          const tab = tabs.find((candidate) => candidate.id === event.active.id);
          const width = event.active.rect.current.initial?.width;
          if (tab && width != null) setDragPreview({ tab, width });
        }}
        sensors={sensors}
      >
        <div
          className="pane-tabs-scroll"
          ref={scrollRef}
        >
          <SortableContext items={tabs.map((tab) => tab.id)} strategy={horizontalListSortingStrategy}>
            <div
              aria-label={ariaLabel}
              className="pane-tabs-list"
              onMouseDownCapture={(event) => {
                if (!(event.target instanceof Element)) return;
                if (event.button !== 1 && event.target.closest("[data-pane-tab-close-button]") == null) return;
                const tab = event.target.closest<HTMLElement>("[data-pane-tab-id]");
                if (tab) setLockedTabWidth(tab.offsetWidth);
              }}
              ref={tabListRef}
              role="tablist"
              style={tabListStyle}
            >
              <AnimatePresence initial={false} onExitComplete={() => setRetainedTabCount(tabs.length)}>
                {tabs.map((tab, index) => {
                  const active = tab.id === activeTabId;
                  const nextActive = tabs[index + 1]?.id === activeTabId;
                  const windowDragEnabled = tab.draggable !== false && Boolean(onTabWindowPointerDown && (canNativeDrag?.(tab) ?? true));
                  return (
                    <SortablePaneTab
                      active={active}
                      key={tab.id}
                      lockedWidth={lockedTabWidth}
                      motionEnabled={motionEnabled}
                      onActivate={() => onTabSelect(tab.id)}
                      onClose={(restoreFocus) => closeTab(tab.id, restoreFocus)}
                      onContextMenu={(x, y) => {
                        if (tab.contextMenu !== false) setTabMenu({ tabId: tab.id, x, y });
                      }}
                      onNavigate={(destination) => navigateTabs(tab.id, destination)}
                      onPin={onTabPin ? () => onTabPin(tab.id) : undefined}
                      onWindowPointerDown={windowDragEnabled ? (event) => onTabWindowPointerDown?.(tab, event) : undefined}
                      separator={index < tabs.length - 1 && !active && !nextActive}
                      tab={tab}
                    />
                  );
                })}
              </AnimatePresence>
            </div>
          </SortableContext>
        </div>
        {afterSticky != null ? <div className={`pane-tabs-actions${dragging ? " is-dragging" : ""}`}>{afterSticky}</div> : null}
        {overlayHost ? createPortal(<DragOverlay adjustScale={false} zIndex={2147483647}>
          {dragPreview ? <div aria-hidden="true" className="pane-tab-drag-preview" style={{ width: dragPreview.width }} inert>
            <PaneTabChrome active={dragPreview.tab.id === activeTabId} dragPreview tab={dragPreview.tab} />
          </div> : null}
        </DragOverlay>, overlayHost) : null}
      </DndContext>

      {tabMenu && menuTab ? createPortal(
        <OcMenuSurface
          aria-label="Tab actions"
          className="workspace-pane-tab-context-menu"
          data-settings-layer="true"
          role="menu"
          surfaceRef={tabMenuRef}
          style={{ left: tabMenu.x, top: tabMenu.y }}
        >
          {extraActions.map((action) => (
            <OcMenuItem
              disabled={action.disabled}
              key={action.id}
              label={action.label}
              leadingIcon={action.glyph}
              onClick={() => {
                setTabMenu(null);
                onContextAction?.(action.id, menuTab);
              }}
              role="menuitem"
            />
          ))}
          {extraActions.length ? <OcMenuSeparator /> : null}
          <OcMenuItem
            label="Close"
            onClick={() => {
              setTabMenu(null);
              closeTab(menuTab.id, true);
            }}
            role="menuitem"
          />
          <OcMenuSeparator />
          <OcMenuItem
            disabled={!hasOtherTabs}
            label="Close other tabs"
            onClick={() => {
              setTabMenu(null);
              onCloseOtherTabs(menuTab.id);
            }}
            role="menuitem"
          />
          <OcMenuItem
            disabled={!hasTabsToRight}
            label="Close tabs to the right"
            onClick={() => {
              setTabMenu(null);
              onCloseTabsToRight(menuTab.id);
            }}
            role="menuitem"
          />
        </OcMenuSurface>,
        document.body,
      ) : null}
    </div>
  );
}

function SortablePaneTab<Tab extends PaneTabPresentation>({
  active,
  lockedWidth,
  motionEnabled,
  onActivate,
  onClose,
  onContextMenu,
  onNavigate,
  onPin,
  onWindowPointerDown,
  separator,
  tab,
}: {
  active: boolean;
  lockedWidth: number | null;
  motionEnabled: boolean;
  onActivate: () => void;
  onClose: (restoreFocus?: boolean) => void;
  onContextMenu: (x: number, y: number) => void;
  onNavigate: (destination: "previous" | "next" | "first" | "last") => void;
  onPin?: () => void;
  onWindowPointerDown?: (event: PointerEvent<HTMLElement>) => void;
  separator: boolean;
  tab: Tab;
}) {
  const { isDragging, listeners, setActivatorNodeRef, setNodeRef, transform, transition } = useSortable({
    disabled: tab.draggable === false || Boolean(onWindowPointerDown),
    id: tab.id,
  });
  const present = useIsPresent();
  const motionRef = useRef<HTMLDivElement | null>(null);

  return (
    <motion.div
      animate={TAB_EXPANDED}
      className={`pane-tab-motion${isDragging ? " is-dragging" : ""}`}
      data-pane-tab-id={tab.id}
      data-pane-tab-draggable={tab.draggable === false ? "false" : undefined}
      exit={motionEnabled ? TAB_COLLAPSED : undefined}
      initial={motionEnabled ? TAB_COLLAPSED : false}
      inert={!present}
      onAnimationComplete={() => {
        if (present && active && motionEnabled) motionRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        onContextMenu(event.clientX, event.clientY);
      }}
      ref={(element) => { motionRef.current = element; setNodeRef(element); }}
      style={{
        flexBasis: lockedWidth ?? 0,
        flexGrow: lockedWidth == null ? 1 : 0,
        transform: DndCss.Translate.toString(transform),
        transition,
      }}
      transition={motionEnabled ? TAB_TRANSITION : { duration: 0 }}
    >
      <div className="pane-tab-slot">
        <PaneTabChrome
          active={active}
          closing={!present}
          onClose={onClose}
          tab={tab}
          tabRef={setActivatorNodeRef}
          buttonProps={{
            ...listeners,
            "aria-keyshortcuts": "Delete",
            onDoubleClick: onPin,
            onKeyDown: (event) => {
              if (isDragging || event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
              const rtl = getComputedStyle(event.currentTarget).direction === "rtl";
              const destination = event.key === "ArrowLeft"
                ? rtl ? "next" : "previous"
                : event.key === "ArrowRight"
                  ? rtl ? "previous" : "next"
                  : event.key === "Home"
                    ? "first"
                    : event.key === "End"
                      ? "last"
                      : null;
              if (destination) {
                event.preventDefault();
                onNavigate(destination);
                return;
              }
              if (event.key === "Delete") {
                event.preventDefault();
                onClose(true);
              }
            },
            onClick: (event) => { if (event.detail === 0) onActivate(); },
            onMouseDown: (event) => {
              if (event.button === 1) { event.preventDefault(); event.stopPropagation(); return; }
              if (event.button !== 0 || event.ctrlKey && /Mac/.test(navigator.platform)) { event.preventDefault(); return; }
              onActivate();
            },
            onMouseUp: (event) => {
              if (event.button === 1) {
                event.preventDefault();
                event.stopPropagation();
              }
            },
            onAuxClick: (event) => {
              if (event.button !== 1) return;
              event.preventDefault();
              event.stopPropagation();
              onClose();
            },
            onPointerDown: (event) => {
              if (event.button !== 0 || event.ctrlKey && /Mac/.test(navigator.platform)) {
                event.stopPropagation();
                return;
              }
              if (onWindowPointerDown) onWindowPointerDown(event);
              else listeners?.onPointerDown?.(event);
            },
          }}
        />
      </div>
      <i aria-hidden="true" className={`pane-tab-separator${separator ? " is-visible" : ""}`} />
    </motion.div>
  );
}

function PaneTabChrome({ active, buttonProps, closing = false, dragPreview = false, onClose, tab, tabRef }: {
  active: boolean;
  buttonProps?: ButtonHTMLAttributes<HTMLButtonElement>;
  closing?: boolean;
  dragPreview?: boolean;
  onClose?: (restoreFocus?: boolean) => void;
  tab: PaneTabPresentation;
  tabRef?: (element: HTMLDivElement | null) => void;
}) {
  const titleRef = useRef<HTMLSpanElement>(null);
  const chromeRef = useRef<HTMLDivElement>(null);
  const [titleOverflowing, setTitleOverflowing] = useState(false);

  useLayoutEffect(() => {
    const title = titleRef.current;
    const chrome = chromeRef.current;
    if (!title || !chrome) return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        if (entry.target === title) setTitleOverflowing(title.scrollWidth > title.clientWidth);
      }
    });
    observer.observe(chrome);
    observer.observe(title);
    return () => observer.disconnect();
  }, [tab.title]);

  const { onPointerDown, onDoubleClick, ...mainProps } = buttonProps ?? {};
  return <div
    onPointerDown={onPointerDown as HTMLAttributes<HTMLDivElement>["onPointerDown"]}
    onDoubleClick={onDoubleClick as HTMLAttributes<HTMLDivElement>["onDoubleClick"]}
    className={`pane-tab${active ? " is-active" : ""}${closing ? " is-closing" : ""}${dragPreview ? " is-drag-preview" : ""}`} ref={(element) => { chromeRef.current = element; tabRef?.(element); }}>
    <div aria-hidden="true" className="pane-tab-background" />
    <button
      {...mainProps}
      aria-label={tab.title}
      aria-selected={active}
      className="pane-tab-main"
      role="tab"
      tabIndex={active ? 0 : -1}
      type="button"
    >
      <span className="pane-tab-content">
        <Glyph className="pane-tab-icon" name={tab.glyph} size={16} />
        <span className="pane-tab-title">
          <span className={`pane-tab-title-text${titleOverflowing ? " is-overflowing" : ""}${tab.preview ? " is-preview" : ""}`} dir="auto" ref={titleRef}>{tab.title}</span>
        </span>
      </span>
    </button>
    {!closing && !dragPreview ? <button
      aria-label={`Close ${tab.title} tab`}
      className="pane-tab-close"
      data-pane-tab-close-button="true"
      onClick={(event) => { event.stopPropagation(); onClose?.(event.detail === 0 || chromeRef.current?.contains(document.activeElement)); }}
      onMouseDown={(event) => { event.preventDefault(); event.stopPropagation(); }}
      onPointerDown={(event) => event.stopPropagation()}
      tabIndex={active ? 0 : -1}
      type="button"
    >
      <Glyph className="pane-tab-close-icon" name="close" size={14} />
    </button> : null}
  </div>;
}
