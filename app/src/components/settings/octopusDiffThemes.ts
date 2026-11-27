import { registerCustomTheme, type ThemeRegistration } from "@pierre/diffs";
import type { CodeThemeId } from "../../appearance";

export type OctopusDiffThemeKind = "light" | "dark";

type ThemeModule = { default: unknown };
type ThemeLoader = () => Promise<ThemeModule>;
type ThemeDescriptor = Readonly<{ name: string; load: ThemeLoader }>;

const lightThemes = {
  absolutely: theme("light", "absolutely", () => import("./code-themes/light-absolutely.json")),
  catppuccin: theme("light", "catppuccin", () => import("@shikijs/themes/catppuccin-latte")),
  codex: theme("light", "codex", () => import("./code-themes/light-codex")),
  everforest: theme("light", "everforest", () => import("@shikijs/themes/everforest-light")),
  github: theme("light", "github", () => import("@shikijs/themes/github-light-default")),
  gruvbox: theme("light", "gruvbox", () => import("@shikijs/themes/gruvbox-light-medium")),
  linear: theme("light", "linear", () => import("./code-themes/light-linear.json")),
  notion: theme("light", "notion", () => import("./code-themes/light-notion.json")),
  one: theme("light", "one", () => import("@shikijs/themes/one-light")),
  proof: theme("light", "proof", () => import("./code-themes/light-proof.json")),
  raycast: theme("light", "raycast", () => import("./code-themes/light-raycast")),
  "rose-pine": theme("light", "rose-pine", () => import("@shikijs/themes/rose-pine-dawn")),
  solarized: theme("light", "solarized", () => import("@shikijs/themes/solarized-light")),
  vercel: theme("light", "vercel", () => import("./code-themes/light-vercel.json")),
  "vscode-plus": theme("light", "vscode-plus", () => import("@shikijs/themes/light-plus")),
  xcode: theme("light", "xcode", () => import("./code-themes/light-xcode.json")),
} satisfies Partial<Record<CodeThemeId, ThemeDescriptor>>;

const darkThemes = {
  absolutely: theme("dark", "absolutely", () => import("./code-themes/dark-absolutely.json")),
  ayu: theme("dark", "ayu", () => import("@shikijs/themes/ayu-dark")),
  catppuccin: theme("dark", "catppuccin", () => import("@shikijs/themes/catppuccin-mocha")),
  codex: theme("dark", "codex", () => import("./code-themes/dark-codex")),
  dracula: theme("dark", "dracula", () => import("@shikijs/themes/dracula")),
  everforest: theme("dark", "everforest", () => import("@shikijs/themes/everforest-dark")),
  github: theme("dark", "github", () => import("@shikijs/themes/github-dark-default")),
  gruvbox: theme("dark", "gruvbox", () => import("@shikijs/themes/gruvbox-dark-medium")),
  linear: theme("dark", "linear", () => import("./code-themes/dark-linear.json")),
  lobster: theme("dark", "lobster", () => import("./code-themes/dark-lobster.json")),
  material: theme("dark", "material", () => import("@shikijs/themes/material-theme-darker")),
  matrix: theme("dark", "matrix", () => import("./code-themes/dark-matrix.json")),
  monokai: theme("dark", "monokai", () => import("@shikijs/themes/monokai")),
  "night-owl": theme("dark", "night-owl", () => import("@shikijs/themes/night-owl")),
  nord: theme("dark", "nord", () => import("@shikijs/themes/nord")),
  notion: theme("dark", "notion", () => import("./code-themes/dark-notion.json")),
  og: theme("dark", "og", () => import("./code-themes/dark-og.json")),
  one: theme("dark", "one", () => import("@shikijs/themes/one-dark-pro")),
  oscurange: theme("dark", "oscurange", () => import("./code-themes/dark-oscurange.json")),
  raycast: theme("dark", "raycast", () => import("./code-themes/dark-raycast")),
  "rose-pine": theme("dark", "rose-pine", () => import("@shikijs/themes/rose-pine-moon")),
  sentry: theme("dark", "sentry", () => import("./code-themes/dark-sentry.json")),
  solarized: theme("dark", "solarized", () => import("@shikijs/themes/solarized-dark")),
  temple: theme("dark", "temple", () => import("./code-themes/dark-temple.json")),
  "tokyo-night": theme("dark", "tokyo-night", () => import("@shikijs/themes/tokyo-night")),
  vercel: theme("dark", "vercel", () => import("./code-themes/dark-vercel.json")),
  "vscode-plus": theme("dark", "vscode-plus", () => import("@shikijs/themes/dark-plus")),
  xcode: theme("dark", "xcode", () => import("./code-themes/dark-xcode.json")),
} satisfies Partial<Record<CodeThemeId, ThemeDescriptor>>;

const themesByKind = { light: lightThemes, dark: darkThemes };

export function octopusDiffThemeName(kind: OctopusDiffThemeKind, id: CodeThemeId): string {
  return octopusDiffTheme(kind, id).name;
}

export function registerOctopusDiffThemes(registerTheme: typeof registerCustomTheme = registerCustomTheme): void {
  for (const themes of Object.values(themesByKind)) {
    for (const descriptor of Object.values(themes)) {
      registerTheme(descriptor.name, async () => {
        const module = await descriptor.load();
        return { ...(module.default as ThemeRegistration), name: descriptor.name };
      });
    }
  }
}

function octopusDiffTheme(kind: OctopusDiffThemeKind, id: CodeThemeId): ThemeDescriptor {
  const descriptor = (themesByKind[kind] as Partial<Record<CodeThemeId, ThemeDescriptor>>)[id];
  if (!descriptor) throw new Error(`Code theme ${id} is unavailable for ${kind} mode.`);
  return descriptor;
}

function theme(kind: OctopusDiffThemeKind, id: CodeThemeId, load: ThemeLoader): ThemeDescriptor {
  return { name: `octopus-codex-${kind}-${id}`, load };
}
