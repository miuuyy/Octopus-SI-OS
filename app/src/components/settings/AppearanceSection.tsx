import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties, type ReactNode } from "react";
import { parseDiffFromFile, preloadHighlighter } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import { createPortal } from "react-dom";
import { HexColorPicker } from "react-colorful";
import {
  darkCodeThemes,
  defaultAppearance,
  lightCodeThemes,
  codeThemeSeed,
  appearanceAccentColor,
  setAppearance,
  useAppearance,
  type AppearanceSettings,
  type CodeThemeDefinition,
  type CodeThemeId,
  type DiffMarkerStyle,
  type MotionMode,
  type ThemeMode,
} from "../../appearance";
import {
  OcButton,
  OcDialog,
  OcDialogFooter,
  OcDialogHeader,
  OcGroup,
  OcMenuItem,
  OcMenuSurface,
  OcSwitch,
  OcTextField,
  cx,
} from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import { ThemePreviewGraphic } from "./AppearanceThemePreview";
import { octopusDiffThemeName, registerOctopusDiffThemes } from "./octopusDiffThemes";
import { octopusDiffRawUnsafeCss } from "./octopusDiffUnsafeCss";
import { SettingsHeading } from "./SettingsHeading";
import { useAppErrorToast } from "../../toasts";
import { copyText } from "../../runtimeFormat";

const themeOptions: Array<{ value: ThemeMode; label: string }> = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const themeCardClass = "flex flex-col [&>*:not(:last-child)]:relative [&>*:not(:last-child)]:after:pointer-events-none [&>*:not(:last-child)]:after:absolute [&>*:not(:last-child)]:after:inset-x-4 [&>*:not(:last-child)]:after:bottom-0 [&>*:not(:last-child)]:after:h-px [&>*:not(:last-child)]:after:bg-border [&>*:not(:last-child)]:after:content-[''] overflow-hidden rounded-2xl border border-default";
const rangeClass = "h-0.5 flex-1 appearance-none rounded-full [&::-moz-range-thumb]:h-5 [&::-moz-range-thumb]:w-5 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:border [&::-moz-range-thumb]:border-transparent [&::-moz-range-thumb]:bg-current [&::-moz-range-thumb]:shadow-sm [&::-moz-range-track]:h-0.5 [&::-moz-range-track]:rounded-full [&::-webkit-slider-runnable-track]:h-0.5 [&::-webkit-slider-runnable-track]:rounded-full [&::-webkit-slider-thumb]:mt-[-9px] [&::-webkit-slider-thumb]:h-5 [&::-webkit-slider-thumb]:w-5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border [&::-webkit-slider-thumb]:border-transparent [&::-webkit-slider-thumb]:bg-current [&::-webkit-slider-thumb]:shadow-sm";

const themePreviewBefore = `const themePreview: ThemeConfig = {
  surface: "sidebar",
  accent: "#2563eb",
  contrast: 42,
};
`;
const themePreviewAfter = `const themePreview: ThemeConfig = {
  surface: "sidebar-elevated",
  accent: "#0ea5e9",
  contrast: 68,
};
`;
const themePreviewDiff = parseDiffFromFile(
  { name: "src/theme-preview.ts", contents: themePreviewBefore },
  { name: "src/theme-preview.ts", contents: themePreviewAfter },
);
const diffThemeRegistrationKey = Symbol.for("octopus.diff-themes.registered");
const diffThemeRegistry = globalThis as typeof globalThis & { [diffThemeRegistrationKey]?: boolean };
if (!diffThemeRegistry[diffThemeRegistrationKey]) {
  registerOctopusDiffThemes();
  diffThemeRegistry[diffThemeRegistrationKey] = true;
}

type ThemeKind = "light" | "dark";

function codeThemeAppearancePatch(kind: ThemeKind, id: CodeThemeId): Partial<AppearanceSettings> {
  const seed = codeThemeSeed(kind, id);
  const contrast = seed.contrast ?? (kind === "light" ? defaultAppearance.lightContrast : defaultAppearance.darkContrast);
  const uiFont = seed.fonts?.ui === undefined
    ? (kind === "light" ? defaultAppearance.lightUiFont : defaultAppearance.darkUiFont)
    : (seed.fonts.ui ?? "");
  const codeFont = seed.fonts?.code === undefined
    ? (kind === "light" ? defaultAppearance.lightCodeFont : defaultAppearance.darkCodeFont)
    : (seed.fonts.code ?? "");
  const translucentSidebar = seed.opaqueWindows == null
    ? (kind === "light" ? defaultAppearance.lightTranslucentSidebar : defaultAppearance.darkTranslucentSidebar)
    : !seed.opaqueWindows;
  return kind === "light"
    ? {
        lightAccent: seed.accent,
        lightAccentSource: id === "codex" ? "codex" : "custom",
        lightBackground: seed.surface,
        lightCodeFont: codeFont,
        lightCodeTheme: id,
        lightContrast: contrast,
        lightDiffAdded: seed.semanticColors.diffAdded,
        lightDiffRemoved: seed.semanticColors.diffRemoved,
        lightForeground: seed.ink,
        lightSkill: seed.semanticColors.skill,
        lightTranslucentSidebar: translucentSidebar,
        lightUiFont: uiFont,
      }
    : {
        darkAccent: seed.accent,
        darkAccentSource: id === "codex" ? "codex" : "custom",
        darkBackground: seed.surface,
        darkCodeFont: codeFont,
        darkCodeTheme: id,
        darkContrast: contrast,
        darkDiffAdded: seed.semanticColors.diffAdded,
        darkDiffRemoved: seed.semanticColors.diffRemoved,
        darkForeground: seed.ink,
        darkSkill: seed.semanticColors.skill,
        darkTranslucentSidebar: translucentSidebar,
        darkUiFont: uiFont,
      };
}

