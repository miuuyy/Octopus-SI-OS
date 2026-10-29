const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const connection = require("../packages/connection.cjs");
const descriptor = require("../interface.json");

const CONNECTOR = descriptor.interface_driver_config.private_config.connector_link.connector_id;
const ACCESS_TOKEN_PREFIX = "e30.";
const directories = [];
test.after(() => directories.forEach((directory) => fs.rmSync(directory, { recursive: true, force: true })));

function token(claims) {
  return `${ACCESS_TOKEN_PREFIX}${Buffer.from(JSON.stringify(claims)).toString("base64url")}.signature`;
}

function harness({ authDocument, linked = true, unauthorized = false } = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "gmail-connection-"));
  directories.push(directory);
  const authFile = path.join(directory, "auth.json");
  if (authDocument) fs.writeFileSync(authFile, JSON.stringify(authDocument));
  const state = { linked, unauthorized, requests: [], opened: [], progress: [] };
  const ctx = {
    descriptor,
    interfaceId: descriptor.id,
    values: {},
    input: {},
    saveValues: async () => assert.fail("connection must not save values"),
    privateDirectory: directory,
    launchEnvironment: (name) => (name === "OCTOPUS_MACHINE_CODEX_AUTH_FILE" ? authFile : undefined),
    openExternal: async (url) => { state.opened.push(url); },
    progress: (update) => { state.progress.push(update); },
    signal: new AbortController().signal,
    fetch: async (url, init) => {
      const body = JSON.parse(init.body);
      state.requests.push({ url, method: body.method, headers: init.headers });
      if (state.unauthorized) return new Response("", { status: 401 });
      if (body.id === undefined) return new Response(null, { status: 202 });
      const result = body.method === "initialize"
        ? { protocolVersion: "2025-11-25", capabilities: { tools: {} }, serverInfo: { name: "plugin-runtime", version: "1" } }
        : { tools: [
          ...(state.linked ? [{ name: "gmail.search_emails", _meta: { connector_id: CONNECTOR,
            link_accounts: [{ link_id: "link_1", profile_email: "owner@gmail.example" }] } }] : []),
          { name: "slack.search", _meta: { connector_id: "connector_other" } },
        ] };
      return Response.json({ jsonrpc: "2.0", id: body.id, result });
    },
  };
  return { ctx, state, authFile };
}

const signedIn = (expiresIn = 3600) => ({
  auth_mode: "chatgpt",
  tokens: {
    access_token: token({ exp: Math.floor(Date.now() / 1000) + expiresIn }),
    account_id: "account-1",
    id_token: token({ email: "owner@chatgpt.example" }),
    refresh_token: "refresh-never-used",
  },
});

test("probe reports sign-in, expiry and connector presence without leaking credentials", async () => {
  const absent = harness();
  assert.deepEqual(await connection.probe(absent.ctx), { state: "not_connected", message: "Sign in to ChatGPT first." });
  assert.equal(absent.state.requests.length, 0);

  const expired = harness({ authDocument: signedIn(-60) });
  assert.deepEqual(await connection.probe(expired.ctx), {
    state: "expired",
    account: { label: "owner@chatgpt.example" },
    message: "ChatGPT sign-in expired — sign in again in Octopus.",
  });
  assert.equal(expired.state.requests.length, 0);

  const linked = harness({ authDocument: signedIn() });
  const connected = await connection.probe(linked.ctx);
  assert.deepEqual(connected, {
    state: "connected",
    account: { label: "owner@chatgpt.example", detail: "Gmail: owner@gmail.example" },
  });
  assert.deepEqual(linked.state.requests.map((request) => request.method), ["initialize", "notifications/initialized", "tools/list"]);
  const authDocument = JSON.parse(fs.readFileSync(linked.authFile, "utf8"));
  for (const request of linked.state.requests) {
    assert.equal(request.url, "https://chatgpt.com/backend-api/ps/mcp");
    assert.equal(request.headers.authorization, `Bearer ${authDocument.tokens.access_token}`);
    assert.equal(request.headers["ChatGPT-Account-ID"], "account-1");
  }
  assert.ok(!JSON.stringify(connected).includes(ACCESS_TOKEN_PREFIX));

  const rejected = harness({ authDocument: signedIn(), unauthorized: true });
  assert.equal((await connection.probe(rejected.ctx)).state, "expired");
});

test("connect opens the ChatGPT app page and waits until the connector is linked", async () => {
  const { ctx, state } = harness({ authDocument: signedIn(), linked: false });
  assert.equal((await connection.probe(ctx)).state, "not_connected");
  const connecting = connection.connect(ctx);
  await new Promise((resolve) => setTimeout(resolve, 200));
  assert.deepEqual(state.opened, [`https://chatgpt.com/apps/gmail/${CONNECTOR}`]);
  assert.equal(state.progress[0].code, "connector_link_pending");
  state.linked = true;
  assert.equal((await connecting).state, "connected");

  const disconnected = await connection.disconnect(ctx);
  assert.equal(disconnected.state, "connected");
  assert.match(disconnected.message, /ChatGPT Settings/);
  assert.equal(state.opened.length, 1);
});
