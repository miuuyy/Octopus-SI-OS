"use strict";

const crypto = require("node:crypto");
const dns = require("node:dns/promises");
const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const tls = require("node:tls");
const { domainToASCII } = require("node:url");
const { setTimeout: delay } = require("node:timers/promises");

const TOKEN = /^[0-9]{1,20}:[A-Za-z0-9_-]{20,128}$/;
const NUMERIC_ID = /^-?[1-9][0-9]{0,15}$/;
const CHAT_ID = /^(?:-?[1-9][0-9]{0,15}|@[A-Za-z][A-Za-z0-9_]{3,31})$/;
const ENV_NAME = /^[A-Z_][A-Z0-9_]*$/;
const CODE_ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const PAIRING_WINDOW_MS = 300_000;
const POLL_SECONDS = 20;
const PROBE_BUDGET_MS = 25_000;
const ANSWER_RESERVE_MS = 5_000;
const MAX_HEAD_BYTES = 65536;
const CONFIRMATION = "Octopus is connected. Messages you send here reach the tasks you connect to Telegram.";
const CONFIG_PATH = path.join(__dirname, "..", "config.json");
const MANAGE = { label: "Manage in BotFather", url: "https://t.me/BotFather" };

class Cancelled extends Error {}

class Problem extends Error {}

class Timeout extends Error {}

class BotApiError extends Error {
  constructor(method, status, body) {
    const code = Number.isInteger(body?.error_code) ? body.error_code : status;
    super(`Telegram ${method} failed with ${code}`);
    this.code = code;
    this.description = typeof body?.description === "string" ? body.description : "";
    this.retryAfter = Number.isFinite(body?.parameters?.retry_after) ? body.parameters.retry_after : null;
  }
}

function redact(text) {
  return String(text).replace(/(?:bot)?[0-9]{5,20}:[A-Za-z0-9_-]{30,128}/g, "[token]").slice(0, 300);
}

