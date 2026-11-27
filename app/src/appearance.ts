import { useSyncExternalStore } from "react";
import codeThemeSeedData from "./design-system/code-theme-seeds.json";
import octopusAccentProfile from "./design-system/accent-profile.json";

export type ThemeMode = "system" | "light" | "dark";
export type AccentSource = "codex" | "custom";
export type MotionMode = "system" | "reduce" | "full";
export type Density = "compact" | "default" | "comfortable";
export type ChatTextSize = "small" | "default" | "large";
export type CornerSoftness = "sharp" | "default" | "round";
export type DiffMarkerStyle = "color" | "symbols";
export type CodeThemeId =
  | "absolutely"
  | "ayu"
  | "catppuccin"
  | "codex"
  | "dracula"
  | "everforest"
  | "github"
  | "gruvbox"
  | "linear"
  | "lobster"
  | "material"
  | "matrix"
  | "monokai"
  | "night-owl"
  | "nord"
  | "notion"
  | "og"
  | "one"
  | "oscurange"
  | "proof"
  | "raycast"
  | "rose-pine"
  | "sentry"
  | "solarized"
  | "temple"
  | "tokyo-night"
  | "vercel"
  | "vscode-plus"
  | "xcode";

export type CodeThemeDefinition = Readonly<{
  id: CodeThemeId;
  label: string;
  background: string;
  accent: string;
  foreground: string;
}>;

export type CodeThemeKind = "light" | "dark";
export type SemanticPalette = Readonly<{
  blue: string;
  green: string;
  orange: string;
  purple: string;
  red: string;
  yellow: string;
}>;
export type CodeThemeSeed = Readonly<{
  accent: string;
  ink: string;
  surface: string;
  contrast?: number;
  fonts?: Readonly<{ code?: string | null; content?: string | null; ui?: string | null }>;
  opaqueWindows?: boolean;
  palette?: SemanticPalette;
  semanticColors: Readonly<{
    diffAdded: string;
    diffRemoved: string;
    skill: string;
  }>;
}>;

const codeThemeSeeds = codeThemeSeedData as unknown as Record<
  CodeThemeKind,
  Partial<Record<CodeThemeId, CodeThemeSeed>>
>;

export const lightCodeThemes = codeThemeDefinitions("light", [
  ["absolutely", "Absolutely"], ["catppuccin", "Catppuccin"], ["codex", "Codex"],
  ["everforest", "Everforest"], ["github", "GitHub"], ["gruvbox", "Gruvbox"],
  ["linear", "Linear"], ["notion", "Notion"], ["one", "One"], ["proof", "Proof"],
  ["raycast", "Raycast"], ["rose-pine", "Rose Pine"], ["solarized", "Solarized"],
  ["vercel", "Vercel"], ["vscode-plus", "VS Code Plus"], ["xcode", "Xcode"],
]);

export const darkCodeThemes = codeThemeDefinitions("dark", [
  ["absolutely", "Absolutely"], ["ayu", "Ayu"], ["catppuccin", "Catppuccin"],
  ["codex", "Codex"], ["dracula", "Dracula"], ["everforest", "Everforest"],
  ["github", "GitHub"], ["gruvbox", "Gruvbox"], ["linear", "Linear"],
  ["lobster", "Lobster"], ["material", "Material"], ["matrix", "Matrix"],
  ["monokai", "Monokai"], ["night-owl", "Night Owl"], ["nord", "Nord"],
  ["notion", "Notion"], ["og", "OG Dark"], ["one", "One"], ["oscurange", "Oscurange"],
  ["raycast", "Raycast"], ["rose-pine", "Rose Pine"], ["sentry", "Sentry"],
  ["solarized", "Solarized"], ["temple", "Temple"], ["tokyo-night", "Tokyo Night"],
  ["vercel", "Vercel"], ["vscode-plus", "VS Code Plus"], ["xcode", "Xcode"],
]);

export function codeThemeSeed(kind: CodeThemeKind, id: CodeThemeId): CodeThemeSeed {
  const seed = codeThemeSeeds[kind][id];
  if (!seed) throw new Error(`Code theme ${id} is unavailable for ${kind} mode.`);
  return seed;
}

function codeThemeDefinitions(
  kind: CodeThemeKind,
  values: ReadonlyArray<readonly [CodeThemeId, string]>,
): readonly CodeThemeDefinition[] {
  return Object.freeze(values.map(([id, label]) => {
    const seed = codeThemeSeed(kind, id);
    return {
      id,
      label,
      background: seed.surface,
      accent: seed.accent,
      foreground: seed.ink,
    };
  }));
}

const lightCodeThemeIds = lightCodeThemes.map((theme) => theme.id);
const darkCodeThemeIds = darkCodeThemes.map((theme) => theme.id);

export type AppearanceSettings = {
  themeMode: ThemeMode;
  lightAccent: string;
  lightAccentSource: AccentSource;
  lightBackground: string;
  lightForeground: string;
  lightDiffAdded: string;
  lightDiffRemoved: string;
  lightSkill: string;
  lightTranslucentSidebar: boolean;
  lightContrast: number;
  lightUiFont: string;
  lightCodeFont: string;
  darkAccent: string;
  darkAccentSource: AccentSource;
  darkBackground: string;
  darkForeground: string;
  darkDiffAdded: string;
  darkDiffRemoved: string;
  darkSkill: string;
  darkTranslucentSidebar: boolean;
  darkContrast: number;
  darkUiFont: string;
  darkCodeFont: string;
  lightCodeTheme: CodeThemeId;
  darkCodeTheme: CodeThemeId;
  uiFontSize: number;
  codeFontSize: number;
  pointerCursors: boolean;
  motionMode: MotionMode;
  diffMarkerStyle: DiffMarkerStyle;
  fontSmoothing: boolean;
  density: Density;
  chatTextSize: ChatTextSize;
  cornerSoftness: CornerSoftness;
  octopuses: boolean;
};

const STORAGE_KEY = "octopus.appearance.v3";

export const defaultAppearance: AppearanceSettings = {
  themeMode: "system",
  lightAccent: "#339CFF",
  lightAccentSource: "codex",
  lightBackground: "#FFFFFF",
  lightForeground: "#1A1C1F",
  lightDiffAdded: "#00A240",
  lightDiffRemoved: "#BA2623",
  lightSkill: "#924FF7",
  lightTranslucentSidebar: true,
  lightContrast: 45,
  lightUiFont: "",
  lightCodeFont: "",
  darkAccent: "#339CFF",
  darkAccentSource: "codex",
  darkBackground: "#181818",
  darkForeground: "#FFFFFF",
  darkDiffAdded: "#40C977",
  darkDiffRemoved: "#FA423E",
  darkSkill: "#AD7BF9",
  darkTranslucentSidebar: true,
  darkContrast: 60,
  darkUiFont: "",
  darkCodeFont: "",
  lightCodeTheme: "codex",
  darkCodeTheme: "codex",
  uiFontSize: 14,
  codeFontSize: 12,
  pointerCursors: false,
  motionMode: "system",
  diffMarkerStyle: "color",
  fontSmoothing: true,
  density: "default",
  chatTextSize: "default",
  cornerSoftness: "default",
  octopuses: false,
};

const densityScale: Record<Density, number> = { compact: 0.84, default: 1, comfortable: 1.16 };
const chatFontPx: Record<ChatTextSize, number> = { small: 13, default: 14, large: 16 };
const cornerScale: Record<CornerSoftness, number> = { sharp: 0.64, default: 1, round: 1.32 };
const spacingTokens: Record<string, number> = {
  "--oc-space-1": 2,
  "--oc-space-2": 4,
  "--oc-space-3": 6,
  "--oc-space-4": 8,
  "--oc-space-5": 12,
  "--oc-space-6": 16,
  "--oc-space-7": 20,
  "--oc-space-8": 24,
  "--oc-space-9": 32,
  "--oc-control-px-xs": 7,
  "--oc-control-px-sm": 10,
  "--oc-control-px-md": 12,
};
const controlHeightTokens: Record<string, number> = {
  "--oc-control-h-xs": 24,
  "--oc-control-h-sm": 28,
  "--oc-control-h-md": 32,
};
const radiusTokens: Record<string, number> = {
  "--oc-radius-xs": 4,
  "--oc-radius-sm": 7,
  "--oc-radius-md": 10,
  "--oc-radius-lg": 14,
  "--oc-radius-card": 20,
};
const uiFontBases = [
  6.8, 7, 7.2, 7.3, 7.4, 7.5, 7.6, 7.7, 7.8, 8, 8.2, 8.4, 8.5, 8.6, 8.8,
  9, 9.2, 9.5, 10, 10.5, 11, 11.5, 12, 12.5, 13, 13.5, 14, 14.5, 15, 16, 17,
  18, 19, 20, 22, 24, 25, 26,
];
const inlineVscodeTypographyVariables = new Set([
  "--vscode-chat-editor-font-size",
  "--vscode-chat-font-size",
  "--vscode-editor-font-family",
  "--vscode-editor-font-size",
  "--vscode-font-family",
  "--vscode-font-size",
]);
const listeners = new Set<() => void>();
const appliedThemeVariables = new WeakMap<HTMLElement, Set<string>>();
let observersInstalled = false;
let current = load();

