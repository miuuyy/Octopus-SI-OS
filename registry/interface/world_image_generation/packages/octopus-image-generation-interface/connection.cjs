"use strict";

const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");

const REQUEST_TIMEOUT_MS = 10_000;
const MAX_ACCOUNT_FILE_BYTES = 1024 * 1024;
const MAX_STATUS_BYTES = 1024 * 1024;
const ACCESS_TOKEN_EXPIRY_SKEW_MS = 30_000;
const FIELD = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const HEADER_NAME = /^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/;
const OWNED_HEADERS = new Set(["authorization", "content-type", "accept"]);
const PLAN_NAMES = {
  free: "Free", go: "Go", plus: "Plus", pro: "Pro", prolite: "Pro Lite", promax: "Pro (Max)", team: "Team",
  self_serve_business_prolite: "Self Serve Business ProLite",
  self_serve_business_usage_based: "Self Serve Business Usage Based",
  business: "Business", ent26: "Enterprise", enterprise_cbp_automation: "Enterprise (Automation)",
  enterprise_cbp_usage_based: "Enterprise CBP Usage Based", enterprise: "Enterprise", hc: "Enterprise",
  edu: "Edu", education: "Edu", edu_plus: "Edu Plus", edu_pro: "Edu Pro",
  sci: "Academic Researchers", sci_plus: "Academic Researchers Plus",
};
const SIGN_IN_FIRST = "Sign in to ChatGPT in Octopus first: Image Generation creates images with that account.";
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

function plainHttps(value) {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.hash ? url : null;
  } catch {
    return null;
  }
}

function transportHeaders(transport, reserved) {
  const headers = {};
  for (const [name, value] of Object.entries(transport.headers ?? {})) {
    const lower = name.toLowerCase();
    if (typeof value !== "string" || !HEADER_NAME.test(name) || OWNED_HEADERS.has(lower) || lower === reserved.toLowerCase()) {
      throw invalid(`transport header '${name}' is not admitted`);
    }
    headers[lower] = value;
  }
  return headers;
}

