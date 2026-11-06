const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test, after, afterEach } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");
const React = require("react");
const { act } = React;
const dom = new JSDOM("<!doctype html><html><body></body></html>", { pretendToBeVisual: true, url: "http://localhost" });
for (const name of ["window", "document", "navigator", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Element", "Node", "Event", "KeyboardEvent", "MutationObserver", "CustomEvent"]) global[name] = name === "window" ? dom.window : dom.window[name];
global.IS_REACT_ACT_ENVIRONMENT = true;
global.getComputedStyle = dom.window.getComputedStyle;
global.requestAnimationFrame = dom.window.requestAnimationFrame.bind(dom.window);
global.cancelAnimationFrame = dom.window.cancelAnimationFrame.bind(dom.window);
global.ResizeObserver = class { observe() {} disconnect() {} };
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
HTMLElement.prototype.scrollIntoView = () => {};
HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, top: 100, left: 100, right: 400, bottom: 132, width: 300, height: 32 });
const { createRoot } = require("react-dom/client");
const roots = [];
const runtimeErrors = [];
window.addEventListener("error", (event) => runtimeErrors.push(event.error ?? event.message));
afterEach(async () => { await act(async () => { for (const root of roots.splice(0)) root.unmount(); }); document.body.replaceChildren(); assert.deepEqual(runtimeErrors.splice(0), []); });
after(() => dom.window.close());

const appRoot = path.resolve(__dirname, "..");
const compiled = build({
  stdin: { contents: 'import "./src/components/icon-glyphs/shell"; export * from "./src/components/task/TasksViewSwitcher"; export * from "./src/components/scheduled/ScheduledWorkspace"; export * from "./src/components/scheduled/ScheduledSidebar"; export * from "./src/components/scheduled/LiveScheduledView"; export { Sidebar } from "./src/components/Sidebar";', resolveDir: appRoot, loader: "tsx" },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", loader: { ".css": "empty", ".png": "dataurl", ".svg": "dataurl" },
  external: ["react", "react-dom", "react/jsx-runtime"],
  plugins: [{ name: "schedule-fixture", setup(builder) {
    builder.onResolve({ filter: /projectionClient$|projectionLive$/ }, ({ path: imported }) => ({ path: imported.endsWith("projectionClient") ? "projection" : "live", namespace: "fixture" }));
    builder.onLoad({ filter: /.*/, namespace: "fixture" }, ({ path: name }) => ({ contents: name === "live"
      ? "export function useProjectionGeneration() { return 0; } export function currentProjectionStructureGeneration() { return 0; }"
      : `export function canMutateScheduledInterfaces() { return globalThis.scheduleFixture.writable; }
         export function hasProjectionBridge() { return false; }
         export function formatProjectionError(error) { return error instanceof Error ? error.message : String(error); }
         export function projectionTargetLabel() { return ""; }
         export async function readProjection(name) { const f = globalThis.scheduleFixture; f.reads.push(name); if (f.error) throw Error(f.error); return { scheduled_interfaces: f.rows }; }
         export async function mutateScheduledInterface(request) { return globalThis.scheduleFixture.mutate(request); }`, loader: "js" }));
  } }],
}).then(({ outputFiles }) => { const module = { exports: {} }; Function("module", "exports", "require", outputFiles[0].text)(module, module.exports, require); return module.exports; });

function sidebarChrome(ui) {
  const projectWorkspace = { projects: [], rootEeId: "", homeRunId: "", homeInterfaceId: "", rootContext: null, rootAllowedChildTemplateIds: [], templates: [], status: "", phase: "ready", pending: false, error: "",
    updatePresentation() {}, async createProject() {}, async createAgentTemplate() { return { agentId: "", workerInterfaceId: "" }; } };
  return function Navigation() {
    const [surface, setSurface] = React.useState("scheduled");
    return React.createElement(ui.Sidebar, { accessProfile: "live", navigationSection: "tasks", activeSurfaceId: surface, activeChatRunId: "", chatRuns: [], chatRunsPhase: "ready", chatRunListError: "", async onRetryChatRuns() {}, homeRunId: "", runtimeHost: { home: "fixture" }, surfaceErrors: [], surfaces: [],
      agentPinnedChatIds: [], agentChatOrderIds: [], agentProjectFilterEeId: "", agentProjectWorkspace: projectWorkspace, agentExecutionContexts: [], agentDefinitions: [], agentProviderInterfaces: [], agentModelsLoading: false, agentModelError: "",
      async onRefreshAgentModels() {}, onSurfaceChange: setSurface, onSelectChat() {}, onNewChat() {}, onAgentPinnedChatIdsChange() {}, onAgentChatOrderIdsChange() {}, onAgentProjectFilterEeIdChange() {}, onSearch() {} });
  };
}

