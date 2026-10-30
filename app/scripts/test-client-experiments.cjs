const assert = require("node:assert/strict");
const { test, after } = require("node:test");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const { JSDOM } = require("jsdom");

const dom = new JSDOM("<body></body>", { url: "https://client.test" });
globalThis.window = dom.window;
after(() => dom.window.close());

function load(file, stubs = {}) {
  const source = fs.readFileSync(path.join(__dirname, "..", "src", file), "utf8");
  const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } });
  const module = { exports: {} };
  Function("require", "module", "exports", outputText)((name) => stubs[name] ?? require(name), module, module.exports);
  return module.exports;
}

test("an experiment is on until the operator turns it off, per profile and across reloads", () => {
  const first = load("clientExperiments.ts");
  assert.equal(first.clientExperimentEnabled("live", "gesture_feedback"), true);
  const { renderToStaticMarkup } = require("react-dom/server");
  const React = require("react");
  let setExperiment;
  const Probe = () => { setExperiment = first.useSetClientExperiment("live"); return null; };
  renderToStaticMarkup(React.createElement(Probe));
  setExperiment("gesture_feedback", false);
  assert.equal(first.clientExperimentEnabled("live", "gesture_feedback"), false);
  assert.equal(first.clientExperimentEnabled("lab", "gesture_feedback"), true);
  const reloaded = load("clientExperiments.ts");
  assert.equal(reloaded.clientExperimentEnabled("live", "gesture_feedback"), false);
});

test("the shortcut hint counts uses of one command and survives a reload", () => {
  const first = load("clientExperiments.ts");
  assert.equal(first.recordMenuCommandUse("lab", "toggle_sidebar"), 1);
  assert.equal(first.recordMenuCommandUse("lab", "toggle_sidebar"), 2);
  assert.equal(load("clientExperiments.ts").recordMenuCommandUse("lab", "toggle_sidebar"), 3);
});

test("the gesture tour starts unfinished and a stored completion is kept", () => {
  const gestures = load("gestureShortcuts.ts", { "./clientActions": { clientCommands: [] } });
  let preferences;
  let setPreferences;
  const { renderToStaticMarkup } = require("react-dom/server");
  const React = require("react");
  const Probe = () => {
    preferences = gestures.useGestureShortcutPreferences("live");
    setPreferences = gestures.useSetGestureShortcutPreferences("live");
    return null;
  };
  renderToStaticMarkup(React.createElement(Probe));
  assert.equal(preferences.tourCompleted, false);
  setPreferences({ enabled: true, tourCompleted: true });
  assert.equal(JSON.parse(dom.window.localStorage.getItem(Object.keys(dom.window.localStorage).find((key) => key.includes("gesture")))).tourCompleted, true);
});
