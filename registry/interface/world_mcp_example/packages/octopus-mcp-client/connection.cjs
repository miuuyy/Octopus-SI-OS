const REQUEST_TIMEOUT_MS = 10_000;
const MAX_RESPONSE_CHARS = 4 * 1024 * 1024;
const LOOPBACK_WAIT_MS = 120_000;
const LOOPBACK_RETRY_MS = 2_000;

class ConnectionFailure extends Error {
  constructor(state, message) {
    super(message);
    this.state = state;
  }
}

function transportOf(descriptor) {
  const transport = descriptor?.interface_driver_config?.transport;
  if (transport?.wire !== "mcp_streamable_http") {
    throw new Error(`Interface '${descriptor?.id}' is not a streamable HTTP MCP interface.`);
  }
  return transport;
}

function declaredName(value) {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function filled(map) {
  return Object.fromEntries(Object.entries(map ?? {})
    .filter(([, value]) => typeof value === "string" && value.trim())
    .map(([name, value]) => [name, value.trim()]));
}

function endpointOf(transport, values) {
  const direct = typeof transport.base_url === "string" ? transport.base_url.trim() : "";
  const envName = declaredName(transport.base_url_env);
  if (direct && envName) throw new Error("The interface declares both base_url and base_url_env.");
  const raw = direct || (envName ? values[envName] : "");
  if (!raw) return null;
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new ConnectionFailure("error", "The server URL is not a valid URL.");
  }
  const loopback = url.hostname === "localhost" || url.hostname === "[::1]" || /^127\./.test(url.hostname);
  if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) {
    throw new ConnectionFailure("error", "Use an https URL, or http only for a server on this Mac.");
  }
  return { url: url.toString(), loopback };
}

function signalFor(ctx) {
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  return ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout;
}

function parseRpcResponse(contentType, text, id) {
  if (text.length > MAX_RESPONSE_CHARS) throw new ConnectionFailure("error", "The server response is too large.");
  const messages = [];
  if (contentType.includes("text/event-stream")) {
    for (const event of text.split(/\r?\n\r?\n/)) {
      const data = event.split(/\r?\n/).filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trimStart()).join("\n");
      if (data) messages.push(JSON.parse(data));
    }
  } else {
    messages.push(JSON.parse(text));
  }
  const response = messages.flat().find((message) => message?.id === id);
  if (!response) throw new ConnectionFailure("error", "The server did not answer the MCP request.");
  if (response.error) {
    throw new ConnectionFailure("error", `The server refused the request: ${String(response.error.message ?? "unknown error").slice(0, 200)}`);
  }
  return response.result;
}

async function rpc(ctx, endpoint, token, session, id, method, params) {
  const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" };
  if (token) headers.authorization = `Bearer ${token}`;
  if (session.id) headers["mcp-session-id"] = session.id;
  if (session.protocol) headers["mcp-protocol-version"] = session.protocol;
  const body = id === null ? { jsonrpc: "2.0", method, params } : { jsonrpc: "2.0", id, method, params };
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(endpoint.url, { method: "POST", headers, body: JSON.stringify(body), signal: signalFor(ctx), redirect: "error" });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    throw new ConnectionFailure(endpoint.loopback ? "not_connected" : "error", endpoint.loopback
      ? `Nothing answers at ${endpoint.url}. Start the server and try again.`
      : `Could not reach ${new URL(endpoint.url).host}.`);
  }
  if (response.status === 401 || response.status === 403) {
    throw new ConnectionFailure("expired", "The server rejected the key. Check it and connect again.");
  }
  if (!response.ok && !(id === null && response.status === 202)) {
    throw new ConnectionFailure("error", `The server answered HTTP ${response.status}.`);
  }
  session.id = response.headers.get("mcp-session-id") ?? session.id;
  if (id === null) return null;
  return parseRpcResponse(response.headers.get("content-type") ?? "", await response.text(), id);
}

async function check(ctx, values) {
  const transport = transportOf(ctx.descriptor);
  const endpoint = endpointOf(transport, values);
  if (!endpoint) return { state: "not_connected", message: "Enter the server URL." };
  const tokenName = declaredName(transport.auth_env_key);
  const token = tokenName ? values[tokenName] : null;
  if (tokenName && !token) return { state: "not_connected", message: "Enter the key." };
  const config = ctx.descriptor.interface_driver_config.private_config ?? {};
  const protocols = Array.isArray(config.protocol_versions) ? config.protocol_versions : [];
  if (!protocols.length) throw new Error(`Interface '${ctx.descriptor.id}' declares no MCP protocol versions.`);
  const session = {};
  const initialized = await rpc(ctx, endpoint, token, session, 1, "initialize", {
    protocolVersion: protocols[0],
    capabilities: {},
    clientInfo: { name: config.client_name ?? "octopus-mcp-client", version: String(config.client_version ?? "1") },
  });
  if (!protocols.includes(initialized?.protocolVersion)) {
    return { state: "error", message: `The server speaks MCP ${initialized?.protocolVersion ?? "of an unknown version"}, which this interface does not declare.` };
  }
  session.protocol = initialized.protocolVersion;
  await rpc(ctx, endpoint, token, session, null, "notifications/initialized", {});
  const listed = initialized.capabilities?.tools ? await rpc(ctx, endpoint, token, session, 2, "tools/list", {}) : { tools: [] };
  const count = Array.isArray(listed?.tools) ? listed.tools.length : 0;
  const server = initialized.serverInfo ?? {};
  const name = [server.title ?? server.name, server.version].filter((part) => typeof part === "string" && part).join(" ");
  return {
    state: "connected",
    account: {
      label: (name || new URL(endpoint.url).host).slice(0, 96),
      detail: `${count}${listed?.nextCursor ? "+" : ""} tool${count === 1 ? "" : "s"} · ${new URL(endpoint.url).host}`.slice(0, 160),
    },
  };
}

async function settle(work) {
  try {
    return await work();
  } catch (error) {
    if (error instanceof ConnectionFailure) return { state: error.state, message: error.message };
    throw error;
  }
}

async function probe(ctx) {
  return settle(() => check(ctx, filled(ctx.values)));
}

async function connect(ctx) {
  const input = filled(ctx.input);
  const values = { ...filled(ctx.values), ...input };
  let endpoint = null;
  try {
    endpoint = endpointOf(transportOf(ctx.descriptor), values);
  } catch (error) {
    if (!(error instanceof ConnectionFailure)) throw error;
  }
  const deadline = Date.now() + LOOPBACK_WAIT_MS;
  for (;;) {
    const result = await settle(() => check(ctx, values));
    if (result.state === "connected") {
      if (Object.keys(input).length) await ctx.saveValues(input);
      return result;
    }
    if (result.state !== "not_connected" || !endpoint?.loopback || Date.now() >= deadline) return result;
    ctx.progress({ message: `Waiting for the server at ${endpoint.url}…` });
    await new Promise((resolve, reject) => {
      const timer = setTimeout(resolve, LOOPBACK_RETRY_MS);
      ctx.signal?.addEventListener("abort", () => {
        clearTimeout(timer);
        reject(ctx.signal.reason);
      }, { once: true });
    });
  }
}

async function disconnect(ctx) {
  const transport = transportOf(ctx.descriptor);
  const names = [declaredName(transport.auth_env_key), declaredName(transport.base_url_env)].filter(Boolean);
  if (!names.length) return probe(ctx);
  await ctx.saveValues(Object.fromEntries(names.map((name) => [name, null])));
  return { state: "not_connected" };
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
