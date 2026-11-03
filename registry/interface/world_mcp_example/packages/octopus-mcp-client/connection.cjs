const { setTimeout: delay } = require("node:timers/promises");
const { publishServerTools } = require("./server_operations.cjs");

const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;
const MAX_REQUEST_TIMEOUT_MS = 600_000;
const MAX_RESPONSE_CHARS = 4 * 1024 * 1024;
const LOOPBACK_WAIT_MS = 10 * 60_000;
const LOOPBACK_RETRY_MS = 2_000;
const CLOSE_TIMEOUT_MS = 5_000;
const TOOL_PAGES = 16;
const PROFILE_FIELDS = new Set(["resource", "request", "accounts", "name", "label", "via", "absent", "manage"]);
const POINTER = /^(?:\/(?:[^~/]|~[01])*)*$/;

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

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function clip(value, limit) {
  return value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;
}

function filled(map) {
  return Object.fromEntries(Object.entries(map ?? {})
    .filter(([, value]) => typeof value === "string" && value.trim())
    .map(([name, value]) => [name, value.trim()]));
}

function serviceUrl(raw, failure) {
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw failure("is not a valid URL");
  }
  const loopback = url.hostname === "localhost" || url.hostname === "[::1]" || /^127\./.test(url.hostname);
  if (url.username || url.password || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))) {
    throw failure("must be an https URL, or http only for a server on this Mac");
  }
  return { url: url.toString(), host: url.host, loopback };
}

function endpointOf(transport, values) {
  const direct = typeof transport.base_url === "string" ? transport.base_url.trim() : "";
  const envName = declaredName(transport.base_url_env);
  if (direct && envName) throw new Error("The interface declares both base_url and base_url_env.");
  const raw = direct || (envName ? values[envName] : "");
  if (!raw) return null;
  const timeoutMs = transport.request_timeout_ms ?? DEFAULT_REQUEST_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_REQUEST_TIMEOUT_MS) {
    throw new Error(`The interface declares request_timeout_ms outside 1..${MAX_REQUEST_TIMEOUT_MS}.`);
  }
  return { ...serviceUrl(raw, (reason) => new ConnectionFailure("error", `The server URL ${reason}.`)), timeoutMs };
}

function accountProfile(descriptor, transport) {
  const profile = descriptor.interface_driver_config.private_config?.account_profile;
  if (profile === undefined) return null;
  const invalid = (detail) => new Error(`Interface '${descriptor.id}' declares an invalid account_profile: ${detail}.`);
  if (!isObject(profile)) throw invalid("it is not an object");
  const unknown = Object.keys(profile).filter((key) => !PROFILE_FIELDS.has(key));
  if (unknown.length) throw invalid(`unknown field ${unknown.join(", ")}`);
  const sources = ["resource", "request"].filter((key) => profile[key] !== undefined);
  if (sources.length !== 1 || !declaredName(profile[sources[0]])) throw invalid("it must name exactly one resource or request");
  const declared = { via: null, absent: null, manage: null, accounts: null, name: null, label: null };
  if (profile.resource !== undefined) declared.resource = profile.resource.trim();
  if (profile.request !== undefined) {
    if (!declaredName(transport.auth_env_key)) throw invalid("a request needs the interface key in transport.auth_env_key");
    declared.request = serviceUrl(profile.request.trim(), (reason) => invalid(`request ${reason}`));
  }
  for (const field of ["accounts", "name"]) {
    if (profile[field] === undefined) continue;
    if (typeof profile[field] !== "string" || !POINTER.test(profile[field])) throw invalid(`${field} must be a JSON pointer`);
    declared[field] = profile[field];
  }
  for (const [field, limit] of [["label", 96], ["via", 120], ["absent", 300]]) {
    if (profile[field] === undefined) continue;
    if (!declaredName(profile[field]) || profile[field].length > limit) throw invalid(`${field} must be text of 1..${limit} characters`);
    declared[field] = profile[field].trim();
  }
  if ((declared.name === null) === (declared.label === null)) throw invalid("it must declare either a name pointer or a fixed label");
  if (declared.accounts !== null && declared.name === null) throw invalid("an account list needs a name pointer");
  if (profile.manage !== undefined) {
    const manage = profile.manage;
    if (!isObject(manage) || Object.keys(manage).some((key) => !["label", "url"].includes(key))
      || !declaredName(manage.label) || manage.label.length > 48) {
      throw invalid("manage must be a label of 1..48 characters and an https url");
    }
    const url = serviceUrl(typeof manage.url === "string" ? manage.url : "", (reason) => invalid(`manage url ${reason}`));
    if (!url.url.startsWith("https:")) throw invalid("manage url must be https");
    declared.manage = { label: manage.label.trim(), url: url.url };
  }
  return declared;
}

