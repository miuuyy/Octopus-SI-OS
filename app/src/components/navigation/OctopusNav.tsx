import { useShouldReduceMotion } from "../../appearance";
import type { MouseEvent, ReactNode } from "react";
import { AnimatePresence, motion } from "motion/react";
import { OctopusMarquee } from "./OctopusMarquee";
import { SidebarWorkingIndicator } from "./SidebarWorkingIndicator";
import { SidebarTaskStatus } from "./SidebarTaskStatus";
import { TaskHoverCard, type TaskHoverCardDetails } from "./TaskHoverCard";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import type { SidebarDragHandle } from "./SidebarDnd";

export type OctopusRowAction = {
  glyph: GlyphName;
  label: string;
  disabled?: boolean;
  onClick: (event: MouseEvent<HTMLButtonElement>) => void;
};

export function OctopusNavItem({
  active = false,
  className,
  disabled = false,
  dragHandle,
  expanded,
  glyph,
  interactiveTrailing,
  label,
  action,
  onClick,
  trailing,
}: {
  active?: boolean;
  className?: string;
  disabled?: boolean;
  dragHandle?: SidebarDragHandle;
  expanded?: boolean;
  glyph: GlyphName;
  interactiveTrailing?: ReactNode;
  label: ReactNode;
  action?: OctopusRowAction;
  onClick: () => void;
  trailing?: ReactNode;
}) {
  const rowClassName = cx(
    "focus-visible:outline-token-border relative flex h-[var(--height-token-row)] w-full shrink-0 cursor-interaction items-center gap-2 overflow-hidden sidebar-item px-[var(--padding-row-cell-x,var(--padding-row-x))] py-row-y text-left text-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-50",
    active ? "bg-token-list-hover-background" : "hover:bg-token-list-hover-background",
    className,
  );
  const content = (
    <div className={cx(
      "flex min-w-0 flex-1 items-center gap-2 text-base",
      active ? "text-token-list-active-selection-foreground" : "text-token-foreground",
    )}>
      <span
        {...dragHandle?.listeners}
        className={cx(
          "sidebar-item-icon flex size-6 shrink-0 items-center justify-center",
          dragHandle && "cursor-grab touch-none active:cursor-grabbing",
        )}
        ref={dragHandle?.setActivatorNodeRef}
      >
        <Glyph
          className={cx("icon-xs", active && "text-token-list-active-selection-icon-foreground")}
          name={glyph}
          size={16}
        />
      </span>
      <span className="text-fade-truncate">{label}</span>
    </div>
  );

  if (interactiveTrailing) {
    return (
      <div className={cx("octopus-nav-row group min-w-0", rowClassName)}>
        <button
          aria-current={active ? "page" : undefined}
          aria-expanded={expanded}
          className="flex min-w-0 flex-1 cursor-interaction items-center text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-token-border disabled:cursor-not-allowed"
          data-sidebar-nav-item=""
          data-gesture-focus=""
          disabled={disabled}
          onClick={onClick}
          type="button"
        >
          {content}
        </button>
        {interactiveTrailing}
      </div>
    );
  }

  return (
    <div className="octopus-nav-row group relative min-w-0">
      <button
        aria-current={active ? "page" : undefined}
        aria-expanded={expanded}
        className={cx(rowClassName, action && "pr-9")}
        data-sidebar-nav-item=""
        data-gesture-focus=""
        disabled={disabled}
        onClick={onClick}
        type="button"
      >
        {content}
        {trailing}
      </button>
      {action ? (
        <div className="absolute right-1 top-0 flex h-full items-center opacity-0 group-hover:opacity-100 [&:has(:focus-visible)]:opacity-100">
          <RowActionButton action={action} />
        </div>
      ) : null}
    </div>
  );
}

