const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { writeJsonFileAtomicallySync } = require("../electron/atomicJsonFile.cjs");

function directory(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-atomic-json-"));
  t.after(() => { fs.chmodSync(root, 0o700); fs.rmSync(root, { recursive: true, force: true }); });
  return root;
}

test("a document is replaced by a complete new one through a renamed sibling", (t) => {
  const root = directory(t), file = path.join(root, "window-state.json");
  writeJsonFileAtomicallySync(file, { bounds: { x: 1 } });
  const first = fs.statSync(file);
  assert.equal(fs.readFileSync(file, "utf8"), '{\n  "bounds": {\n    "x": 1\n  }\n}\n');
  assert.equal(first.mode & 0o777, 0o600);
  writeJsonFileAtomicallySync(file, { bounds: { x: 2 } });
  assert.deepEqual(JSON.parse(fs.readFileSync(file, "utf8")), { bounds: { x: 2 } });
  // The target is never opened for writing: the new document is another file renamed over it.
  assert.notEqual(fs.statSync(file).ino, first.ino);
  assert.deepEqual(fs.readdirSync(root), ["window-state.json"]);
  writeJsonFileAtomicallySync(path.join(root, "shared.json"), [], { mode: 0o644 });
  assert.equal(fs.statSync(path.join(root, "shared.json")).mode & 0o777, 0o644);
});

test("a failed write keeps the previous document and leaves no temporary file", (t) => {
  const root = directory(t), file = path.join(root, "window-state.json");
  writeJsonFileAtomicallySync(file, { saved: true });
  const before = fs.readFileSync(file, "utf8");

  const cyclic = {};
  cyclic.self = cyclic;
  assert.throws(() => writeJsonFileAtomicallySync(file, cyclic), TypeError);
  assert.throws(() => writeJsonFileAtomicallySync(file, undefined), /no JSON representation/);

  // The rename fails after the temporary file was written completely.
  const occupied = path.join(root, "occupied.json");
  fs.mkdirSync(occupied);
  fs.writeFileSync(path.join(occupied, "child"), "");
  assert.throws(() => writeJsonFileAtomicallySync(occupied, { saved: false }));
  assert.deepEqual(fs.readdirSync(occupied), ["child"]);

  // The write itself fails after the temporary file was created.
  const write = fs.writeFileSync;
  t.mock.method(fs, "writeFileSync", (target, ...rest) => {
    if (typeof target === "number") throw Object.assign(new Error("no space left on device"), { code: "ENOSPC" });
    return write(target, ...rest);
  });
  assert.throws(() => writeJsonFileAtomicallySync(file, { saved: false }), /no space left/);
  t.mock.restoreAll();

  if (process.getuid?.() !== 0) {
    fs.chmodSync(root, 0o500);
    assert.throws(() => writeJsonFileAtomicallySync(file, { saved: false }), /EACCES|EPERM/);
    fs.chmodSync(root, 0o700);
  }
  assert.equal(fs.readFileSync(file, "utf8"), before);
  assert.deepEqual(fs.readdirSync(root).sort(), ["occupied.json", "window-state.json"]);
});