function pointerValue(document, pointer) {
  let value = document;
  for (const part of pointer.split("/").slice(1)) {
    const key = part.replace(/~1/g, "/").replace(/~0/g, "~");
    if (Array.isArray(value) && /^(?:0|[1-9]\d*)$/.test(key)) value = value[Number(key)];
    else if (isObject(value) && Object.prototype.hasOwnProperty.call(value, key)) value = value[key];
    else return undefined;
  }
  return value;
}

function signalFor(ctx, timeoutMs) {
  const timeout = AbortSignal.timeout(timeoutMs);
  return ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout;
}

function unanswered(error, host, timeoutMs, otherwise) {
  const seconds = Math.ceil(timeoutMs / 1000);
  return error?.name === "TimeoutError"
    ? new ConnectionFailure("error", `${host} did not answer within ${seconds} second${seconds === 1 ? "" : "s"}.`)
    : otherwise;
}

async function textOf(ctx, response, host, timeoutMs) {
  try {
    return await response.text();
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    throw unanswered(error, host, timeoutMs, new ConnectionFailure("error", `${host} stopped answering before its reply was complete.`));
  }
}

function headersFor(token, session, headers = {}) {
  if (token) headers.authorization = `Bearer ${token}`;
  if (session.id) headers["mcp-session-id"] = session.id;
  if (session.protocol) headers["mcp-protocol-version"] = session.protocol;
  return headers;
}

function parseJson(text, message) {
  try {
    return JSON.parse(text);
  } catch {
    throw new ConnectionFailure("error", message);
  }
}

function parseRpcResponse(contentType, text, id) {
  if (text.length > MAX_RESPONSE_CHARS) throw new ConnectionFailure("error", "The server response is too large.");
  const messages = [];
  if (contentType.includes("text/event-stream")) {
    for (const event of text.split(/\r?\n\r?\n/)) {
      const data = event.split(/\r?\n/).filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trimStart()).join("\n");
      if (data) messages.push(parseJson(data, "The server sent malformed MCP data."));
    }
  } else {
    messages.push(parseJson(text, "The server sent malformed MCP data."));
  }
  const response = messages.flat().find((message) => message?.id === id);
  if (!response) throw new ConnectionFailure("error", "The server did not answer the MCP request.");
  if (response.error) {
    throw new ConnectionFailure("error", `The server refused the request: ${String(response.error.message ?? "unknown error").slice(0, 200)}`);
  }
  return response.result;
}

async function rpc(ctx, endpoint, token, session, method, params, notification = false) {
  const headers = headersFor(token, session, { "content-type": "application/json", accept: "application/json, text/event-stream" });
  const id = notification ? null : session.next++;
  const body = notification ? { jsonrpc: "2.0", method, params } : { jsonrpc: "2.0", id, method, params };
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(endpoint.url, { method: "POST", headers, body: JSON.stringify(body), signal: signalFor(ctx, endpoint.timeoutMs), redirect: "error" });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    throw unanswered(error, endpoint.host, endpoint.timeoutMs, new ConnectionFailure(endpoint.loopback ? "not_connected" : "error", endpoint.loopback
      ? `Nothing answers at ${endpoint.url}. Start the server and try again.`
      : `Could not reach ${endpoint.host}.`));
  }
  if (response.status === 401) {
    throw new ConnectionFailure("expired", "The server rejected the key. Check it and connect again.");
  }
  if (response.status === 403) {
    throw new ConnectionFailure("error", "The server refused this key access (HTTP 403).");
  }
  if (!response.ok && !(notification && response.status === 202)) {
    throw new ConnectionFailure("error", `The server answered HTTP ${response.status}.`);
  }
  session.id = response.headers.get("mcp-session-id") ?? session.id;
  if (notification) return null;
  return parseRpcResponse(response.headers.get("content-type") ?? "", await textOf(ctx, response, endpoint.host, endpoint.timeoutMs), id);
}

