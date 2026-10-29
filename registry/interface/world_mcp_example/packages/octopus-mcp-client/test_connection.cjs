const assert = require("node:assert/strict");
const http = require("node:http");
const connection = require("./connection.cjs");

function descriptor(transport) {
  return {
    id: "world_mcp_test",
    interface_driver_config: {
      private_config: { protocol_versions: ["2025-11-25"], client_name: "octopus-mcp-client", client_version: "1" },
      transport: { wire: "mcp_streamable_http", ...transport },
    },
  };
}

function context(transport, values = {}, input = {}) {
  const saved = [];
  return {
    saved,
    ctx: { descriptor: descriptor(transport), values, input, saveValues: async (map) => saved.push(map), progress() {}, signal: undefined },
  };
}

async function server(handler) {
  const instance = http.createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => handler(request, body ? JSON.parse(body) : null, response));
  });
  await new Promise((resolve) => instance.listen(0, "127.0.0.1", resolve));
  return { url: `http://127.0.0.1:${instance.address().port}/mcp`, close: () => new Promise((resolve) => instance.close(resolve)) };
}

(async () => {
  const seen = [];
  const mcp = await server((request, message, response) => {
    seen.push({ method: message.method, auth: request.headers.authorization, session: request.headers["mcp-session-id"] });
    if (request.headers.authorization !== "Bearer good") {
      response.writeHead(401).end();
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
    assert.deepEqual(seen.map((entry) => entry.method), ["initialize", "notifications/initialized", "tools/list"]);
    assert.equal(seen[2].session, "s1");

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