function clip(text, limit) {
  return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

function plural(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function settings() {
  let config;
  try {
    config = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf8"));
  } catch {
    throw new Problem("The Telegram connector configuration cannot be read. Reinstall the Telegram application.");
  }
  const transport = config?.transport;
  const authz = config?.authz;
  const names = [transport?.api_base_env, transport?.proxy_env, transport?.fallback_ips_env, authz?.allow_all_users_env, authz?.allowed_chat_ids_env];
  const valid = names.every((name) => typeof name === "string" && ENV_NAME.test(name))
    && typeof transport.api_base_default === "string"
    && Array.isArray(transport.fallback_ips) && transport.fallback_ips.every((address) => typeof address === "string")
    && Number.isSafeInteger(transport.connect_timeout_ms) && transport.connect_timeout_ms > 0
    && Number.isSafeInteger(transport.max_response_bytes) && transport.max_response_bytes > 0
    && typeof authz.allow_all_users === "boolean"
    && Array.isArray(authz.allowed_user_ids) && Array.isArray(authz.allowed_chat_ids);
  if (!valid) throw new Problem("The Telegram connector configuration is incomplete. Reinstall the Telegram application.");
  return {
    apiBaseEnv: transport.api_base_env,
    apiBaseDefault: transport.api_base_default,
    proxyEnv: transport.proxy_env,
    fallbackIpsEnv: transport.fallback_ips_env,
    fallbackIps: transport.fallback_ips,
    connectTimeoutMs: transport.connect_timeout_ms,
    maxResponseBytes: transport.max_response_bytes,
    allowAll: authz.allow_all_users,
    allowAllEnv: authz.allow_all_users_env,
    allowedUsers: authz.allowed_user_ids.map(String),
    allowedChats: authz.allowed_chat_ids.map(String),
    allowedChatsEnv: authz.allowed_chat_ids_env,
  };
}

function environmentNames(descriptor) {
  const environment = descriptor?.interface_driver_config?.private_config?.environment;
  const names = { token: environment?.bot_token_env, users: environment?.allowed_user_ids_env, home: environment?.home_chat_env };
  const values = Object.values(names);
  if (values.some((name) => typeof name !== "string" || !ENV_NAME.test(name)) || new Set(values).size !== values.length) {
    throw new Problem(`Interface '${descriptor?.id}' does not declare its Telegram settings.`);
  }
  return names;
}

async function stored(ctx, name) {
  const saved = ctx.values?.[name];
  const ambient = await ctx.launchEnvironment(name);
  if (typeof saved === "string" && typeof ambient === "string" && saved !== ambient) {
    throw new Problem(`${name} is set in this Mac's environment and saved in Octopus with a different value, so the Telegram interface cannot start. Remove it from the environment or connect with the same value.`);
  }
  const value = typeof ambient === "string" ? ambient : typeof saved === "string" ? saved : "";
  return value.trim();
}

async function inherited(ctx, name, value, same = (left, right) => left === right) {
  const ambient = await ctx.launchEnvironment(name);
  if (typeof ambient !== "string") return false;
  if (value && !same(value, ambient.trim())) {
    throw new Problem(`${name} is set in this Mac's environment to a different value, and the Telegram interface uses that one. Remove it from the environment or enter the same value.`);
  }
  return true;
}

async function save(ctx, changes) {
  const written = {};
  for (const [name, value] of Object.entries(changes)) written[name] = typeof (await ctx.launchEnvironment(name)) === "string" ? null : value;
  await ctx.saveValues(written);
}

function typed(ctx, name) {
  return Object.prototype.hasOwnProperty.call(ctx.input ?? {}, name) ? String(ctx.input[name] ?? "").trim() : undefined;
}

function parseIds(text, problem) {
  const parts = String(text ?? "").split(/[\s,]+/).filter(Boolean);
  if (parts.some((part) => !NUMERIC_ID.test(part) || Math.abs(Number(part)) >= 2 ** 53)) throw new Problem(problem);
  return [...new Set(parts)];
}

function parseUsers(text) {
  const users = parseIds(text, "Allowed user IDs must be numeric Telegram user IDs separated by commas.");
  if (users.some((user) => user.startsWith("-"))) throw new Problem("Allowed user IDs must be numeric Telegram user IDs separated by commas.");
  return users;
}

function parseChat(text) {
  if (!CHAT_ID.test(text)) throw new Problem("Home chat ID must be a numeric Telegram chat ID or a public @username.");
  return text;
}

function parseFlag(text, name) {
  if (!text) return null;
  const value = text.toLowerCase();
  if (["1", "true", "yes", "on"].includes(value)) return true;
  if (["0", "false", "no", "off"].includes(value)) return false;
  throw new Problem(`${name} must be true or false.`);
}

async function allowed(ctx, names, config, users) {
  const chats = parseIds(await stored(ctx, config.allowedChatsEnv), `${config.allowedChatsEnv} must list numeric Telegram chat IDs separated by commas.`);
  const everyone = parseFlag(await stored(ctx, config.allowAllEnv), config.allowAllEnv);
  const home = await stored(ctx, names.home);
  if (home) parseChat(home);
  return {
    users: [...new Set([...config.allowedUsers, ...(users ?? parseUsers(await stored(ctx, names.users)))])],
    chats: [...new Set([...config.allowedChats, ...chats])],
    everyone: everyone ?? config.allowAll,
  };
}

function reachable(access) {
  return access.everyone || access.users.length > 0 || access.chats.length > 0;
}

function isId(value) {
  return Number.isSafeInteger(value) && value !== 0;
}

function bot(me) {
  if (me?.is_bot !== true || !isId(me.id) || typeof me.username !== "string" || !me.username) {
    throw new Problem("Telegram did not identify this token as a bot.");
  }
  return me;
}

function displayName(user) {
  const name = [user?.first_name, user?.last_name].filter((part) => typeof part === "string" && part.trim()).join(" ");
  return name || (typeof user?.username === "string" ? `@${user.username}` : `user ${user?.id}`);
}

function account(me, access, route, pairedWith) {
  const parts = [];
  if (access.users.length === 1) parts.push(`allowed user ${access.users[0]}`);
  else if (access.users.length) parts.push(plural(access.users.length, "allowed user"));
  if (access.chats.length === 1) parts.push(`allowed chat ${access.chats[0]}`);
  else if (access.chats.length) parts.push(plural(access.chats.length, "allowed chat"));
  const who = pairedWith ? `paired with ${pairedWith}`
    : access.everyone ? "for every Telegram user"
      : parts.length ? `for ${parts.join(" and ")}` : "with no allowed users yet";
  const where = route.official ? "Through the Telegram Bot API" : `Through the Bot API server at ${route.base.authority}`;
  return { label: clip(`@${me.username}`, 96), detail: clip(`${where}${route.proxy ? " via a proxy" : ""}, ${who}`, 160) };
}

function loopback(host) {
  const name = host.replace(/^\[|\]$/g, "").replace(/\.$/, "").toLowerCase();
  if (name === "localhost" || name.endsWith(".localhost")) return true;
  if (net.isIPv4(name)) return name.startsWith("127.");
  return name === "::1";
}

function publicIpv4(value) {
  if (!net.isIPv4(value)) return false;
  const [a, b, c, d] = value.split(".").map(Number);
  if (a === 192 && b === 0 && c === 0) return d === 9 || d === 10;
  return !(a === 0 || a === 10 || a === 127 || a >= 224 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b < 32)
    || (a === 192 && b === 168) || (a === 192 && b === 0 && c === 2) || (a === 198 && (b === 18 || b === 19))
    || (a === 198 && b === 51 && c === 100) || (a === 203 && b === 0 && c === 113));
}

function parseBase(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Problem("The Telegram Bot API address is not a URL.");
  }
  const https = url.protocol === "https:";
  if ((!https && !(url.protocol === "http:" && loopback(url.hostname))) || !url.hostname || url.username || url.password || url.search || url.hash) {
    throw new Problem("The Telegram Bot API address must be an HTTPS origin.");
  }
  return {
    https,
    host: url.hostname.replace(/^\[|\]$/g, ""),
    port: Number(url.port) || (https ? 443 : 80),
    authority: url.host,
    prefix: url.pathname.replace(/\/+$/, ""),
  };
}