async function closeSession(ctx, endpoint, token, session) {
  if (!session.id) return;
  try {
    const response = await (ctx.fetch ?? fetch)(endpoint.url, {
      method: "DELETE", headers: headersFor(token, session), signal: AbortSignal.timeout(Math.min(endpoint.timeoutMs, CLOSE_TIMEOUT_MS)), redirect: "error",
    });
    await response.body?.cancel();
  } catch {}
}

async function listTools(ctx, endpoint, token, session, capabilities, all) {
  const tools = [];
  if (!capabilities?.tools) return { tools, complete: true };
  let cursor;
  for (let page = 0; page < (all ? TOOL_PAGES : 1); page += 1) {
    const listed = await rpc(ctx, endpoint, token, session, "tools/list", cursor ? { cursor } : {});
    if (!Array.isArray(listed?.tools)) throw new ConnectionFailure("error", "The server listed no tools array.");
    tools.push(...listed.tools);
    cursor = typeof listed.nextCursor === "string" && listed.nextCursor ? listed.nextCursor : undefined;
    if (!cursor) return { tools, complete: true };
  }
  if (all) throw new ConnectionFailure("error", `The server lists more than ${TOOL_PAGES} pages of tools.`);
  return { tools, complete: false };
}

async function readResourceDocument(ctx, endpoint, token, session, uri) {
  const result = await rpc(ctx, endpoint, token, session, "resources/read", { uri });
  const content = Array.isArray(result?.contents) ? result.contents.find((entry) => typeof entry?.text === "string") : null;
  if (!content) throw new ConnectionFailure("error", `The server answered ${uri} without text.`);
  if (content.text.length > MAX_RESPONSE_CHARS) throw new ConnectionFailure("error", `The server answered ${uri} with too much data.`);
  return parseJson(content.text, `The server answered ${uri} with text that is not JSON.`);
}

async function requestAccountDocument(ctx, request, token, timeoutMs) {
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(request.url, {
      method: "GET", headers: { accept: "application/json", authorization: `Bearer ${token}` }, signal: signalFor(ctx, timeoutMs), redirect: "error",
    });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    throw unanswered(error, request.host, timeoutMs, new ConnectionFailure("error", `Could not reach ${request.host} to check the key.`));
  }
  if (response.status === 401) throw new ConnectionFailure("expired", `${request.host} rejected the key. Check it and connect again.`);
  if (response.status === 403) throw new ConnectionFailure("error", `${request.host} refused to check this key (HTTP 403).`);
  if (!response.ok) throw new ConnectionFailure("error", `${request.host} answered the key check with HTTP ${response.status}.`);
  const text = await textOf(ctx, response, request.host, timeoutMs);
  if (text.length > MAX_RESPONSE_CHARS) throw new ConnectionFailure("error", `${request.host} answered the key check with too much data.`);
  return parseJson(text, `${request.host} answered the key check with data that is not JSON.`);
}

async function accountNames(ctx, endpoint, token, session, profile, capabilities) {
  let document;
  if (profile.resource) {
    if (!isObject(capabilities?.resources)) throw new ConnectionFailure("error", `The server offers no resources, so it cannot name its account at ${profile.resource}.`);
    document = await readResourceDocument(ctx, endpoint, token, session, profile.resource);
  } else {
    document = await requestAccountDocument(ctx, profile.request, token, endpoint.timeoutMs);
  }
  if (profile.name === null) return [profile.label];
  const entries = profile.accounts === null ? [document] : pointerValue(document, profile.accounts);
  if (!Array.isArray(entries)) throw new ConnectionFailure("error", "The server answered without the list of accounts it works in.");
  const names = entries.map((entry) => pointerValue(entry, profile.name))
    .filter((name) => typeof name === "string" && name.trim() && !/[\u0000-\u001f\u007f]/u.test(name))
    .map((name) => name.trim());
  return [...new Set(names)];
}

