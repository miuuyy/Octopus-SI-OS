"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const http = require("node:http");
const { createRequire } = require("node:module");
const net = require("node:net");
const path = require("node:path");
const test = require("node:test");

const ROOT = path.resolve(__dirname, "..");
const connection = require(path.join(ROOT, "packages", "connection.cjs"));
const descriptor = JSON.parse(fs.readFileSync(path.join(ROOT, "interface.json"), "utf8"));
const application = JSON.parse(fs.readFileSync(path.join(ROOT, "application.json"), "utf8"));
const TOKEN = `123456789:${"A".repeat(35)}`;
const BOT = { id: 999, is_bot: true, first_name: "Octo", username: "octo_bot" };
const ANN = { id: 4242, is_bot: false, first_name: "Ann" };
const MANAGE = { label: "Manage in BotFather", url: "https://t.me/BotFather" };

function assertOperationCatalog(root, prefix, count) {
  const Ajv = createRequire(path.resolve(root, "../../../app/package.json"))("ajv");
  const ajv = new Ajv({ strict: true, strictTypes: true, allErrors: true });
  const config = JSON.parse(fs.readFileSync(path.join(root, "interface.json"), "utf8")).interface_driver_config.private_config;
  assert.deepEqual([config.operation_catalog_dir, config.operation_catalog_label, config.operation_request_label], ["operations", `${prefix}.operation_catalog`, `${prefix}.operation.request`]);
  const files = fs.readdirSync(path.join(root, "operations")).sort();
  assert.equal(files.length, count);
  for (const file of files) {
    const operation = JSON.parse(fs.readFileSync(path.join(root, "operations", file), "utf8"));
    assert.deepEqual(Object.keys(operation).sort(), ["description", "handler_config", "id", "input_schema", "output_label", "request_label"]);
    assert.equal(`${operation.id}.json`, file);
    assert.match(operation.id, new RegExp(`^${prefix}\\.[a-z_]+$`));
    assert.deepEqual([operation.request_label, operation.output_label], [`${prefix}.operation.request`, `${prefix}.operation.result`]);
    assert.deepEqual([operation.input_schema.type, operation.input_schema.additionalProperties], ["object", false]);
    ajv.compile(operation.input_schema);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, "application.json"), "utf8")).task_capabilities, undefined);
  const contract = JSON.parse(fs.readFileSync(path.resolve(root, `../../artifact/artifact-application-${prefix}-context/artifact.json`), "utf8")).text;
  assert.doesNotMatch(contract, /artifact\.send|delivery\.result|\.failure|\.operation\b/);
}

function listen(server) {
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => {
    server.unref();
    resolve(server.address().port);
  }));
}

async function telegram(script) {
  const calls = [];
  const server = http.createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", async () => {
      const [, token, method] = /\/bot([^/]+)\/(\w+)$/.exec(request.url);
      const params = JSON.parse(body);
      calls.push({ method, params, token });
      let answer = script[method] ?? true;
      if (typeof answer === "function") answer = await answer(params, calls);
      const reply = answer?.status ? answer : { status: 200, body: { ok: true, result: answer } };
      response.writeHead(reply.status, { "content-type": "application/json" });
      response.end(JSON.stringify(reply.body));
    });
  });
  const port = await listen(server);
  return { base: `http://127.0.0.1:${port}`, authority: `127.0.0.1:${port}`, calls };
}

async function connectProxy() {
  const tunnels = [];
  const server = net.createServer((client) => {
    client.on("error", () => {});
    client.once("data", (chunk) => {
      const [, host, port] = /^CONNECT ([^:\s]+):(\d+) HTTP\/1\.1\r\n/.exec(chunk.toString("latin1"));
      tunnels.push(`${host}:${port}`);
      const upstream = net.connect(Number(port), host, () => {
        client.write("HTTP/1.1 200 Connection established\r\n\r\n");
        client.pipe(upstream).pipe(client);
      });
      upstream.on("error", () => client.destroy());
    });
  });
  return { url: `http://127.0.0.1:${await listen(server)}`, tunnels };
}

function context(api, { values = {}, input = {}, launch = {} } = {}) {
  const saved = [];
  const progress = [];
  let current = { ...values };
  const environment = { OCTOPUS_TELEGRAM_API_BASE: api.base, ...launch };
  const ctx = {
    descriptor, interfaceId: descriptor.id, input, privateDirectory: "/unused", signal: undefined,
    get values() { return current; },
    saveValues: async (map) => {
      saved.push(map);
      current = Object.fromEntries(Object.entries({ ...current, ...map }).filter(([, value]) => value !== null));
    },
    launchEnvironment: (name) => environment[name],
    openExternal: async () => {},
    progress: (event) => progress.push(event),
  };
  return { ctx, saved, progress };
}

