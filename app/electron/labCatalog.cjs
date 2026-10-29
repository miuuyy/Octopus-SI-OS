const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

const LAB_INSTALLED_APPLICATIONS = new Set([
  "system_memory",
  "world_cron",
  "world_mcp_github",
  "world_operator_chat",
  "world_provider_anthropic",
  "world_provider_codex",
  "world_provider_openrouter",
  "world_provider_realtime",
  "world_slack",
]);
const LAB_INSTALLED_DRIVERS = new Set([
  "audio_device_session",
  "browser_session",
  "code_mode",
  "desktop_session",
  "filesystem_workspace",
  "git_worktree",
  "process_session",
  "system_room",
  "web_search",
]);
const LAB_PREFERRED_PROVIDER = "world_provider_codex";

function createLabCatalog(options = {}) {
  const repoRoot = path.resolve(__dirname, "..", "..");
  const registryRoot = () => resolveRoot(options.localPackageRegistry, path.join(repoRoot, "registry"));
  const libraryRoot = () => resolveRoot(options.localPackageLibrary, path.join(repoRoot, "app", "distribution", "local-packages"));
  let pending = null;
  return {
    load() {
      pending ??= buildLabCatalog(registryRoot(), libraryRoot()).catch((error) => {
        pending = null;
        throw error;
      });
      return pending;
    },
  };
}

function resolveRoot(resolver, fallback) {
  const value = typeof resolver === "function" ? resolver() : "";
  return typeof value === "string" && value ? value : fallback;
}

async function buildLabCatalog(registryRoot, libraryRoot) {
  const { readLocalPackageCatalog } = require("./localPackages.cjs");
  const { readProviderInterfaceDescriptors } = require("./projection.cjs");
  const stateRoot = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-lab-catalog-"));
  let catalog;
  try {
    catalog = await readLocalPackageCatalog({
      sourceRegistryRoot: registryRoot,
      activeRegistryRoot: path.join(stateRoot, "registry"),
      libraryRoot,
      stateRoot: path.join(stateRoot, "package-state"),
    });
  } finally {
    await fs.rm(stateRoot, { recursive: true, force: true });
  }
  const installedIds = new Set(catalog.packages.filter(labInstalled).map((record) => record.id));
  const packages = catalog.packages.map((record) => labPackageRecord(record, installedIds));
  const packageById = new Map(packages.map((record) => [record.id, record]));
  const packs = catalog.packs.map((pack) => labPackRecord(pack, packageById));
  const errors = [...catalog.errors];
  const applications = [];
  for (const record of packages.filter((candidate) => candidate.product_kind === "application")) {
    try {
      const manifest = await readApplicationManifest(registryRoot, record.product_id);
      if (manifest) applications.push({ record, manifest });
    } catch (error) {
      errors.push(String(error?.message ?? error));
    }
  }
  const drivers = [];
  for (const record of packages.filter((candidate) => candidate.product_kind === "driver")) {
    const descriptor = await readJson(path.join(registryRoot, "worker_environment_kind", record.product_id, "worker_environment_kind.json"));
    if (descriptor) drivers.push({ record, descriptor, tools: await readKindTools(registryRoot, record.product_id) });
  }
  const providers = (await readProviderInterfaceDescriptors(registryRoot))
    .filter((provider) => LAB_INSTALLED_APPLICATIONS.has(provider.id))
    .map((provider) => ({ ...provider, descriptorPath: `lab://registry/interface/${provider.id}/interface.json`, preferred: provider.id === LAB_PREFERRED_PROVIDER }))
    .sort((left, right) => Number(right.preferred) - Number(left.preferred) || left.id.localeCompare(right.id));
  return {
    localPackages: { packages, packs, errors: catalog.errors },
    errors,
    applications,
    drivers,
    providers,
  };
}

function labInstalled(record) {
  if (record.product_kind === "application") return LAB_INSTALLED_APPLICATIONS.has(record.product_id);
  if (record.product_kind === "driver") return LAB_INSTALLED_DRIVERS.has(record.product_id);
  return record.product_kind === "artifact";
}

function labPackageRecord(record, installedIds) {
  const installed = installedIds.has(record.id);
  return {
    ...record,
    manifest_path: `lab://local-packages/${record.id}/package.json`,
    installed,
    materialized: installed,
    partially_materialized: false,
    receipt_state: installed ? "valid" : "missing",
    receipt_operation_id: installed ? `lab-install-${record.id}` : "",
    receipt_digest: installed ? record.source_digest : "",
    installed_digest: installed ? record.source_digest : "",
    member_states: (record.member_states ?? []).map((member) => ({
      ...member,
      installed_digest: installed ? member.source_digest : "",
      content_installed_digest: installed ? member.content_source_digest : "",
      materialized: installed,
    })),
    error: "",
    install_packages: (record.install_packages ?? []).map((entry) => ({
      ...entry,
      installed: installedIds.has(entry.id),
      receipt_state: installedIds.has(entry.id) ? "valid" : "missing",
    })),
  };
}

function labPackRecord(pack, packageById) {
  const components = (pack.components ?? []).map((component) => {
    const record = packageById.get(component.package_id);
    return {
      ...component,
      installed: record?.installed === true,
      materialized: record?.installed === true,
      partially_materialized: false,
      receipt_state: record?.receipt_state ?? "missing",
      receipt_digest: record?.receipt_digest ?? "",
      error: "",
    };
  });
  return {
    ...pack,
    components,
    required_components_materialized: components
      .filter((component) => component.selection === "included")
      .every((component) => component.installed),
  };
}

async function readApplicationManifest(registryRoot, applicationId) {
  const { parseApplicationManifest, readApplicationLogoAsset } = require("./projection.cjs");
  const interfaceRoot = path.join(registryRoot, "interface", applicationId);
  const document = await readJson(path.join(interfaceRoot, "application.json"));
  if (!document) return null;
  const manifest = parseApplicationManifest(document, applicationId, path.join(interfaceRoot, "application.json"));
  const descriptors = {};
  for (const interfaceId of manifest.interfaces) {
    const descriptor = await readJson(path.join(registryRoot, "interface", interfaceId, "interface.json"));
    if (!descriptor) throw new Error(`application '${applicationId}' interface '${interfaceId}' has no descriptor in the bundled registry`);
    descriptors[interfaceId] = descriptor;
  }
  return {
    ...manifest,
    descriptors,
    logo_data_url: await readApplicationLogoAsset(interfaceRoot, manifest.logo, `application logo ${applicationId}`),
    logo_dark_data_url: manifest.logo_dark
      ? await readApplicationLogoAsset(interfaceRoot, manifest.logo_dark, `application dark logo ${applicationId}`)
      : "",
  };
}

async function readKindTools(registryRoot, kind) {
  const toolRoot = path.join(registryRoot, "tool");
  const entries = await fs.readdir(toolRoot, { withFileTypes: true });
  const tools = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const descriptor = await readJson(path.join(toolRoot, entry.name, "tool.json"));
    if (Array.isArray(descriptor?.supported_worker_environment_kinds) && descriptor.supported_worker_environment_kinds.includes(kind)) {
      tools.push({ id: String(descriptor.id || entry.name), interface: String(descriptor.interface || "") });
    }
  }
  return tools.sort((left, right) => left.id.localeCompare(right.id));
}

async function readJson(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

module.exports = {
  createLabCatalog,
};
