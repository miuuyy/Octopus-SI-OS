const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { after, beforeEach, test } = require("node:test");
const { createApplicationConnectionHost, createRegistryConnectionTargets } = require("../electron/applicationConnection.cjs");
const { parseApplicationManifest } = require("../electron/projection/sidecars.cjs");

const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-application-connection-"));
after(() => fs.rmSync(root, { recursive: true, force: true }));
const registry = path.join(root, "registry");
const home = path.join(root, "home");
fs.mkdirSync(path.join(home, "interfaces"), { recursive: true });
const opened = [];
const host = createApplicationConnectionHost({
  resolveTargets: createRegistryConnectionTargets({
    registryRoot: () => registry,
    octopusHome: () => home,
    launchEnvironment: () => ({ DEMO_REGION: "eu-west", HOME: "/Users/someone" }),
    openExternal: async (url) => { opened.push(url); },
  }),
  scope: () => home,
});
let sequence = 0;
const operationId = () => `operation-${String(++sequence).padStart(4, "0")}`;
const trampoline = "module.exports = { schemaVersion: 1, probe: (ctx) => globalThis.demoConnection.probe(ctx), connect: (ctx) => globalThis.demoConnection.connect(ctx) };\n";
const directModule = "interface://world_demo_mail/packages/connection.cjs";
const sharedModule = "interface://world_demo_owner/packages/demo-crate/connection.cjs";

function write(relative, value) {
  const file = path.join(registry, "interface", relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, typeof value === "string" ? value : `${JSON.stringify(value, null, 2)}\n`);
}
function manifest(id, extra = {}) {
  return { schema_version: 3, id, name: "Demo Mail", description: "Reads mail", category: "connector", developer: "Demo", version: "1.0.0", logo: "logo.svg", interfaces: [id], ...extra };
}
function descriptor(id, module) {
  return {
    id,
    interface_driver_config: {
      private_config: {
        environment: { address_env: "DEMO_ADDRESS", token_env: "DEMO_TOKEN" },
        ...(module ? { client_connection: { schema_version: 1, module } } : {}),
      },
    },
  };
}
function connectionInterface(id, module) {
  write(`${id}/interface.json`, descriptor(id, module));
  write(`${id}/life.json`, {
    processes: [{
      inherit_env: ["DEMO_REGION"],
      inherit_env_from_interface: ["/interface_driver_config/private_config/environment/address_env", "/interface_driver_config/private_config/environment/token_env"],
      authority: { read_env_paths: ["DEMO_ROOT"] },
    }],
  });
}
function savedValues(id) {
  const file = path.join(home, "interfaces", id, "environment.json");
  return fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")).values : null;
}

beforeEach(() => {
  fs.rmSync(registry, { recursive: true, force: true });
  fs.rmSync(path.join(home, "interfaces"), { recursive: true, force: true });
  fs.mkdirSync(path.join(home, "interfaces"));
  const field = { interface: "world_demo_mail", pointer: "/interface_driver_config/private_config/environment/address_env", label: "Email address", kind: "text", required: true };
  write("world_demo_mail/application.json", manifest("world_demo_mail", { setup: { fields: [field] } }));
  connectionInterface("world_demo_mail", directModule);
  write("world_demo_mail/packages/connection.cjs", trampoline);
  write("world_demo_owner/interface.json", descriptor("world_demo_owner", sharedModule));
  write("world_demo_owner/packages/demo-crate/connection.cjs", trampoline);
  write("world_demo_shared/application.json", manifest("world_demo_shared"));
  connectionInterface("world_demo_shared", sharedModule);
  globalThis.demoConnection = {
    probe: async (ctx) => ctx.values.DEMO_TOKEN ? { state: "connected", account: { label: ctx.values.DEMO_ADDRESS } } : { state: "not_connected" },
    connect: async (ctx) => {
      await ctx.saveValues({ DEMO_ADDRESS: ctx.input.DEMO_ADDRESS ?? "shared@example.com", DEMO_TOKEN: "token-1" });
      return { state: "connected" };
    },
  };
});

