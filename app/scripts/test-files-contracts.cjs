const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test, after } = require("node:test");
const ts = require("typescript");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const appRoot = path.resolve(__dirname, "..");
const source = (relativePath) => fs.readFileSync(path.join(appRoot, relativePath), "utf8");
const main = source("src/main.tsx");

const dom = new JSDOM('<div id="harness"></div><div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "MouseEvent", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
globalThis.requestAnimationFrame = w.requestAnimationFrame = (callback) => setTimeout(callback, 0);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame = clearTimeout;
const filesCss = w.document.createElement("style");
filesCss.textContent = source("src/components/files/files.css");
w.document.head.append(filesCss);

const React = require("react");
const { createRoot } = require("react-dom/client");
const harness = globalThis.__files = { requires: [], codeViews: [], virtualFiles: [], editorProps: [], trees: [], copies: [], workers: [], providers: [], renderOptions: [], themeRegistrations: 0 };
const settle = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));
const shown = (selector) => document.querySelector(selector) !== null;

class CodeView {
  constructor() { harness.codeViews.push(this); }
  setOptions(options) { this.options = options; }
  setup(container) { this.container = container; }
  setItems(items) { this.items = items; }
  render() { this.editor ??= this.options.createEditor("file", { tabSize: 2 }, "edit-state"); }
  getItem(id) { return this.items.find((item) => item.id === id); }
  updateItem(next) { this.items = this.items.map((item) => item.id === next.id ? next : item); }
  scrollTo() {}
  getScrollTop() { return 0; }
  cleanUp() { this.cleaned = true; }
}
class VirtualizedFile {
  constructor(options, virtualizer, metrics) { Object.assign(this, { options, virtualizer, metrics }); harness.virtualFiles.push(this); }
  hydrate(input) { this.hydrated = input; }
  setOptions(options) { this.options = options; }
  setMetrics(metrics) { this.metrics = metrics; }
  render(input) { this.rendered = input; }
  cleanUp() {}
}
const filesDiffs = {
  CodeView, VirtualizedFile, DIFFS_TAG_NAME: "diffs-container", EXTENSION_TO_FILE_FORMAT: { ts: "typescript", md: "markdown", txt: "text" },
  Virtualizer: class { setup(root, content) { Object.assign(this, { root, content }); } cleanUp() {} },
};
const filesEdit = { Editor: class { constructor(type, options) { Object.assign(this, { type, options, canUndo: false, canRedo: false }); } } };
const heavy = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : () => null });
function moduleRequire(id) {
  if (id.startsWith("@octopus/")) harness.requires.push(id);
  if (id === "@octopus/files-diffs") return filesDiffs;
  if (id === "@octopus/files-diffs/edit") return filesEdit;
  if (id.startsWith("@octopus/")) throw new Error(`${id} is not a Files dependency`);
  if (id === "motion/react") return { motion: new Proxy({}, { get: (_, tag) => ({ children, style, ...props }) => React.createElement(tag, props, children) }) };
  if (/^(@pierre\/(diffs|trees)|shiki|@shikijs)/.test(id)) return heavy;
  return require(id);
}

async function compile(entry, stubs) {
  const resolved = Object.fromEntries(Object.entries(stubs).map(([key, value]) => [key.startsWith("src/") ? path.join(appRoot, key) : key, value]));
  const result = await build({
    entryPoints: [path.join(appRoot, entry)], bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
    loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
    define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
    external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@octopus/*", "@pierre/*", "shiki", "@shikijs/*"],
    plugins: [{ name: "stubs", setup(stage) {
      stage.onResolve({ filter: /.*/ }, (args) => {
        const key = args.path.startsWith(".") ? path.resolve(path.dirname(args.importer), args.path) : args.path;
        return Object.hasOwn(resolved, key) ? { path: key, namespace: "stub" } : undefined;
      });
      stage.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ contents: resolved[args.path], loader: "jsx", resolveDir: appRoot }));
    } }],
  });
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(moduleRequire, module, module.exports);
  return module.exports;
}