function start(progress, text) {
  const code = progress.find((event) => event.code)?.code;
  return { message: { message_id: 3, date: 1730000000, chat: { id: ANN.id, type: "private" }, from: ANN, text: text ?? `/start ${code}` } };
}

test("the declared module, setup fields and disclosure satisfy the client contract", () => {
  assert.equal(connection.schemaVersion, 1);
  assert.equal(descriptor.interface_driver_config.private_config.client_connection.module, "interface://world_telegram/packages/connection.cjs");
  for (const name of ["probe", "connect", "disconnect"]) assert.equal(typeof connection[name], "function");
  assert.ok(application.connection.disclosure.length <= 4);
  assert.ok(application.connection.disclosure.every((line) => line.length <= 160));
  assert.equal(application.connection.help.url, "https://t.me/BotFather");
  assert.deepEqual(application.setup.fields.map((field) => [field.kind, field.required]), [["secret", true], ["text", false], ["text", false]]);
});

test("probe reaches the Bot API like the interface and reports missing, rejected, unpaired and connected bots", async () => {
  assert.equal((await connection.probe(context(await telegram({})).ctx)).state, "not_connected");
  const revoked = await telegram({ getMe: { status: 401, body: { ok: false, error_code: 401, description: "Unauthorized" } } });
  assert.deepEqual(await connection.probe(context(revoked, { values: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx), {
    state: "expired", manage: MANAGE, message: "Telegram no longer accepts this bot token. Paste a new token from @BotFather.",
  });
  const unpaired = await connection.probe(context(await telegram({ getMe: BOT }), { values: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx);
  assert.deepEqual([unpaired.state, unpaired.account.label], ["not_connected", "@octo_bot"]);
  const api = await telegram({ getMe: BOT });
  const paired = await connection.probe(context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242" } }).ctx);
  assert.deepEqual(paired, { state: "connected", account: { label: "@octo_bot", detail: `Through the Bot API server at ${api.authority}, for allowed user 4242` }, manage: MANAGE });
  assert.deepEqual(api.calls.map((call) => [call.method, call.token]), [["getMe", TOKEN]]);
  const proxy = await connectProxy();
  const proxied = await connection.probe(context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { OCTOPUS_TELEGRAM_PROXY: proxy.url, TELEGRAM_ALLOW_ALL_USERS: "yes" } }).ctx);
  assert.deepEqual(proxied.account, { label: "@octo_bot", detail: `Through the Bot API server at ${api.authority} via a proxy, for every Telegram user` });
  assert.deepEqual(proxy.tunnels, [api.authority]);
  const split = await connection.probe(context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { TELEGRAM_BOT_TOKEN: `555:${"B".repeat(35)}` } }).ctx);
  assert.equal(split.state, "error");
  assert.match(split.message, /^TELEGRAM_BOT_TOKEN is set in this Mac's environment and saved in Octopus with a different value/);
  assert.equal(api.calls.length, 2);
});

test("pairing captures the sender of /start, confirms only that update and greets the user", async () => {
  let session;
  const api = await telegram({
    getMe: BOT,
    getWebhookInfo: { url: "https://old.example/hook" },
    getUpdates: (params) => (params.offset ? [] : [{ update_id: 70, ...start(session.progress) }]),
    sendMessage: { message_id: 4 },
  });
  session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN } });
  const state = await connection.connect(session.ctx);
  assert.deepEqual(state.account, { label: "@octo_bot", detail: `Through the Bot API server at ${api.authority}, paired with Ann` });
  assert.deepEqual([state.state, state.manage], ["connected", MANAGE]);
  const link = session.progress.find((event) => event.url);
  assert.equal(link.url, `https://t.me/octo_bot?start=${link.code}`);
  assert.match(link.code, /^[2-9A-HJKMNP-Z]{5}-[2-9A-HJKMNP-Z]{5}$/);
  assert.deepEqual(api.calls.find((call) => call.method === "deleteWebhook").params, { drop_pending_updates: false });
  assert.deepEqual(api.calls.filter((call) => call.method === "getUpdates").map((call) => call.params.offset), [undefined, 71]);
  assert.deepEqual(session.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242", TELEGRAM_HOME_CHAT: "4242" }]);
  assert.equal(api.calls.at(-1).method, "sendMessage");
  assert.equal(api.calls.at(-1).params.chat_id, 4242);
});

