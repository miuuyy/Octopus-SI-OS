"use strict";

const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const tls = require("node:tls");

const REQUEST_TIMEOUT_MS = 10_000;
const MAX_HEAD_BYTES = 65536;
const ENV_NAME = /^[A-Z_][A-Z0-9_]*$/;
const PATTERNS = {
  bot_token_env: /^xoxb-[0-9A-Za-z-]{15,4096}$/,
  app_token_env: /^xapp-[0-9A-Za-z-]{15,4096}$/,
  allowed_user_ids_env: /^[UW][A-Z0-9]{8,32}$/,
  allowed_channel_ids_env: /^[CDG][A-Z0-9]{8,32}$/,
  home_channel_env: /^[CDG][A-Z0-9]{8,32}$/,
};
const TEAM = /^T[A-Z0-9]{8,32}$/;
const APP_ID = /^A[A-Z0-9]{8,32}$/;
const LIST_KEYS = new Set(["bot_token_env", "allowed_user_ids_env", "allowed_channel_ids_env"]);
const FIELD_NAMES = {
  bot_token_env: "The bot token must start with xoxb-.",
  app_token_env: "The app-level token must start with xapp-.",
  allowed_user_ids_env: "Member IDs look like U0123ABCDE, separated by commas.",
  allowed_channel_ids_env: "Channel IDs look like C0123ABCDE, separated by commas.",
  home_channel_env: "The default channel ID looks like C0123ABCDE.",
};
const APPS_PAGE = "https://api.slack.com/apps";
const MANAGE_LABEL = "Manage in Slack";

class SlackFailure extends Error {
  constructor(state, message) {
    super(message);
    this.state = state;
  }
}

function clip(text, limit) {
  return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

function plural(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function environmentNames(descriptor) {
  const environment = descriptor?.interface_driver_config?.private_config?.environment;
  const names = {};
  for (const key of Object.keys(PATTERNS)) {
    if (typeof environment?.[key] !== "string" || !ENV_NAME.test(environment[key])) {
      throw new SlackFailure("error", `Slack interface does not declare private_config.environment.${key}.`);
    }
    names[key] = environment[key];
  }
  return names;
}

function settings() {
  let config;
  try {
    config = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "config.json"), "utf8"));
  } catch {
    throw new SlackFailure("error", "The Slack connector configuration cannot be read. Reinstall the Slack application.");
  }
  const transport = config?.transport;
  const authz = config?.authz;
  const names = [transport?.api_base_env, transport?.proxy_env, transport?.socket_url_env, authz?.allow_all_users_env, authz?.allowed_team_ids_env];
  const valid = names.every((name) => typeof name === "string" && ENV_NAME.test(name))
    && typeof transport.api_base_default === "string" && transport.api_base_default.startsWith("https://")
    && typeof authz.allow_all_users === "boolean"
    && ["allowed_user_ids", "allowed_channel_ids", "allowed_team_ids"].every((key) => Array.isArray(authz[key]) && authz[key].every((id) => typeof id === "string"));
  if (!valid) throw new SlackFailure("error", "The Slack connector configuration is incomplete. Reinstall the Slack application.");
  return {
    apiBaseEnv: transport.api_base_env,
    apiBaseDefault: transport.api_base_default,
    proxyEnv: transport.proxy_env,
    socketUrlEnv: transport.socket_url_env,
    allowAll: authz.allow_all_users,
    allowAllEnv: authz.allow_all_users_env,
    allowedUsers: authz.allowed_user_ids,
    allowedChannels: authz.allowed_channel_ids,
    allowedTeams: authz.allowed_team_ids,
    allowedTeamsEnv: authz.allowed_team_ids_env,
  };
}

async function effective(ctx, name) {
  const saved = ctx.values?.[name];
  const ambient = await ctx.launchEnvironment(name);
  if (typeof saved === "string" && typeof ambient === "string" && saved !== ambient) {
    throw new SlackFailure("error", `${name} is set in this Mac's environment and saved in Octopus with a different value, so the Slack interface cannot start. Remove it from the environment or connect with the same value.`);
  }
  const value = typeof ambient === "string" ? ambient : typeof saved === "string" ? saved : "";
  return value.trim();
}

async function entries(ctx, names, input) {
  const values = {};
  for (const name of Object.values(names)) {
    const typed = typeof input[name] === "string" && input[name].trim() ? input[name].trim() : null;
    const ambient = await ctx.launchEnvironment(name);
    if (typed !== null && typeof ambient === "string" && typed !== ambient.trim()) {
      throw new SlackFailure("error", `${name} is set in this Mac's environment to a different value, and the Slack interface uses that one. Remove it from the environment or enter the same value.`);
    }
    values[name] = typed ?? await effective(ctx, name);
  }
  return values;
}