const appearance = `export const useAppearance = () => ({ darkCodeTheme: "github", lightCodeTheme: "solarized", themeMode: "dark", codeFontSize: 12 }); export const applyAppearance = () => {};`;
const themes = `export const octopusDiffThemeName = (kind, id) => \`octopus-\${kind}-\${id}\`; export const registerOctopusDiffThemes = () => { globalThis.__files.themeRegistrations += 1; };`;
const nothing = (...names) => names.map((name) => `export function ${name}() { return null; }`).join("\n");
const editorModules = compile("src/components/files/FilesCodeEditor.tsx", {
  "src/appearance": appearance,
  "src/components/settings/octopusDiffThemes": themes,
  "src/components/files/filesPierreSetup": "export const adoptFilesStyles = () => {};",
  "src/components/navigation/OctopusSpinner": nothing("OctopusSpinner"),
});
const filesViewModule = compile("src/components/files/FilesView.tsx", {
  "src/components/files/FilesCodeEditor": `export function FilesCodeEditor(props) { globalThis.__files.editorProps.push(props); return null; }`,
  "src/components/OctopusSurface": `import { createElement } from "react";
export { octopusTreeAncestors } from "${path.join(appRoot, "src/components/OctopusSurface.tsx")}";
export function OctopusFileTree(props) { globalThis.__files.trees.push(props); return null; }
export function OctopusFilterField() { return null; }
export function OctopusPanelNote({ children }) { return createElement("p", { className: "stub-note" }, children); }`,
  "src/components/artifacts/artifactFileOperations": `export const resolveArtifactFilePreview = (reference) => globalThis.__files.resolveArtifact(reference);
export const openArtifactFile = async () => {}; export const safeArtifactFilename = (value) => \`safe:\${value}\`;`,
  "src/design-system/components": `import { createElement } from "react";
export function OcButton({ children, disabled, onClick }) { return createElement("button", { disabled, onClick, type: "button" }, children); }
export function OcIconButton({ label, disabled, onClick }) { return createElement("button", { "aria-label": label, disabled, onClick, type: "button" }); }
export function OcMenuItem({ label, disabled, onClick }) { return createElement("button", { role: "menuitem", disabled, onClick, type: "button" }, label); }
export function OcStatus({ label }) { return createElement("span", null, label); }`,
  "src/design-system/OcDropdown": `export function OcDropdown({ open, children }) { return open ? children : null; }`,
  "src/runtimeFormat": `export const copyText = async (value) => { globalThis.__files.copies.push(value); return true; };`,
  "src/toasts": `export const appToast = { danger() {}, success() {} };`,
  "src/keyboardShortcuts": `export const clientDialogOwnsCommands = () => false;`,
  "src/components/artifacts/ArtifactPreview": nothing("ArtifactPreview"),
  "src/components/IconGlyphs": nothing("Glyph"),
  "src/components/navigation/OctopusSpinner": nothing("OctopusSpinner"),
  "src/components/SetupDialogShell": nothing("SetupDialogShell"),
  "src/components/shell/PanelResizeHandle": `export function PanelResizeHandle() { return null; } export const useAnimatedPanelSize = () => ({ mounted: true, opacity: 1, animatedSize: 250 });`,
  "src/components/files/FilesBreadcrumb": nothing("FilesBreadcrumb"),
});
const treeModule = compile("src/components/OctopusSurface.tsx", {
  "@pierre/trees/react": `export const useFileTree = (options) => globalThis.__files.useFileTree(options); export function FileTree() { return null; }`,
  "src/components/IconGlyphs": nothing("Glyph"),
});

const root = createRoot(document.getElementById("harness"));
after(() => { root.unmount(); w.close(); });
async function show(element) {
  root.render(null);
  await settle(5);
  root.render(element);
  await settle();
}

test("Files code surfaces load lazily: a readonly file through the virtualized renderer, an edit through the public editor", async () => {
  const { FilesCodeEditor } = await editorModules;
  const changes = [];
  root.render(React.createElement(FilesCodeEditor, { documentId: '["source","src/view.ts"]', filename: "view.ts", label: "View view.ts", value: "const a = 1;\n", wordWrap: false }));
  await settle(0);
  assert.equal(document.querySelector('[role="status"]')?.getAttribute("aria-label"), "Loading file");
  await settle(400);
  const viewer = document.querySelector(".os-files-code-viewer");
  assert.equal(viewer.getAttribute("aria-label"), "View view.ts");
  assert.equal(window.getComputedStyle(viewer).getPropertyValue("--diffs-gap-block"), "0", "the Files stylesheet removes the block gap between code items");
  const [file] = harness.virtualFiles;
  assert.deepEqual([file.hydrated.file.name, file.hydrated.file.lang, file.hydrated.file.contents], ["view.ts", "typescript", "const a = 1;\n"]);
  assert.equal(file.options.disableWorkerPool, undefined);
  assert.deepEqual([...new Set(harness.requires)], ["@octopus/files-diffs"], "a readonly file never loads the editor");
  root.render(null);
  await settle(5);
  root.render(React.createElement(FilesCodeEditor, { documentId: '["source","src/edit.md"]', filename: "edit.md", label: "Edit edit.md", onChange: (value) => changes.push(value), value: "# Draft\n", wordWrap: true }));
  await settle(0);
  assert.equal(document.querySelector('[role="status"]')?.getAttribute("aria-label"), "Loading editor");
  await settle(400);
  const surface = document.querySelector(".os-files-pierre-viewport");
  assert.equal(surface.getAttribute("aria-label"), "Edit edit.md");
  assert.ok(document.querySelector("textarea") === null);
  const view = harness.codeViews.at(-1);
  assert.deepEqual(view.items.map(({ id, type, edit, file: { name, lang, contents } }) => ({ id, type, edit, name, lang, contents })),
    [{ id: '["source","src/edit.md"]', type: "file", edit: true, name: "edit.md", lang: "markdown", contents: "# Draft\n" }]);
  assert.equal(view.options.overflow, "wrap");
  assert.equal(view.options.disableWorkerPool, undefined);
  view.options.onItemEditChange({ editor: view.editor, file: { ...view.items[0].file, contents: "# Draft\nmore\n" } }, view.items[0]);
  assert.deepEqual(changes, ["# Draft\nmore\n"]);
  assert.deepEqual([...new Set(harness.requires)].sort(), ["@octopus/files-diffs", "@octopus/files-diffs/edit"]);
  assert.equal(window.getComputedStyle(surface).getPropertyValue("--diffs-gap-block"), "0");
});

