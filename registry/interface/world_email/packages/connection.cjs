"use strict";

const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const tls = require("node:tls");
const { domainToASCII } = require("node:url");

const CONFIG_FILE = path.join(__dirname, "..", "config.json");
const ENVIRONMENT_KEYS = ["address_env", "password_env", "imap_host_env", "smtp_host_env", "allowed_senders_env"];
const MAX_LINE_BYTES = 65536;
const LOCAL_PART = /^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*$/;
const DOMAIN_LABEL = /^(?!-)[a-z0-9-]{1,63}(?<!-)$/;
const HOST = /^(?=.{1,253}$)(?!-)[A-Za-z0-9-]{1,63}(?<!-)(?:\.(?!-)[A-Za-z0-9-]{1,63}(?<!-))*$/;
const GROUPED_APP_PASSWORD = /^[A-Za-z]{4}(?:[  ][A-Za-z]{4}){3}$/;
const APP_PASSWORD_REQUIRED = /application-specific password|app password|app-specific password|\b5\.7\.9\b/i;
const ACCESS_TURNED_OFF = /not enabled for imap|imap access is disabled|imap is disabled|smtpclientauthentication is disabled|basic authentication is disabled|\b5\.7\.139\b/i;
const TEMPORARY = /\[(?:UNAVAILABLE|INUSE|LIMIT|SERVERBUG)\]|try again|temporar|too many/i;
const KEEP_LOOKING = new Set(["unreachable", "timeout", "wrong_port"]);

class MailFailure extends Error {
  constructor(kind, message, server = "") {
    super(message);
    this.kind = kind;
    this.server = server;
  }
}

function loadConfig() {
  let config;
  try {
    config = JSON.parse(fs.readFileSync(CONFIG_FILE, "utf8"));
  } catch (error) {
    throw new Error(`The Email settings could not be read: ${error.message}`);
  }
  const port = (value) => Number.isInteger(value) && value >= 1 && value <= 65535;
  const endpoint = (value) => typeof value?.host === "string" && HOST.test(value.host) && port(value.port) && ["implicit", "starttls"].includes(value.tls);
  const profile = (value) => typeof value?.sign_in_help === "string" && value.sign_in_help.length > 0;
  const rules = (value) => port(value?.default_port)
    && ["implicit_tls_ports", "starttls_ports", "discovery_ports"].every((key) => Array.isArray(value[key]) && value[key].every(port))
    && value.discovery_ports[0] === value.default_port;
  const valid = config?.schema_version === 1
    && Array.isArray(config.providers)
    && config.providers.every((provider) => Array.isArray(provider?.domains) && endpoint(provider.imap) && endpoint(provider.smtp) && profile(provider))
    && profile(config.custom_provider)
    && rules(config.endpoints?.imap)
    && rules(config.endpoints?.smtp)
    && Number.isInteger(config.transport?.connect_timeout_ms)
    && config.transport.connect_timeout_ms > 0;
  if (!valid) throw new Error("The Email config.json does not match the connection contract.");
  return config;
}

function environmentNames(descriptor) {
  const environment = descriptor?.interface_driver_config?.private_config?.environment;
  if (!environment || ENVIRONMENT_KEYS.some((key) => typeof environment[key] !== "string" || !environment[key])) {
    throw new Error("The Email interface does not declare its connection fields.");
  }
  return Object.fromEntries(ENVIRONMENT_KEYS.map((key) => [key.slice(0, -4), environment[key]]));
}

function normalizeDomain(value) {
  const ascii = domainToASCII(String(value).replace(/\.$/, "")).toLowerCase();
  const labels = ascii.split(".");
  return ascii && ascii.length <= 253 && labels.length >= 2 && labels.every((label) => DOMAIN_LABEL.test(label)) ? ascii : null;
}