function parsed(names, values, key) {
  const raw = values[names[key]];
  if (!raw) return [];
  const parts = LIST_KEYS.has(key) ? raw.split(",").map((part) => part.trim()).filter(Boolean) : [raw];
  if (!parts.length || parts.some((part) => !PATTERNS[key].test(part))) throw new SlackFailure("error", FIELD_NAMES[key]);
  return parts;
}

async function access(ctx, names, values, config) {
  const flag = await ctx.launchEnvironment(config.allowAllEnv);
  let everyone = config.allowAll;
  if (typeof flag === "string") {
    const value = flag.trim().toLowerCase();
    if (!["1", "true", "yes", "on", "0", "false", "no", "off"].includes(value)) {
      throw new SlackFailure("error", `${config.allowAllEnv} must be true or false.`);
    }
    everyone = ["1", "true", "yes", "on"].includes(value);
  }
  const teams = (await effective(ctx, config.allowedTeamsEnv)).split(",").map((part) => part.trim()).filter(Boolean);
  if (teams.some((team) => !TEAM.test(team))) {
    throw new SlackFailure("error", `${config.allowedTeamsEnv} must list Slack workspace IDs such as T0123ABCDE.`);
  }
  if (!values[names.bot_token_env]) return { missing: "Enter the bot token." };
  if (!values[names.app_token_env]) return { missing: "Enter the app-level token." };
  const bots = parsed(names, values, "bot_token_env");
  const app = parsed(names, values, "app_token_env")[0];
  const users = [...new Set([...config.allowedUsers, ...parsed(names, values, "allowed_user_ids_env")])];
  if (!users.length && !everyone) return { missing: "Enter the allowed member IDs." };
  const channels = [...new Set([...config.allowedChannels, ...parsed(names, values, "allowed_channel_ids_env")])];
  parsed(names, values, "home_channel_env");
  if (new Set(bots).size !== bots.length) throw new SlackFailure("error", "The bot token list repeats a token; keep each workspace's token once.");
  return { bots, app, users, channels, everyone, teams: [...new Set([...config.allowedTeams, ...teams])] };
}

function loopback(host) {
  const name = host.replace(/^\[|\]$/g, "").replace(/\.$/, "").toLowerCase();
  if (name === "localhost" || name.endsWith(".localhost")) return true;
  if (net.isIPv4(name)) return name.startsWith("127.");
  return name === "::1";
}

async function route(ctx, config) {
  const base = (await effective(ctx, config.apiBaseEnv)) || config.apiBaseDefault;
  let url;
  try {
    url = new URL(base);
  } catch {
    throw new SlackFailure("error", `${config.apiBaseEnv} is not a URL.`);
  }
  if (url.protocol !== "https:" || !url.hostname || url.username || url.password) {
    throw new SlackFailure("error", `${config.apiBaseEnv} must be an HTTPS address.`);
  }
  const proxyValue = await effective(ctx, config.proxyEnv);
  let proxy = null;
  if (proxyValue) {
    let parsedProxy;
    let username;
    let password;
    try {
      parsedProxy = new URL(proxyValue);
      username = parsedProxy.username ? decodeURIComponent(parsedProxy.username) : null;
      password = parsedProxy.password ? decodeURIComponent(parsedProxy.password) : null;
    } catch {
      throw new SlackFailure("error", `${config.proxyEnv} is not a proxy URL.`);
    }
    if (!["http:", "https:"].includes(parsedProxy.protocol) || !parsedProxy.hostname || !["", "/"].includes(parsedProxy.pathname)
      || parsedProxy.search || parsedProxy.hash || (username === null) !== (password === null)) {
      throw new SlackFailure("error", `${config.proxyEnv} must be an HTTP or HTTPS proxy URL without a path.`);
    }
    if (username !== null && username.includes(":")) {
      throw new SlackFailure("error", `The proxy user name in ${config.proxyEnv} must not contain a colon.`);
    }
    if (parsedProxy.protocol !== "https:" && !loopback(parsedProxy.hostname)) {
      throw new SlackFailure("error", `${config.proxyEnv} must use an HTTPS proxy for a proxy on another computer.`);
    }
    proxy = {
      https: parsedProxy.protocol === "https:",
      host: parsedProxy.hostname.replace(/^\[|\]$/g, ""),
      port: Number(parsedProxy.port) || (parsedProxy.protocol === "https:" ? 443 : 80),
      username,
      password,
    };
  }
  const socket = await effective(ctx, config.socketUrlEnv);
  if (socket) {
    let socketUrl;
    try {
      socketUrl = new URL(socket);
    } catch {
      socketUrl = null;
    }
    if (socketUrl?.protocol !== "wss:" || !socketUrl.hostname || socketUrl.username || socketUrl.password) {
      throw new SlackFailure("error", `${config.socketUrlEnv} must be an absolute wss address.`);
    }
  }
  return {
    base: base.replace(/\/+$/, ""),
    host: url.hostname.replace(/^\[|\]$/g, ""),
    port: Number(url.port) || 443,
    authority: url.host,
    prefix: url.pathname.replace(/\/+$/, ""),
    proxy,
    proxyEnv: config.proxyEnv,
    socketOverride: Boolean(socket),
  };
}

