const assert = require("node:assert/strict");
const path = require("node:path");

const appRoot = path.resolve(__dirname, "..");

function source(relativePath) {
  return require("./splitSource.cjs").readSource(path.join(appRoot, relativePath));
}


async function verifyTemplateFiles() {
  const ts = require("typescript");
  const files = [{ id: `architecture:${"a".repeat(64)}`, content: Array(32).fill(170) }];
  const worker = { id: "worker", payload_artifact_bindings: files };
  const projected = { contexts: [{ ee_id: "ee", workers: [worker] }] };
  function load(relativePath) {
    const module = { exports: {} };
    const code = ts.transpileModule(source(relativePath), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    Function("module", "exports", "require", code)(module, module.exports, (specifier) => (
      specifier === "./projectionClient" ? { readProjection: async () => projected }
        : load(path.join(path.dirname(relativePath), `${specifier}.ts`))
    ));
    return module.exports;
  }
  const { loadChatExecutionContexts } = load("src/chatRuns.ts");
  const { executionContextTaskCapabilities } = load("src/components/chat/chatWorkspaceCapabilities.ts");
  const contexts = await loadChatExecutionContexts();
  const selected = executionContextTaskCapabilities(contexts, "ee", "worker", [], []);
  assert.deepEqual(selected.payloadArtifactBindings, files);
  selected.payloadArtifactBindings[0].content[0] = 0;
  assert.equal(files[0].content[0], 170);
  assert.equal(contexts[0].workers[0].payloadArtifactBindings[0].content[0], 170);
  worker.payload_artifact_bindings = [{ ...files[0], content: [170] }];
  await assert.rejects(loadChatExecutionContexts(), /invalid file reference/);
}

async function verifyDictationAndComposer261120Contracts() {
  const { build } = require("esbuild");
  const { EditorState, TextSelection } = require("prosemirror-state");
  const { schema } = require("prosemirror-markdown");
  const compiled = await build({
    entryPoints: [path.join(appRoot, "src/components/chat/composerDictation.ts")],
    bundle: true, platform: "node", format: "cjs", write: false,
    external: ["prosemirror-state", "prosemirror-view"],
  });
  const mod = { exports: {} };
  Function("require", "module", "exports", compiled.outputFiles[0].text)(require, mod, mod.exports);
  const { composerDictationKey, composerDictationPlugin, startComposerDictation, updateComposerDictation, finishComposerDictation } = mod.exports;
  const emptyDoc = schema.topNodeType.create(null, schema.nodes.paragraph.create());
  const plugin = composerDictationPlugin();
  const view = {
    state: EditorState.create({ doc: emptyDoc, plugins: [plugin] }),
    dispatch(tr) { this.state = this.state.apply(tr); },
    focus() {},
  };
  startComposerDictation(view);
  let draft = composerDictationKey.getState(view.state);
  assert.equal(draft.hideInitialCaret, true);
  assert.equal(draft.appendAtEnd, true);
  assert.equal(plugin.props.attributes(view.state).style, "caret-color: transparent");
  updateComposerDictation(view, "hello world again");
  draft = composerDictationKey.getState(view.state);
  assert.deepEqual(draft.arrivals.map((item) => item.delay), [0, 30, 60]);
  view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, 1)));
  assert.equal(composerDictationKey.getState(view.state).hideInitialCaret, false);
  finishComposerDictation(view, "hello world again");
  assert.equal(composerDictationKey.getState(view.state), null);
  assert.equal(view.state.doc.textContent, "hello world again");

  const composer = source("src/components/chat/ChatComposer.tsx");
  const dictationHook = source("src/components/chat/useLocalDictation.ts");
  const recordings = source("src/components/chat/DictationRecordings.tsx");
  const chatCss = source("src/components/chat/chat.css");
  const queueCss = source("src/components/chat/queue.css");
  const picker = source("src/components/chat/ProviderModelPicker.tsx");
  const pickerCss = source("src/components/chat/providerModelPicker.css");
  assert.match(composer, /ComposerPrimaryActionIcon/);
  assert.match(composer, /shortcut=\{shortcut\}/);
  assert.match(composer, /AnimatePresence initial=\{false\}/);
  assert.match(composer, /data-dictation-restoring/);
  assert.match(dictationHook, /dictation-audio/);
  assert.match(dictationHook, /prepareDictation/);
  assert.match(recordings, /readDictationRecording[\s\S]*URL\.revokeObjectURL/);
  assert.match(chatCss, /--link-primary-text-color-hover:\s*var\(--color-text-user-message\)/);
  assert.match(chatCss, /@keyframes reveal-word/);
  assert.match(chatCss, /interpolate-size:\s*allow-keywords/);
  assert.doesNotMatch(queueCss, /superellipse\(1\.5\)/);
  assert.doesNotMatch(pickerCss, /superellipse\(1\.5\)/);
  assert.match(picker, /data-pointer-down=/);
  assert.match(picker, /TriggerEffortLabel/);
  assert.match(pickerCss, /\[data-pointer-down="false"\]\s+\.provider-model-power-tick:hover/);
  assert.match(pickerCss, /--power-slider-tick-color-duration/);
  assert.match(pickerCss, /var\(--color-background-tooltip\)/);
}

Promise.all([verifyTemplateFiles(), verifyDictationAndComposer261120Contracts()]).then(() => {
  process.stdout.write("chat composer source contracts and template file binding passed\n");
}).catch((error) => { console.error(error); process.exitCode = 1; });
