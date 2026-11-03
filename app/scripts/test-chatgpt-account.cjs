const assert = require("node:assert/strict");
const { test } = require("node:test");
const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const tick = () => new Promise((resolve) => setImmediate(resolve));
const signedIn = { signedIn: true, authType: "chatgpt", email: "first@example.test", planType: "pro", workspaceName: null, credentialHome: "/native", octopusHome: "/home" };
const signedOut = { ...signedIn, signedIn: false, authType: null, email: null, planType: null };
const limit = { limitId: "core", limitName: "Core", primary: { usedPercent: 25, windowDurationMins: 300, resetsAt: 2000000000 }, secondary: null, credits: null, individualLimit: null, planType: "pro", rateLimitReachedType: null, spendControlReached: null };

function loadTs(relative, imports = {}, extraSource = "") {
  const module = { exports: {} };
  const source = fs.readFileSync(path.join(__dirname, "../src", relative), "utf8") + extraSource;
  const output = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  new Function("module", "exports", "require", output)(module, module.exports, (name) => imports[name] ?? imports.$fallback?.(name) ?? (name.endsWith(".css") ? {} : require(name)));
  return module.exports;
}

const ui = {
  OcButton: ({ children, disabled, onClick, type }) => React.createElement("button", { disabled, onClick, type }, children),
  OcSettingsRow: ({ label, description, control }) => React.createElement("div", null, label, description, control),
  OcDialog: ({ open, children }) => open ? children : null,
  OcIconButton: ({ label, onClick }) => React.createElement("button", { onClick }, label),
  OcMenuItem: ({ label, description, onClick, disabled }) => React.createElement("button", { onClick, disabled }, label, description),
};
const usageModel = loadTs("account/chatgptUsageModel.ts");
const loadingViews = loadTs("design-system/LoadingSkeleton.tsx", {
  "../appearance": { useShouldReduceMotion: () => true },
});
const usageViews = loadTs("components/account/AccountUsage.tsx", {
  "../../account/chatgptUsageModel": usageModel, "../../design-system/components": ui,
  "../../design-system/LoadingSkeleton": loadingViews,
  "../IconGlyphs": { Glyph: ({ name }) => React.createElement("svg", { "data-glyph": name }) },
});
const logoutView = loadTs("components/account/AccountLogoutDialog.tsx", { "../../design-system/components": ui });
const accountStateModule = loadTs("account/chatgptAccountState.ts");
const workspaceModel = loadTs("account/chatgptWorkspaces.ts");
const accountModels = { "../../account/chatgptAccountState": accountStateModule, "../../account/chatgptWorkspaces": workspaceModel };
const emptyUsage = { identity: null, value: null, loading: false, error: "" };

function nativeFixture(env = {}) {
  let home = "/native", persisted = { type: "chatgpt", email: "first@example.test", planType: "pro" }, stamp = 1n, changes = 0;
  const children = [], calls = [];
  const control = { refuseLogout: false, quotaError: "", quotaHook: null, selected: "personal-1", ssoWorkspace: "", sessionsList: null,
    workspaces: [{ accountId: "personal-1", name: null, imageUrl: null, kind: "personal" }, { accountId: "team-1", name: "Team", imageUrl: null, kind: "workspace" }] };
  const sessionList = (child) => child.cachedAccount?.type !== "chatgpt" ? { activeSessionId: null, sessions: [] } : { activeSessionId: "session-1", sessions: [{
    sessionId: "session-1", email: child.cachedAccount.email, userId: "user-1", displayName: null, imageUrl: null, lastUsedAt: 1, isActive: true,
    selectedWorkspaceAccountId: control.selected, workspaces: control.workspaces }] };
  const module = { exports: {} };
  const fileSystem = { constants: { X_OK: 1 }, accessSync() {}, mkdirSync() {}, statSync() {
    if (control.authMissing) throw Object.assign(new Error("Missing"), { code: "ENOENT" });
    return { dev: 1n, ino: 1n, size: stamp, mtimeNs: stamp, ctimeNs: stamp };
  } };
  const spawn = (_binary, _args, options) => {
    const child = new EventEmitter();
    child.spawnEnv = options.env;
    child.cachedAccount = persisted;
    child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
    child.stdout.setEncoding = child.stderr.setEncoding = () => {};
    child.kill = () => { child.killed = true; };
    child.stdin = { write(line) {
      const request = JSON.parse(line);
      calls.push({ ...request, home: options.env.CODEX_HOME });
      if (request.id === undefined) return;
      let result = {}, error;
      if (request.method === "account/read") {
        result = { account: child.cachedAccount, requiresOpenaiAuth: true };
        if (control.accountError) error = { message: control.accountError };
      }
      if (request.method === "account/rateLimits/read") {
        control.quotaHook?.(child);
        result = { rateLimits: limit, rateLimitsByLimitId: { core: limit, extra: { ...limit, limitId: "extra", primary: null } } };
        if (control.quotaError) error = { message: control.quotaError };
      }
      if (request.method === "account/login/start") result = { type: "chatgpt", loginId: `login-${children.length}`, authUrl: "https://example.test/login" };
      if (request.method === "account/login/cancel") result = { status: "canceled" };
      if (request.method === "accountSession/list") result = control.sessionsList?.(child) ?? sessionList(child);
      if (request.method === "accountSession/switch" && request.params.accountId === control.ssoWorkspace) {
        result = { loginId: "workspace-login", authUrl: "https://example.test/sso" };
      } else if (request.method === "accountSession/switch" && control.switchError) {
        stamp += 1n; error = { message: control.switchError };
      } else if (request.method === "accountSession/switch") {
        control.selected = request.params.accountId;
        persisted = child.cachedAccount = { ...child.cachedAccount, workspaceName: control.workspaces.find((workspace) => workspace.accountId === control.selected)?.name ?? null };
        stamp += 1n;
        result = sessionList(child);
      }
      if (request.method === "model/list") result = { data: control.modelRows ?? [{ model: "model", displayName: "Model", defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "high" }] }], nextCursor: null };
      if (request.method === "account/logout" && !control.refuseLogout) { persisted = child.cachedAccount = null; stamp += 1n; }
      const reply = () => child.stdout.emit("data", JSON.stringify({ id: request.id, ...(error ? { error } : { result }) }) + "\n");
      if (request.method === "account/read" && control.accountReply) control.accountReply(reply);
      else if (request.method === "model/list" && control.modelReply) control.modelReply(reply);
      else queueMicrotask(reply);
    } };
    children.push(child);
    return child;
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "../electron/chatgptSignIn.cjs"), "utf8"), {
    module, require: (name) => name === "node:fs" ? fileSystem : name === "node:child_process" ? { spawn } : require(require.resolve(name, { paths: [path.join(__dirname, "../electron")] })),
    process: { env }, URL, setTimeout, clearTimeout,
  });
  const client = module.exports.createChatgptSignIn({ codexBinary: () => "/fixture/codex", codexHome: () => home,
    octopusHome: () => "/home", version: () => "test", openExternal: async () => {}, onAccountChanged: () => { changes += 1; } });
  return { client, control, children, calls, get changes() { return changes; },
    externalAccount(account) { persisted = account; stamp += 1n; }, changeHome(value) { home = value; },
    complete(loginId, account, success = true) {
      persisted = account; stamp += 1n;
      const child = children.at(-1); child.cachedAccount = account;
      child.stdout.emit("data", JSON.stringify({ method: "account/login/completed", params: { loginId, success, error: success ? null : "Refused" } }) + "\n");
    } };
}