function parseProxy(value, name) {
  if (!value) return null;
  let url;
  let username;
  let password;
  try {
    url = new URL(value);
    username = url.username ? decodeURIComponent(url.username) : null;
    password = url.password ? decodeURIComponent(url.password) : null;
  } catch {
    throw new Problem(`${name} is not a proxy URL.`);
  }
  const scheme = url.protocol.slice(0, -1);
  if (!["http", "https", "socks5", "socks5h"].includes(scheme) || !url.hostname || !["", "/"].includes(url.pathname) || url.search || url.hash) {
    throw new Problem(`${name} must be an http, https, socks5 or socks5h URL without a path.`);
  }
  if ((username === null) !== (password === null)) throw new Problem(`${name} needs both a user name and a password, or neither.`);
  if (scheme !== "https" && !loopback(url.hostname)) throw new Problem(`${name} must use an HTTPS proxy for a proxy on another computer.`);
  const host = url.hostname.replace(/^\[|\]$/g, "");
  return { scheme, host, port: Number(url.port) || ({ http: 80, https: 443 }[scheme] ?? 1080), username, password };
}

async function route(ctx, config, token, deadline = Infinity) {
  const base = parseBase((await stored(ctx, config.apiBaseEnv)) || config.apiBaseDefault);
  const official = parseBase(config.apiBaseDefault);
  const proxy = parseProxy(await stored(ctx, config.proxyEnv), config.proxyEnv);
  const override = await stored(ctx, config.fallbackIpsEnv);
  const listed = override ? override.split(/[\s,]+/).filter(Boolean) : config.fallbackIps;
  if (!listed.every(publicIpv4)) {
    throw new Problem(`${override ? config.fallbackIpsEnv : "The Telegram fallback addresses"} must list public IPv4 addresses.`);
  }
  const isOfficial = base.https && base.host === official.host;
  return {
    token,
    base,
    proxy,
    proxyEnv: config.proxyEnv,
    official: isOfficial,
    fallback: isOfficial ? [...new Set(listed)] : [],
    deadline,
    connectTimeoutMs: config.connectTimeoutMs,
    maxResponseBytes: config.maxResponseBytes,
  };
}

class Bytes {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.ended = false;
    this.failure = null;
    this.wake = null;
    this.onData = (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.notify();
    };
    this.onEnd = () => {
      this.ended = true;
      this.notify();
    };
    this.onError = (error) => {
      this.failure ??= error;
      this.notify();
    };
    socket.on("data", this.onData).on("end", this.onEnd).on("close", this.onEnd).on("error", this.onError);
  }

  notify() {
    const wake = this.wake;
    this.wake = null;
    wake?.();
  }

  async wait(ready) {
    for (;;) {
      const value = ready(this.buffer, this.ended);
      if (value !== undefined) return value;
      if (this.failure) throw this.failure;
      if (this.ended) throw new Error("the connection closed early");
      await new Promise((resolve) => { this.wake = resolve; });
    }
  }

  async take(count) {
    const bytes = await this.wait((buffer) => (buffer.length >= count ? Buffer.from(buffer.subarray(0, count)) : undefined));
    this.buffer = this.buffer.subarray(count);
    return bytes;
  }

  release() {
    this.socket.off("data", this.onData).off("end", this.onEnd).off("close", this.onEnd).off("error", this.onError);
  }
}