function schedule(index, status = "enabled") {
  return {
    interface_id: `world_task_${index}`, display_name: `Task ${index}`, status, valid: status !== "invalid", activation_digest: String(index % 10).repeat(64),
    schedule: { type: "cron", cron: "0 13 * * *" }, timezone: "UTC", fire_on_start: false, instructions: `Review task ${index}.`,
    targets: [{ run_id: `run_task_${index}`, chat_interface_id: `world_operator_chat_task_${index}`, title: `Task ${index}` }],
    next_run_at_ms: Date.now() + (index + 1) * 60_000, next_run_source: "driver_state", error: status === "invalid" ? "Invalid fixture configuration" : "",
  };
}
function fixture(rows) {
  const f = { rows, reads: [], writes: [], error: "", writable: true, async mutate(request) {
    f.writes.push(request);
    if (request.action === "create") f.rows = [...f.rows, { ...schedule(20), interface_id: request.interfaceId, display_name: "New brief", instructions: request.draft.instructions, schedule: { type: "cron", cron: request.draft.cron } }];
    else if (request.action === "set_enabled") f.rows = f.rows.map((row) => row.interface_id === request.interfaceId ? { ...row, status: request.enabled ? "enabled" : "disabled" } : row);
  } };
  global.scheduleFixture = f;
  return f;
}
async function mount({ active = true, scope = "Experiment", secondary = false, drafts = [], opened = [] } = {}) {
  const ui = await compiled;
  const Navigation = sidebarChrome(ui);
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host); roots.push(root);
  const render = async (next = scope) => act(async () => root.render(React.createElement(ui.ScheduledWorkspaceProvider, { active, key: next, onCreateTask: (text) => drafts.push(text) },
    !secondary && React.createElement(Navigation), React.createElement(ui.LiveScheduledView, { navigationInPane: secondary, searchFocusTick: 0, onSelectChat: (id) => opened.push(id) }))));
  await render();
  return { host, render, drafts };
}
function button(host, label) { return [...host.querySelectorAll("button")].find((element) => element.getAttribute("aria-label") === label || element.textContent.trim() === label); }
function menuItem(label) { return [...document.querySelectorAll('[role^="menuitem"]')].find((element) => element.textContent.trim().replace(/\s/g, " ") === label); }
function rows(host) { return [...host.querySelectorAll('[data-scheduled-sidebar] [role="list"] > [role="listitem"] > [role="button"]')]; }
function secondary(row) { return row.querySelector("[data-thread-secondary-title]").textContent; }
function detail(host) { return host.querySelector("[data-scheduled-detail]"); }
function dialog() { return document.querySelector('[role="dialog"] form[data-density="form"]'); }
async function click(element) {
  assert.ok(element, "button exists");
  await act(async () => element.click());
  // Let a newly opened dialog take initial focus before the next user action.
  await act(async () => new Promise((resolve) => window.requestAnimationFrame(resolve)));
}
async function input(element, value) {
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(prototype, "value").set;
  await act(async () => { setter.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}
async function commit(element, value) { await act(async () => element.focus()); await input(element, value); await act(async () => element.blur()); }
async function choose(menuLabel, option) { await click(button(dialog(), menuLabel)); await click(menuItem(option)); }

test("event triggers show their receiving task without offering cron mutations", async () => {
  const f = fixture([{
    interface_id: "world_issue_events", display_name: "Issue events", status: "enabled", valid: true,
    event_source: { sources: ["issue.opened"], targets: [{ run_id: "review", chat_interface_id: "chat", title: "Review issues" }] },
  }]);
  const opened = [];
  const { host } = await mount({ opened });
  assert.equal(rows(host).length, 1);
  assert.equal(secondary(rows(host)[0]), "On event");
  await click(rows(host)[0]);
  assert.match(detail(host).textContent, /issue.opened/);
  assert.match(detail(host).textContent, /No events received yet/);
  assert.equal(button(detail(host), "Trigger actions"), undefined);
  assert.equal(button(detail(host), "Edit"), undefined);
  await click(button(detail(host), "Open task"));
  assert.deepEqual(opened, ["review"]);
  assert.deepEqual(f.writes, []);
});

test("Triggers keeps navigation, selection, search and authoring in one workspace", async () => {
  const f = fixture([...Array.from({ length: 8 }, (_, index) => schedule(index)), schedule(8, "disabled"), schedule(9, "invalid")]);
  const { host, drafts } = await mount();
  assert.deepEqual(f.reads, ["projection.scheduled_interfaces.list"]);
  const switcher = host.querySelector('[aria-label="Tasks views"]');
  assert.equal(button(switcher, "Triggers").getAttribute("aria-pressed"), "true");
  await click(button(switcher, "All tasks")); assert.match(host.querySelector("nav").textContent, /Tasks\s*No tasks yet/); assert.ok(host.querySelector("[data-scheduled-sidebar]") === null);
  await click(button(switcher, "Triggers")); assert.ok(host.querySelector("[data-scheduled-sidebar]"));
  await click(button(switcher, "Archived tasks")); assert.match(host.querySelector("nav").textContent, /Archived\s*No archived tasks/);
  await click(button(switcher, "Triggers")); assert.equal(host.querySelector('[aria-label="Tasks views"]'), switcher);
  assert.equal(host.querySelectorAll("[data-scheduled-landing] [data-automation-card]").length, 6);
  assert.equal(host.querySelector("[data-scheduled-sidebar] h2").textContent, "Triggers");
  assert.equal(rows(host).length, 5);
  assert.match(secondary(rows(host)[0]), /^Today .+·Daily$/);
  await click(button(host, "Show more")); assert.equal(rows(host).length, 10);
  assert.match(secondary(rows(host)[8]), /^Paused·/);
  assert.match(secondary(rows(host)[9]), /^Needs attention/);
  await click(button(host, "Search triggers"));
  const search = host.querySelector("[data-scheduled-sidebar] input[type=search]"); assert.equal(document.activeElement, search);
  await input(search, "Task 6"); assert.equal(rows(host).length, 1);
  await click(rows(host)[0]);
  assert.equal(detail(host).querySelector("[data-automation-card] .truncate").textContent, "Task 6");
  assert.match(detail(host).querySelector("[data-scheduled-confirmation]").textContent, /^Your trigger is scheduled\. Its next run is /);
  assert.equal(host.querySelector("[data-selected-scheduled-task] [role=button]").getAttribute("aria-current"), "true");
  await act(async () => search.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  assert.equal(host.querySelector("[data-scheduled-sidebar] input[type=search]"), null);
  assert.equal(document.activeElement, button(host, "Search triggers"));
  assert.ok(detail(host));
  await click(button(host.querySelector("[data-scheduled-sidebar]"), "Triggers"));
  await click(host.querySelector("[data-scheduled-landing] [data-automation-card] button"));
  assert.equal(f.writes.length, 0, "suggestions only open a draft");
  assert.equal(dialog(), null);
  assert.match(drafts[0], /^Create a trigger: /);
  assert.match(drafts[0], /Prepare a concise daily brief/);
  assert.match(drafts[0], /weekdays at 9:00 AM/i);
  assert.equal(f.writes.length, 0);
});

test("new triggers open an ordinary chat draft without needing an installed template", async () => {
  const f = fixture([]);
  const { host, drafts } = await mount();
  await click(button(host, "New trigger"));
  assert.deepEqual(drafts, ["Create a trigger that "]);
  assert.equal(dialog(), null);
  assert.equal(f.writes.length, 0);
});

test("the editor preserves interval and custom schedule controls for existing triggers", async () => {
  const f = fixture([schedule(0)]);
  const { host } = await mount();
  await click(rows(host)[0]);
  await click(button(detail(host), "Open"));
  await input(dialog().querySelector("textarea"), "Check the weather.");
  await choose("Repeat schedule", "Interval");
  await commit(dialog().querySelector('input[aria-label="Repeat interval"]'), "2");
  await click(button(dialog(), "Save"));
  assert.equal(f.writes[0].action, "update");
  assert.equal(f.writes[0].interfaceId, "world_task_0");
  assert.equal(f.writes[0].draft.intervalSeconds, 7200);
  await click(button(detail(host), "Open"));
  await choose("Repeat schedule", "Custom");
  await input(document.querySelector('input[placeholder="0 9 * * 1-5"]'), "*/15 9-17 * * 1-5");
  await click(button(document.querySelector('input[placeholder="0 9 * * 1-5"]').closest("form"), "Save"));
  await click(button(dialog(), "Save"));
  assert.equal(f.writes[1].draft.cron, "*/15 9-17 * * 1-5");
});

test("filters, pause, empty and read-only states retain exact declared interface identity", async () => {
  const f = fixture([schedule(0), schedule(1, "disabled")]);
  const { host, render } = await mount();
  assert.equal(rows(host).length, 1, "with fewer than five active triggers the rest wait behind Show more");
  await click(button(host, "Filter tasks"));
  for (const label of ["Active", "Completed", "Needs attention"]) await click(menuItem(label));
  assert.match(host.querySelector("[data-scheduled-sidebar]").textContent, /Paused/);
  assert.equal(rows(host).length, 1);
  await click(rows(host)[0]);
  assert.match(detail(host).textContent, /paused/);
  await click(button(detail(host), "Trigger actions")); await click(menuItem("Resume"));
  assert.deepEqual(f.writes[0], { action: "set_enabled", interfaceId: "world_task_1", expectedDigest: "1".repeat(64), enabled: true });
  assert.match(detail(host).querySelector("[data-scheduled-confirmation]").textContent, /^Your trigger is scheduled/);
  f.rows = []; await render("empty");
  assert.equal(button(host, "New trigger").disabled, false);
  assert.ok([...host.querySelectorAll("[data-scheduled-landing] [data-automation-card] button")].every((element) => !element.disabled));
  f.rows = [schedule(0)]; f.writable = false; await render("read-only");
  assert.equal(button(host, "New trigger").disabled, true);
  await click(rows(host)[0]); assert.equal(button(detail(host), "Open").disabled, true);
  f.error = "Projection disconnected"; await render("error");
  assert.match(host.textContent, /Projection disconnected/);
  assert.equal(rows(host).length, 0);
});

test("inactive Scheduled reads nothing and calendar labels respect timezone boundaries", async () => {
  const f = fixture([schedule(0)]);
  await mount({ active: false }); assert.equal(f.reads.length, 0);
  const { scheduledCalendarTime } = await compiled;
  const now = Date.parse("2026-10-03T21:30:00Z"), next = Date.parse("2026-10-04T01:00:00Z");
  assert.match(scheduledCalendarTime(next, now, "Europe/Kyiv"), /^Today /);
  assert.match(scheduledCalendarTime(next, now, "UTC"), /^Tomorrow /);
  assert.equal(scheduledCalendarTime(next, now, "invalid/timezone"), "Time unavailable");
});

test("a secondary Triggers pane retains its own list without replacing the primary sidebar", async () => {
  const title = "Repaired Home voice memory capture and updated the installed configuration. ".repeat(8).trim();
  fixture([{ ...schedule(0), deliveries: [{ seq: 1, at_ms: Date.now(), run_id: "review", chat_interface_id: "chat", title }] }]);
  const stylesheet = document.createElement("style");
  stylesheet.textContent = fs.readFileSync(path.join(appRoot, "src/design-system/oc-dropdown.css"), "utf8");
  document.body.append(stylesheet);
  const { host } = await mount({ secondary: true });
  assert.equal(host.querySelector("aside"), null);
  await click(button(host, "Triggers"));
  assert.ok(document.querySelector('[role="dialog"] [data-scheduled-sidebar] [role=listitem] > [role=button]'));
  await click(document.querySelector('[role="dialog"] [data-scheduled-sidebar] [role=listitem] > [role=button]'));
  assert.equal(detail(host).querySelector("[data-automation-card] .truncate").textContent, "Task 0");
  assert.ok(document.querySelector('[role="dialog"]') === null);
  await click(button(host, "Triggers"));
  const navigation = document.querySelector('[role="dialog"][aria-label="Triggers"]');
  assert.equal(navigation.querySelector('[data-selected-scheduled-task] [data-thread-title]').textContent, "Task 0");
  assert.ok(navigation.textContent.includes(title), "history keeps the complete title for display and accessibility");
  assert.equal(getComputedStyle(navigation).width, "340px", "history cannot expand the dropdown through its intrinsic content width");
  await click(navigation.querySelector('[data-selected-scheduled-task] > [role="button"]'));
  assert.equal(getComputedStyle(navigation).width, "340px", "collapsing history preserves the same navigation width");
});

test("one-shot deadlines retain their kind and exact timestamp through editing", async () => {
  const dueAt = "2026-10-12T14:30:00.123456789+03:00";
  const one = { ...schedule(0), schedule: { type: "once", due_at: dueAt }, next_run_at_ms: Date.parse(dueAt), next_run_source: "schedule" };
  const f = fixture([one]);
  const { host } = await mount();
  assert.doesNotMatch(secondary(rows(host)[0]), /Estimated|Daily/);
  await click(rows(host)[0]);
  await click(button(detail(host), "Open"));
  assert.equal(dialog().querySelector('[role=switch]').getAttribute("aria-checked"), "false");
  assert.ok(dialog().querySelector('input[type="date"]'));
  assert.equal(button(dialog(), "Repeat schedule"), undefined);
  assert.equal(button(dialog(), "Cancel"), undefined);
  await click(button(dialog(), "Save"));
  assert.equal(f.writes.length, 1);
  assert.equal(f.writes[0].draft.type, "once");
  assert.equal(f.writes[0].draft.dueAt, dueAt);
  assert.equal(f.writes[0].draft.fireOnStart, false);
  assert.equal(f.writes[0].draft.cron, undefined);
});

test("admitted one-shot displays Completed without inventing a next run or Task success", async () => {
  fixture([{ ...schedule(0), schedule: { type: "once", due_at: "2026-10-04T00:00:00Z" }, last_run_at_ms: Date.parse("2026-10-04T00:00:01Z"), next_run_at_ms: null, next_run_source: "" }]);
  const { host } = await mount();
  assert.match(secondary(rows(host)[0]), /^Completed/);
  await click(rows(host)[0]);
  assert.match(detail(host).textContent, /does not repeat/);
  assert.doesNotMatch(detail(host).textContent, /next run is/);
});

test("one-shot timestamps validate calendar dates and preserve nanosecond deadline identity", () => {
  const { parseScheduledDueAt } = require('../electron/scheduledTime.cjs');
  assert.equal(parseScheduledDueAt('2026-10-12T14:30:00.123456789+03:00'), parseScheduledDueAt('2026-10-12T11:30:00.123456789Z'));
  assert.equal(parseScheduledDueAt('2026-10-12T11:30:00.123456790Z') - parseScheduledDueAt('2026-10-12T11:30:00.123456789Z'), 1n);
  for (const invalid of ['2026-02-30T00:00:00Z', '2026-10-04T00:00:00-00:00', '2026-10-04T00:00:00', '2263-01-01T00:00:00Z']) assert.throws(() => parseScheduledDueAt(invalid));
});

test("an installed inactivity template opens a chat draft, reports honest state and undoes a journaled run", async () => {
  const inactivity = { seconds: 3600, interface_match: { interface_type: "world", role: "user", interface_driver_ref: "app_server" } };
  const template = { ...schedule(0), interface_id: "world_system_review", display_name: "System review", status: "template", template: true, targets: [], schedule: { type: "inactivity", ...inactivity }, next_run_at_ms: null, next_run_source: "" };
  const journal = { digest: "a".repeat(64), firing: "1790000000000", outcome: "changed", summary: "Saved the export procedure as a skill.", undoable: true, undone: false, checks: [{ check: "doctor", result: "passed", evidence: "seq 9" }],
    changes: [{ kind: "file", path: "/home/registry/artifact/a/artifact.json", action: "created", reason: "repeated", status: "applied", restorable: true, unrestorable_reason: "" }] };
  const trigger = { ...template, interface_id: "world_quiet_review", display_name: "Quiet review", status: "enabled", template: false, schedule: { type: "inactivity", ...inactivity, within_ee: "ee_project" },
    targets: [{ run_id: "run_review_1", chat_interface_id: "world_operator_chat_review", title: "Quiet review" }],
    inactivity_state: { status: "waiting", current: true, last_activity_at_ms: Date.now() }, next_run_at_ms: Date.now() + 3_600_000, next_run_source: "driver_state",
    deliveries: [{ seq: 12, at_ms: Date.now() - 60_000, run_id: "run_review_1", chat_interface_id: "world_operator_chat_review", journal }] };
  const f = fixture([template]);
  const { host, drafts } = await mount();
  assert.equal(rows(host).length, 0);
  const suggestion = [...host.querySelectorAll("[data-scheduled-landing] [data-automation-card] button")].find((element) => element.textContent.includes("System review"));
  assert.match(suggestion.textContent, /After 1 hour without activity/);
  await click(suggestion);
  assert.match(drafts[0], /System review/);
  assert.match(drafts[0], /After 1 hour without activity/);
  assert.equal(f.writes.length, 0);
  assert.equal(dialog(), null);
  f.rows = [template, trigger]; f.writes = [];
  const view = await mount();
  await click(rows(view.host)[0]);
  assert.match(detail(view.host).textContent, /Starts at .* if work stays quiet/);
  const run = view.host.querySelector('[data-selected-scheduled-task] [aria-label^="Saved the export procedure as a skill."]');
  assert.ok(run, "the selected trigger expands into its runs");
  await click(button(run, "Run actions")); await click(menuItem("Undo changes…"));
  assert.match(document.querySelector("[data-scheduled-journal]").textContent, /created · \/home\/registry\/artifact\/a\/artifact\.json/);
  await click(button(document, "Undo changes"));
  assert.deepEqual(f.writes[0], { action: "undo_changes", runId: "run_review_1", firing: "1790000000000", expectedJournalDigest: "a".repeat(64) });
});

test("a change journal undoes only files still holding the recorded result", async () => {
  const { mutateScheduledInterface } = require("../electron/projection.cjs");
  const os = require("node:os");
  const crypto = require("node:crypto");
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "octopus-journal-")));
  const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");
  const registry = path.join(home, "registry");
  const modified = path.join(registry, "interface", "world_a", "interface.json");
  const created = path.join(registry, "artifact", "artifact-new", "artifact.json");
  const journalRoot = path.join(home, "state", "change-journals", "run_review", "1790000000000");
  for (const file of [modified, created, path.join(journalRoot, "before", "0")]) fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(modified, "after\n"); fs.writeFileSync(created, "new\n"); fs.writeFileSync(path.join(journalRoot, "before", "0"), "before\n");
  const journal = { schema_version: 1, run: "run_review", outcome: "changed", summary: "s", changes: [
    { kind: "file", path: modified, action: "modified", before_sha256: sha("before\n"), after_sha256: sha("after\n"), before_copy: "before/0", reason: "r" },
    { kind: "file", path: created, action: "created", before_sha256: null, after_sha256: sha("new\n"), before_copy: null, reason: "r" }] };
  fs.writeFileSync(path.join(journalRoot, "journal.json"), JSON.stringify(journal));
  const digest = sha(fs.readFileSync(path.join(journalRoot, "journal.json")));
  const plans = [];
  const hooks = { commitClientAuthoring: async (plan) => { plans.push(plan); } };
  fs.writeFileSync(created, "edited later\n");
  await assert.rejects(mutateScheduledInterface(registry, home, { action: "undo_changes", runId: "run_review", firing: "1790000000000", expectedJournalDigest: digest }, hooks), /artifact.json is changed/);
  fs.writeFileSync(created, "new\n");
  await mutateScheduledInterface(registry, home, { action: "undo_changes", runId: "run_review", firing: "1790000000000", expectedJournalDigest: digest }, hooks);
  assert.deepEqual(plans[0].writes.map((write) => [write.file, write.beforeDigest, write.content ?? write.next]), [[modified, sha("after\n"), "before\n"], [created, sha("new\n"), null]]);
  fs.rmSync(home, { recursive: true, force: true });
});