test("native account discovery owns its client identity and credential Home", async () => {
  const inherited = { CODEX_HOME: "/other-client", CODEX_INTERNAL_ORIGINATOR_OVERRIDE: "other-client",
    HTTPS_PROXY: "https://proxy.example.test" };
  const fixture = nativeFixture(inherited);
  try {
    await fixture.client.account();
    const env = fixture.children[0].spawnEnv;
    assert.equal(env.CODEX_HOME, "/native");
    assert.equal(env.CODEX_INTERNAL_ORIGINATOR_OVERRIDE, "octopus_desktop");
    assert.equal(env.HTTPS_PROXY, inherited.HTTPS_PROXY);
    assert.equal(inherited.CODEX_INTERNAL_ORIGINATOR_OVERRIDE, "other-client");
    assert.equal(fixture.calls.find((call) => call.method === "initialize").params.clientInfo.name, env.CODEX_INTERNAL_ORIGINATOR_OVERRIDE);
  } finally { fixture.client.dispose(); }
});

test("models without reasoning controls are normalized while malformed catalogues still fail closed", async () => {
  const row = { model: "model", displayName: "Model", defaultReasoningEffort: "none", supportedReasoningEfforts: [] };
  for (const rows of [[row], [{ ...row, defaultReasoningEffort: "high" }], [row, row],
    [{ ...row, supportedReasoningEfforts: [{ reasoningEffort: "none" }, { reasoningEffort: "none" }] }]]) {
    const fixture = nativeFixture();
    fixture.control.modelRows = rows;
    try {
      if (rows.length === 1 && rows[0] === row) {
        const [model] = await fixture.client.models();
        assert.equal(model.defaultReasoningEffort, "");
        assert.equal(model.supportedReasoningEfforts.length, 0);
      } else {
        await assert.rejects(fixture.client.models(), /invalid or repeated model/);
      }
    } finally { fixture.client.dispose(); }
  }
});

test("simultaneous catalogue reads share discovery and account changes invalidate pending results", async () => {
  const fixture = nativeFixture(), { client } = fixture;
  const replies = [];
  fixture.control.modelReply = reply => replies.push(reply);
  const count = () => fixture.calls.filter(call => call.method === "model/list").length;
  try {
    const first = client.models(), second = client.models();
    await tick(); assert.equal(count(), 1);
    replies.shift()();
    assert.equal((await first)[0].model, "model"); assert.deepEqual(await first, await second);
    const stale = client.models();
    await tick(); assert.equal(count(), 2);
    fixture.externalAccount({ type: "chatgpt", email: "second@example.test", planType: "plus" });
    replies.shift()();
    await assert.rejects(stale, /account changed/);
    const current = client.models(); await tick(); assert.equal(count(), 3);
    replies.shift()(); assert.equal((await current)[0].model, "model");
    const oldPlan = client.models(); await tick();
    fixture.children.at(-1).cachedAccount = { ...fixture.children.at(-1).cachedAccount, planType: "pro" };
    fixture.children.at(-1).stdout.emit("data", JSON.stringify({ method: "account/updated", params: { authMode: "chatgpt", planType: "pro" } }) + "\n");
    const newPlan = client.models(); await tick(); assert.equal(count(), 5);
    replies.shift()(); await assert.rejects(oldPlan, /account changed/);
    replies.shift()(); assert.equal((await newPlan)[0].model, "model");
    const oldIdentity = client.models(); await tick();
    fixture.children.at(-1).cachedAccount = { ...fixture.children.at(-1).cachedAccount, email: "third@example.test" };
    await client.account();
    fixture.children.at(-1).stdout.emit("data", JSON.stringify({ method: "account/updated", params: { authMode: "chatgpt", planType: "pro" } }) + "\n");
    const newIdentity = client.models(); await tick(); assert.equal(count(), 7);
    replies.shift()(); await assert.rejects(oldIdentity, /account changed/);
    replies.shift()(); assert.equal((await newIdentity)[0].model, "model");
    const oldScope = client.models(); await tick(); fixture.changeHome("/other"); replies.shift()();
    await assert.rejects(oldScope, /scope changed/);
  } finally { client.dispose(); }
});

test("unchanged native notifications verify identity without invalidating discovery or publishing refresh loops", async () => {
  const fixture = nativeFixture(), { client } = fixture;
  const replies = [], accountReplies = [];
  fixture.control.modelReply = reply => replies.push(reply);
  fixture.control.accountReply = reply => {
    if (fixture.calls.filter(call => call.method === "account/read").length === 1) queueMicrotask(reply);
    else accountReplies.push(reply);
  };
  const notify = () => fixture.children.at(-1).stdout.emit("data", JSON.stringify({ method: "account/updated", params: { authMode: "chatgpt", planType: "pro" } }) + "\n");
  try {
    let completed = false;
    const first = client.models().then(value => { completed = true; return value; });
    const initialJoined = client.models();
    await tick();
    assert.equal(fixture.calls.findIndex(call => call.method === "account/read") < fixture.calls.findIndex(call => call.method === "model/list"), true);
    notify(); await tick();
    replies.shift()(); await tick();
    assert.equal(completed, false, "discovery must wait for notification identity verification");
    fixture.control.accountReply = null;
    for (const reply of accountReplies.splice(0)) reply();
    assert.equal((await first)[0].model, "model");
    assert.deepEqual(await initialJoined, await first);
    const second = client.models(); await tick();
    notify(); notify();
    const joined = client.models(); await tick();
    assert.equal(fixture.calls.filter(call => call.method === "model/list").length, 2);
    replies.shift()(); await Promise.all([second, joined]);
    assert.equal(fixture.changes, 0);
    const failed = client.models(); await tick();
    fixture.control.accountError = "Identity unavailable";
    notify(); await tick(); replies.shift()();
    await assert.rejects(failed, /Identity unavailable/);
    assert.ok(fixture.changes > 0);
  } finally { client.dispose(); }
});

test("native identity, plan, named limits, logout and login replacement use one credential scope", async () => {
  const fixture = nativeFixture(), { client } = fixture;
  try {
    assert.deepEqual(JSON.parse(JSON.stringify(await client.account())), signedIn);
    const usage = await client.rateLimits();
    assert.deepEqual(Object.keys(usage.rateLimitsByLimitId), ["core", "extra"]);
    assert.equal(usage.rateLimitsByLimitId.core.primary.usedPercent, 25);
    fixture.control.quotaHook = (child) => child.stdout.emit("data", JSON.stringify({ method: "account/updated", params: { authMode: "chatgpt", planType: "pro" } }) + "\n");
    const quotaReads = fixture.calls.filter((call) => call.method === "account/rateLimits/read").length;
    assert.equal((await client.rateLimits()).rateLimits.primary.usedPercent, 25);
    assert.equal(fixture.calls.filter((call) => call.method === "account/rateLimits/read").length, quotaReads + 1);
    fixture.control.authMissing = true;
    await assert.rejects(client.rateLimits(), /account changed/);
    fixture.control.authMissing = false;
    fixture.control.quotaHook = null;
    fixture.control.quotaError = "Offline";
    await assert.rejects(client.rateLimits(), /Offline/);
    assert.equal((await client.account()).signedIn, true);
    fixture.control.quotaError = "";
    assert.deepEqual(JSON.parse(JSON.stringify(await client.logout())), signedOut);
    await assert.rejects(client.rateLimits(), /Sign in/);
    const { loginId, authUrl } = await client.start();
    assert.equal(authUrl, "https://example.test/login");
    await assert.rejects(client.start(), /already in progress/);
    fixture.complete(loginId, { type: "chatgpt", email: "second@example.test", planType: "plus" });
    await tick();
    assert.equal(client.status(loginId).state, "completed");
    assert.equal((await client.account()).email, "second@example.test");
    assert.equal((await client.account()).planType, "plus");
    assert.ok(fixture.calls.some((call) => call.method === "account/read" && call.params.refreshToken === true));
    assert.ok(fixture.changes >= 2);
    assert.ok(fixture.calls.every((call) => call.home === "/native"));
    fixture.externalAccount(null);
    assert.equal((await client.account()).signedIn, false);
    assert.ok(fixture.children[0].killed);
  } finally { client.dispose(); }
});