test("pairing never consumes updates that arrived before Start", async () => {
  let session;
  const earlier = { update_id: 69, message: { message_id: 2, chat: { id: 77, type: "private" }, from: { id: 77, first_name: "Bob" }, text: "hello" } };
  const api = await telegram({ getMe: BOT, getWebhookInfo: { url: "" }, getUpdates: () => [earlier, { update_id: 70, ...start(session.progress) }] });
  session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "", TELEGRAM_HOME_CHAT: "-100500" } });
  assert.equal((await connection.connect(session.ctx)).state, "connected");
  assert.ok(api.calls.every((call) => call.method !== "getUpdates" || call.params.offset === undefined));
  assert.deepEqual(session.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242", TELEGRAM_HOME_CHAT: "-100500" }]);
});

test("the environment is checked before Telegram is called, and pairing leaves the environment's home chat to it", async () => {
  let session;
  const api = await telegram({ getMe: BOT, getWebhookInfo: { url: "" }, getUpdates: () => [{ update_id: 70, ...start(session.progress) }], sendMessage: { message_id: 4 } });
  const split = await connection.connect(context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { TELEGRAM_BOT_TOKEN: `555:${"B".repeat(35)}` } }).ctx);
  assert.match(split.message, /^TELEGRAM_BOT_TOKEN is set in this Mac's environment to a different value/);
  const reserved = await connection.probe(context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { OCTOPUS_TELEGRAM_FALLBACK_IPS: "100.64.0.1, 203.0.113.5" } }).ctx);
  assert.deepEqual(reserved, { state: "error", message: "OCTOPUS_TELEGRAM_FALLBACK_IPS must list public IPv4 addresses." });
  assert.equal(api.calls.length, 0);
  session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { TELEGRAM_HOME_CHAT: "-100500" } });
  assert.equal((await connection.connect(session.ctx)).state, "connected");
  assert.deepEqual(session.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242" }]);
});

test("a new bot pairs again, and a polling conflict asks for the user ID instead and saves nothing", async () => {
  const api = await telegram({ getMe: BOT, getWebhookInfo: { url: "" }, getUpdates: { status: 409, body: { ok: false, error_code: 409, description: "Conflict: terminated by other getUpdates request" } } });
  const session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN }, values: { TELEGRAM_BOT_TOKEN: `555:${"B".repeat(35)}`, TELEGRAM_ALLOWED_USERS: "4242" } });
  const state = await connection.connect(session.ctx);
  assert.equal(state.state, "error");
  assert.match(state.message, /Allowed user IDs/);
  assert.ok(state.message.length <= 300);
  assert.deepEqual(session.saved, []);
});

test("typed allowed users skip pairing, bad input fails clearly and disconnect says what it removed and what remains", async () => {
  const api = await telegram({ getMe: BOT });
  const typed = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242, 5151" } });
  assert.equal((await connection.connect(typed.ctx)).account.detail, `Through the Bot API server at ${api.authority}, for 2 allowed users`);
  assert.deepEqual(typed.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242,5151" }]);
  assert.ok(api.calls.every((call) => call.method === "getMe"));
  const wrongUsers = await connection.connect(context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "@ann" } }).ctx);
  assert.match(wrongUsers.message, /numeric Telegram user IDs/);
  const wrongToken = await connection.connect(context(api, { input: { TELEGRAM_BOT_TOKEN: "hello" } }).ctx);
  assert.equal(wrongToken.state, "error");
  const rejected = await telegram({ getMe: { status: 404, body: { ok: false, error_code: 404, description: "Not Found" } } });
  assert.match((await connection.connect(context(rejected, { input: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx)).message, /rejected this token/);
  const cleared = context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242" } });
  const gone = await connection.disconnect(cleared.ctx);
  assert.deepEqual([gone.state, gone.manage], ["not_connected", MANAGE]);
  assert.match(gone.message, /^Telegram is disconnected: Octopus removed the bot token and allowed users from this Mac\./);
  assert.deepEqual(cleared.saved, [{ TELEGRAM_BOT_TOKEN: null, TELEGRAM_ALLOWED_USERS: null, TELEGRAM_HOME_CHAT: null }]);
  const ambient = context(api, { values: { TELEGRAM_BOT_TOKEN: TOKEN }, launch: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242" } });
  const kept = await connection.disconnect(ambient.ctx);
  assert.equal(kept.state, "connected");
  assert.match(kept.message, /TELEGRAM_BOT_TOKEN is set in this Mac's environment, so the Telegram interface keeps using that bot/);
});

test("the operation catalog declares one precise tool per Telegram action", () => {
  assertOperationCatalog(ROOT, "telegram", 22);
});
