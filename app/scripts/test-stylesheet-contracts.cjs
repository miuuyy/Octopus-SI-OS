const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test, after } = require("node:test");
const { JSDOM } = require("jsdom");
const { stylesheetCascade, tailwindStylesheet, parseStylesheet } = require("./support/stylesheet-cascade.cjs");

const appRoot = path.resolve(__dirname, "..");
const dom = new JSDOM("<!doctype html><html><body></body></html>");
const { document } = dom.window;
after(() => dom.window.close());
const applicationSheets = ["src/foundation.css", "src/octopus-owned-components.css", "src/styles.css", "src/theme.css", "src/shell.css", "src/components/shell/paneTabs.css", "src/components/chat/chat.css", "src/components/chat/messageTooltip.css", "src/components/chat/queue.css", "src/components/voice/voice.css"];
const application = stylesheetCascade(applicationSheets);
const fixture = (markup) => { document.body.innerHTML = markup; return (selector) => document.body.querySelector(selector); };
const state = (element, name) => { for (let current = element; current; current = current.parentElement) current.setAttribute(`data-${name}`, ""); };

test("an activity row toggles from anywhere on its header while its file link stays its own target", () => {
  const $ = fixture(`<div class="octopus-agent-activity"><div class="octopus-agent-activity-header"><button class="octopus-agent-activity-toggle"></button>
    <span class="octopus-agent-activity-leading"><span class="octopus-agent-activity-summary">Read <span class="chat-tooltip-trigger is-activity-target" data-agent-activity-file-link="true">notes.md</span></span></span>
    <span class="octopus-agent-activity-chevron-wrap"><svg class="octopus-agent-activity-chevron"></svg></span></div></div>`);
  const toggle = $(".octopus-agent-activity-toggle");
  assert.equal(application.value(toggle, "position"), "absolute");
  assert.deepEqual(["top", "right", "bottom", "left"].map((side) => application.value(toggle, side)), ["0", "0", "0", "0"]);
  assert.notEqual(application.value($(".octopus-agent-activity-header"), "position") ?? "static", "static", "the toggle covers its own header");
  assert.equal(application.value($(".octopus-agent-activity-summary"), "pointer-events"), "none");
  assert.equal(application.value($(".octopus-agent-activity-chevron"), "pointer-events"), "none");
  assert.equal(application.value($("[data-agent-activity-file-link]"), "pointer-events"), "auto");
  const resting = application.value($(".octopus-agent-activity-summary"), "color");
  state($(".octopus-agent-activity-summary"), "hover");
  const rowHover = application.value($(".octopus-agent-activity-summary"), "color");
  assert.notEqual(rowHover, resting, "hovering the row highlights its summary");
  state($("[data-agent-activity-file-link]"), "hover");
  assert.equal(application.value($(".octopus-agent-activity-summary"), "color"), resting, "hovering the file link leaves the row unhighlighted");
});

test("an expanding activity body carries no block spacing and its detail is never height-capped", () => {
  const $ = fixture(`<div class="octopus-agent-activity"><div class="octopus-agent-activity-body"><div class="octopus-agent-activity-viewport"><div class="octopus-reasoning-detail"></div></div></div></div>`);
  assert.deepEqual(application.properties($(".octopus-agent-activity-body")).filter((property) => /padding|margin|gap|height/.test(property)), []);
  for (const selector of [".octopus-agent-activity-viewport", ".octopus-reasoning-detail"]) assert.equal(application.value($(selector), "max-height") ?? "none", "none", selector);
});

test("thread content and the floating summary clear the toolbar only in a standalone thread", () => {
  const thread = (embedded) => `<div class="chat-view${embedded ? " is-embedded" : ""}" style="--height-toolbar: 77px"><div class="chat-thread">
    <div class="task-summary-surface"><div class="task-summary-surface-inner"><div class="task-summary-motion"></div></div></div>
    <div class="octopus-chat-scroll thread-scroll-container"></div></div></div>`;
  for (const embedded of [false, true]) {
    const $ = fixture(thread(embedded));
    for (const [element, property] of [[$(".task-summary-surface"), "top"], [$(".octopus-chat-scroll"), "--thread-content-top-inset"]]) {
      const resolved = application.value(element, property);
      assert.ok(resolved, property);
      assert.equal(resolved.includes("77px"), !embedded, `${property} ${resolved}`);
    }
  }
});

test("the task summary surface passes clicks to the thread until it opens", () => {
  const $ = fixture(`<div class="task-summary-surface"><div class="task-summary-surface-inner"><div class="task-summary-motion"><button>Plan</button></div></div></div>`);
  assert.equal(application.value($(".task-summary-surface-inner"), "pointer-events"), "none");
  assert.equal(application.value($("button"), "pointer-events"), "none");
  $(".task-summary-surface").classList.add("is-open");
  assert.equal(application.value($("button"), "pointer-events"), "auto");
});