function ready(socket, event, deadline) {
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
      resolve(socket);
    };
    const timer = setTimeout(() => fail(new Timeout("connect timed out")), Math.max(1, deadline - Date.now()));
    socket.once("error", fail);
    socket.once(event, done);
  });
}

async function handshake(socket, deadline, work) {
  const bytes = new Bytes(socket);
  const timer = setTimeout(() => socket.destroy(new Timeout("the proxy did not answer in time")), Math.max(1, deadline - Date.now()));
  try {
    return await work(bytes);
  } finally {
    clearTimeout(timer);
    bytes.release();
  }
}

function ipv6Bytes(address) {
  const [head, tail] = address.split("::");
  const groups = (text) => (text ? text.split(":") : []);
  const words = tail === undefined ? groups(head) : [...groups(head), ...Array(8 - groups(head).length - groups(tail).length).fill("0"), ...groups(tail)];
  return words.flatMap((word) => [parseInt(word, 16) >> 8, parseInt(word, 16) & 255]);
}

async function lookup(host, options, milliseconds) {
  let timer;
  const expired = new Promise((resolve, reject) => {
    timer = setTimeout(() => reject(new Timeout(`looking up ${host} took too long`)), Math.max(1, milliseconds));
  });
  try {
    return await Promise.race([dns.lookup(host, options), expired]);
  } finally {
    clearTimeout(timer);
  }
}

function share(route, count) {
  return Math.min(route.connectTimeoutMs, (route.deadline - ANSWER_RESERVE_MS - Date.now()) / count);
}

async function socksAddress(proxy, target, deadline) {
  if (net.isIPv4(target)) return [1, ...target.split(".").map(Number)];
  if (net.isIPv6(target)) return [4, ...ipv6Bytes(target)];
  if (proxy.scheme === "socks5h") {
    const name = Buffer.from(domainToASCII(target));
    if (!name.length || name.length > 255) throw new Problem("The Telegram Bot API host cannot be sent through a SOCKS5 proxy.");
    return [3, name.length, ...name];
  }
  return socksAddress({ scheme: "socks5" }, (await lookup(target, {}, deadline - Date.now())).address, deadline);
}

async function socks(socket, proxy, target, port, deadline) {
  const address = await socksAddress(proxy, target, deadline);
  await handshake(socket, deadline, async (bytes) => {
    const credentials = proxy.username !== null;
    socket.write(Buffer.from(credentials ? [5, 2, 0, 2] : [5, 1, 0]));
    const [version, method] = await bytes.take(2);
    if (version !== 5 || ![0, 2].includes(method) || (method === 2 && !credentials)) throw new Error("the SOCKS5 proxy refused the offered sign-in");
    if (method === 2) {
      const user = Buffer.from(proxy.username, "utf8");
      const secret = Buffer.from(proxy.password, "utf8");
      if (user.length > 255 || secret.length > 255) throw new Problem("The SOCKS5 proxy credentials are longer than 255 bytes.");
      socket.write(Buffer.concat([Buffer.from([1, user.length]), user, Buffer.from([secret.length]), secret]));
      const [, status] = await bytes.take(2);
      if (status !== 0) throw new Error("the SOCKS5 proxy rejected its credentials");
    }
    socket.write(Buffer.from([5, 1, 0, ...address, port >> 8, port & 255]));
    const [version2, status, , kind] = await bytes.take(4);
    if (version2 !== 5 || status !== 0) throw new Error(`the SOCKS5 proxy could not connect (status ${status})`);
    if (![1, 3, 4].includes(kind)) throw new Error("the SOCKS5 proxy answered with an invalid address type");
    await bytes.take((kind === 1 ? 4 : kind === 4 ? 16 : (await bytes.take(1))[0]) + 2);
  });
}

