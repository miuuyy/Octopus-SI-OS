const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const descriptor = require("../interface.json");
const [, owner, file] = descriptor.interface_driver_config.private_config.client_connection.module.match(/^interface:\/\/([^/]+)\/(.+)$/);
const connection = require(path.resolve(__dirname, "../..", owner, file));

const CONNECTOR = descriptor.interface_driver_config.private_config.connector_link.connector_id;
const ACCESS_TOKEN_PREFIX = "e30.";
const LINK = "link_69ffe0001";
const PROFILE = "gmail.get_profile";
const MANAGE = { label: "Manage in ChatGPT", url: `https://chatgpt.com/apps/gmail/${CONNECTOR}` };
const directories = [];
test.after(() => directories.forEach((directory) => fs.rmSync(directory, { recursive: true, force: true })));

function token(claims) {
  return `${ACCESS_TOKEN_PREFIX}${Buffer.from(JSON.stringify(claims)).toString("base64url")}.signature`;
}

function linkedTools(explicit) {
  if (!explicit) {
    const meta = { connector_id: CONNECTOR, link_id: LINK };
    return [
      { name: "gmail.search_emails", description: "Search email", inputSchema: { type: "object", properties: { query: { type: "string" } } }, _meta: meta },
      { name: PROFILE, inputSchema: { type: "object" }, annotations: { readOnlyHint: true }, _meta: meta },
    ];
  }
  const meta = { connector_id: CONNECTOR, link_accounts: explicit.map((link_id) => ({ link_id })), _codex_apps: { requires_explicit_link_id: true } };
  const inputSchema = { type: "object", properties: { link_id: { type: "string" } }, required: ["link_id"] };
  return [
    { name: "gmail.search_emails", description: "Search email", inputSchema, _meta: meta },
    { name: PROFILE, inputSchema, annotations: { readOnlyHint: true }, _meta: meta },
  ];
}

function harness({ authDocument, linked = true, explicit = null, profiles = { [LINK]: "owner@gmail.example" }, unauthorized = false } = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "gmail-connection-"));
  directories.push(directory);
  const authFile = path.join(directory, "auth.json");
  if (authDocument) fs.writeFileSync(authFile, JSON.stringify(authDocument));
  const state = { linked, unauthorized, requests: [], opened: [], progress: [], published: [] };
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
    publishOperations: async (operations, generated) => {
      assert.ok(operations.every(generated));
      state.published.push(operations);
      return { published: operations.length, skipped: [] };
    },
    fetch: async (url, init) => {
      const body = JSON.parse(init.body);
      state.requests.push({ url, method: body.method, params: body.params, headers: init.headers });
      if (state.unauthorized) return new Response("", { status: 401 });
      if (body.id === undefined) return new Response(null, { status: 202 });
      if (body.method === "tools/call") {
        const email = profiles[body.params.arguments.link_id ?? body.params._meta.link_id];
        if (body.params.name !== PROFILE || !email) return Response.json({ jsonrpc: "2.0", id: body.id, error: { code: -32000, message: "unavailable" } });
        return Response.json({ jsonrpc: "2.0", id: body.id, result: { content: [], structuredContent: { id: "profile-1", name: "Owner", email } } });
      }
      const result = body.method === "initialize"
        ? { protocolVersion: "2025-11-25", capabilities: { tools: {} }, serverInfo: { name: "plugin-runtime", version: "1" } }
        : { tools: [
          ...(state.linked ? linkedTools(explicit)
            : [{ name: "gmail.batch_read_email", _meta: { connector_id: CONNECTOR, _codex_apps: { synthetic_link: true } } }]),
          { name: "slack.search", _meta: { connector_id: "connector_other" } },
        ] };
      return Response.json({ jsonrpc: "2.0", id: body.id, result });
    },
  };
  return { ctx, state, authFile, directory };
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
const calls = (state) => state.requests.filter((request) => request.method === "tools/call");