test("the thread's top fade shows only while the header marks content scrolled beneath it", () => {
  const $ = fixture(`<div class="chat-view"><div class="chat-thread-top-fade"></div></div>`);
  assert.equal(application.value($(".chat-thread-top-fade"), "opacity"), "0");
  $(".chat-thread-top-fade").setAttribute("data-app-shell-main-content-top-fade", "visible");
  assert.equal(application.value($(".chat-thread-top-fade"), "opacity"), "1");
});

test("a pane header drags the window while its tabs and actions stay clickable, and a tab strip replaces the titlebar spacer", () => {
  const $ = fixture(`<div class="workspace-surface-pane"><div class="workspace-surface-pane-header"><div class="pane-tabs"><div class="pane-tab" role="tab">Files</div></div>
    <div class="workspace-pane-header-actions"><button>Share</button></div></div><div class="workspace-surface-pane-content"><div class="surface-titlebar-spacer"></div></div></div>`);
  assert.equal(application.appRegion($(".workspace-surface-pane-header")), "drag");
  for (const selector of [".pane-tabs", "[role=tab]", ".workspace-pane-header-actions", ".workspace-pane-header-actions button"]) assert.equal(application.appRegion($(selector)), "no-drag", selector);
  assert.notEqual(application.value($(".surface-titlebar-spacer"), "display"), "none");
  $(".workspace-surface-pane").classList.add("has-tab-strip");
  assert.equal(application.value($(".surface-titlebar-spacer"), "display"), "none");
});

test("focusing the secondary pane collapses and hides the primary pane", () => {
  const $ = fixture(`<div class="workspace-pane-manager"><div class="workspace-pane-primary"><button>Chat</button></div><div class="workspace-pane-secondary"></div></div>`);
  const primary = $(".workspace-pane-primary");
  assert.equal(application.value(primary, "flex-grow"), "1");
  assert.notEqual(application.value(primary, "visibility"), "hidden");
  $(".workspace-pane-manager").classList.add("is-secondary-focused");
  assert.deepEqual([application.value(primary, "flex-grow"), Number.parseFloat(application.value(primary, "flex-basis"))], ["0", 0]);
  assert.equal(application.value(primary, "visibility"), "hidden");
  assert.equal(application.value($(".workspace-pane-primary button"), "pointer-events"), "none");
});

test("surface loading states cover their surface", () => {
  const $ = fixture(`<div class="voice-view"><div class="surface-content-loading-state"></div></div>`);
  const loading = $(".surface-content-loading-state");
  assert.equal(application.value(loading, "position"), "absolute");
  assert.deepEqual(["top", "right", "bottom", "left"].map((side) => application.value(loading, side)), ["0", "0", "0", "0"]);
});

test("the chat scroll container neither anchors positioned overlays nor scrolls smoothly", () => {
  for (const media of [{}, { "prefers-reduced-motion": "reduce" }]) {
    const sheets = stylesheetCascade(applicationSheets, { media });
    const $ = fixture(`<div class="chat-view"><div class="octopus-chat-scroll thread-scroll-container"><div data-thread-scroll-footer="true"></div></div></div>`);
    assert.equal(sheets.value($(".octopus-chat-scroll"), "position") ?? "static", "static");
    assert.notEqual(sheets.value($(".octopus-chat-scroll"), "scroll-behavior"), "smooth");
  }
});

test("the full-access warning utility paints with the design system's warning text color", async () => {
  const utilities = await tailwindStylesheet(path.join(appRoot, "src/owned-utilities.css"), ["text-token-warning", "text-token-text-tertiary"]);
  const sheets = stylesheetCascade(["src/foundation.css", utilities]);
  const $ = fixture(`<svg class="text-token-warning"></svg><svg class="text-token-text-tertiary"></svg>`);
  const warning = sheets.value($(".text-token-warning"), "color");
  assert.ok(warning && warning === sheets.value(document.documentElement, "--color-text-warning"), warning);
  assert.notEqual(warning, sheets.value($(".text-token-text-tertiary"), "color"));
});

test("the startup loader drags the window, shows the shipped logo, and shimmers until the app mounts unless motion is reduced", () => {
  const html = fs.readFileSync(path.join(appRoot, "index.html"), "utf8");
  const page = new JSDOM(html).window.document;
  const inline = parseStylesheet(path.join(appRoot, "index.html"), [...page.querySelectorAll("style")].map((style) => style.textContent).join("\n"));
  const loader = page.querySelector("#root > .startup-loader"), logo = loader.querySelector("img.startup-loader__base"), overlay = loader.querySelector(".startup-loader__overlay");
  assert.ok(logo && overlay);
  assert.ok(fs.statSync(path.join(appRoot, "public", new URL(logo.getAttribute("src"), "file:///").pathname)).isFile());
  const motion = stylesheetCascade([inline]);
  assert.equal(motion.appRegion(loader), "drag");
  const shimmer = motion.splitTop(motion.value(overlay, "animation"), " ");
  assert.ok(shimmer.some((token) => motion.keyframes.has(token)) && shimmer.includes("infinite"), shimmer.join(" "));
  const reduced = stylesheetCascade([inline], { media: { "prefers-reduced-motion": "reduce" } });
  assert.equal(reduced.value(overlay, "animation"), "none");
  assert.equal(reduced.value(loader.querySelector(".startup-loader__logo"), "opacity"), "1");
});