export function AppearanceSection() {
  const appearance = useAppearance();
  const [transferStatus, setTransferStatus] = useState("");
  const [transferError, setTransferError] = useState(false);
  const [importMode, setImportMode] = useState<ThemeKind | null>(null);
  const [importValue, setImportValue] = useState("");
  useAppErrorToast(transferError ? transferStatus : "");

  async function copyTheme(mode: ThemeKind) {
    try {
      if (!await copyText(themeShareString(appearance, mode))) throw new Error("Clipboard access is unavailable.");
      setTransferStatus(`${capitalize(mode)} theme copied.`);
      setTransferError(false);
    } catch (error) {
      setTransferStatus(error instanceof Error ? error.message : String(error));
      setTransferError(true);
    }
  }

  function openThemeImport(mode: ThemeKind) {
    setImportValue("");
    setImportMode(mode);
  }

  function importTheme() {
    if (!importMode) return;
    try {
      setAppearance(readThemeShare(importValue, importMode));
      setTransferStatus(`${capitalize(importMode)} theme imported.`);
      setTransferError(false);
      setImportMode(null);
      setImportValue("");
    } catch (error) {
      setTransferStatus(error instanceof Error ? error.message : String(error));
      setTransferError(true);
    }
  }

  return (
    <section className="settings-section settings-appearance">
      <SettingsHeading title="Appearance" />

      <OcGroup title="Theme">
        <div className="flex flex-col gap-4">
          <div aria-label="Theme" className="grid w-68 max-w-full grid-cols-3 gap-4" role="radiogroup">
            {themeOptions.map((option) => (
              <ThemeOption
                checked={appearance.themeMode === option.value}
                key={option.value}
                label={option.label}
                mode={option.value}
                onChange={() => setAppearance({ themeMode: option.value })}
              />
            ))}
          </div>

          <ThemeDiffPreview appearance={appearance} />

          {transferStatus && !transferError ? (
            <div className={cx("settings-transfer-status", transferError && "is-error")} role="status">
              {transferStatus}
            </div>
          ) : null}

          <div className="flex flex-col gap-[var(--padding-panel)]">
            {appearance.themeMode !== "dark" ? (
              <ThemeEditor
                accent={appearanceAccentColor(appearance, "light")}
                background={appearance.lightBackground}
                codeFont={appearance.lightCodeFont}
                codeTheme={appearance.lightCodeTheme}
                codeThemes={lightCodeThemes}
                contrast={appearance.lightContrast}
                foreground={appearance.lightForeground}
                mode="light"
                onAccent={(lightAccent) => setAppearance({ lightAccent })}
                onBackground={(lightBackground) => setAppearance({ lightBackground })}
                onCodeFont={(lightCodeFont) => setAppearance({ lightCodeFont })}
                onCodeTheme={(lightCodeTheme) => setAppearance(codeThemeAppearancePatch("light", lightCodeTheme))}
                onContrast={(lightContrast) => setAppearance({ lightContrast })}
                onCopy={() => void copyTheme("light")}
                onForeground={(lightForeground) => setAppearance({ lightForeground })}
                onImport={() => openThemeImport("light")}
                onTranslucent={(lightTranslucentSidebar) => setAppearance({ lightTranslucentSidebar })}
                onUiFont={(lightUiFont) => setAppearance({ lightUiFont })}
                translucent={appearance.lightTranslucentSidebar}
                uiFont={appearance.lightUiFont}
              />
            ) : null}
            {appearance.themeMode !== "light" ? (
              <ThemeEditor
                accent={appearanceAccentColor(appearance, "dark")}
                background={appearance.darkBackground}
                codeFont={appearance.darkCodeFont}
                codeTheme={appearance.darkCodeTheme}
                codeThemes={darkCodeThemes}
                contrast={appearance.darkContrast}
                foreground={appearance.darkForeground}
                mode="dark"
                onAccent={(darkAccent) => setAppearance({ darkAccent })}
                onBackground={(darkBackground) => setAppearance({ darkBackground })}
                onCodeFont={(darkCodeFont) => setAppearance({ darkCodeFont })}
                onCodeTheme={(darkCodeTheme) => setAppearance(codeThemeAppearancePatch("dark", darkCodeTheme))}
                onContrast={(darkContrast) => setAppearance({ darkContrast })}
                onCopy={() => void copyTheme("dark")}
                onForeground={(darkForeground) => setAppearance({ darkForeground })}
                onImport={() => openThemeImport("dark")}
                onTranslucent={(darkTranslucentSidebar) => setAppearance({ darkTranslucentSidebar })}
                onUiFont={(darkUiFont) => setAppearance({ darkUiFont })}
                translucent={appearance.darkTranslucentSidebar}
                uiFont={appearance.darkUiFont}
              />
            ) : null}
          </div>
        </div>
      </OcGroup>

      <OcGroup title="Preferences">
        <div
          className={themeCardClass}
          style={{ backgroundColor: "var(--color-background-panel, var(--color-background-primary-soft-alpha))" }}
        >
          <PreferenceRow
            control={(
              <OcSwitch
                checked={appearance.pointerCursors}
                label="Use pointer cursors"
                onCheckedChange={(pointerCursors) => setAppearance({ pointerCursors })}
              />
            )}
            description="Change the cursor to a pointer when hovering over interactive elements"
            label="Use pointer cursors"
          />
          <PreferenceRow
            control={(
              <PreferenceSegmented<MotionMode>
                label="Reduce motion"
                onChange={(motionMode) => setAppearance({ motionMode })}
                options={[
                  { value: "system", label: "System" },
                  { value: "reduce", label: "On" },
                  { value: "full", label: "Off" },
                ]}
                value={appearance.motionMode}
              />
            )}
            description="Reduce animations or match your system"
            label="Reduce motion"
          />
          <PreferenceRow
            control={(
              <OcSwitch
                checked={appearance.octopuses}
                label="Octopuses"
                onCheckedChange={(octopuses) => setAppearance({ octopuses })}
              />
            )}
            description="Octopuses swim through the waves on Home and leave when a call starts; on the new-task screen each task's agent swims in its colour"
            label="Octopuses"
          />
          <PreferenceRow
            control={(
              <NumberUnitField
                label="Sans font size"
                max={16}
                min={11}
                onChange={(uiFontSize) => setAppearance({ uiFontSize })}
                value={appearance.uiFontSize}
              />
            )}
            description="Adjust the base size used for the Octopus UI"
            label="UI font size"
          />
          <PreferenceRow
            control={(
              <NumberUnitField
                label="Code font size"
                max={24}
                min={8}
                onChange={(codeFontSize) => setAppearance({ codeFontSize })}
                value={appearance.codeFontSize}
              />
            )}
            description="Adjust the base size used for code across runs and artifacts"
            label="Code font size"
          />
          <PreferenceRow
            className="gap-6"
            control={(
              <PreferenceSegmented<DiffMarkerStyle>
                label="Diff markers"
                onChange={(diffMarkerStyle) => setAppearance({ diffMarkerStyle })}
                options={[
                  { value: "color", label: "Color", ariaLabel: "Color diff markers" },
                  { value: "symbols", label: "+/-", ariaLabel: "Plus / minus diff markers" },
                ]}
                value={appearance.diffMarkerStyle}
              />
            )}
            description="Show changes using colors or +/− markers"
            label="Diff markers"
          />
          <PreferenceRow
            control={(
              <OcSwitch
                checked={appearance.fontSmoothing}
                label="Font smoothing"
                onCheckedChange={(fontSmoothing) => setAppearance({ fontSmoothing })}
              />
            )}
            description="Use native macOS font anti-aliasing"
            label="Font smoothing"
          />
        </div>
      </OcGroup>

      <ThemeImportDialog
        example={importMode ? themeShareString(appearance, importMode) : ""}
        mode={importMode}
        onClose={() => {
          setImportMode(null);
          setImportValue("");
        }}
        onSubmit={importTheme}
        onValueChange={setImportValue}
        value={importValue}
      />
    </section>
  );
}