test("account sessions list workspaces and switch them in the shared credential scope or through browser sign-in", async () => {
  const fixture = nativeFixture(), { client } = fixture;
  try {
    const listed = await client.sessions();
    assert.equal(fixture.calls.find((call) => call.method === "initialize").params.capabilities.experimentalApi, true);
    assert.equal(fixture.calls.find((call) => call.method === "accountSession/list").params.refreshWorkspaceMetadata, true);
    assert.deepEqual(JSON.parse(JSON.stringify(listed.sessions[0].workspaces)), [{ accountId: "personal-1", name: null, kind: "personal" }, { accountId: "team-1", name: "Team", kind: "workspace" }]);
    assert.equal(listed.sessions[0].selectedWorkspaceId, "personal-1"); assert.equal(listed.credentialHome, "/native");
    const changes = fixture.changes;
    const switched = await client.switchWorkspace({ sessionId: "session-1", accountId: "team-1" });
    assert.equal(switched.state, "switched"); assert.equal(switched.account.workspaceName, "Team");
    assert.equal(switched.sessions.sessions[0].selectedWorkspaceId, "team-1");
    assert.ok(fixture.changes > changes, "a switch publishes an account change so model catalogues reload");
    assert.equal((await client.account()).workspaceName, "Team");
    fixture.control.ssoWorkspace = "personal-1";
    assert.deepEqual(JSON.parse(JSON.stringify(await client.switchWorkspace({ sessionId: "session-1", accountId: "personal-1" }))),
      { state: "browser", loginId: "workspace-login", authUrl: "https://example.test/sso" });
    assert.equal(client.status("workspace-login").state, "pending");
    await assert.rejects(client.switchWorkspace({ sessionId: "session-1", accountId: "team-1" }), /already in progress/);
    await client.cancel("workspace-login");
    await assert.rejects(client.switchWorkspace({ sessionId: " session-1", accountId: "team-1" }), /Choose a workspace/);
    fixture.control.switchError = "could not restore the previous workspace";
    const beforeFailure = fixture.changes;
    await assert.rejects(client.switchWorkspace({ sessionId: "session-1", accountId: "team-1" }), /could not restore/);
    assert.ok(fixture.changes > beforeFailure, "a failed switch that rewrote the shared credential still publishes an account change");
    fixture.control.switchError = "";
    fixture.control.sessionsList = () => ({ activeSessionId: "session-1", sessions: [] });
    await assert.rejects(client.sessions(), /invalid account list/);
    assert.ok(fixture.calls.every((call) => call.home === "/native"));
  } finally { client.dispose(); }
});

test("workspace switching replaces the observation, drops stale workspace lists and discloses the shared sign-in", async () => {
  const { createChatgptAccountState } = accountStateModule;
  const team = { ...signedIn, workspaceName: "Team" };
  const sessions = (selected) => ({ activeSessionId: "session-1", credentialHome: "/native", octopusHome: "/home", sessions: [{ sessionId: "session-1",
    email: signedIn.email, displayName: null, active: true, selectedWorkspaceId: selected,
    workspaces: [{ accountId: "personal-1", name: null, kind: "personal" }, { accountId: "team-1", name: "Team", kind: "workspace" }] }] });
  let account = signedIn, listed = 0, switchResult;
  const bridge = { chatgptAccount: async () => account, chatgptAccountSessions: async () => { listed += 1; return sessions(account.workspaceName ? "team-1" : "personal-1"); },
    chatgptSwitchWorkspace: async () => switchResult(), chatgptSignInStatus: async () => ({ state: "completed", error: "" }), onChatgptAccountChanged: () => () => {} };
  const controller = createChatgptAccountState(bridge, "/home");
  await controller.refresh(); await controller.loadSessions();
  assert.equal(controller.getSnapshot().sessions.sessions[0].selectedWorkspaceId, "personal-1");
  await controller.refresh(); assert.ok(controller.getSnapshot().sessions, "an unchanged account keeps its workspace list");
  switchResult = () => { account = team; return { state: "switched", account: team, sessions: sessions("team-1") }; };
  assert.equal(await controller.switchWorkspace("session-1", "team-1"), "switched");
  assert.equal(controller.getSnapshot().account.workspaceName, "Team");
  assert.equal(controller.getSnapshot().sessions.sessions[0].selectedWorkspaceId, "team-1"); assert.equal(listed, 1);
  account = signedIn; await controller.refresh();
  assert.equal(controller.getSnapshot().sessions, null, "another workspace observation invalidates the listed workspaces");
  switchResult = () => ({ state: "browser", loginId: "workspace-login", authUrl: "https://example.test/sso" });
  assert.equal(await controller.switchWorkspace("session-1", "team-1"), "browser");
  assert.equal(controller.getSnapshot().action, "waiting"); assert.equal(controller.getSnapshot().signInUrl, "https://example.test/sso");
  await tick(); await tick();
  assert.equal(controller.getSnapshot().action, "idle");
  switchResult = () => { throw new Error("The selected workspace is unavailable"); };
  assert.equal(await controller.switchWorkspace("session-1", "team-1"), "failed");
  assert.match(controller.getSnapshot().error, /workspace is unavailable/); assert.equal(controller.getSnapshot().action, "idle");
  await controller.loadSessions();
  const { AccountSettingsSection } = loadTs("components/settings/AccountSettingsSection.tsx", {
    "../../design-system/LoadingSkeleton": loadingViews, "../../account/useChatgptAccount": { useChatgptAccount: () => ({ ...controller.getSnapshot(), switchWorkspace: controller.switchWorkspace }) },
    "../../account/useChatgptUsage": { useChatgptUsage: () => emptyUsage }, "../../account/chatgptUsageModel": usageModel,
    "../../design-system/components": ui, "../account/AccountUsage": usageViews, "../account/AccountLogoutDialog": logoutView, ...accountModels,
    "./SettingsHeading": { SettingsHeading: () => null }, "./settingsCatalog": { settingsTargetProps: () => ({}) },
  });
  const markup = renderToStaticMarkup(React.createElement(AccountSettingsSection));
  assert.match(markup, /Workspaces/); assert.match(markup, /Personal account/); assert.match(markup, /Current/); assert.match(markup, /Switch/);
  assert.match(markup, /Add account/); assert.match(markup, /shared with Codex on this computer/); assert.match(markup, /your own Codex CLI/);
  assert.doesNotMatch(markup, /\/native/);
});