test("probe reads the linked mailbox through the profile tool, names the ChatGPT account apart and writes nothing", async () => {
  const absent = harness();
  assert.deepEqual(await connection.probe(absent.ctx), { state: "not_connected", message: "Sign in to ChatGPT first." });
  assert.equal(absent.state.requests.length, 0);

  const expired = harness({ authDocument: signedIn(-60) });
  assert.deepEqual(await connection.probe(expired.ctx), {
    state: "expired",
    account: { label: "ChatGPT account owner@chatgpt.example" },
    manage: MANAGE,
    message: "ChatGPT sign-in expired — sign in again in Octopus.",
  });
  assert.equal(expired.state.requests.length, 0);

  const linked = harness({ authDocument: signedIn() });
  const connected = await connection.probe(linked.ctx);
  assert.deepEqual(connected, {
    state: "connected",
    account: { label: "owner@gmail.example", detail: "Through ChatGPT account owner@chatgpt.example" },
    manage: MANAGE,
  });
  assert.deepEqual(linked.state.requests.map((request) => request.method), ["initialize", "notifications/initialized", "tools/list", "tools/call"]);
  assert.deepEqual(calls(linked.state)[0].params, { name: PROFILE, arguments: {}, _meta: { connector_id: CONNECTOR, link_id: LINK } });
  const authDocument = JSON.parse(fs.readFileSync(linked.authFile, "utf8"));
  for (const request of linked.state.requests) {
    assert.equal(request.url, "https://chatgpt.com/backend-api/ps/mcp");
    assert.equal(request.headers.authorization, `Bearer ${authDocument.tokens.access_token}`);
    assert.equal(request.headers["ChatGPT-Account-ID"], "account-1");
  }
  assert.ok(!JSON.stringify(connected).includes(ACCESS_TOKEN_PREFIX));
  assert.deepEqual(fs.readdirSync(linked.directory), ["auth.json"]);

  const unread = harness({ authDocument: signedIn(), profiles: {} });
  const unknown = await connection.probe(unread.ctx);
  assert.deepEqual(unknown.account, { label: "Gmail linked in ChatGPT", detail: "Through ChatGPT account owner@chatgpt.example" });
  assert.match(unknown.message, /could not read which mailbox/);

  const rejected = harness({ authDocument: signedIn(), unauthorized: true });
  assert.equal((await connection.probe(rejected.ctx)).state, "expired");
});

test("synthetic link tools are not a connection; Connect links Gmail, remembers its mailbox, publishes real tools and Disconnect stays honest", async () => {
  const { ctx, state, directory } = harness({ authDocument: signedIn(), linked: false });
  assert.deepEqual(await connection.probe(ctx), {
    state: "not_connected",
    account: { label: "ChatGPT account owner@chatgpt.example" },
    manage: MANAGE,
    message: "Gmail is not linked to this ChatGPT account.",
  });
  assert.equal(calls(state).length, 0);
  const connecting = connection.connect(ctx);
  await new Promise((resolve) => setTimeout(resolve, 200));
  assert.deepEqual(state.opened, [MANAGE.url]);
  assert.equal(state.progress[0].code, "connector_link_pending");
  state.linked = true;
  const connected = await connecting;
  assert.equal(connected.state, "connected");
  assert.equal(connected.account.label, "owner@gmail.example");
  assert.deepEqual(state.published[0].map((operation) => operation.name), ["gmail.search_emails", PROFILE]);
  assert.equal(fs.statSync(path.join(directory, "connection-accounts.json")).mode & 0o777, 0o600);

  const before = calls(state).length;
  assert.equal((await connection.probe(ctx)).account.label, "owner@gmail.example");
  assert.equal(calls(state).length, before);

  const disconnected = await connection.disconnect(ctx);
  assert.equal(disconnected.state, "connected");
  assert.deepEqual(disconnected.manage, MANAGE);
  assert.match(disconnected.message, /cannot unlink Gmail .* stays linked/);
  assert.equal(state.opened.length, 1);
});

test("Connect publishes the tools and says so when the mailbox can be neither read nor kept", async () => {
  const { ctx, state, directory } = harness({ authDocument: signedIn(), profiles: {} });
  ctx.privateDirectory = path.join(directory, "missing");
  const connected = await connection.connect(ctx);
  assert.equal(connected.state, "connected");
  assert.equal(connected.account.label, "Gmail linked in ChatGPT");
  assert.equal(state.published.length, 1);
  assert.match(connected.message, /could not read which mailbox it uses\. 2 tools are available to connected tasks\. Octopus could not keep the Gmail mailbox list/);
});

test("several linked accounts are read through the link choice their tool schema asks for", async () => {
  const { ctx, state } = harness({ authDocument: signedIn(), explicit: ["link_a", "link_b"], profiles: { link_a: "a@gmail.example", link_b: "b@gmail.example" } });
  const connected = await connection.connect(ctx);
  assert.equal(connected.account.label, "a@gmail.example, b@gmail.example");
  assert.deepEqual(calls(state).map((request) => request.params.arguments), [{ link_id: "link_a" }, { link_id: "link_b" }]);
  assert.deepEqual(state.opened, []);
  assert.match(state.published[0][0].description, /Linked accounts, by link_id: link_a \(a@gmail\.example\), link_b \(b@gmail\.example\)\.$/);
});