function ThemeOption({
  checked,
  label,
  mode,
  onChange,
}: {
  checked: boolean;
  label: string;
  mode: ThemeMode;
  onChange: () => void;
}) {
  return (
    <MessageTooltip asChild label={label}>
      <label className="group flex min-w-0 cursor-interaction flex-col items-center gap-1.5 text-center">
        <input
          aria-label={label}
          checked={checked}
          className="peer sr-only"
          name="appearance-theme"
          onChange={onChange}
          type="radio"
        />
        <span
          aria-hidden="true"
          className="relative isolate block aspect-4/3 w-full overflow-hidden rounded-lg text-info peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-ring"
        >
          <ThemePreviewGraphic mode={mode} />
          <span className={cx(
            "absolute inset-0 rounded-lg border-2",
            checked ? "border-ring" : "border-default group-hover:border-strong",
          )} />
        </span>
      </label>
    </MessageTooltip>
  );
}

function ThemeDiffPreview({ appearance }: { appearance: AppearanceSettings }) {
  const [highlighterReady, setHighlighterReady] = useState(false);
  const systemDark = useSyncExternalStore(subscribeToSystemTheme, systemThemeIsDark, () => false);
  const themeType = appearance.themeMode === "system"
    ? (systemDark ? "dark" : "light")
    : appearance.themeMode;
  const codeTheme = themeType === "dark" ? appearance.darkCodeTheme : appearance.lightCodeTheme;
  const diffThemeName = octopusDiffThemeName(themeType, codeTheme);
  const options = useMemo(() => ({
    diffIndicators: appearance.diffMarkerStyle === "symbols" ? "classic" as const : "bars" as const,
    diffStyle: "split" as const,
    disableFileHeader: true,
    expansionLineCount: 8,
    hunkSeparators: "line-info" as const,
    lineDiffType: "none" as const,
    overflow: "scroll" as const,
    theme: diffThemeName,
    themeType,
    unsafeCSS: octopusDiffRawUnsafeCss,
  }), [appearance.diffMarkerStyle, diffThemeName, themeType]);

  useEffect(() => {
    let active = true;
    setHighlighterReady(false);
    void preloadHighlighter({
      langs: ["typescript"],
      themes: [diffThemeName],
    }).then(() => {
      if (active) setHighlighterReady(true);
    });
    return () => {
      active = false;
    };
  }, [diffThemeName]);

  return (
    <div className="isolate overflow-hidden rounded-xl border border-token-border bg-token-main-surface-primary" data-testid="theme-preview">
      {highlighterReady ? <FileDiff disableWorkerPool fileDiff={themePreviewDiff} key={diffThemeName} options={options} /> : null}
    </div>
  );
}