test("native logout refusal, unverified login, cancellation and scope races never become success", async () => {
  const fixture = nativeFixture(), { client } = fixture;
  try {
    fixture.control.refuseLogout = true;
    await assert.rejects(client.logout(), /still reports/);
    fixture.control.refuseLogout = false;
    await client.logout();
    let { loginId } = await client.start();
    fixture.complete(loginId, null);
    await tick();
    assert.equal(client.status(loginId).state, "failed");
    ({ loginId } = await client.start());
    assert.equal((await client.cancel(loginId)).cancelled, true);
    fixture.complete(loginId, null);
    await tick();
    assert.equal(client.status(loginId).state, "failed");
    fixture.externalAccount({ type: "chatgpt", email: "first@example.test", planType: "pro" });
    fixture.control.quotaHook = (child) => { child.cachedAccount = { type: "chatgpt", email: "other@example.test", planType: "plus" }; };
    await assert.rejects(client.rateLimits(), /account changed/);
    fixture.control.quotaHook = (child) => { fixture.externalAccount(child.cachedAccount); };
    await assert.rejects(client.rateLimits(), /account changed/);
    fixture.control.quotaHook = () => fixture.changeHome("/another-native");
    await assert.rejects(client.rateLimits(), /scope changed/);
  } finally { client.dispose(); }
});

test("client clears native signed-out identity and rejects stale reads and stale browser flags", async () => {
  const { createChatgptAccountState } = loadTs("account/chatgptAccountState.ts");
  let account = signedIn, callback, resolveOld, status = { state: "completed", error: "" };
  const bridge = { chatgptAccount: async () => account, chatgptLogout: async () => (account = signedOut),
    chatgptSignInStart: async () => ({ loginId: "id", authUrl: "https://example.test/login" }), chatgptSignInStatus: async () => status,
    chatgptSignInCancel: async () => ({ cancelled: true }), onChatgptAccountChanged: (listener) => { callback = listener; return () => {}; } };
  const controller = createChatgptAccountState(bridge);
  controller.connect(); await controller.refresh();
  bridge.chatgptAccount = () => new Promise((resolve) => { resolveOld = resolve; });
  const oldRead = controller.refresh();
  assert.equal(controller.getSnapshot().account, null, "pending discovery cannot authorize from the previous account");
  await controller.logout(); resolveOld(signedIn); await oldRead;
  assert.equal(controller.getSnapshot().account.signedIn, false);
  assert.equal(controller.getSnapshot().account.email, null);
  bridge.chatgptAccount = async () => account;
  account = { ...signedIn, email: "second@example.test" };
  await controller.signIn(); await tick();
  assert.equal(controller.getSnapshot().account.email, "second@example.test");
  assert.equal(controller.getSnapshot().action, "idle");
  let resolveFirst, resolveLatest;
  bridge.chatgptAccount = () => new Promise((resolve) => { if (!resolveFirst) resolveFirst = resolve; else resolveLatest = resolve; });
  const first = controller.refresh(), latest = controller.refresh();
  await tick(); resolveFirst(signedIn); resolveLatest(account);
  assert.equal((await first).email, "second@example.test"); await latest;
  bridge.chatgptAccount = async () => account;
  account = signedOut; callback(); await tick();
  assert.equal(controller.getSnapshot().account.signedIn, false);
  let statusChecks = 0, starts = 0;
  bridge.chatgptSignInStart = async () => { starts += 1; return { loginId: "same-attempt", authUrl: "https://example.test/same-attempt" }; };
  bridge.chatgptSignInStatus = async (loginId) => { assert.equal(loginId, "same-attempt"); statusChecks += 1; throw new Error("Status IPC unavailable"); };
  await controller.signIn(); await tick(); await controller.refresh();
  assert.equal(controller.getSnapshot().action, "waiting");
  assert.equal(controller.getSnapshot().signInUrl, "https://example.test/same-attempt");
  assert.match(controller.getSnapshot().signInStatusError, /Status IPC unavailable/);
  const accountHook = { useChatgptAccount: () => ({ ...controller.getSnapshot(), retrySignInStatus: controller.retrySignInStatus }) };
  const { ChatGptSignInGate } = loadTs("components/onboarding/ChatGptSignInGate.tsx", {
    "../../account/useChatgptAccount": accountHook, "../OctopusLoadingLogo": { OctopusLoadingState: () => null },
    "../../assets/openai-logo.svg?raw": { default: "" },
  });
  const { AccountSettingsSection } = loadTs("components/settings/AccountSettingsSection.tsx", {
    "../../design-system/LoadingSkeleton": loadingViews,
    "../../account/useChatgptAccount": accountHook,
    "../../account/useChatgptUsage": { useChatgptUsage: () => emptyUsage },
    "../../account/chatgptUsageModel": usageModel,
    "../../design-system/components": ui, "../account/AccountUsage": usageViews,
    "../account/AccountLogoutDialog": logoutView, ...accountModels,
    "./SettingsHeading": { SettingsHeading: () => null }, "./settingsCatalog": { settingsTargetProps: () => ({}) },
  });
  assert.match(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Retry sign-in check/);
  assert.match(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Cancel sign-in/);
  assert.match(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Copy sign-in link/);
  assert.doesNotMatch(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Waiting for browser/);
  assert.doesNotMatch(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Shared sign-in|Account and Home|\/native/);
  await new Promise((resolve) => setTimeout(resolve, 1050));
  assert.equal(statusChecks, 1, "a failed status check waits for explicit retry");
  bridge.chatgptSignInStatus = async (loginId) => { assert.equal(loginId, "same-attempt"); statusChecks += 1; return { state: "completed", error: "" }; };
  account = signedIn; await controller.refresh();
  const settingsMarkup = renderToStaticMarkup(React.createElement(AccountSettingsSection));
  assert.match(settingsMarkup, /Retry sign-in check/);
  assert.doesNotMatch(settingsMarkup, /Account and Home|Native credential location|Signed in with ChatGPT|\/native/);
  await Promise.all([controller.retrySignInStatus(), controller.retrySignInStatus()]);
  assert.equal(statusChecks, 2); assert.equal(starts, 1);
  assert.equal(controller.getSnapshot().action, "idle");
  assert.equal(controller.getSnapshot().signInStatusError, "");
  assert.equal(controller.getSnapshot().signInUrl, "");
  assert.equal(controller.getSnapshot().account.signedIn, true);
  account = signedOut; await controller.refresh();
  bridge.chatgptSignInStatus = async () => status;
  status = { state: "failed", error: "Browser sign-in refused" };
  await controller.signIn(); await tick();
  assert.match(controller.getSnapshot().error, /refused/);
  bridge.chatgptAccount = async () => { throw new Error("Native unavailable"); };
  await controller.refresh();
  assert.equal(controller.getSnapshot().account, null);
  assert.match(controller.getSnapshot().error, /Native unavailable/);
  const unavailableMarkup = renderToStaticMarkup(React.createElement(AccountSettingsSection));
  assert.match(unavailableMarkup, /Unavailable/); assert.doesNotMatch(unavailableMarkup, /Signed out/);
  assert.match(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /ChatGPT account unavailable/);
  bridge.chatgptAccount = async () => signedOut; await controller.refresh();
  assert.match(renderToStaticMarkup(React.createElement(AccountSettingsSection)), /Signed out/);
  assert.match(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /Sign in to ChatGPT/);
  assert.doesNotMatch(renderToStaticMarkup(React.createElement(ChatGptSignInGate)), /ChatGPT account unavailable/);
  const onboarding = loadTs("onboarding/chatgptSignIn.ts");
  assert.equal(onboarding.chatgptSignInCompleted(), false);
  onboarding.recordChatgptSignIn(); assert.equal(onboarding.chatgptSignInCompleted(), true);
  onboarding.recordChatgptSignIn(false); assert.equal(onboarding.chatgptSignInCompleted(), false);
});