function normalizeAddress(value, label) {
  const text = String(value ?? "").trim();
  const parts = text.split("@");
  const domain = parts.length === 2 && !/[\r\n\0 <>,;"()[\]\\]/.test(text) && parts[0].length <= 64 && LOCAL_PART.test(parts[0])
    ? normalizeDomain(parts[1])
    : null;
  if (!domain) throw new MailFailure("input", `${label} ${JSON.stringify(clip(text, 80))} is not an email address.`);
  return `${parts[0]}@${domain}`;
}

function normalizeAllowedSenders(value) {
  const result = [];
  const seen = new Set();
  for (const token of String(value ?? "").trim().split(/[,;\s]+/).filter(Boolean)) {
    let entry;
    if (token.startsWith("@")) {
      const domain = normalizeDomain(token.slice(1));
      if (!domain) throw new MailFailure("input", `Allowed sender ${JSON.stringify(clip(token, 80))} is not an address or @domain.`);
      entry = `@${domain}`;
    } else {
      entry = normalizeAddress(token, "Allowed sender");
    }
    if (!seen.has(entry.toLowerCase())) {
      seen.add(entry.toLowerCase());
      result.push(entry);
    }
  }
  return result;
}

function normalizePassword(value) {
  const text = String(value ?? "").trim();
  return GROUPED_APP_PASSWORD.test(text) ? text.replace(/[  ]/g, "") : text;
}

function parseServer(value, rules, label, discover) {
  const match = String(value).trim().match(/^([^:\s/@]+)(?::(\d{1,5}))?$/);
  const host = match ? match[1].toLowerCase().replace(/\.$/, "") : "";
  const port = match?.[2] === undefined ? null : Number(match[2]);
  if (!match || !HOST.test(host) || (port !== null && (port < 1 || port > 65535))) {
    throw new MailFailure("input", `${label} must be a server name, optionally followed by :port.`);
  }
  const ports = port !== null ? [port] : discover ? rules.discovery_ports : [rules.default_port];
  return ports.map((candidate) => ({ host, port: candidate, tls: rules.starttls_ports.includes(candidate) ? "starttls" : "implicit" }));
}

function resolveAccount(config, entries, discover) {
  const password = normalizePassword(entries.password);
  if (!String(entries.address ?? "").trim() || !password) return null;
  const address = normalizeAddress(entries.address, "The email address");
  const domain = address.slice(address.lastIndexOf("@") + 1);
  const imapOverride = entries.imap ? parseServer(entries.imap, config.endpoints.imap, "The IMAP server", discover) : null;
  const smtpOverride = entries.smtp ? parseServer(entries.smtp, config.endpoints.smtp, "The SMTP server", discover) : null;
  const provider = config.providers.find((candidate) => candidate.domains.includes(domain))
    ?? (imapOverride ? config.providers.find((candidate) => candidate.imap.host === imapOverride[0].host) : undefined);
  const imap = imapOverride ?? (provider ? [provider.imap] : null);
  const smtp = smtpOverride ?? (provider ? [provider.smtp] : null);
  if (!imap || !smtp) {
    throw new MailFailure("input", `${domain} has no built-in mail server settings; enter its IMAP and SMTP servers.`);
  }
  return {
    address,
    password,
    imap,
    smtp,
    imapOverride: imapOverride !== null,
    smtpOverride: smtpOverride !== null,
    allowed: normalizeAllowedSenders(entries.allowed),
    help: (provider ?? config.custom_provider).sign_in_help,
  };
}

function transportFailure(endpoint, error) {
  const where = `${endpoint.host}:${endpoint.port}`;
  const code = typeof error?.code === "string" ? error.code : "";
  const text = String(error?.message ?? "");
  if (code === "ENOTFOUND" || code === "EAI_AGAIN" || code === "EAI_NONAME") {
    return new MailFailure("unknown_host", `The server name ${endpoint.host} could not be found. Check the name and your network.`);
  }
  if (code === "ECONNREFUSED") return new MailFailure("unreachable", `${where} refused the connection.`);
  if (["ETIMEDOUT", "EHOSTUNREACH", "ENETUNREACH", "ECONNRESET", "EPIPE"].includes(code)) {
    return new MailFailure("unreachable", `${where} could not be reached.`);
  }
  if (code === "ERR_SSL_WRONG_VERSION_NUMBER" || code === "EPROTO" || /wrong version number|unknown protocol|packet length too long/i.test(text)) {
    return new MailFailure("wrong_port", `${where} did not start an encrypted connection; check the port.`);
  }
  if (/CERT|SELF_SIGNED|UNABLE_TO_|ALTNAME|HOSTNAME/i.test(code) || error?.reason) {
    return new MailFailure("tls", `The certificate of ${endpoint.host} is not trusted (${code || text}).`);
  }
  return new MailFailure("unreachable", `The connection to ${where} failed.`, text);
}

class LineSocket {
  constructor(endpoint, timeoutMs, signal) {
    this.endpoint = endpoint;
    this.buffer = Buffer.alloc(0);
    this.waiting = null;
    this.pending = null;
    this.failure = null;
    this.socket = null;
    this.closed = false;
    this.tags = 0;
    this.signal = signal;
    this.onData = (chunk) => this.receive(chunk);
    this.onError = (error) => this.fail(transportFailure(endpoint, error));
    this.onClose = () => this.fail(new MailFailure("unreachable", `${endpoint.host} closed the connection.`));
    this.onAbort = () => this.fail(signal.reason instanceof Error ? signal.reason : new Error("The connection was cancelled."));
    this.timer = setTimeout(() => this.fail(new MailFailure("timeout", `${endpoint.host}:${endpoint.port} did not answer in time.`)), timeoutMs);
    signal?.addEventListener("abort", this.onAbort, { once: true });
  }

  open() {
    const { host, port } = this.endpoint;
    const socket = this.endpoint.tls === "implicit"
      ? tls.connect({ host, port, servername: net.isIP(host) ? undefined : host, minVersion: "TLSv1.2" })
      : net.connect({ host, port });
    return this.attach(socket, this.endpoint.tls === "implicit" ? "secureConnect" : "connect");
  }

  upgrade() {
    if (this.buffer.length) return Promise.reject(new MailFailure("tls", `${this.endpoint.host} sent data before encryption started.`));
    const plain = this.socket;
    plain.removeListener("data", this.onData);
    plain.removeListener("error", this.onError);
    plain.removeListener("close", this.onClose);
    const host = this.endpoint.host;
    return this.attach(tls.connect({ socket: plain, host, servername: net.isIP(host) ? undefined : host, minVersion: "TLSv1.2" }), "secureConnect");
  }

  attach(socket, ready) {
    this.socket = socket;
    socket.on("data", this.onData);
    socket.on("error", this.onError);
    socket.on("close", this.onClose);
    if (this.failure) {
      socket.destroy();
      return Promise.reject(this.failure);
    }
    return new Promise((resolve, reject) => {
      this.pending = { resolve, reject };
      socket.once(ready, () => {
        this.pending = null;
        resolve();
      });
    });
  }

  receive(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    if (this.buffer.length > MAX_LINE_BYTES && this.buffer.indexOf(10) < 0) {
      this.fail(new MailFailure("protocol", `${this.endpoint.host} sent a line longer than ${MAX_LINE_BYTES} bytes.`));
      return;
    }
    this.deliver();
  }

  deliver() {
    if (!this.waiting) return;
    const index = this.buffer.indexOf(10);
    if (index < 0) return;
    const end = index > 0 && this.buffer[index - 1] === 13 ? index - 1 : index;
    const line = this.buffer.subarray(0, end).toString("utf8");
    this.buffer = this.buffer.subarray(index + 1);
    const { resolve } = this.waiting;
    this.waiting = null;
    resolve(line);
  }

  line() {
    if (this.failure) return Promise.reject(this.failure);
    return new Promise((resolve, reject) => {
      this.waiting = { resolve, reject };
      this.deliver();
    });
  }

  write(text) {
    if (!this.failure) this.socket.write(text);
  }

  fail(error) {
    if (this.closed || this.failure) return;
    this.failure = error;
    this.release();
    this.pending?.reject(error);
    this.waiting?.reject(error);
    this.pending = null;
    this.waiting = null;
  }

  close() {
    if (!this.failure) this.failure = new MailFailure("closed", "The connection was closed.");
    this.closed = true;
    this.release();
  }

  release() {
    clearTimeout(this.timer);
    this.signal?.removeEventListener("abort", this.onAbort);
    this.socket?.destroy();
  }
}

function signInFailure(host, server) {
  if (APP_PASSWORD_REQUIRED.test(server)) {
    return new MailFailure("app_password", `${host} needs an app password instead of the account password.`, server);
  }
  if (ACCESS_TURNED_OFF.test(server)) {
    return new MailFailure("access_off", `${host} does not let mail apps sign in to this account; turn on IMAP and SMTP access in the mail settings.`, server);
  }
  if (TEMPORARY.test(server)) {
    return new MailFailure("temporary", `${host} is refusing sign-ins for now; try again in a few minutes.`, server);
  }
  return new MailFailure("auth", `${host} did not accept this email address and app password.`, server);
}

function quoted(value) {
  return `"${value.replace(/[\\"]/g, (character) => `\\${character}`)}"`;
}

async function imapCommand(wire, command, continuation = null) {
  const tag = `E${++wire.tags}`;
  wire.write(`${tag} ${command}\r\n`);
  const untagged = [];
  for (;;) {
    const line = await wire.line();
    if (line.startsWith("+")) {
      if (!continuation) throw new MailFailure("protocol", `${wire.endpoint.host} asked for input it should not need.`, line);
      wire.write(`${continuation}\r\n`);
      continuation = null;
      continue;
    }
    if (line.startsWith(`${tag} `)) {
      const [, status = "", text = ""] = line.slice(tag.length + 1).match(/^(\S+)\s*(.*)$/) ?? [];
      return { status: status.toUpperCase(), text, untagged };
    }
    untagged.push(line);
  }
}

function capabilityTokens(text) {
  return new Set(text.trim().toUpperCase().split(/\s+/));
}

async function imapCapabilities(wire) {
  const reply = await imapCommand(wire, "CAPABILITY");
  const line = reply.untagged.find((entry) => /^\* CAPABILITY /i.test(entry));
  if (reply.status !== "OK" || !line) throw new MailFailure("protocol", `${wire.endpoint.host} did not list its capabilities.`, reply.text);
  return capabilityTokens(line.slice("* CAPABILITY ".length));
}

async function checkImap(endpoint, account, timeoutMs, signal) {
  const wire = new LineSocket(endpoint, timeoutMs, signal);
  try {
    await wire.open();
    const greeting = await wire.line();
    if (!/^\* OK\b/i.test(greeting)) throw new MailFailure("protocol", `${endpoint.host} did not greet like an IMAP server.`, greeting);
    const advertised = greeting.match(/\[CAPABILITY ([^\]]+)\]/i);
    let capabilities = advertised ? capabilityTokens(advertised[1]) : await imapCapabilities(wire);
    if (endpoint.tls === "starttls") {
      if (!capabilities.has("STARTTLS")) {
        throw new MailFailure("tls", `${endpoint.host} does not offer STARTTLS on port ${endpoint.port}; Octopus never signs in without encryption.`);
      }
      const started = await imapCommand(wire, "STARTTLS");
      if (started.status !== "OK") throw new MailFailure("tls", `${endpoint.host} refused to start encryption.`, started.text);
      await wire.upgrade();
      capabilities = await imapCapabilities(wire);
    }
    let reply;
    if (capabilities.has("AUTH=PLAIN")) {
      const token = Buffer.from(`\0${account.address}\0${account.password}`, "utf8").toString("base64");
      reply = await imapCommand(wire, "AUTHENTICATE PLAIN", token);
    } else if (capabilities.has("LOGINDISABLED")) {
      throw new MailFailure("auth", `${endpoint.host} does not accept password sign-in on this connection.`);
    } else if (!/^[\x20-\x7e]*$/.test(account.password)) {
      throw new MailFailure("auth", `${endpoint.host} accepts only plain ASCII passwords.`);
    } else {
      reply = await imapCommand(wire, `LOGIN ${quoted(account.address)} ${quoted(account.password)}`);
    }
    if (reply.status === "BAD") throw new MailFailure("protocol", `${endpoint.host} did not understand the sign-in.`, reply.text);
    if (reply.status !== "OK") throw signInFailure(endpoint.host, reply.text);
    await imapCommand(wire, "LOGOUT").catch(() => undefined);
    return endpoint;
  } finally {
    wire.close();
  }
}