async function tunnel(socket, proxy, target, port, deadline) {
  await handshake(socket, deadline, async (bytes) => {
    const authority = net.isIPv6(target) ? `[${target}]:${port}` : `${target}:${port}`;
    const lines = [`CONNECT ${authority} HTTP/1.1`, `Host: ${authority}`, "Proxy-Connection: keep-alive"];
    if (proxy.username !== null) lines.push(`Proxy-Authorization: Basic ${Buffer.from(`${proxy.username}:${proxy.password}`, "utf8").toString("base64")}`);
    socket.write(`${lines.join("\r\n")}\r\n\r\n`);
    const end = await bytes.wait((buffer) => {
      const index = buffer.indexOf("\r\n\r\n");
      if (index < 0 && buffer.length > MAX_HEAD_BYTES) throw new Error("the proxy answered CONNECT with too much data");
      return index < 0 ? undefined : index;
    });
    const status = /^HTTP\/1\.[01] ([0-9]{3})/.exec(bytes.buffer.subarray(0, end).toString("latin1"));
    if (status?.[1] !== "200" || bytes.buffer.length !== end + 4) throw new Error(`the proxy refused CONNECT${status ? ` with status ${status[1]}` : ""}`);
  });
}

async function open(route, target, signal, slotMs) {
  const deadline = Date.now() + slotMs;
  const { base, proxy } = route;
  let socket = null;
  const abort = () => socket?.destroy(new Cancelled());
  signal?.addEventListener?.("abort", abort, { once: true });
  const step = (next, event) => {
    socket = next;
    return ready(next, event, deadline);
  };
  try {
    if (!proxy) {
      await step(net.connect({ host: target, port: base.port }), "connect");
    } else {
      await (proxy.scheme === "https"
        ? step(tls.connect({ host: proxy.host, port: proxy.port, servername: net.isIP(proxy.host) ? undefined : proxy.host }), "secureConnect")
        : step(net.connect({ host: proxy.host, port: proxy.port }), "connect"));
      await (proxy.scheme.startsWith("socks5") ? socks(socket, proxy, target, base.port, deadline) : tunnel(socket, proxy, target, base.port, deadline));
    }
    if (base.https) await step(tls.connect({ socket, host: base.host, servername: net.isIP(base.host) ? undefined : base.host }), "secureConnect");
    return socket;
  } catch (error) {
    socket?.destroy();
    throw error;
  } finally {
    signal?.removeEventListener?.("abort", abort);
  }
}

function chunks(body) {
  const parts = [];
  for (let offset = 0; ;) {
    const end = body.indexOf("\r\n", offset);
    if (end < 0) return undefined;
    const size = parseInt(body.subarray(offset, end).toString("latin1"), 16);
    if (!Number.isSafeInteger(size) || size < 0) throw new Error("Telegram sent a malformed chunked answer");
    if (size === 0) return Buffer.concat(parts);
    if (end + 4 + size > body.length) return undefined;
    parts.push(body.subarray(end + 2, end + 2 + size));
    offset = end + 4 + size;
  }
}

function response(buffer, ended, limit) {
  const end = buffer.indexOf("\r\n\r\n");
  if (end < 0) {
    if (buffer.length > MAX_HEAD_BYTES) throw new Error("Telegram sent an answer header that is too large");
    return undefined;
  }
  const [line, ...fields] = buffer.subarray(0, end).toString("latin1").split("\r\n");
  const status = /^HTTP\/1\.[01] ([0-9]{3})/.exec(line);
  if (!status) throw new Error("Telegram sent a malformed answer");
  const headers = new Map(fields.map((field) => [field.slice(0, field.indexOf(":")).trim().toLowerCase(), field.slice(field.indexOf(":") + 1).trim()]));
  const body = buffer.subarray(end + 4);
  if (body.length > limit) throw new Error("Telegram sent more data than this application allows");
  if (/\bchunked\b/i.test(headers.get("transfer-encoding") ?? "")) {
    const decoded = chunks(body);
    return decoded && { status: Number(status[1]), body: decoded };
  }
  if (headers.has("content-length")) {
    const length = Number(headers.get("content-length"));
    if (!Number.isSafeInteger(length) || length < 0 || length > limit) throw new Error("Telegram sent an invalid answer length");
    return body.length >= length ? { status: Number(status[1]), body: body.subarray(0, length) } : undefined;
  }
  return ended ? { status: Number(status[1]), body } : undefined;
}

function unreachable(route, failures) {
  const where = route.official ? "Telegram" : `the Bot API server at ${route.base.authority}`;
  const untrusted = failures.find((error) => /CERT|SELF_SIGNED|UNABLE_TO_|ALTNAME|HOSTNAME/i.test(String(error?.code ?? "")));
  if (untrusted) return `The certificate of ${route.base.host} is not trusted (${untrusted.code}), so Octopus did not send the bot token.`;
  if (failures.length && failures.every((error) => error instanceof Timeout)) {
    return `${where} did not answer in time${route.proxy ? ` through the proxy in ${route.proxyEnv}` : ""}. Check the internet connection and try again.`;
  }
  return route.proxy
    ? `Octopus could not reach ${where} through the proxy in ${route.proxyEnv}. Check the proxy and try again.`
    : `Octopus could not reach ${where}. Check the internet connection, or set a proxy in ${route.proxyEnv}, and try again.`;
}