test("usage preserves multiple buckets, unknown windows, zero remaining and reset seconds", () => {
  const { accountUsageGroups, accountResetTime, accountRemainingLabel } = usageModel;
  const usage = { rateLimits: limit, rateLimitsByLimitId: { core: limit, extra: { ...limit, primary: { usedPercent: 100, windowDurationMins: 10080, resetsAt: null } } }, observedAtMs: 1 };
  const groups = accountUsageGroups(usage);
  assert.equal(groups.length, 2); assert.equal(groups[0].windows[0].remaining, 75);
  assert.equal(groups[0].windows[1].remaining, null); assert.equal(groups[1].windows[0].remaining, 0);
  assert.equal(groups[1].windows[0].label, "Weekly usage limit");
  assert.equal(groups[0].windows[0].label, "5 hour usage limit");
  assert.equal(groups[0].windows[1].reported, false);
  assert.equal(accountRemainingLabel(75.3), "75% left");
  assert.equal(accountRemainingLabel(null), "Unavailable");
  assert.equal(usageModel.accountPlanLabel("promax"), "Pro (Max)");
  assert.equal(accountResetTime(null), "Reset time unavailable");
  const date = new Date(2000000000000), now = date.getTime() - 3_600_000;
  assert.equal(accountResetTime(2000000000, 300, now), `Resets ${new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(date)}`);
  assert.equal(accountResetTime(2000000000, 10080, now), `Resets ${new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date)}`);
  assert.equal(accountUsageGroups({ ...usage, rateLimitsByLimitId: {} }).length, 0);
  assert.equal(accountUsageGroups({ ...usage, rateLimitsByLimitId: null }).length, 1);
});

test("usage projection fences account changes, clears unavailable values and retries only on request", async () => {
  const { createChatgptUsageState } = loadTs("account/chatgptUsageState.ts");
  const value = { rateLimits: limit, rateLimitsByLimitId: { core: limit }, observedAtMs: 1 };
  let calls = 0, resolveOld;
  const bridge = { chatgptRateLimits: () => { calls += 1; return new Promise((resolve) => { resolveOld = resolve; }); } };
  const controller = createChatgptUsageState(bridge);
  const first = controller.observe(signedIn); await tick();
  await controller.observe(signedOut); resolveOld(value); await first;
  assert.equal(controller.getSnapshot().value, null);
  bridge.chatgptRateLimits = async () => { calls += 1; throw new Error("Usage unavailable"); };
  await controller.observe(signedIn);
  assert.equal(controller.getSnapshot().value, null); assert.match(controller.getSnapshot().error, /Usage unavailable/);
  const count = calls; await controller.observe(signedIn); await tick(); assert.equal(calls, count);
  bridge.chatgptRateLimits = async () => { calls += 1; return value; };
  await Promise.all([controller.refresh(), controller.refresh()]); assert.equal(calls, count + 1);
  const full = renderToStaticMarkup(React.createElement(usageViews.AccountUsageSections, { state: controller.getSnapshot(), signedIn: true }));
  assert.match(full, /5 hour usage limit/); assert.match(full, /value="75"/); assert.match(full, /75% left/);
  assert.doesNotMatch(full, /Secondary limit/);
  const unavailable = renderToStaticMarkup(React.createElement(usageViews.AccountRemainingProgress, { remaining: null, label: "Unknown" }));
  assert.match(unavailable, /Unavailable/); assert.doesNotMatch(unavailable, /<progress\b/);
  const exhausted = renderToStaticMarkup(React.createElement(usageViews.AccountRemainingProgress, { remaining: 0, label: "Exhausted" }));
  assert.match(exhausted, /value="0"/); assert.match(exhausted, /0% left/);
  await controller.observe(null); assert.equal(controller.getSnapshot().value, null);
});

test("shared logout confirmation never calls auth on open or cancel and binds the confirmed native account", () => {
  const { AccountLogoutDialog } = loadTs("components/account/AccountLogoutDialog.tsx", {
    react: { ...React, useId: () => "logout-title", useRef: () => ({ current: null }) }, "../../design-system/components": ui,
  });
  let confirmed = 0, closed = 0;
  const props = { account: signedIn, open: true, busy: false, onClose: () => { closed += 1; }, onConfirm: () => { confirmed += 1; } };
  const view = AccountLogoutDialog(props);
  assert.equal(view.props.size, "narrow"); assert.equal(confirmed, 0);
  const markup = renderToStaticMarkup(view);
  assert.match(markup, /Log out\?/); assert.match(markup, /first@example.test/); assert.match(markup, /\/native/);
  assert.match(markup, /Administrator access are kept/);
  view.props.onClose(); assert.equal(closed, 1); assert.equal(confirmed, 0);
  const submit = { preventDefault() {} };
  AccountLogoutDialog({ ...props, busy: true }).props.children.props.onSubmit(submit); assert.equal(confirmed, 0);
  AccountLogoutDialog({ ...props, account: signedOut }).props.children.props.onSubmit(submit); assert.equal(confirmed, 0);
  view.props.children.props.onSubmit(submit); assert.equal(confirmed, 1); assert.equal(closed, 2);
});

