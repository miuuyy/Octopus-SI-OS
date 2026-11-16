const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { SUPPORTED_TARGETS, hostTarget, resolveCliTarget, takeTargetArgument } = require("./cli-target.cjs");
const { planTree, copyEntries, copyTree, copyFile } = require("./package-copy.cjs");

test("a CLI distribution resolves the target of the machine that builds it", () => {
  assert.deepEqual(SUPPORTED_TARGETS, ["aarch64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"]);
  for (const [platform, architecture, triple] of [
    ["darwin", "arm64", "aarch64-apple-darwin"], ["darwin", "x64", "x86_64-apple-darwin"],
    ["linux", "arm64", "aarch64-unknown-linux-gnu"], ["linux", "x64", "x86_64-unknown-linux-gnu"],
  ]) {
    assert.equal(hostTarget(platform, architecture), triple);
    const implied = resolveCliTarget({ platform, architecture });
    assert.deepEqual([implied.triple, implied.platform, implied.architecture], [triple, platform, architecture]);
    assert.deepEqual(resolveCliTarget({ platform, architecture, target: triple }), implied);
  }
});

test("a Linux distribution is headless and a macOS one keeps every macOS part", () => {
  const all = (value) => ({ client_filesystem: value, desktop_native: value, managed_codex: value, code_signing: value });
  assert.deepEqual(resolveCliTarget({ platform: "darwin", architecture: "arm64" }).components, all(true));
  assert.deepEqual(resolveCliTarget({ platform: "darwin", architecture: "x64" }).components, all(true));
  assert.deepEqual(resolveCliTarget({ platform: "linux", architecture: "x64" }).components, all(false));
  assert.deepEqual(resolveCliTarget({ platform: "linux", architecture: "arm64" }).components, all(false));
});

test("an unsupported host, an unknown target and a cross build are refused by name", () => {
  assert.throws(() => hostTarget("win32", "x64"), /not built on win32-x64; supported targets: aarch64-apple-darwin/);
  assert.throws(() => hostTarget("linux", "riscv64"), /not built on linux-riscv64/);
  assert.throws(() => resolveCliTarget({ platform: "freebsd", architecture: "x64" }), /not built on freebsd-x64/);
  assert.throws(() => resolveCliTarget({ platform: "linux", architecture: "x64", target: "x86_64-unknown-linux-musl" }), /Unknown CLI target 'x86_64-unknown-linux-musl'/);
  assert.throws(() => resolveCliTarget({ platform: "linux", architecture: "x64", target: "aarch64-unknown-linux-gnu" }),
    /aarch64-unknown-linux-gnu cannot be built on x86_64-unknown-linux-gnu/);
  assert.throws(() => resolveCliTarget({ platform: "darwin", architecture: "arm64", target: "x86_64-unknown-linux-gnu" }),
    /x86_64-unknown-linux-gnu cannot be built on aarch64-apple-darwin/);
});

test("the target argument is read once and leaves the other arguments", () => {
  assert.deepEqual(takeTargetArgument(["--runtime"]), { target: null, rest: ["--runtime"] });
  assert.deepEqual(takeTargetArgument(["--target=x86_64-unknown-linux-gnu", "--runtime"]), { target: "x86_64-unknown-linux-gnu", rest: ["--runtime"] });
  assert.throws(() => takeTargetArgument(["--target"]), /requires a value/);
  assert.throws(() => takeTargetArgument(["--target="]), /requires a value/);
  assert.throws(() => takeTargetArgument(["--target=a", "--target=b"]), /more than once/);
});

test("a package tree is copied without clone support, keeping modes, times and links", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-package-copy-test-"));
  try {
    const source = path.join(root, "source"), destination = path.join(root, "destination");
    fs.mkdirSync(path.join(source, "bin"), { recursive: true });
    fs.mkdirSync(path.join(source, "skipped"));
    fs.writeFileSync(path.join(source, "bin", "tool"), "executable", { mode: 0o755 });
    fs.chmodSync(path.join(source, "bin"), 0o750);
    fs.writeFileSync(path.join(source, "data.json"), "{}", { mode: 0o640 });
    fs.writeFileSync(path.join(source, "skipped", "file"), "left out");
    fs.symlinkSync("bin/tool", path.join(source, "relative-link"));
    const old = new Date(Date.UTC(2020, 0, 2, 3, 4, 5));
    fs.utimesSync(path.join(source, "data.json"), old, old);
    const options = { recursive: true, verbatimSymlinks: true, filter: (from) => path.basename(from) !== "skipped" };

    const plan = planTree(source, destination, options);
    assert.deepEqual(plan.map((entry) => [entry.type, path.relative(destination, entry.destination)]).sort(),
      [["directory", ""], ["directory", "bin"], ["file", "bin/tool"], ["file", "data.json"], ["symlink", "relative-link"]]);
    assert.throws(() => planTree(source, destination, {}), /Recursive copy is required/);

    copyTree(source, destination, options, "linux");
    assert.equal(fs.readFileSync(path.join(destination, "bin", "tool"), "utf8"), "executable");
    assert.equal(fs.statSync(path.join(destination, "bin", "tool")).mode & 0o777, 0o755);
    assert.equal(fs.statSync(path.join(destination, "bin")).mode & 0o777, 0o750);
    assert.equal(fs.statSync(path.join(destination, "data.json")).mode & 0o777, 0o640);
    assert.equal(fs.statSync(path.join(destination, "data.json")).mtimeMs, old.getTime());
    assert.notEqual(fs.statSync(path.join(destination, "data.json")).ino, fs.statSync(path.join(source, "data.json")).ino);
    assert.equal(fs.readlinkSync(path.join(destination, "relative-link")), "bin/tool");
    assert.equal(fs.existsSync(path.join(destination, "skipped")), false);

    const resolved = path.join(root, "resolved");
    copyTree(source, resolved, { recursive: true, filter: options.filter }, "linux");
    const sameFile = (left, right) => {
      const one = fs.statSync(left), other = fs.statSync(right);
      return one.dev === other.dev && one.ino === other.ino;
    };
    const recreated = fs.readlinkSync(path.join(resolved, "relative-link"));
    assert.equal(path.isAbsolute(recreated), true);
    assert.equal(sameFile(recreated, path.join(source, "bin", "tool")), true);
    assert.equal(sameFile(path.join(resolved, "relative-link"), path.join(source, "bin", "tool")), true);
    assert.equal(sameFile(recreated, path.join(source, "data.json")), false);

    fs.writeFileSync(path.join(source, "data.json"), "{\"changed\":true}");
    copyTree(source, destination, options, "linux");
    assert.equal(fs.readFileSync(path.join(destination, "data.json"), "utf8"), "{\"changed\":true}");
    copyFile(path.join(source, "bin", "tool"), path.join(destination, "data.json"), "linux");
    assert.equal(fs.readFileSync(path.join(destination, "data.json"), "utf8"), "executable");
    assert.throws(() => copyFile(path.join(source, "data.json"), path.join(destination, "bin"), "linux"), /destination is a directory/);
    assert.throws(() => copyFile(path.join(source, "data.json"), path.join(source, "data.json"), "linux"), /source and destination are identical/);
    assert.throws(() => copyFile(path.join(source, "bin"), path.join(destination, "copy"), "linux"), /not a regular file/);
    assert.throws(() => copyEntries([{ type: "socket", destination: path.join(destination, "x") }]), /Unknown package copy entry/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
