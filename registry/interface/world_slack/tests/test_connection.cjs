const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const connection = require("../packages/connection.cjs");
const descriptor = require(path.join(__dirname, "..", "interface.json"));

const BOT = "xoxb-1111111111-2222222222-abcdefghijkl";
const APP = "xapp-1-A111-2222222222-abcdef";
const USER = "U0123ABCDE9";

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

function context(input, replies) {
  const calls = [];
  const saved = [];
  const fetch = async (url, init) => {
    const method = url.split("/").pop();
    calls.push({ method, token: init.headers.authorization });
    const body = replies[method];
    return { ok: true, status: 200, json: async () => body };
  };
  return { calls, saved, ctx: { descriptor, values: {}, input, saveValues: async (map) => saved.push(map), fetch, progress() {} } };
}

(async () => {
  const good = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, {
    "auth.test": { ok: true, team: "Octopus Lab", user: "octopus" },
    "apps.connections.open": { ok: true, url: "wss://example.invalid" },
  });
  const result = await connection.connect(good.ctx);
  assert.deepEqual(result, { state: "connected", account: { label: "Octopus Lab", detail: "Bot @octopus" } });
  assert.deepEqual(good.calls, [{ method: "auth.test", token: `Bearer ${BOT}` }, { method: "apps.connections.open", token: `Bearer ${APP}` }]);
  assert.deepEqual(good.saved, [{ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }]);

  const swapped = context({ SLACK_BOT_TOKEN: APP, SLACK_APP_TOKEN: BOT, SLACK_ALLOWED_USERS: USER }, {});
  assert.deepEqual(await connection.connect(swapped.ctx), { state: "error", message: "The bot token must start with xoxb-." });
  assert.equal(swapped.calls.length, 0);

  const revoked = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP, SLACK_ALLOWED_USERS: USER }, { "auth.test": { ok: false, error: "token_revoked" } });
  const refused = await connection.connect(revoked.ctx);
  assert.equal(refused.state, "expired");
  assert.equal(revoked.saved.length, 0);

  const missing = context({ SLACK_BOT_TOKEN: BOT, SLACK_APP_TOKEN: APP }, {});
  assert.deepEqual(await connection.probe(missing.ctx), { state: "not_connected", message: "Enter the bot token." });
  assert.deepEqual(await connection.connect(missing.ctx), { state: "not_connected", message: "Enter the allowed member IDs." });
  assertOperationCatalog(path.resolve(__dirname, ".."), "slack", 34);
  console.log("slack connection: ok");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