async function smtpReply(wire) {
  const lines = [];
  for (;;) {
    const line = await wire.line();
    const match = line.match(/^(\d{3})([ -]?)(.*)$/);
    if (!match) throw new MailFailure("protocol", `${wire.endpoint.host} did not answer like an SMTP server.`, line);
    lines.push(match[3]);
    if (match[2] !== "-") return { code: Number(match[1]), text: `${match[1]} ${lines.join(" ")}`.trim(), lines };
  }
}

async function smtpCommand(wire, command) {
  wire.write(`${command}\r\n`);
  return smtpReply(wire);
}

async function smtpHello(wire) {
  const reply = await smtpCommand(wire, "EHLO localhost");
  if (reply.code !== 250) throw new MailFailure("protocol", `${wire.endpoint.host} refused the SMTP greeting.`, reply.text);
  const extensions = new Map();
  for (const line of reply.lines.slice(1)) {
    const [name, ...values] = line.trim().toUpperCase().split(/[\s=]+/);
    if (name) extensions.set(name, new Set(values));
  }
  return extensions;
}

async function checkSmtp(endpoint, account, timeoutMs, signal) {
  const wire = new LineSocket(endpoint, timeoutMs, signal);
  try {
    await wire.open();
    const greeting = await smtpReply(wire);
    if (greeting.code !== 220) {
      throw greeting.code >= 400 && greeting.code < 500
        ? new MailFailure("temporary", `${endpoint.host} is not taking connections right now; try again in a few minutes.`, greeting.text)
        : new MailFailure("protocol", `${endpoint.host} refused the SMTP connection.`, greeting.text);
    }
    let extensions = await smtpHello(wire);
    if (endpoint.tls === "starttls") {
      if (!extensions.has("STARTTLS")) {
        throw new MailFailure("tls", `${endpoint.host} does not offer STARTTLS on port ${endpoint.port}; Octopus never signs in without encryption.`);
      }
      const started = await smtpCommand(wire, "STARTTLS");
      if (started.code !== 220) throw new MailFailure("tls", `${endpoint.host} refused to start encryption.`, started.text);
      await wire.upgrade();
      extensions = await smtpHello(wire);
    }
    const mechanisms = extensions.get("AUTH") ?? new Set();
    const encode = (value) => Buffer.from(value, "utf8").toString("base64");
    let reply;
    if (mechanisms.has("PLAIN")) {
      const token = encode(`\0${account.address}\0${account.password}`);
      reply = await smtpCommand(wire, `AUTH PLAIN ${token}`);
      if (reply.code === 334) reply = await smtpCommand(wire, token);
    } else if (mechanisms.has("LOGIN")) {
      reply = await smtpCommand(wire, "AUTH LOGIN");
      if (reply.code === 334) reply = await smtpCommand(wire, encode(account.address));
      if (reply.code === 334) reply = await smtpCommand(wire, encode(account.password));
    } else {
      throw new MailFailure("auth", `${endpoint.host} offers no password sign-in for sending mail.`);
    }
    if (reply.code !== 235) {
      throw reply.code >= 400 && reply.code < 500 && !APP_PASSWORD_REQUIRED.test(reply.text)
        ? new MailFailure("temporary", `${endpoint.host} is refusing sign-ins for now; try again in a few minutes.`, reply.text)
        : signInFailure(endpoint.host, reply.text);
    }
    await smtpCommand(wire, "QUIT").catch(() => undefined);
    return endpoint;
  } finally {
    wire.close();
  }
}

