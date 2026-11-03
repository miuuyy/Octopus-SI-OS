const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const connection = require("./connection.cjs");
const descriptor = require(path.join(__dirname, "..", "..", "interface.json"));

const MANAGE = { label: "Manage in ChatGPT", url: "https://chatgpt.com/settings/usage" };
const directory = fs.mkdtempSync(path.join(os.tmpdir(), "image-connection-"));

function token(claims) {
  return `e30.${Buffer.from(JSON.stringify(claims)).toString("base64url")}.signature`;
}

function signIn(name, expiresIn, email = "owner@example.com") {
  const file = path.join(directory, `${name}.json`);
  const exp = Math.floor(Date.now() / 1000) + expiresIn;
  fs.writeFileSync(file, JSON.stringify({ auth_mode: "chatgpt", tokens: { access_token: token({ exp }), account_id: "account-1", id_token: token(email ? { email } : {}) } }));
  return file;
}

function context({ authFile, status = 200, body = {}, headers = {}, input = {}, target = descriptor }) {
  const requests = [];
  const saved = [];
  const fetch = async (url, init) => {
    requests.push({ url: String(url), headers: init.headers });
    return Response.json(body, { status, headers });
  };
  const launchEnvironment = (name) => (name === "OCTOPUS_MACHINE_CODEX_AUTH_FILE" ? authFile : undefined);
  return { requests, saved, ctx: { descriptor: target, values: {}, input, launchEnvironment, saveValues: async (map) => saved.push(map), fetch, progress() {} } };
}

(async () => {
  const usage = { email: "owner@example.com", account_id: "account-1", plan_type: "plus" };
  const paid = context({ authFile: signIn("paid", 3600), body: usage });
  assert.deepEqual(await connection.connect(paid.ctx), { state: "connected", account: { label: "owner@example.com", detail: "ChatGPT Plus plan" }, manage: MANAGE });
  assert.equal(paid.requests[0].url, "https://chatgpt.com/backend-api/wham/usage");
  assert.match(paid.requests[0].headers.authorization, /^Bearer e30\./);
  assert.equal(paid.requests[0].headers["ChatGPT-Account-ID"], "account-1");
  assert.equal(paid.requests[0].headers.originator, "octopus_desktop");

  const free = context({ authFile: signIn("free", 3600), body: { ...usage, plan_type: "free" } });
  const unpaid = await connection.probe(free.ctx);
  assert.deepEqual([unpaid.state, unpaid.account.detail], ["not_connected", "ChatGPT Free plan"]);
  assert.match(unpaid.message, /paid ChatGPT plan/);
  const lite = await connection.probe(context({ authFile: signIn("lite", 3600), body: { ...usage, plan_type: "prolite" } }).ctx);
  assert.deepEqual([lite.state, lite.account.detail], ["connected", "ChatGPT Pro Lite plan"]);
  const nameless = await connection.probe(context({ authFile: signIn("nameless", 3600, null), body: { account_id: "account-1", plan_type: "plus" } }).ctx);
  assert.deepEqual([nameless.state, nameless.account], ["error", undefined]);

  const lapsed = context({ authFile: signIn("lapsed", -60) });
  assert.equal((await connection.probe(lapsed.ctx)).state, "expired");
  assert.equal(lapsed.requests.length, 0);
  assert.equal((await connection.probe(context({ authFile: signIn("revoked", 3600), status: 401 }).ctx)).state, "expired");
  assert.equal((await connection.probe(context({ authFile: path.join(directory, "absent.json") }).ctx)).state, "not_connected");

  const keyed = structuredClone(descriptor);
  delete keyed.interface_driver_config.private_config.auth;
  Object.assign(keyed.interface_driver_config.transport, {
    base_url: "https://api.openai.com/v1/images/generations",
    base_url_env: "OCTOPUS_IMAGE_GENERATION_URL",
    auth_env_key: "OCTOPUS_IMAGE_GENERATION_API_KEY",
    auth_header: "Authorization",
  });
  const accepted = context({ target: keyed, headers: { "openai-organization": "octopus-lab" }, input: { OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-test" } });
  assert.deepEqual(await connection.connect(accepted.ctx), { state: "connected", account: { label: "octopus-lab", detail: "API key accepted by api.openai.com" } });
  assert.deepEqual([accepted.requests[0].url, accepted.requests[0].headers.Authorization], ["https://api.openai.com/v1/models", "Bearer sk-test"]);
  assert.equal(accepted.requests[0].headers.originator, "octopus_desktop");
  assert.deepEqual(accepted.saved, [{ OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-test" }]);
  const rejected = context({ target: keyed, status: 401, input: { OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-bad" } });
  assert.equal((await connection.connect(rejected.ctx)).state, "expired");
  assert.equal(rejected.saved.length, 0);
  const plain = context({ target: keyed, input: { OCTOPUS_IMAGE_GENERATION_API_KEY: "key", OCTOPUS_IMAGE_GENERATION_URL: "http://images.example/v1/images/generations" } });
  assert.deepEqual(await connection.connect(plain.ctx), { state: "error", message: "The API URL must use https." });
  console.log("image generation connection: ok");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
}).finally(() => fs.rmSync(directory, { recursive: true, force: true }));