test("sign-in copy completion belongs to the current URL and mounted attempt", async () => {
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM('<div id="root"></div>', { url: "https://client.test" });
  const globals = { window: dom.window, document: dom.window.document, navigator: dom.window.navigator,
    HTMLElement: dom.window.HTMLElement, Node: dom.window.Node, IS_REACT_ACT_ENVIRONMENT: true };
  const previous = new Map(Object.keys(globals).map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  for (const [key, value] of Object.entries(globals)) Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  const pending = [], copiedUrls = [];
  Object.defineProperty(navigator, "clipboard", { value: { writeText: (url) => {
    copiedUrls.push(url);
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  } } });
  let state = { account: signedOut, loading: false, error: "", signInStatusError: "", action: "waiting", signInUrl: "", cancel: async () => {} };
  const { ChatGptSignInGate } = loadTs("components/onboarding/ChatGptSignInGate.tsx", {
    "../../account/useChatgptAccount": { useChatgptAccount: () => state },
    "../OctopusLoadingLogo": { OctopusLoadingState: () => null }, "../../assets/openai-logo.svg?raw": { default: "" },
  });
  const { createRoot } = require("react-dom/client"), root = createRoot(document.getElementById("root"));
  let mounted = true;
  const render = async (url) => {
    state = { ...state, signInUrl: url, action: url ? "waiting" : "idle" };
    await React.act(async () => root.render(React.createElement(ChatGptSignInGate)));
  };
  const copy = async () => React.act(async () => document.querySelector(".chatgpt-signin-copy").dispatchEvent(new window.MouseEvent("click", { bubbles: true })));
  const settle = async (callback) => React.act(async () => { callback(); await tick(); });
  const assertUnchanged = () => {
    assert.equal(document.querySelector(".chatgpt-signin-copy").textContent, "Copy sign-in link");
    assert.ok(document.querySelector('[role="alert"]') === null);
  };
  try {
    await render("https://example.test/first"); await copy();
    await render(""); await render("https://example.test/second");
    await settle(() => pending[0].resolve()); assertUnchanged();
    await copy(); await render(""); await render("https://example.test/first");
    await settle(() => pending[1].reject(new Error("Old clipboard failed"))); assertUnchanged();
    await copy(); await render(""); await render("https://example.test/first");
    await settle(() => pending[2].resolve()); assertUnchanged();
    await copy(); await settle(() => pending[3].reject(new Error("Current clipboard failed")));
    assert.match(document.querySelector('[role="alert"]').textContent, /Current clipboard failed/);
    await copy(); await settle(() => pending[4].resolve());
    assert.equal(document.querySelector(".chatgpt-signin-copy").textContent, "Copied");
    assert.ok(document.querySelector('[role="alert"]') === null);
    await copy(); await React.act(async () => root.unmount()); mounted = false;
    await settle(() => pending[5].reject(new Error("Unmounted clipboard failed")));
    assert.equal(document.getElementById("root").childElementCount, 0);
    assert.deepEqual(copiedUrls, ["https://example.test/first", "https://example.test/second", ...Array(4).fill("https://example.test/first")]);
  } finally {
    if (mounted) await React.act(async () => root.unmount());
    dom.window.close();
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});

test("reopened setup displays committed progress before workspace discovery and follows the running operation", async () => {
  const dom = new (require("jsdom").JSDOM)('<div id="root"></div>', { url: "https://client.test" });
  const previous = new Map();
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  }
  const setup = { status: "in_progress", operationId: "setup_progress_001", steps: ["packages", "extensions", "workspace", "provider"].map((step, i) => ({ step, state: i < 2 ? "complete" : "pending" })) };
  let releaseWorkspace, notify, resumeCalls = 0;
  let workspace = new Promise(resolve => { releaseWorkspace = resolve; });
  const progressView = ({ applyState }) => React.createElement("div", { "data-progress": applyState.percent }, applyState.label);
  const { WorkspaceSetupGate } = loadTs("components/onboarding/WorkspaceSetupGate.tsx", {
    "../../account/useChatgptAccount": { useChatgptAccount: () => ({ account: null, loading: true }) },
    "./ChatGptSignInGate": { ChatGptSignInGate: () => "Sign in" },
    "../chat/chatWorkspaceCapabilities": {}, "../../onboarding/chatgptSignIn": { chatgptSignInCompleted: () => false },
    "../../extensions/useClientExtensions": { useClientExtensions: () => ({ loading: true, extensions: [], errors: [] }) },
    "../../onboarding/workspaceSetup": { readWorkspaceSetupDraft: () => null },
    "../../projectionClient": {
      readCurrentClientSetup: async () => setup,
      readWorkspaceSetupState: () => workspace,
      readProjection: async method => method.endsWith("local_packages") ? { packs: [], packages: [], errors: [] }
        : { status: "unbound", root_ee_id: "", home_run_id: "", home_interface_id: "", root_environment: null, projects: [], errors: [] },
      onClientSetupProgress: listener => { notify = listener; return () => { notify = null; }; },
      resumeClientSetup: () => { resumeCalls++; return new Promise(() => {}); },
    },
    "../../projectionLive": { useProjectionStructureGeneration: () => 0 }, "../../design-system/components": ui,
    "../../toasts": { appToast: { danger: () => ({ close() {} }) } },
    "../OctopusLoadingLogo": { OctopusLoadingState: () => "Opening Octopus", OctopusLoadingLogo: () => null },
    "./WorkspaceSetupOverlay": { WorkspaceSetupProgress: progressView, WorkspaceSetupOverlay: progressView, createDefaultWorkspaceSetupDraft: () => ({}) },
  });
  const root = require("react-dom/client").createRoot(document.getElementById("root"));
  const act = callback => React.act(async () => { await callback(); await tick(); });
  const show = () => act(() => root.render(React.createElement(WorkspaceSetupGate, { accessProfile: "live", runtimeHome: "/home" }, "Shell")));
  try {
    await show();
    assert.equal(document.querySelector("[data-progress]").dataset.progress, "45");
    assert.match(document.body.textContent, /Checking Home and projects/);
    assert.doesNotMatch(document.body.textContent, /Opening Octopus|Sign in|Shell/);
    workspace = { status: "not_configured", errors: [], receipt: null };
    await act(() => releaseWorkspace(workspace));
    assert.equal(resumeCalls, 1);
    assert.match(document.body.textContent, /Checking Home and projects/);
    await act(() => notify({ operationId: setup.operationId, step: "provider", phase: "verifying", percent: 68 }));
    assert.match(document.body.textContent, /Checking provider sign-in/);
    await act(() => notify({ operationId: "other_operation", step: "packages", phase: "applying", percent: 0 }));
    assert.match(document.body.textContent, /Checking provider sign-in/);
    await act(() => root.render(null));
    setup.steps[2].state = "complete";
    workspace = new Promise(() => {});
    await show();
    assert.equal(document.querySelector("[data-progress]").dataset.progress, "68");
    assert.match(document.body.textContent, /Checking provider sign-in/);
  } finally {
    await act(() => root.unmount()); dom.window.close();
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
});

test("account menu prefers top placement and retains viewport collision handling", () => {
  let account = { ...signedIn, planType: "promax" };
  const { AccountRailButton } = loadTs("components/shell/AccountRailButton.tsx", {
    react: { ...React, useState: () => [true, () => {}], useLayoutEffect: () => {} },
    "../../design-system/components": ui, "../../design-system/OcDropdown": { OcDropdown: ({ children }) => React.createElement("div", null, children) },
    "../../keyboardShortcuts": { useKeyboardShortcuts: () => ({}), formatKeyboardShortcuts: () => "" },
    "../../account/useChatgptAccount": { useChatgptAccount: () => ({ account, loading: false, action: "idle" }) },
    "../../account/useChatgptUsage": { useChatgptUsage: () => emptyUsage }, "../../account/chatgptUsageModel": usageModel,
        "../chat/MessageTooltip": { MessageTooltip: ({ children }) => children }, ...accountModels,
  });
  const menu = () => renderToStaticMarkup(React.createElement(AccountRailButton, { accessProfile: "live", runtimeHost: {}, onLogOut() {}, onOpenSettings() {} }));
  assert.match(menu(), /Account: Administrator/); assert.match(menu(), /<small>ChatGPT Pro \(Max\)<\/small>/);
  assert.doesNotMatch(menu(), /first@example.test/); assert.ok(menu().indexOf("Settings") < menu().indexOf("Usage"));
  assert.doesNotMatch(menu(), /Log out of ChatGPT|Usage remaining|Using ChatGPT/); assert.match(menu(), /Add account/);
  account = null; assert.match(menu(), /Administrator/); assert.match(menu(), /ChatGPT unavailable/);
  const { dropdownPlacement } = loadTs("design-system/OcDropdown.tsx", { "./components": ui, "./modalFocus": {} }, "\nexport { dropdownPlacement };\n");
  const anchor = { left: 24, right: 64, top: 350, bottom: 390 };
  const top = dropdownPlacement(anchor, 240, 120, 1000, 800, "start", 6, 0, "top");
  assert.equal(top.side, "top"); assert.equal(top.left, 24); assert.equal(top.top, 224);
  const bottom = dropdownPlacement(anchor, 240, 120, 1000, 800, "start", 6);
  assert.equal(bottom.side, "bottom"); assert.equal(bottom.top, 396);
  const crowded = dropdownPlacement({ ...anchor, top: 60, bottom: 100 }, 240, 200, 1000, 800, "start", 6, 0, "top");
  assert.equal(crowded.side, "bottom"); assert.equal(crowded.top, 106);
  const right = dropdownPlacement(anchor, 240, 120, 1000, 800, "start", 6, 0, "right");
  assert.equal(right.side, "right"); assert.equal(right.left, 70); assert.equal(right.top, 350);
});

test("local expiry returns to unlock before Home verification and ignores stale access reads", async () => {
  const dom = new (require("jsdom").JSDOM)('<div id="root"></div>', { url: "https://client.test" });
  const previous = new Map();
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  }
  const locked = { configured: true, authenticated: false, profile: null, expiresAt: null };
  const live = { ...locked, authenticated: true, profile: "live", expiresAt: new Date(Date.now() + 60_000).toISOString() };
  let status = locked, read = async () => status, access, expiry, homeChecks = 0;
  window.setTimeout = (callback) => { expiry = callback; return 1; }; window.clearTimeout = () => { expiry = null; };
  const { useClientAccess } = loadTs("useClientAccess.ts", { "./clientAccess": { readClientAccessStatus: () => read() } });
  const { App } = loadTs("App.tsx", {
    "./useClientAccess": { useClientAccess: () => (access = useClientAccess()) },
    "./clientAccess": { readTouchIdStatus: async () => ({ available: false }) },
    "./onboarding/onboardingStorage": { recordOnboardingVisit: () => ({ completed: true }) },
    "./runtimeHost": { useRuntimeHost: () => ({ ready: true, available: true, phase: "ready" }) },
    "./clientPreferences": { ClientPreferencesProvider: ({ children }) => children },
    "./components/onboarding/WorkspaceSetupGate": { WorkspaceSetupGate: ({ children }) => { homeChecks += 1; return children; } },
    $fallback: (name) => name.startsWith(".") ? new Proxy({}, { get: (_, key) => () => String(key) }) : undefined,
  });
  const root = require("react-dom/client").createRoot(document.getElementById("root"));
  const act = (callback) => React.act(async () => { callback(); await tick(); });
  try {
    await act(() => root.render(React.createElement(App)));
    assert.match(document.body.textContent, /PasswordGate/); assert.equal(homeChecks, 0);
    status = live; await act(() => access.setAccess(live));
    assert.ok(homeChecks > 0);
    assert.match(document.body.textContent, /OperatorShell/);
    const previousChecks = homeChecks;
    status = locked; await act(() => expiry());
    assert.match(document.body.textContent, /PasswordGate/); assert.equal(homeChecks, previousChecks);
    status = live; await act(() => access.setAccess(live));
    let finish; read = () => new Promise(resolve => { finish = resolve; });
    await act(() => window.dispatchEvent(new window.Event("focus")));
    await act(() => access.setAccess({ ...live })); await act(() => finish(locked));
    assert.match(document.body.textContent, /OperatorShell/);
    read = async () => locked; await act(() => window.dispatchEvent(new window.Event("focus")));
    assert.match(document.body.textContent, /PasswordGate/);
    status = { ...locked, configured: false }; read = async () => status;
    await act(() => access.setAccess(status)); assert.match(document.body.textContent, /PasswordGate/);
  } finally {
    await act(() => root.unmount()); dom.window.close();
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key];
    }
  }
});

