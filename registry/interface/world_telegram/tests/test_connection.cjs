"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const test = require("node:test");

const ROOT = path.resolve(__dirname, "..");
const connection = require(path.join(ROOT, "packages", "connection.cjs"));
const descriptor = JSON.parse(fs.readFileSync(path.join(ROOT, "interface.json"), "utf8"));
const application = JSON.parse(fs.readFileSync(path.join(ROOT, "application.json"), "utf8"));
const TOKEN = `123456789:${"A".repeat(35)}`;
const BOT = { id: 999, is_bot: true, first_name: "Octo", username: "octo_bot" };
const ANN = { id: 4242, is_bot: false, first_name: "Ann" };

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

function telegram(script) {
  const calls = [];
  async function fetch(url, init) {
    const [, token, method] = /\/bot([^/]+)\/(\w+)$/.exec(url);
    const params = JSON.parse(init.body);
    calls.push({ method, params, token });
    let answer = script[method] ?? true;
    if (typeof answer === "function") answer = await answer(params, calls);
    const reply = answer?.status ? answer : { status: 200, body: { ok: true, result: answer } };
    return { status: reply.status, text: async () => JSON.stringify(reply.body) };
  }
  return { fetch, calls };
}

function context(api, { values = {}, input = {}, launch = {} } = {}) {
  const saved = [];
  const progress = [];
  const ctx = {
    descriptor, interfaceId: descriptor.id, values, input, privateDirectory: "/unused", signal: undefined, fetch: api.fetch,
    saveValues: async (map) => { saved.push(map); },
    launchEnvironment: async (name) => launch[name],
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

test("probe reports missing, rejected, unpaired and connected bots", async () => {
  assert.equal((await connection.probe(context(telegram({})).ctx)).state, "not_connected");
  const revoked = telegram({ getMe: { status: 401, body: { ok: false, error_code: 401, description: "Unauthorized" } } });
  assert.equal((await connection.probe(context(revoked, { values: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx)).state, "expired");
  const unpaired = await connection.probe(context(telegram({ getMe: BOT }), { values: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx);
  assert.deepEqual([unpaired.state, unpaired.account.label], ["not_connected", "@octo_bot"]);
  const paired = await connection.probe(context(telegram({ getMe: BOT }), { values: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242" } }).ctx);
  assert.deepEqual(paired, { state: "connected", account: { label: "@octo_bot", detail: "Allowed user 4242" } });
});

test("pairing captures the sender of /start, confirms only that update and greets the user", async () => {
  let session;
  const api = telegram({
    getMe: BOT,
    getWebhookInfo: { url: "https://old.example/hook" },
    getUpdates: (params) => (params.offset ? [] : [{ update_id: 70, ...start(session.progress) }]),
    sendMessage: { message_id: 4 },
  });
  session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN } });
  const state = await connection.connect(session.ctx);
  assert.deepEqual(state.account, { label: "@octo_bot", detail: "Paired with Ann" });
  assert.equal(state.state, "connected");
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
  const api = telegram({ getMe: BOT, getWebhookInfo: { url: "" }, getUpdates: () => [earlier, { update_id: 70, ...start(session.progress) }] });
  session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "", TELEGRAM_HOME_CHAT: "-100500" } });
  assert.equal((await connection.connect(session.ctx)).state, "connected");
  assert.ok(api.calls.every((call) => call.method !== "getUpdates" || call.params.offset === undefined));
  assert.deepEqual(session.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242", TELEGRAM_HOME_CHAT: "-100500" }]);
});

test("a new bot pairs again, and a polling conflict asks for the user ID instead and saves nothing", async () => {
  const api = telegram({ getMe: BOT, getWebhookInfo: { url: "" }, getUpdates: { status: 409, body: { ok: false, error_code: 409, description: "Conflict: terminated by other getUpdates request" } } });
  const session = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN }, values: { TELEGRAM_BOT_TOKEN: `555:${"B".repeat(35)}`, TELEGRAM_ALLOWED_USERS: "4242" } });
  const state = await connection.connect(session.ctx);
  assert.equal(state.state, "error");
  assert.match(state.message, /Allowed user IDs/);
  assert.ok(state.message.length <= 300);
  assert.deepEqual(session.saved, []);
});

test("typed allowed users skip pairing, bad input fails clearly and disconnect clears the settings", async () => {
  const api = telegram({ getMe: BOT });
  const typed = context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242, 5151" } });
  assert.equal((await connection.connect(typed.ctx)).state, "connected");
  assert.deepEqual(typed.saved, [{ TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "4242,5151" }]);
  assert.ok(api.calls.every((call) => call.method === "getMe"));
  const wrongUsers = await connection.connect(context(api, { input: { TELEGRAM_BOT_TOKEN: TOKEN, TELEGRAM_ALLOWED_USERS: "@ann" } }).ctx);
  assert.match(wrongUsers.message, /numeric Telegram user IDs/);
  const wrongToken = await connection.connect(context(api, { input: { TELEGRAM_BOT_TOKEN: "hello" } }).ctx);
  assert.equal(wrongToken.state, "error");
  const rejected = telegram({ getMe: { status: 404, body: { ok: false, error_code: 404, description: "Not Found" } } });
  assert.match((await connection.connect(context(rejected, { input: { TELEGRAM_BOT_TOKEN: TOKEN } }).ctx)).message, /rejected this token/);
  const cleared = context(api);
  assert.equal((await connection.disconnect(cleared.ctx)).state, "not_connected");
  assert.deepEqual(cleared.saved, [{ TELEGRAM_BOT_TOKEN: null, TELEGRAM_ALLOWED_USERS: null, TELEGRAM_HOME_CHAT: null }]);
});

test("the operation catalog declares one precise tool per Telegram action", () => {
  assertOperationCatalog(ROOT, "telegram", 22);
});
