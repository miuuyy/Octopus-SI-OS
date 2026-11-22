const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { createRuntimePackager, stripArgsForPlatform, isOpenCodexBridgeCurrent } = require("./runtime-package.cjs");

test("CLI and desktop share exact owner executable materialization", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-cli-package-"));
  const put = (file, value) => {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, typeof value === "string" ? value : `${JSON.stringify(value)}\n`);
  };
  try {
    const appRoot = path.join(root, "app"), source = path.join(root, "registry"), outputRoot = path.join(root, "output");
    put(path.join(appRoot, "distribution/generated-outputs.json"), { schema_version: 1, paths: ["registry/interface/provider/generated/"] });
    put(path.join(source, "interface/world_provider_codex/packages/octopus-provider-codex/managed-cli/bootstrap.cjs"), "module.exports = {};\n");
    put(path.join(source, ".octopus-source.json"), { schema_version: 1, id: "test-baseline" });
    put(path.join(source, "worker_environment_kind/test/worker_environment_kind.json"), {
      worker_environment_kind_id: "test", tool_process_bins: ["target/debug/test-tool"],
    });
    put(path.join(source, "tool/test.run/tool.json"), {
      id: "test.run", interface: "test_door", supported_worker_environment_kinds: ["test"], process: { bin: "target/debug/test-tool" },
    });
    put(path.join(source, "interface/test_door/life.json"), {
      processes: [{ bin: "target/debug/test-life", authority: { executable_paths: ["target/debug/test-tool"] } }],
    });
    put(path.join(source, "interface/test_door/stdio.json"), { command: "target/debug/test-tool", args: ["--server"] });
    put(path.join(source, "interface/test_planner/interface.json"), {
      interface_driver_config: { private_config: { task_planner_executable: "target/debug/test-planner" } },
    });
    put(path.join(source, "interface/test_door/target/excluded"), "development output");
    put(path.join(root, "target/release/test-tool"), "tool executable");
    put(path.join(root, "target/release/test-life"), "door executable");
    const manifest = path.join(appRoot, "distribution/system-baseline.json");
    put(manifest, { schema_version: 1, id: "test-baseline", registry_owners: [{ kind: "interface", id: "test_door" }] });
    const builder = createRuntimePackager({ repoRoot: root, appRoot, outputRoot });
    for (const platform of ["linux", "darwin"]) {
      const target = createRuntimePackager({ repoRoot: root, appRoot, outputRoot, platform });
      const service = "octopus-linux-process-authority";
      assert.equal(target.collectBundledBinaries(source).includes(service), platform === "linux");
      assert.equal(target.sharedBinaries(new Set()).includes(service), platform === "linux");
    }
    const registry = path.join(outputRoot, "local-packages/registry");
    builder.copyRegistrySurface(source, registry);
    builder.copyRegistryBaseline(source, path.join(outputRoot, "registry"), manifest);
    const owned = new Set(), installationReferences = new Set();
    builder.rewriteBundledExecutablePaths(registry, builder.registryExecutableOwnership(registry), owned, installationReferences);
    const read = file => JSON.parse(fs.readFileSync(path.join(registry, file), "utf8"));
    assert.equal(read("tool/test.run/tool.json").process.bin, "owner://packages/bin/test-tool");
    assert.deepEqual(read("worker_environment_kind/test/worker_environment_kind.json").tool_process_bins, ["owner://packages/bin/test-tool"]);
    assert.deepEqual(read("interface/test_door/stdio.json"), { command: "owner://packages/bin/test-tool", args: ["--server"] });
    const process = read("interface/test_door/life.json").processes[0];
    assert.equal(process.bin, "owner://packages/bin/test-life");
    assert.deepEqual(process.authority.executable_paths, []);
    assert.deepEqual(process.authority.executable_root_subpaths, [{ root_slot: "registry_worker_environment_kind_root", relative_path: "test/packages/bin/test-tool" }]);
    assert.equal(read("interface/test_planner/interface.json").interface_driver_config.private_config.task_planner_executable, "bin/test-planner");
    const shared = builder.sharedBinaries(installationReferences);
    assert.ok(shared.includes("octopus") && shared.includes("test-planner"));
    assert.ok(!shared.includes("test-tool") && !shared.includes("test-life"));
    assert.ok(builder.collectBundledBinaries(source).includes("test-tool"));
    assert.equal(owned.size, 2);
    for (const file of owned) assert.ok(fs.statSync(file).mode & 0o111);
    assert.equal(fs.readFileSync(path.join(registry, "worker_environment_kind/test/packages/bin/test-tool"), "utf8"), "tool executable");
    assert.equal(fs.existsSync(path.join(registry, "interface/test_door/target")), false);
    assert.equal(fs.existsSync(path.join(source, "interface/test_door/packages/bin")), false);
    assert.equal(fs.existsSync(path.join(outputRoot, "registry/tool/test.run")), false);
    assert.throws(() => builder.materializeOwnerExecutable(registry, "interface", "../foreign", "test-life", owned), /invalid executable owner/);
    assert.deepEqual(stripArgsForPlatform("linux"), ["--strip-unneeded"]);
    assert.deepEqual(stripArgsForPlatform("darwin"), ["-x"]);
    assert.equal(isOpenCodexBridgeCurrent(path.join(root, "absent-bridge"), { platform: "darwin", architecture: "arm64" }), false);
  } finally { fs.rmSync(root, { recursive: true }); }
});