async function firstWorking(candidates, check) {
  let unreachable = null;
  for (const endpoint of candidates) {
    try {
      return await check(endpoint);
    } catch (error) {
      if (!(error instanceof MailFailure) || !KEEP_LOOKING.has(error.kind)) throw error;
      unreachable ??= error;
    }
  }
  throw unreachable;
}

function clip(value, limit = 300) {
  const text = String(value ?? "").replace(/[\u0000-\u001f\u007f]+/gu, " ").replace(/\s+/g, " ").trim();
  return text.length > limit ? `${text.slice(0, limit - 1).trimEnd()}…` : text;
}

function redact(text, account) {
  let result = String(text ?? "");
  if (account?.password && account.password.length >= 4) {
    const secrets = [account.password, Buffer.from(account.password, "utf8").toString("base64"), Buffer.from(`\0${account.address}\0${account.password}`, "utf8").toString("base64")];
    for (const secret of secrets) result = result.split(secret).join("[secret]");
  }
  return result;
}

function failureMessage(error, account, saved = false) {
  if (!(error instanceof MailFailure)) return clip(redact(error instanceof Error ? error.message : String(error), account)) || "The mailbox could not be checked.";
  let message = error.message;
  if (saved && error.kind === "auth") message = message.replace("this email address and app password", "the saved email address and app password");
  if (["auth", "app_password"].includes(error.kind) && account) message = `${message} ${account.help}`;
  if (["access_off", "temporary", "protocol", "tls"].includes(error.kind) && error.server) message = `${message} The server said: ${clip(error.server, 140)}`;
  return clip(redact(message, account));
}