export function OctopusNavSection({
  children,
  className,
  collapsed,
  title,
  titleActions,
  titleActionsOnHover = false,
  titleClassName,
  titleRowClassName,
  titleTrailing,
}: {
  children: ReactNode;
  className?: string;
  collapsed?: boolean;
  title?: ReactNode;
  titleActions?: ReactNode;
  titleActionsOnHover?: boolean;
  titleClassName?: string;
  titleRowClassName?: string;
  titleTrailing?: ReactNode;
}) {
  const reduceMotion = useShouldReduceMotion();
  return (
    <div className={cx("flex flex-col", collapsed == null && "gap-1", className)}>
      {title ? (
        <div className={cx("group/nav-section-title flex min-h-[var(--height-token-row)] items-center justify-between gap-2", titleRowClassName ?? "ps-[var(--padding-row-cell-x,var(--padding-row-x))] pe-1.5")}>
          <div className={cx("min-w-0 flex-1", titleClassName ?? "text-base font-medium text-tertiary")}>
            {title}
          </div>
          {titleActions != null || titleTrailing != null ? (
            <div className="flex shrink-0 items-center gap-1">
              {titleActions == null ? null : (
                <div
                  className={cx(
                    "shrink-0",
                    titleActionsOnHover
                      && "pointer-events-none opacity-0 group-focus-within/nav-section-title:pointer-events-auto group-focus-within/nav-section-title:opacity-100 group-hover/nav-section-title:pointer-events-auto group-hover/nav-section-title:opacity-100 has-[[data-state=open]]:pointer-events-auto has-[[data-state=open]]:opacity-100",
                  )}
                >
                  {titleActions}
                </div>
              )}
              {titleTrailing == null ? null : <div className="me-1.5 shrink-0">{titleTrailing}</div>}
            </div>
          ) : null}
        </div>
      ) : null}
      {collapsed == null ? (
        <div className="flex flex-col gap-px">{children}</div>
      ) : (
        <div inert={collapsed} aria-hidden={collapsed}>
        <AnimatePresence initial={false}>
          {collapsed ? null : (
            <motion.div
              animate={{ height: "auto", opacity: 1, transitionEnd: { overflow: "visible" } }}
              className="overflow-hidden"
              exit={{ height: 0, opacity: 0, overflow: "hidden" }}
              initial={{ height: 0, opacity: 0 }}
              transition={{ duration: reduceMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
            >
              <div className={cx("flex flex-col gap-px", Boolean(title) && "pt-1")}>{children}</div>
            </motion.div>
          )}
        </AnimatePresence>
        </div>
      )}
    </div>
  );
}

export function OctopusTaskRow({
  action,
  active,
  failed = false,
  hoverDetails,
  hoverDisabled = false,
  hoverScope,
  meta,
  onClick,
  onContextMenu,
  onDoubleClick,
  scheduledLabel,
  subtitle,
  title,
  unread = false,
  waiting = false,
  working = false,
}: {
  action?: OctopusRowAction;
  active: boolean;
  failed?: boolean;
  hoverDetails?: TaskHoverCardDetails;
  hoverDisabled?: boolean;
  hoverScope?: string;
  meta?: string;
  onClick: () => void;
  onContextMenu?: (event: MouseEvent<HTMLDivElement>) => void;
  onDoubleClick?: () => void;
  scheduledLabel?: string;
  subtitle?: string;
  title: string;
  unread?: boolean;
  waiting?: boolean;
  working?: boolean;
}) {
  const row = (
    <div
      aria-busy={working || undefined}
      aria-current={active ? "page" : undefined}
      aria-label={[title, subtitle, working ? "Running" : failed ? "Failed" : waiting ? "Waiting" : scheduledLabel, unread ? "unread" : ""].filter(Boolean).join(", ")}
      className={cx(
        "octopus-nav-row octopus-task-row group relative h-[var(--height-token-row)] cursor-interaction sidebar-item px-[var(--padding-row-cell-x,var(--padding-row-x))] py-row-y text-sm hover:bg-token-list-hover-background focus-visible:outline-offset-[-2px]",
        active && "bg-token-list-hover-background",
      )}
      data-sidebar-nav-item=""
      onClick={onClick}
      onContextMenu={(event) => {
        if (!onContextMenu) return;
        event.preventDefault();
        onContextMenu(event);
      }}
      onDoubleClick={(event) => {
        if (event.button !== 0 || !onDoubleClick) return;
        event.preventDefault();
        onDoubleClick();
      }}
      onKeyDown={(event) => {
        if (event.currentTarget !== event.target || (event.key !== "Enter" && event.key !== " ")) return;
        event.preventDefault();
        onClick();
      }}
      role="button"
      tabIndex={0}
    >
      <div className="flex h-full w-full items-center text-sm leading-4">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <div className={cx(
            "flex min-w-0 flex-1 self-stretch items-center gap-2 text-base leading-5",
            active || unread
                ? "text-token-text-primary"
                : "text-token-foreground",
          )}>
            <OctopusMarquee className="sidebar-task-title min-w-0 flex-1 select-none" data-thread-title data-unread={unread || undefined} animateOnGroupHover>
              <span dir="auto">{title}</span>
            </OctopusMarquee>
          </div>
        </div>
        {working || failed || waiting || scheduledLabel || unread || meta || action ? (
          <div className="sidebar-task-trailing">
            {working ? (
              <SidebarWorkingIndicator />
            ) : failed ? (
              <SidebarTaskStatus state="failed" detail="Task failed · open to view the error" />
            ) : waiting ? (
              <SidebarTaskStatus state="waiting" detail="Waiting for a response" />
            ) : scheduledLabel ? (
              <SidebarTaskStatus state="scheduled" label={scheduledLabel} text={scheduledLabel.replace(/^Scheduled · /, "")} />
            ) : unread ? <SidebarTaskStatus state="unread" label="Unread reply" />
              : meta ? <span className="sidebar-task-age">{meta}</span> : null}
            {action ? <div className="sidebar-task-action"><RowActionButton action={action} /></div> : null}
          </div>
        ) : null}
      </div>
    </div>
  );
  return hoverDetails ? (
    <TaskHoverCard age={meta} details={hoverDetails} disabled={hoverDisabled} key={JSON.stringify([hoverScope, hoverDetails.runId, hoverDetails.eeId])} title={title}>{row}</TaskHoverCard>
  ) : row;
}

export function OctopusAgentRow({
  action,
  active = false,
  label,
  logo,
  meta,
  onClick,
}: {
  action?: OctopusRowAction;
  active?: boolean;
  label: string;
  logo: ReactNode;
  meta?: string;
  onClick: () => void;
}) {
  return (
    <div className="octopus-nav-row group relative h-[var(--height-token-row)] min-w-0">
      <button
        aria-current={active ? "page" : undefined}
        aria-label={[label, meta].filter(Boolean).join(", ")}
        className={cx(
          "flex h-full w-full cursor-interaction items-center gap-2 overflow-hidden sidebar-item px-[var(--padding-row-cell-x,var(--padding-row-x))] py-row-y text-left text-base text-token-foreground outline-none hover:bg-token-list-hover-background focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-token-border",
          active && "bg-token-list-hover-background font-medium",
          action && "pr-9",
        )}
        data-sidebar-nav-item=""
        onClick={onClick}
        type="button"
      >
        <span className="sidebar-item-icon flex size-6 shrink-0 items-center justify-center">{logo}</span>
        <span className="text-fade-truncate min-w-0 flex-1">{label}</span>
        {meta ? (
          <span className={cx(
            "max-w-20 truncate text-sm text-token-description-foreground",
            action && "group-hover:hidden group-has-[:focus-visible]:hidden",
          )} title={meta}>
            {meta}
          </span>
        ) : null}
      </button>
      {action ? (
        <div className="absolute right-1 top-0 flex h-full items-center opacity-0 group-hover:opacity-100 [&:has(:focus-visible)]:opacity-100">
          <RowActionButton action={action} />
        </div>
      ) : null}
    </div>
  );
}

export function OctopusProjectRow({
  action,
  active = false,
  collapsed,
  depth = 0,
  dragHandle,
  label,
  logo,
  onClick,
  workingTaskCount = 0,
}: {
  action?: OctopusRowAction;
  active?: boolean;
  collapsed?: boolean;
  depth?: number;
  dragHandle?: SidebarDragHandle;
  label: string;
  logo?: ReactNode;
  onClick: () => void;
  workingTaskCount?: number;
}) {
  return (
    <div
      aria-current={active ? "page" : undefined}
      aria-expanded={collapsed == null ? undefined : !collapsed}
      aria-label={workingTaskCount ? `${label}, ${workingTaskCount} running ${workingTaskCount === 1 ? "task" : "tasks"}` : label}
      aria-busy={workingTaskCount > 0 || undefined}
      className={cx(
        "octopus-nav-row group/folder-row group relative flex h-[var(--height-token-row)] cursor-interaction items-center justify-between overflow-x-hidden sidebar-item px-[var(--padding-row-cell-x,var(--padding-row-x))] text-sm text-token-foreground hover:bg-token-list-hover-background focus-visible:outline focus-visible:outline-offset-2",
        active && "bg-token-list-hover-background",
      )}
      data-sidebar-nav-item=""
      onClick={(event) => {
        if (event.defaultPrevented || event.button !== 0) return;
        onClick();
      }}
      onKeyDown={(event) => {
        if (event.currentTarget !== event.target || (event.key !== "Enter" && event.key !== " ")) return;
        event.preventDefault();
        onClick();
      }}
      role="button"
      style={depth > 0 ? { paddingLeft: `calc(var(--padding-row-cell-x, var(--padding-row-x)) + ${depth * 14}px)` } : undefined}
      tabIndex={0}
    >
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <span className="sidebar-item-icon flex size-6 shrink-0 items-center justify-center">
          {logo ?? <Glyph className="icon-xs shrink-0" name={collapsed === false ? "folderOpen" : "folder"} size={16} />}
        </span>
        <div
          {...dragHandle?.listeners}
          className={cx(
            "flex min-w-0 flex-1 items-center gap-2 whitespace-nowrap rounded-md py-1 pr-0 text-left text-base text-token-foreground",
            active && "font-medium text-token-foreground",
            dragHandle && "cursor-interaction touch-none",
          )}
          ref={dragHandle?.setActivatorNodeRef}
        >
          <span className="flex min-w-0 flex-1 items-center gap-2 whitespace-nowrap">
            <span className="flex min-w-0 flex-1 items-center gap-0.5">
              <span className={cx("min-w-0 truncate pr-1", active && "font-medium text-token-foreground")}>{label}</span>
              {workingTaskCount > 0 ? <SidebarWorkingIndicator count={workingTaskCount} label={`${workingTaskCount} running ${workingTaskCount === 1 ? "task" : "tasks"}`} /> : null}
            </span>
          </span>
        </div>
      </div>
      {action ? (
        <div className="mr-1 hidden min-w-[26px] shrink-0 group-hover:block group-focus-visible:block">
          <div className="flex items-center justify-end">
            <RowActionButton action={action} />
          </div>
        </div>
      ) : null}
    </div>
  );
}

function RowActionButton({ action }: { action: OctopusRowAction }) {
  return (
    <MessageTooltip label={action.label}>
      <button
        aria-label={action.label}
        disabled={action.disabled}
        className="border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap rounded-full electron:rounded-md text-token-muted-foreground select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 enabled:hover:bg-transparent data-[state=open]:bg-transparent hover:text-token-foreground border-transparent electron:p-1 electron:[&>svg]:icon-sm flex items-center justify-center p-0.5 !h-5 !w-5 !p-0 [&>svg]:!h-4 [&>svg]:!w-4 !text-token-foreground/50 hover:!text-token-foreground focus-visible:!text-token-foreground"
        onClick={(event) => {
          event.stopPropagation();
          action.onClick(event);
        }}
        onPointerDown={(event) => event.stopPropagation()}
        type="button"
      >
        <Glyph className="translate-x-px" name={action.glyph} size={16} />
      </button>
    </MessageTooltip>
  );
}

function cx(...values: Array<string | false | null | undefined>): string {
  return values.filter(Boolean).join(" ");
}
