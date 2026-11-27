module.exports = { readApplicationPackageCatalog, readApplicationPackageSource, scanApplicationPackageRoot, snapshotApplicationPackageDirectory };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, stableJson } = require("./jsonFiles.cjs");
const { hashVerifiedRegularFile } = require("../parallelFileHash.cjs");
const { readApplicationLogoAsset } = require("./privateConfiguration.cjs");
const { parseApplicationManifest } = require("./sidecars.cjs");
const { isPathInside, mapConcurrentOrdered, requiredSafeRegistryId, stringValue } = require("./values.cjs");

const APPLICATION_SNAPSHOT_CACHE_MAX = 1024;
const SOURCE_EXCLUDES = new Set([".DS_Store", ".git", "__pycache__", "node_modules", "target"]);
const applicationSnapshotCache = new Map();

async function readApplicationPackageCatalog(activeRegistryRoot, localRegistryRoot, warnings, names = null) {
  const [active, local] = await Promise.all([
    scanApplicationPackageRoot(activeRegistryRoot, "active", warnings, true, names),
    scanApplicationPackageRoot(localRegistryRoot, "local", warnings, false, names),
  ]);
  return {
    active,
    sources: local,
    source_conflicts: {},
  };
}

async function scanApplicationPackageRoot(registryRootValue, sourceKind, warnings, optionalRoot, names = null) {
  const registryRoot = stringValue(registryRootValue);
  if (!registryRoot || !path.isAbsolute(registryRoot)) {
    if (!optionalRoot) warnings.push(`${sourceKind} application source registry is unavailable`);
    return [];
  }
  const interfaceRoot = path.join(registryRoot, "interface");
  let entries;
  try {
    const stat = await fs.lstat(interfaceRoot);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error("interface root is not a plain directory");
    entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  } catch (error) {
    if (optionalRoot && error?.code === "ENOENT") return [];
    warnings.push(`${sourceKind} application source cannot be read: ${interfaceRoot}: ${error.message}`);
    return [];
  }
  const packages = await mapConcurrentOrdered(
    entries.filter((entry) => entry.isDirectory() && (!names || names.has(entry.name))),
    8,
    async (entry) => {
      const manifestPath = path.join(interfaceRoot, entry.name, "application.json");
      let manifestDocument;
      try {
        manifestDocument = await readOptionalRegularJsonDocument(
          manifestPath,
          `${sourceKind} application manifest ${entry.name}`,
        );
        if (!manifestDocument) return null;
        return await readApplicationPackageSource(
          interfaceRoot,
          entry.name,
          sourceKind,
          manifestDocument,
        );
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        return null;
      }
    },
  );
  return packages.filter(Boolean);
}

async function readApplicationPackageSource(interfaceRootValue, applicationIdValue, sourceKind, knownManifest = null) {
  const interfaceRoot = await fs.realpath(interfaceRootValue);
  const applicationId = requiredSafeRegistryId(applicationIdValue, "application package id");
  const ownerDirectory = path.join(interfaceRoot, applicationId);
  await requirePlainDirectory(ownerDirectory, `${sourceKind} application package ${applicationId}`);
  const manifestPath = path.join(ownerDirectory, "application.json");
  const manifestDocument = knownManifest ?? await readRegularJsonDocument(
    manifestPath,
    `${sourceKind} application manifest ${applicationId}`,
  );
  const manifest = parseApplicationManifest(
    manifestDocument.document,
    applicationId,
    manifestPath,
  );
  const directories = [];
  for (const interfaceId of manifest.interfaces) {
    const snapshot = await snapshotApplicationPackageDirectory(interfaceRoot, interfaceId, sourceKind);
    const descriptor = await readRegularJsonDocument(
      path.join(snapshot.directory, "interface.json"),
      `${sourceKind} application interface ${interfaceId}`,
    );
    const declaredId = stringValue(descriptor.document?.id ?? descriptor.document?.rule_id);
    if (declaredId !== interfaceId) {
      throw new Error(`${sourceKind} application interface '${interfaceId}' has drifted folder identity`);
    }
    directories.push(snapshot);
  }
  const packageDigest = crypto.createHash("sha256").update(stableJson(
    directories.flatMap((directory) => directory.files.map((file) => ({
      path: path.posix.join("interface", directory.interface_id, file.relative_path.split(path.sep).join("/")),
      sha256: file.sha256,
      mode: file.mode,
    }))),
  )).digest("hex");
  return {
    manifest,
    manifest_path: manifestPath,
    manifest_digest: manifestDocument.digest,
    descriptor_path: path.join(ownerDirectory, "interface.json"),
    package_digest: packageDigest,
    source_kind: sourceKind,
    interface_root: interfaceRoot,
    directories,
    file_count: directories.reduce((count, directory) => count + directory.files.length, 0),
    logo_data_url: await readApplicationLogoAsset(
      ownerDirectory,
      manifest.logo,
      `${sourceKind} application logo ${applicationId}`,
    ),
    logo_dark_data_url: manifest.logo_dark
      ? await readApplicationLogoAsset(
          ownerDirectory,
          manifest.logo_dark,
          `${sourceKind} application dark logo ${applicationId}`,
        )
      : "",
  };
}