async function check(ctx, values, all = false) {
  const transport = transportOf(ctx.descriptor);
  const profile = accountProfile(ctx.descriptor, transport);
  const endpoint = endpointOf(transport, values);
  if (!endpoint) return { state: "not_connected", message: "Enter the server URL." };
  const tokenName = declaredName(transport.auth_env_key);
  const token = tokenName ? values[tokenName] : null;
  if (tokenName && !token) return { state: "not_connected", message: "Enter the key." };
  const config = ctx.descriptor.interface_driver_config.private_config ?? {};
  const protocols = Array.isArray(config.protocol_versions) ? config.protocol_versions : [];
  if (!protocols.length) throw new Error(`Interface '${ctx.descriptor.id}' declares no MCP protocol versions.`);
  const session = { id: null, protocol: null, next: 1 };
  const manage = profile?.manage ? { manage: profile.manage } : {};
  try {
    const initialized = await rpc(ctx, endpoint, token, session, "initialize", {
      protocolVersion: protocols[0],
      capabilities: {},
      clientInfo: { name: config.client_name ?? "octopus-mcp-client", version: String(config.client_version ?? "1") },
    });
    if (!protocols.includes(initialized?.protocolVersion)) {
      return { state: "error", message: `The server speaks MCP ${initialized?.protocolVersion ?? "of an unknown version"}, which this interface does not declare.` };
    }
    session.protocol = initialized.protocolVersion;
    await rpc(ctx, endpoint, token, session, "notifications/initialized", {}, true);
    const server = initialized.serverInfo ?? {};
    const name = [server.title ?? server.name, server.version].filter((part) => typeof part === "string" && part).join(" ");
    if (!profile) {
      const listed = await listTools(ctx, endpoint, token, session, initialized.capabilities, all);
      const count = listed.tools.length;
      return {
        state: "connected",
        tools: listed.tools,
        account: {
          label: name || endpoint.host,
          detail: `${count}${listed.complete ? "" : "+"} tool${count === 1 ? "" : "s"} · ${endpoint.host}`,
        },
      };
    }
    const names = await accountNames(ctx, endpoint, token, session, profile, initialized.capabilities);
    if (!names.length) {
      return { state: "not_connected", answered: true, message: profile.absent ?? "The server answers, but names no account it works in.", ...manage };
    }
    const listed = all ? await listTools(ctx, endpoint, token, session, initialized.capabilities, true) : { tools: [] };
    return {
      state: "connected",
      tools: listed.tools,
      account: { label: names.join(", "), detail: `Through ${profile.via ?? `${name || "the server"} at ${endpoint.host}`}` },
      ...manage,
    };
  } catch (error) {
    if (error instanceof ConnectionFailure && profile?.manage) error.manage = profile.manage;
    throw error;
  } finally {
    await closeSession(ctx, endpoint, token, session);
  }
}

function presented(state) {
  const result = { state: state.state };
  if (state.account) {
    result.account = { label: clip(state.account.label, 96) };
    if (state.account.detail) result.account.detail = clip(state.account.detail, 160);
  }
  if (state.manage) result.manage = state.manage;
  if (state.message) result.message = clip(state.message, 300);
  return result;
}

async function settle(work) {
  try {
    return await work();
  } catch (error) {
    if (error instanceof ConnectionFailure) {
      return { state: error.state, message: error.message, ...(error.manage ? { manage: error.manage } : {}) };
    }
    throw error;
  }
}

async function probe(ctx) {
  return presented(await settle(() => check(ctx, filled(ctx.values))));
}

async function publish(ctx, result) {
  if (typeof ctx.publishOperations !== "function") return presented(result);
  const { published, skipped } = await publishServerTools(ctx, result.tools);
  const parts = [`${published} tool${published === 1 ? " is" : "s are"} available to connected tasks.`];
  if (skipped.length) parts.push(`Only through call_tool: ${skipped.join("; ")}`);
  return presented({ ...result, message: parts.join(" ") });
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
    const result = await settle(() => check(ctx, values, typeof ctx.publishOperations === "function"));
    if (result.state === "connected") {
      if (Object.keys(input).length) await ctx.saveValues(input);
      return publish(ctx, result);
    }
    if (result.state !== "not_connected" || !endpoint?.loopback || Date.now() >= deadline) return presented(result);
    ctx.progress({ message: clip(result.answered ? `${result.message} Octopus keeps checking.` : `Waiting for the server at ${endpoint.url}…`, 300) });
    await delay(LOOPBACK_RETRY_MS, undefined, { signal: ctx.signal }).catch((error) => {
      throw ctx.signal?.aborted ? ctx.signal.reason : error;
    });
  }
}

async function disconnect(ctx) {
  const transport = transportOf(ctx.descriptor);
  const names = [declaredName(transport.auth_env_key), declaredName(transport.base_url_env)].filter(Boolean);
  if (names.length) {
    await ctx.saveValues(Object.fromEntries(names.map((name) => [name, null])));
    return { state: "not_connected" };
  }
  const state = await probe(ctx);
  if (state.state !== "connected") return state;
  return presented({ ...state, message: "Octopus keeps no sign-in for this server, so it stays connected while the server runs. Stop the server to disconnect it." });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
