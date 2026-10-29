"use strict";

const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");

const MAX_ACCOUNT_FILE_BYTES = 1024 * 1024;
const ACCESS_TOKEN_EXPIRY_SKEW_MS = 30_000;
const LINK_TIMEOUT_MS = 10 * 60_000;
const LINK_FIRST_POLL_MS = 3_000;
const LINK_POLL_STEP_MS = 2_000;
const LINK_MAX_POLL_MS = 15_000;
const DEFAULT_RESPONSE_BYTES = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 256 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;
const MAX_REQUEST_TIMEOUT_MS = 600_000;
const TRANSPORT_OWNED_HEADERS = new Set([
  "authorization", "content-type", "accept", "mcp-protocol-version", "mcp-session-id", "last-event-id",
]);
const SIGN_IN_FIRST = "Sign in to ChatGPT first.";
const SIGN_IN_EXPIRED = "ChatGPT sign-in expired — sign in again in Octopus.";
const UNREADABLE_SIGN_IN = "The ChatGPT sign-in on this Mac can't be read. Sign in to ChatGPT again in Octopus.";

class ConnectionStop extends Error {
  constructor(state, message) {
    super(message);
    this.state = state;
  }
}

function isText(value) {
  return typeof value === "string" && value.length > 0 && value.trim() === value;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function clip(value, limit) {
  return value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;
}

function invalid(detail) {
  return new ConnectionStop("error", `This application's connection settings are invalid: ${detail}.`);
}

function boundedInteger(value, fallback, maximum, field) {
  if (value === undefined && fallback !== undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < 1 || value > maximum) throw invalid(`${field} is out of range`);
  return value;
}

function connectionConfig(descriptor) {
  const driver = descriptor?.interface_driver_config;
  const config = driver?.private_config;
  const transport = driver?.transport;
  if (descriptor?.interface_driver_ref !== "mcp_client" || !isObject(config) || !isObject(transport)
    || transport.wire !== "mcp_streamable_http") {
    throw invalid("it is not a streamable MCP client");
  }
  let endpoint;
  try {
    endpoint = new URL(transport.base_url);
  } catch {
    throw invalid("its endpoint is not a URL");
  }
  if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password || endpoint.hash) {
    throw invalid("its endpoint must be a plain https URL");
  }
  const auth = config.auth;
  if (auth?.mode !== "chatgpt_account" || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(auth.auth_file_env ?? "")
    || !/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/.test(auth.account_header ?? "")
    || TRANSPORT_OWNED_HEADERS.has(auth.account_header.toLowerCase())) {
    throw invalid("it declares no ChatGPT account sign-in");
  }
  const filter = config.tool_filter;
  if (!isText(filter?.meta_key) || !Array.isArray(filter.values) || !filter.values.length
    || !filter.values.every(isText)) {
    throw invalid("it declares no tool filter");
  }
  const link = config.connector_link;
  if (!/^[A-Za-z0-9_-]+$/.test(link?.connector_id ?? "") || !isText(link?.name)
    || filter.meta_key !== "connector_id" || !filter.values.includes(link.connector_id)) {
    throw invalid("its connector link is not admitted by its tool filter");
  }
  const versions = config.protocol_versions;
  if (!Array.isArray(versions) || !versions.length || !versions.every((value) => /^\d{4}-\d{2}-\d{2}$/.test(value))
    || !isText(config.client_name) || !isText(config.client_version)) {
    throw invalid("its MCP client identity is incomplete");
  }
  const headers = {};
  for (const [name, value] of Object.entries(transport.headers ?? {})) {
    const lower = name.toLowerCase();
    if (typeof value !== "string" || TRANSPORT_OWNED_HEADERS.has(lower) || lower === auth.account_header.toLowerCase()) {
      throw invalid(`transport header '${name}' is not admitted`);
    }
    headers[lower] = value;
  }
  return {
    endpoint: endpoint.href,
    origin: endpoint.origin,
    authFileEnv: auth.auth_file_env,
    accountHeader: auth.account_header,
    metaKey: filter.meta_key,
    values: new Set(filter.values),
    connectorId: link.connector_id,
    name: link.name,
    versions,
    clientName: config.client_name,
    clientVersion: config.client_version,
    maxPages: boundedInteger(config.max_catalog_pages, undefined, 1024, "max_catalog_pages"),
    maxResponseBytes: boundedInteger(transport.max_response_bytes, DEFAULT_RESPONSE_BYTES, MAX_RESPONSE_BYTES, "max_response_bytes"),
    timeoutMs: boundedInteger(transport.request_timeout_ms, DEFAULT_REQUEST_TIMEOUT_MS, MAX_REQUEST_TIMEOUT_MS, "request_timeout_ms"),
    headers,
  };
}