test("fullscreen disables all titlebar and pane drag regions while keeping every topbar control above the dock slot", () => {
  const $ = fixture(`<div class="app-shell is-fullscreen is-chrome-revealed"><header class="app-titlebar is-electron is-fullscreen is-chrome-revealed"><div class="app-titlebar-drag-region"></div><div class="app-titlebar-controls no-drag"><button class="app-titlebar-action no-drag"></button></div><div class="app-titlebar-dock-slot"><nav class="navigation-rail"></nav></div><div class="app-titlebar-pane-actions no-drag"><button class="app-titlebar-action no-drag"></button></div><div class="app-titlebar-system-tray no-drag"><button class="app-titlebar-action no-drag"></button></div></header><div class="workspace-surface-pane-header"></div></div>`);
  for (const selector of [".app-titlebar", ".app-titlebar-drag-region", ".app-titlebar-controls", ".app-titlebar-action", ".app-titlebar-pane-actions", ".app-titlebar-system-tray", ".workspace-surface-pane-header"]) {
    assert.equal(application.appRegion($(selector)), "no-drag", selector);
  }
  for (const selector of [".app-titlebar-controls", ".app-titlebar-action", ".app-titlebar-pane-actions", ".app-titlebar-system-tray"]) {
    assert.equal(application.value($(selector), "pointer-events"), "auto", selector);
  }
  assert.equal(application.value($(".app-titlebar-dock-slot"), "pointer-events"), "none");
  const dockZ = Number(application.value($(".app-titlebar-dock-slot"), "z-index"));
  const dragZ = Number(application.value($(".app-titlebar-drag-region"), "z-index"));
  for (const selector of [".app-titlebar-controls", ".app-titlebar-pane-actions", ".app-titlebar-system-tray"]) {
    const z = Number(application.value($(selector), "z-index"));
    assert.ok(z > dockZ && z > dragZ, `${selector} z-index (${z}) must exceed dock (${dockZ}) and drag (${dragZ})`);
  }
});

test("hidden chrome keeps active dark-theme pane tab contrast over the pane header surface", () => {
  document.documentElement.dataset.theme = "dark";
  try {
    const $ = fixture(`<div class="app-shell is-chrome-hidden"><div class="pane-tab is-active"><div class="pane-tab-background"></div></div></div>`);
    const resolved = application.value($(".pane-tab-background"), "background");
    assert.equal(resolved, application.value(document.documentElement, "--color-background-primary-ghost-hover"));
    assert.notEqual(resolved, application.value(document.documentElement, "--color-surface"));
  } finally {
    delete document.documentElement.dataset.theme;
  }
});

test("electron shell layers docked sidebar tint over window fill while dark composer and slash surfaces match Codex elevation", () => {
  const sheets = stylesheetCascade([...applicationSheets, "src/components/shell/shellTooltip.css"]);
  document.documentElement.dataset.theme = "dark";
  document.documentElement.dataset.octopusWindowType = "electron";
  document.documentElement.dataset.octopusOs = "darwin";
  try {
    const $ = fixture(`<div class="app-shell"><aside class="app-shell-left-panel"></aside><div class="composer-suggestion-surface"></div><div class="composer-surface-chrome composer-multiline-surface"></div><div class="chat-tooltip is-shell"></div></div>`);
    assert.equal(sheets.value($(".app-shell"), "background-color"), sheets.value(document.documentElement, "--app-shell-page-window-fill"));
    assert.match(sheets.value($(".app-shell"), "background-image") ?? "", /linear-gradient/);
    assert.equal(sheets.value($(".app-shell-left-panel"), "background"), "transparent");
    assert.equal(sheets.value($(".composer-suggestion-surface"), "background-color"), sheets.value(document.documentElement, "--color-background-composer-surface"));
    assert.equal(sheets.value($(".composer-surface-chrome"), "box-shadow"), sheets.value(document.documentElement, "--elevation-composer-dark"));
    assert.equal(sheets.value($(".chat-tooltip.is-shell"), "inline-size"), "max-content");
  } finally {
    delete document.documentElement.dataset.theme;
    delete document.documentElement.dataset.octopusWindowType;
    delete document.documentElement.dataset.octopusOs;
  }
});