function accountState(account, imap, smtp) {
  return { label: clip(account.address, 96), detail: clip(`Receives through ${imap.host} and sends through ${smtp.host}`, 160) };
}

function serverValue(endpoint) {
  return `${endpoint.host}:${endpoint.port}`;
}

async function probe(ctx) {
  const config = loadConfig();
  const names = environmentNames(ctx.descriptor);
  const saved = (name) => ctx.values[names[name]] ?? "";
  let account;
  try {
    account = resolveAccount(config, { address: saved("address"), password: saved("password"), imap: saved("imap_host"), smtp: saved("smtp_host"), allowed: saved("allowed_senders") }, false);
  } catch (error) {
    return { state: "error", message: failureMessage(error, null) };
  }
  if (!account) return { state: "not_connected" };
  const timeout = config.transport.connect_timeout_ms;
  const results = await Promise.allSettled([
    checkImap(account.imap[0], account, timeout, ctx.signal),
    checkSmtp(account.smtp[0], account, timeout, ctx.signal),
  ]);
  if (ctx.signal?.aborted) throw ctx.signal.reason;
  const failure = results.find((result) => result.status === "rejected")?.reason;
  const label = accountState(account, account.imap[0], account.smtp[0]);
  if (!failure) return { state: "connected", account: label };
  const expired = failure instanceof MailFailure && ["auth", "app_password"].includes(failure.kind);
  return { state: expired ? "expired" : "error", account: label, message: failureMessage(failure, account, true) };
}

