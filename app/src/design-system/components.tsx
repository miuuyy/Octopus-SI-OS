import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import type {
  ButtonHTMLAttributes,
  CSSProperties,
  HTMLAttributes,
  InputHTMLAttributes,
  KeyboardEvent as ReactKeyboardEvent,
  KeyboardEventHandler,
  ReactNode,
  Ref,
  RefObject,
  SelectHTMLAttributes,
  TextareaHTMLAttributes,
} from "react";
import "./octopusIconButton.css";
import "./booleanControls.css";
import "./nativeSlider.css";
import { OcDropdown } from "./OcDropdown";
import { containModalFocus, isolateModalLayer, isTopmostModal, restoreModalFocus } from "./modalFocus";
import { Glyph, type GlyphName } from "../components/IconGlyphs";
import { OctopusSpinner } from "../components/navigation/OctopusSpinner";
import { ShellTooltip } from "../components/shell/ShellTooltip";
import "./oc-menu.css";

export type OcTone = "neutral" | "accent" | "success" | "warning" | "danger" | "muted";
export type OcControlVariant = "primary" | "secondary" | "ghost" | "outline";
export type OcControlSize = "xs" | "sm" | "md";

const OCTOPUS_BUTTON_BASE = "border-default no-drag cursor-interaction flex items-center gap-1 rounded-lg border whitespace-nowrap select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40";
const OCTOPUS_BUTTON_SIZE: Record<OcControlSize, string> = {
  xs: "h-token-button-composer-sm px-1.5 py-0 text-sm leading-[18px]",
  sm: "h-token-button-composer px-2 py-0 text-base leading-[18px]",
  md: "min-h-9 px-3 text-sm font-medium",
};
const OCTOPUS_CONTROL_HEIGHT: Record<OcControlSize, string> = {
  xs: "h-token-button-composer-sm",
  sm: "h-token-button-composer",
  md: "min-h-toolbar",
};
const OCTOPUS_BUTTON_VARIANT: Record<OcControlVariant, string> = {
  primary: "bg-token-foreground text-token-dropdown-background enabled:hover:bg-token-foreground/80 data-[state=open]:bg-token-foreground/80",
  secondary: "border-transparent bg-token-foreground/5 text-token-foreground enabled:hover:bg-token-foreground/10 data-[state=open]:bg-token-foreground/10",
  ghost: "border-transparent bg-transparent text-tertiary enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
  outline: "bg-primary-soft-alpha text-default enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
};

export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

export const OCTOPUS_COMPACT_SURFACE_CLASS = "border-token-border bg-token-dropdown-background/90 relative flex w-full flex-col overflow-hidden rounded-2xl border p-1 text-sm backdrop-blur-sm";
export const OCTOPUS_FLOATING_SURFACE_CLASS = "group relative z-40 flex w-full flex-col overflow-hidden rounded-2xl bg-token-main-surface-primary/70 text-token-foreground shadow-lg ring-[0.5px] ring-token-border";
export const OCTOPUS_WIDGET_SURFACE_CLASS = "group relative flex w-full flex-col overflow-hidden rounded-xl border border-token-foreground/5 bg-token-foreground/[0.01] text-token-foreground";

type OcMenuSurfaceProps = HTMLAttributes<HTMLDivElement> & {
  density?: "menu" | "wide";
  surface?: "menu" | "panel" | "opaque";
  surfaceRef?: Ref<HTMLDivElement>;
};

export function OcMenuSurface({
  className,
  density = "menu",
  surface = "menu",
  surfaceRef,
  ...props
}: OcMenuSurfaceProps) {
  return (
    <div
      {...props}
      className={cx(
        "oc-menu-surface no-drag z-50 m-px flex select-none flex-col overflow-y-auto",
        surface === "panel" && "is-panel",
        surface === "opaque" && "is-opaque",
        density === "wide" && "is-wide",
        className,
      )}
      ref={surfaceRef}
    />
  );
}

export function OcMenuSectionLabel({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      {...props}
      data-menu-section-label="true"
      className={cx(
        "oc-menu-section-label",
        className,
      )}
    />
  );
}

export function OcMenuMessage({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      {...props}
      className={cx(
        "oc-menu-message px-[var(--padding-row-x)] py-[var(--padding-row-y)] text-sm text-token-input-placeholder-foreground",
        className,
      )}
    />
  );
}

type OcMenuSettingRowProps = HTMLAttributes<HTMLDivElement> & {
  description?: ReactNode;
  label: ReactNode;
  trailing: ReactNode;
};

export function OcMenuSettingRow({
  className,
  description,
  label,
  trailing,
  ...props
}: OcMenuSettingRowProps) {
  return (
    <div
      {...props}
      className={cx(
        "oc-menu-setting-row no-drag flex w-full items-center gap-3 rounded-lg px-[var(--padding-row-x)] py-[var(--padding-row-y)] text-left text-sm text-token-foreground outline-hidden",
        className,
      )}
    >
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="min-w-0 truncate">{label}</span>
        {description == null ? null : (
          <span className="min-w-0 truncate text-sm text-token-description-foreground">{description}</span>
        )}
      </span>
      <span className="flex shrink-0 items-center">{trailing}</span>
    </div>
  );
}

type OcMenuItemProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children" | "itemRef"> & {
  allowWrap?: boolean;
  buttonRef?: Ref<HTMLButtonElement>;
  density?: "menu" | "workspace";
  description?: ReactNode;
  descriptionAllowWrap?: boolean;
  label: ReactNode;
  leadingIcon?: GlyphName;
  selected?: boolean;
  tone?: "default" | "danger";
  trailing?: ReactNode;
};

export function OcMenuItem({
  allowWrap = false,
  buttonRef,
  className,
  density = "menu",
  description,
  descriptionAllowWrap = false,
  disabled,
  label,
  leadingIcon,
  selected = false,
  tone = "default",
  trailing,
  type = "button",
  ...props
}: OcMenuItemProps) {
  const icon = leadingIcon ? (
    <Glyph
      className={cx(
        description ? "icon-sm" : "icon-xs",
        "shrink-0 opacity-75 group-focus:opacity-100 group-hover:opacity-100",
      )}
      name={leadingIcon}
      size={description ? 18 : 16}
    />
  ) : null;
  const end = trailing ?? (selected ? (
    <Glyph
      className="icon-xs shrink-0 opacity-75 group-focus:opacity-100 group-hover:opacity-100"
      name="check"
      size={16}
    />
  ) : null);

  return (
    <button
      className={cx(
        "oc-menu-item no-drag flex w-full flex-col text-left outline-hidden",
        density === "workspace" && "is-workspace",
        tone === "danger" && "is-danger",
        disabled
          ? "cursor-default opacity-50"
          : "group cursor-interaction",
        className,
      )}
      data-selected={selected || undefined}
      data-text-value={typeof label === "string" ? label : undefined}
      {...props}
      disabled={disabled}
      ref={buttonRef}
      type={type}
    >
      {description == null ? (
        <span className="oc-menu-item-content">
          {icon ? <span className="oc-menu-item-icon">{icon}</span> : null}
          <span className={cx("min-w-0 flex-1", allowWrap ? "whitespace-normal" : "truncate")}>{label}</span>
          {end}
        </span>
      ) : (
        <span className="flex w-full items-center gap-1.5">
          {icon ? <span className="oc-menu-item-icon">{icon}</span> : null}
          <span className="flex min-w-0 flex-1 flex-col">
            <span className={cx("min-w-0", allowWrap ? "whitespace-normal" : "truncate")}>{label}</span>
            <span className={cx(
              "oc-menu-item-description min-w-0",
              descriptionAllowWrap ? "whitespace-normal" : "truncate",
            )}>{description}</span>
          </span>
          {end}
        </span>
      )}
    </button>
  );
}

type OcCompactActionProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  trailing?: ReactNode;
};

export function OcCompactAction({
  children,
  className,
  disabled,
  trailing,
  type = "button",
  ...props
}: OcCompactActionProps) {
  return (
    <button
      {...props}
      className={cx(
        "oc-compact-action no-drag flex w-full shrink-0 items-center justify-between gap-1.5 overflow-hidden rounded-lg px-[var(--padding-row-x)] py-[var(--padding-row-y)] text-left text-sm outline-hidden",
        disabled
          ? "cursor-default opacity-50"
          : "cursor-interaction opacity-75 hover:bg-token-list-hover-background hover:opacity-100 focus:bg-token-list-hover-background focus:opacity-100",
        className,
      )}
      disabled={disabled}
      type={type}
    >
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {trailing}
    </button>
  );
}

type OcButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  buttonRef?: Ref<HTMLButtonElement>;
  leadingIcon?: GlyphName;
  trailingIcon?: GlyphName;
  meta?: ReactNode;
  selected?: boolean;
  tone?: OcTone;
  variant?: OcControlVariant;
  size?: OcControlSize;
};

export function OcButton({
  buttonRef,
  leadingIcon,
  trailingIcon,
  meta,
  selected = false,
  tone = "neutral",
  variant = "secondary",
  size = "sm",
  className,
  children,
  type = "button",
  title,
  ...props
}: OcButtonProps) {
  const button = (
    <button
      ref={buttonRef}
      className={cx(
        "oc-button-root",
        `oc-tone-${tone}`,
        OCTOPUS_BUTTON_BASE,
        OCTOPUS_BUTTON_SIZE[size],
        OCTOPUS_BUTTON_VARIANT[variant],
        selected && "is-selected bg-token-foreground/10",
        className,
      )}
      data-oc-control="button"
      data-state={selected ? "selected" : undefined}
      {...props}
      type={type}
    >
      {leadingIcon ? <Glyph name={leadingIcon} size={size === "xs" ? 12 : 14} /> : null}
      {children ? <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">{children}</span> : null}
      {meta ? <strong className="overflow-hidden text-ellipsis whitespace-nowrap text-xs font-medium text-token-foreground">{meta}</strong> : null}
      {trailingIcon ? <Glyph name={trailingIcon} size={size === "xs" ? 12 : 14} /> : null}
    </button>
  );
  return <ShellTooltip asChild disabled={props.disabled || !title} label={title ?? ""}>{button}</ShellTooltip>;
}

export type OcAnimatedActionSlotProps = {
  children: ReactNode;
  className?: string;
};

export function OcAnimatedActionSlot({ children, className }: OcAnimatedActionSlotProps) {
  const reducedMotion = useReducedMotion();
  const contentRef = useRef<HTMLSpanElement>(null);
  const [width, setWidth] = useState<number | null>(null);

  useLayoutEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const update = () => setWidth(Math.ceil(content.getBoundingClientRect().width));
    update();
    const observer = new ResizeObserver(update);
    observer.observe(content);
    return () => observer.disconnect();
  }, [children]);

  return (
    <motion.span
      animate={width == null ? undefined : { width }}
      className={cx("inline-flex shrink-0 justify-end overflow-hidden align-middle", className)}
      data-oc-control="animated-action-slot"
      initial={false}
      transition={{ duration: reducedMotion ? 0 : 0.18, ease: [0.77, 0, 0.175, 1] }}
    >
      <span className="inline-flex min-w-max shrink-0 justify-end [&>button]:w-full [&>button]:min-w-0 [&>button]:shrink-0 [&>button]:justify-center" ref={contentRef}>
        {children}
      </span>
    </motion.span>
  );
}

type OcIconButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  appearance?: "default" | "sidebar" | "compact" | "rail" | "dialog-close";
  buttonRef?: Ref<HTMLButtonElement>;
  glyph: GlyphName;
  label: string;
  selected?: boolean;
  tone?: OcTone;
  variant?: OcControlVariant;
  size?: OcControlSize;
};

export function OcIconButton({
  appearance = "default",
  buttonRef,
  glyph,
  label,
  selected = false,
  tone = "neutral",
  variant = "secondary",
  size = "sm",
  className,
  type = "button",
  title,
  ...props
}: OcIconButtonProps) {
  const tooltipLabel = title ?? label;
  const button = appearance !== "default" ? <button
    {...props}
    aria-label={label}
    className={cx("oc-octopus-icon-control no-drag", className)}
    data-appearance={appearance}
    data-selected={selected || undefined}
    ref={buttonRef}
    type={type}
  ><Glyph name={glyph} className={appearance === "rail" ? "icon-md" : "icon-xs"} size={appearance === "rail" ? 20 : 16} /></button> : (
    <button
      ref={buttonRef}
      aria-label={label}
      className={cx(
        "oc-icon-button-root",
        `oc-tone-${tone}`,
        "no-drag cursor-interaction flex shrink-0 items-center justify-center rounded-lg border border-transparent text-token-icon-foreground focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40 enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
        size === "xs" ? "size-token-button-composer-sm" : size === "md" ? "size-toolbar" : "size-token-button-composer",
        selected && "is-selected bg-token-foreground/10",
        className,
      )}
      data-oc-control="icon-button"
      data-state={selected ? "selected" : undefined}
      {...props}
      type={type}
    >
      <Glyph name={glyph} size={size === "xs" ? 12 : 15} />
    </button>
  );
  return <ShellTooltip asChild disabled={props.disabled || !tooltipLabel} label={tooltipLabel}>{button}</ShellTooltip>;
}

export type OcSegmentedOption<T extends string> = {
  value: T;
  label: ReactNode;
  disabled?: boolean;
};

type OcSegmentedProps<T extends string> = {
  label: string;
  value: T;
  options: Array<OcSegmentedOption<T>>;
  onChange: (next: T) => void;
  className?: string;
  size?: OcControlSize;
};