function subscribeToSystemTheme(listener: () => void): () => void {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

function systemThemeIsDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function ThemeEditor({
  mode,
  accent,
  background,
  foreground,
  uiFont,
  codeFont,
  codeTheme,
  codeThemes,
  translucent,
  contrast,
  onAccent,
  onBackground,
  onForeground,
  onUiFont,
  onCodeFont,
  onCodeTheme,
  onTranslucent,
  onContrast,
  onImport,
  onCopy,
}: {
  mode: ThemeKind;
  accent: string;
  background: string;
  foreground: string;
  uiFont: string;
  codeFont: string;
  codeTheme: CodeThemeId;
  codeThemes: readonly CodeThemeDefinition[];
  translucent: boolean;
  contrast: number;
  onAccent: (value: string) => void;
  onBackground: (value: string) => void;
  onForeground: (value: string) => void;
  onUiFont: (value: string) => void;
  onCodeFont: (value: string) => void;
  onCodeTheme: (value: CodeThemeId) => void;
  onTranslucent: (value: boolean) => void;
  onContrast: (value: number) => void;
  onImport: () => void;
  onCopy: () => void;
}) {
  const title = `${capitalize(mode)} theme`;
  const titleId = `appearance-${mode}-theme`;
  return (
    <section aria-labelledby={titleId}>
      <div className={themeCardClass} style={{ backgroundColor: "var(--color-background-panel, var(--color-background-primary-soft-alpha))" }}>
        <ThemeRow
          className="gap-6 py-3"
          control={(
            <div className="flex items-center gap-2 max-sm:w-full max-sm:flex-wrap max-sm:justify-end">
              <OcButton aria-label={`Import ${title}`} onClick={onImport} variant="ghost">Import</OcButton>
              <OcButton aria-label={`Copy ${title}`} onClick={onCopy} variant="ghost">Copy theme</OcButton>
              <CodeThemePicker
                accent={accent}
                ariaLabel={`${capitalize(mode)} code theme`}
                background={background}
                foreground={foreground}
                onChange={onCodeTheme}
                options={codeThemes}
                value={codeTheme}
              />
            </div>
          )}
          label={<h2 className="font-medium" id={titleId}>{title}</h2>}
        />
        <ThemeRow control={<ColorField label={`${capitalize(mode)} accent color`} onChange={onAccent} value={accent} />} label="Accent" />
        <ThemeRow control={<ColorField label={`${capitalize(mode)} background color`} onChange={onBackground} value={background} />} label="Background" />
        <ThemeRow control={<ColorField label={`${capitalize(mode)} ink color`} onChange={onForeground} value={foreground} />} label="Foreground" />
        <ThemeRow control={<ThemeFontField label={`${capitalize(mode)} UI font`} onChange={onUiFont} placeholder={'-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'} value={uiFont} />} label="UI font" />
        <ThemeRow control={<ThemeFontField label={`${capitalize(mode)} code font`} onChange={onCodeFont} placeholder={'ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace'} value={codeFont} />} label="Code font" />
        <ThemeRow control={<OcSwitch checked={translucent} label={`${capitalize(mode)} translucent sidebar`} onCheckedChange={onTranslucent} />} label="Translucent sidebar" />
        <ThemeRow control={<ThemeContrast accent={accent} background={background} label={`${capitalize(mode)} contrast`} onChange={onContrast} value={contrast} />} label="Contrast" />
      </div>
    </section>
  );
}

function ThemeImportDialog({
  example,
  mode,
  onClose,
  onSubmit,
  onValueChange,
  value,
}: {
  example: string;
  mode: ThemeKind | null;
  onClose: () => void;
  onSubmit: () => void;
  onValueChange: (value: string) => void;
  value: string;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const valid = mode ? isThemeShareValid(value, mode) : false;

  if (!mode) return null;
  return (
    <OcDialog
      ariaLabelledBy="appearance-theme-import-title"
      initialFocusRef={inputRef}
      onClose={onClose}
      open={Boolean(mode)}
      panelClassName="settings-theme-import-dialog"
    >
      <OcDialogHeader className="settings-theme-import-header" onClose={onClose} title="Import theme" titleId="appearance-theme-import-title" />
      <div className="min-h-0 px-5 pt-3">
        <OcTextField
          inputClassName="font-mono text-sm"
          inputRef={inputRef}
          label={`${capitalize(mode)} theme share string`}
          onChange={(event) => onValueChange(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && valid) {
              event.preventDefault();
              onSubmit();
            }
          }}
          placeholder={example}
          showLabel={false}
          spellCheck={false}
          type="text"
          value={value}
        />
      </div>
      <OcDialogFooter className="settings-theme-import-footer">
        <OcButton onClick={onClose} variant="ghost">Cancel</OcButton>
        <OcButton disabled={!valid} onClick={onSubmit} variant="primary">Import theme</OcButton>
      </OcDialogFooter>
    </OcDialog>
  );
}

function CodeThemePicker({
  accent,
  ariaLabel,
  background,
  foreground,
  onChange,
  options,
  value,
}: {
  accent: string;
  ariaLabel: string;
  background: string;
  foreground: string;
  onChange: (value: CodeThemeId) => void;
  options: readonly CodeThemeDefinition[];
  value: CodeThemeId;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const selected = options.find((option) => option.id === value) ?? options[0];
  return (
    <>
      <button
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={ariaLabel}
        className="no-drag cursor-interaction flex h-token-button-composer w-[11rem] max-w-full items-center justify-between gap-1 rounded-lg border border-default bg-surface-secondary px-2.5 py-0 text-base leading-[18px] text-default shadow-sm whitespace-nowrap select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40 enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover"
        data-state={open ? "open" : "closed"}
        onClick={() => setOpen((current) => !current)}
        ref={triggerRef}
        type="button"
      >
        <span className="flex min-w-0 flex-1 items-center gap-2">
          <CodeThemeSwatch accent={accent} background={background} foreground={foreground} />
          <span className="truncate text-sm leading-[18px]">{selected.label}</span>
        </span>
        <Glyph className="icon-xs shrink-0 text-tertiary opacity-65" name="chevronDown" size={16} />
      </button>
      <OcDropdown anchorRef={triggerRef} open={open} onOpenChange={setOpen} align="end" contentWidth="menuWide" aria-label={ariaLabel}>
        <div className="max-h-80 overflow-y-auto pb-1">
          {options.map((option) => (
            <OcMenuItem
              aria-checked={option.id === value}
              data-text-value={option.label}
              key={option.id}
              label={(
                <span className="flex min-w-0 items-center gap-2">
                  <CodeThemeSwatch accent={option.accent} background={option.background} foreground={option.foreground} />
                  <span className="truncate">{option.label}</span>
                </span>
              )}
              onClick={() => {
                onChange(option.id);
                setOpen(false);
              }}
              role="menuitemradio"
              selected={option.id === value}
            />
          ))}
        </div>
      </OcDropdown>
    </>
  );
}

function CodeThemeSwatch({
  accent,
  background,
  foreground,
}: {
  accent: string;
  background: string;
  foreground: string;
}) {
  return (
    <span
      aria-hidden="true"
      className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md border text-xs leading-none font-semibold"
      style={{
        backgroundColor: background,
        borderColor: `color-mix(in srgb, ${foreground} 16%, ${background})`,
        color: accent,
      }}
    >
      Aa
    </span>
  );
}

function ThemeRow({ label, control, className }: { label: ReactNode; control: ReactNode; className?: string }) {
  return (
    <div className={cx("settings-theme-row flex items-center justify-between gap-4 px-4 py-2", className)}>
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <div className="flex min-w-0 flex-col gap-0.5">
          <div className="min-w-0 break-words text-sm text-default">{label}</div>
        </div>
      </div>
      <div className="flex max-w-full shrink-0 items-center gap-2">{control}</div>
    </div>
  );
}

function PreferenceRow({
  label,
  description,
  control,
  className,
}: {
  label: ReactNode;
  description?: ReactNode;
  control: ReactNode;
  className?: string;
}) {
  return (
    <div className={cx("settings-preference-row flex items-center justify-between px-4 gap-6 py-3", className)}>
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <div className="flex min-w-0 flex-col gap-0.5">
          <div className="min-w-0 break-words text-token-text-primary text-sm font-medium">{label}</div>
          {description ? (
            <div className="min-w-0 text-xs leading-4 text-wrap break-words text-token-text-secondary">{description}</div>
          ) : null}
        </div>
      </div>
      <div className="flex max-w-full shrink-0 items-center gap-2">{control}</div>
    </div>
  );
}

function ColorField({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<{ right: number; bottom: number } | null>(null);
  const [draft, setDraft] = useState(value.toUpperCase());
  useEffect(() => setDraft(value.toUpperCase()), [value]);

  useLayoutEffect(() => {
    if (!open) {
      setAnchor(null);
      return;
    }
    const trigger = triggerRef.current;
    if (!trigger) return;
    const update = () => {
      const rect = trigger.getBoundingClientRect();
      const ratio = window.devicePixelRatio || 1;
      const round = (value: number) => Math.round(value * ratio) / ratio;
      setAnchor({
        right: round(window.innerWidth - rect.right),
        bottom: round(window.innerHeight - rect.top + 8),
      });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(trigger);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!triggerRef.current?.contains(target) && !popoverRef.current?.contains(target)) setOpen(false);
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  const foreground = readableColor(value);
  const updateDraft = (next: string) => {
    const sanitized = sanitizeHexDraft(next);
    if (!/^#[0-9A-F]{6}$/.test(sanitized)) {
      setDraft(sanitized);
      return;
    }
    setDraft(sanitized);
    onChange(sanitized.toLowerCase());
  };
  return (
    <>
      <div className="relative flex h-7 w-full max-w-[8.5rem] items-center gap-2 rounded-lg border border-transparent px-2 shadow-sm max-sm:max-w-none" style={{ backgroundColor: value, color: foreground }}>
        <button
          aria-expanded={open}
          aria-haspopup="dialog"
          aria-label={`Choose ${label}`}
          className="h-3.5 w-3.5 shrink-0 rounded-full disabled:cursor-default"
          onClick={() => setOpen((current) => !current)}
          ref={triggerRef}
          style={{ backgroundColor: value, border: `1px solid color-mix(in srgb, ${foreground} 18%, ${value})` }}
          type="button"
        >
          <span aria-hidden="true" className="sr-only" />
        </button>
        <input
          aria-label={label}
          className="min-w-0 flex-1 bg-transparent text-xs uppercase tabular-nums outline-hidden disabled:cursor-default"
          onBlur={() => setDraft(value.toUpperCase())}
          onChange={(event) => updateDraft(event.currentTarget.value)}
          onClick={(event) => event.stopPropagation()}
          onPointerDown={(event) => event.stopPropagation()}
          spellCheck={false}
          type="text"
          value={draft}
        />
      </div>
      {open && anchor ? createPortal(
        <OcMenuSurface
          aria-label={`${label} picker`}
          className="fixed w-auto origin-[var(--radix-popover-content-transform-origin)] outline-hidden"
          data-align="end"
          data-settings-layer="true"
          data-side="top"
          data-state="open"
          role="dialog"
          surface="panel"
          surfaceRef={popoverRef}
          style={{ right: anchor.right, bottom: anchor.bottom }}
          tabIndex={-1}
        >
          <HexColorPicker className="h-34 w-34" color={value} onChange={onChange} />
        </OcMenuSurface>,
        document.body,
      ) : null}
    </>
  );
}

function ThemeFontField({ label, value, placeholder, onChange }: { label: string; value: string; placeholder: string; onChange: (value: string) => void }) {
  const commit = (input: HTMLInputElement) => {
    const next = input.value.trim();
    input.value = next;
    onChange(next);
  };
  return (
    <input
      aria-label={label}
      className="focus-visible:ring-token-focus h-7 w-full max-w-[8.5rem] rounded-lg border border-token-border bg-token-input-background px-2 text-xs text-token-text-primary shadow-sm outline-none focus-visible:ring-2 max-sm:max-w-none"
      defaultValue={value}
      key={`${label}:${value}`}
      onBlur={(event) => commit(event.currentTarget)}
      onKeyDown={(event) => {
        if (event.key !== "Enter") return;
        event.preventDefault();
        commit(event.currentTarget);
      }}
      placeholder={placeholder}
      spellCheck={false}
      type="text"
    />
  );
}

function ThemeContrast({ label, value, accent, background, onChange }: { label: string; value: number; accent: string; background: string; onChange: (value: number) => void }) {
  const style = {
    background: `linear-gradient(90deg, color-mix(in srgb, ${accent} 35%, ${background}) 0%, ${accent} 32%, ${accent} 100%)`,
    color: "var(--color-text)",
  } as CSSProperties;
  return (
    <div className="flex h-9 min-w-[12rem] items-center gap-2.5 max-sm:w-full max-sm:min-w-0">
      <input aria-label={label} className={rangeClass} max={100} min={0} onChange={(event) => onChange(Number(event.currentTarget.value))} step={1} style={style} type="range" value={value} />
      <span className="w-9 text-right text-sm text-token-text-primary tabular-nums">{value}</span>
    </div>
  );
}

function PreferenceSegmented<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: Array<{ value: T; label: string; ariaLabel?: string }>; onChange: (value: T) => void }) {
  return (
    <div aria-label={label} className="inline-flex hide-scrollbar relative min-w-0 max-w-full items-center gap-0.5 overflow-x-auto overflow-y-hidden" role="group">
      {options.map((option) => {
        const selected = value === option.value;
        return (
          <button
            aria-label={option.ariaLabel ?? option.label}
            aria-pressed={selected}
            className={cx(
              "no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 focus-visible:ring-inset disabled:cursor-default disabled:opacity-40 rounded-full border-transparent px-2 py-0.5 text-sm leading-[18px] shrink-0",
              selected
                ? "text-default bg-text/5 enabled:hover:bg-text/10 data-[state=open]:bg-text/10"
                : "text-tertiary enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover",
            )}
            key={option.value}
            onClick={() => onChange(option.value)}
            type="button"
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

function NumberUnitField({ label, value, min, max, onChange }: { label: string; value: number; min: number; max: number; onChange: (value: number) => void }) {
  const commit = (input: HTMLInputElement) => {
    const next = Number.parseFloat(input.value);
    if (Number.isNaN(next)) {
      input.value = String(value);
      return;
    }
    input.value = String(next);
    if (next !== value) onChange(next);
  };
  return (
    <div className="flex items-center gap-2">
      <input
        aria-label={label}
        className="focus-visible:ring-token-focus h-token-button-composer w-16 rounded-lg border border-token-border bg-token-input-background px-2 py-0 text-right text-sm text-token-text-primary shadow-sm outline-none focus-visible:ring-2"
        defaultValue={value}
        key={value}
        max={max}
        min={min}
        onBlur={(event) => commit(event.currentTarget)}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          commit(event.currentTarget);
        }}
        step={1}
        type="number"
      />
      <span className="text-sm text-token-text-secondary">px</span>
    </div>
  );
}

const THEME_SHARE_PREFIX = "octopus-theme-v1:";

function themeShareString(appearance: AppearanceSettings, mode: ThemeKind): string {
  const light = mode === "light";
  return `${THEME_SHARE_PREFIX}${JSON.stringify({
    codeThemeId: light ? appearance.lightCodeTheme : appearance.darkCodeTheme,
    theme: {
      accent: appearanceAccentColor(appearance, mode).toLowerCase(),
      accentSource: appearance[`${mode}AccentSource`],
      contrast: light ? appearance.lightContrast : appearance.darkContrast,
      fonts: {
        code: (light ? appearance.lightCodeFont : appearance.darkCodeFont) || null,
        ui: (light ? appearance.lightUiFont : appearance.darkUiFont) || null,
      },
      ink: (light ? appearance.lightForeground : appearance.darkForeground).toLowerCase(),
      opaqueWindows: !(light ? appearance.lightTranslucentSidebar : appearance.darkTranslucentSidebar),
      semanticColors: {
        diffAdded: (light ? appearance.lightDiffAdded : appearance.darkDiffAdded).toLowerCase(),
        diffRemoved: (light ? appearance.lightDiffRemoved : appearance.darkDiffRemoved).toLowerCase(),
        skill: (light ? appearance.lightSkill : appearance.darkSkill).toLowerCase(),
      },
      surface: (light ? appearance.lightBackground : appearance.darkBackground).toLowerCase(),
    },
    variant: mode,
  })}`;
}

function readThemeShare(value: string, mode: ThemeKind): Partial<AppearanceSettings> {
  const input = value.trim();
  if (!input.startsWith(THEME_SHARE_PREFIX)) throw new Error(`Theme string must start with ${THEME_SHARE_PREFIX}`);

  let parsed: unknown;
  try {
    parsed = JSON.parse(input.slice(THEME_SHARE_PREFIX.length));
  } catch {
    throw new Error("Theme string contains invalid JSON.");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("Theme string must contain an object.");
  const envelope = parsed as Record<string, unknown>;
  if (envelope.variant !== mode) throw new Error(`Expected a ${mode} theme string.`);
  if (!envelope.theme || typeof envelope.theme !== "object" || Array.isArray(envelope.theme)) throw new Error("Theme settings are missing.");

  const theme = envelope.theme as Record<string, unknown>;
  if (theme.accentSource !== undefined && theme.accentSource !== "codex" && theme.accentSource !== "custom") throw new Error("Invalid accent source.");
  const colors = ["accent", "surface", "ink"] as const;
  for (const key of colors) {
    if (typeof theme[key] !== "string" || !/^#[0-9a-f]{6}$/i.test(theme[key] as string)) throw new Error(`Invalid ${key}.`);
  }
  if (typeof theme.contrast !== "number" || !Number.isFinite(theme.contrast) || theme.contrast < 0 || theme.contrast > 100) throw new Error("Invalid contrast.");
  if (typeof theme.opaqueWindows !== "boolean") throw new Error("Invalid opaqueWindows.");
  if (!theme.fonts || typeof theme.fonts !== "object" || Array.isArray(theme.fonts)) throw new Error("Invalid font settings.");
  const fonts = theme.fonts as Record<string, unknown>;
  if (fonts.ui !== null && typeof fonts.ui !== "string") throw new Error("Invalid UI font.");
  if (fonts.code !== null && typeof fonts.code !== "string") throw new Error("Invalid code font.");
  if (!theme.semanticColors || typeof theme.semanticColors !== "object" || Array.isArray(theme.semanticColors)) throw new Error("Invalid semantic colors.");
  const semanticColors = theme.semanticColors as Record<string, unknown>;
  for (const key of ["diffAdded", "diffRemoved", "skill"]) {
    if (typeof semanticColors[key] !== "string" || !/^#[0-9a-f]{6}$/i.test(semanticColors[key] as string)) {
      throw new Error(`Invalid ${key}.`);
    }
  }

  const codeThemes = mode === "light" ? lightCodeThemes : darkCodeThemes;
  const importedCodeTheme = codeThemes.find((themeOption) => themeOption.id === envelope.codeThemeId)?.id;
  if (!importedCodeTheme) throw new Error("Invalid codeThemeId.");

  const accent = (theme.accent as string).toLowerCase();
  const surface = (theme.surface as string).toLowerCase();
  const ink = (theme.ink as string).toLowerCase();
  const uiFont = typeof fonts.ui === "string" ? fonts.ui.trim() : "";
  const codeFont = typeof fonts.code === "string" ? fonts.code.trim() : "";
  return mode === "light"
    ? {
        lightAccent: accent,
        lightAccentSource: theme.accentSource === "codex" ? "codex" : "custom",
        lightBackground: surface,
        lightCodeFont: codeFont,
        lightForeground: ink,
        lightDiffAdded: semanticColors.diffAdded as string,
        lightDiffRemoved: semanticColors.diffRemoved as string,
        lightSkill: semanticColors.skill as string,
        lightTranslucentSidebar: !theme.opaqueWindows,
        lightContrast: theme.contrast,
        lightCodeTheme: importedCodeTheme,
        lightUiFont: uiFont,
      }
    : {
        darkAccent: accent,
        darkAccentSource: theme.accentSource === "codex" ? "codex" : "custom",
        darkBackground: surface,
        darkCodeFont: codeFont,
        darkForeground: ink,
        darkDiffAdded: semanticColors.diffAdded as string,
        darkDiffRemoved: semanticColors.diffRemoved as string,
        darkSkill: semanticColors.skill as string,
        darkTranslucentSidebar: !theme.opaqueWindows,
        darkContrast: theme.contrast,
        darkCodeTheme: importedCodeTheme,
        darkUiFont: uiFont,
      };
}

function isThemeShareValid(value: string, mode: ThemeKind): boolean {
  try {
    readThemeShare(value, mode);
    return true;
  } catch {
    return false;
  }
}

function sanitizeHexDraft(value: string): string {
  const hex = value.toUpperCase().replace(/[^0-9A-F#]/g, "").replace(/#/g, "");
  return hex.length === 0 ? "#" : `#${hex.slice(0, 6)}`;
}

function readableColor(hex: string): "#101010" | "#FFFFFF" {
  const red = Number.parseInt(hex.slice(1, 3), 16);
  const green = Number.parseInt(hex.slice(3, 5), 16);
  const blue = Number.parseInt(hex.slice(5, 7), 16);
  return red * 0.299 + green * 0.587 + blue * 0.114 > 155 ? "#101010" : "#FFFFFF";
}

function capitalize(value: string): string {
  return `${value.charAt(0).toUpperCase()}${value.slice(1)}`;
}
