const fs = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { copyTree, copyFile } = require("./copy-macos.cjs");
const { readGeneratedSourcePaths, assertDeclaredGeneratedOutput } = require("./generated-source-boundary.cjs");

function createRuntimePackager({ repoRoot, appRoot, outputRoot }) {
  for (const [name, value] of Object.entries({ repoRoot, appRoot, outputRoot })) {
    if (typeof value !== "string" || !path.isAbsolute(value)) throw new Error(`${name} must be absolute`);
  }
  const bundledAppRoot = outputRoot;
  const generatedOutputs = readGeneratedSourcePaths(repoRoot);
  const openCodexOwnerRoot = path.join(repoRoot, "registry", "interface", "world_provider_codex", "packages", "octopus-provider-codex");
  const managedCodexRoot = path.join(openCodexOwnerRoot, "managed-cli");
  const managedCodexDistribution = require(path.join(managedCodexRoot, "bootstrap.cjs"));
  const openCodexLifeBinary = "octopus-provider-opencodex-life";

  function bundleManagedCodex(registries) {
    const sharedBin = path.join(bundledAppRoot, "bin");
    run(process.execPath, [path.join(managedCodexRoot, "bootstrap.cjs"), "materialize", "--destination", sharedBin], appRoot);
    const roots = [sharedBin];
    for (const registry of registries) {
      const owner = path.join(registry, "interface", "world_provider_codex");
      if (!fs.existsSync(owner)) continue;
      const destination = path.join(owner, "packages", "bin");
      managedCodexDistribution.copyInstallation(sharedBin, destination);
      roots.push(destination);
    }
    return roots;
  }

  function copyRegistrySurface(source, destination) {
    copyTree(source, destination, {
      recursive: true,
      verbatimSymlinks: true,
      filter: registryCopyFilter,
    });
  }

  function copyLocalPackageDefinitions(source, destination) {
    for (const directory of ["packages", "packs"]) {
      const from = path.join(source, directory);
      const to = path.join(destination, directory);
      if (!fs.statSync(from).isDirectory()) throw new Error(`local package definition root is missing: ${from}`);
      copyTree(from, to, { recursive: true, verbatimSymlinks: true });
    }
  }

  function copyRegistryBaseline(source, destination, manifestPath) {
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    if (
      manifest?.schema_version !== 1
      || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(manifest?.id || "")
      || !Array.isArray(manifest?.registry_owners)
    ) {
      throw new Error(`system baseline manifest is invalid: ${manifestPath}`);
    }
    const identity = JSON.parse(fs.readFileSync(path.join(source, ".octopus-source.json"), "utf8"));
    if (identity?.schema_version !== 1 || identity?.id !== manifest.id) {
      throw new Error(`system baseline identity disagrees with registry source: ${manifestPath}`);
    }
    fs.rmSync(destination, { recursive: true, force: true });
    fs.mkdirSync(destination, { recursive: true });
    copyFile(path.join(source, ".octopus-source.json"), path.join(destination, ".octopus-source.json"));
    for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
      if (entry.isDirectory() && !["target", "node_modules", "__pycache__", ".git"].includes(entry.name)) {
        fs.mkdirSync(path.join(destination, entry.name), { recursive: true });
      }
    }
    const seen = new Set();
    for (const owner of manifest.registry_owners) {
      if (
        !owner
        || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner.kind || "")
        || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner.id || "")
      ) {
        throw new Error(`system baseline contains an invalid registry owner: ${JSON.stringify(owner)}`);
      }
      const key = `${owner.kind}/${owner.id}`;
      if (seen.has(key)) throw new Error(`system baseline repeats registry owner '${key}'`);
      seen.add(key);
      const ownerSource = path.join(source, owner.kind, owner.id);
      const ownerTarget = path.join(destination, owner.kind, owner.id);
      if (!fs.statSync(ownerSource).isDirectory()) throw new Error(`system baseline owner is missing: ${ownerSource}`);
      copyTree(ownerSource, ownerTarget, {
        recursive: true,
        verbatimSymlinks: true,
        filter: registryCopyFilter,
      });
    }
  }

  function registryCopyFilter(candidate) {
    return !candidate.split(path.sep).some((part) => (
      part === ".git"
      || part === "target"
      || part === "node_modules"
      || part === "__pycache__"
      || part === ".DS_Store"
    ));
  }

  const installationBinaries = Object.freeze([
    "octopus",
    "octopus-operator-chat-artifact",
    "octopus-operator-chat-topology",
    "octopus-secret-store",
    "octopus-world-cron-task-plan",
  ]);

  function collectBundledBinaries(source) {
    const binaries = new Set(installationBinaries);
    visitJson(source, (value) => {
      if (typeof value !== "string") return;
      const match = /^target\/debug\/([^/]+)$/.exec(value);
      if (match) binaries.add(match[1]);
    });
    return [...binaries].sort();
  }

  function sharedBinaries(installationReferences) {
    return [...new Set([...installationBinaries, ...installationReferences])].sort();
  }

  function ownedProviderLives(registry = path.join(repoRoot, "registry")) {
    const interfaces = path.join(registry, "interface");
    const lives = [];
    for (const owner of fs.readdirSync(interfaces).sort()) {
      const lifePath = path.join(interfaces, owner, "life.json");
      if (!fs.existsSync(lifePath)) continue;
      for (const process of JSON.parse(fs.readFileSync(lifePath, "utf8")).processes ?? []) {
        const match = /^owner:\/\/packages\/([A-Za-z0-9][A-Za-z0-9._-]*)\/life-bin\/([A-Za-z0-9][A-Za-z0-9._-]*)$/.exec(process.bin ?? "");
        if (!match) continue;
        const ownerRoot = path.join(interfaces, owner, "packages", match[1]);
        if (!fs.statSync(path.join(ownerRoot, "Cargo.toml")).isFile()) throw new Error(`owner life '${process.bin}' has no Cargo package: ${ownerRoot}`);
        lives.push({ ownerRoot, binary: match[2], crateRoot: ownerRoot });
      }
    }
    return lives;
  }

  function ownedNpmBuildPackages() {
    const packages = [];
    for (const declared of generatedOutputs) {
      const relative = typeof declared === "string" ? declared : declared?.path;
      if (typeof relative !== "string" || !relative.startsWith("registry/interface/") || !relative.endsWith("/build-receipt.json")) continue;
      const root = path.join(repoRoot, path.dirname(relative));
      const manifestPath = path.join(root, "package.json");
      if (!fs.existsSync(manifestPath)) continue;
      const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
      if (typeof manifest?.scripts?.build !== "string") throw new Error(`declared npm-built owner package has no build script: ${root}`);
      packages.push(root);
    }
    return [...new Set(packages)].sort();
  }

  function buildOwnedNpmPackages() {
    for (const root of ownedNpmBuildPackages()) {
      run("npm", ["ci", "--ignore-scripts"], root);
      run("npm", ["run", "build"], root);
    }
  }

  function materializeProviderLife(ownerRoot, binary, crateRoot = openCodexOwnerRoot) {
    const source = path.join(repoRoot, "target", "release", binary);
    const destination = path.join(ownerRoot, "life-bin", binary);
    assertDeclaredGeneratedOutput(destination, repoRoot, generatedOutputs);
    assertDeclaredGeneratedOutput(path.join(ownerRoot, "build-receipt.json"), repoRoot, generatedOutputs);
    const stat = fs.statSync(source);
    if (!stat.isFile()) throw new Error(`built provider life is not a regular file: ${source}`);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    copyFile(source, destination);
    fs.chmodSync(destination, 0o755);
    const bytes = fs.readFileSync(destination);
    const receipt = {
      schema_version: 1,
      executable: `life-bin/${binary}`,
      executable_sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
      executable_bytes: bytes.length,
      inputs: {
        "Cargo.lock": crypto.createHash("sha256").update(fs.readFileSync(path.join(repoRoot, "Cargo.lock"))).digest("hex"),
        "Cargo.toml": crypto.createHash("sha256").update(fs.readFileSync(path.join(crateRoot, "Cargo.toml"))).digest("hex"),
      },
    };
    fs.writeFileSync(path.join(ownerRoot, "build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
  }

  function validateOpenCodexOwnerAssets(ownerRoot) {
    const life = path.join(ownerRoot, "life-bin", openCodexLifeBinary);
    const lifeReceiptFile = path.join(ownerRoot, "build-receipt.json");
    const bridgeRoot = path.join(ownerRoot, "opencodex-bridge");
    const bridge = path.join(bridgeRoot, "bin", "octopus-opencodex-bridge");
    const receiptFile = path.join(bridgeRoot, "build-receipt.json");
    for (const executable of [life, bridge]) {
      const stat = fs.statSync(executable);
      if (!stat.isFile() || (stat.mode & 0o111) === 0) {
        throw new Error(`OpenCodex owner executable is not runnable: ${executable}`);
      }
    }
    const lifeBytes = fs.readFileSync(life);
    const lifeReceipt = JSON.parse(fs.readFileSync(lifeReceiptFile, "utf8"));
    const lifeDigest = crypto.createHash("sha256").update(lifeBytes).digest("hex");
    if (
      lifeReceipt?.schema_version !== 1
      || lifeReceipt.executable !== `life-bin/${openCodexLifeBinary}`
      || lifeReceipt.executable_bytes !== lifeBytes.length
      || lifeReceipt.executable_sha256 !== lifeDigest
    ) {
      throw new Error(`OpenCodex owner build receipt does not attest the imported life: ${lifeReceiptFile}`);
    }
    const receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8"));
    const upstream = JSON.parse(fs.readFileSync(path.join(bridgeRoot, "upstream.json"), "utf8"));
    const bridgeBytes = fs.readFileSync(bridge);
    const bridgeDigest = crypto.createHash("sha256").update(bridgeBytes).digest("hex");
    if (
      receipt?.schema_version !== 1
      || receipt.executable !== "bin/octopus-opencodex-bridge"
      || receipt.executable_bytes !== bridgeBytes.length
      || receipt.executable_sha256 !== bridgeDigest
      || typeof upstream?.npm_package !== "string"
      || typeof upstream?.commit !== "string"
      || receipt?.upstream?.npm_package !== upstream.npm_package
      || receipt?.upstream?.commit !== upstream.commit
    ) {
      throw new Error(`OpenCodex owner build receipt does not attest the packaged bridge: ${receiptFile}`);
    }
  }

  function visitJson(root, visit) {
    for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
      const candidate = path.join(root, entry.name);
      if (!registryCopyFilter(candidate)) continue;
      if (entry.isDirectory()) {
        visitJson(candidate, visit);
        continue;
      }
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      walkJson(JSON.parse(fs.readFileSync(candidate, "utf8")), visit);
    }
  }

  function walkJson(value, visit) {
    visit(value);
    if (Array.isArray(value)) {
      for (const item of value) walkJson(item, visit);
    } else if (value && typeof value === "object") {
      for (const item of Object.values(value)) walkJson(item, visit);
    }
  }

  function copyBundledBinaries(binaries) {
    const destination = path.join(bundledAppRoot, "bin");
    fs.mkdirSync(destination, { recursive: true });
    const installed = [];
    for (const binary of binaries) {
      const source = path.join(repoRoot, "target", "release", binary);
      if (!fs.existsSync(source)) throw new Error(`built runtime binary is missing: ${source}`);
      const target = path.join(destination, binary);
      copyFile(source, target);
      fs.chmodSync(target, 0o755);
      installed.push(target);
    }
    return installed;
  }

  function registryExecutableOwnership(registry) {
    const toolBinaryKinds = new Map();
    const doorInterfaceKinds = new Map();
    const kindRoot = path.join(registry, "worker_environment_kind");
    for (const entry of fs.readdirSync(kindRoot, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const file = path.join(kindRoot, entry.name, "worker_environment_kind.json");
      if (!fs.existsSync(file)) continue;
      const descriptor = JSON.parse(fs.readFileSync(file, "utf8"));
      if (descriptor.worker_environment_kind_id !== entry.name || !Array.isArray(descriptor.tool_process_bins)) {
        throw new Error(`worker environment kind has invalid executable ownership: ${file}`);
      }
      for (const declared of descriptor.tool_process_bins) {
        const match = /^target\/debug\/([^/]+)$/.exec(declared);
        if (!match) continue;
        const kinds = toolBinaryKinds.get(match[1]) ?? new Set();
        kinds.add(entry.name);
        toolBinaryKinds.set(match[1], kinds);
      }
    }
    const toolRoot = path.join(registry, "tool");
    for (const entry of fs.readdirSync(toolRoot, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const file = path.join(toolRoot, entry.name, "tool.json");
      if (!fs.existsSync(file)) continue;
      const descriptor = JSON.parse(fs.readFileSync(file, "utf8"));
      if (
        descriptor.id !== entry.name
        || !Array.isArray(descriptor.supported_worker_environment_kinds)
        || descriptor.supported_worker_environment_kinds.length !== 1
        || typeof descriptor.interface !== "string"
      ) throw new Error(`tool descriptor has invalid executable ownership: ${file}`);
      const kind = descriptor.supported_worker_environment_kinds[0];
      const kinds = doorInterfaceKinds.get(descriptor.interface) ?? new Set();
      kinds.add(kind);
      doorInterfaceKinds.set(descriptor.interface, kinds);
    }
    return { registry, toolBinaryKinds, doorInterfaceKinds };
  }

  function rewriteBundledExecutablePaths(root, ownership, ownerExecutables, installationReferences) {
    visitJsonFiles(root, (file, value) => {
      const relative = path.relative(root, file).split(path.sep);
      const registryKind = relative[0] || "";
      const owner = relative[1] || "";
      let rewritten = mapJsonEntries(value, [], (item, keys) => {
        if (typeof item !== "string") return item;
        const match = /^target\/debug\/([^/]+)$/.exec(item);
        if (!match) return item;
        const binary = match[1];
        const key = keys.at(-1);
        if (registryKind === "interface" && key === "bin") {
          materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
          return `owner://packages/bin/${binary}`;
        }
        if (registryKind === "tool" && key === "bin") {
          const descriptor = value;
          const kinds = Array.isArray(descriptor.supported_worker_environment_kinds)
            ? descriptor.supported_worker_environment_kinds
            : [];
          if (kinds.length !== 1) throw new Error(`tool '${owner}' has no exact executable owner`);
          materializeOwnerExecutable(root, "worker_environment_kind", kinds[0], binary, ownerExecutables);
          return `owner://packages/bin/${binary}`;
        }
        if (registryKind === "worker_environment_kind" && keys.includes("tool_process_bins")) {
          materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
          return `owner://packages/bin/${binary}`;
        }
        if (registryKind === "interface" && key === "command" && ownership.doorInterfaceKinds.has(owner)) {
          const kind = executableKindForInterface(ownership, owner, binary);
          if (!kind || !ownership.doorInterfaceKinds.get(owner).has(kind)) {
            throw new Error(`door '${owner}' has no exact kind owner for command '${binary}'`);
          }
          materializeOwnerExecutable(root, "worker_environment_kind", kind, binary, ownerExecutables);
          return `owner://packages/bin/${binary}`;
        }
        if (registryKind === "interface" && keys.includes("executable_paths")) {
          const kind = executableKindForInterface(ownership, owner, binary);
          if (kind) {
            materializeOwnerExecutable(root, "worker_environment_kind", kind, binary, ownerExecutables);
            return path.posix.join("..", "..", "worker_environment_kind", kind, "packages", "bin", binary);
          }
          materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
          return `packages/bin/${binary}`;
        }
        if (registryKind === "interface" && keys.includes("executables")) {
          materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
          return `owner://packages/bin/${binary}`;
        }
        if (registryKind === "tool" && keys.includes("executable_paths")) {
          const descriptor = value;
          const kinds = Array.isArray(descriptor.supported_worker_environment_kinds)
            ? descriptor.supported_worker_environment_kinds
            : [];
          if (kinds.length !== 1) throw new Error(`tool '${owner}' has no exact executable owner`);
          materializeOwnerExecutable(root, "worker_environment_kind", kinds[0], binary, ownerExecutables);
          return `packages/bin/${binary}`;
        }
        installationReferences.add(binary);
        return `bin/${binary}`;
      });
      if (registryKind === "interface") {
        rewritten = rewriteInterfaceExecutableImports(rewritten);
      }
      fs.writeFileSync(file, `${JSON.stringify(rewritten, null, 2)}\n`);
    });
  }

  function rewriteInterfaceExecutableImports(value) {
    if (Array.isArray(value)) return value.map(rewriteInterfaceExecutableImports);
    if (!value || typeof value !== "object") return value;
    const rewritten = Object.fromEntries(Object.entries(value).map(([key, item]) => [
      key,
      rewriteInterfaceExecutableImports(item),
    ]));
    if (!Array.isArray(rewritten.executable_paths)) return rewritten;
    const retained = [];
    const imports = Array.isArray(rewritten.executable_root_subpaths)
      ? [...rewritten.executable_root_subpaths]
      : [];
    for (const executable of rewritten.executable_paths) {
      const match = /^\.\.\/\.\.\/worker_environment_kind\/([^/]+)\/packages\/bin\/([^/]+)$/.exec(executable);
      if (!match) {
        retained.push(executable);
        continue;
      }
      imports.push({
        root_slot: "registry_worker_environment_kind_root",
        relative_path: `${match[1]}/packages/bin/${match[2]}`,
      });
    }
    rewritten.executable_paths = retained;
    if (imports.length) {
      rewritten.executable_root_subpaths = imports.sort((left, right) => (
        `${left.root_slot}/${left.relative_path}`.localeCompare(`${right.root_slot}/${right.relative_path}`)
      ));
    }
    return rewritten;
  }

  function executableKindForInterface(ownership, interfaceId, binary) {
    const interfaceKinds = ownership.doorInterfaceKinds.get(interfaceId) ?? new Set();
    const binaryKinds = ownership.toolBinaryKinds.get(binary) ?? new Set();
    const candidates = [...interfaceKinds].filter((kind) => binaryKinds.has(kind));
    if (candidates.length > 1) {
      throw new Error(`interface '${interfaceId}' has ambiguous owner for executable '${binary}'`);
    }
    if (candidates.length === 1) return candidates[0];
    if (binaryKinds.size === 1) return [...binaryKinds][0];
    return "";
  }

  function materializeOwnerExecutable(registry, registryKind, owner, binary, ownerExecutables) {
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner)) throw new Error(`invalid executable owner '${registryKind}/${owner}'`);
    const source = path.join(repoRoot, "target", "release", binary);
    if (!fs.statSync(source).isFile()) throw new Error(`built owner executable is missing: ${source}`);
    const ownerRoot = path.join(registry, registryKind, owner);
    if (!fs.statSync(ownerRoot).isDirectory()) throw new Error(`executable owner folder is missing: ${ownerRoot}`);
    const targetRoot = path.join(ownerRoot, "packages", "bin");
    const target = path.join(targetRoot, binary);
    fs.mkdirSync(targetRoot, { recursive: true });
    if (!fs.existsSync(target)) copyFile(source, target);
    fs.chmodSync(target, 0o755);
    ownerExecutables.add(target);
  }

  function stripExecutables(executables) {
    transformExecutableCopies(executables, file => run("strip", ["-x", file], appRoot));
  }

  function transformExecutableCopies(executables, transform) {
    const copies = new Map();
    for (const file of executables) {
      const digest = crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
      const key = `${path.basename(file)}:${fs.statSync(file).mode & 0o777}:${digest}`;
      if (!copies.has(key)) copies.set(key, []);
      copies.get(key).push(file);
    }
    for (const [first, ...rest] of copies.values()) {
      transform(first);
      for (const file of rest) copyFile(first, file);
    }
  }

  function visitJsonFiles(root, visit) {
    for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
      const candidate = path.join(root, entry.name);
      if (!registryCopyFilter(candidate)) continue;
      if (entry.isDirectory()) {
        visitJsonFiles(candidate, visit);
        continue;
      }
      if (entry.isFile() && entry.name.endsWith(".json")) {
        visit(candidate, JSON.parse(fs.readFileSync(candidate, "utf8")));
      }
    }
  }

  function mapJsonEntries(value, keys, transform) {
    if (Array.isArray(value)) return transform(value.map((item, index) => mapJsonEntries(item, [...keys, String(index)], transform)), keys);
    if (value && typeof value === "object") {
      return transform(Object.fromEntries(Object.entries(value).map(([key, item]) => [
        key,
        mapJsonEntries(item, [...keys, key], transform),
      ])), keys);
    }
    return transform(value, keys);
  }

  function validateBundledLayout(registries, binaries, ownerExecutables) {
    for (const directory of ["public", "target"]) {
      if (fs.existsSync(path.join(bundledAppRoot, directory))) {
        throw new Error(`packaged application contains development directory '${directory}'`);
      }
    }
    for (const registry of registries) {
      visitJson(registry, (value) => {
        if (typeof value !== "string") return;
        if (value.startsWith("target/debug/")) {
          throw new Error(`packaged registry retains a debug executable path: ${value}`);
        }
      });
      const ownerRoot = path.join(
        registry,
        "interface",
        "world_provider_codex",
        "packages",
        "octopus-provider-codex",
      );
      if (fs.existsSync(ownerRoot)) validateOpenCodexOwnerAssets(ownerRoot);
    }
    for (const binary of binaries) {
      const file = path.join(bundledAppRoot, "bin", binary);
      const stat = fs.statSync(file);
      if (!stat.isFile() || (stat.mode & 0o111) === 0) {
        throw new Error(`packaged executable is not a runnable regular file: ${file}`);
      }
    }
    for (const file of ownerExecutables) {
      const stat = fs.statSync(file);
      if (!stat.isFile() || (stat.mode & 0o111) === 0) {
        throw new Error(`package-owned executable is not runnable: ${file}`);
      }
    }
  }

  function providerCompilerReceipts(registry) {
    const interfaces = path.join(registry, "interface");
    if (!fs.existsSync(interfaces)) return [];
    const receipts = [];
    const pending = fs.readdirSync(interfaces).sort()
      .map((owner) => path.join(interfaces, owner, "packages"))
      .filter((packages) => fs.existsSync(packages));
    while (pending.length) {
      const directory = pending.shift();
      for (const entry of fs.readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
        const candidate = path.join(directory, entry.name);
        if (!registryCopyFilter(candidate) || entry.isSymbolicLink()) continue;
        if (entry.isDirectory()) pending.push(candidate);
        else if (entry.isFile() && entry.name === "build-receipt.json") receipts.push(candidate);
      }
    }
    return receipts;
  }

  function captureProviderSigningInputs(registries) {
    const records = [];
    for (const registry of registries) {
      for (const receiptPath of providerCompilerReceipts(registry)) {
        const owner = path.dirname(receiptPath);
        const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
        if (receipt?.schema_version !== 1 || typeof receipt.executable !== "string"
          || path.isAbsolute(receipt.executable) || receipt.executable.split("/").some((part) => !part || part === "." || part === "..")) {
          throw new Error(`Provider compiler receipt does not name an owned executable: ${receiptPath}`);
        }
        const executable = path.join(owner, receipt.executable);
        const bytes = fs.readFileSync(executable);
        if (bytes.length !== receipt.executable_bytes || crypto.createHash("sha256").update(bytes).digest("hex") !== receipt.executable_sha256) {
          throw new Error(`Provider compiler receipt does not match the unsigned packaging input: ${executable}`);
        }
        records.push({ receiptPath, executable, before: receipt });
      }
    }
    return records;
  }

  function sealProviderSigningInputs(records, { relativeRoot = bundledAppRoot, receiptPath = path.join(bundledAppRoot, "provider-signing.json") } = {}) {
    const transformations = records.map(({ receiptPath, executable, before }) => {
      run("codesign", ["--verify", "--strict", executable], appRoot);
      const bytes = fs.readFileSync(executable);
      const after = { ...before, executable_sha256: crypto.createHash("sha256").update(bytes).digest("hex"), executable_bytes: bytes.length };
      fs.writeFileSync(receiptPath, `${JSON.stringify(after, null, 2)}\n`, { mode: 0o644 });
      return { receipt: path.relative(relativeRoot, receiptPath), signing: "ad-hoc", before, after };
    });
    fs.writeFileSync(receiptPath, `${JSON.stringify({ schema_version: 1, transformations }, null, 2)}\n`);
  }

  function signClientResources(root) {
    const pending = [root]; let entries = 0;
    const executables = [];
    for (let cursor = 0; cursor < pending.length; cursor++) for (const name of fs.readdirSync(pending[cursor]).sort()) {
      if (++entries > 100000) throw new Error("Client code signing inventory exceeds its bound");
      const file = path.join(pending[cursor], name), stat = fs.lstatSync(file);
      if (stat.isSymbolicLink()) {
        if (!fs.realpathSync(file).startsWith(root + path.sep)) throw new Error("Client signing source symlink escapes resources");
        continue;
      }
      if (stat.isDirectory()) { pending.push(file); continue; }
      if (!stat.isFile()) throw new Error("Client signing source must be a file or directory");
      const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW), header = Buffer.alloc(4);
      let count; try { count = fs.readSync(fd, header, 0, 4, 0); } finally { fs.closeSync(fd); }
      if (count === 4 && ["cefaedfe","cffaedfe","feedface","feedfacf","cafebabe","bebafeca","cafebabf","bfbafeca"].includes(header.toString("hex"))) {
        executables.push(file);
      }
    }
    transformExecutableCopies(executables, file => run("codesign", ["--force", "--sign", "-", file], appRoot));
  }

  function run(command, args, cwd) {
    const result = spawnSync(command, args, { cwd, stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed with status ${result.status}`);
  }
  return Object.freeze({
    ownedProviderLives,
    buildOwnedNpmPackages,
    captureProviderSigningInputs,
    sealProviderSigningInputs,
    signClientResources,
    bundleManagedCodex,
    copyRegistrySurface,
    copyLocalPackageDefinitions,
    copyRegistryBaseline,
    registryCopyFilter,
    collectBundledBinaries,
    sharedBinaries,
    materializeProviderLife,
    validateOpenCodexOwnerAssets,
    visitJson,
    walkJson,
    copyBundledBinaries,
    registryExecutableOwnership,
    rewriteBundledExecutablePaths,
    rewriteInterfaceExecutableImports,
    executableKindForInterface,
    materializeOwnerExecutable,
    stripExecutables,
    transformExecutableCopies,
    visitJsonFiles,
    mapJsonEntries,
    validateBundledLayout
  });
}

module.exports = { createRuntimePackager };
