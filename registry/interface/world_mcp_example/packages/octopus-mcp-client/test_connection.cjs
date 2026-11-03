const assert = require("node:assert/strict");
const http = require("node:http");
const connection = require("./connection.cjs");

function descriptor(transport, profile) {
  return {
    id: "world_mcp_test",
    interface_driver_config: {
      private_config: {
        protocol_versions: ["2025-11-25"], client_name: "octopus-mcp-client", client_version: "1",
        ...(profile ? { account_profile: profile } : {}),
      },
      transport: { wire: "mcp_streamable_http", ...transport },
    },
  };
}

function context(transport, values = {}, input = {}, profile = undefined) {
  const saved = [];
  return {
    saved,
    ctx: { descriptor: descriptor(transport, profile), values, input, saveValues: async (map) => saved.push(map), progress() {}, signal: undefined },
  };
}

async function server(handler) {
  const instance = http.createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => handler(request, body ? JSON.parse(body) : null, response));
  });
  await new Promise((resolve) => instance.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${instance.address().port}/mcp`,
    close: () => new Promise((resolve) => {
      instance.close(resolve);
      instance.closeAllConnections();
    }),
  };
}

(async () => {
  const seen = [];
  const mcp = await server((request, message, response) => {
    seen.push({ method: message?.method ?? request.method, auth: request.headers.authorization, session: request.headers["mcp-session-id"] });
    if (request.headers.authorization !== "Bearer good") {
      response.writeHead(401).end();
    } else if (request.method === "DELETE") {
      response.writeHead(405).end();
    } else if (message.method === "initialize") {
      response.writeHead(200, { "content-type": "application/json", "mcp-session-id": "s1" });
      response.end(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2025-11-25", capabilities: { tools: {} }, serverInfo: { name: "fake", version: "2.0" } } }));
    } else if (message.method === "notifications/initialized") {
      response.writeHead(202).end();
    } else {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { tools: [{ name: "a" }, { name: "b" }] } })}\n\n`);
    }
  });
  try {
    const transport = { base_url: mcp.url, auth_env_key: "KEY" };
    const ok = context(transport, {}, { KEY: "good" });
    const connected = await connection.connect(ok.ctx);
    assert.equal(connected.state, "connected");
    assert.equal(connected.account.label, "fake 2.0");
    assert.match(connected.account.detail, /^2 tools · 127\.0\.0\.1:/);
    assert.deepEqual(ok.saved, [{ KEY: "good" }]);
    assert.deepEqual(seen.map((entry) => entry.method), ["initialize", "notifications/initialized", "tools/list", "DELETE"]);
    assert.equal(seen[2].session, "s1");
    assert.deepEqual(seen[3], { method: "DELETE", auth: "Bearer good", session: "s1" });

    const rejected = context(transport, { KEY: "bad" });
    assert.equal((await connection.probe(rejected.ctx)).state, "expired");
    assert.equal(rejected.saved.length, 0);

    const missing = context(transport);
    assert.deepEqual(await connection.probe(missing.ctx), { state: "not_connected", message: "Enter the key." });

    const remotePlain = context({ base_url: "http://192.0.2.10/mcp" });
    assert.equal((await connection.probe(remotePlain.ctx)).state, "error");

    const cleared = context({ base_url_env: "URL", auth_env_key: "KEY" }, { URL: mcp.url, KEY: "good" });
    assert.deepEqual(await connection.disconnect(cleared.ctx), { state: "not_connected" });
    assert.deepEqual(cleared.saved, [{ KEY: null, URL: null }]);
  } finally {
    await mcp.close();
  }

  const editors = [];
  const calls = [];
  const accounts = await server((request, message, response) => {
    calls.push(message?.method ?? request.method);
    if (request.method === "GET") {
      response.writeHead(request.headers.authorization === "Bearer good" ? 200 : 401, { "content-type": "application/json" });
      response.end(JSON.stringify({ success: true }));
    } else if (message.method === "notifications/initialized") {
      response.writeHead(202).end();
    } else {
      const result = message.method === "initialize"
        ? { protocolVersion: "2025-11-25", capabilities: { tools: {}, resources: {} }, serverInfo: { name: "fake" } }
        : message.method === "tools/list" ? { tools: [{ name: "a" }] }
          : { contents: [{ uri: message.params.uri, text: JSON.stringify({ instances: editors }) }] };
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
    }
  });
  try {
    const resource = { resource: "test://instances", accounts: "/instances", name: "/name", via: "the editor bridge", absent: "No editor is connected." };
    const empty = await connection.probe(context({ base_url: accounts.url }, {}, {}, resource).ctx);
    assert.deepEqual(empty, { state: "not_connected", message: "No editor is connected." });
    assert.deepEqual(calls, ["initialize", "notifications/initialized", "resources/read"]);
    editors.push({ name: "Game" }, { name: "Game" }, { name: "Tools" });
    assert.deepEqual((await connection.probe(context({ base_url: accounts.url }, {}, {}, resource).ctx)).account, { label: "Game, Tools", detail: "Through the editor bridge" });

    const manage = { label: "Manage keys", url: "https://keys.example/" };
    const keyed = { request: accounts.url.replace(/\/mcp$/, "/account"), label: "Your team", via: "your key", manage };
    const verified = await connection.probe(context({ base_url: accounts.url, auth_env_key: "KEY" }, { KEY: "good" }, {}, keyed).ctx);
    assert.deepEqual(verified, { state: "connected", account: { label: "Your team", detail: "Through your key" }, manage });
    const refused = await connection.probe(context({ base_url: accounts.url, auth_env_key: "KEY" }, { KEY: "stale" }, {}, keyed).ctx);
    assert.equal(refused.state, "expired");
    assert.deepEqual(refused.manage, manage);
  } finally {
    await accounts.close();
  }

  const stalled = await server((request, message, response) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.write(": open\n\n");
  });
  try {
    const slow = await connection.probe(context({ base_url: stalled.url, request_timeout_ms: 200 }).ctx);
    assert.deepEqual(slow, { state: "error", message: `${new URL(stalled.url).host} did not answer within 1 second.` });
  } finally {
    await stalled.close();
  }

  const idle = await server(() => {});
  await idle.close();
  const waiting = context({ base_url: idle.url });
  const controller = new AbortController();
  waiting.ctx.signal = controller.signal;
  const messages = [];
  waiting.ctx.progress = (update) => {
    messages.push(update.message);
    controller.abort(new Error("cancelled"));
  };
  await assert.rejects(connection.connect(waiting.ctx), /cancelled/);
  assert.match(messages[0], /^Waiting for the server at http:\/\/127\.0\.0\.1:/);
  console.log("mcp client connection: ok");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
