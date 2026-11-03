"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const digest = bytes => createHash("sha256").update(bytes).digest("hex");
const hash = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const runtimeVersion = value => typeof value === "string" && /^\d+\.\d+\.\d+$/.test(value);
const assetName = "assets/pop-out-window-egg@3x.png";
const assetDigest = "8979fa7ce7b7dabf97ab3e73c5e7652cbf5564a94ed7cd213fa070eebc2519ea";
const settingsAssets = {"assets/SettingsAccessory.bundle/Contents/Info.plist": "9fc985fd70f134c1eb5971e4b93ad52aef8ba6df2157ab8ae532eb4684a1bbfe", "assets/SettingsAccessory.bundle/Contents/Resources/Assets.car": "f20daabb37db9e1a92f1af8d108c55683603b2c13633b3bc9d1d88524b41ccbb"};
function regular(file, maximum) {
  if (!path.isAbsolute(file) || fs.realpathSync(file) !== file) throw new Error("Client presentation artifact path must be canonical");
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > maximum) throw new Error("Client presentation artifact must be a bounded regular file");
  return fs.readFileSync(file);
}
function sourceContract(manifest, receipt, manifestDigest, architecture) {
  const build = receipt?.build;
  if (receipt?.schema_version !== 1 || receipt.id !== "octopus.client.presentation" || receipt.module !== "octopus-pip-receiver.node"
    || !["arm64", "x64"].includes(architecture) || receipt.architecture !== architecture
    || manifest?.schema_version !== 1 || manifest.platform !== "darwin" || manifest.module !== receipt.module || manifest.node_api_version !== 8
    || !manifest.architectures?.includes(architecture)
    || build?.schema_version !== 2 || build.target !== "addon" || build.architecture !== architecture || build.compiled !== true || build.nativeExecution !== false
    || !runtimeVersion(build.header_version) || !runtimeVersion(build.electron_version) || build.manifest_sha256 !== manifestDigest
    || !hash(receipt.compiled_output_sha256) || build.output_sha256 !== receipt.compiled_output_sha256 || !hash(receipt.output_sha256)) {
    throw new Error("Client presentation artifact receipt differs from its exact native build contract");
  }
  if (!Array.isArray(manifest.files) || !manifest.files.length) throw new Error("Client presentation source inventory is missing");
  const source = Object.create(null);
  for (const entry of manifest.files) {
    if (typeof entry?.path !== "string" || path.basename(entry.path) !== entry.path || !hash(entry.sha256) || Object.hasOwn(source, entry.path)) throw new Error("Client presentation source inventory is invalid");
    source[entry.path] = entry.sha256;
  }
  const same = (actual, expected) => actual && Object.keys(actual).length === Object.keys(expected).length && Object.entries(expected).every(([key, value]) => actual[key] === value);
  if (!same(build.source_sha256, source) || !same(build.stack_sha256, { "stack.h": source["stack.h"], "stack.mm": source["stack.mm"], [assetName]: assetDigest, ...settingsAssets })) throw new Error("Client presentation source or placement asset receipt differs");
  if (build.swift?.source !== "settings-visuals.swift" || build.swift.source_sha256 !== source["settings-visuals.swift"]
    || !["compiler_sha256", "object_sha256", "header_sha256"].every(key => hash(build.swift[key]))) throw new Error("Client presentation SwiftUI build receipt differs");
  return build;
}
function verifyClientPresentationArtifact({ resourcesRoot, appRoot, isPackaged, architecture, nodeVersion, electronVersion }) {
  if (!isPackaged) throw new Error("Native client presentation requires a packaged, receipted client artifact");
  if (!path.isAbsolute(resourcesRoot) || fs.realpathSync(resourcesRoot) !== resourcesRoot || appRoot !== path.join(resourcesRoot, "app") || fs.realpathSync(appRoot) !== appRoot) throw new Error("Client presentation must come from this client's canonical resources");
  const root = path.join(resourcesRoot, "native-presentation"), manifestBytes = regular(path.join(root, "manifest.json"), 1024 * 1024);
  const manifest = JSON.parse(manifestBytes), receipt = JSON.parse(regular(path.join(root, "receipt.json"), 1024 * 1024));
  const build = sourceContract(manifest, receipt, digest(manifestBytes), architecture);
  if (nodeVersion !== build.header_version || electronVersion !== build.electron_version || receipt.signing?.scheme !== "ad-hoc" || receipt.signing.owner !== "package-macos.cjs") throw new Error("Client presentation runtime or packaged signing receipt differs");
  const modulePath = path.join(root, receipt.module), placementAssetPath = path.join(root, assetName);
  if (digest(regular(modulePath, 64 * 1024 * 1024)) !== receipt.output_sha256 || digest(regular(placementAssetPath, 3767)) !== assetDigest) throw new Error("Client presentation bytes differ from the packaged receipt");
  for (const [name, expected] of Object.entries(settingsAssets)) if (digest(regular(path.join(root, name), 28632)) !== expected) throw new Error("Client Settings image resource differs from its receipt");
  return { modulePath, placementAssetPath, settingsAssetBundlePath: path.join(root, "assets/SettingsAccessory.bundle"), outputSha256: receipt.output_sha256 };
}
function createClientPresentationHost(options) {
  for (const field of ["artifactOptions", "homePath", "requireAccess", "syncHosts", "onEvent"]) if (typeof options?.[field] !== "function") throw new Error(`Client presentation requires ${field}`);
  const suppressed = new Set(), batches = new Map();
  let host = null, pending = null, home = null, closed = false;
  function current() {
    if (closed) throw new Error("Client presentation host is closed");
    options.requireAccess();
    const now = options.homePath();
    if (home !== null && now !== home) throw new Error("Client presentation Home changed; restart the client");
    if (home === null) home = now;
  }
  function onEvent(event) {
    if (closed) return;
    if (event?.type === "hide-groups" && Array.isArray(event.groups) && event.groups.every(group => typeof group === "string")) {
      for (const group of event.groups) suppressed.add(group);
      host?.setSelection({ activeGroupId: null, suppressedGroupIds: [...suppressed] });
    }
    options.onEvent(event);
  }
  async function get() {
    current();
    if (!pending) pending = Promise.resolve().then(() => {
      current(); const artifact = verifyClientPresentationArtifact(options.artifactOptions()); current();
      const addon = (options.loadAddon ?? require)(artifact.modulePath);
      if (typeof addon?.createPiPHost !== "function") throw new Error("Client presentation library lacks its exact main factory");
      current();
      const value = addon.createPiPHost({ placementAssetPath: artifact.placementAssetPath, settingsAssetBundlePath: artifact.settingsAssetBundlePath, onEvent,
        controlText: { closeTooltip: "Close preview", hide: "Hide", hideForGroup: "Hide for this task", hideForAllGroups: "Hide for all tasks", placementTooltip: "Move preview" } });
      try {
        current();
        for (const method of ["registerHost", "removeHost", "setSelection", "upsertBrowserImage", "invalidateBrowserImage", "beginSource", "installSource", "cancelSource", "embedSource", "detachEmbeddedSource", "hideEmbeddedSource", "completeGroup", "clearCompletedGroup", "stop"]) {
          if (typeof value?.[method] !== "function") throw new Error("Client presentation library API is incomplete");
        }
        host = value; options.syncHosts(value); return value;
      } catch (error) { value?.stop?.(); throw error; }
    });
    const value = await pending; current(); return value;
  }
  function sync(turns) {
    current();
    if (!Array.isArray(turns)) throw new Error("Client presentation requires the current turn projection");
    for (const turn of turns) {
      if (typeof turn?.run_id !== "string" || !Number.isSafeInteger(turn.turn_opened_movement_seq) || turn.turn_opened_movement_seq < 0) continue;
      const previous = batches.get(turn.run_id);
      if (turn.turn_open && previous !== undefined && previous !== turn.turn_opened_movement_seq) suppressed.delete(turn.run_id);
      batches.set(turn.run_id, turn.turn_opened_movement_seq);
    }
    if (host) { options.syncHosts(host); host.setSelection({ activeGroupId: null, suppressedGroupIds: [...suppressed] }); }
  }
  function stop() { if (closed) return; closed = true; host?.stop(); host = null; suppressed.clear(); batches.clear(); }
  return { get, sync, stop, home: () => home };
}
module.exports = { verifyClientPresentationArtifact, createClientPresentationHost, sourceContract, settingsAssets };