function load(): AppearanceSettings {
  try {
    if (typeof window === "undefined") return { ...defaultAppearance };
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...defaultAppearance };
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return {
      themeMode: choice(parsed.themeMode, ["system", "light", "dark"], defaultAppearance.themeMode),
      lightAccent: color(parsed.lightAccent, defaultAppearance.lightAccent),
      lightAccentSource: readAccentSource(parsed, "light"),
      lightBackground: color(parsed.lightBackground, defaultAppearance.lightBackground),
      lightForeground: color(parsed.lightForeground, defaultAppearance.lightForeground),
      lightDiffAdded: color(parsed.lightDiffAdded, defaultAppearance.lightDiffAdded),
      lightDiffRemoved: color(parsed.lightDiffRemoved, defaultAppearance.lightDiffRemoved),
      lightSkill: color(parsed.lightSkill, defaultAppearance.lightSkill),
      lightTranslucentSidebar: booleanValue(parsed.lightTranslucentSidebar, defaultAppearance.lightTranslucentSidebar),
      lightContrast: numberValue(parsed.lightContrast, defaultAppearance.lightContrast, 0, 100),
      lightUiFont: stringValue(parsed.lightUiFont),
      lightCodeFont: stringValue(parsed.lightCodeFont),
      darkAccent: color(parsed.darkAccent, defaultAppearance.darkAccent),
      darkAccentSource: readAccentSource(parsed, "dark"),
      darkBackground: color(parsed.darkBackground, defaultAppearance.darkBackground),
      darkForeground: color(parsed.darkForeground, defaultAppearance.darkForeground),
      darkDiffAdded: color(parsed.darkDiffAdded, defaultAppearance.darkDiffAdded),
      darkDiffRemoved: color(parsed.darkDiffRemoved, defaultAppearance.darkDiffRemoved),
      darkSkill: color(parsed.darkSkill, defaultAppearance.darkSkill),
      darkTranslucentSidebar: booleanValue(parsed.darkTranslucentSidebar, defaultAppearance.darkTranslucentSidebar),
      darkContrast: numberValue(parsed.darkContrast, defaultAppearance.darkContrast, 0, 100),
      darkUiFont: stringValue(parsed.darkUiFont),
      darkCodeFont: stringValue(parsed.darkCodeFont),
      lightCodeTheme: choice(parsed.lightCodeTheme, lightCodeThemeIds, defaultAppearance.lightCodeTheme),
      darkCodeTheme: choice(parsed.darkCodeTheme, darkCodeThemeIds, defaultAppearance.darkCodeTheme),
      uiFontSize: numberValue(parsed.uiFontSize, defaultAppearance.uiFontSize, 11, 20),
      codeFontSize: numberValue(parsed.codeFontSize, defaultAppearance.codeFontSize, 10, 20),
      pointerCursors: booleanValue(parsed.pointerCursors, defaultAppearance.pointerCursors),
      motionMode: choice(parsed.motionMode, ["system", "reduce", "full"], defaultAppearance.motionMode),
      diffMarkerStyle: choice(parsed.diffMarkerStyle, ["color", "symbols"], defaultAppearance.diffMarkerStyle),
      fontSmoothing: booleanValue(parsed.fontSmoothing, defaultAppearance.fontSmoothing),
      density: choice(parsed.density, ["compact", "default", "comfortable"], defaultAppearance.density),
      chatTextSize: choice(parsed.chatTextSize, ["small", "default", "large"], defaultAppearance.chatTextSize),
      cornerSoftness: choice(parsed.cornerSoftness, ["sharp", "default", "round"], defaultAppearance.cornerSoftness),
      octopuses: booleanValue(parsed.octopuses, defaultAppearance.octopuses),
    };
  } catch {
    return { ...defaultAppearance };
  }
}

function readAccentSource(value: Record<string, unknown>, mode: CodeThemeKind): AccentSource {
  const native = color(value[`${mode}Accent`], defaultAppearance[`${mode}Accent`]).toLowerCase() === defaultAppearance[`${mode}Accent`].toLowerCase()
    && color(value[`${mode}Background`], defaultAppearance[`${mode}Background`]).toLowerCase() === defaultAppearance[`${mode}Background`].toLowerCase()
    && color(value[`${mode}Foreground`], defaultAppearance[`${mode}Foreground`]).toLowerCase() === defaultAppearance[`${mode}Foreground`].toLowerCase()
    && numberValue(value[`${mode}Contrast`], defaultAppearance[`${mode}Contrast`], 0, 100) === defaultAppearance[`${mode}Contrast`];
  return choice(value[`${mode}AccentSource`], ["codex", "custom"], native ? "codex" : "custom");
}

export function appearanceAccentColor(settings: AppearanceSettings, mode: CodeThemeKind): string {
  return settings[`${mode}AccentSource`] === "codex" ? octopusAccentProfile[mode].accent : settings[`${mode}Accent`];
}