export function OcSegmented<T extends string>({
  label,
  value,
  options,
  onChange,
  className,
  size = "xs",
}: OcSegmentedProps<T>) {
  return (
    <div
      aria-label={label}
      className={cx(
        "oc-segmented-root inline-flex hide-scrollbar relative min-w-0 max-w-full items-center gap-0.5 overflow-x-auto overflow-y-hidden",
        className,
      )}
      data-oc-control="segmented"
      role="group"
    >
      {options.map((option) => (
        <button
          aria-pressed={option.value === value}
          className={cx(
            "no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 focus-visible:ring-inset disabled:cursor-default disabled:opacity-40 border-transparent shrink-0",
            size === "xs"
              ? "rounded-full px-2 py-0.5 text-sm leading-[18px]"
              : cx("rounded-lg px-2 py-0 text-base leading-[18px]", OCTOPUS_CONTROL_HEIGHT[size]),
            option.value === value
              ? "text-default bg-text/5 enabled:hover:bg-text/10 data-[state=open]:bg-text/10"
              : "text-tertiary enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
          )}
          disabled={option.disabled}
          key={option.value}
          onClick={() => onChange(option.value)}
          type="button"
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export type OcStepProgressProps = HTMLAttributes<HTMLDivElement> & {
  activeIndex: number;
  label: string;
  stepCount: number;
};

export function OcStepProgress({ activeIndex, label, stepCount, className, ...props }: OcStepProgressProps) {
  const count = Math.max(1, Math.floor(stepCount));
  const current = Math.max(0, Math.min(count - 1, Math.floor(activeIndex)));
  return (
    <div
      {...props}
      aria-label={label}
      aria-valuemax={count}
      aria-valuemin={1}
      aria-valuenow={current + 1}
      className={cx("flex h-[3px] w-20 gap-1.5", className)}
      data-active-index={current}
      data-oc-control="step-progress"
      role="progressbar"
    >
      {Array.from({ length: count }, (_, index) => (
        <span
          aria-hidden="true"
          className={cx("h-full flex-1 rounded-full", index === current ? "bg-token-foreground" : "bg-token-foreground/10")}
          key={index}
        />
      ))}
    </div>
  );
}

export type OcInlineProgressProps = Omit<HTMLAttributes<HTMLSpanElement>, "children"> & {
  label: ReactNode;
  progressLabel?: string;
  showValue?: boolean;
  value?: number | null;
};

export function OcInlineProgress({
  label,
  progressLabel,
  showValue = false,
  value,
  className,
  ...props
}: OcInlineProgressProps) {
  const reducedMotion = useReducedMotion();
  const determinate = typeof value === "number" && Number.isFinite(value);
  const progress = determinate ? Math.max(0, Math.min(100, value)) : null;
  const roundedProgress = progress == null ? null : Math.round(progress);
  const radius = 7;
  const circumference = Math.PI * 2 * radius;
  const accessibleLabel = progressLabel ?? (typeof label === "string" ? label : "Progress");
  return (
    <span
      {...props}
      aria-label={accessibleLabel}
      aria-valuemax={determinate ? 100 : undefined}
      aria-valuemin={determinate ? 0 : undefined}
      aria-valuenow={roundedProgress ?? undefined}
      className={cx("inline-flex min-w-0 items-center gap-1.5", className)}
      data-oc-control="inline-progress"
      data-state={determinate ? "determinate" : "indeterminate"}
      role="progressbar"
    >
      {determinate ? <svg
        aria-hidden="true"
        className="size-4 shrink-0 -rotate-90"
        viewBox="0 0 18 18"
      >
        <circle className="fill-none stroke-current opacity-20" cx="9" cy="9" r={radius} strokeWidth="2" />
        <circle
          className="fill-none stroke-current"
          cx="9"
          cy="9"
          r={radius}
          strokeDasharray={circumference}
          strokeDashoffset={progress == null ? 0 : circumference * (1 - progress / 100)}
          strokeLinecap="round"
          strokeWidth="2"
          style={determinate ? {
            transitionDuration: reducedMotion ? "0ms" : "500ms",
            transitionProperty: "stroke-dashoffset",
            transitionTimingFunction: "ease-out",
          } : undefined}
        />
      </svg> : <OctopusSpinner className="icon-xs shrink-0" />}
      <span className="min-w-0 truncate">{label}{showValue && roundedProgress != null ? ` ${roundedProgress}%` : ""}</span>
    </span>
  );
}

type OcStatusProps = {
  label: string;
  detail?: string;
  tone?: OcTone;
  className?: string;
};

export function OcStatus({ label, detail, tone = "neutral", className }: OcStatusProps) {
  return (
    <ShellTooltip asChild disabled={!detail} label={detail ?? ""}><span className={cx("oc-status", `oc-tone-${tone}`, className)} aria-description={detail} data-oc-status>
      <span aria-hidden="true" className="oc-status__dot" />
      <span className="oc-status__label">{label}</span>
    </span></ShellTooltip>
  );
}

type OcSurfaceHeaderProps = {
  kicker?: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  actions?: ReactNode;
  className?: string;
};

export function OcSurfaceHeader({ kicker, title, detail, actions, className }: OcSurfaceHeaderProps) {
  return (
    <header className={cx("oc-surface-header", className)} data-oc-surface-header>
      <div className="oc-surface-header__copy">
        {kicker ? <span>{kicker}</span> : null}
        <strong>{title}</strong>
        {detail ? <em>{detail}</em> : null}
      </div>
      {actions ? <div className="oc-surface-header__actions">{actions}</div> : null}
    </header>
  );
}

export function OcKbd({ children }: { children: ReactNode }) {
  return <kbd className="oc-kbd">{children}</kbd>;
}

export type OcCheckboxProps = Omit<
  InputHTMLAttributes<HTMLInputElement>,
  "checked" | "children" | "onChange" | "readOnly" | "type"
> & {
  checked: boolean | "mixed";
  description?: ReactNode;
  label: ReactNode;
  onCheckedChange: (checked: boolean) => void;
  showLabel?: boolean;
};

export function OcCheckbox({
  checked,
  className,
  description,
  disabled,
  id,
  label,
  onCheckedChange,
  showLabel = false,
  ...props
}: OcCheckboxProps) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const labelId = `${inputId}-label`;
  const descriptionId = description ? `${inputId}-description` : undefined;
  const inputRef = useRef<HTMLInputElement>(null);
  const state = checked === "mixed" ? "mixed" : checked ? "checked" : "unchecked";

  useEffect(() => {
    if (inputRef.current) inputRef.current.indeterminate = checked === "mixed";
  }, [checked]);

  return (
    <label
      className={cx("oc-checkbox inline-flex min-w-0 cursor-interaction items-center gap-2", disabled && "cursor-not-allowed", className)}
      data-oc-control="checkbox"
      data-state={state}
    >
      <input
        {...props}
        aria-checked={checked === "mixed" ? "mixed" : checked}
        aria-describedby={descriptionId}
        aria-labelledby={labelId}
        checked={checked === true}
        className="oc-checkbox__input peer sr-only"
        disabled={disabled}
        id={inputId}
        onChange={(event) => onCheckedChange(event.currentTarget.checked)}
        ref={inputRef}
        type="checkbox"
      />
      <span
        aria-hidden="true"
        className="oc-checkbox__control"
      >
        {checked === "mixed" ? <svg width="14" height="14" viewBox="0 0 20 20" fill="none" focusable="false"><path d="M3.5 10.0002C3.5 9.63297 3.79777 9.33521 4.16504 9.33521H15.835C16.2022 9.33521 16.5 9.63297 16.5 10.0002C16.5 10.3675 16.2022 10.6652 15.835 10.6652H4.16504C3.79777 10.6652 3.5 10.3675 3.5 10.0002Z" fill="currentColor" /></svg> : checked ? <Glyph name="check" size={12} /> : null}
      </span>
      <span className={cx("min-w-0", !showLabel && "sr-only")}>
        <span className="block min-w-0 text-sm font-medium text-token-text-primary" id={labelId}>{label}</span>
        {description ? <span className="mt-0.5 block min-w-0 text-xs leading-4 text-token-text-secondary" id={descriptionId}>{description}</span> : null}
      </span>
    </label>
  );
}

export type OcSelectionCardProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> & {
  description?: ReactNode;
  label: ReactNode;
  selected: boolean;
};

export function OcSelectionCard({
  className,
  description,
  disabled,
  label,
  selected,
  type = "button",
  ...props
}: OcSelectionCardProps) {
  return (
    <button
      {...props}
      aria-pressed={selected}
      className={cx(
        "relative flex min-h-10 min-w-0 cursor-interaction items-center justify-center overflow-hidden rounded-xl border px-3 py-2 text-center text-sm leading-5 font-normal text-token-foreground outline-none focus-visible:ring-2 focus-visible:ring-token-focus-border disabled:cursor-not-allowed disabled:opacity-40",
        selected
          ? "border-token-foreground/20 bg-token-foreground/[0.06]"
          : "border-token-border bg-token-main-surface-primary hover:bg-token-foreground/[0.03]",
        Boolean(description) && "min-h-14",
        className,
      )}
      data-oc-control="selection-card"
      data-state={selected ? "selected" : "unselected"}
      disabled={disabled}
      type={type}
    >
      {selected ? <Glyph aria-hidden="true" className="absolute left-2 size-4 text-token-foreground" name="check" size={16} /> : null}
      <span className="min-w-0 px-3">
        <span className="block truncate">{label}</span>
        {description ? <span className="mt-0.5 block line-clamp-2 text-xs leading-4 text-token-text-secondary">{description}</span> : null}
      </span>
    </button>
  );
}

export type OcSwitchProps = {
  label: ReactNode;
  description?: ReactNode;
  showLabel?: boolean;
  checked: boolean;
  disabled?: boolean;
  className?: string;
  id?: string;
  onCheckedChange?: (checked: boolean) => void;
};

export function OcSwitch({
  label,
  description,
  showLabel = false,
  className,
  disabled,
  onCheckedChange,
  id,
  checked,
}: OcSwitchProps) {
  const generatedId = useId();
  const labelId = id ?? `${generatedId}-label`;
  const descriptionId = description ? `${generatedId}-description` : undefined;
  const state = checked ? "checked" : "unchecked";

  const control = (
    <button
      aria-checked={checked}
      aria-describedby={descriptionId}
      aria-label={typeof label === "string" ? label : undefined}
      aria-labelledby={typeof label === "string" ? undefined : labelId}
      className={cx(
        "inline-flex cursor-interaction items-center text-sm focus-visible:rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-60",
        !showLabel && className,
      )}
      data-state={state}
      disabled={disabled}
      onClick={() => onCheckedChange?.(!checked)}
      role="switch"
      type="button"
    >
      <span
        aria-hidden="true"
        className={cx(
          "oc-switch__track relative inline-flex h-5 w-8 shrink-0 items-center rounded-full transition-colors duration-basic ease-out",
          checked ? "bg-token-charts-blue" : "bg-token-foreground/10",
        )}
        data-state={state}
      >
        <span
          className="oc-switch__thumb h-4 w-4 rounded-full border border-[color:var(--gray-0)] bg-[color:var(--gray-0)] shadow-sm transition-transform duration-basic ease-out"
          data-state={state}
        />
      </span>
    </button>
  );

  if (!showLabel) return control;
  return (
    <div className={cx("flex min-w-0 items-center gap-2", className)}>
      {control}
      <span className="flex min-w-0 flex-col gap-0.5">
        <span className="text-sm font-medium text-token-text-primary" id={labelId}>{label}</span>
        {description ? <span className="text-xs leading-4 text-token-text-secondary" id={descriptionId}>{description}</span> : null}
      </span>
    </div>
  );
}

export type OcSelectOption<T extends string> = {
  value: T;
  label: string;
  glyph?: GlyphName;
  description?: string;
  disabled?: boolean;
};

export type OcSelectProps<T extends string> = Omit<
  SelectHTMLAttributes<HTMLSelectElement>,
  "children" | "onChange" | "size" | "value"
> & {
  label: ReactNode;
  value?: T;
  options: ReadonlyArray<OcSelectOption<T>>;
  onChange: (value: T) => void;
  description?: ReactNode;
  error?: ReactNode;
  placeholder?: string;
  showLabel?: boolean;
  size?: OcControlSize;
  stretch?: boolean;
};

export function OcSelect<T extends string>({
  label,
  value,
  options,
  onChange,
  description,
  error,
  placeholder,
  showLabel = false,
  size = "sm",
  stretch = false,
  className,
  id,
  disabled,
  title,
  ...selectProps
}: OcSelectProps<T>) {
  const generatedId = useId();
  const selectId = id ?? generatedId;
  const supportId = description || error ? `${selectId}-support` : undefined;
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const selected = options.find((option) => option.value === value);

  return (
    <div className={cx("oc-field", "oc-select-field", stretch && "w-full", disabled && "is-disabled", className)}>
      <span className={cx("oc-field__label", !showLabel && "oc-visually-hidden")} id={`${selectId}-label`}>{label}</span>
      <ShellTooltip asChild disabled={disabled || open || !title} label={title ?? ""}><button
        aria-labelledby={`${selectId}-label`}
        aria-describedby={supportId}
        aria-expanded={open}
        aria-haspopup="menu"
        aria-invalid={Boolean(error) || undefined}
        className={cx(
          "oc-select border-default no-drag cursor-interaction flex min-w-0 max-w-full items-center justify-between gap-1 whitespace-nowrap rounded-lg border bg-primary-soft-alpha px-3 py-0 text-base leading-[18px] text-default select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40 enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
          stretch ? "w-full justify-between" : "w-fit",
          size === "xs" ? "h-token-button-composer-sm" : size === "md" ? "min-h-toolbar" : "h-token-button-composer",
          Boolean(error) && "has-error",
        )}
        data-state={open ? "open" : "closed"}
        disabled={disabled}
        id={selectId}
        onClick={() => setOpen((current) => !current)}
        ref={triggerRef}
        type="button"
      >
        <span className="flex min-w-0 flex-1 items-center gap-1.5 truncate">
          {selected?.label ?? placeholder ?? "Select"}
        </span>
        <Glyph className="icon-2xs shrink-0 text-tertiary" name="chevronDown" size={14} />
      </button></ShellTooltip>
      <OcDropdown anchorRef={triggerRef} open={open} onOpenChange={setOpen} align="end" contentWidth="menuWide" aria-label={typeof label === "string" ? label : undefined}>
        <div className="max-h-80 overflow-y-auto">
          {options.map((option) => <OcMenuItem
            aria-label={option.label}
            data-orientation="vertical"
            description={option.description}
            descriptionAllowWrap
            disabled={option.disabled}
            key={option.value}
            label={option.label}
            leadingIcon={option.glyph}
            onClick={() => onChange(option.value)}
            role="menuitem"
            selected={option.value === value}
          />)}
        </div>
      </OcDropdown>
      {description || error ? (
        <span className={cx("oc-field__support", Boolean(error) && "is-error")} id={supportId}>
          {error ?? description}
        </span>
      ) : null}
    </div>
  );
}

export type OcFieldRowProps = HTMLAttributes<HTMLDivElement> & {
  label: ReactNode;
  description?: ReactNode;
  leading?: ReactNode;
  meta?: ReactNode;
  control?: ReactNode;
  disabled?: boolean;
  htmlFor?: string;
  variant?: "default" | "nested";
};

export function OcFieldRow({
  label,
  description,
  leading,
  meta,
  control,
  children,
  disabled = false,
  htmlFor,
  variant = "default",
  className,
  ...props
}: OcFieldRowProps) {
  const LabelElement = htmlFor ? "label" : "div";

  return (
    <div {...props} className={cx(
      "oc-field-row flex items-center justify-between px-4",
      variant === "nested" ? "min-h-10 gap-3 py-2 max-sm:min-h-0 max-sm:flex-col max-sm:items-stretch" : "gap-6 py-3",
      disabled && "is-disabled",
      className,
    )}>
      <div className="oc-field-row__copy flex min-w-0 flex-1 items-center gap-3">
        {leading ? <div className="oc-field-row__leading shrink-0">{leading}</div> : null}
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <LabelElement className={cx("oc-field-row__label min-w-0 text-sm text-default", variant === "default" && "font-medium")} {...(htmlFor ? { htmlFor } : {})}>{label}</LabelElement>
          {description ? <div className="oc-field-row__description min-w-0 text-balance text-xs leading-4 text-secondary">{description}</div> : null}
          {meta ? <div className="oc-field-row__meta min-w-0 text-balance text-sm leading-5 text-token-error-foreground">{meta}</div> : null}
        </div>
      </div>
      <div className={cx(
        "oc-field-row__control flex items-center",
        variant === "nested" ? "min-w-0 flex-1 justify-end max-sm:justify-stretch" : "max-w-full shrink-0 gap-2",
      )}>{control ?? children}</div>
    </div>
  );
}

export function OcSettingsRow(props: OcFieldRowProps) {
  return <OcFieldRow {...props} className={cx("oc-settings-row", props.className)} />;
}

export type OcCardProps = HTMLAttributes<HTMLElement> & {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
};

export function OcCard({ title, description, actions, children, className, style, ...props }: OcCardProps) {
  return (
    <section
      {...props}
      className={cx("oc-card flex min-w-0 flex-col overflow-hidden rounded-2xl border border-default text-default shadow-none", className)}
      style={{ backgroundColor: "var(--color-background-panel, var(--color-background-primary-soft-alpha))", ...style }}
    >
      {title || description || actions ? (
        <header className="oc-card__header flex items-center justify-between gap-4 px-4 py-3">
          <div className="oc-card__copy flex min-w-0 flex-col gap-0.5">
            {title ? <h3 className="m-0 min-w-0 text-sm font-medium text-token-text-primary">{title}</h3> : null}
            {description ? <p className="m-0 min-w-0 text-xs leading-4 text-token-text-secondary">{description}</p> : null}
          </div>
          {actions ? <div className="oc-card__actions">{actions}</div> : null}
        </header>
      ) : null}
      <div className="oc-card__content flex flex-col [&>*:not(:last-child)]:relative [&>*:not(:last-child)]:after:pointer-events-none [&>*:not(:last-child)]:after:absolute [&>*:not(:last-child)]:after:inset-x-4 [&>*:not(:last-child)]:after:bottom-0 [&>*:not(:last-child)]:after:h-[0.5px] [&>*:not(:last-child)]:after:bg-border [&>*:not(:last-child)]:after:content-['']">{children}</div>
    </section>
  );
}

export type OcGroupProps = HTMLAttributes<HTMLElement> & {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
};

export function OcGroup({ title, description, actions, children, className, ...props }: OcGroupProps) {
  const hasDescription = description != null;
  return (
    <section {...props} className={cx("oc-group flex min-w-0 flex-col gap-0", className)}>
      {title || description || actions ? (
        <header className={cx(
          "oc-group__header flex justify-between gap-4",
          hasDescription ? "items-start pb-3" : "min-h-toolbar items-center pb-1.5",
        )}>
          <div className="oc-group__copy flex min-w-0 flex-1 flex-col gap-0.5">
            {title ? (
              <h2 className={cx(
                "m-0 font-medium text-default",
                hasDescription ? "text-lg" : "text-base",
              )}>{title}</h2>
            ) : null}
            {description ? (
              <p className="m-0 text-balance text-sm leading-[18px] font-normal text-secondary">
                {description}
              </p>
            ) : null}
          </div>
          {actions ? <div className="oc-group__actions flex items-center gap-2">{actions}</div> : null}
        </header>
      ) : null}
      <div className="oc-group__content flex min-w-0 flex-col gap-1.5">{children}</div>
    </section>
  );
}

export type OcTextFieldProps = Omit<InputHTMLAttributes<HTMLInputElement>, "size"> & {
  label: ReactNode;
  description?: ReactNode;
  error?: ReactNode;
  leading?: ReactNode;
  trailing?: ReactNode;
  showLabel?: boolean;
  size?: OcControlSize;
  inputRef?: Ref<HTMLInputElement>;
  inputClassName?: string;
};

export function OcTextField({
  label,
  description,
  error,
  leading,
  trailing,
  showLabel = false,
  size = "sm",
  inputRef,
  inputClassName,
  className,
  id,
  disabled,
  ...props
}: OcTextFieldProps) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const supportId = description || error ? `${inputId}-support` : undefined;

  return (
    <label className={cx("flex min-w-0 flex-col gap-1 text-default", className)} htmlFor={inputId}>
      <span className={cx("text-xs font-medium text-default", !showLabel && "sr-only")}>{label}</span>
      <span className="flex min-w-0 items-center">
        {leading ? <span className="flex shrink-0 items-center ps-2 text-token-text-secondary">{leading}</span> : null}
        <input
          {...props}
          aria-describedby={supportId}
          aria-invalid={Boolean(error) || undefined}
          disabled={disabled}
          id={inputId}
          ref={inputRef}
          className={cx(
            "w-full min-w-0 border border-primary-outline rounded-md bg-primary-soft text-sm text-default outline-none placeholder:text-tertiary focus:border-ring aria-invalid:border-text-danger aria-invalid:ring-2 aria-invalid:ring-text-danger/20 disabled:cursor-not-allowed disabled:bg-text/5 disabled:text-secondary",
            size === "xs" ? "h-7 px-2" : "h-9 px-2.5",
            size === "xs" && props.type === "number" && "tabular-nums",
            inputClassName,
          )}
        />
        {trailing ? <span className="flex shrink-0 items-center pe-2 text-token-text-secondary">{trailing}</span> : null}
      </span>
      {description || error ? (
        <span className={cx("text-xs", error ? "text-danger" : "text-secondary")} id={supportId}>
          {error ?? description}
        </span>
      ) : null}
    </label>
  );
}

export type OcTextAreaProps = TextareaHTMLAttributes<HTMLTextAreaElement> & {
  label: ReactNode;
  description?: ReactNode;
  error?: ReactNode;
  showLabel?: boolean;
  textareaClassName?: string;
};

export function OcTextArea({
  label,
  description,
  error,
  showLabel = false,
  textareaClassName,
  className,
  id,
  disabled,
  ...props
}: OcTextAreaProps) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const supportId = description || error ? `${inputId}-support` : undefined;

  return (
    <label className={cx("flex min-w-0 flex-col gap-1 text-default", className)} htmlFor={inputId}>
      <span className={cx("text-xs font-medium text-default", !showLabel && "sr-only")}>{label}</span>
      <textarea
        {...props}
        aria-describedby={supportId}
        aria-invalid={Boolean(error) || undefined}
        className={cx(
          "w-full min-w-0 min-h-24 resize-y rounded-md border border-primary-outline bg-primary-soft px-2.5 py-2 text-sm text-default outline-none placeholder:text-tertiary focus:border-ring aria-invalid:border-text-danger aria-invalid:ring-2 aria-invalid:ring-text-danger/20 disabled:cursor-not-allowed disabled:bg-text/5 disabled:text-secondary",
          textareaClassName,
        )}
        disabled={disabled}
        id={inputId}
      />
      {description || error ? (
        <span className={cx("text-xs", error ? "text-danger" : "text-secondary")} id={supportId}>
          {error ?? description}
        </span>
      ) : null}
    </label>
  );
}

export type OcEmptyStateProps = HTMLAttributes<HTMLDivElement> & {
  title: ReactNode;
  action?: ReactNode;
  detail?: ReactNode;
  glyph?: GlyphName;
  size?: "compact" | "default";
  tone?: "neutral" | "danger";
};

export function OcEmptyState({
  title,
  action,
  detail,
  glyph,
  size = "default",
  tone = "neutral",
  className,
  ...props
}: OcEmptyStateProps) {
  return (
    <div
      {...props}
      className={cx(
        "oc-empty-state flex flex-col items-center justify-center gap-2 px-4 text-center",
        size === "compact" ? "min-h-40 py-8" : "min-h-56 py-12",
        tone === "danger" ? "text-token-error-foreground" : "text-token-description-foreground",
        className,
      )}
      data-oc-empty-state
    >
      {glyph ? <Glyph className="shrink-0" name={glyph} size={20} /> : null}
      <strong className={cx(
        "text-base font-normal",
        tone === "danger" ? "text-token-error-foreground" : "text-token-text-secondary",
      )}>{title}</strong>
      {detail ? <span className="max-w-lg whitespace-pre-wrap text-sm leading-5">{detail}</span> : null}
      {action ? <div className="mt-1 flex items-center justify-center gap-2">{action}</div> : null}
    </div>
  );
}

export type OcExpandableListProps<T> = {
  batchSize?: number;
  children: (visibleItems: readonly T[]) => ReactNode;
  disclosureClassName?: string;
  getItemKey: (item: T) => string;
  getItemLabel: (item: T) => string;
  items: readonly T[];
  renderItemPreview?: (item: T) => ReactNode;
  resetKey: string | number;
  visibleItemLimit: number;
};

export function OcExpandableList<T>({
  batchSize,
  children,
  disclosureClassName,
  getItemKey,
  getItemLabel,
  items,
  renderItemPreview,
  resetKey,
  visibleItemLimit,
}: OcExpandableListProps<T>) {
  const initialLimit = Math.max(0, Math.floor(visibleItemLimit));
  const batch = Math.max(1, Math.floor(batchSize ?? (items.length || 1)));
  const [expansion, setExpansion] = useState({ key: resetKey, page: 0 });
  const requestedPage = Object.is(expansion.key, resetKey) ? expansion.page : 0;
  const lastPage = Math.max(0, Math.ceil(Math.max(0, items.length - initialLimit) / batch));
  const page = Math.min(requestedPage, lastPage);

  const visibleCount = initialLimit + page * batch;
  const visibleItems = items.slice(0, visibleCount);
  const hiddenItems = items.slice(visibleCount);
  const isExpanded = page > 0 && hiddenItems.length === 0;
  const previewItems: Array<{ key: string; content: ReactNode }> = [];
  if (!isExpanded && renderItemPreview) {
    for (const item of hiddenItems.slice(0, 3)) {
      const content = renderItemPreview(item);
      if (content != null) previewItems.push({ key: getItemKey(item), content });
    }
  }
  const names = hiddenItems.slice(0, 2).map(getItemLabel).filter(Boolean);
  const remaining = Math.max(hiddenItems.length - names.length, 0);
  const disclosureLabel = isExpanded
    ? "Show less"
    : names.length
      ? `See ${names.join(", ")}${remaining ? `, and ${remaining} more` : ""}`
      : `Show ${hiddenItems.length} more`;
  return (
    <>
      {children(visibleItems)}
      {items.length > initialLimit && (isExpanded || hiddenItems.length > 0) ? (
        <button
          aria-expanded={isExpanded}
          className={cx(
            "flex min-h-[var(--height-token-row)] w-full cursor-interaction items-center gap-3 rounded-[var(--radius-token-row)] px-[var(--padding-row-x)] py-[var(--padding-row-y)] text-left text-sm leading-relaxed font-normal text-token-description-foreground outline-hidden hover:text-token-foreground focus-visible:text-token-foreground",
            disclosureClassName,
          )}
          data-oc-control="expandable-list-disclosure"
          onClick={() => setExpansion({ key: resetKey, page: isExpanded ? 0 : page + 1 })}
          type="button"
        >
          {previewItems.length ? (
            <span aria-hidden="true" className="flex h-10 w-8 shrink-0 items-center">
              {previewItems.map(({ key, content }) => (
                <span className="-ml-2 flex size-4 items-center justify-center overflow-hidden rounded-sm border border-token-border-default bg-token-main-surface-primary first:ml-0" key={key}>
                  {content}
                </span>
              ))}
            </span>
          ) : null}
          <span>{disclosureLabel}</span>
        </button>
      ) : null}
    </>
  );
}

export type OcTreePreviewItem = {
  description?: ReactNode;
  id: string;
  label: ReactNode;
  leading?: ReactNode;
  meta?: ReactNode;
  parentId?: string | null;
};

export type OcTreePreviewProps = Omit<HTMLAttributes<HTMLDivElement>, "children"> & {
  items: readonly OcTreePreviewItem[];
  label: string;
};

export function OcTreePreview({ items, label, className, ...props }: OcTreePreviewProps) {
  const reducedMotion = useReducedMotion();
  const rows = useMemo(() => flattenTreePreviewItems(items), [items]);
  const transition = reducedMotion ? { duration: 0 } : { duration: 0.3, ease: [0.19, 1, 0.22, 1] as const };
  return (
    <div {...props} aria-label={label} className={cx("flex min-w-0 flex-col gap-px", className)} data-oc-control="tree-preview" role="list">
      <AnimatePresence initial={false}>
        {rows.map(({ depth, item }) => (
          <motion.div
            animate={{ height: "auto", opacity: 1, transitionEnd: { overflow: "visible" } }}
            className="overflow-hidden"
            exit={{ height: 0, opacity: 0, overflow: "hidden" }}
            initial={{ height: 0, opacity: 0 }}
            key={item.id}
            layout="position"
            role="listitem"
            transition={transition}
          >
            <div
              className="flex min-h-[var(--height-token-row)] min-w-0 items-center gap-1 rounded-[var(--radius-token-row)] text-sm text-token-foreground"
              style={depth > 0 ? { paddingLeft: depth * 14 } : undefined}
            >
              <span aria-hidden="true" className="flex size-[var(--height-token-row)] shrink-0 items-center justify-center">
                {item.leading ?? <Glyph className="icon-xs text-token-text-secondary" name="folder" size={16} />}
              </span>
              <span className="flex min-w-0 flex-1 flex-col py-1 pr-1">
                <span className="min-w-0 truncate text-base text-token-foreground"><span className="sr-only">Level {depth + 1}. </span>{item.label}</span>
                {item.description ? <span className="min-w-0 truncate text-xs text-token-description-foreground">{item.description}</span> : null}
              </span>
              {item.meta ? <span className="mr-2 shrink-0 text-xs text-token-text-secondary">{item.meta}</span> : null}
            </div>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}

function flattenTreePreviewItems(items: readonly OcTreePreviewItem[]): Array<{ item: OcTreePreviewItem; depth: number }> {
  const knownIds = new Set(items.map((item) => item.id));
  const childrenByParent = new Map<string, OcTreePreviewItem[]>();
  for (const item of items) {
    const parentId = item.parentId && knownIds.has(item.parentId) ? item.parentId : "";
    const children = childrenByParent.get(parentId) ?? [];
    children.push(item);
    childrenByParent.set(parentId, children);
  }
  const rows: Array<{ item: OcTreePreviewItem; depth: number }> = [];
  const visited = new Set<string>();
  const visit = (parentId: string, depth: number) => {
    for (const item of childrenByParent.get(parentId) ?? []) {
      if (visited.has(item.id)) continue;
      visited.add(item.id);
      rows.push({ item, depth });
      visit(item.id, depth + 1);
    }
  };
  visit("", 0);
  for (const item of items) {
    if (visited.has(item.id)) continue;
    visited.add(item.id);
    rows.push({ item, depth: 0 });
    visit(item.id, 1);
  }
  return rows;
}

export const OC_SEARCH_FIELD_CLASS = "no-drag flex h-8 min-w-0 items-center gap-2 rounded-full border border-token-input-border bg-token-input-background/90 px-2.5 py-0 text-base leading-[18px] backdrop-blur-sm electron:dark:bg-token-dropdown-background";
const OC_SEARCH_INPUT_CLASS = "min-w-0 flex-1 bg-transparent text-base leading-[18px] text-token-input-foreground outline-none select-text placeholder:text-token-input-placeholder-foreground [&::placeholder]:select-none";

export type OcSearchFieldProps = {
  autoFocus?: boolean;
  className?: string;
  id?: string;
  inputRef?: Ref<HTMLInputElement>;
  label: string;
  onChange: (next: string) => void;
  onKeyDown?: KeyboardEventHandler<HTMLInputElement>;
  placeholder: string;
  trailing?: ReactNode;
  value: string;
};

export function OcSearchField({
  autoFocus,
  className,
  id: providedId,
  inputRef,
  label,
  onChange,
  onKeyDown,
  placeholder,
  trailing,
  value,
}: OcSearchFieldProps) {
  const generatedId = useId();
  const id = providedId ?? generatedId;
  return (
    <div className={cx(OC_SEARCH_FIELD_CLASS, className)} role="search">
      <Glyph className="icon-sm shrink-0 text-token-text-secondary" name="search" size={18} />
      <label className="sr-only" htmlFor={id}>{label}</label>
      <input
        autoFocus={autoFocus}
        className={OC_SEARCH_INPUT_CLASS}
        id={id}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        ref={inputRef}
        type="text"
        value={value}
      />
      {value.length ? (
        <button
          aria-label="Clear search"
          className="flex shrink-0 cursor-interaction text-token-text-secondary hover:text-token-foreground"
          onClick={() => onChange("")}
          type="button"
        >
          <Glyph className="icon-sm" name="close" size={18} />
        </button>
      ) : trailing}
    </div>
  );
}

export type OcNumberFieldProps = Omit<OcTextFieldProps, "type">;

export function OcNumberField(props: OcNumberFieldProps) {
  return <OcTextField {...props} type="number" />;
}

export type OcSliderProps = Omit<
  InputHTMLAttributes<HTMLInputElement>,
  "defaultValue" | "onChange" | "size" | "type" | "value"
> & {
  label: ReactNode;
  value?: number;
  defaultValue?: number;
  onChange?: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  description?: ReactNode;
  formatValue?: (value: number) => ReactNode;
  showValue?: boolean;
};

export function OcSlider({
  label,
  value,
  defaultValue,
  onChange,
  min = 0,
  max = 100,
  step = 1,
  description,
  formatValue = (current) => current,
  showValue = true,
  className,
  id,
  disabled,
  style,
  ...props
}: OcSliderProps) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const [internalValue, setInternalValue] = useState(defaultValue ?? min);
  const currentValue = value ?? internalValue;
  const formattedValue = formatValue(currentValue);
  const descriptionId = description ? `${inputId}-description` : undefined;

  return (
    <label className={cx("oc-native-slider", className)} htmlFor={inputId}>
      <span className="sr-only">{label}</span>
      {description ? <span className="oc-native-slider__description" id={descriptionId}>{description}</span> : null}
      <span className="oc-native-slider__row">
        <input
          {...props}
          aria-describedby={props["aria-describedby"] ?? descriptionId}
          aria-valuetext={props["aria-valuetext"] ?? (typeof formattedValue === "string" ? formattedValue : undefined)}
          className="oc-native-slider__range"
          disabled={disabled}
          id={inputId}
          max={max}
          min={min}
          onChange={(event) => {
            const next = Number(event.currentTarget.value);
            if (value === undefined) setInternalValue(next);
            onChange?.(next);
          }}
          step={step}
          style={style}
          type="range"
          value={currentValue}
        />
        {showValue ? <output htmlFor={inputId}>{formattedValue}</output> : null}
      </span>
    </label>
  );
}

export type OcDialogSize = "narrow" | "feature" | "compact" | "setup" | "default" | "wide" | "xwide" | "xxwide" | "editor";

export type OcDialogProps = {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  ariaLabel?: string;
  ariaLabelledBy?: string;
  busy?: boolean;
  backdropClassName?: string;
  initialFocusRef?: RefObject<HTMLElement | null>;
  panelClassName?: string;
  size?: OcDialogSize;
  variant?: "default" | "form";
  position?: "centered" | "top";
};

export function OcDialog({
  open,
  onClose,
  children,
  ariaLabel,
  ariaLabelledBy,
  busy = false,
  backdropClassName,
  initialFocusRef,
  panelClassName,
  size = "default",
  variant = "default",
  position = "centered",
}: OcDialogProps) {
  const reducedMotion = useReducedMotion();
  const dialogRef = useRef<HTMLDivElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const previousFocus = useMemo(() => open && document.activeElement instanceof HTMLElement ? document.activeElement : null, [open]);

  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    layer.toggleAttribute("inert", !open);
    if (!open) return;
    return isolateModalLayer(layer);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const frame = window.requestAnimationFrame(() => {
      (initialFocusRef?.current ?? dialogRef.current)?.focus();
    });
    return () => {
      window.cancelAnimationFrame(frame);
      restoreModalFocus(previousFocus);
    };
  }, [initialFocusRef, open, previousFocus]);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.key !== "Escape" || busy || !isTopmostModal(dialogRef.current)) return;
      event.preventDefault();
      event.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", dismiss);
    return () => window.removeEventListener("keydown", dismiss);
  }, [busy, onClose, open]);

  if (typeof document === "undefined") return null;
  const duration = reducedMotion ? 0 : 0.18;
  return createPortal(
    <AnimatePresence initial={false}>
      {open ? (
        <motion.div
          animate={{ opacity: 1 }}
          className={cx(
            "oc-dialog-overlay fixed inset-0 z-[1100] flex justify-center",
            position === "top" ? "items-start" : "items-center",
            backdropClassName,
          )}
          data-oc-dialog-variant={variant}
          exit={{ opacity: 0 }}
          initial={{ opacity: 0 }}
          key="oc-dialog"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !busy) onClose();
          }}
          role="presentation"
          ref={layerRef}
          transition={{ duration }}
        >
          <motion.div
            animate={{ opacity: 1, scale: 1, y: 0 }}
            aria-busy={busy || undefined}
            aria-label={ariaLabel}
            aria-labelledby={ariaLabelledBy}
            aria-modal="true"
            className={cx(
              "octopus-dialog oc-dialog flex flex-col overflow-hidden outline-none",
              variant === "form"
                ? "relative bg-surface-elevated-secondary text-default"
                : "rounded-3xl bg-token-dropdown-background/90 text-token-foreground shadow-lg ring-[0.5px] ring-token-border backdrop-blur-xl",
              panelClassName,
            )}
            data-oc-dialog
            data-oc-dialog-position={position}
            data-oc-dialog-size={size}
            data-oc-dialog-variant={variant}
            data-settings-layer="true"
            exit={{ opacity: 0, scale: reducedMotion ? 1 : 0.98, y: reducedMotion ? 0 : 4 }}
            initial={{ opacity: 0, scale: reducedMotion ? 1 : 0.98, y: reducedMotion ? 0 : 4 }}
            onKeyDown={(event) => containModalFocus(event, dialogRef.current)}
            ref={dialogRef}
            role="dialog"
            tabIndex={-1}
            transition={{ duration, ease: [0.33, 1, 0.68, 1] }}
          >
            {children}
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>,
    document.body,
  );
}