test("an application manifest carries connection disclosure, help and field hints, and refuses malformed presentation", () => {
  const field = { interface: "world_demo_mail", pointer: "/a", label: "Email address", kind: "text", required: true, placeholder: "name@example.com", help: "Use your sign-in address." };
  const connection = { disclosure: ["Reads mail only in tasks you connect."], help: { label: "About Demo Mail", url: "https://example.com/help" } };
  const parsed = parseApplicationManifest(manifest("world_demo_mail", { setup: { fields: [field] }, connection }), "world_demo_mail", "application.json");
  assert.deepEqual(parsed.connection, connection);
  assert.deepEqual(parsed.setup.fields[0], field);
  assert.deepEqual(parseApplicationManifest(manifest("world_demo_mail"), "world_demo_mail", "application.json").connection, { disclosure: [], help: null });
  for (const [label, extra] of [
    ["five disclosure statements", { connection: { disclosure: ["a", "b", "c", "d", "e"] } }],
    ["a help link that is not https", { connection: { help: { label: "Help", url: "http://example.com/" } } }],
    ["an unknown connection field", { connection: { scopes: [] } }],
    ["a placeholder over 96 characters", { setup: { fields: [{ ...field, placeholder: "x".repeat(97) }] } }],
    ["field help with a line break", { setup: { fields: [{ ...field, help: "one\ntwo" }] } }],
  ]) {
    assert.throws(() => parseApplicationManifest(manifest("world_demo_mail", extra), "world_demo_mail", "application.json"), Error, label);
  }
});

test("a declared module connects with live progress, saves only declared values and reads only declared launch environment", async () => {
  const seen = {};
  globalThis.demoConnection.connect = async (ctx) => {
    seen.input = { ...ctx.input };
    ctx.progress({ message: "Approve the sign-in request.", url: "https://example.com/device", code: "ABCD-1234" });
    await ctx.saveValues({ DEMO_ADDRESS: ctx.input.DEMO_ADDRESS, DEMO_TOKEN: "token-1" });
    seen.values = { ...ctx.values };
    await assert.rejects(ctx.saveValues({ OTHER_TOKEN: "x" }), /does not declare private value 'OTHER_TOKEN'/);
    seen.region = ctx.launchEnvironment("DEMO_REGION");
    seen.root = ctx.launchEnvironment("DEMO_ROOT");
    assert.throws(() => ctx.launchEnvironment("HOME"), /does not declare 'HOME'/);
    await assert.rejects(ctx.openExternal("http://example.com/"), /https/);
    await ctx.openExternal("https://example.com/approve");
    return { state: "connected", account: { label: ctx.input.DEMO_ADDRESS, detail: "Personal" } };
  };
  assert.deepEqual(await host.probe({ applicationId: "world_demo_mail" }), { applicationId: "world_demo_mail", interfaces: [{ interfaceId: "world_demo_mail", state: { state: "not_connected" } }] });
  const progress = [];
  const id = operationId();
  const state = await host.connect({ operationId: id, applicationId: "world_demo_mail", interfaceId: "world_demo_mail", input: { DEMO_ADDRESS: "me@example.com" } }, { onProgress: (update) => progress.push(update) });
  assert.deepEqual(state, { state: "connected", account: { label: "me@example.com", detail: "Personal" } });
  assert.deepEqual(progress, [{ operationId: id, applicationId: "world_demo_mail", interfaceId: "world_demo_mail", message: "Approve the sign-in request.", url: "https://example.com/device", code: "ABCD-1234" }]);
  assert.deepEqual(seen, { input: { DEMO_ADDRESS: "me@example.com" }, values: { DEMO_ADDRESS: "me@example.com", DEMO_TOKEN: "token-1" }, region: "eu-west", root: undefined });
  assert.deepEqual(opened, ["https://example.com/approve"]);
  assert.deepEqual(savedValues("world_demo_mail"), { DEMO_ADDRESS: "me@example.com", DEMO_TOKEN: "token-1" });
  assert.equal(fs.statSync(path.join(home, "interfaces", "world_demo_mail")).mode & 0o777, 0o700);
  assert.equal(fs.statSync(path.join(home, "interfaces", "world_demo_mail", "environment.json")).mode & 0o777, 0o600);
  assert.deepEqual((await host.probe({ applicationId: "world_demo_mail" })).interfaces[0].state, { state: "connected", account: { label: "me@example.com" } });
  await assert.rejects(host.connect({ operationId: operationId(), applicationId: "world_demo_mail", interfaceId: "world_demo_mail", input: { DEMO_TOKEN: "typed" } }), /'DEMO_TOKEN' is not a setup field/);
});