function waitFor(socket, event, deadline) {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      clearTimeout(timer);
      socket.off("error", fail);
      socket.off(event, done);
    };
    const fail = (error) => {
      cleanup();
      socket.destroy();
      reject(error);
    };
    const done = () => {
      cleanup();
      resolve();
    };
    const timer = setTimeout(() => fail(new Error("timed out")), Math.max(1, deadline - Date.now()));
    socket.once("error", fail);
    socket.once(event, done);
  });
}

function collect(socket, ready, deadline) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    const cleanup = () => {
      clearTimeout(timer);
      socket.off("data", onData).off("end", onEnd).off("close", onEnd).off("error", onError);
    };
    const settle = (ended) => {
      try {
        const value = ready(buffer, ended);
        if (value !== undefined) {
          cleanup();
          resolve(value);
        } else if (ended) {
          cleanup();
          reject(new Error("the connection closed early"));
        }
      } catch (error) {
        cleanup();
        reject(error);
      }
    };
    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      settle(false);
    };
    const onEnd = () => settle(true);
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    const timer = setTimeout(() => onError(new Error("timed out")), Math.max(1, deadline - Date.now()));
    socket.on("data", onData).on("end", onEnd).on("close", onEnd).on("error", onError);
  });
}

function chunks(body) {
  const parts = [];
  for (let offset = 0; ;) {
    const end = body.indexOf("\r\n", offset);
    if (end < 0) return undefined;
    const size = parseInt(body.subarray(offset, end).toString("latin1"), 16);
    if (!Number.isSafeInteger(size) || size < 0) throw new Error("malformed chunked answer");
    if (size === 0) return Buffer.concat(parts);
    if (end + 4 + size > body.length) return undefined;
    parts.push(body.subarray(end + 2, end + 2 + size));
    offset = end + 4 + size;
  }
}

function response(buffer, ended) {
  const end = buffer.indexOf("\r\n\r\n");
  if (end < 0) {
    if (buffer.length > MAX_HEAD_BYTES) throw new Error("answer header too large");
    return undefined;
  }
  const [line, ...fields] = buffer.subarray(0, end).toString("latin1").split("\r\n");
  const status = /^HTTP\/1\.[01] ([0-9]{3})/.exec(line);
  if (!status) throw new Error("malformed answer");
  const headers = new Map(fields.map((field) => [field.slice(0, field.indexOf(":")).trim().toLowerCase(), field.slice(field.indexOf(":") + 1).trim()]));
  const body = buffer.subarray(end + 4);
  if (/\bchunked\b/i.test(headers.get("transfer-encoding") ?? "")) {
    const decoded = chunks(body);
    return decoded && { status: Number(status[1]), text: decoded.toString("utf8") };
  }
  if (headers.has("content-length")) {
    const length = Number(headers.get("content-length"));
    if (!Number.isSafeInteger(length) || length < 0) throw new Error("invalid answer length");
    return body.length >= length ? { status: Number(status[1]), text: body.subarray(0, length).toString("utf8") } : undefined;
  }
  return ended ? { status: Number(status[1]), text: body.toString("utf8") } : undefined;
}

