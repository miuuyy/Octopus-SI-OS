const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { Readable } = require("node:stream");
const { createRequire } = require("node:module");
const { test, after } = require("node:test");
const { buildSync } = require("esbuild");

const appRoot = path.resolve(__dirname, "..");
const configFile = path.join(appRoot, "vite.config.ts");
const realRequire = createRequire(configFile);
const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-dev-server-"));
after(() => fs.rmSync(root, { recursive: true, force: true }));
const home = path.join(root, "home");
const blobs = path.join(home, "ledger", "blobs");
const bundle = buildSync({
  entryPoints: [configFile], bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  packages: "external", define: { "import.meta.url": "__configUrl" },
}).outputFiles[0].text;

function loadDevServer(overrides = {}) {
  const world = { events: [], loaderOptions: [], required: [], chatBatch: null };
  world.chatClient = {
    readOperatorChatArtifacts: async (deps, request) => {
      await deps.readArtifactPayload({ artifactId: "a", label: "turn.message" });
      return world.chatBatch(request);
    },
    submitOperatorTurnControl: async (deps, request) => { world.events.push(["turn-control", request]); return { controlled: request.action }; },
    submitOperatorTurn: async (deps, request) => {
      world.events.push(["submit-operator-turn", request.text]);
      await deps.readArtifactPayload({ artifactId: "b", label: "operator.message" });
      return { chatInterfaceId: "world_operator_chat_home", runId: "run-1", label: "operator.message", messageId: request.messageId, attachmentCount: 0, contentHash: "h", messageRecovered: false, messageVerdict: { admitted: { seq: 1 } } };
    },
    updateOperatorChatPins: async (deps, request) => ({ pinned: request.runIds }),
    assertGenericOperatorChatArtifactAllowed: async (request) => {
      world.events.push(["chat-artifact-check", request.interfaceId]);
      if (request.label === "forbidden") throw new Error("operator chat refuses this artifact");
    },
  };
  const electronModules = {
    "./electron/operatorChatClientContract.cjs": (real) => ({ ...real, createOperatorChatClientLoader: (options) => {
      world.loaderOptions.push(options);
      return { load: async () => { world.events.push(["chat-client-load"]); return world.chatClient; } };
    } }),
    "./electron/clientDataSource.cjs": (real) => ({ ...real, createClientProjectionStore: (profile) => ({ read: async () => ({}), generation: () => 0, ensureWatchers: () => {}, profile }) }),
    "./electron/clientArtifacts.cjs": (real) => ({ ...real, submitClientInterfaceArtifact: async (deps, request) => {
      await deps.authorizeSubmission(request);
      world.events.push(["artifact-submitted", request.interfaceId]);
      return { submitted: true };
    } }),
    "./electron/contentAddressedArtifact.cjs": (real) => ({ ...real, readContentAddressedArtifactPayload: async (input) => { world.events.push(["read-payload", input]); return {}; } }),
  };
  const devRequire = Object.assign((id) => {
    world.required.push(id);
    if (Object.hasOwn(overrides, id)) return overrides[id]();
    return electronModules[id] ? electronModules[id](realRequire(id)) : realRequire(id);
  }, { resolve: realRequire.resolve, cache: {} });
  const externals = {
    vite: { defineConfig: (config) => config },
    "@vitejs/plugin-react": () => ({ name: "react" }),
    "@tailwindcss/vite": () => ({ name: "tailwind" }),
    "node:module": { createRequire: () => devRequire },
  };
  const module = { exports: {} };
  Function("require", "module", "exports", "__configUrl", bundle)((id) => externals[id] ?? realRequire(id), module, module.exports, `file://${configFile}`);
  const config = module.exports.default;
  const routes = new Map();
  const server = { middlewares: { use: (route, handler) => routes.set(route, handler) }, watcher: { add: () => {}, on: () => {} }, httpServer: { once: () => {} } };
  const saved = { ...process.env };
  for (const key of Object.keys(process.env)) if (key.startsWith("OCTOPUS_")) delete process.env[key];
  Object.assign(process.env, {
    OCTOPUS_HOME: home,
    OCTOPUS_CLIENT_ACCESS_PATH: path.join(home, "client-access.json"),
    OCTOPUS_INSTALLATION_ROOT: path.join(root, "install"),
    OCTOPUS_LOCAL_PACKAGE_REGISTRY: path.join(root, "registry"),
    OCTOPUS_LOCAL_PACKAGE_LIBRARY: path.join(root, "library"),
  });
  try { config.plugins.find((plugin) => plugin.name === "octopus-client-server").configureServer(server); }
  finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
  const call = (route, { method = "POST", url = "/", token = "", body } = {}) => new Promise((resolve) => {
    const req = Readable.from(body === undefined ? [] : [Buffer.from(JSON.stringify(body))]);
    Object.assign(req, { method, url, headers: token ? { cookie: `octopus_client_access=${encodeURIComponent(token)}` } : {} });
    const headers = {};
    const res = {
      statusCode: 200, headersSent: false,
      setHeader: (key, value) => { headers[key] = value; },
      end: (text) => { res.headersSent = true; resolve({ status: res.statusCode, headers, body: text ? JSON.parse(text) : undefined }); },
    };
    routes.get(route)(req, res, () => resolve({ status: 404 }));
  });
  return { world, config, call };
}