test("module answers outside the contract fail closed", async () => {
  const connect = (result) => {
    globalThis.demoConnection.connect = async (ctx) => typeof result === "function" ? result(ctx) : result;
    return host.connect({ operationId: operationId(), applicationId: "world_demo_mail", interfaceId: "world_demo_mail", input: {} });
  };
  await assert.rejects(connect({ state: "connected", token: "secret" }), /unknown field token/);
  await assert.rejects(connect({ state: "ready" }), /state must be connected/);
  await assert.rejects(connect({ state: "connected", account: { label: "x".repeat(97) } }), /account.label/);
  await assert.rejects(connect((ctx) => ctx.progress({ message: "Open", url: "http://example.com/" })), /https/);
  globalThis.demoConnection.probe = async () => { throw new Error("Service is offline"); };
  assert.deepEqual((await host.probe({ applicationId: "world_demo_mail" })).interfaces[0].state, { state: "error", message: "Service is offline" });
  write("world_demo_mail/interface.json", descriptor("world_demo_mail", "interface://world_demo_mail/packages/../life.cjs"));
  assert.match((await host.probe({ applicationId: "world_demo_mail" })).interfaces[0].state.message, /invalid client connection declaration/);
  fs.rmSync(path.join(registry, "interface", "world_demo_mail", "packages", "connection.cjs"));
  fs.symlinkSync(path.join(registry, "interface", "world_demo_owner", "packages", "demo-crate", "connection.cjs"), path.join(registry, "interface", "world_demo_mail", "packages", "connection.cjs"));
  write("world_demo_mail/interface.json", descriptor("world_demo_mail", directModule));
  assert.match((await host.probe({ applicationId: "world_demo_mail" })).interfaces[0].state.message, /must be a regular registry file/);
});

test("cancelling a waiting attempt stops it, and only one attempt per interface runs at a time", async () => {
  globalThis.demoConnection.connect = (ctx) => new Promise((resolve) => ctx.signal.addEventListener("abort", () => resolve({ state: "error", message: "late" })));
  const id = operationId();
  const waiting = host.connect({ operationId: id, applicationId: "world_demo_mail", interfaceId: "world_demo_mail", input: {} });
  await new Promise((resolve) => setTimeout(resolve, 20));
  await assert.rejects(host.connect({ operationId: operationId(), applicationId: "world_demo_mail", interfaceId: "world_demo_mail", input: {} }), /already in progress/);
  await assert.rejects(host.disconnect({ applicationId: "world_demo_mail", interfaceId: "world_demo_mail" }), /Finish or cancel/);
  assert.deepEqual(host.cancel({ operationId: id }), { cancelled: true });
  await assert.rejects(waiting, (error) => error.code === "cancelled");
  assert.deepEqual(host.cancel({ operationId: id }), { cancelled: false });
});

test("a crate module is shared only when its owner declares it identically, and disconnecting without an export clears saved values", async () => {
  await host.connect({ operationId: operationId(), applicationId: "world_demo_shared", interfaceId: "world_demo_shared", input: {} });
  assert.deepEqual(savedValues("world_demo_shared"), { DEMO_ADDRESS: "shared@example.com", DEMO_TOKEN: "token-1" });
  assert.deepEqual(await host.disconnect({ applicationId: "world_demo_shared", interfaceId: "world_demo_shared" }), { state: "not_connected" });
  assert.equal(savedValues("world_demo_shared"), null);
  write("world_demo_owner/interface.json", descriptor("world_demo_owner", "interface://world_demo_owner/packages/demo-crate/other.cjs"));
  assert.match((await host.probe({ applicationId: "world_demo_shared" })).interfaces[0].state.message, /is not declared by its installed owner/);
  write("world_demo_owner/interface.json", descriptor("world_demo_owner", null));
  await assert.rejects(host.connect({ operationId: operationId(), applicationId: "world_demo_shared", interfaceId: "world_demo_shared", input: {} }), /is not declared by its installed owner/);
});