function connectorLinkUrl(config) {
  let slug = "";
  for (const character of config.name) slug += /[A-Za-z0-9]/.test(character) ? character.toLowerCase() : "-";
  slug = slug.replace(/^-+|-+$/g, "") || "app";
  return `${config.origin}/apps/${slug}/${config.connectorId}`;
}

function jwtClaims(token) {
  const parts = typeof token === "string" ? token.split(".") : [];
  if (parts.length !== 3 || !/^[A-Za-z0-9_-]+$/.test(parts[1])) return null;
  try {
    const claims = JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8"));
    return isObject(claims) ? claims : null;
  } catch {
    return null;
  }
}

function accountLabel(idToken) {
  const claims = jwtClaims(idToken);
  const email = claims?.email ?? claims?.["https://api.openai.com/profile"]?.email;
  return isText(email) ? email : "ChatGPT account";
}

async function readBoundedRegularFile(file) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(file, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size <= 0 || before.size > MAX_ACCOUNT_FILE_BYTES) {
      throw new Error("not a bounded regular file");
    }
    const bytes = await handle.readFile();
    if (bytes.byteLength !== before.size) throw new Error("changed while it was read");
    return bytes;
  } finally {
    await handle.close();
  }
}

async function readCredential(ctx, config) {
  const file = ctx.launchEnvironment(config.authFileEnv);
  if (typeof file !== "string" || !path.isAbsolute(file)) {
    throw new ConnectionStop("error", "Octopus did not provide the location of the ChatGPT sign-in.");
  }
  let bytes;
  try {
    bytes = await readBoundedRegularFile(file);
  } catch (error) {
    if (error?.code === "ENOENT") return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
    throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  }
  let document;
  try {
    document = JSON.parse(bytes.toString("utf8"));
  } catch {
    throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  }
  if (!isObject(document) || (document.auth_mode !== undefined && document.auth_mode !== "chatgpt")) {
    return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
  }
  const tokens = document.tokens;
  if (!isText(tokens?.access_token) || !isText(tokens?.account_id)) {
    return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
  }
  const account = { label: accountLabel(tokens.id_token) };
  const expiresAt = jwtClaims(tokens.access_token)?.exp;
  if (!Number.isFinite(expiresAt)) throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  if (expiresAt * 1000 <= Date.now() + ACCESS_TOKEN_EXPIRY_SKEW_MS) {
    return { stop: { state: "expired", account, message: SIGN_IN_EXPIRED } };
  }
  return { accessToken: tokens.access_token, accountId: tokens.account_id, account };
}

function parseEventStream(text, method) {
  const messages = [];
  let data = [];
  let event = "";
  const flush = () => {
    if (data.length && event !== "endpoint") messages.push(parseJson(data.join("\n"), method));
    data = [];
    event = "";
  };
  for (const line of text.split(/\r?\n/)) {
    if (line === "") {
      flush();
    } else if (!line.startsWith(":")) {
      const split = line.indexOf(":");
      const field = split < 0 ? line : line.slice(0, split);
      const value = split < 0 ? "" : line.slice(split + 1).replace(/^ /, "");
      if (field === "data") data.push(value);
      if (field === "event") event = value;
    }
  }
  flush();
  return messages;
}

function parseJson(text, method) {
  try {
    return JSON.parse(text);
  } catch {
    throw new ConnectionStop("error", `ChatGPT's apps service sent malformed data for ${method}.`);
  }
}