test("completed Home uses checked local receipts offline and cancels proof across Home targets", { timeout: 10000 }, async () => {
  const fsp = require("node:fs/promises"), crypto = require("node:crypto");
  const temporary = await fsp.mkdtemp(path.join(require("node:os").tmpdir(), "octopus-home-gate-"));
  const stable = value => value && typeof value === "object" && !Array.isArray(value)
    ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${stable(value[key])}`).join(",")}}`
    : Array.isArray(value) ? `[${value.map(stable).join(",")}]` : JSON.stringify(value);
  const digest = value => crypto.createHash("sha256").update(value).digest("hex");
  const operationId = "workspace_gate_fixture", requestDigest = digest("workspace-request");
  const { createClientSetupStore } = require("../electron/clientSetup.cjs");
  const owners = Object.fromEntries(Object.entries({ packages: "package_install_evidence", extensions: "extension_lifecycle_receipt",
    workspace: "workspace_receipt", provider: "provider_observation" }).map(([step, kind]) => [step, {
    probe: async () => [{ kind, id: `${step}.proof`, evidence: { owner: step }, digest: digest(stable({ owner: step })) }],
    apply: async () => { throw new Error("fixture proof must already exist"); },
  }]));
  const store = createClientSetupStore({ root: temporary, owners });
  await store.begin({ operationId, plan: {
    pack: { id: "codex", manifest_digest: digest("pack"), selected_recommended: [] },
    packages: [{ id: "world_provider_codex", source_digest: digest("package"), desired_state: "installed" }], extensions: [],
    provider: { interface_id: "world_provider_codex", required_state: "account_readable" },
    workspace: { request_digest: requestDigest, request: { action: "initialize_workspace", operationId }, project_defaults: [] },
  } });
  await store.resume(operationId);
  const receiptFile = path.join(temporary, "receipts", `${operationId}.json`), receiptBytes = await fsp.readFile(receiptFile);
  const dom = new (require("jsdom").JSDOM)('<div id="root"></div>', { url: "https://client.test" });
  const previous = new Map();
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  }
  let releaseInitialAccount;
  let home = "/home", accountResult = new Promise(resolve => { releaseInitialAccount = resolve; }), accountChanged;
  let currentSetup = () => store.readCurrent(), configured = true, scopeHome = "home_run", catalogReads = 0, setupWrites = 0, workspaceError = "";
  window.octopusTransport = {
    chatgptAccount: async () => { const result = await accountResult; if (result instanceof Error) throw result; return result; },
    onChatgptAccountChanged: callback => { accountChanged = callback; return () => { accountChanged = null; }; },
  };
  const onboarding = loadTs("onboarding/chatgptSignIn.ts");
  const accountHook = loadTs("account/useChatgptAccount.ts", {
    "./chatgptAccountState": loadTs("account/chatgptAccountState.ts"), "../onboarding/chatgptSignIn": onboarding,
  });
  const logo = { OctopusLoadingState: ({ label }) => label, OctopusLoadingLogo: () => null };
  const { ChatGptSignInGate } = loadTs("components/onboarding/ChatGptSignInGate.tsx", {
    "../../account/useChatgptAccount": accountHook, "../OctopusLoadingLogo": logo, "../../assets/openai-logo.svg?raw": { default: "" },
  });
  const { AccountSettingsSection } = loadTs("components/settings/AccountSettingsSection.tsx", {
    "../../design-system/LoadingSkeleton": loadingViews,
    "../../account/useChatgptAccount": accountHook, "../../account/useChatgptUsage": { useChatgptUsage: () => emptyUsage },
    "../../account/chatgptUsageModel": usageModel, "../../design-system/components": ui, "../account/AccountUsage": usageViews,
    "../account/AccountLogoutDialog": logoutView, "./SettingsHeading": { SettingsHeading: () => null }, "./settingsCatalog": { settingsTargetProps: () => ({}) },
    ...accountModels,
  });
  const { WorkspaceSetupGate } = loadTs("components/onboarding/WorkspaceSetupGate.tsx", {
    "../../account/useChatgptAccount": accountHook, "./ChatGptSignInGate": { ChatGptSignInGate },
    "../chat/chatWorkspaceCapabilities": {}, "../../onboarding/chatgptSignIn": onboarding,
    "../../extensions/useClientExtensions": { useClientExtensions: () => ({ loading: true, extensions: [], errors: [] }) },
    "../../onboarding/workspaceSetup": { readWorkspaceSetupDraft: () => null },
    "../../projectionClient": {
      readCurrentClientSetup: () => currentSetup(),
      readWorkspaceSetupState: async () => ({ status: workspaceError ? "degraded" : configured ? "configured" : "not_configured", errors: workspaceError ? [workspaceError] : [], receipt: !configured ? null : {
        operationId, requestDigest, rootEeId: "root_ee", homeRunId: "home_run", homeInterfaceId: "home_interface", projects: [],
      } }),
      readProjection: async method => {
        if (method === "projection.operator.os.local_packages") { catalogReads++; return { packs: [], packages: [], errors: [] }; }
        assert.equal(method, "projection.operator.os.scope");
        return { status: configured ? "ready" : "unbound", root_ee_id: configured ? "root_ee" : "",
          home_run_id: configured ? scopeHome : "", home_interface_id: configured ? "home_interface" : "", root_environment: null, projects: [], errors: [] };
      },
      onClientSetupProgress: () => () => {},
      beginClientSetup: async () => { setupWrites++; throw new Error("unexpected setup write"); },
      resumeClientSetup: async () => { setupWrites++; throw new Error("unexpected setup resume"); },
    },
    "../../projectionLive": { useProjectionStructureGeneration: () => 0 }, "../../design-system/components": ui,
    "../../toasts": { appToast: { danger: () => ({ close() {} }) } },
    "../OctopusLoadingLogo": logo, "./WorkspaceSetupOverlay": { WorkspaceSetupOverlay: () => "Review setup", createDefaultWorkspaceSetupDraft: () => ({}) },
  });
  const { App } = loadTs("App.tsx", {
    "./useClientAccess": { useClientAccess: () => ({ access: { configured: true, authenticated: true, profile: "live" } }) },
    "./runtimeHost": { useRuntimeHost: () => ({ ready: true, available: true, phase: "ready", home }) },
    "./onboarding/onboardingStorage": { recordOnboardingVisit: () => ({ completed: true }) },
    "./clientPreferences": { ClientPreferencesProvider: ({ children }) => children },
    "./components/onboarding/WorkspaceSetupGate": { WorkspaceSetupGate },
    "./components/shell/OperatorShell": { OperatorShell: () => React.createElement("section", { "data-shell": "" }, "Local workspace", React.createElement(AccountSettingsSection)) },
    $fallback: name => name.startsWith(".") ? new Proxy({}, { get: () => () => null }) : undefined,
  });
  const root = require("react-dom/client").createRoot(document.getElementById("root"));
  const act = async callback => React.act(async () => { await callback(); await tick(); });
  const show = () => act(() => root.render(React.createElement(App)));
  const waitFor = async predicate => { for (let i = 0; i < 100 && !predicate(); i++) await act(() => new Promise(resolve => setTimeout(resolve, 5))); assert.ok(predicate(), document.body.textContent); };
  const shell = () => document.querySelector("[data-shell]");
  try {
    await show(); await waitFor(shell);
    assert.equal(catalogReads, 0);
    await act(() => releaseInitialAccount(new Error("workspace routing discovery timed out")));
    await waitFor(() => shell() && document.body.textContent.includes("Unavailable"));
    assert.match(document.body.textContent, /workspace routing discovery timed out/);
    assert.equal(catalogReads, 0); assert.equal(onboarding.chatgptSignInCompleted(), false);
    accountResult = signedOut; await act(() => accountChanged());
    assert.ok(shell()); assert.match(document.body.textContent, /Signed out/);
    let releaseOldAccount;
    accountResult = new Promise(resolve => { releaseOldAccount = resolve; }); await act(() => accountChanged());
    home = "/second-complete"; accountResult = new Error("second Home account unavailable"); await show();
    await waitFor(() => shell() && document.body.textContent.includes("second Home account unavailable"));
    await act(() => releaseOldAccount(signedIn));
    assert.equal(onboarding.chatgptSignInCompleted(), false);
    assert.match(document.body.textContent, /second Home account unavailable/); assert.equal(catalogReads, 0);
    const invalid = JSON.parse(receiptBytes); invalid.plan_digest = "0".repeat(64);
    await fsp.writeFile(receiptFile, JSON.stringify(invalid)); home = "/corrupt"; await show();
    await waitFor(() => document.body.textContent.includes("plan digest is invalid")); assert.equal(shell(), null);
    await fsp.writeFile(receiptFile, receiptBytes); scopeHome = "another_run"; home = "/mismatch"; await show();
    await waitFor(() => document.body.textContent.includes("Completed setup does not match Home")); assert.equal(shell(), null);
    scopeHome = "home_run";
    workspaceError = "Workspace Home run identity is no longer valid"; home = "/degraded"; await show();
    await waitFor(() => document.body.textContent.includes(workspaceError)); assert.equal(shell(), null);
    assert.doesNotMatch(document.body.textContent, /Completed setup does not match Home/);
    workspaceError = "";
    let releaseOld, releaseNew;
    currentSetup = () => new Promise(resolve => { releaseOld = resolve; }); home = "/old-pending"; await show();
    assert.equal(shell(), null);
    currentSetup = () => new Promise(resolve => { releaseNew = resolve; }); home = "/new-pending"; configured = false; await show();
    await act(async () => releaseOld(await store.readCurrent())); assert.equal(shell(), null);
    accountResult = new Error("workspace routing discovery timed out"); currentSetup = async () => null;
    await act(() => releaseNew(null)); await waitFor(() => document.body.textContent.includes("ChatGPT account unavailable"));
    assert.equal(catalogReads, 0); assert.equal(shell(), null);
    accountResult = { ...signedOut, octopusHome: home }; await act(() => accountChanged());
    assert.match(document.body.textContent, /Sign in to ChatGPT/); assert.doesNotMatch(document.body.textContent, /ChatGPT account unavailable/);
    accountResult = signedIn; await act(() => accountChanged());
    assert.match(document.body.textContent, /observation belongs to another Home/); assert.equal(catalogReads, 0);
    accountResult = { ...signedIn, octopusHome: home }; await act(() => accountChanged());
    await waitFor(() => document.body.textContent.includes("Review setup")); assert.equal(shell(), null); assert.equal(catalogReads, 1);
    configured = true; home = "/missing-aggregate"; accountResult = { ...signedIn, octopusHome: home }; await show();
    await waitFor(() => document.body.textContent.includes("Workspace verification is incomplete")); assert.equal(shell(), null);
    assert.equal(setupWrites, 0);
  } finally {
    await act(() => root.unmount()); dom.window.close(); await fsp.rm(temporary, { recursive: true, force: true });
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
});
