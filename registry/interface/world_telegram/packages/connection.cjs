"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");

const TOKEN = /^[0-9]{1,20}:[A-Za-z0-9_-]{20,128}$/;
const USER_ID = /^[1-9][0-9]{0,15}$/;
const CHAT_ID = /^(?:-?[1-9][0-9]{0,15}|@[A-Za-z][A-Za-z0-9_]{3,31})$/;
const ENV_NAME = /^[A-Z_][A-Z0-9_]*$/;
const CODE_ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const PAIRING_WINDOW_MS = 300_000;
const POLL_SECONDS = 20;
const CONFIRMATION = "Octopus is connected. Messages you send here reach the tasks you connect to Telegram.";
const CONFIG_PATH = path.join(__dirname, "..", "config.json");

class Cancelled extends Error {}

class Problem extends Error {}

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

function settings() {
  const config = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf8"));
  const value = {
    apiBaseEnv: config?.transport?.api_base_env,
    apiBaseDefault: config?.transport?.api_base_default,
    allowAllEnv: config?.authz?.allow_all_users_env,
    allowedChatsEnv: config?.authz?.allowed_chat_ids_env,
  };
  if (Object.values(value).some((entry) => typeof entry !== "string" || !entry)) {
    throw new Problem("The Telegram connector configuration is incomplete. Reinstall the Telegram application.");
  }
  return value;
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
  if (typeof saved === "string" && saved.trim()) return saved.trim();
  const launched = await ctx.launchEnvironment(name);
  return typeof launched === "string" ? launched.trim() : "";
}

function typed(ctx, name) {
  return Object.prototype.hasOwnProperty.call(ctx.input ?? {}, name) ? String(ctx.input[name] ?? "").trim() : undefined;
}

function parseUsers(text) {
  const parts = String(text ?? "").split(/[\s,]+/).filter(Boolean);
  if (parts.some((part) => !USER_ID.test(part) || Number(part) >= 2 ** 53)) {
    throw new Problem("Allowed user IDs must be numeric Telegram user IDs separated by commas.");
  }
  return [...new Set(parts)];
}

function parseChat(text) {
  if (!CHAT_ID.test(text)) throw new Problem("Home chat ID must be a numeric Telegram chat ID or a public @username.");
  return text;
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

function account(me, users, pairedWith) {
  const detail = pairedWith ? `Paired with ${pairedWith}`
    : users.length === 1 ? `Allowed user ${users[0]}`
      : users.length ? `${users.length} allowed users` : me.first_name || "Telegram bot";
  return { label: `@${me.username}`.slice(0, 96), detail: detail.slice(0, 160) };
}

async function apiBase(ctx, config) {
  const value = (await stored(ctx, config.apiBaseEnv)) || config.apiBaseDefault;
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Problem("The Telegram Bot API address is not a URL.");
  }
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if ((url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) || url.username || url.password || url.search || url.hash) {
    throw new Problem("The Telegram Bot API address must be an HTTPS origin.");
  }
  return value.replace(/\/+$/, "");
}