export function applyAppearance(settings: AppearanceSettings = current): void {
  if (typeof document === "undefined") return;
  installObservers();
  const root = document.documentElement;
  const theme = resolveTheme(settings.themeMode);
  const palette = theme === "dark"
    ? {
        accent: appearanceAccentColor(settings, "dark"),
        background: settings.darkBackground,
        foreground: settings.darkForeground,
        diffAdded: settings.darkDiffAdded,
        diffRemoved: settings.darkDiffRemoved,
        skill: settings.darkSkill,
        contrast: settings.darkContrast,
        translucentSidebar: settings.darkTranslucentSidebar,
        uiFont: settings.darkUiFont,
        codeFont: settings.darkCodeFont,
      }
    : {
        accent: appearanceAccentColor(settings, "light"),
        background: settings.lightBackground,
        foreground: settings.lightForeground,
        diffAdded: settings.lightDiffAdded,
        diffRemoved: settings.lightDiffRemoved,
        skill: settings.lightSkill,
        contrast: settings.lightContrast,
        translucentSidebar: settings.lightTranslucentSidebar,
        uiFont: settings.lightUiFont,
        codeFont: settings.lightCodeFont,
      };
  const reduced = shouldReduceMotion(settings);
  const uiScale = settings.uiFontSize / defaultAppearance.uiFontSize;
  const foregroundRgb = hexRgb(palette.foreground);
  const selectedCodeThemeId = theme === "dark" ? settings.darkCodeTheme : settings.lightCodeTheme;
  const codeThemes = theme === "dark" ? darkCodeThemes : lightCodeThemes;
  const selectedCodeTheme = codeThemes.find((candidate) => candidate.id === selectedCodeThemeId)
    ?? codeThemes.find((candidate) => candidate.id === "codex")
    ?? codeThemes[0];
  const selectedSeed = codeThemeSeeds[theme][selectedCodeTheme.id];
  const octopusTheme = resolveOctopusTheme(theme, palette, settings[`${theme}AccentSource`] === "codex", selectedSeed);

  root.dataset.theme = theme;
  root.dataset.themePreference = settings.themeMode;
  root.dataset.codeTheme = selectedCodeTheme.id;
  root.dataset.appearancePreset = selectedCodeTheme.id;
  root.dataset.diffMarkerStyle = settings.diffMarkerStyle;
  root.dataset.density = settings.density;
  root.dataset.octopusOs = "darwin";
  root.dataset.octopusWindowType = "electron";
  root.dataset.windowType = "electron";
  root.dataset.octopusWindowChrome = "native";
  root.classList.toggle("electron-dark", theme === "dark");
  root.classList.toggle("electron-light", theme === "light");
  root.classList.toggle("reduce-motion", reduced);
  root.classList.toggle("full-motion", settings.motionMode === "full");
  root.classList.toggle("pointer-cursors", settings.pointerCursors);
  root.classList.toggle("font-smoothing", settings.fontSmoothing);
  root.classList.toggle("translucent-sidebar", palette.translucentSidebar);
  window.octopusTransport?.setWindowAppearance({ theme, preference: settings.themeMode, translucent: palette.translucentSidebar });
  root.style.setProperty("--ui-font-scale", String(uiScale));
  root.style.setProperty("--chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--octopus-chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--octopus-chat-code-font-size", `${settings.codeFontSize}px`);
  root.style.setProperty("--vscode-chat-editor-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--vscode-editor-font-size", `${settings.codeFontSize}px`);
  root.style.setProperty("--vscode-chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--corner-scale", String(cornerScale[settings.cornerSoftness]));
  root.style.setProperty("--ui-font-size", `${settings.uiFontSize}px`);
  root.style.setProperty("--code-font-size", `${settings.codeFontSize}px`);
  const textSizes: Record<string, number> = {
    "--text-4xl": 72,
    "--text-3xl": 48,
    "--text-2xl": 36,
    "--text-xl": 28,
    "--text-lg": 16,
    "--text-base": defaultAppearance.uiFontSize,
    "--text-sm": defaultAppearance.uiFontSize - 2,
    "--text-xs": defaultAppearance.uiFontSize - 3,
    "--text-heading-app": 28,
    "--text-heading-lg": 24,
    "--text-heading-md": 20,
    "--text-heading-sm": 18,
  };
  for (const element of [root, document.body]) {
    element.style.setProperty("--vscode-font-size", `${settings.uiFontSize}px`);
    for (const [name, size] of Object.entries(textSizes)) {
      element.style.setProperty(name, `${Math.round(size * uiScale)}px`);
    }
  }
  root.style.setProperty("--oc-type-xs", scaledPx(10, uiScale));
  root.style.setProperty("--oc-type-sm", scaledPx(12, uiScale));
  root.style.setProperty("--oc-type-md", scaledPx(14, uiScale));
  root.style.setProperty("--oc-type-lg", scaledPx(16, uiScale));
  root.style.setProperty("--oc-type-xl", scaledPx(20, uiScale));
  for (const base of uiFontBases) {
    root.style.setProperty(`--ui-font-${String(base).replace(".", "_")}`, scaledPx(base, uiScale));
  }
  setScaledTokens(root, spacingTokens, densityScale[settings.density]);
  setScaledTokens(root, controlHeightTokens, Math.max(densityScale[settings.density], uiScale * 0.82));
  setScaledTokens(root, radiusTokens, cornerScale[settings.cornerSoftness]);
  const uiFont = [palette.uiFont, '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'].filter(Boolean).join(", ");
  const codeFont = [palette.codeFont, 'ui-monospace, "SFMono-Regular", "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'].filter(Boolean).join(", ");
  for (const element of [root, document.body]) {
    element.style.setProperty("--font-ui", uiFont);
    element.style.setProperty("--font-mono", codeFont);
    if (palette.uiFont) element.style.setProperty("--vscode-font-family", uiFont);
    else element.style.removeProperty("--vscode-font-family");
    if (palette.codeFont) element.style.setProperty("--vscode-editor-font-family", codeFont);
    else element.style.removeProperty("--vscode-editor-font-family");
  }
  clearDerivedOctopusVariables(root);
  const previousApplied = appliedThemeVariables.get(root);
  if (previousApplied) {
    for (const name of previousApplied) {
      if (!(name in octopusTheme.variables)) root.style.removeProperty(name);
    }
  }
  const nextApplied = new Set<string>();
  for (const [name, value] of Object.entries(octopusTheme.variables)) {
    root.style.setProperty(name, value);
    nextApplied.add(name);
  }
  appliedThemeVariables.set(root, nextApplied);
  root.style.setProperty("--code-theme-background", selectedCodeTheme.background);
  root.style.setProperty("--code-theme-accent", selectedCodeTheme.accent);
  root.style.setProperty("--code-theme-foreground", selectedCodeTheme.foreground);
  root.style.setProperty("--oc-color-canvas", palette.background);
  root.style.setProperty("--oc-color-sidebar", colorHex(octopusTheme.sidebarSurface));
  root.style.setProperty("--oc-color-surface", octopusTheme.panel);
  root.style.setProperty("--oc-color-card", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--oc-color-raised", octopusTheme.controlBackgroundOpaque);
  root.style.setProperty("--oc-color-control", octopusTheme.buttonSecondaryBackground);
  root.style.setProperty("--oc-color-control-hover", octopusTheme.buttonSecondaryBackgroundHover);
  root.style.setProperty("--oc-color-control-active", octopusTheme.buttonSecondaryBackgroundActive);
  root.style.setProperty("--oc-color-text", octopusTheme.textForeground);
  root.style.setProperty("--oc-color-text-soft", octopusTheme.textForegroundSecondary);
  root.style.setProperty("--oc-color-text-muted", octopusTheme.textForegroundTertiary);
  root.style.setProperty("--oc-color-border", octopusTheme.border);
  root.style.setProperty("--oc-color-border-strong", octopusTheme.borderHeavy);
  root.style.setProperty("--oc-color-accent", palette.accent);
  root.style.setProperty("--oc-color-accent-hover", mixHex(palette.accent, palette.foreground, 0.12));
  root.style.setProperty("--oc-color-danger", theme === "dark" ? "#FF6764" : "#E02E2A");
  root.style.setProperty("--oc-focus-color", "var(--color-ring)");
  root.style.setProperty("--bg", palette.background);
  root.style.setProperty("--bg-deep", palette.background);
  root.style.setProperty("--stage", palette.background);
  root.style.setProperty("--workspace-material", palette.translucentSidebar
    ? colorAlpha(rgbColor(palette.background), 0.9)
    : palette.background);
  root.style.setProperty("--welcome-material", palette.translucentSidebar
    ? colorAlpha(rgbColor(palette.background), 0.88)
    : palette.background);
  root.style.setProperty("--sidebar", palette.translucentSidebar
    ? colorAlpha(octopusTheme.sidebarSurface, 0.78)
    : colorHex(octopusTheme.sidebarSurface));
  root.style.setProperty("--sidebar-soft", colorAlpha(octopusTheme.sidebarSurface, 0.78));
  root.style.setProperty("--panel", octopusTheme.panel);
  root.style.setProperty("--panel-soft", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--panel-raised", octopusTheme.controlBackgroundOpaque);
  root.style.setProperty("--field", rgba(foregroundRgb, 0.035));
  root.style.setProperty("--line", rgba(foregroundRgb, 0.084));
  root.style.setProperty("--line-light", rgba(foregroundRgb, 0.042));
  root.style.setProperty("--line-strong", rgba(foregroundRgb, 0.156));
  root.style.setProperty("--line-bright", rgba(foregroundRgb, 0.29));
  root.style.setProperty("--text", octopusTheme.textForeground);
  root.style.setProperty("--text-soft", rgba(foregroundRgb, 0.72));
  root.style.setProperty("--muted", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--copy", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--copy-dim", rgba(foregroundRgb, 0.5));
  root.style.setProperty("--dim", rgba(foregroundRgb, 0.42));
  root.style.setProperty("--faint", rgba(foregroundRgb, 0.36));
  root.style.setProperty("--text-muted", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--text-faint", rgba(foregroundRgb, 0.42));
  root.style.setProperty("--icon", rgba(foregroundRgb, 0.9));
  root.style.setProperty("--icon-muted", rgba(foregroundRgb, 0.7));
  root.style.setProperty("--icon-dim", rgba(foregroundRgb, 0.5));
  root.style.setProperty("--button-secondary", rgba(foregroundRgb, 0.05));
  root.style.setProperty("--button-secondary-hover", rgba(foregroundRgb, 0.08));
  root.style.setProperty("--button-secondary-active", rgba(foregroundRgb, 0.12));
  root.style.setProperty("--row-hover", rgba(foregroundRgb, 0.055));
  root.style.setProperty("--row-active", rgba(foregroundRgb, 0.11));
  root.style.setProperty("--surface-0", octopusTheme.surfaceUnder);
  root.style.setProperty("--surface-1", palette.background);
  root.style.setProperty("--surface-2", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--surface-3", octopusTheme.controlBackgroundOpaque);
  root.style.setProperty("--blue", palette.accent);
}

type OctopusPalette = {
  accent: string;
  background: string;
  foreground: string;
  diffAdded: string;
  diffRemoved: string;
  skill: string;
  contrast: number;
  translucentSidebar: boolean;
  uiFont: string;
  codeFont: string;
};

type RgbColor = { red: number; green: number; blue: number };

function resolveOctopusTheme(
  theme: "light" | "dark",
  palette: OctopusPalette,
  linkedAccent = false,
  presetSeed?: CodeThemeSeed,
) {
  const contrast = octopusContrast(palette.contrast, theme);
  const surface = rgbColor(palette.background);
  const ink = rgbColor(palette.foreground);
  const accent = rgbColor(palette.accent);
  const black = rgbColor("#000000");
  const white = rgbColor("#ffffff");
  const editorBackground = theme === "light"
    ? mixRgb(surface, white, 0.12)
    : mixRgb(surface, ink, 0.07);
  const sidebarSurface = theme === "light"
    ? mixRgb(surface, rgbColor(colorHex(mixRgb(surface, ink, 0.04 + (palette.contrast - 45) * 0.0012))), 0.5)
    : mixRgb(surface, editorBackground, 0.5);
  const applicationMenu = mixRgb(surface, ink, 0.02 + contrast * 0.02);
  const surfaceUnderRatio = theme === "light"
    ? 0.04 + (palette.contrast - 45) * 0.0012
    : 0.16 + (palette.contrast - 60) * 0.0015;
  const surfaceUnder = colorHex(mixRgb(surface, theme === "light" ? ink : black, surfaceUnderRatio));
  const panel = colorHex(mixRgb(
    surface,
    theme === "light" ? white : ink,
    (theme === "light" ? 0.18 : 0.03) + contrast * (theme === "light" ? 0.008 : 0.03),
  ));
  const diffAdded = color(palette.diffAdded, theme === "light" ? "#00A240" : "#40C977").toLowerCase();
  const diffRemoved = color(palette.diffRemoved, theme === "light" ? "#BA2623" : "#FA423E").toLowerCase();
  const values = theme === "light"
    ? octopusLightColors(surface, ink, accent, contrast)
    : octopusDarkColors(surface, ink, accent, contrast);
  const accentHex = colorHex(accent);
  const foregroundHex = colorHex(ink);
  const surfaceHex = colorHex(surface);
  const defaultSurface = (theme === "dark" ? defaultAppearance.darkBackground : defaultAppearance.lightBackground).toLowerCase();
  const defaultInk = (theme === "dark" ? defaultAppearance.darkForeground : defaultAppearance.lightForeground).toLowerCase();
  const defaultContrast = theme === "dark" ? defaultAppearance.darkContrast : defaultAppearance.lightContrast;
  const editedSurface = surfaceHex !== defaultSurface || foregroundHex !== defaultInk || palette.contrast !== defaultContrast;
  const monochromeAccent = !linkedAccent && (
    (theme === "dark" && accentHex === WHITE_HEX)
    || (theme === "light" && accentHex === BLACK_HEX)
  );
  const useDefaultDarkForeground = theme === "dark"
    && !editedSurface
    && !palette.uiFont
    && !palette.codeFont
    && palette.translucentSidebar === defaultAppearance.darkTranslucentSidebar
    && diffAdded === defaultAppearance.darkDiffAdded.toLowerCase()
    && diffRemoved === defaultAppearance.darkDiffRemoved.toLowerCase()
    && (linkedAccent || monochromeAccent);
  const resolvedValues = useDefaultDarkForeground
    ? { ...values, textForeground: "var(--gray-fixed-150)" }
    : values;

  const variables: Record<string, string> = {
    "--color-background-composer-primary": "var(--color-background-primary-solid)",
    "--color-text-composer-primary": "var(--color-text-primary-solid)",
    "--color-background-user-message": "color-mix(in oklab, var(--color-text) 5%, transparent)",
    "--color-text-user-message": "var(--color-text)",
    "--color-background-text-selection": "var(--color-background-accent-active)",
    "--color-background-attribution-highlight": "var(--color-background-accent-active)",
    "--codex-base-accent": accentHex,
    "--codex-base-contrast": String(palette.contrast),
    "--codex-base-ink": foregroundHex,
    "--codex-base-surface": surfaceHex,
    "--octopus-base-accent": accentHex,
    "--octopus-base-contrast": String(palette.contrast),
    "--octopus-base-ink": foregroundHex,
    "--octopus-base-surface": surfaceHex,
    "--color-background-accent": values.accentBackground,
    "--color-background-accent-active": values.accentBackgroundActive,
    "--color-background-accent-hover": values.accentBackgroundHover,
    "--color-background-application-menu": colorHex(applicationMenu),
    "--color-background-button-primary": values.buttonPrimaryBackground,
    "--color-background-button-primary-active": values.buttonPrimaryBackgroundActive,
    "--color-background-button-primary-hover": values.buttonPrimaryBackgroundHover,
    "--color-background-button-primary-inactive": values.buttonPrimaryBackgroundInactive,
    "--color-background-button-secondary": values.buttonSecondaryBackground,
    "--color-background-button-secondary-active": values.buttonSecondaryBackgroundActive,
    "--color-background-button-secondary-hover": values.buttonSecondaryBackgroundHover,
    "--color-background-button-secondary-inactive": values.buttonSecondaryBackgroundInactive,
    "--color-background-button-tertiary": values.buttonTertiaryBackground,
    "--color-background-button-tertiary-active": values.buttonTertiaryBackgroundActive,
    "--color-background-button-tertiary-hover": values.buttonTertiaryBackgroundHover,
    "--color-background-control": values.controlBackground,
    "--color-background-control-opaque": values.controlBackgroundOpaque,
    "--color-background-callout-surface": values.elevatedPrimary,
    "--color-background-composer-action-bar": theme === "light" ? surfaceUnder : values.elevatedSecondaryOpaque,
    "--color-token-composer-utility-background": "var(--color-background-composer-action-bar)",
    "--color-background-editor-opaque": rgbString(editorBackground),
    "--color-background-elevated-primary": values.elevatedPrimary,
    "--color-background-elevated-primary-opaque": values.elevatedPrimaryOpaque,
    "--color-background-elevated-secondary": values.elevatedSecondary,
    "--color-background-elevated-secondary-opaque": values.elevatedSecondaryOpaque,
    "--color-background-elevated-tertiary-opaque": theme === "light"
      ? colorHex(mixRgb(mixRgb(surface, white, 0.08 + contrast * 0.08), ink, 0.04 + contrast * 0.02))
      : rgbString(editorBackground),
    "--color-background-mode-toggle-track": theme === "light" ? surfaceUnder : values.buttonTertiaryBackgroundHover,
    "--color-background-mode-toggle-selected": theme === "dark" ? values.controlBackgroundOpaque : `color-mix(in oklab, ${values.controlBackground} 90%, transparent)`,
    "--color-background-panel": panel,
    "--color-background-surface": surfaceHex,
    "--color-background-surface-under": surfaceUnder,
    "--color-border": values.border,
    "--color-border-application-menu-separator": theme === "dark" ? colorHex(mixRgb(applicationMenu, ink, 0.28)) : values.borderHeavy,
    "--color-border-focus": values.borderFocus,
    "--color-border-heavy": values.borderHeavy,
    "--color-border-light": values.borderLight,
    "--color-border-mode-toggle-selected": values.border,
    "--color-decoration-added": diffAdded,
    "--color-decoration-deleted": diffRemoved,
    "--color-editor-added": colorAlpha(rgbColor(diffAdded), theme === "light" ? 0.15 : 0.23),
    "--color-editor-deleted": colorAlpha(rgbColor(diffRemoved), theme === "light" ? 0.15 : 0.23),
    "--color-icon-primary": values.iconPrimary,
    "--color-icon-secondary": values.iconSecondary,
    "--color-icon-tertiary": values.iconTertiary,
    "--color-simple-scrim": values.simpleScrim,
    "--color-text-button-primary": values.textButtonPrimary,
    "--color-text-button-secondary": values.textButtonSecondary,
    "--color-text-button-tertiary": values.textButtonTertiary,
    "--color-foreground-application-menu": theme === "dark" ? colorHex(mixRgb(applicationMenu, ink, 0.875)) : resolvedValues.textForeground,
    "--color-text-foreground": resolvedValues.textForeground,
    "--color-text-foreground-secondary": resolvedValues.textForegroundSecondary,
    "--color-text-foreground-tertiary": resolvedValues.textForegroundTertiary,
    "--color-text-mode-toggle-inactive": `color-mix(in oklab, ${resolvedValues.textForeground} 80%, transparent)`,
    "--shadow-mode-toggle-selected": "var(--shadow-md)",
  };

  if (linkedAccent) {
    const profile = octopusAccentProfile[theme];
    Object.assign(variables, {
      "--color-background-composer-primary": profile.submitBackground,
      "--color-text-composer-primary": profile.submitText,
      "--color-background-user-message": profile.userMessageBackground,
      "--color-text-user-message": profile.userMessageText,
      "--color-background-text-selection": profile.selection,
      "--color-background-attribution-highlight": profile.attributionHighlight,
      "--color-background-accent": profile.soft,
      "--color-background-accent-active": profile.soft,
      "--color-background-accent-hover": profile.soft,
      "--color-border-focus": profile.accent,
      "--user-message-background-luminance": String(userMessageBackgroundLuminance(profile.userMessageBackground, surfaceHex, 1)),
    });
    if (editedSurface) {
      Object.assign(variables, solveAccentTextAndRing({
        accent: defaultAppearance[`${theme}Accent`].toLowerCase(),
        surface: surfaceHex,
        foreground: foregroundHex,
      }, [surfaceHex]));
    }
  } else {
    if (monochromeAccent) {
      const solid = theme === "dark" ? WHITE_HEX : BLACK_HEX;
      const hover = theme === "dark" ? "#e0e0e0" : "#1f1f1f";
      const active = theme === "dark" ? "#c2c2c2" : "#3d3d3d";
      const onSolid = theme === "dark" ? BLACK_HEX : WHITE_HEX;
      const selection = theme === "dark"
        ? "color-mix(in srgb, var(--color-text) 32%, transparent)"
        : "#afafaf66";
      const profile = octopusAccentProfile[theme];
      Object.assign(variables, {
        "--color-accent": solid,
        "--color-text-on-accent": onSolid,
        "--color-background-accent-solid": solid,
        "--color-background-accent-solid-hover": hover,
        "--color-background-accent-solid-active": active,
        "--color-text-accent-solid": onSolid,
        ...PERSONALIZED_ACCENT_CONTROL_BINDINGS,
        "--color-text-accent": solid,
        "--color-text-accent-hover": hover,
        "--color-text-accent-soft": solid,
        "--color-ring-accent": solid,
        "--color-border-focus": solid,
        "--color-border-accent-outline": solid,
        "--color-background-accent-soft-alpha": `color-mix(in srgb, ${solid} 10%, transparent)`,
        "--color-background-accent-soft-alpha-hover": `color-mix(in srgb, ${solid} 17%, transparent)`,
        "--color-background-accent-soft-alpha-active": `color-mix(in srgb, ${solid} 25%, transparent)`,
        "--color-background-composer-primary": solid,
        "--color-text-composer-primary": onSolid,
        "--color-background-user-message": theme === "dark" ? profile.userMessageBackground : BLACK_HEX,
        "--color-text-user-message": WHITE_HEX,
        "--color-background-text-selection": selection,
        "--color-background-attribution-highlight": selection,
        "--user-message-background-luminance": String(userMessageBackgroundLuminance(
          theme === "dark" ? profile.userMessageBackground : BLACK_HEX,
          surfaceHex,
          1,
        )),
      });
    } else {
      Object.assign(
        variables,
        solvePersonalizedAccentTokens({
          accent: accentHex,
          surface: surfaceHex,
          foreground: foregroundHex,
        }),
        {
          "--user-message-background-luminance": String(userMessageBackgroundLuminance(foregroundHex, surfaceHex, 0.05)),
        },
      );
    }
    if (presetSeed?.palette) {
      Object.assign(
        variables,
        solveCustomSurfaceSemanticText(theme, surfaceHex, foregroundHex, presetSeed.palette),
      );
    }
  }
  return { ...resolvedValues, panel, sidebarSurface, surfaceUnder, editorBackground, variables };
}

function clearDerivedOctopusVariables(root: HTMLElement): void {
  for (const name of Array.from(root.style)) {
    const derivedToken = name.startsWith("--color-token-");
    const derivedVscode = name.startsWith("--vscode-") && !inlineVscodeTypographyVariables.has(name);
    if (derivedToken || derivedVscode) root.style.removeProperty(name);
  }
}

function octopusLightColors(surface: RgbColor, ink: RgbColor, accent: RgbColor, contrast: number) {
  const white = { red: 255, green: 255, blue: 255 };
  const control = mixRgb(surface, white, 0.09 + contrast * 0.04);
  const elevatedSecondary = mixRgb(surface, white, 0.08 + contrast * 0.08);
  const elevatedPrimary = mixRgb(surface, white, 0.16 + contrast * 0.12);
  return {
    accentBackground: colorHex(mixRgb(surface, accent, 0.11 + contrast * 0.04)),
    accentBackgroundActive: colorHex(mixRgb(surface, accent, 0.13 + contrast * 0.05)),
    accentBackgroundHover: colorHex(mixRgb(surface, accent, 0.12 + contrast * 0.045)),
    border: colorAlpha(ink, 0.06 + contrast * 0.04),
    borderFocus: colorHex(accent),
    borderHeavy: colorAlpha(ink, 0.09 + contrast * 0.06),
    borderLight: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonPrimaryBackground: colorHex(ink),
    buttonPrimaryBackgroundActive: colorAlpha(ink, 0.1 + contrast * 0.12),
    buttonPrimaryBackgroundHover: colorAlpha(ink, 0.05 + contrast * 0.06),
    buttonPrimaryBackgroundInactive: colorAlpha(ink, 0.18 + contrast * 0.14),
    buttonSecondaryBackground: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonSecondaryBackgroundActive: colorAlpha(ink, 0.03 + contrast * 0.02),
    buttonSecondaryBackgroundHover: colorAlpha(ink, 0.04 + contrast * 0.03),
    buttonSecondaryBackgroundInactive: colorAlpha(ink, 0.01 + contrast * 0.02),
    buttonTertiaryBackground: colorAlpha(ink, 0),
    buttonTertiaryBackgroundActive: colorAlpha(ink, 0.16 + contrast * 0.08),
    buttonTertiaryBackgroundHover: colorAlpha(ink, 0.08 + contrast * 0.04),
    controlBackground: colorAlpha(control, 0.96),
    controlBackgroundOpaque: rgbString(control),
    elevatedPrimary: colorAlpha(elevatedPrimary, 0.96),
    elevatedPrimaryOpaque: rgbString(elevatedPrimary),
    elevatedSecondary: colorAlpha(elevatedSecondary, 0.96),
    elevatedSecondaryOpaque: rgbString(elevatedSecondary),
    iconAccent: colorHex(accent),
    iconPrimary: colorHex(ink),
    iconSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    iconTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    simpleScrim: colorAlpha({ red: 0, green: 0, blue: 0 }, 0.08 + contrast * 0.04),
    textAccent: colorHex(accent),
    textButtonPrimary: colorHex(surface),
    textButtonSecondary: colorHex(ink),
    textButtonTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    textForeground: colorHex(ink),
    textForegroundSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    textForegroundTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
  };
}

function octopusDarkColors(surface: RgbColor, ink: RgbColor, accent: RgbColor, contrast: number) {
  const black = { red: 0, green: 0, blue: 0 };
  const white = { red: 255, green: 255, blue: 255 };
  const control = mixRgb(surface, ink, 0.06 + contrast * 0.05);
  const tunedAccent = mixRgb(accent, white, 0.3 + contrast * 0.15);
  const primaryButton = mixRgb(surface, black, 0.38 + contrast * 0.12);
  const elevatedPrimary = mixRgb(surface, ink, 0.08 + contrast * 0.08);
  return {
    accentBackground: colorHex(mixRgb(black, accent, 0.2 + contrast * 0.08)),
    accentBackgroundActive: colorHex(mixRgb(black, accent, 0.22 + contrast * 0.12)),
    accentBackgroundHover: colorHex(mixRgb(black, accent, 0.21 + contrast * 0.1)),
    border: colorAlpha(ink, 0.06 + contrast * 0.04),
    borderFocus: colorAlpha(tunedAccent, 0.7 + contrast * 0.1),
    borderHeavy: colorAlpha(ink, 0.12 + contrast * 0.06),
    borderLight: colorAlpha(ink, 0.03 + contrast * 0.02),
    buttonPrimaryBackground: rgbString(primaryButton),
    buttonPrimaryBackgroundActive: colorAlpha(ink, 0.07 + contrast * 0.05),
    buttonPrimaryBackgroundHover: colorAlpha(ink, 0.04 + contrast * 0.03),
    buttonPrimaryBackgroundInactive: colorAlpha(ink, 0.02 + contrast * 0.02),
    buttonSecondaryBackground: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonSecondaryBackgroundActive: colorAlpha(ink, 0.09 + contrast * 0.05),
    buttonSecondaryBackgroundHover: colorAlpha(ink, 0.06 + contrast * 0.03),
    buttonSecondaryBackgroundInactive: colorAlpha(ink, 0.02 + contrast * 0.03),
    buttonTertiaryBackground: colorAlpha(ink, 0.02 + contrast * 0.015),
    buttonTertiaryBackgroundActive: colorAlpha(ink, 0.07 + contrast * 0.05),
    buttonTertiaryBackgroundHover: colorAlpha(ink, 0.05 + contrast * 0.03),
    controlBackground: colorAlpha(control, 0.96),
    controlBackgroundOpaque: rgbString(control),
    elevatedPrimary: colorAlpha(elevatedPrimary, 0.96),
    elevatedPrimaryOpaque: rgbString(elevatedPrimary),
    elevatedSecondary: colorAlpha(ink, 0.02 + contrast * 0.02),
    elevatedSecondaryOpaque: colorHex(mixRgb(surface, ink, 0.04 + contrast * 0.05)),
    iconAccent: rgbString(tunedAccent),
    iconPrimary: colorAlpha(ink, 0.82 + contrast * 0.14),
    iconSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    iconTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    simpleScrim: colorAlpha(ink, 0.08 + contrast * 0.04),
    textAccent: rgbString(tunedAccent),
    textButtonPrimary: rgbString(primaryButton),
    textButtonSecondary: colorHex(mixRgb(ink, surface, 0.7 + contrast * 0.1)),
    textButtonTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    textForeground: colorHex(ink),
    textForegroundSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    textForegroundTertiary: colorAlpha(ink, 0.42 + contrast * 0.13),
  };
}

function octopusContrast(value: number, theme: "light" | "dark"): number {
  const baseline = theme === "dark" ? 60 : 45;
  const normalized = value / 100 + ((value - baseline) / 60) * 0.7;
  if (value <= baseline) return normalized;
  const base = baseline / 100;
  return base + (normalized - base) * 2;
}

function rgbColor(value: string): RgbColor {
  const [red, green, blue] = hexRgb(value);
  return { red, green, blue };
}

function mixRgb(from: RgbColor, to: RgbColor, ratio: number): RgbColor {
  const clamped = Math.min(1, Math.max(0, ratio));
  return {
    red: Math.round(from.red + (to.red - from.red) * clamped),
    green: Math.round(from.green + (to.green - from.green) * clamped),
    blue: Math.round(from.blue + (to.blue - from.blue) * clamped),
  };
}

function colorHex(value: RgbColor): string {
  return `#${[value.red, value.green, value.blue].map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

function rgbString(value: RgbColor): string {
  return `rgb(${value.red}, ${value.green}, ${value.blue})`;
}

function colorAlpha(value: RgbColor, alpha: number): string {
  const clamped = Math.min(1, Math.max(0, alpha));
  const formatted = clamped.toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
  return `rgba(${value.red}, ${value.green}, ${value.blue}, ${formatted})`;
}

function relativeLuminance(color: RgbColor): number {
  return linearChannel(color.red) * 0.2126
    + linearChannel(color.green) * 0.7152
    + linearChannel(color.blue) * 0.0722;
}

function linearChannel(channel: number): number {
  const normalized = channel / 255;
  return normalized <= 0.04045
    ? normalized / 12.92
    : ((normalized + 0.055) / 1.055) ** 2.4;
}

const BLACK_HEX = "#000000";
const WHITE_HEX = "#ffffff";
const BLACK_RGB: RgbColor = { red: 0, green: 0, blue: 0 };
const WHITE_RGB: RgbColor = { red: 255, green: 255, blue: 255 };
const DARK_ACCENT_MIX_STEPS = [0.25, 0.4, 0.55, 0.75, 1];
const LIGHT_ACCENT_MIX_STEPS = [0.5, 0.65, 0.75, 0.9, 1];
const PALETTE_SCALE_STEPS = [25, 50, 75, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950, 1000] as const;
const SEMANTIC_TEXT_BASE_STEPS: Record<"info" | "danger" | "success" | "warning" | "caution" | "discovery", readonly [number, number]> = {
  info: [500, 200],
  danger: [700, 500],
  success: [700, 400],
  warning: [700, 500],
  caution: [700, 500],
  discovery: [700, 500],
};
const SEMANTIC_PALETTE_ENTRIES: ReadonlyArray<readonly [keyof typeof SEMANTIC_TEXT_BASE_STEPS, keyof SemanticPalette]> = [
  ["info", "blue"],
  ["danger", "red"],
  ["success", "green"],
  ["warning", "orange"],
  ["caution", "yellow"],
  ["discovery", "purple"],
];
const CUSTOM_SURFACE_THRESHOLD: Record<"light" | "dark", RgbColor> = {
  light: { red: 239, green: 241, blue: 199 },
  dark: { red: 52, green: 53, blue: 65 },
};

const PERSONALIZED_ACCENT_CONTROL_BINDINGS: Record<string, string> = {
  "--color-border-accent-outline-hover": "var(--color-text-accent)",
  "--color-text-accent-ghost": "var(--color-text-accent)",
  "--color-text-accent-ghost-hover": "var(--color-text-accent-hover)",
  "--color-text-accent-outline": "var(--color-text-accent)",
  "--color-text-accent-outline-hover": "var(--color-text-accent-hover)",
  "--color-background-accent-ghost-hover": "var(--color-background-accent-soft-alpha-hover)",
  "--color-background-accent-ghost-active": "var(--color-background-accent-soft-alpha-active)",
  "--color-background-accent-outline-hover": "var(--color-background-accent-soft-alpha-hover)",
  "--color-background-accent-outline-active": "var(--color-background-accent-soft-alpha-active)",
  "--color-background-accent-soft-alt": "var(--color-background-accent-soft-alpha)",
  "--color-border-accent-soft-alt": "var(--color-border-accent-outline)",
  "--color-text-accent-soft-alt": "var(--color-text-accent-soft)",
  "--checkbox-checked-background-color": "var(--color-background-accent-solid)",
  "--checkbox-checked-mark-color": "var(--color-text-accent-solid)",
  "--switch-track-color-checked": "var(--color-background-accent-solid)",
  "--switch-thumb-color-checked": "var(--color-text-accent-solid)",
  "--switch-track-color-checked-disabled": "var(--switch-track-color-checked)",
  "--color-button-accent": "var(--color-background-accent-solid)",
  "--color-button-accent-hover": "var(--color-background-accent-solid-hover)",
  "--color-button-accent-active": "var(--color-background-accent-solid-active)",
  "--color-token-on-accent": "var(--color-text-accent-solid)",
};

function apcaLuminance(value: RgbColor): number {
  const y = (value.red / 255) ** 2.4 * 0.2126729
    + (value.green / 255) ** 2.4 * 0.7151522
    + (value.blue / 255) ** 2.4 * 0.072175;
  return y >= 0.022 ? y : y + (0.022 - y) ** 1.414;
}

function apcaContrast(text: RgbColor, background: RgbColor): number {
  const txtY = apcaLuminance(text);
  const bgY = apcaLuminance(background);
  if (Math.abs(bgY - txtY) < 5e-4) return 0;
  const sapc = bgY > txtY
    ? (bgY ** 0.56 - txtY ** 0.57) * 1.14
    : (bgY ** 0.65 - txtY ** 0.62) * 1.14;
  if (Math.abs(sapc) < 0.1) return 0;
  return (sapc > 0 ? sapc - 0.027 : sapc + 0.027) * 100;
}

function mixRgbLinear(from: RgbColor, to: RgbColor, ratio: number): RgbColor {
  return {
    red: from.red + (to.red - from.red) * ratio,
    green: from.green + (to.green - from.green) * ratio,
    blue: from.blue + (to.blue - from.blue) * ratio,
  };
}

function roundHex(value: RgbColor): string {
  return `#${[value.red, value.green, value.blue].map((channel) => Math.round(channel).toString(16).padStart(2, "0")).join("")}`;
}

function mixHexLinear(from: string, to: string, ratio: number): string {
  return roundHex(mixRgbLinear(rgbColor(from), rgbColor(to), ratio));
}

function wcagContrast(foreground: string, background: string | RgbColor): number {
  const lumA = relativeLuminance(rgbColor(foreground)) + 0.05;
  const lumB = relativeLuminance(typeof background === "string" ? rgbColor(background) : background) + 0.05;
  return Math.max(lumA, lumB) / Math.min(lumA, lumB);
}

function apcaAbs(foreground: string, background: string | RgbColor): number {
  return Math.abs(apcaContrast(rgbColor(foreground), typeof background === "string" ? rgbColor(background) : background));
}

function pickSolidText(background: string): string {
  return apcaAbs(BLACK_HEX, background) > apcaAbs(WHITE_HEX, background) ? BLACK_HEX : WHITE_HEX;
}

function ensureOnAccentText(background: string): string {
  const preferred = pickSolidText(background);
  return wcagContrast(preferred, background) >= 4.5 ? preferred : preferred === BLACK_HEX ? WHITE_HEX : BLACK_HEX;
}

function solveSolidAccent(accentHex: string): Record<string, string> {
  let onSolid = pickSolidText(accentHex);
  const shiftToward = relativeLuminance(rgbColor(onSolid)) <= relativeLuminance(rgbColor(accentHex)) ? WHITE_HEX : BLACK_HEX;
  let solid = accentHex;
  for (let step = 1; wcagContrast(onSolid, solid) < 4.5 && step <= 100; step++) {
    solid = mixHexLinear(accentHex, shiftToward, step / 100);
  }
  const nearTarget = apcaAbs(solid, shiftToward) < 3;
  if (nearTarget && wcagContrast(onSolid, mixHexLinear(solid, onSolid, 0.24)) < 4.5) {
    onSolid = ensureOnAccentText(solid);
  }
  const stateTarget = nearTarget ? onSolid : shiftToward;
  const previewHover = mixHexLinear(accentHex, stateTarget, 0.12);
  const baseForStates = previewHover !== solid && wcagContrast(onSolid, previewHover) >= 4.5 ? accentHex : solid;
  return {
    "--color-background-accent-solid": solid,
    "--color-background-accent-solid-hover": mixHexLinear(baseForStates, stateTarget, 0.12),
    "--color-background-accent-solid-active": mixHexLinear(baseForStates, stateTarget, 0.24),
    "--color-text-accent-solid": onSolid,
  };
}

function solveAccentTextAndRing(
  { accent, surface, foreground }: { accent: string; surface: string; foreground: string },
  backgrounds: ReadonlyArray<string | RgbColor>,
): Record<string, string> {
  const minWcag = (candidate: string) => Math.min(...backgrounds.map((bg) => wcagContrast(candidate, bg)));
  const minApca = (candidate: string) => Math.min(...backgrounds.map((bg) => apcaAbs(candidate, bg)));
  const darkSteps = DARK_ACCENT_MIX_STEPS.map((ratio) => mixHexLinear(accent, BLACK_HEX, ratio));
  const lightSteps = LIGHT_ACCENT_MIX_STEPS.map((ratio) => mixHexLinear(accent, WHITE_HEX, ratio));
  const ordered = [
    accent,
    ...(minApca(BLACK_HEX) > minApca(WHITE_HEX) ? [...darkSteps, ...lightSteps] : [...lightSteps, ...darkSteps]),
    foreground,
  ];
  const candidates = ordered.filter((candidate) => minWcag(candidate) >= 4.5);
  const textAccent = candidates.find((candidate) => minApca(candidate) >= 60)
    ?? (candidates.length > 0
      ? candidates.reduce((best, candidate) => (minApca(candidate) > minApca(best) ? candidate : best))
      : foreground);
  const textAccentIndex = candidates.indexOf(textAccent);
  const textAccentHover = (textAccentIndex >= 0
    ? candidates.slice(textAccentIndex + 1).find((candidate) => candidate !== textAccent)
    : undefined)
    ?? candidates.find((candidate) => candidate !== textAccent)
    ?? textAccent;
  return {
    "--color-text-accent": textAccent,
    "--color-text-accent-hover": textAccentHover,
    "--color-ring-accent": wcagContrast(accent, surface) >= 3 ? accent : textAccent,
  };
}

function solvePersonalizedAccentTokens(
  params: { accent: string; surface: string; foreground: string },
): Record<string, string> {
  const { accent, surface } = params;
  const surfaceRgb = rgbColor(surface);
  const blendOnSurface = (hex: string, alpha: number) => mixRgbLinear(surfaceRgb, rgbColor(hex), alpha);
  const softBackgrounds = (hex: string): Array<string | RgbColor> => [
    surface,
    ...[0.1, 0.17, 0.25].map((alpha) => blendOnSurface(hex, alpha)),
  ];
  const tint = [accent, BLACK_HEX].find((candidate) => (
    wcagContrast(candidate, surface) >= 1.1
    && [BLACK_HEX, WHITE_HEX].some((fg) => softBackgrounds(candidate).every((bg) => wcagContrast(fg, bg) >= 4.5))
  )) ?? WHITE_HEX;
  const textTokens = solveAccentTextAndRing(params, softBackgrounds(tint));
  const textAccent = textTokens["--color-text-accent"];
  const outlineBorder = wcagContrast(surface, blendOnSurface(accent, 0.45)) >= 3
    ? `color-mix(in srgb, ${accent} 45%, transparent)`
    : textAccent;
  return {
    "--color-accent": accent,
    "--color-text-on-accent": ensureOnAccentText(accent),
    ...solveSolidAccent(accent),
    ...PERSONALIZED_ACCENT_CONTROL_BINDINGS,
    ...textTokens,
    "--color-text-accent-soft": textAccent,
    "--color-border-accent-outline": outlineBorder,
    "--color-background-accent-soft-alpha": `color-mix(in srgb, ${tint} 10%, transparent)`,
    "--color-background-accent-soft-alpha-hover": `color-mix(in srgb, ${tint} 17%, transparent)`,
    "--color-background-accent-soft-alpha-active": `color-mix(in srgb, ${tint} 25%, transparent)`,
  };
}

function paletteShadeRgb(base: RgbColor, step: number): RgbColor {
  return step < 400
    ? mixRgbLinear(base, WHITE_RGB, (400 - step) / 400)
    : mixRgbLinear(base, BLACK_RGB, (step - 400) / 625);
}

function solveCustomSurfaceSemanticText(
  appearance: "light" | "dark",
  surfaceHex: string,
  foregroundHex: string,
  palette: SemanticPalette,
): Record<string, string> {
  const surfaceRgb = rgbColor(surfaceHex);
  const threshold = CUSTOM_SURFACE_THRESHOLD[appearance];
  const withinDefaultSurface = appearance === "light"
    ? surfaceRgb.red >= threshold.red && surfaceRgb.green >= threshold.green && surfaceRgb.blue >= threshold.blue
    : surfaceRgb.red <= threshold.red && surfaceRgb.green <= threshold.green && surfaceRgb.blue <= threshold.blue;
  if (withinDefaultSurface) return {};

  const inkRgb = rgbColor(foregroundHex);
  const dark = appearance === "dark";
  const result: Record<string, string> = {};
  for (const [semantic, hue] of SEMANTIC_PALETTE_ENTRIES) {
    const baseHex = palette[hue];
    if (!baseHex) continue;
    const baseRgb = rgbColor(baseHex);
    const startStep = SEMANTIC_TEXT_BASE_STEPS[semantic][dark ? 1 : 0];
    const backgrounds = [surfaceRgb, mixRgbLinear(surfaceRgb, baseRgb, 0.25)];
    const steps = dark
      ? PALETTE_SCALE_STEPS.filter((step) => step <= startStep).reverse()
      : PALETTE_SCALE_STEPS.filter((step) => step >= startStep);
    let chosen: number | "ink" | "black" | "white" = startStep;
    let bestScore = -1;
    for (const candidate of [...steps, "ink" as const, "black" as const, "white" as const]) {
      const candidateRgb = typeof candidate === "number"
        ? paletteShadeRgb(baseRgb, candidate)
        : candidate === "ink" ? inkRgb : candidate === "black" ? BLACK_RGB : WHITE_RGB;
      const score = Math.min(...backgrounds.map((bg) => Math.abs(apcaContrast(candidateRgb, bg))));
      if (score >= 60) {
        chosen = candidate;
        break;
      }
      if (score > bestScore) {
        chosen = candidate;
        bestScore = score;
      }
    }
    result[`--color-text-${semantic}`] = typeof chosen === "number"
      ? `var(--${hue}-${chosen})`
      : chosen === "ink" ? roundHex(inkRgb) : `var(--${chosen})`;
  }
  return result;
}

function userMessageBackgroundLuminance(background: string, surfaceHex: string, defaultAlpha: number): number {
  const rgbaMatch = /^rgb\((\d+) (\d+) (\d+) \/ (\d+)%\)$/.exec(background.trim());
  const source = rgbaMatch
    ? { red: Number(rgbaMatch[1]), green: Number(rgbaMatch[2]), blue: Number(rgbaMatch[3]) }
    : rgbColor(background);
  const alpha = Math.round((rgbaMatch ? Number(rgbaMatch[4]) / 100 : defaultAlpha) * 255) / 255;
  if (alpha >= 1) return relativeLuminance(source);
  const base = rgbColor(surfaceHex);
  return relativeLuminance({
    red: Math.round(base.red + alpha * (source.red - base.red)),
    green: Math.round(base.green + alpha * (source.green - base.green)),
    blue: Math.round(base.blue + alpha * (source.blue - base.blue)),
  });
}

export function getAppearance(): AppearanceSettings {
  return current;
}

export function shouldReduceMotion(settings: AppearanceSettings = current): boolean {
  return settings.motionMode === "reduce"
    || (settings.motionMode === "system" && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
}

export function setAppearance(next: Partial<AppearanceSettings>): AppearanceSettings {
  current = normalize({
    ...current,
    ...next,
    ...(Object.hasOwn(next, "lightAccent") && !Object.hasOwn(next, "lightAccentSource") ? { lightAccentSource: "custom" } : {}),
    ...(Object.hasOwn(next, "darkAccent") && !Object.hasOwn(next, "darkAccentSource") ? { darkAccentSource: "custom" } : {}),
  });
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
  } catch {
    void 0;
  }
  applyAppearance(current);
  for (const listener of [...listeners]) listener();
  return current;
}

export function resetAppearance(): AppearanceSettings {
  return setAppearance({ ...defaultAppearance });
}

export function useAppearance(): AppearanceSettings {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getAppearance,
    () => defaultAppearance,
  );
}

export function useShouldReduceMotion(): boolean {
  const settings = useAppearance();
  const systemReduced = useSyncExternalStore(
    subscribeToReducedMotion,
    systemPrefersReducedMotion,
    () => false,
  );
  return settings.motionMode === "reduce"
    || (settings.motionMode === "system" && systemReduced);
}

function normalize(value: AppearanceSettings): AppearanceSettings {
  return {
    ...value,
    themeMode: choice(value.themeMode, ["system", "light", "dark"], defaultAppearance.themeMode),
    lightAccent: color(value.lightAccent, defaultAppearance.lightAccent),
    lightAccentSource: choice(value.lightAccentSource, ["codex", "custom"], "custom"),
    lightBackground: color(value.lightBackground, defaultAppearance.lightBackground),
    lightForeground: color(value.lightForeground, defaultAppearance.lightForeground),
    lightDiffAdded: color(value.lightDiffAdded, defaultAppearance.lightDiffAdded),
    lightDiffRemoved: color(value.lightDiffRemoved, defaultAppearance.lightDiffRemoved),
    lightSkill: color(value.lightSkill, defaultAppearance.lightSkill),
    lightContrast: numberValue(value.lightContrast, defaultAppearance.lightContrast, 0, 100),
    lightUiFont: stringValue(value.lightUiFont),
    lightCodeFont: stringValue(value.lightCodeFont),
    darkAccent: color(value.darkAccent, defaultAppearance.darkAccent),
    darkAccentSource: choice(value.darkAccentSource, ["codex", "custom"], "custom"),
    darkBackground: color(value.darkBackground, defaultAppearance.darkBackground),
    darkForeground: color(value.darkForeground, defaultAppearance.darkForeground),
    darkDiffAdded: color(value.darkDiffAdded, defaultAppearance.darkDiffAdded),
    darkDiffRemoved: color(value.darkDiffRemoved, defaultAppearance.darkDiffRemoved),
    darkSkill: color(value.darkSkill, defaultAppearance.darkSkill),
    darkContrast: numberValue(value.darkContrast, defaultAppearance.darkContrast, 0, 100),
    darkUiFont: stringValue(value.darkUiFont),
    darkCodeFont: stringValue(value.darkCodeFont),
    uiFontSize: numberValue(value.uiFontSize, defaultAppearance.uiFontSize, 11, 20),
    codeFontSize: numberValue(value.codeFontSize, defaultAppearance.codeFontSize, 10, 20),
    motionMode: choice(value.motionMode, ["system", "reduce", "full"], defaultAppearance.motionMode),
    density: choice(value.density, ["compact", "default", "comfortable"], defaultAppearance.density),
    chatTextSize: choice(value.chatTextSize, ["small", "default", "large"], defaultAppearance.chatTextSize),
    cornerSoftness: choice(value.cornerSoftness, ["sharp", "default", "round"], defaultAppearance.cornerSoftness),
    octopuses: booleanValue(value.octopuses, defaultAppearance.octopuses),
  };
}

function installObservers() {
  if (observersInstalled || typeof window === "undefined") return;
  observersInstalled = true;
  for (const query of ["(prefers-color-scheme: dark)", "(prefers-reduced-motion: reduce)"]) {
    window.matchMedia(query).addEventListener("change", () => applyAppearance(current));
  }
}

function subscribeToReducedMotion(listener: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return () => undefined;
  const media = window.matchMedia("(prefers-reduced-motion: reduce)");
  media.addEventListener("change", listener);
  return () => media.removeEventListener("change", listener);
}

function systemPrefersReducedMotion(): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode !== "system") return mode;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function choice<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return allowed.includes(value as T) ? value as T : fallback;
}

function booleanValue(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function numberValue(value: unknown, fallback: number, min: number, max: number): number {
  const number = Number(value);
  return Number.isFinite(number) ? Math.min(max, Math.max(min, Math.round(number))) : fallback;
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function color(value: unknown, fallback: string): string {
  const text = stringValue(value).toUpperCase();
  return /^#[0-9A-F]{6}$/.test(text) ? text : fallback;
}

function hexRgb(value: string): [number, number, number] {
  const normalized = color(value, "#000000").slice(1);
  return [
    Number.parseInt(normalized.slice(0, 2), 16),
    Number.parseInt(normalized.slice(2, 4), 16),
    Number.parseInt(normalized.slice(4, 6), 16),
  ];
}

function mixHex(background: string, foreground: string, foregroundRatio: number): string {
  const from = hexRgb(background);
  const to = hexRgb(foreground);
  const ratio = Math.min(1, Math.max(0, foregroundRatio));
  const mixed = from.map((channel, index) => Math.round(channel + (to[index] - channel) * ratio));
  return `#${mixed.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

function rgba([red, green, blue]: [number, number, number], alpha: number): string {
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}

function setScaledTokens(root: HTMLElement, tokens: Record<string, number>, scale: number): void {
  for (const [name, value] of Object.entries(tokens)) {
    root.style.setProperty(name, scaledPx(value, scale));
  }
}

function scaledPx(value: number, scale: number): string {
  return `${Math.round(value * scale * 10) / 10}px`;
}