test("setup arms each selected pack trigger once as a template copy bound into its own task", async () => {
  const { setupTriggerEntries, setupTriggerProof, armSetupTriggers } = require("../electron/projection.cjs");
  const os = require("node:os");
  const repoRegistry = path.join(appRoot, "..", "registry");
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "octopus-setup-triggers-")));
  const registry = path.join(home, "registry");
  for (const kind of ["interface", "tool"]) {
    for (const entry of fs.readdirSync(path.join(repoRegistry, kind), { withFileTypes: true }).filter((row) => row.isDirectory())) {
      for (const name of ["interface.json", "life.json", "policy.json", "activation.json", "tool.json"]) {
        const source = path.join(repoRegistry, kind, entry.name, name);
        if (!fs.existsSync(source)) continue;
        fs.mkdirSync(path.join(registry, kind, entry.name), { recursive: true });
        fs.copyFileSync(source, path.join(registry, kind, entry.name, name));
      }
    }
  }
  fs.mkdirSync(path.join(home, "state", "runs"), { recursive: true });
  const pack = JSON.parse(fs.readFileSync(path.join(appRoot, "distribution", "local-packages", "packs", "codex.json"), "utf8"));
  const entries = setupTriggerEntries(pack, ["world_system_review"]);
  assert.deepEqual(entries.map((entry) => [entry.interface_id, entry.quiet_period_seconds]), [["world_self_improvement", 3600]]);
  const labels = [];
  const planned = [];
  const runPath = path.join(home, "state", "runs", "run_trigger_task.json");
  const hooks = {
    commitClientAuthoring: async (plan) => {
      labels.push(plan.label);
      for (const write of plan.writes) {
        fs.mkdirSync(path.dirname(write.file), { recursive: true });
        fs.writeFileSync(write.file, write.content ?? `${JSON.stringify(write.next, null, 2)}\n`);
      }
    },
    planTriggerTask: async (entry, input) => {
      planned.push([entry.task.worker_interface_id, input]);
      assert.ok(fs.existsSync(path.join(registry, "interface", input.interfaceId, "interface.json")), "the trigger folder exists before its task is planned");
      const run = { id: "run_trigger_task", checkpoints: [{ id: input.checkpointId, interface_binding: { interface: input.interfaceId } }] };
      return { runId: run.id, writes: [{ file: runPath, beforeDigest: null, content: `${JSON.stringify(run)}\n` }] };
    },
  };
  assert.equal(await setupTriggerProof(registry, home, entries), null);
  await armSetupTriggers(registry, home, entries, hooks);
  await armSetupTriggers(registry, home, entries, hooks);
  assert.deepEqual(labels, ["create trigger world_self_improvement", "create the task of trigger world_self_improvement"]);
  assert.deepEqual(planned, [["system_worker_system_review", { checkpointId: "cp_world_self_improvement", interfaceId: "world_self_improvement", artifactNames: ["cron.current_time"] }]]);
  const [proof] = await setupTriggerProof(registry, home, entries);
  assert.deepEqual(proof.run_ids, ["run_trigger_task"]);
  const descriptor = JSON.parse(fs.readFileSync(path.join(registry, "interface", "world_self_improvement", "interface.json"), "utf8"));
  assert.equal(descriptor.copy_of, "world_cron");
  const read = (id) => JSON.parse(fs.readFileSync(path.join(registry, "interface", id, "activation.json"), "utf8")).activations[0];
  const armed = read("world_self_improvement");
  const template = read("world_system_review");
  assert.equal(armed.enabled, true);
  assert.equal(template.enabled, false);
  assert.equal(armed.config.schedule.inactivity.seconds, 3600);
  assert.deepEqual(armed.config.schedule.inactivity.exclude_interface_match, template.config.schedule.inactivity.exclude_interface_match);
  assert.equal(armed.config.schedule.reminder_default, template.config.schedule.reminder_default);
  assert.deepEqual(Object.keys(armed.config), ["schedule"]);
  fs.rmSync(home, { recursive: true, force: true });
});