export type OcDialogHeaderProps = HTMLAttributes<HTMLElement> & {
  title: ReactNode;
  description?: ReactNode;
  leading?: ReactNode;
  onClose?: () => void;
  closeLabel?: string;
  closeDisabled?: boolean;
  titleId?: string;
};

export function OcDialogHeader({
  title,
  description,
  leading,
  onClose,
  closeLabel = "Close dialog",
  closeDisabled = false,
  titleId,
  className,
  ...props
}: OcDialogHeaderProps) {
  return (
    <header {...props} className={cx("flex shrink-0 items-start gap-4 px-6 pt-6 pb-4", className)}>
      {leading ? <div className="shrink-0">{leading}</div> : null}
      <div className="min-w-0 flex-1">
        <h2 className="heading-dialog m-0 font-semibold text-token-foreground" id={titleId}>{title}</h2>
        {description ? <p className="mt-1 text-sm leading-5 text-token-text-secondary">{description}</p> : null}
      </div>
      {onClose ? <OcIconButton appearance="dialog-close" disabled={closeDisabled} glyph="close" label={closeLabel} onClick={onClose} variant="ghost" /> : null}
    </header>
  );
}

export type OcDialogFooterProps = HTMLAttributes<HTMLElement> & {
  leading?: ReactNode;
};