async function connect(ctx) {
  const config = loadConfig();
  const names = environmentNames(ctx.descriptor);
  const entry = (name) => ctx.input[names[name]] ?? ctx.values[names[name]] ?? "";
  let account;
  try {
    account = resolveAccount(config, { address: entry("address"), password: entry("password"), imap: entry("imap_host"), smtp: entry("smtp_host"), allowed: entry("allowed_senders") }, true);
  } catch (error) {
    return { state: "error", message: failureMessage(error, null) };
  }
  if (!account) return { state: "error", message: "Enter your email address and app password." };
  const timeout = config.transport.connect_timeout_ms;
  try {
    ctx.progress({ message: `Signing in to ${account.imap[0].host}…` });
    const imap = await firstWorking(account.imap, (endpoint) => checkImap(endpoint, account, timeout, ctx.signal));
    ctx.progress({ message: `Checking that ${account.smtp[0].host} accepts mail from ${account.address}…` });
    const smtp = await firstWorking(account.smtp, (endpoint) => checkSmtp(endpoint, account, timeout, ctx.signal));
    await ctx.saveValues({
      [names.address]: account.address,
      [names.password]: account.password,
      [names.imap_host]: account.imapOverride ? serverValue(imap) : null,
      [names.smtp_host]: account.smtpOverride ? serverValue(smtp) : null,
      [names.allowed_senders]: account.allowed.length ? account.allowed.join(", ") : null,
    });
    return { state: "connected", account: accountState(account, imap, smtp) };
  } catch (error) {
    if (ctx.signal?.aborted) throw ctx.signal.reason;
    return { state: "error", account: accountState(account, account.imap[0], account.smtp[0]), message: failureMessage(error, account) };
  }
}

async function disconnect(ctx) {
  const names = environmentNames(ctx.descriptor);
  await ctx.saveValues(Object.fromEntries(Object.values(names).map((name) => [name, null])));
  return { state: "not_connected" };
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