function connectionConfig(descriptor) {
  const config = descriptor?.interface_driver_config?.private_config;
  const transport = descriptor?.interface_driver_config?.transport;
  if (!isObject(config) || !isObject(transport) || transport.wire !== "image_json_http") {
    throw invalid("it is not an image generation interface");
  }
  const key = typeof transport.auth_env_key === "string" ? transport.auth_env_key.trim() : "";
  if (config.auth === undefined) {
    if (!key) throw invalid("it declares neither an API key nor a ChatGPT sign-in");
    const declaredHeader = transport.auth_header ?? "Authorization";
    const authHeader = typeof declaredHeader === "string" ? declaredHeader.trim() : "";
    const authPrefix = transport.auth_value_prefix ?? "Bearer ";
    if (!HEADER_NAME.test(authHeader) || typeof authPrefix !== "string") throw invalid("its API key header is incomplete");
    return { kind: "key", transport, key, authHeader, authPrefix, headers: transportHeaders(transport, authHeader) };
  }
  if (key) throw invalid("it declares both an API key and a ChatGPT sign-in");
  const auth = config.auth;
  if (auth?.mode !== "chatgpt_account" || !ENV_NAME.test(auth.auth_file_env ?? "") || !HEADER_NAME.test(auth.account_header ?? "")
    || OWNED_HEADERS.has(auth.account_header.toLowerCase())) {
    throw invalid("its ChatGPT sign-in is incomplete");
  }
  const check = config.account_check;
  const url = plainHttps(check?.url);
  const manage = plainHttps(check?.manage?.url);
  if (!url || !["email_field", "account_field", "plan_field"].every((field) => FIELD.test(check[field] ?? ""))
    || !Array.isArray(check.excluded_plans) || !check.excluded_plans.every(isText)
    || !isText(check.manage?.label) || check.manage.label.length > 48 || !manage) {
    throw invalid("it declares no account check");
  }
  return {
    kind: "account",
    authFileEnv: auth.auth_file_env,
    accountHeader: auth.account_header,
    url: url.href,
    emailField: check.email_field,
    accountField: check.account_field,
    planField: check.plan_field,
    excluded: new Set(check.excluded_plans.map((plan) => plan.toLowerCase())),
    manage: { label: check.manage.label, url: manage.href },
    headers: transportHeaders(transport, auth.account_header),
  };
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

function emailAddress(value) {
  return typeof value === "string" && value.length <= 254
    && /^[^\s@\u0000-\u001f\u007f]+@[^\s@\u0000-\u001f\u007f]+$/u.test(value) ? value : null;
}

async function readBoundedRegularFile(file, limit) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(file, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size <= 0 || before.size > limit) throw new Error("not a bounded regular file");
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
  let document;
  try {
    document = JSON.parse((await readBoundedRegularFile(file, MAX_ACCOUNT_FILE_BYTES)).toString("utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
    throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  }
  if (!isObject(document) || (document.auth_mode !== undefined && document.auth_mode !== "chatgpt")) {
    return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
  }
  const tokens = document.tokens;
  if (!isText(tokens?.access_token) || !isText(tokens?.account_id)) {
    return { stop: { state: "not_connected", message: SIGN_IN_FIRST } };
  }
  const claims = jwtClaims(tokens.id_token);
  const signIn = emailAddress(claims?.email ?? claims?.["https://api.openai.com/profile"]?.email);
  const expiresAt = jwtClaims(tokens.access_token)?.exp;
  if (!Number.isFinite(expiresAt)) throw new ConnectionStop("error", UNREADABLE_SIGN_IN);
  if (expiresAt * 1000 <= Date.now() + ACCESS_TOKEN_EXPIRY_SKEW_MS) {
    return { stop: { state: "expired", ...(signIn ? { account: { label: signIn, detail: "ChatGPT account" } } : {}), manage: config.manage, message: SIGN_IN_EXPIRED } };
  }
  return { accessToken: tokens.access_token, accountId: tokens.account_id, signIn };
}

function planName(plan) {
  const raw = plan.toLowerCase();
  return Object.hasOwn(PLAN_NAMES, raw)
    ? PLAN_NAMES[raw]
    : plan.replace(/[_-]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

async function readAccountStatus(ctx, config, credential) {
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(config.url, {
      headers: {
        ...config.headers,
        accept: "application/json",
        authorization: `Bearer ${credential.accessToken}`,
        [config.accountHeader]: credential.accountId,
      },
      redirect: "manual",
      signal: ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout,
    });
  } catch {
    if (ctx.signal?.aborted) throw new ConnectionStop("error", "The connection check was cancelled.");
    throw new ConnectionStop("error", `ChatGPT could not be reached at ${new URL(config.url).host} to check this account.`);
  }
  if (response.status === 401) throw new ConnectionStop("expired", SIGN_IN_EXPIRED);
  if (response.status === 403) throw new ConnectionStop("error", "ChatGPT refused to show this account's plan (HTTP 403).");
  if (response.status < 200 || response.status > 299) {
    throw new ConnectionStop("error", `ChatGPT answered the account check with HTTP ${response.status}.`);
  }
  const length = Number(response.headers.get("content-length"));
  const bytes = Number.isFinite(length) && length > MAX_STATUS_BYTES ? null : Buffer.from(await response.arrayBuffer());
  let status = null;
  try {
    status = bytes && bytes.byteLength <= MAX_STATUS_BYTES ? JSON.parse(bytes.toString("utf8")) : null;
  } catch {
    status = null;
  }
  if (!isObject(status)) throw new ConnectionStop("error", "ChatGPT answered the account check with malformed data.");
  return status;
}

async function inspectAccount(ctx, config) {
  const credential = await readCredential(ctx, config);
  if (credential.stop) return credential.stop;
  const signedIn = credential.signIn ? { account: { label: credential.signIn, detail: "ChatGPT account" } } : {};
  try {
    const status = await readAccountStatus(ctx, config, credential);
    const answered = status[config.accountField];
    if (answered !== undefined && answered !== credential.accountId) {
      throw new ConnectionStop("error", "ChatGPT answered the account check for a different account than this sign-in.");
    }
    const label = emailAddress(status[config.emailField]) ?? credential.signIn;
    if (!label) {
      throw new ConnectionStop("error", "ChatGPT did not say which account this sign-in belongs to. Sign in to ChatGPT again in Octopus.");
    }
    const plan = status[config.planField];
    if (!isText(plan)) throw new ConnectionStop("error", "ChatGPT did not say which plan this account is on.");
    const account = { label, detail: `ChatGPT ${planName(plan)} plan` };
    if (config.excluded.has(plan.toLowerCase())) {
      return {
        state: "not_connected",
        account,
        manage: config.manage,
        message: `Image generation needs a paid ChatGPT plan, and this account is on ${planName(plan)}. Upgrade it, or sign in to ChatGPT with another account in Octopus.`,
      };
    }
    return { state: "connected", account, manage: config.manage };
  } catch (error) {
    if (error instanceof ConnectionStop) return { state: error.state, ...signedIn, manage: config.manage, message: error.message };
    throw error;
  }
}

function filled(map) {
  return Object.fromEntries(Object.entries(map ?? {})
    .filter(([, value]) => typeof value === "string" && value.trim())
    .map(([name, value]) => [name, value.trim()]));
}

function endpointOf(transport, values) {
  const override = typeof transport.base_url_env === "string" ? values[transport.base_url_env.trim()] : "";
  let url;
  try {
    url = new URL(override || transport.base_url);
  } catch {
    return { error: "The API URL is not a valid URL." };
  }
  if (url.protocol !== "https:") return { error: "The API URL must use https." };
  const models = url.pathname.endsWith("/images/generations")
    ? new URL(url.pathname.slice(0, -"/images/generations".length) + "/models", url.origin)
    : null;
  return { url, models };
}

async function checkKey(ctx, config, values) {
  const transport = config.transport;
  const key = values[config.key];
  if (!key) return { state: "not_connected", message: "Enter the API key." };
  const endpoint = endpointOf(transport, values);
  if (endpoint.error) return { state: "error", message: endpoint.error };
  const host = endpoint.url.host.slice(0, 96);
  if (!endpoint.models) {
    return {
      state: "connected",
      account: { label: host, detail: "This API names no account to check the key against" },
      message: "This API has no models list to check the key against; the first image request will show whether it works.",
    };
  }
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(endpoint.models, {
      headers: { ...config.headers, accept: "application/json", [config.authHeader]: `${config.authPrefix}${key}` },
      redirect: "manual",
      signal: ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout,
    });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    return { state: "error", message: `Could not reach ${endpoint.models.host}.` };
  }
  if (response.status === 401 || response.status === 403) return { state: "expired", message: "The API rejected the key." };
  if (!response.ok) return { state: "error", message: `The API answered HTTP ${response.status}.` };
  const organization = response.headers.get("openai-organization")?.trim();
  return {
    state: "connected",
    account: organization
      ? { label: clip(organization, 96), detail: `API key accepted by ${host}` }
      : { label: host, detail: "API key accepted; this API does not name its account" },
  };
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

async function settled(work) {
  try {
    return finish(await work());
  } catch (error) {
    if (error instanceof ConnectionStop) return finish({ state: error.state, message: error.message });
    throw error;
  }
}

async function probe(ctx) {
  return settled(async () => {
    const config = connectionConfig(ctx.descriptor);
    return config.kind === "account" ? inspectAccount(ctx, config) : checkKey(ctx, config, filled(ctx.values));
  });
}

async function connect(ctx) {
  return settled(async () => {
    const config = connectionConfig(ctx.descriptor);
    if (config.kind === "account") return inspectAccount(ctx, config);
    const input = filled(ctx.input);
    const result = await checkKey(ctx, config, { ...filled(ctx.values), ...input });
    if (result.state === "connected" && Object.keys(input).length) await ctx.saveValues(input);
    return result;
  });
}

async function disconnect(ctx) {
  return settled(async () => {
    const config = connectionConfig(ctx.descriptor);
    if (config.kind === "account") {
      const state = await inspectAccount(ctx, config);
      if (state.state !== "connected") return state;
      return {
        ...state,
        message: "Image Generation uses the ChatGPT sign-in on this Mac and stays connected while it lasts. Leave it out of a project or task to keep it from creating images there, or sign in to ChatGPT with another account to change it.",
      };
    }
    const names = [config.transport.auth_env_key, config.transport.base_url_env].filter((name) => typeof name === "string" && name.trim());
    await ctx.saveValues(Object.fromEntries(names.map((name) => [name.trim(), null])));
    return { state: "not_connected" };
  });
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