async function exchange(route, method, payload, timeoutMs, signal) {
  const head = [
    `POST ${route.base.prefix}/bot${route.token}/${method} HTTP/1.1`,
    `Host: ${route.base.authority}`,
    "User-Agent: octopus-world-telegram/1",
    "Accept: application/json",
    "Content-Type: application/json",
    `Content-Length: ${payload.length}`,
    "Connection: close",
  ];
  const request = Buffer.concat([Buffer.from(`${head.join("\r\n")}\r\n\r\n`, "utf8"), payload]);
  const targets = route.proxy ? [route.base.host] : [...new Set([
    ...(await lookup(route.base.host, { all: true }, share(route, route.fallback.length + 1)).catch(() => []))
      .sort((left, right) => (left.family === 4 ? 0 : 1) - (right.family === 4 ? 0 : 1))
      .map((entry) => entry.address),
    ...route.fallback,
  ])];
  const walk = targets.length ? targets : [route.base.host];
  const failures = [];
  for (const [index, target] of walk.entries()) {
    const slot = share(route, walk.length - index);
    if (slot < 1) {
      failures.push(new Timeout("no time was left for this address"));
      break;
    }
    let socket;
    try {
      socket = await open(route, target, signal, slot);
    } catch (error) {
      if (signal?.aborted || error instanceof Cancelled) throw new Cancelled();
      if (error instanceof Problem) throw error;
      failures.push(error);
      continue;
    }
    const bytes = new Bytes(socket);
    const timer = setTimeout(() => socket.destroy(new Timeout("Telegram did not answer in time")), Math.max(1, Math.min(timeoutMs, route.deadline - Date.now())));
    const abort = () => socket.destroy(new Cancelled());
    signal?.addEventListener?.("abort", abort, { once: true });
    try {
      socket.write(request);
      return await bytes.wait((buffer, ended) => response(buffer, ended, route.maxResponseBytes));
    } catch (error) {
      if (signal?.aborted || error instanceof Cancelled) throw new Cancelled();
      if (error instanceof Timeout) throw new Problem(`Telegram did not answer ${method} in time. Check the internet connection and try again.`);
      throw new Problem(`Telegram stopped answering ${method}. Check the internet connection and try again.`);
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener?.("abort", abort);
      bytes.release();
      socket.destroy();
    }
  }
  throw new Problem(unreachable(route, failures));
}

async function call(ctx, route, method, params = {}, timeoutMs = 15_000) {
  if (ctx.signal?.aborted) throw new Cancelled();
  const { status, body } = await exchange(route, method, Buffer.from(JSON.stringify(params), "utf8"), timeoutMs, ctx.signal);
  let parsed;
  try {
    parsed = JSON.parse(body.toString("utf8"));
  } catch {
    throw new BotApiError(method, status, {});
  }
  if (parsed?.ok === true && status === 200) return parsed.result;
  throw new BotApiError(method, status, parsed);
}

async function sleep(ctx, milliseconds) {
  try {
    await delay(milliseconds, undefined, ctx.signal ? { signal: ctx.signal } : undefined);
  } catch {
    throw new Cancelled();
  }
}

function rejected(error) {
  return error instanceof BotApiError && (error.code === 401 || error.code === 404);
}

function pairingCode() {
  const characters = Array.from({ length: 10 }, () => CODE_ALPHABET[crypto.randomInt(CODE_ALPHABET.length)]);
  return `${characters.slice(0, 5).join("")}-${characters.slice(5).join("")}`;
}

function sameCode(candidate, code) {
  const normalize = (value) => String(value ?? "").toUpperCase().replace(/[\s-]+/g, "");
  return normalize(candidate) !== "" && normalize(candidate) === normalize(code);
}

function pairingMessage(update, code, username) {
  const message = update?.message;
  if (message?.chat?.type !== "private" || message.from?.is_bot === true || !isId(message.from?.id) || !isId(message.chat?.id)) return null;
  const text = typeof message.text === "string" ? message.text.trim() : "";
  const command = /^\/start(?:@([A-Za-z0-9_]+))?(?:\s+(\S+))?$/.exec(text);
  if (command && command[1] && command[1].toLowerCase() !== username.toLowerCase()) return null;
  return sameCode(command ? command[2] : text, code) ? message : null;
}

