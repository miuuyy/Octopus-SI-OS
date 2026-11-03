const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const family = require("./chatgpt_apps.cjs");

const interfaces = path.resolve(__dirname, "../../..");
const directories = [];
test.after(() => directories.forEach((directory) => fs.rmSync(directory, { recursive: true, force: true })));

const applications = fs.readdirSync(interfaces).flatMap((id) => {
  const file = path.join(interfaces, id, "interface.json");
  const descriptor = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : null;
  return descriptor?.interface_driver_ref === "mcp_client"
    && descriptor.interface_driver_config?.private_config?.auth?.mode === "chatgpt_account" ? [descriptor] : [];
});

function token(claims) {
  return `e30.${Buffer.from(JSON.stringify(claims)).toString("base64url")}.signature`;
}

function harness(descriptor, { profileValue, readOnly = true } = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "chatgpt-apps-"));
  directories.push(directory);
  const authFile = path.join(directory, "auth.json");
  fs.writeFileSync(authFile, JSON.stringify({ auth_mode: "chatgpt", tokens: {
    access_token: token({ exp: Math.floor(Date.now() / 1000) + 3600 }), account_id: "account-1", id_token: token({ email: "owner@chatgpt.example" }),
  } }));
  const link = descriptor.interface_driver_config.private_config.connector_link;
  const field = link.account_profile.email_field ?? link.account_profile.login_field;
  const meta = { connector_id: link.connector_id, link_id: "link_1" };
  const tools = [
    { name: link.account_profile.tool, inputSchema: { type: "object" }, annotations: { readOnlyHint: readOnly }, _meta: meta },
    { name: `${link.account_profile.tool}_change`, inputSchema: { type: "object" }, annotations: { readOnlyHint: false }, _meta: meta },
    { name: "other.search", inputSchema: { type: "object" }, _meta: { connector_id: "connector_other", link_id: "link_2" } },
  ];
  const state = { calls: [], published: [] };
  const ctx = {
    descriptor,
    privateDirectory: directory,
    launchEnvironment: (name) => (name === descriptor.interface_driver_config.private_config.auth.auth_file_env ? authFile : undefined),
    openExternal: async () => assert.fail("a linked connector must not open ChatGPT"),
    progress() {},
    signal: new AbortController().signal,
    publishOperations: async (operations, generated) => {
      assert.ok(operations.every(generated));
      state.published.push(operations.map((operation) => operation.name));
      return { published: operations.length, skipped: [] };
    },
    fetch: async (url, init) => {
      const body = JSON.parse(init.body);
      if (body.id === undefined) return new Response(null, { status: 202 });
      if (body.method === "tools/call") {
        state.calls.push(body.params.name);
        return Response.json({ jsonrpc: "2.0", id: body.id, result: { content: [], structuredContent: { [field]: profileValue } } });
      }
      const result = body.method === "initialize"
        ? { protocolVersion: descriptor.interface_driver_config.private_config.protocol_versions[0], capabilities: { tools: {} }, serverInfo: { name: "plugin-runtime" } }
        : { tools };
      return Response.json({ jsonrpc: "2.0", id: body.id, result });
    },
  };
  return { ctx, state, link };
}

function withProfile(descriptor, profile) {
  const changed = structuredClone(descriptor);
  changed.interface_driver_config.private_config.connector_link.account_profile = profile;
  return changed;
}

test("every ChatGPT account application binds the family and shows the account its declared profile field names", async () => {
  assert.ok(applications.some((descriptor) => descriptor.id === "world_gmail"));
  for (const descriptor of applications) {
    const declared = descriptor.interface_driver_config.private_config.client_connection.module;
    assert.equal(declared, "interface://world_mcp_example/packages/octopus-mcp-client/chatgpt_apps.cjs", descriptor.id);
    assert.equal(require(path.join(interfaces, ...declared.slice("interface://".length).split("/"))), family);
    const login = descriptor.interface_driver_config.private_config.connector_link.account_profile.login_field !== undefined;
    const account = login ? "octo-owner" : "owner@example.com";
    const { ctx, state, link } = harness(descriptor, { profileValue: account });
    const manage = { label: "Manage in ChatGPT", url: `https://chatgpt.com/apps/${link.slug}/${link.connector_id}` };
    assert.deepEqual(await family.probe(ctx), { state: "connected", account: { label: account, detail: "Through ChatGPT account owner@chatgpt.example" }, manage });
    assert.deepEqual(state.calls, [link.account_profile.tool]);
    assert.equal((await family.connect(ctx)).account.label, account);
    assert.deepEqual(state.published, [[link.account_profile.tool, `${link.account_profile.tool}_change`]]);
  }
});

test("an account the profile does not prove is never shown, and an unreadable profile declaration fails closed", async () => {
  const github = applications.find((descriptor) => descriptor.id === "world_mcp_github");
  const { tool, noun } = github.interface_driver_config.private_config.connector_link.account_profile;
  const spaced = harness(github, { profileValue: "octo owner" });
  const unread = await family.probe(spaced.ctx);
  assert.equal(unread.account.label, "GitHub linked in ChatGPT");
  assert.equal(unread.message, `GitHub is linked in ChatGPT, but Octopus could not read which ${noun} it uses.`);
  const writable = harness(github, { profileValue: "octo-owner", readOnly: false });
  assert.equal((await family.probe(writable.ctx)).account.label, "GitHub linked in ChatGPT");
  assert.deepEqual(writable.state.calls, []);
  for (const profile of [{ tool, noun, login_field: "login", email_field: "email" }, { tool, login_field: "login" }, { tool, noun, name_field: "name" }]) {
    const broken = harness(withProfile(github, profile), { profileValue: "octo-owner" });
    const state = await family.probe(broken.ctx);
    assert.equal(state.state, "error");
    assert.match(state.message, /connection settings are invalid: its connector link declares no account profile tool/);
  }
});
