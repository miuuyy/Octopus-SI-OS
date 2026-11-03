"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");
const { publishServerTools } = require("./server_operations.cjs");

const MAX_SIGN_IN_FILE_BYTES = 1024 * 1024;
const ACCOUNTS_FILE = "connection-accounts.json";
const MAX_ACCOUNTS_FILE_BYTES = 16 * 1024;
const MAX_REMEMBERED_ACCOUNTS = 32;
const MAX_PROFILE_READS = 8;
const MANAGE_LABEL = "Manage in ChatGPT";
const LINK_ID = /^[\x21-\x7e]{1,256}$/;
const PROFILE_FIELD = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;
const TOOL_NAME = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/;
const SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const NOUN = /^[a-z][a-z ]{0,30}[a-z]$/;
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
const ACCOUNT_VALUES = {
  email_field: (value) => (typeof value === "string" && value.length <= 254
    && /^[^\s@\u0000-\u001f\u007f]+@[^\s@\u0000-\u001f\u007f]+$/u.test(value) ? value : null),
  login_field: (value) => (typeof value === "string" && /^[^\s\u0000-\u001f\u007f]{1,96}$/u.test(value) ? value : null),
};
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

function accountProfile(profile) {
  const fields = isObject(profile) ? Object.keys(profile).filter((key) => Object.hasOwn(ACCOUNT_VALUES, key)) : [];
  if (!isObject(profile) || fields.length !== 1
    || Object.keys(profile).some((key) => !["tool", "noun", fields[0]].includes(key))
    || !TOOL_NAME.test(profile.tool ?? "") || !PROFILE_FIELD.test(profile[fields[0]] ?? "") || !NOUN.test(profile.noun ?? "")) {
    throw invalid("its connector link declares no account profile tool, one email or login field and the noun of its account");
  }
  return { tool: profile.tool, field: profile[fields[0]], value: ACCOUNT_VALUES[fields[0]], noun: profile.noun };
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
  if (!isObject(link) || Object.keys(link).some((key) => !["connector_id", "name", "slug", "account_profile"].includes(key))
    || !/^[A-Za-z0-9_-]+$/.test(link.connector_id ?? "") || !isText(link.name) || !SLUG.test(link.slug ?? "")
    || filter.meta_key !== "connector_id" || !filter.values.includes(link.connector_id)) {
    throw invalid("its connector link is not a connector id, name and slug admitted by its tool filter");
  }
  const profile = accountProfile(link.account_profile);
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
    slug: link.slug,
    profile,
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
  return `${config.origin}/apps/${config.slug}/${config.connectorId}`;
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

function signInEmail(idToken) {
  const claims = jwtClaims(idToken);
  const email = claims?.email ?? claims?.["https://api.openai.com/profile"]?.email;
  return isText(email) ? email : null;
}

function signInAccount(signIn) {
  return { label: signIn ? `ChatGPT account ${signIn}` : "ChatGPT account" };
}

function manageLink(config) {
  return { label: MANAGE_LABEL, url: connectorLinkUrl(config) };
}

async function readBoundedRegularFile(file, limit) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(file, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size <= 0 || before.size > limit) {
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
    bytes = await readBoundedRegularFile(file, MAX_SIGN_IN_FILE_BYTES);
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
  const signIn = signInEmail(tokens.id_token);
  const expiresAt = jwtClaims(tokens.access_token)?.exp;
  if (!Number.isFinite(expiresAt)) throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  if (expiresAt * 1000 <= Date.now() + ACCESS_TOKEN_EXPIRY_SKEW_MS) {
    return { stop: { state: "expired", account: signInAccount(signIn), manage: manageLink(config), message: SIGN_IN_EXPIRED } };
  }
  return { accessToken: tokens.access_token, accountId: tokens.account_id, signIn };
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

async function listConnectorTools(ctx, config, credential, session) {
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
      if (typeof value === "string" && config.values.has(value) && tool._meta._codex_apps?.synthetic_link !== true) tools.push(tool);
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

function linkIds(tool) {
  const ids = [];
  if (LINK_ID.test(tool?._meta?.link_id ?? "")) ids.push(tool._meta.link_id);
  for (const entry of Array.isArray(tool?._meta?.link_accounts) ? tool._meta.link_accounts : []) {
    if (LINK_ID.test(entry?.link_id ?? "")) ids.push(entry.link_id);
  }
  return ids;
}

function profileRequest(config, tools, link) {
  for (const tool of tools) {
    if (tool.name !== config.profile.tool || tool.annotations?.readOnlyHint !== true) continue;
    if (tool._meta._codex_apps?.requires_explicit_link_id === true) {
      if (isObject(tool.inputSchema?.properties?.link_id) && linkIds(tool).includes(link)) {
        return { name: tool.name, arguments: { link_id: link }, _meta: tool._meta };
      }
    } else if (tool._meta.link_id === link) {
      return { name: tool.name, arguments: {}, _meta: tool._meta };
    }
  }
  return null;
}

async function readAccount(ctx, config, credential, session, request) {
  try {
    const result = await rpc(ctx, config, credential, session, "tools/call", request);
    return result.isError === true || !isObject(result.structuredContent)
      ? null : config.profile.value(result.structuredContent[config.profile.field]);
  } catch (error) {
    if (!(error instanceof ConnectionStop) || error.state !== "error" || ctx.signal?.aborted) throw error;
    return null;
  }
}

async function readAccounts(ctx, config, credential, session, tools, links, remembered) {
  const accounts = new Map();
  let reads = 0;
  for (const link of links) {
    if (remembered.has(link)) {
      accounts.set(link, remembered.get(link));
      continue;
    }
    const request = reads < MAX_PROFILE_READS ? profileRequest(config, tools, link) : null;
    if (!request) continue;
    reads += 1;
    const account = await readAccount(ctx, config, credential, session, request);
    if (account) accounts.set(link, account);
  }
  return accounts;
}

function accountsFile(ctx) {
  if (typeof ctx.privateDirectory !== "string" || !path.isAbsolute(ctx.privateDirectory)) {
    throw new ConnectionStop("error", "Octopus did not provide this application's private directory.");
  }
  return path.join(ctx.privateDirectory, ACCOUNTS_FILE);
}

async function rememberedAccounts(ctx, config) {
  let document;
  try {
    document = JSON.parse((await readBoundedRegularFile(accountsFile(ctx), MAX_ACCOUNTS_FILE_BYTES)).toString("utf8"));
  } catch (error) {
    if (error instanceof ConnectionStop) throw error;
    return new Map();
  }
  const entries = isObject(document?.links) ? Object.entries(document.links) : [];
  if (document?.schema_version !== 1 || document.connector_id !== config.connectorId || entries.length > MAX_REMEMBERED_ACCOUNTS
    || entries.some(([link, account]) => !LINK_ID.test(link) || !config.profile.value(account))) {
    return new Map();
  }
  return new Map(entries);
}

async function rememberAccounts(ctx, config, accounts) {
  const file = accountsFile(ctx);
  const links = Object.fromEntries([...accounts].slice(0, MAX_REMEMBERED_ACCOUNTS));
  const body = `${JSON.stringify({ schema_version: 1, connector_id: config.connectorId, links }, null, 2)}\n`;
  if (Buffer.byteLength(body) > MAX_ACCOUNTS_FILE_BYTES) throw new Error(`the ${config.name} ${config.profile.noun} list is too large`);
  const temporary = path.join(path.dirname(file), `.${ACCOUNTS_FILE}.${process.pid}.${crypto.randomUUID()}.tmp`);
  try {
    await fsPromises.writeFile(temporary, body, { flag: "wx", mode: 0o600 });
    await fsPromises.rename(temporary, file);
  } catch (error) {
    await fsPromises.rm(temporary, { force: true });
    throw error;
  }
}

function connectedState(config, signIn, links, accounts) {
  const labels = [...new Set(links.flatMap((link) => (accounts.has(link) ? [accounts.get(link)] : [])))];
  const unread = links.filter((link) => !accounts.has(link)).length;
  const noun = config.profile.noun;
  const state = {
    state: "connected",
    account: { label: labels.length ? labels.join(", ") : `${config.name} linked in ChatGPT`, detail: `Through ${signInAccount(signIn).label}` },
    manage: manageLink(config),
  };
  if (!labels.length) state.message = `${config.name} is linked in ChatGPT, but Octopus could not read which ${noun} it uses.`;
  else if (unread) state.message = `Octopus could not read the ${noun} of ${unread} more ${config.name} account${unread === 1 ? "" : "s"} linked in ChatGPT.`;
  return state;
}

function finish(state) {
  const result = { state: state.state };
  if (state.account) {
    result.account = { label: clip(state.account.label, 96) };
    if (state.account.detail) result.account.detail = clip(state.account.detail, 160);
  }
  if (state.manage) result.manage = { label: clip(state.manage.label, 48), url: state.manage.url };
  if (state.message) result.message = clip(state.message, 300);
  return result;
}

async function inspect(ctx, fresh) {
  let signedIn = null;
  const outcome = (state, tools = [], accounts = new Map()) => ({ state: finish(state), tools, accounts, signedIn: Boolean(signedIn) });
  try {
    const config = connectionConfig(ctx.descriptor);
    const credential = await readCredential(ctx, config);
    if (credential.stop) return outcome(credential.stop);
    signedIn = { account: signInAccount(credential.signIn), manage: manageLink(config) };
    const session = { id: null, protocol: config.versions[0], nextId: 1 };
    const tools = await listConnectorTools(ctx, config, credential, session);
    if (!tools.length) {
      return outcome({ state: "not_connected", ...signedIn, message: `${config.name} is not linked to this ChatGPT account.` });
    }
    const links = [...new Set(tools.flatMap(linkIds))];
    const remembered = fresh ? new Map() : await rememberedAccounts(ctx, config);
    const accounts = await readAccounts(ctx, config, credential, session, tools, links, remembered);
    return outcome(connectedState(config, credential.signIn, links, accounts), tools, accounts);
  } catch (error) {
    if (error instanceof ConnectionStop) return outcome({ state: error.state, ...signedIn, message: error.message });
    return outcome({ state: "error", ...signedIn, message: `The connection could not be checked: ${error?.message ?? error}` });
  }
}

async function probe(ctx) {
  return (await inspect(ctx, false)).state;
}

async function published(ctx, config, inspected) {
  if (inspected.state.state !== "connected") return inspected.state;
  let unkept = "";
  try {
    await rememberAccounts(ctx, config, inspected.accounts);
  } catch (error) {
    unkept = `Octopus could not keep the ${config.name} ${config.profile.noun} list and reads it again on the next check: ${error?.message ?? error}`;
  }
  if (typeof ctx.publishOperations !== "function") {
    return unkept ? finish({ ...inspected.state, message: [inspected.state.message, unkept].filter(Boolean).join(" ") }) : inspected.state;
  }
  const tools = inspected.tools.map((tool) => {
    const linked = (Array.isArray(tool._meta.link_accounts) ? tool._meta.link_accounts : []).filter((entry) => LINK_ID.test(entry?.link_id ?? ""));
    if (!linked.length) return tool;
    const accounts = linked.map((entry) => `${entry.link_id}${inspected.accounts.has(entry.link_id) ? ` (${inspected.accounts.get(entry.link_id)})` : ""}`).join(", ");
    return { ...tool, description: `${isText(tool.description) ? `${tool.description}\n\n` : ""}Linked accounts, by link_id: ${accounts}.` };
  });
  const { published: count, skipped } = await publishServerTools(ctx, tools);
  const message = [
    inspected.state.message,
    `${count} tool${count === 1 ? " is" : "s are"} available to connected tasks.`,
    unkept,
    skipped.length ? `Only through call_tool: ${skipped.join("; ")}` : "",
  ].filter(Boolean).join(" ");
  return finish({ ...inspected.state, message });
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
  const first = await inspect(ctx, true);
  let state = first.state;
  if (state.state !== "not_connected" || !first.signedIn) return published(ctx, config, first);
  const url = connectorLinkUrl(config);
  try {
    await ctx.openExternal(url);
  } catch (error) {
    return finish({ ...state, state: "error", message: `Octopus could not open the ChatGPT page for ${config.name}: ${error?.message ?? error}` });
  }
  await ctx.progress({
    message: `Link ${config.name} on the ChatGPT page that opened and choose its account there. Octopus continues when ChatGPT reports it linked.`,
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
    const next = await inspect(ctx, true);
    state = next.state;
    if (state.state === "connected" || state.state === "expired" || !next.signedIn) return published(ctx, config, next);
  }
  return finish({
    ...state,
    state: state.state === "error" ? "error" : "not_connected",
    message: `${config.name} was not linked within ${LINK_TIMEOUT_MS / 60_000} minutes. Link it on ChatGPT, then try again.`,
  });
}

async function disconnect(ctx) {
  const { config, state: invalidState } = describedConfig(ctx);
  if (!config) return invalidState;
  const state = await probe(ctx);
  if (state.state !== "connected") return state;
  return finish({
    ...state,
    message: `Octopus cannot unlink ${config.name} from your ChatGPT account, so it stays linked. To disconnect it or change its account, choose ${MANAGE_LABEL} and do it there.`,
  });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