async function takeOverWebhook(ctx, path) {
  const info = await call(ctx, path, "getWebhookInfo");
  if (typeof info?.url === "string" && info.url) {
    await call(ctx, path, "deleteWebhook", { drop_pending_updates: false });
    ctx.progress({ message: "This bot was sending its messages to a webhook. Octopus switched it to receive them directly." });
  }
}

async function pair(ctx, path, me, access) {
  await takeOverWebhook(ctx, path);
  const code = pairingCode();
  const url = `https://t.me/${me.username}?start=${code}`;
  const waiting = { message: `Open @${me.username} in Telegram and press Start.`, url, code };
  ctx.progress(waiting);
  const deadline = Date.now() + PAIRING_WINDOW_MS;
  let failures = 0;
  while (Date.now() < deadline) {
    let updates;
    try {
      const timeout = Math.max(0, Math.min(POLL_SECONDS, Math.floor((deadline - Date.now()) / 1000)));
      updates = await call(ctx, path, "getUpdates", { timeout, limit: 100 }, (timeout + 15) * 1000);
      if (failures) ctx.progress(waiting);
      failures = 0;
    } catch (error) {
      if (error instanceof Cancelled || rejected(error)) throw error;
      if (error instanceof BotApiError && error.code === 409) {
        return {
          state: "error",
          account: account(me, access, path),
          manage: MANAGE,
          message: "Telegram already delivers this bot's messages to another connection, usually a task connected to Telegram. Enter your numeric Telegram user ID in Allowed user IDs instead.",
        };
      }
      if (error instanceof BotApiError && error.code === 429) {
        await sleep(ctx, Math.min(30, error.retryAfter ?? 1) * 1000);
        continue;
      }
      failures += 1;
      ctx.progress({ ...waiting, message: `${waiting.message} Reconnecting to Telegram…` });
      await sleep(ctx, Math.min(10_000, 1000 * 2 ** failures));
      continue;
    }
    const list = Array.isArray(updates) ? updates.filter((update) => isId(update?.update_id)) : [];
    const index = list.findIndex((update) => pairingMessage(update, code, me.username) !== null);
    if (index >= 0) {
      const message = list[index].message;
      if (index === 0) {
        await call(ctx, path, "getUpdates", { offset: list[0].update_id + 1, limit: 1, timeout: 0 }).catch((error) => {
          if (error instanceof Cancelled) throw error;
        });
      }
      return { userId: message.from.id, chatId: message.chat.id, name: displayName(message.from) };
    }
    if (list.length) await sleep(ctx, 1500);
  }
  return {
    state: "not_connected",
    account: account(me, access, path),
    manage: MANAGE,
    message: `Telegram did not receive Start in @${me.username} within 5 minutes. Press Connect to try again.`,
  };
}

async function guarded(operation) {
  try {
    const state = await operation();
    return typeof state.message === "string" ? { ...state, message: clip(state.message, 300) } : state;
  } catch (error) {
    if (error instanceof Cancelled) return { state: "not_connected", message: "Connecting to Telegram was cancelled." };
    if (rejected(error)) return { state: "expired", manage: MANAGE, message: "Telegram no longer accepts this bot token. Paste a new token from @BotFather." };
    if (error instanceof Problem) return { state: "error", message: clip(error.message, 300) };
    if (error instanceof BotApiError) return { state: "error", message: redact(error.description ? `Telegram refused the request: ${error.description}` : error.message) };
    return { state: "error", message: redact(error instanceof Error ? error.message : String(error)) };
  }
}

async function probe(ctx) {
  const deadline = Date.now() + PROBE_BUDGET_MS;
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    const config = settings();
    const token = await stored(ctx, names.token);
    if (!token) return { state: "not_connected", message: "Paste your bot token from @BotFather to connect Telegram." };
    if (!TOKEN.test(token)) return { state: "expired", manage: MANAGE, message: "The saved bot token is malformed. Paste the token from @BotFather again." };
    const access = await allowed(ctx, names, config);
    const path = await route(ctx, config, token, deadline);
    const me = bot(await call(ctx, path, "getMe"));
    if (!reachable(access)) {
      return { state: "not_connected", account: account(me, access, path), manage: MANAGE, message: `Press Connect, then Start in @${me.username}, to link your Telegram account.` };
    }
    return { state: "connected", account: account(me, access, path), manage: MANAGE };
  });
}

