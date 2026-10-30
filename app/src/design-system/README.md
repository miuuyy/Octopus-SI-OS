# Octopus App Design System

This folder is the first-party UI contract for the Electron/operator app.

Use these primitives for shared UI anatomy:

- `OcButton` for command buttons with optional icon and meta text.
- `OcIconButton` for icon-only commands; dialog headers use its `dialog-close` appearance.
- `OcSegmented` for two-or-more mutually exclusive view modes.
- `OcStatus` for compact state: a neutral outlined badge, or a soft colored pill
  for success, warning, danger and accent. The color is the state; it never
  carries a second dot.
- `OcPaneGroup` for main/detail pane layouts.
- `OcSurfaceHeader` for repeated surface headers.
- `OcKbd` for keyboard hints.

Feature CSS may position a primitive or set a local width. It must not redefine
shared button, segmented-control, status, pane, or keyboard anatomy. If a new
shared control is needed, add it here first and wire it through the design-system
guard.

## Color code — color is earned, never decorative

The base of every surface is ink: foreground mixes on the theme background.
A color appears ONLY when it carries a state the user must read, the same way
the sidebar does it (Running shimmer, failed red). If a dot, glyph, label, or
line has no state, it is an ink mix — there is no "brand blue" chrome and no
palette-for-the-sake-of-palette. Concretely:

- Success/positive state → `--oc-decor-positive`; failure/danger state →
  `--oc-decor-danger`; unread/new markers → `--oc-decor-accent`. These theme
  themselves (light: `#3ea169` / `#dd5a5a` / `#4a82ec`; dark: monochrome
  foreground mixes — dark stays radically black-and-white).
- Everything stateless in widgets — timeline dots, header glyphs, "accent"
  and "warning" tone text, inactive donut segments — renders as ink mixes of
  `--color-token-foreground` at stepped weights.
- `--oc-series-1..6` are reserved for analytics (Metrics charts), where the
  series identity IS the information. Monochrome ramp in dark; the pastel six
  in light: `#7aa7f0` `#57bd7e` `#f0c64a` `#f0926e` `#bda4f2` `#ee8f8f`.
- `--oc-tint-1..5-bg/-fg` — **tint pairs**: when a widget element does carry
  color, it is a PAIR — a soft wash container plus a deeper glyph/stroke of
  the same hue (tinted glyph chips in widget headers, the progress ring on its
  same-hue track). One hue per widget. Light: a single cool family —
  periwinkle / mint / sky / violet / slate — calibrated high-lightness,
  low-chroma so nothing pops out of the white; warm hues never enter widget
  chrome. Dark: monochrome mixes. Bare saturated dots on a plain card are
  exactly the "random colors" failure — color lives inside soft containers.

Progress donuts are single-hue rings (value arc on a same-hue tinted track),
never multi-color pies. **One card, one hue**: a widget carries at most one
hue total (its chip + its ring share it); legend dots and secondary marks are
neutral — only a true alarm (danger) may break the rule. Cards breathe:
42px headers, 14px+ side padding, 8px row gaps — cramped density reads as
HTML, not as an OS surface.

Depth comes from the canvas–card relationship, not from strokes: in light
theme the Home canvas is a smooth LINEAR wash (`#f3f3f7 → #eaedf5`, one
diagonal pass — radial spots read as blotches and are banned), and cards sit
on it whiter than the canvas, radius 16px, hairline inset instead of a
border, whisper-level diffuse shadow. Dark theme keeps its plain canvas and
existing card chrome.

Consumers never write per-theme overrides and never name raw hexes or
`--color-token-charts-*` tokens.

Exemptions: the Topology surface keeps its own colors in both themes.
Functional signals (live error states in controls and toasts, link
affordances in rendered markdown) keep the system tokens — the color code
governs decoration, not legibility.

The app shell is an Octopus projection surface, not a runtime primitive. Keep
runtime vocabulary in the architecture docs; keep visual UI contracts here.