async function throughProxy(ctx, route, method, token) {
  const deadline = Date.now() + REQUEST_TIMEOUT_MS;
  const { proxy } = route;
  let socket = proxy.https
    ? tls.connect({ host: proxy.host, port: proxy.port, servername: net.isIP(proxy.host) ? undefined : proxy.host })
    : net.connect({ host: proxy.host, port: proxy.port });
  const abort = () => socket.destroy(new Error("cancelled"));
  ctx.signal?.addEventListener?.("abort", abort, { once: true });
  try {
    await waitFor(socket, proxy.https ? "secureConnect" : "connect", deadline);
    const authority = `${net.isIPv6(route.host) ? `[${route.host}]` : route.host}:${route.port}`;
    const lines = [`CONNECT ${authority} HTTP/1.1`, `Host: ${authority}`];
    if (proxy.username !== null) lines.push(`Proxy-Authorization: Basic ${Buffer.from(`${proxy.username}:${proxy.password}`, "utf8").toString("base64")}`);
    socket.write(`${lines.join("\r\n")}\r\n\r\n`);
    await collect(socket, (buffer) => {
      const end = buffer.indexOf("\r\n\r\n");
      if (end < 0) {
        if (buffer.length > MAX_HEAD_BYTES) throw new Error("CONNECT answer too large");
        return undefined;
      }
      if (/^HTTP\/1\.[01] ([0-9]{3})/.exec(buffer.subarray(0, end).toString("latin1"))?.[1] !== "200" || buffer.length !== end + 4) {
        throw new SlackFailure("error", `The proxy in ${route.proxyEnv} refused to connect to Slack.`);
      }
      return true;
    }, deadline);
    socket = tls.connect({ socket, host: route.host, servername: net.isIP(route.host) ? undefined : route.host });
    await waitFor(socket, "secureConnect", deadline);
    const head = [
      `POST ${route.prefix}/${method} HTTP/1.1`,
      `Host: ${route.authority}`,
      "User-Agent: octopus-world-slack/1",
      "Accept: application/json",
      `Authorization: Bearer ${token}`,
      "Content-Type: application/x-www-form-urlencoded",
      "Content-Length: 0",
      "Connection: close",
    ];
    socket.write(`${head.join("\r\n")}\r\n\r\n`);
    return await collect(socket, response, deadline);
  } finally {
    ctx.signal?.removeEventListener?.("abort", abort);
    socket.destroy();
  }
}

async function directly(ctx, route, method, token) {
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  const answer = await ctx.fetch(`${route.base}/${method}`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/x-www-form-urlencoded" },
    body: "",
    redirect: "error",
    signal: ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout,
  });
  return { status: answer.status, text: await answer.text() };
}

function refusal(code, label) {
  const messages = {
    invalid_auth: `Slack rejected the ${label}.`,
    not_authed: `Slack received no ${label}.`,
    account_inactive: "The Slack app or its workspace is inactive.",
    token_revoked: `The ${label} was revoked. Reinstall the app and copy the new token.`,
    not_allowed_token_type: `The ${label} is the wrong type of token for this field.`,
    missing_scope: `The ${label} lacks a scope Slack needs${label === "app-level token" ? ": add connections:write" : ""}.`,
  };
  const state = ["invalid_auth", "token_revoked", "account_inactive"].includes(code) ? "expired" : "error";
  return new SlackFailure(state, messages[code] ?? `Slack refused the ${label} (${code}).`);
}

async function call(ctx, route, method, token, label) {
  let answer;
  try {
    answer = route.proxy ? await throughProxy(ctx, route, method, token) : await directly(ctx, route, method, token);
  } catch (error) {
    if (ctx.signal?.aborted || error instanceof SlackFailure) throw error;
    throw new SlackFailure("error", route.proxy ? `Could not reach Slack through the proxy in ${route.proxyEnv}.` : "Could not reach Slack.");
  }
  if (answer.status === 429) throw new SlackFailure("error", "Slack is rate limiting this check. Try again in a minute.");
  if (answer.status < 200 || answer.status > 299) throw new SlackFailure("error", `Slack answered HTTP ${answer.status}.`);
  let body;
  try {
    body = JSON.parse(answer.text);
  } catch {
    throw new SlackFailure("error", `Slack answered ${method} with data that is not JSON.`);
  }
  if (body?.ok !== true) throw refusal(typeof body?.error === "string" ? body.error : "unknown_error", label);
  return body;
}

function socketAppId(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    url = null;
  }
  if (url?.protocol !== "wss:" || !url.hostname) throw new SlackFailure("error", "Slack answered without a Socket Mode address for this app.");
  const id = url.searchParams.get("app_id");
  return APP_ID.test(id ?? "") ? id : null;
}

function manage(appId) {
  return { label: MANAGE_LABEL, url: appId ? `${APPS_PAGE}/${appId}` : APPS_PAGE };
}

function identity(response) {
  if (!TEAM.test(response.team_id ?? "") || !PATTERNS.allowed_user_ids_env.test(response.user_id ?? "")) {
    throw new SlackFailure("error", "Slack did not identify the bot and its workspace for this token.");
  }
  return { team: typeof response.team === "string" && response.team ? response.team : response.team_id, teamId: response.team_id, bot: typeof response.user === "string" && response.user ? response.user : response.user_id };
}

