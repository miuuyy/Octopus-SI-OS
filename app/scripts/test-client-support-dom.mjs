import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { build } from "esbuild";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const output = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-support-dom-"));
const dom = new JSDOM('<div id="root"></div>', { url: "https://client.test" });
const w = dom.window;
const channels = [];
const NativeMessageChannel = globalThis.MessageChannel;
globalThis.MessageChannel = class extends NativeMessageChannel {
  constructor() { super(); channels.push(this); }
};
Object.assign(globalThis, { window: w, document: w.document, HTMLElement: w.HTMLElement, Element: w.Element, IS_REACT_ACT_ENVIRONMENT: true });
Object.defineProperty(globalThis, "navigator", { value: w.navigator, configurable: true });
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
try {
  await build({ stdin: { contents: 'import "./src/components/icon-glyphs/shell"; export {SupportWindow} from "./src/support/SupportWindow"; export * as React from "react"; export {createRoot} from "react-dom/client";', resolveDir: path.resolve(import.meta.dirname, ".."), loader: "tsx" }, outfile: path.join(output, "test.mjs"), bundle: true, format: "esm", platform: "browser", loader: { ".css": "empty" }, define: { "process.env.NODE_ENV": '"development"' }, logLevel: "silent" });
  const { React, createRoot, SupportWindow } = await import(pathToFileURL(path.join(output, "test.mjs")));
  const report = { report_id: "visible", captured_at: new Date().toISOString(), access: "live", application: { version: "1.0.0", revision: null } };
  let copied, failDetails = false, cancelChoice = false;
  let source = { kind: "current", name: "Current Home" };
  w.octopusSupport = {
    collect: async (details, choice) => {
      if (choice === "choose" && cancelChoice) return null;
      if (choice) source = choice === "choose" ? { kind: "saved", name: ".octopus-preserved" } : { kind: "current", name: "Current Home" };
      if (details && failDetails) throw new Error("Collection failed");
      return { report: { ...report, disclosure: details ? "detailed" : "metadata" }, source };
    },
    copy: async (value) => { copied = value; return { copied: true }; },
    save: async () => ({ saved: false }),
    openIssue: async () => ({ opened: true }),
  };
  const root = createRoot(document.getElementById("root"));
  const button = (label) => [...document.querySelectorAll("button")].find((node) => node.textContent === label);
  const click = (node) => React.act(async () => node.click());
  await React.act(async () => root.render(React.createElement(React.StrictMode, null, React.createElement(SupportWindow))));
  assert.match(document.querySelector("pre").textContent, /metadata/);
  await click(button("Copy report"));
  assert.equal(copied.reportId, "visible");
  await click(button("Save report…"));
  assert.doesNotMatch(document.body.textContent, /Report saved/);
  cancelChoice = true;
  await click(button("Choose saved Home…"));
  assert.equal(button("Copy report").disabled, false);
  cancelChoice = false;
  await click(button("Choose saved Home…"));
  assert.match(document.body.textContent, /Saved Home: .octopus-preserved/);
  await click(button("Refresh"));
  assert.match(document.body.textContent, /Saved Home: .octopus-preserved/);
  await click(button("Use current Home"));
  assert.doesNotMatch(document.body.textContent, /Saved Home:/);
  failDetails = true;
  await click(document.querySelector('input[type="checkbox"]'));
  assert.match(document.querySelector('[role="alert"]').textContent, /Collection failed/);
  assert.equal(button("Copy report").disabled, true);
  assert.equal(document.querySelector('input[type="checkbox"]').disabled, false);
  await click(document.querySelector('input[type="checkbox"]'));
  assert.equal(button("Copy report").disabled, false);
  await React.act(async () => root.unmount());
  console.log("PASS report preview, snapshot actions, cancellation and collection failure recovery");
} finally {
  dom.window.close();
  for (const channel of channels) { channel.port1.close(); channel.port2.close(); }
  globalThis.MessageChannel = NativeMessageChannel;
  await fs.rm(output, { recursive: true, force: true });
}