function filesBridge() {
  const pending = [];
  const bridge = {
    pending,
    texts: {},
    osFileRoots: async () => ({ roots: [{ id: "source", path: "/fixture/root", label: "Source" }] }),
    listOsFiles: (request) => new Promise((resolve) => pending.push(["list", request.path, () => resolve({ ...request, entries: [] })])),
    readOsFile: (request) => new Promise((resolve) => pending.push(["read", request.path, () => resolve({
      rootId: request.rootId, path: request.path, name: path.basename(request.path), size: 5, modifiedAt: "2026-10-01T00:00:00.000Z",
      kind: "text", text: bridge.texts[request.path] ?? "hello", truncated: request.path.includes("huge"), digest: "0".repeat(64),
    })])),
    release: async () => { while (pending.length) pending.shift()[2](); await settle(); },
  };
  window.octopusTransport = bridge;
  return bridge;
}

test("opening a file loads its root and every ancestor directory alongside the file, and edits the exact root and path", async () => {
  const { FilesView } = await filesViewModule;
  const bridge = filesBridge();
  bridge.texts["src/app/main.ts"] = "export {};\n";
  harness.editorProps.length = 0;
  await show(React.createElement(FilesView, { accessProfile: "live", active: true, paneKey: "pane-file", target: { kind: "file", rootId: "source", path: "src/app/main.ts" } }));
  const requested = bridge.pending.map(([kind, requestPath]) => `${kind}:${requestPath}`);
  for (const expected of ["list:", "list:src", "list:src/app", "read:src/app/main.ts"]) assert.ok(requested.includes(expected), `${expected} is requested before any directory answers`);
  await bridge.release();
  const editor = harness.editorProps.at(-1);
  assert.deepEqual([editor.documentId, editor.filename, editor.value], [JSON.stringify(["source", "src/app/main.ts"]), "main.ts", "export {};\n"]);
  editor.onChange("export const edited = true;\n");
  await settle();
  document.querySelector('[aria-label="File viewer options"]').click();
  await settle();
  const copyContents = [...document.querySelectorAll('[role="menuitem"]')].find((item) => item.textContent === "Copy file contents");
  copyContents.click();
  await settle();
  assert.deepEqual(harness.copies, ["export const edited = true;\n"], "copy takes the current draft");
});

test("a truncated file is read-only and offers no contents to copy", async () => {
  const { FilesView } = await filesViewModule;
  const bridge = filesBridge();
  harness.editorProps.length = 0;
  await show(React.createElement(FilesView, { accessProfile: "live", active: true, paneKey: "pane-huge", target: { kind: "file", rootId: "source", path: "huge.log" } }));
  await bridge.release();
  assert.equal(harness.editorProps.length, 0);
  assert.match(document.querySelector(".os-files-editor").textContent, /File is too large/);
  document.querySelector('[aria-label="File viewer options"]').click();
  await settle();
  const copyContents = [...document.querySelectorAll('[role="menuitem"]')].find((item) => item.textContent === "Copy file contents");
  assert.equal(copyContents.disabled, true);
});