test("connecting publishes server tools as operations of the declaring interface, replacing only earlier generated ones", async () => {
  const id = "world_demo_mail";
  const declared = descriptor(id, directModule);
  Object.assign(declared.interface_driver_config.private_config, {
    operation_catalog_dir: "operations",
    operation_catalog_label: "mcp.operation_catalog",
    operation_request_label: "mcp.operation.request",
    server_tool_operations: { prefix: "demo" },
  });
  write(`${id}/interface.json`, declared);
  const callTool = { id: "demo.call_tool", request_label: "mcp.operation.request", output_label: "mcp.tool.result",
    handler_config: { operation: "call_tool" }, description: "Call a tool", input_schema: { type: "object" } };
  write(`${id}/operations/demo.call_tool.json`, callTool);
  write(`${id}/operations/demo.retired.json`, { id: "demo.retired", handler_config: { operation: "call_tool", mcp_tool_name: "retired" } });
  const schema = { type: "object", properties: { query: { type: "string" } } };
  const published = [];
  globalThis.demoConnection.connect = async (ctx) => {
    published.push(await ctx.publishOperations([
      { name: "search", description: "Search mail", inputSchema: schema },
      { name: "demo.read", title: "Read mail", inputSchema: schema },
      { name: "call_tool", inputSchema: schema },
      { name: "bad/name", inputSchema: schema },
    ]));
    return { state: "connected" };
  };
  await host.connect({ operationId: operationId(), applicationId: id, interfaceId: id, input: { DEMO_ADDRESS: "me@example.com" } });
  assert.deepEqual(published[0].published, 2);
  assert.deepEqual(published[0].skipped, ["call_tool: it would replace an operation of the application", "bad/name: its name cannot be an operation id"]);
  const catalog = path.join(registry, "interface", id, "operations");
  assert.deepEqual(fs.readdirSync(catalog).sort(), ["demo.call_tool.json", "demo.read.json", "demo.search.json"]);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(catalog, "demo.search.json"), "utf8")), {
    id: "demo.search",
    request_label: "mcp.operation.request",
    output_label: "mcp.tool.result",
    handler_config: { operation: "call_tool", mcp_tool_name: "search" },
    description: "Search mail",
    input_schema: schema,
  });
  const read = JSON.parse(fs.readFileSync(path.join(catalog, "demo.read.json"), "utf8"));
  assert.equal(read.description, "Read mail");
  const artifact = JSON.parse(fs.readFileSync(path.join(registry, "artifact", `artifact-operation-catalog-${id}`, "artifact.json"), "utf8"));
  assert.deepEqual(artifact, {
    id: `artifact-operation-catalog-${id}`, label: "mcp.operation_catalog", interface: id, request_label: "mcp.operation.request",
    operations: [callTool, read, JSON.parse(fs.readFileSync(path.join(catalog, "demo.search.json"), "utf8"))].map((operation) => ({
      name: operation.id, description: operation.description, input_schema: operation.input_schema,
      request_label: operation.request_label, output_label: operation.output_label,
    })),
  });
  declared.interface_driver_config.private_config.task_input_labels = ["demo.event"];
  delete declared.interface_driver_config.private_config.server_tool_operations;
  write(`${id}/interface.json`, declared);
  fs.rmSync(path.join(catalog, "demo.read.json"));
  globalThis.demoConnection.connect = async (ctx) => ({ state: typeof ctx.publishOperations === "function" ? "error" : "connected" });
  await host.connect({ operationId: operationId(), applicationId: id, interfaceId: id, input: {} });
  const connector = JSON.parse(fs.readFileSync(path.join(registry, "artifact", `artifact-operation-catalog-${id}`, "artifact.json"), "utf8"));
  assert.deepEqual(connector.task_input_labels, ["demo.event"]);
  assert.deepEqual(connector.operations.map((operation) => operation.name), ["demo.call_tool", "demo.search"]);
  globalThis.demoConnection.probe = async (ctx) => ({ state: typeof ctx.publishOperations === "function" ? "error" : "not_connected" });
  assert.equal((await host.probe({ applicationId: id })).interfaces[0].state.state, "not_connected");
});
