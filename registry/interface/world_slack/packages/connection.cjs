const fs = require("node:fs");
const path = require("node:path");

const REQUEST_TIMEOUT_MS = 10_000;
const PATTERNS = {
  bot_token_env: /^xoxb-[0-9A-Za-z-]{15,4096}$/,
  app_token_env: /^xapp-[0-9A-Za-z-]{15,4096}$/,
  allowed_user_ids_env: /^[UW][A-Z0-9]{8,32}$/,
  allowed_channel_ids_env: /^[CDG][A-Z0-9]{8,32}$/,
  home_channel_env: /^[CDG][A-Z0-9]{8,32}$/,
};
const LIST_KEYS = new Set(["bot_token_env", "allowed_user_ids_env", "allowed_channel_ids_env"]);
const FIELD_NAMES = {
  bot_token_env: "The bot token must start with xoxb-.",
  app_token_env: "The app-level token must start with xapp-.",
  allowed_user_ids_env: "Member IDs look like U0123ABCDE, separated by commas.",
  allowed_channel_ids_env: "Channel IDs look like C0123ABCDE, separated by commas.",
  home_channel_env: "The default channel ID looks like C0123ABCDE.",
};
const SLACK_ERRORS = {
  invalid_auth: "Slack rejected the token.",
  not_authed: "Slack received no token.",
  account_inactive: "The Slack app or its workspace is inactive.",
  token_revoked: "The token was revoked. Reinstall the app and copy the new token.",
  not_allowed_token_type: "That token is the wrong type for this field.",
  missing_scope: "The app-level token needs the connections:write scope.",
};

class SlackFailure extends Error {
  constructor(state, message) {
    super(message);
    this.state = state;
  }
}

function environmentNames(descriptor) {
  const environment = descriptor?.interface_driver_config?.private_config?.environment;
  const names = {};
  for (const key of Object.keys(PATTERNS)) {
    if (typeof environment?.[key] !== "string" || !environment[key].trim()) {
      throw new Error(`Slack interface does not declare private_config.environment.${key}.`);
    }
    names[key] = environment[key].trim();
  }
  return names;
}

function apiBase() {
  const config = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "config.json"), "utf8"));
  const base = config?.transport?.api_base_default;
  if (typeof base !== "string" || !base.startsWith("https://")) throw new Error("Slack config declares no https API base.");
  return base.replace(/\/+$/, "");
}

function filled(map) {
  return Object.fromEntries(Object.entries(map ?? {})
    .filter(([, value]) => typeof value === "string" && value.trim())
    .map(([name, value]) => [name, value.trim()]));
}

function validate(names, values) {
  for (const key of ["bot_token_env", "app_token_env", "allowed_user_ids_env"]) {
    if (!values[names[key]]) return { state: "not_connected", message: `Enter the ${key === "allowed_user_ids_env" ? "allowed member IDs" : key === "bot_token_env" ? "bot token" : "app-level token"}.` };
  }
  for (const [key, pattern] of Object.entries(PATTERNS)) {
    const raw = values[names[key]];
    if (!raw) continue;
    const parts = LIST_KEYS.has(key) ? raw.split(",").map((part) => part.trim()).filter(Boolean) : [raw];
    if (!parts.length || parts.some((part) => !pattern.test(part))) return { state: "error", message: FIELD_NAMES[key] };
  }
  return null;
}

async function call(ctx, base, method, token) {
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(`${base}/${method}`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/x-www-form-urlencoded" },
      body: "",
      signal: ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout,
    });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    throw new SlackFailure("error", "Could not reach Slack.");
  }
  if (response.status === 429) throw new SlackFailure("error", "Slack is rate limiting this check. Try again in a minute.");
  if (!response.ok) throw new SlackFailure("error", `Slack answered HTTP ${response.status}.`);
  const body = await response.json();
  if (body?.ok !== true) {
    const code = typeof body?.error === "string" ? body.error : "unknown_error";
    const state = ["invalid_auth", "token_revoked", "account_inactive"].includes(code) ? "expired" : "error";
    throw new SlackFailure(state, SLACK_ERRORS[code] ?? `Slack refused the check (${code}).`);
  }
  return body;
}

async function check(ctx, values) {
  const names = environmentNames(ctx.descriptor);
  const invalid = validate(names, values);
  if (invalid) return invalid;
  const base = apiBase();
  try {
    const bots = values[names.bot_token_env].split(",").map((part) => part.trim()).filter(Boolean);
    const identities = [];
    for (const token of bots) identities.push(await call(ctx, base, "auth.test", token));
    const identity = identities[0];
    await call(ctx, base, "apps.connections.open", values[names.app_token_env]);
    return {
      state: "connected",
      account: {
        label: String(identity.team ?? identity.team_id ?? "Slack").slice(0, 96),
        detail: `Bot @${identity.user ?? identity.user_id ?? "unknown"}`.slice(0, 160),
      },
    };
  } catch (error) {
    if (error instanceof SlackFailure) return { state: error.state, message: error.message };
    throw error;
  }
}

async function probe(ctx) {
  return check(ctx, filled(ctx.values));
}

async function connect(ctx) {
  const input = filled(ctx.input);
  const result = await check(ctx, { ...filled(ctx.values), ...input });
  if (result.state === "connected" && Object.keys(input).length) await ctx.saveValues(input);
  return result;
}

async function disconnect(ctx) {
  const names = environmentNames(ctx.descriptor);
  await ctx.saveValues(Object.fromEntries(Object.values(names).map((name) => [name, null])));
  return { state: "not_connected" };
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