async function post(ctx, config, credential, session, body) {
  const method = body.method;
  const headers = {
    ...config.headers,
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
    "mcp-protocol-version": session.protocol,
    authorization: `Bearer ${credential.accessToken}`,
    [config.accountHeader]: credential.accountId,
  };
  if (session.id) headers["mcp-session-id"] = session.id;
  const signals = [AbortSignal.timeout(config.timeoutMs)];
  if (ctx.signal) signals.push(ctx.signal);
  let response;
  try {
    response = await ctx.fetch(config.endpoint, {
      method: "POST", headers, body: JSON.stringify(body), redirect: "manual", signal: AbortSignal.any(signals),
    });
  } catch {
    if (ctx.signal?.aborted) throw new ConnectionStop("error", "The connection check was cancelled.");
    throw new ConnectionStop("error", `ChatGPT's apps service could not be reached for ${method}.`);
  }
  if (response.status === 401) throw new ConnectionStop("expired", SIGN_IN_EXPIRED);
  if (response.status === 403) {
    throw new ConnectionStop("error", "ChatGPT refused apps access for this account (HTTP 403).");
  }
  if (response.status < 200 || response.status > 299) {
    throw new ConnectionStop("error", `ChatGPT's apps service answered ${method} with HTTP ${response.status}.`);
  }
  const sessionId = response.headers.get("mcp-session-id");
  if (sessionId) {
    if (session.id && session.id !== sessionId) {
      throw new ConnectionStop("error", "ChatGPT's apps service changed the MCP session during the check.");
    }
    session.id = sessionId;
  }
  if (body.id === undefined) return [];
  const length = Number(response.headers.get("content-length"));
  if (Number.isFinite(length) && length > config.maxResponseBytes) {
    throw new ConnectionStop("error", `ChatGPT's apps service sent more data for ${method} than this application allows.`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.byteLength > config.maxResponseBytes) {
    throw new ConnectionStop("error", `ChatGPT's apps service sent more data for ${method} than this application allows.`);
  }
  const type = (response.headers.get("content-type") ?? "").split(";")[0].trim().toLowerCase();
  if (type === "application/json") return [parseJson(bytes.toString("utf8"), method)];
  if (type === "text/event-stream") return parseEventStream(bytes.toString("utf8"), method);
  throw new ConnectionStop("error", `ChatGPT's apps service answered ${method} with unsupported content.`);
}

async function rpc(ctx, config, credential, session, method, params) {
  const id = session.nextId;
  session.nextId += 1;
  const messages = await post(ctx, config, credential, session, { jsonrpc: "2.0", id, method, params });
  const response = messages.find((message) => isObject(message) && message.id === id && message.method === undefined);
  if (!response) throw new ConnectionStop("error", `ChatGPT's apps service sent no answer for ${method}.`);
  if (response.error !== undefined) {
    const code = Number.isSafeInteger(response.error?.code) ? ` (code ${response.error.code})` : "";
    throw new ConnectionStop("error", `ChatGPT's apps service refused ${method}${code}.`);
  }
  if (!isObject(response.result)) throw new ConnectionStop("error", `ChatGPT's apps service sent a malformed ${method} result.`);
  return response.result;
}

async function listConnectorTools(ctx, config, credential) {
  const session = { id: null, protocol: config.versions[0], nextId: 1 };
  const initialized = await rpc(ctx, config, credential, session, "initialize", {
    protocolVersion: session.protocol,
    capabilities: {},
    clientInfo: { name: config.clientName, version: config.clientVersion },
  });
  if (!config.versions.includes(initialized.protocolVersion) || !isObject(initialized.serverInfo)
    || !isObject(initialized.capabilities)) {
    throw new ConnectionStop("error", "ChatGPT's apps service answered with an unsupported MCP handshake.");
  }
  if (!isObject(initialized.capabilities.tools)) {
    throw new ConnectionStop("error", "ChatGPT's apps service offers no tools for this account.");
  }
  session.protocol = initialized.protocolVersion;
  await post(ctx, config, credential, session, { jsonrpc: "2.0", method: "notifications/initialized", params: {} });
  const tools = [];
  const cursors = new Set();
  let cursor;
  for (let page = 0; page < config.maxPages; page += 1) {
    const result = await rpc(ctx, config, credential, session, "tools/list", cursor === undefined ? {} : { cursor });
    if (!Array.isArray(result.tools)) throw new ConnectionStop("error", "ChatGPT's apps service sent a malformed tool list.");
    for (const tool of result.tools) {
      const value = tool?._meta?.[config.metaKey];
      if (typeof value === "string" && config.values.has(value)) tools.push(tool);
    }
    if (result.nextCursor === undefined || result.nextCursor === null) return tools;
    if (!isText(result.nextCursor) || cursors.has(result.nextCursor)) {
      throw new ConnectionStop("error", "ChatGPT's apps service sent an invalid tool list page.");
    }
    cursors.add(result.nextCursor);
    cursor = result.nextCursor;
  }
  throw new ConnectionStop("error", "ChatGPT's apps service sent more tool pages than this application allows.");
}

function connectedState(config, account, tools) {
  if (!tools.length) {
    return { state: "not_connected", account, message: `${config.name} is not connected to this ChatGPT account.` };
  }
  const emails = new Set();
  for (const tool of tools) {
    const linked = tool?._meta?.link_accounts;
    for (const entry of Array.isArray(linked) ? linked : []) {
      if (isText(entry?.profile_email)) emails.add(entry.profile_email);
    }
  }
  const connected = { label: account.label };
  if (emails.size) connected.detail = `${config.name}: ${[...emails].join(", ")}`;
  return { state: "connected", account: connected };
}

function finish(state) {
  const result = { state: state.state };
  if (state.account) {
    result.account = { label: clip(state.account.label, 96) };
    if (state.account.detail) result.account.detail = clip(state.account.detail, 160);
  }
  if (state.message) result.message = clip(state.message, 300);
  return result;
}

async function probe(ctx) {
  let account;
  try {
    const config = connectionConfig(ctx.descriptor);
    const credential = await readCredential(ctx, config);
    if (credential.stop) return finish(credential.stop);
    account = credential.account;
    return finish(connectedState(config, account, await listConnectorTools(ctx, config, credential)));
  } catch (error) {
    if (error instanceof ConnectionStop) return finish({ state: error.state, account, message: error.message });
    return finish({ state: "error", account, message: `The connection could not be checked: ${error?.message ?? error}` });
  }
}

function describedConfig(ctx) {
  try {
    return { config: connectionConfig(ctx.descriptor) };
  } catch (error) {
    return { state: finish({ state: "error", message: error.message }) };
  }
}

async function connect(ctx) {
  const { config, state: invalidState } = describedConfig(ctx);
  if (!config) return invalidState;
  let state = await probe(ctx);
  if (state.state !== "not_connected" || !state.account) return state;
  const url = connectorLinkUrl(config);
  try {
    await ctx.openExternal(url);
  } catch (error) {
    return finish({ ...state, state: "error", message: `Octopus could not open the ChatGPT page for ${config.name}: ${error?.message ?? error}` });
  }
  await ctx.progress({
    message: `Connect ${config.name} on the ChatGPT page that opened. Octopus continues when ChatGPT reports it connected.`,
    url,
    code: "connector_link_pending",
  });
  const deadline = Date.now() + LINK_TIMEOUT_MS;
  for (let attempt = 0; Date.now() < deadline; attempt += 1) {
    try {
      await delay(Math.min(LINK_FIRST_POLL_MS + attempt * LINK_POLL_STEP_MS, LINK_MAX_POLL_MS), undefined, { signal: ctx.signal });
    } catch (error) {
      if (!ctx.signal?.aborted) throw error;
    }
    if (ctx.signal?.aborted) return finish({ ...state, message: `Connecting ${config.name} was cancelled.` });
    state = await probe(ctx);
    if (state.state === "connected" || state.state === "expired" || !state.account) return state;
  }
  return finish({
    state: state.state === "error" ? "error" : "not_connected",
    account: state.account,
    message: `${config.name} was not connected within ${LINK_TIMEOUT_MS / 60_000} minutes. Connect it on ChatGPT, then try again.`,
  });
}

async function disconnect(ctx) {
  const { config, state: invalidState } = describedConfig(ctx);
  if (!config) return invalidState;
  const state = await probe(ctx);
  if (state.state !== "connected") return state;
  return finish({
    ...state,
    message: `${config.name} stays connected to your ChatGPT account. To disconnect it, open ChatGPT Settings, Apps, and disconnect ${config.name} there.`,
  });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
