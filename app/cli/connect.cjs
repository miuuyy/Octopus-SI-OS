const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { createApplicationConnectionHost, createRegistryConnectionTargets } = require("../electron/applicationConnection.cjs");
const { readRegularJsonDocument, requirePlainDirectory } = require("../electron/projection/jsonFiles.cjs");
const { parseApplicationManifest } = require("../electron/projection/sidecars.cjs");
const { isPlainRecord, requiredSafeRegistryId } = require("../electron/projection/values.cjs");

const ACTIONS = new Set(["status", "connect", "disconnect"]);

function parseConnectArguments(words) {
  const args = [...words];
  const action = ACTIONS.has(args[0]) ? args.shift() : "connect";
  const applicationId = args.shift();
  if (typeof applicationId !== "string" || !applicationId.trim()) {
    throw new Error("Application id is required. Use 'octopus connect [status|disconnect] <application-id> [interface-id]'.");
  }
  const interfaceId = args.shift() ?? null;
  if (args.length) {
    throw new Error("connect accepts at most an action, application id, and optional interface id. See 'octopus connect --help'.");
  }
  return {
    action,
    applicationId: requiredSafeRegistryId(applicationId, "application id"),
    interfaceId: interfaceId === null ? null : requiredSafeRegistryId(interfaceId, "interface id"),
  };
}

async function resolveConnectableApplication(registryRoot, applicationId, requestedInterfaceId) {
  const applicationDirectory = path.join(registryRoot, "interface", applicationId);
  try {
    await requirePlainDirectory(applicationDirectory, `Application '${applicationId}'`);
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error(`Application '${applicationId}' is not installed in '${registryRoot}'. Run 'octopus setup' first.`);
    }
    throw error;
  }
  const manifestFile = path.join(applicationDirectory, "application.json");
  const manifest = parseApplicationManifest(
    (await readRegularJsonDocument(manifestFile, `application manifest ${applicationId}`)).document,
    applicationId,
    manifestFile,
  );
  if (requestedInterfaceId !== null && !manifest.interfaces.includes(requestedInterfaceId)) {
    throw new Error(`Application '${applicationId}' does not include interface '${requestedInterfaceId}'.`);
  }
  const candidates = requestedInterfaceId === null ? manifest.interfaces : [requestedInterfaceId];
  const interfaceIds = [];
  for (const id of candidates) {
    const descriptorFile = path.join(registryRoot, "interface", id, "interface.json");
    const { document } = await readRegularJsonDocument(descriptorFile, `interface descriptor ${id}`);
    const config = document?.interface_driver_config?.private_config;
    if (isPlainRecord(config) && Object.prototype.hasOwnProperty.call(config, "client_connection")) {
      interfaceIds.push(id);
    } else if (requestedInterfaceId !== null) {
      throw new Error(`Interface '${id}' declares no client connection module.`);
    }
  }
  if (!interfaceIds.length) {
    throw new Error(`Application '${applicationId}' declares no connectable client_connection interface.`);
  }
  return { manifest, interfaceIds };
}

async function ensureInterfacesRoot(home) {
  await requirePlainDirectory(home, `Octopus Home '${home}'`);
  const interfacesRoot = path.join(home, "interfaces");
  try {
    await requirePlainDirectory(interfacesRoot, `interface private root '${interfacesRoot}'`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    await fs.mkdir(interfacesRoot, { mode: 0o700 });
    await requirePlainDirectory(interfacesRoot, `interface private root '${interfacesRoot}'`);
  }
}

async function connectApplication(environment, options) {
  const { action, applicationId, interfaceId } = parseConnectArguments(options.words ?? []);
  const registryRoot = environment.packageRoots.activeRegistryRoot;
  const onProgress = typeof options.onProgress === "function" ? options.onProgress : () => {};
  const openExternal = typeof options.openExternal === "function"
    ? options.openExternal
    : () => { throw new Error("Sign-in requires an external address handler."); };

  const { manifest, interfaceIds } = await resolveConnectableApplication(registryRoot, applicationId, interfaceId);
  await ensureInterfacesRoot(environment.home);
  const host = createApplicationConnectionHost({
    resolveTargets: createRegistryConnectionTargets({
      registryRoot: () => registryRoot,
      octopusHome: () => environment.home,
      launchEnvironment: () => ({ ...process.env, ...(environment.launchEnv ?? {}) }),
      openExternal,
    }),
    scope: () => environment.home,
  });

  try {
    if (action === "status") {
      const probed = await host.probe({ applicationId });
      const interfaces = interfaceId === null
        ? probed.interfaces
        : probed.interfaces.filter((entry) => entry.interfaceId === interfaceId);
      return { applicationId, action, home: environment.home, interfaces };
    }
    if (action === "disconnect") {
      const interfaces = [];
      for (const id of interfaceIds) {
        const state = await host.disconnect({ applicationId, interfaceId: id });
        interfaces.push({ interfaceId: id, state });
      }
      return { applicationId, action, home: environment.home, interfaces };
    }
    const requiredFields = (manifest.setup?.fields ?? []).filter((field) => field.required);
    if (requiredFields.length) {
      throw new Error(
        `Application '${applicationId}' requires setup fields (${requiredFields.map((field) => field.label).join(", ")}) that terminal connect does not collect yet.`,
      );
    }
    for (const statement of manifest.connection?.disclosure ?? []) {
      onProgress({ message: statement });
    }
    const interfaces = [];
    for (const id of interfaceIds) {
      const operationId = `cli_connect_${crypto.randomUUID()}`;
      const abort = () => { host.cancel({ operationId }); };
      options.signal?.addEventListener("abort", abort, { once: true });
      try {
        if (options.signal?.aborted) abort();
        const state = await host.connect(
          { operationId, applicationId, interfaceId: id, input: {} },
          {
            onProgress: (update) => {
              const parts = [update.message];
              if (update.url) parts.push(update.url);
              if (update.code) parts.push(`code: ${update.code}`);
              onProgress({ ...update, message: parts.join(" — ") });
            },
          },
        );
        if (state?.state !== "connected") {
          throw new Error(state?.message || `Application '${applicationId}' interface '${id}' did not connect (${state?.state ?? "unknown"}).`);
        }
        interfaces.push({ interfaceId: id, state });
      } finally {
        options.signal?.removeEventListener("abort", abort);
      }
    }
    return { applicationId, action, home: environment.home, interfaces };
  } finally {
    host.dispose();
  }
}

function presentConnectResult(result) {
  return result.interfaces.map(({ interfaceId, state }) => {
    const details = [
      state.state,
      state.account?.label ? `(${state.account.label})` : "",
      state.message ? `— ${state.message}` : "",
    ].filter(Boolean).join(" ");
    return `${interfaceId}  ${details}`;
  }).join("\n");
}

module.exports = {
  parseConnectArguments,
  connectApplication,
  presentConnectResult,
};
