const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const connection = require("../packages/connection.cjs");
const descriptor = require(path.join(__dirname, "..", "interface.json"));

const BOT = "xoxb-1111111111-2222222222-abcdefghijkl";
const APP = "xapp-1-A111-2222222222-abcdef";
const USER = "U0123ABCDE9";
const TEAM = "T0123ABCDE9";

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

function context(input, replies, { values = {}, launch = {} } = {}) {
  const calls = [];
  const saved = [];
  let current = { ...values };
  const fetch = async (url, init) => {
    const method = url.split("/").pop();
    calls.push({ method, token: init.headers.authorization });
    const body = typeof replies[method] === "function" ? replies[method](init.headers.authorization) : replies[method];
    return { status: 200, text: async () => JSON.stringify(body) };
  };
  const ctx = {
    descriptor, input, fetch, progress() {},
    get values() { return current; },
    saveValues: async (map) => {
      saved.push(map);
      current = Object.fromEntries(Object.entries({ ...current, ...map }).filter(([, value]) => value !== null));
    },
    launchEnvironment: (name) => launch[name],
  };
  return { calls, saved, ctx };
}

(async () => {
  const replies = {
    "auth.test": { ok: true, team: "Octopus Lab", team_id: TEAM, user: "octopus", user_id: "U0BOT00001" },
    "apps.connections.open": { ok: true, url: "wss://wss-primary.slack.com/link/?ticket=1&app_id=A0123ABCDEF" },
  };
  const good = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, replies);
  const result = await connection.connect(good.ctx);
  assert.deepEqual(result, {
    state: "connected",
    account: { label: "@octopus in Octopus Lab", detail: `Through Socket Mode, for member ${USER}` },
    manage: { label: "Manage in Slack", url: "https://api.slack.com/apps/A0123ABCDEF" },
  });
  assert.deepEqual(good.calls, [{ method: "auth.test", token: `Bearer ${BOT}` }, { method: "apps.connections.open", token: `Bearer ${APP}` }]);
  assert.deepEqual(good.saved, [{ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }]);
  assert.deepEqual(await connection.probe(good.ctx), result);

  const swapped = context({ SLACK_BOT_TOKEN: APP, SLACK_APP_TOKEN: BOT, SLACK_ALLOWED_USERS: USER }, {});
  assert.deepEqual(await connection.connect(swapped.ctx), { state: "error", message: "The bot token must start with xoxb-." });
  assert.equal(swapped.calls.length, 0);

  const revoked = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, { "auth.test": { ok: false, error: "token_revoked" } });
  assert.deepEqual(await connection.connect(revoked.ctx), { state: "expired", message: "The bot token was revoked. Reinstall the app and copy the new token." });
  assert.equal(revoked.saved.length, 0);

  const twice = context({ SLACK_BOT_TOKEN: `${BOT},${BOT}0`, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, replies);
  assert.match((await connection.connect(twice.ctx)).message, /same workspace/);
  assert.equal(twice.saved.length, 0);

  const everyone = context({}, replies, { values: { SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP }, launch: { SLACK_ALLOW_ALL_USERS: "true", SLACK_ALLOWED_TEAMS: "T0OTHER0001" } });
  const outside = await connection.probe(everyone.ctx);
  assert.deepEqual([outside.state, outside.account.detail], ["error", "Through Socket Mode, for every member"]);
  assert.match(outside.message, /^SLACK_ALLOWED_TEAMS does not include Octopus Lab/);

  const missing = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP }, {});
  assert.deepEqual(await connection.probe(missing.ctx), { state: "not_connected", message: "Enter the bot token." });
  assert.deepEqual(await connection.connect(missing.ctx), { state: "not_connected", message: "Enter the allowed member IDs." });

  const gone = await connection.disconnect(good.ctx);
  assert.deepEqual([gone.state, gone.manage], ["not_connected", { label: "Manage in Slack", url: "https://api.slack.com/apps" }]);
  assert.match(gone.message, /^Slack is disconnected: Octopus removed the tokens and member settings from this Mac\./);
  const ambient = context({}, replies, { values: { SLACK_BOT_TOKEN: BOT }, launch: { SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER } });
  const kept = await connection.disconnect(ambient.ctx);
  assert.equal(kept.state, "connected");
  assert.match(kept.message, /SLACK_BOT_TOKEN and SLACK_APP_TOKEN are set in this Mac's environment/);
  const half = context({}, replies, { values: { SLACK_BOT_TOKEN: BOT, SLACK_ALLOWED_USERS: USER }, launch: { SLACK_APP_TOKEN: APP } });
  const leftover = await connection.disconnect(half.ctx);
  assert.equal(leftover.state, "not_connected");
  assert.match(leftover.message, /SLACK_APP_TOKEN is set in this Mac's environment, but without the removed settings no Slack message reaches tasks/);
  const plain = context({}, replies, { values: { SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, launch: { OCTOPUS_SLACK_PROXY: "http://proxy.example.test:3128" } });
  assert.deepEqual(await connection.probe(plain.ctx), { state: "error", message: "OCTOPUS_SLACK_PROXY must use an HTTPS proxy for a proxy on another computer." });
  assert.equal(plain.calls.length, 0);
  const split = context({ SLACK_BOT_TOKEN: `${BOT}0` }, replies, { launch: { SLACK_BOT_TOKEN: BOT } });
  assert.match((await connection.connect(split.ctx)).message, /^SLACK_BOT_TOKEN is set in this Mac's environment to a different value/);
  assertOperationCatalog(path.resolve(__dirname, ".."), "slack", 33);
  console.log("slack connection: ok");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