export function OcDialogFooter({ leading, children, className, ...props }: OcDialogFooterProps) {
  return (
    <footer
      {...props}
      className={cx(
        "flex min-h-[76px] shrink-0 items-center gap-3 border-t border-token-border/40 px-6 pb-[max(1.5rem,env(safe-area-inset-bottom))] pt-4",
        className,
      )}
    >
      {leading ? <div className="min-w-0 flex-1 text-xs leading-4 text-token-description-foreground">{leading}</div> : <div className="flex-1" />}
      <div className="flex shrink-0 items-center justify-end gap-2">{children}</div>
    </footer>
  );
}

export type OcPopoverProps = {
  open: boolean;
  anchor: ReactNode;
  children: ReactNode;
  label: string;
  onDismiss?: () => void;
  placement?: "top" | "bottom";
  align?: "start" | "center" | "end";
  role?: "dialog" | "menu" | "listbox";
  modal?: boolean;
  className?: string;
  panelClassName?: string;
};

export function OcPopover({
  open,
  anchor,
  children,
  label,
  onDismiss,
  placement = "bottom",
  align = "start",
  role = "dialog",
  modal = false,
  className,
  panelClassName,
}: OcPopoverProps) {
  return (
    <div
      className={cx("oc-popover", className)}
      data-align={align}
      data-open={open ? "true" : "false"}
      data-placement={placement}
      data-settings-layer={open || undefined}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !event.defaultPrevented && open && onDismiss) {
          event.preventDefault();
          event.stopPropagation();
          onDismiss();
        }
      }}
    >
      <div className="oc-popover__anchor">{anchor}</div>
      {open ? (
        <>
          {onDismiss && modal ? (
            <button
              aria-label={`Close ${label}`}
              className="oc-popover__scrim"
              onClick={onDismiss}
              tabIndex={-1}
              type="button"
            />
          ) : null}
          <div
            aria-label={label}
            aria-modal={modal || undefined}
            className={cx("oc-popover__panel", panelClassName)}
            role={role}
          >
            {children}
          </div>
        </>
      ) : null}
    </div>
  );
}