async function call(ctx, base, token, method, params = {}, timeoutMs = 15_000) {
  if (ctx.signal?.aborted) throw new Cancelled();
  const controller = new AbortController();
  const abort = () => controller.abort();
  const timer = setTimeout(abort, timeoutMs);
  ctx.signal?.addEventListener?.("abort", abort, { once: true });
  try {
    const response = await ctx.fetch(`${base}/bot${token}/${method}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(params),
      signal: controller.signal,
      redirect: "error",
    });
    const text = await response.text();
    let body;
    try {
      body = JSON.parse(text);
    } catch {
      throw new BotApiError(method, response.status, {});
    }
    if (body?.ok === true && response.status === 200) return body.result;
    throw new BotApiError(method, response.status, body);
  } catch (error) {
    if (error instanceof BotApiError) throw error;
    if (ctx.signal?.aborted) throw new Cancelled();
    throw new Problem(controller.signal.aborted
      ? "Telegram did not answer in time. Check the internet connection and try again."
      : "Octopus could not reach Telegram. Check the internet connection or proxy and try again.");
  } finally {
    clearTimeout(timer);
    ctx.signal?.removeEventListener?.("abort", abort);
  }
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

async function takeOverWebhook(ctx, base, token) {
  const info = await call(ctx, base, token, "getWebhookInfo");
  if (typeof info?.url === "string" && info.url) {
    await call(ctx, base, token, "deleteWebhook", { drop_pending_updates: false });
    ctx.progress({ message: "This bot was sending its messages to a webhook. Octopus switched it to receive them directly." });
  }
}

async function pair(ctx, base, token, me) {
  await takeOverWebhook(ctx, base, token);
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
      updates = await call(ctx, base, token, "getUpdates", { timeout, limit: 100 }, (timeout + 15) * 1000);
      if (failures) ctx.progress(waiting);
      failures = 0;
    } catch (error) {
      if (error instanceof Cancelled || rejected(error)) throw error;
      if (error instanceof BotApiError && error.code === 409) {
        return {
          state: "error",
          account: account(me, []),
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
        await call(ctx, base, token, "getUpdates", { offset: list[0].update_id + 1, limit: 1, timeout: 0 }).catch((error) => {
          if (error instanceof Cancelled) throw error;
        });
      }
      return { userId: message.from.id, chatId: message.chat.id, name: displayName(message.from) };
    }
    if (list.length) await sleep(ctx, 1500);
  }
  return {
    state: "not_connected",
    account: account(me, []),
    message: `Telegram did not receive Start in @${me.username} within 5 minutes. Press Connect to try again.`,
  };
}

async function guarded(operation) {
  try {
    const state = await operation();
    return typeof state.message === "string" ? { ...state, message: state.message.slice(0, 300) } : state;
  } catch (error) {
    if (error instanceof Cancelled) return { state: "not_connected", message: "Connecting to Telegram was cancelled." };
    if (rejected(error)) return { state: "expired", message: "Telegram no longer accepts this bot token. Paste a new token from @BotFather." };
    if (error instanceof Problem) return { state: "error", message: error.message };
    if (error instanceof BotApiError) return { state: "error", message: redact(error.description ? `Telegram refused the request: ${error.description}` : error.message) };
    return { state: "error", message: redact(error instanceof Error ? error.message : String(error)) };
  }
}

module.exports = {
  schemaVersion: 1,

  async probe(ctx) {
    return guarded(async () => {
      const names = environmentNames(ctx.descriptor);
      const config = settings();
      const token = await stored(ctx, names.token);
      if (!token) return { state: "not_connected", message: "Paste your bot token from @BotFather to connect Telegram." };
      if (!TOKEN.test(token)) return { state: "expired", message: "The saved bot token is malformed. Paste the token from @BotFather again." };
      const me = bot(await call(ctx, await apiBase(ctx, config), token, "getMe"));
      const users = parseUsers(await stored(ctx, names.users));
      const groups = await stored(ctx, config.allowedChatsEnv);
      const everyone = /^(?:1|true|yes|on)$/i.test(await stored(ctx, config.allowAllEnv));
      if (!users.length && !groups && !everyone) {
        return { state: "not_connected", account: account(me, users), message: `Press Connect, then Start in @${me.username}, to link your Telegram account.` };
      }
      return { state: "connected", account: account(me, users) };
    });
  },

  async connect(ctx) {
    return guarded(async () => {
      const names = environmentNames(ctx.descriptor);
      const config = settings();
      const saved = await stored(ctx, names.token);
      const token = typed(ctx, names.token) || saved;
      if (!token) return { state: "not_connected", message: "Paste your bot token from @BotFather to connect Telegram." };
      if (!TOKEN.test(token)) {
        return { state: "error", message: "That does not look like a bot token. Copy the whole token from @BotFather; it looks like 123456789:AAH…" };
      }
      const base = await apiBase(ctx, config);
      ctx.progress({ message: "Checking the bot token with Telegram…" });
      let me;
      try {
        me = bot(await call(ctx, base, token, "getMe"));
      } catch (error) {
        if (rejected(error)) return { state: "error", message: "Telegram rejected this token. Copy it again from @BotFather, or issue a new one there with /token." };
        throw error;
      }
      const sameBot = !saved || saved.split(":")[0] === token.split(":")[0];
      const users = parseUsers(typed(ctx, names.users) ?? (sameBot ? await stored(ctx, names.users) : ""));
      const home = typed(ctx, names.home);
      if (home) parseChat(home);
      if (users.length) {
        await ctx.saveValues({ [names.token]: token, [names.users]: users.join(","), ...(home === undefined ? {} : { [names.home]: home || null }) });
        return { state: "connected", account: account(me, users), message: `@${me.username} is connected.` };
      }
      const paired = await pair(ctx, base, token, me);
      if (paired.state) return paired;
      await ctx.saveValues({ [names.token]: token, [names.users]: String(paired.userId), [names.home]: home || String(paired.chatId) });
      const confirmed = await call(ctx, base, token, "sendMessage", { chat_id: paired.chatId, text: CONFIRMATION }).then(() => true, (error) => {
        if (error instanceof Cancelled) throw error;
        return false;
      });
      return {
        state: "connected",
        account: account(me, [paired.userId], paired.name),
        message: confirmed
          ? `@${me.username} is connected to ${paired.name}'s Telegram account.`
          : `@${me.username} is connected to ${paired.name}'s Telegram account, but Telegram did not accept the confirmation message.`,
      };
    });
  },

  async disconnect(ctx) {
    return guarded(async () => {
      const names = environmentNames(ctx.descriptor);
      await ctx.saveValues({ [names.token]: null, [names.users]: null, [names.home]: null });
      return { state: "not_connected", message: "Telegram is disconnected. Connect again with the bot token to use it." };
    });
  },
};