const loadsOf = (world) => world.events.filter(([name]) => name === "chat-client-load").length;
async function signIn(call) {
  const body = { password: "fixture-administrator" };
  const response = fs.existsSync(path.join(home, "client-access.json"))
    ? await call("/__octopus/access", { url: "/unlock", body })
    : await call("/__octopus/access", { url: "/configure", body });
  assert.equal(response.status, 200);
  return decodeURIComponent(/octopus_client_access=([^;]+)/.exec(response.headers["set-cookie"])[1]);
}

test("the dev server bundles ES workers and loads the chat client only through the verified package loader", async () => {
  const { world, config, call } = loadDevServer();
  assert.deepEqual(config.worker, { format: "es" });
  assert.equal(world.loaderOptions.length, 1);
  const [loader] = world.loaderOptions;
  assert.equal(loader.clientPath(), path.join(home, "registry", "interface", "world_operator_chat", "client.cjs"));
  assert.deepEqual(loader.packageRoots(), {
    sourceRegistryRoot: path.join(root, "registry"), activeRegistryRoot: path.join(home, "registry"),
    libraryRoot: path.join(root, "library"), stateRoot: path.join(home, "client", "package-state"),
  });
  const token = await signIn(call);
  world.chatBatch = () => ({ artifacts: [], cursor: { offset: 0, seq: null } });
  for (const [route, body, expected] of [
    ["/__octopus/operator-chat-artifacts", { runId: "run-1" }, { artifacts: [], cursor: { offset: 0, seq: null } }],
    ["/__octopus/operator-turn-control", { interfaceId: "world_operator_chat_home", action: "pause" }, { controlled: "pause" }],
    ["/__octopus/submit", { runId: "run-1", messageId: "m-1", text: "hello", attachments: [] }, undefined],
    ["/__octopus/chat-steer", { runId: "run-1", messageId: "m-2", text: "steer" }, undefined],
    ["/__octopus/chat-pins", { runIds: ["run-1"] }, { pinned: ["run-1"] }],
    ["/__octopus/interface-artifact", { interfaceId: "world_operator_chat_home", label: "note" }, { submitted: true }],
  ]) {
    const before = loadsOf(world);
    const response = await call(route, { token, body });
    assert.equal(response.status, 200, `${route}: ${JSON.stringify(response.body)}`);
    if (expected) assert.deepEqual(response.body, expected);
    assert.equal(loadsOf(world), before + 1, `${route} loads the chat client through the package loader`);
  }
  assert.deepEqual(world.events.filter(([name]) => name === "read-payload").map(([, input]) => input.blobsRoot), [blobs, blobs, blobs]);
  assert.equal(world.required.some((id) => String(id).includes("world_operator_chat")), false, "no route requires the installed chat client directly");
  assert.equal((await call("/__octopus/chat-pins", { token: "stale", body: {} })).status, 401);
});

test("dev interface artifacts consult the chat client only for chat interfaces, and invalid chat artifact batches fail closed", async () => {
  const { world, call } = loadDevServer();
  const token = await signIn(call);
  world.events.length = 0;
  assert.equal((await call("/__octopus/interface-artifact", { token, body: { interfaceId: "world_telegram", label: "note" } })).status, 200);
  assert.deepEqual(world.events.map(([name]) => name), ["artifact-submitted"]);
  world.events.length = 0;
  const refused = await call("/__octopus/interface-artifact", { token, body: { interfaceId: "world_operator_chat_home", label: "forbidden" } });
  assert.deepEqual([refused.status, refused.body], [400, { error: "operator chat refuses this artifact" }]);
  assert.deepEqual(world.events.map(([name]) => name), ["chat-client-load", "chat-artifact-check"]);
  world.chatBatch = () => ({ artifacts: [{ from_interface_id: "world_provider_codex", from_interface_role: "" }], cursor: { offset: 0, seq: null } });
  const invalid = await call("/__octopus/operator-chat-artifacts", { token, body: { runId: "run-1" } });
  assert.equal(invalid.status, 400);
  assert.match(invalid.body.error, /from_interface_role/);
});

test("dev server fails closed at startup when node:sqlite is unavailable", () => {
  assert.throws(
    () => loadDevServer({ "node:sqlite": () => { throw new Error("No such built-in module: node:sqlite"); } }),
    /requires Node\.js with built-in node:sqlite/,
  );
});