async function connect(ctx) {
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    const config = settings();
    const entered = typed(ctx, names.token);
    await inherited(ctx, names.token, entered);
    const saved = await stored(ctx, names.token);
    const token = entered || saved;
    if (!token) return { state: "not_connected", message: "Paste your bot token from @BotFather to connect Telegram." };
    if (!TOKEN.test(token)) {
      return { state: "error", message: "That does not look like a bot token. Copy the whole token from @BotFather; it looks like 123456789:AAH…" };
    }
    const sameBot = !saved || saved.split(":")[0] === token.split(":")[0];
    const typedUsers = typed(ctx, names.users);
    const usersInherited = await inherited(ctx, names.users, typedUsers, (left, right) => parseUsers(left).sort().join(",") === parseUsers(right).sort().join(","));
    const users = parseUsers(usersInherited ? await stored(ctx, names.users) : typedUsers ?? (sameBot ? await stored(ctx, names.users) : ""));
    const home = typed(ctx, names.home);
    if (home) parseChat(home);
    const homeInherited = await inherited(ctx, names.home, home);
    const access = await allowed(ctx, names, config, users);
    const pairing = !reachable(access);
    if (pairing && usersInherited) {
      throw new Problem(`${names.users} is set in this Mac's environment without a user ID, so Octopus cannot save the Telegram account you pair. Remove it from the environment or put your Telegram user ID in it.`);
    }
    const path = await route(ctx, config, token);
    ctx.progress({ message: "Checking the bot token with Telegram…" });
    let me;
    try {
      me = bot(await call(ctx, path, "getMe"));
    } catch (error) {
      if (rejected(error)) return { state: "error", manage: MANAGE, message: "Telegram rejected this token. Copy it again from @BotFather, or issue a new one there with /token." };
      throw error;
    }
    const homeChange = home === undefined ? {} : { [names.home]: home || null };
    if (!pairing) {
      const usersChange = typedUsers !== undefined || !sameBot ? { [names.users]: users.length ? users.join(",") : null } : {};
      await save(ctx, { [names.token]: token, ...usersChange, ...homeChange });
      return { state: "connected", account: account(me, access, path), manage: MANAGE, message: `@${me.username} is connected.` };
    }
    const paired = await pair(ctx, path, me, access);
    if (paired.state) return paired;
    const keepHome = home || homeInherited || await stored(ctx, names.home);
    await save(ctx, { [names.token]: token, [names.users]: String(paired.userId), ...(keepHome ? homeChange : { [names.home]: String(paired.chatId) }) });
    const confirmed = await call(ctx, path, "sendMessage", { chat_id: paired.chatId, text: CONFIRMATION }).then(() => true, (error) => {
      if (error instanceof Cancelled) throw error;
      return false;
    });
    return {
      state: "connected",
      account: account(me, { ...access, users: [String(paired.userId)] }, path, paired.name),
      manage: MANAGE,
      message: confirmed
        ? `@${me.username} is connected to ${paired.name}'s Telegram account.`
        : `@${me.username} is connected to ${paired.name}'s Telegram account, but Telegram did not accept the confirmation message.`,
    };
  });
}

async function disconnect(ctx) {
  return guarded(async () => {
    const names = environmentNames(ctx.descriptor);
    await ctx.saveValues({ [names.token]: null, [names.users]: null, [names.home]: null });
    const removed = "Telegram is disconnected: Octopus removed the bot token and allowed users from this Mac.";
    const remains = "The bot still exists; revoke its token or delete it in BotFather.";
    if (typeof (await ctx.launchEnvironment(names.token)) !== "string") return { state: "not_connected", manage: MANAGE, message: `${removed} ${remains}` };
    const listed = `${names.token} is set in this Mac's environment`;
    const state = await probe(ctx);
    if (state.state === "error") return { ...state, message: `Octopus removed the saved Telegram settings, but ${listed} and checking it failed: ${state.message}` };
    if (state.state === "not_connected") {
      return { ...state, manage: MANAGE, message: `${removed} ${listed}, but no Telegram account reaches tasks through it. ${remains}` };
    }
    return {
      ...state,
      message: `Octopus removed the saved Telegram settings, but ${listed}, so the Telegram interface keeps using that bot. Remove it there to disconnect.`,
    };
  });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
