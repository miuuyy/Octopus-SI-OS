const { spawn } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const LOGIN_TIMEOUT_MS = 300_000;
const REQUEST_TIMEOUT_MS = 30_000;
const IDLE_STOP_MS = 20_000;

function createChatgptSignIn(options) {
  let appServer = null;
  let idleTimer = null;
  let authBusy = false;
  let authGeneration = 0;
  let accountNotificationGeneration = 0;
  let modelDiscovery = null;
  const logins = new Map();

  function nativeAuthStamp(codexHome) {
    try {
      const stat = fs.statSync(path.join(codexHome, "auth.json"), { bigint: true });
      return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeNs}:${stat.ctimeNs}`;
    } catch (error) {
      if (error.code === "ENOENT") return null;
      throw error;
    }
  }

  function requireCurrentScope(server) {
    if (server !== appServer || server.closed || server.codexHome !== options.codexHome()
      || server.binary !== options.codexBinary()) throw new Error("ChatGPT account scope changed. Refresh and try again.");
  }

  function publishAccountChange() {
    authGeneration += 1;
    options.onAccountChanged?.();
  }

  function scheduleIdleStop() {
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      idleTimer = null;
      const server = appServer;
      if (!server || server.closed) return;
      if (server.pending.size > 0 || [...logins.values()].some((login) => login.state === "pending")) {
        scheduleIdleStop();
        return;
      }
      dispose();
    }, IDLE_STOP_MS);
    idleTimer.unref();
  }

  function ensureAppServer() {
    const binary = options.codexBinary();
    const codexHome = options.codexHome();
    const authStamp = nativeAuthStamp(codexHome);
    if (appServer && !appServer.closed) {
      if (appServer.binary === binary && appServer.codexHome === codexHome
        && (appServer.authStamp === authStamp || [...logins.values()].some((login) => login.server === appServer && login.state === "pending"))) return appServer.ready;
      dispose();
    }
    fs.accessSync(binary, fs.constants.X_OK);
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    const env = { ...process.env, CODEX_HOME: codexHome, CODEX_INTERNAL_ORIGINATOR_OVERRIDE: "octopus_desktop" };
    const child = spawn(binary, ["app-server"], {
      cwd: codexHome,
      env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    const server = { child, binary, codexHome, authStamp, closed: false, nextId: 1, pending: new Map(), buffer: "", stderr: "", ready: null,
      accountSnapshot: null, accountReadSequence: 0, accountUpdateRead: null };
    appServer = server;
    const fail = (error) => {
      server.closed = true;
      for (const waiter of server.pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(error);
      }
      server.pending.clear();
      for (const login of logins.values()) {
        if (login.state === "pending") Object.assign(login, { state: "failed", error: error.message });
      }
    };
    child.on("error", (error) => fail(new Error(`ChatGPT sign-in could not start: ${error.message}`)));
    child.on("exit", () => {
      if (!server.closed) fail(new Error("ChatGPT sign-in stopped before it finished"));
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => { server.stderr = (server.stderr + chunk).slice(-4000); });
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      server.buffer += chunk;
      for (;;) {
        const newline = server.buffer.indexOf("\n");
        if (newline < 0) break;
        const line = server.buffer.slice(0, newline).trim();
        server.buffer = server.buffer.slice(newline + 1);
        if (!line) continue;
        let message;
        try { message = JSON.parse(line); } catch { continue; }
        if (message.id !== undefined && (message.result !== undefined || message.error !== undefined)) {
          const waiter = server.pending.get(message.id);
          if (!waiter) continue;
          server.pending.delete(message.id);
          clearTimeout(waiter.timer);
          if (message.error) waiter.reject(new Error(String(message.error.message || "ChatGPT sign-in was refused")));
          else waiter.resolve(message.result);
          continue;
        }
        if (message.method === "account/login/completed") {
          const login = logins.get(String(message.params?.loginId ?? ""));
          if (login?.server === server && login.state === "pending" && !login.verifying) {
            if (message.params?.success !== true) {
              clearTimeout(login.timer);
              Object.assign(login, { state: "failed", error: String(message.params?.error || "Sign-in did not complete") });
            } else {
              login.verifying = true;
              const generation = authGeneration;
              void readAccount(server, true).then((value) => {
                if (login.state !== "pending" || generation !== authGeneration) return;
                if (!value.signedIn) throw new Error("ChatGPT sign-in finished without an authorized account.");
                server.authStamp = nativeAuthStamp(server.codexHome);
                clearTimeout(login.timer);
                Object.assign(login, { state: "completed", error: "" });
                publishAccountChange();
              }).catch((error) => {
                if (login.state !== "pending") return;
                clearTimeout(login.timer);
                Object.assign(login, { state: "failed", error: error.message });
                publishAccountChange();
              }).finally(scheduleIdleStop);
            }
          }
        }
        if (message.method === "account/updated" && server === appServer && !server.closed
          && !authBusy && ![...logins.values()].some((login) => login.state === "pending")) {
          verifyAccountUpdate(server, message.params);
        }
      }
    });
    server.request = (method, params) => new Promise((resolve, reject) => {
      if (server.closed) return reject(new Error("ChatGPT sign-in is not running"));
      const id = server.nextId++;
      const timer = setTimeout(() => {
        server.pending.delete(id);
        reject(new Error("ChatGPT sign-in did not respond"));
      }, REQUEST_TIMEOUT_MS);
      server.pending.set(id, { resolve, reject, timer });
      child.stdin.write(`${JSON.stringify({ id, method, ...(params === undefined ? {} : { params }) })}\n`);
    });
    server.ready = server.request("initialize", {
      clientInfo: { name: "octopus_desktop", title: "Octopus", version: options.version() },
      capabilities: { experimentalApi: true },
    }).then(() => {
      child.stdin.write(`${JSON.stringify({ method: "initialized" })}\n`);
      return server;
    });
    return server.ready;
  }

  function dispose() {
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = null;
    const server = appServer;
    appServer = null;
    modelDiscovery = null;
    if (server && !server.closed) {
      server.closed = true;
      for (const waiter of server.pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error("Codex account connection was closed."));
      }
      server.pending.clear();
      for (const login of logins.values()) {
        if (login.server !== server || login.state !== "pending") continue;
        clearTimeout(login.timer);
        Object.assign(login, { state: "failed", error: "ChatGPT account connection was closed." });
      }
      server.child.kill("SIGTERM");
    }
  }

  async function readAccount(server, refreshToken = false) {
    const generation = authGeneration;
    const authStamp = nativeAuthStamp(server.codexHome);
    const sequence = ++server.accountReadSequence;
    const response = await server.request("account/read", { refreshToken });
    requireCurrentScope(server);
    if (!response || !Object.hasOwn(response, "account") || (response.account !== null
      && !["chatgpt", "apiKey", "amazonBedrock", "gcp"].includes(response.account?.type))) {
      throw new Error("ChatGPT returned an invalid account response.");
    }
    const native = response.account;
    if (native?.type === "chatgpt" && ((native.email != null && typeof native.email !== "string")
      || (native.planType != null && typeof native.planType !== "string")
      || (native.workspaceName != null && typeof native.workspaceName !== "string"))) throw new Error("ChatGPT returned invalid account identity.");
    const value = {
      signedIn: native?.type === "chatgpt", authType: native?.type ?? null,
      email: native?.type === "chatgpt" ? native.email ?? null : null,
      planType: native?.type === "chatgpt" ? native.planType ?? null : null,
      workspaceName: native?.type === "chatgpt" ? native.workspaceName ?? null : null,
      credentialHome: server.codexHome, octopusHome: options.octopusHome?.() ?? null,
    };
    if (sequence > (server.accountSnapshot?.sequence ?? 0)
      && generation === authGeneration && authStamp === nativeAuthStamp(server.codexHome)) {
      const previous = server.accountSnapshot;
      server.accountSnapshot = { value, generation, authStamp, sequence };
      if (previous && previous.generation === generation
        && (previous.authStamp !== authStamp || JSON.stringify(previous.value) !== JSON.stringify(value))
        && !authBusy && ![...logins.values()].some((login) => login.state === "pending")) {
        accountNotificationGeneration += 1;
        options.onAccountChanged?.();
      }
    }
    return value;
  }

  function verifyAccountUpdate(server, notification) {
    const previous = server.accountSnapshot;
    const notificationGeneration = accountNotificationGeneration;
    const previousRead = server.accountUpdateRead;
    const read = (previousRead ? previousRead.catch(() => {}) : Promise.resolve()).then(async () => {
      requireCurrentScope(server);
      const value = await readAccount(server);
      const authMode = notification?.authMode === "chatgptAuthTokens" ? "chatgpt" : notification?.authMode;
      if (previous && previous.authStamp !== null && previous.authStamp === nativeAuthStamp(server.codexHome)
        && previous.generation === authGeneration && JSON.stringify(previous.value) === JSON.stringify(value)
        && authMode === value.authType && notification?.planType === value.planType) return;
      if (notificationGeneration === accountNotificationGeneration) {
        accountNotificationGeneration += 1;
        options.onAccountChanged?.();
      }
    }).catch((error) => {
      if (server === appServer && !server.closed && notificationGeneration === accountNotificationGeneration) {
        accountNotificationGeneration += 1;
        options.onAccountChanged?.();
      }
      throw error;
    }).finally(() => {
      if (server.accountUpdateRead === read) server.accountUpdateRead = null;
      scheduleIdleStop();
    });
    server.accountUpdateRead = read;
    void read.catch(() => {});
  }

  async function waitForAccountUpdates(server) {
    while (server.accountUpdateRead) await server.accountUpdateRead;
    requireCurrentScope(server);
  }

  async function account() {
    try { return await readAccount(await ensureAppServer()); }
    finally { scheduleIdleStop(); }
  }

  async function rateLimits() {
    const server = await ensureAppServer();
    const generation = authGeneration;
    const notificationGeneration = accountNotificationGeneration;
    const authStamp = nativeAuthStamp(server.codexHome);
    try {
      const before = await readAccount(server);
      if (!before.signedIn) throw new Error("Sign in with ChatGPT to see usage limits.");
      const response = await server.request("account/rateLimits/read");
      const after = await readAccount(server);
      if (generation !== authGeneration || authStamp !== nativeAuthStamp(server.codexHome)
        || (authStamp === null && notificationGeneration !== accountNotificationGeneration)
        || JSON.stringify(before) !== JSON.stringify(after)) {
        throw new Error("ChatGPT account changed while loading usage. Refresh and try again.");
      }
      if (!validLimit(response?.rateLimits)
        || (response.rateLimitsByLimitId != null && (typeof response.rateLimitsByLimitId !== "object" || Array.isArray(response.rateLimitsByLimitId)
          || !Object.values(response.rateLimitsByLimitId).every(validLimit)))) {
        throw new Error("ChatGPT returned invalid usage limits.");
      }
      return { rateLimits: response.rateLimits, rateLimitsByLimitId: response.rateLimitsByLimitId ?? null, observedAtMs: Date.now() };
    } finally { scheduleIdleStop(); }
  }

  function validLimit(limit) {
    if (!limit || typeof limit !== "object" || Array.isArray(limit)) return false;
    for (const key of ["limitId", "limitName", "planType", "rateLimitReachedType"]) {
      if (limit[key] != null && typeof limit[key] !== "string") return false;
    }
    for (const key of ["primary", "secondary"]) {
      const window = limit[key];
      if (window != null && (typeof window !== "object" || !Number.isFinite(window.usedPercent)
        || (window.windowDurationMins != null && !Number.isFinite(window.windowDurationMins))
        || (window.resetsAt != null && !Number.isFinite(window.resetsAt)))) return false;
    }
    const credits = limit.credits;
    if (credits != null && (typeof credits.hasCredits !== "boolean" || typeof credits.unlimited !== "boolean"
      || (credits.balance != null && typeof credits.balance !== "string"))) return false;
    const individual = limit.individualLimit;
    if (individual != null && (typeof individual.limit !== "string" || typeof individual.used !== "string"
      || !Number.isFinite(individual.remainingPercent) || !Number.isFinite(individual.resetsAt))) return false;
    return limit.spendControlReached == null || typeof limit.spendControlReached === "boolean";
  }

  function requireNoAccountChange() {
    if (authBusy || [...logins.values()].some((login) => login.state === "pending")) {
      throw new Error("A ChatGPT account change is already in progress.");
    }
  }

  async function beginBrowserLogin(server, loginId, address) {
    const authUrl = new URL(address);
    if (authUrl.protocol !== "https:") throw new Error("ChatGPT sign-in returned an unsafe address");
    const login = { state: "pending", error: "", server, verifying: false, timer: null };
    logins.set(loginId, login);
    login.timer = setTimeout(() => {
      if (login.state !== "pending") return;
      Object.assign(login, { state: "failed", error: "Sign-in was not finished in the browser in time" });
      void server.request("account/login/cancel", { loginId }).catch((error) => {
        login.error += `. Cancellation failed: ${error.message}`;
      }).finally(() => { publishAccountChange(); scheduleIdleStop(); });
    }, LOGIN_TIMEOUT_MS).unref();
    try { await options.openExternal(authUrl.toString()); }
    catch (error) {
      await cancel(loginId);
      throw error;
    }
    return { loginId, authUrl: authUrl.toString() };
  }

  async function start() {
    requireNoAccountChange();
    authBusy = true;
    authGeneration += 1;
    try {
      const server = await ensureAppServer();
      requireCurrentScope(server);
      const response = await server.request("account/login/start", { type: "chatgpt" });
      requireCurrentScope(server);
      if (response?.type !== "chatgpt" || typeof response.loginId !== "string" || typeof response.authUrl !== "string") {
        throw new Error("ChatGPT sign-in returned an unexpected response");
      }
      return await beginBrowserLogin(server, response.loginId, response.authUrl);
    } finally { authBusy = false; scheduleIdleStop(); }
  }

  function opaqueId(value) {
    return typeof value === "string" && value.length > 0 && value.trim() === value && !/[\r\n\0]/.test(value);
  }

  function optionalText(value) {
    if (value != null && typeof value !== "string") throw new Error("ChatGPT returned an invalid account list.");
    return value ?? null;
  }

  function accountSessions(response, server) {
    if (!response || typeof response !== "object" || !Array.isArray(response.sessions)
      || (response.activeSessionId !== null && !opaqueId(response.activeSessionId))) {
      throw new Error("ChatGPT returned an invalid account list.");
    }
    const sessionIds = new Set();
    const sessions = response.sessions.map((session) => {
      if (!opaqueId(session?.sessionId) || sessionIds.has(session.sessionId) || typeof session.isActive !== "boolean"
        || !Array.isArray(session.workspaces) || (session.selectedWorkspaceAccountId !== null && !opaqueId(session.selectedWorkspaceAccountId))) {
        throw new Error("ChatGPT returned an invalid account list.");
      }
      sessionIds.add(session.sessionId);
      const workspaceIds = new Set();
      const workspaces = session.workspaces.map((workspace) => {
        if (!opaqueId(workspace?.accountId) || workspaceIds.has(workspace.accountId)
          || (workspace.kind != null && !["personal", "workspace"].includes(workspace.kind))) {
          throw new Error("ChatGPT returned an invalid account list.");
        }
        workspaceIds.add(workspace.accountId);
        return { accountId: workspace.accountId, name: optionalText(workspace.name), kind: workspace.kind ?? null };
      });
      if (session.selectedWorkspaceAccountId !== null && !workspaceIds.has(session.selectedWorkspaceAccountId)) {
        throw new Error("ChatGPT returned an invalid account list.");
      }
      return {
        sessionId: session.sessionId, email: optionalText(session.email), displayName: optionalText(session.displayName),
        active: session.isActive, selectedWorkspaceId: session.selectedWorkspaceAccountId, workspaces,
      };
    });
    const active = sessions.filter((session) => session.active);
    if (response.activeSessionId === null ? active.length !== 0
      : active.length !== 1 || active[0].sessionId !== response.activeSessionId) {
      throw new Error("ChatGPT returned an invalid account list.");
    }
    return { activeSessionId: response.activeSessionId, sessions, credentialHome: server.codexHome, octopusHome: options.octopusHome?.() ?? null };
  }

  async function sessions() {
    const server = await ensureAppServer();
    const generation = authGeneration;
    const authStamp = nativeAuthStamp(server.codexHome);
    try {
      const before = await readAccount(server);
      if (!before.signedIn) return accountSessions({ activeSessionId: null, sessions: [] }, server);
      const response = await server.request("accountSession/list", { refreshWorkspaceMetadata: true });
      requireCurrentScope(server);
      const listed = accountSessions(response, server);
      const after = await readAccount(server);
      if (generation !== authGeneration || authStamp !== nativeAuthStamp(server.codexHome)
        || JSON.stringify(before) !== JSON.stringify(after)) {
        throw new Error("ChatGPT account changed while loading accounts. Refresh and try again.");
      }
      return listed;
    } finally { scheduleIdleStop(); }
  }

  async function switchWorkspace(request) {
    const sessionId = request?.sessionId, accountId = request?.accountId;
    if (!opaqueId(sessionId) || !opaqueId(accountId)) throw new Error("Choose a workspace from the account list.");
    requireNoAccountChange();
    authBusy = true;
    authGeneration += 1;
    let server = null, authStamp;
    try {
      server = await ensureAppServer();
      requireCurrentScope(server);
      authStamp = nativeAuthStamp(server.codexHome);
      const response = await server.request("accountSession/switch", { sessionId, accountId, allowBrowserLogin: true });
      requireCurrentScope(server);
      if (response && typeof response === "object" && Object.hasOwn(response, "loginId")) {
        if (!opaqueId(response.loginId) || typeof response.authUrl !== "string") {
          throw new Error("ChatGPT workspace sign-in returned an unexpected response");
        }
        return { state: "browser", ...await beginBrowserLogin(server, response.loginId, response.authUrl) };
      }
      const listed = accountSessions(response, server);
      if (listed.activeSessionId !== sessionId
        || listed.sessions.find((session) => session.sessionId === sessionId)?.selectedWorkspaceId !== accountId) {
        throw new Error("ChatGPT did not confirm the workspace switch.");
      }
      const account = await readAccount(server);
      if (!account.signedIn) throw new Error("ChatGPT switched workspaces without an authorized account.");
      server.authStamp = nativeAuthStamp(server.codexHome);
      publishAccountChange();
      return { state: "switched", account, sessions: listed };
    } catch (error) {
      if (authStamp !== undefined && nativeAuthStamp(server.codexHome) !== authStamp) publishAccountChange();
      throw error;
    } finally { authBusy = false; scheduleIdleStop(); }
  }

  function status(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login) throw new Error("Unknown ChatGPT sign-in attempt");
    return { state: login.state, error: login.error };
  }

  async function cancel(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login || login.state !== "pending") return { cancelled: false };
    const server = login.server;
    requireCurrentScope(server);
    const response = await server.request("account/login/cancel", { loginId: String(loginId) });
    if (!["canceled", "notFound"].includes(response?.status)) throw new Error("ChatGPT did not confirm sign-in cancellation.");
    clearTimeout(login.timer);
    Object.assign(login, { state: "failed", error: "Sign-in was cancelled" });
    publishAccountChange();
    scheduleIdleStop();
    return { cancelled: response.status === "canceled" };
  }

  async function logout() {
    if (authBusy) throw new Error("A ChatGPT account change is already in progress.");
    authBusy = true;
    authGeneration += 1;
    try {
      for (const [loginId, login] of logins) {
        if (login.state === "pending") await cancel(loginId);
      }
      const server = await ensureAppServer();
      await server.request("account/logout");
      const value = await readAccount(server);
      if (value.signedIn) throw new Error("ChatGPT still reports a signed-in account after logout. Refresh and try again.");
      server.authStamp = nativeAuthStamp(server.codexHome);
      publishAccountChange();
      return value;
    } finally { authBusy = false; scheduleIdleStop(); }
  }

  async function models() {
    const server = await ensureAppServer();
    const generation = authGeneration;
    const authStamp = nativeAuthStamp(server.codexHome);
    const before = await readAccount(server).finally(scheduleIdleStop);
    await waitForAccountUpdates(server);
    if (generation !== authGeneration || authStamp !== nativeAuthStamp(server.codexHome)) {
      throw new Error("ChatGPT account changed while loading models. Refresh and try again.");
    }
    const notificationGeneration = accountNotificationGeneration;
    if (!modelDiscovery || modelDiscovery.server !== server || modelDiscovery.generation !== generation
      || modelDiscovery.authStamp !== authStamp || modelDiscovery.notificationGeneration !== notificationGeneration) {
      const discovery = { server, generation, authStamp, notificationGeneration };
      discovery.promise = readModels(server).then(async (result) => {
        const after = await readAccount(server);
        await waitForAccountUpdates(server);
        requireCurrentScope(server);
        if (generation !== authGeneration || authStamp !== nativeAuthStamp(server.codexHome)
          || notificationGeneration !== accountNotificationGeneration || JSON.stringify(before) !== JSON.stringify(after)) {
          throw new Error("ChatGPT account changed while loading models. Refresh and try again.");
        }
        return result;
      }).finally(() => {
        if (modelDiscovery === discovery) modelDiscovery = null;
      });
      modelDiscovery = discovery;
    }
    return modelDiscovery.promise;
  }

  async function readModels(server) {
    const result = [];
    const modelIds = new Set();
    const cursors = new Set();
    let cursor = null;
    try {
      for (let page = 0; page < 100; page += 1) {
        const response = await server.request("model/list", { cursor, includeHidden: true });
        if (!Array.isArray(response?.data)) throw new Error("Codex returned an invalid model list.");
        for (const model of response.data) {
          const efforts = model?.supportedReasoningEfforts?.map((entry) => entry?.reasoningEffort);
          const hasNoReasoningControls = efforts?.length === 0 && model?.defaultReasoningEffort === "none";
          if (typeof model?.model !== "string" || !model.model || modelIds.has(model.model)
            || typeof model.displayName !== "string" || !Array.isArray(efforts)
            || !efforts.every((effort) => typeof effort === "string" && effort)
            || new Set(efforts).size !== efforts.length
            || (!hasNoReasoningControls && !efforts.includes(model.defaultReasoningEffort))) {
            throw new Error("Codex returned an invalid or repeated model.");
          }
          modelIds.add(model.model);
          result.push({
            model: model.model, displayName: model.displayName,
            description: typeof model.description === "string" ? model.description : "",
            defaultReasoningEffort: hasNoReasoningControls ? "" : model.defaultReasoningEffort, supportedReasoningEfforts: efforts,
            showInPicker: model.hidden !== true, isDefault: model.isDefault === true,
            ...require("./providerModelTiers.cjs").modelServiceTiers(model.serviceTiers, model.defaultServiceTier, `Codex model '${model.model}'`),
          });
        }
        if (response.nextCursor === null) {
          if (server !== appServer || server.codexHome !== options.codexHome() || server.binary !== options.codexBinary()) {
            throw new Error("Codex account scope changed during model discovery.");
          }
          return result;
        }
        if (typeof response.nextCursor !== "string" || !response.nextCursor || cursors.has(response.nextCursor)) {
          throw new Error("Codex returned an invalid or repeated model-list cursor.");
        }
        cursor = response.nextCursor;
        cursors.add(cursor);
      }
      throw new Error("Codex model list exceeded its pagination limit.");
    } finally {
      scheduleIdleStop();
    }
  }

  return { account, rateLimits, sessions, switchWorkspace, start, status, cancel, logout, dispose, models };
}

module.exports = { createChatgptSignIn };