function account(identities, allowed, route) {
  const members = allowed.everyone ? "every member" : allowed.users.length === 1 ? `member ${allowed.users[0]}` : plural(allowed.users.length, "allowed member");
  const channels = allowed.channels.length === 1 ? ` in channel ${allowed.channels[0]}` : allowed.channels.length ? ` in ${plural(allowed.channels.length, "allowed channel")}` : "";
  return {
    label: clip(identities.map((entry) => `@${entry.bot} in ${entry.team}`).join(", "), 96),
    detail: clip(`Through Socket Mode${route.proxy ? " via a proxy" : ""}, for ${members}${channels}`, 160),
  };
}

async function check(ctx, values, names, config) {
  const allowed = await access(ctx, names, values, config);
  if (allowed.missing) return { state: "not_connected", message: allowed.missing };
  const path = await route(ctx, config);
  const identities = [];
  for (const token of allowed.bots) identities.push(identity(await call(ctx, path, "auth.test", token, "bot token")));
  const teams = identities.map((entry) => entry.teamId);
  if (new Set(teams).size !== teams.length) {
    throw new SlackFailure("error", "Two bot tokens belong to the same workspace; keep one token per workspace.");
  }
  const shown = account(identities, allowed, path);
  const outside = identities.filter((entry) => allowed.teams.length && !allowed.teams.includes(entry.teamId));
  if (outside.length) {
    return { state: "error", account: shown, manage: manage(null), message: `${config.allowedTeamsEnv} does not include ${outside.map((entry) => entry.team).join(", ")}, so messages from there never reach tasks.` };
  }
  let appId = null;
  if (!path.socketOverride) {
    try {
      appId = socketAppId((await call(ctx, path, "apps.connections.open", allowed.app, "app-level token")).url);
    } catch (error) {
      if (error instanceof SlackFailure) return { state: error.state, account: shown, manage: manage(null), message: error.message };
      throw error;
    }
  }
  return { state: "connected", account: shown, manage: manage(appId) };
}

async function guarded(operation) {
  try {
    const state = await operation();
    return state.message ? { ...state, message: clip(state.message, 300) } : state;
  } catch (error) {
    if (error instanceof SlackFailure) return { state: error.state, message: clip(error.message, 300) };
    throw error;
  }
}

async function probe(ctx) {
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    const config = settings();
    return check(ctx, await entries(ctx, names, {}), names, config);
  });
}

async function connect(ctx) {
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    const config = settings();
    const input = Object.fromEntries(Object.entries(ctx.input ?? {}).filter(([, value]) => typeof value === "string" && value.trim()).map(([name, value]) => [name, value.trim()]));
    const result = await check(ctx, await entries(ctx, names, input), names, config);
    if (result.state !== "connected" || !Object.keys(input).length) return result;
    const written = {};
    for (const [name, value] of Object.entries(input)) written[name] = typeof (await ctx.launchEnvironment(name)) === "string" ? null : value;
    await ctx.saveValues(written);
    return result;
  });
}

async function disconnect(ctx) {
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    await ctx.saveValues(Object.fromEntries(Object.values(names).map((name) => [name, null])));
    const kept = [];
    for (const name of [names.bot_token_env, names.app_token_env]) {
      if (typeof (await ctx.launchEnvironment(name)) === "string") kept.push(name);
    }
    const removed = "Slack is disconnected: Octopus removed the tokens and member settings from this Mac.";
    if (!kept.length) {
      return { state: "not_connected", manage: manage(null), message: `${removed} The Slack app stays installed in its workspace; uninstall it or regenerate its tokens in the app's Slack settings.` };
    }
    const listed = `${kept.join(" and ")} ${kept.length === 1 ? "is" : "are"} set in this Mac's environment`;
    const state = await probe(ctx);
    if (state.state === "error") return { ...state, message: `Octopus removed the saved Slack settings, but ${listed} and checking ${kept.length === 1 ? "it" : "them"} failed: ${state.message}` };
    if (state.state === "not_connected") {
      return { ...state, manage: manage(null), message: `${removed} ${listed}, but without the removed settings no Slack message reaches tasks. The Slack app stays installed in its workspace.` };
    }
    return {
      ...state,
      message: `Octopus removed the saved Slack settings, but ${listed}, so the Slack interface keeps using ${kept.length === 1 ? "it" : "them"}. Remove ${kept.length === 1 ? "it" : "them"} there to disconnect.`,
    };
  });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