async function snapshotApplicationPackageDirectory(interfaceRoot, interfaceId, sourceKind, options = {}) {
  const directory = path.join(interfaceRoot, interfaceId);
  const resolved = await fs.realpath(directory);
  if (!isPathInside(interfaceRoot, resolved)) {
    throw new Error(`${sourceKind} application interface '${interfaceId}' escapes its package source`);
  }
  await requirePlainDirectory(resolved, `${sourceKind} application interface ${interfaceId}`);
  const inventory = [];
  const files = [];
  const directories = [""];
  const visit = async (current, relativeDirectory) => {
    const entries = (await fs.readdir(current, { withFileTypes: true }))
      .sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      if ((sourceKind === "local" || sourceKind === "bundled") && SOURCE_EXCLUDES.has(entry.name)) {
        continue;
      }
      const entryPath = path.join(current, entry.name);
      const relativePath = path.join(relativeDirectory, entry.name);
      const stat = await fs.lstat(entryPath, { bigint: true });
      if (stat.isSymbolicLink()) {
        throw new Error(`${sourceKind} application package contains a symbolic link: ${entryPath}`);
      }
      if (stat.isDirectory()) {
        directories.push(relativePath);
        await visit(entryPath, relativePath);
        continue;
      }
      if (!stat.isFile()) {
        throw new Error(`${sourceKind} application package contains a non-file entry: ${entryPath}`);
      }
      inventory.push({ relative_path: relativePath, path: entryPath, stat });
    }
  };
  await visit(resolved, "");
  const cacheKey = options.fresh ? "" : `${sourceKind}\0${resolved}`;
  const fingerprint = crypto.createHash("sha256").update(stableJson({
    directories,
    files: inventory.map(({ relative_path, stat }) => ({
      path: relative_path,
      dev: String(stat.dev),
      ino: String(stat.ino),
      size: Number(stat.size),
      mode: Number(stat.mode),
      mtimeNs: String(stat.mtimeNs),
      ctimeNs: String(stat.ctimeNs),
    })),
  })).digest("hex");
  const cached = cacheKey ? applicationSnapshotCache.get(cacheKey) : null;
  if (cached?.fingerprint === fingerprint) {
    applicationSnapshotCache.delete(cacheKey);
    applicationSnapshotCache.set(cacheKey, cached);
    return {
      interface_id: interfaceId,
      directory: resolved,
      directories: [...cached.snapshot.directories],
      files: cached.snapshot.files.map((file) => ({ ...file })),
    };
  }
  const hashed = await mapConcurrentOrdered(inventory, 8, async ({ relative_path: relativePath, path: entryPath, stat: before }) => {
    const content = await hashVerifiedRegularFile(entryPath, before, {
      changedLabel: `${sourceKind} application package changed while it was inspected`,
      useVerifiedCache: !options.fresh,
    });
    const after = await fs.lstat(entryPath, { bigint: true });
    if (
      after.isSymbolicLink()
      || !after.isFile()
      || content.size !== Number(before.size)
      || before.dev !== after.dev
      || before.ino !== after.ino
      || before.size !== after.size
      || before.mode !== after.mode
      || before.mtimeNs !== after.mtimeNs
      || before.ctimeNs !== after.ctimeNs
    ) {
      throw new Error(`${sourceKind} application package changed while it was inspected: ${entryPath}`);
    }
    return {
      relative_path: relativePath,
      path: entryPath,
      sha256: content.sha256,
      mode: Number(before.mode) & 0o777,
      size: Number(before.size),
    };
  });
  files.push(...hashed);
  const snapshot = { interface_id: interfaceId, directory: resolved, directories, files };
  if (cacheKey) {
    if (applicationSnapshotCache.size >= APPLICATION_SNAPSHOT_CACHE_MAX) {
      applicationSnapshotCache.delete(applicationSnapshotCache.keys().next().value);
    }
    applicationSnapshotCache.set(cacheKey, {
      fingerprint,
      snapshot: {
        directories: [...directories],
        files: files.map((file) => ({ ...file })),
      },
    });
  }
  return snapshot;
}
