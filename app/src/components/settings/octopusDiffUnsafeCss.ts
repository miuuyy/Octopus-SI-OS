export const octopusDiffUnsafeCss = `@layer base, theme, rendered, unsafe;
@layer unsafe {
  [data-diffs-header],
  :is([data-diff], [data-file]) {
    --codex-diffs-surface: var(--color-surface);
    --codex-diffs-context-surface: color-mix(
      in srgb,
      var(--codex-diffs-surface) 94%,
      var(--color-surface)
    );
    --codex-diffs-separator-surface: color-mix(
      in srgb,
      var(--codex-diffs-surface) 94%,
      var(--color-text-info)
    );
    --codex-diffs-hover-surface: color-mix(
      in srgb,
      var(--codex-diffs-surface) 92%,
      var(--color-surface)
    );
    --codex-diffs-header-surface: var(--codex-diffs-surface);
    --codex-diffs-context-number: color-mix(
      in lab,
      var(--codex-diffs-surface) 98.5%,
      var(--diffs-mixer)
    );
    --codex-diffs-addition-number: light-dark(
      color-mix(in lab, var(--diffs-light-bg, #fff) 91%, var(--diffs-addition-base)),
      color-mix(in lab, var(--diffs-dark-bg, #000) 85%, var(--diffs-addition-base))
    );
    --codex-diffs-deletion-number: light-dark(
      color-mix(in lab, var(--diffs-light-bg, #fff) 91%, var(--diffs-deletion-base)),
      color-mix(in lab, var(--diffs-dark-bg, #000) 85%, var(--diffs-deletion-base))
    );
    --codex-diffs-addition-hover: light-dark(
      color-mix(
        in lab,
        var(--diffs-light-bg, #fff) 80%,
        var(--diffs-addition-base)
      ),
      color-mix(
        in lab,
        var(--diffs-dark-bg, #000) 70%,
        var(--diffs-addition-base)
      )
    );
    --codex-diffs-deletion-hover: light-dark(
      color-mix(
        in lab,
        var(--diffs-light-bg, #fff) 80%,
        var(--diffs-deletion-base)
      ),
      color-mix(
        in lab,
        var(--diffs-dark-bg, #000) 75%,
        var(--diffs-deletion-base)
      )
    );
    --octopus-diffs-surface: var(--codex-diffs-surface);
    --octopus-diffs-context-surface: var(--codex-diffs-context-surface);
    --octopus-diffs-separator-surface: var(--codex-diffs-separator-surface);
    --octopus-diffs-hover-surface: var(--codex-diffs-hover-surface);
    --octopus-diffs-header-surface: var(--codex-diffs-header-surface);
    --octopus-diffs-context-number: var(--codex-diffs-context-number);
    --octopus-diffs-addition-number: var(--codex-diffs-addition-number);
    --octopus-diffs-deletion-number: var(--codex-diffs-deletion-number);
    --octopus-diffs-addition-hover: var(--codex-diffs-addition-hover);
    --octopus-diffs-deletion-hover: var(--codex-diffs-deletion-hover);
    /* Keep unhighlighted lines readable while syntax tokens are pending. */
    --diffs-dark: var(--color-text);
    --diffs-light: var(--color-text);
    --diffs-bg-context-override: var(--codex-diffs-context-surface);
    --diffs-bg-separator-override: var(--codex-diffs-separator-surface);
    --diffs-bg-hover-override: var(--codex-diffs-hover-surface);
    --diffs-bg: var(--codex-diffs-surface) !important;
    background-color: var(--codex-diffs-surface) !important;
  }

  :is([data-diff], [data-file]) [data-utility-button] {
    background-color: var(--color-background-primary-solid);
    color: var(--color-text-primary-solid);
    border: none;
    border-radius: 4px;
    margin-right: 0;
  }

  :is([data-diff], [data-file]) [data-utility-button]:hover {
    background-color: var(--color-background-primary-solid-hover);
  }

  :is([data-diff], [data-file]) [data-selected-line][data-line-annotation] {
    background-color: var(--diffs-bg);
  }

  [data-placeholder][data-loading] {
    --codex-diffs-placeholder-base: color-mix(in srgb, var(--color-codex-description, var(--color-text-description)) 10%, transparent);
    --codex-diffs-placeholder-highlight: color-mix(in srgb, var(--color-codex-description, var(--color-text-description)) 16%, transparent);
    position: relative;
    overflow: hidden;
    background-image: linear-gradient(90deg, var(--codex-diffs-placeholder-base) 82%, transparent 82%);
    background-position: 12px 8px;
    background-size: calc(100% - 24px) 22px;
    background-repeat: repeat-y;
  }

  [data-placeholder][data-loading]::before {
    content: "";
    position: absolute;
    inset: 8px 12px;
    background: linear-gradient(90deg, #0000, var(--codex-diffs-placeholder-highlight), #0000);
    translate: -100% 0;
    mask-image: repeating-linear-gradient(180deg, #000 0 14px, transparent 14px 22px);
    animation: octopus-diffs-placeholder-shimmer 3s linear infinite;
  }

  @keyframes octopus-diffs-placeholder-shimmer {
    to { translate: 100% 0; }
  }

  @media (prefers-reduced-motion: reduce) {
    [data-placeholder][data-loading]::before { animation: none; }
  }

  :is([data-diff], [data-file]) [data-code] {
    scrollbar-gutter: auto;
  }

  :is([data-diff], [data-file]) [data-line-type='change-addition']:is([data-line], [data-no-newline]) {
    --diffs-computed-diff-line-bg: var(--diffs-bg-addition);
  }

  :is([data-diff], [data-file]) [data-line-type='change-addition']:is([data-column-number], [data-gutter-buffer]) {
    --diffs-computed-diff-line-bg: var(--codex-diffs-addition-number);
  }

  :is([data-diff], [data-file]) [data-line-type='change-deletion']:is([data-line], [data-no-newline]) {
    --diffs-computed-diff-line-bg: var(--diffs-bg-deletion);
  }

  :is([data-diff], [data-file]) [data-line-type='change-deletion']:is([data-column-number], [data-gutter-buffer]) {
    --diffs-computed-diff-line-bg: var(--codex-diffs-deletion-number);
  }

  :is([data-diff], [data-file]) [data-line-type='change-addition'][data-hovered]:not([data-selected-line]) {
    --diffs-computed-hovered-line-bg: var(--codex-diffs-addition-hover);
  }

  :is([data-diff], [data-file]) [data-line-type='change-deletion'][data-hovered]:not([data-selected-line]) {
    --diffs-computed-hovered-line-bg: var(--codex-diffs-deletion-hover);
  }

  :is([data-diff], [data-file]) [data-line-type="change-addition"][data-column-number]
    + [data-line-type="change-addition"][data-column-number]::before {
    contain: none;
    top: -1px;
    height: calc(100% + 1px);
  }

  mark.octopus-thread-find-match {
    background-color: var(--vscode-charts-yellow);
    color: var(--gray-fixed-1000, #0d0d0d);
    border-radius: var(--radius-2xs);
    padding: 0;
    margin: 0;
    border: 0;
    font: inherit;
    line-height: inherit;
    letter-spacing: inherit;
    word-spacing: inherit;
    vertical-align: baseline;
  }

  mark.octopus-thread-find-active {
    background-color: var(--vscode-charts-orange);
    color: var(--gray-fixed-1000, #0d0d0d);
  }

  :host(.composer-diff-simple-line) [data-separator]:empty {
    background-color: transparent;
  }

  :host(.composer-diff-simple-line) [data-separator]:empty::after {
    content: "";
    grid-column: 2 / 3;
    align-self: center;
    margin-inline: 1ch;
    border-top: 1px solid color-mix(in srgb, var(--diffs-fg) 18%, transparent);
  }
}`;

export const octopusDiffRawUnsafeCss = octopusDiffUnsafeCss
  .replace(/^@layer base, theme, rendered, unsafe;\n@layer unsafe \{\n/, "")
  .replace(/\n\}\s*$/, "");