test("an artifact opens as text up to the 2 MB preview limit and lists its sanitized filename in the tree", async () => {
  const { FilesView } = await filesViewModule;
  filesBridge();
  const target = { kind: "artifact", artifactId: "artifact-1", runId: "run-1", interfaceId: "world_fixture", label: "report" };
  for (const [paneKey, size] of [["pane-artifact-limit", 2 * 1024 * 1024], ["pane-artifact-over", 2 * 1024 * 1024 + 1]]) {
    harness.editorProps.length = 0;
    harness.trees.length = 0;
    harness.resolveArtifact = async () => ({ file: { filename: "notes/report.md", mediaType: "text/markdown", bytes: new Uint8Array(size).fill(65) }, model: { kind: "text" } });
    await show(React.createElement(FilesView, { accessProfile: "live", active: true, paneKey, target }));
    await settle();
    if (size === 2 * 1024 * 1024) {
      const editor = harness.editorProps.at(-1);
      assert.equal(editor.documentId, `artifact:${JSON.stringify(["artifact-1", "run-1", "world_fixture"])}`);
      assert.equal(editor.value.length, size);
      assert.equal(editor.onChange, undefined);
    } else {
      assert.equal(harness.editorProps.length, 0);
      assert.match(document.querySelector(".os-files-editor").textContent, /exceeds the 2 MB Files text preview limit/);
    }
    assert.deepEqual(harness.trees.at(-1).paths, ["safe:notes/report.md"]);
  }
});

test("an admitted read opens its exact content in Files instead of the tool envelope", async () => {
  const { FilesView } = await filesViewModule;
  filesBridge();
  const previewText = '{"content":{"text":"Retained instructions · Ω"}}';
  const target = { kind: "artifact", artifactId: "read:context", runId: "run-1", interfaceId: "provider", label: "self-improvement", previewText };
  harness.resolveArtifact = async (reference) => {
    assert.equal(reference.inlinePayload, previewText);
    return { file: { filename: "self-improvement.json", mediaType: "application/json", bytes: new TextEncoder().encode(reference.inlinePayload) }, model: { kind: "json" } };
  };
  await show(React.createElement(FilesView, { accessProfile: "live", active: true, paneKey: "pane-read-context", target }));
  await settle();
  assert.equal(harness.editorProps.at(-1).value, previewText);
  assert.equal(harness.editorProps.at(-1).onChange, undefined);
  assert.equal(document.querySelector('[role="dialog"]'), null);
});

test("the renderer root gives the diff worker pool the appearance themes and a Pierre worker", async () => {
  window.octopusTransport = undefined;
  harness.pool = { setRenderOptions: async (options) => { harness.renderOptions.push(options); } };
  await compile("src/main.tsx", {
    "@pierre/diffs/react": `import { createElement } from "react";
export function WorkerPoolContextProvider(props) { globalThis.__files.providers.push(props); return props.children; }
export const useWorkerPool = () => globalThis.__files.pool;`,
    "@pierre/diffs/worker/worker.js?worker": `export default class PierreDiffWorker { constructor() { globalThis.__files.workers.push(this); } }`,
    "src/App": nothing("App"),
    "src/RendererFailureBoundary": `export function RendererFailureBoundary({ children }) { return children; }`,
    "src/appearance": appearance,
    "src/components/settings/octopusDiffThemes": themes,
  });
  await settle(40);
  const theme = { dark: "octopus-dark-github", light: "octopus-light-solarized" };
  const provider = harness.providers.at(-1);
  assert.deepEqual(provider.highlighterOptions, { theme });
  const worker = provider.poolOptions.workerFactory();
  assert.equal(harness.workers.at(-1), worker);
  assert.deepEqual(harness.renderOptions.at(-1), { theme });
  assert.equal(harness.themeRegistrations, 1);
});

test("the file tree resets only for new paths, reveals the selection by scrolling, and never expands parents itself", async () => {
  const { OctopusFileTree } = await treeModule;
  const calls = [];
  const selected = new Set();
  const model = {
    resetPaths: (paths, options) => calls.push(["reset", [...paths], options]),
    getItem: (itemPath) => ({ isDirectory: () => itemPath.endsWith("/"), select: () => selected.add(itemPath), deselect: () => selected.delete(itemPath) }),
    getSelectedPaths: () => [...selected],
    scrollToPath: (itemPath, options) => calls.push(["scroll", itemPath, options]),
    subscribe: () => () => {},
    expand: (itemPath) => calls.push(["expand", itemPath]),
  };
  harness.useFileTree = () => ({ model });
  const selections = [];
  const render = (props) => root.render(React.createElement(OctopusFileTree, { onSelectPath: (value) => selections.push(value), revealSelectedPath: true, revealSelectedPathScrollOffset: "center", ...props }));
  root.render(null);
  await settle(5);
  render({ paths: ["src/", "src/a.ts"], selectedPath: "src/a.ts" });
  await settle();
  assert.deepEqual(calls, [["scroll", "src/a.ts", { offset: "center" }]]);
  assert.deepEqual([...selected], ["src/a.ts"]);
  render({ paths: ["src/", "src/a.ts"], selectedPath: "src/a.ts" });
  await settle();
  assert.equal(calls.length, 1, "the same paths neither reset the model nor scroll again");
  render({ paths: ["src/", "src/a.ts", "src/b.ts"], initialExpandedPaths: ["src"], selectedPath: "src/b.ts" });
  await settle();
  assert.deepEqual(calls.slice(1), [["reset", ["src/", "src/a.ts", "src/b.ts"], { initialExpandedPaths: ["src"] }], ["scroll", "src/b.ts", { offset: "center" }]]);
  assert.deepEqual(selections, [], "syncing the selection is not a user selection");
  assert.equal(calls.some(([name]) => name === "expand"), false);
});

test("Files render worker applies its theme and throws the exact sync failure", async () => {
  const parsed = ts.createSourceFile("main.tsx", main, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "PierreDiffWorkerThemeSync");
  assert.ok(owner, "The current render-worker failure owner must exist");
  const code = ts.transpileModule(owner.getText(parsed), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const theme = { dark: "dark-fixture", light: "light-fixture" };
  const failure = new Error("injected worker theme failure");
  for (const rejected of [null, failure]) {
    let state = null, effect;
    const requests = [];
    const sync = Function("React", "useWorkerPool", `${code}\nreturn PierreDiffWorkerThemeSync;`)(
      { useState: () => [state, (value) => { state = value; }], useEffect: (callback) => { effect = callback; } },
      () => ({ setRenderOptions: (request) => { requests.push(request); return rejected ? Promise.reject(rejected) : Promise.resolve(); } }),
    );
    assert.equal(sync({ theme }), null);
    const cleanup = effect();
    try {
      await Promise.resolve();
      assert.deepEqual(requests, [{ theme }]);
      if (rejected) assert.throws(() => sync({ theme }), (error) => error === failure);
      else assert.equal(sync({ theme }), null);
    } finally { cleanup(); }
  }
});

test("trace file links resolve only through their task and exact tool workspace", () => {
  const parsed = ts.createSourceFile("ChatView.tsx", source("src/components/ChatView.tsx"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "traceFileOpenTarget");
  const code = ts.transpileModule(owner.getText(parsed), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const resolve = Function(code + ";return traceFileOpenTarget;")();
  const room = (tool, root) => ({ id: `room:${tool}`, root, tools: [tool], surfaces: [{ id: "files", kind: "files", mode: "direct", root }, { id: "toolchain", kind: "files", mode: "direct", root: "/toolchain" }] });
  const run = { workerEnvironments: [room("read.one", "/fixture"), room("read.two", "/another")], workerEnvironment: {} };
  assert.deepEqual(resolve("./context.txt", run, "read.one"), { workerEnvironmentId: "room:read.one", surfaceId: "files", panelKind: "file", filePath: "context.txt" });
  assert.deepEqual(resolve("context.txt", run, "read.two"), { workerEnvironmentId: "room:read.two", surfaceId: "files", panelKind: "file", filePath: "context.txt" });
  assert.deepEqual(resolve("/explicit/file.txt", null), { kind: "file", absolutePath: "/explicit/file.txt" });
  assert.equal(resolve("context.txt", { ...run, workerEnvironments: [room("read", "/")] }, "read").filePath, "context.txt");
  assert.equal(resolve("context.txt", { ...run, workerEnvironments: [room("read", "C:\\\\")] }, "read").filePath, "context.txt");
  assert.deepEqual(resolve("/fixture/deep/file.txt", run, "read.one"), { workerEnvironmentId: "room:read.one", surfaceId: "files", panelKind: "file", filePath: "deep/file.txt" });
  assert.equal(resolve("/toolchain/bin/tool", run, "read.two").surfaceId, "toolchain");
  assert.equal(resolve("/Users/person/Downloads/video.mp4", { ...run, workerEnvironments: [room("read", "/")] }, "read").filePath, "Users/person/Downloads/video.mp4");
  assert.throws(() => resolve("/fixture/../outside", run, "read.one"));
  for (const [file, selectedRun, tool] of [["context.txt", run], ["context.txt", null], ["context.txt", run, "missing"], ["../context.txt", run, "read.one"]]) assert.throws(() => resolve(file, selectedRun, tool));
});
