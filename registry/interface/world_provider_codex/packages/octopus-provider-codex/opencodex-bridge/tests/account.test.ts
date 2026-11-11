import { afterAll, afterEach, beforeAll, expect, mock, spyOn, test } from "bun:test";
import { chmod, link, mkdtemp, readFile, readdir, rm, stat, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { accountStatus, createOAuthAccountBoundary, type OAuthAccountConfig, type OAuthAccountDependencies } from "../src/account.ts";

const roots: string[] = [];
let network: ReturnType<typeof spyOn>;
beforeAll(() => { network = spyOn(globalThis, "fetch").mockImplementation(() => { throw new Error("Real network is forbidden"); }); });
afterAll(() => { expect(network).not.toHaveBeenCalled(); network.mockRestore(); });
afterEach(async () => { await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true }))); });

const fresh = () => ({ access: "access-fixture", refresh: "refresh-fixture", expires: Date.now() + 3_600_000, projectId: "project-fixture", accountId: "unverified-jwt-id", email: "unverified@example.test" });
const userinfo = () => Response.json({ id: "google-subject", email: "Owner@Example.Test" });
const onAuth = () => {};
function gate() { let resolve!: () => void; const promise = new Promise<void>((done) => { resolve = done; }); return { promise, resolve }; }

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "octopus-oauth-test-"));
  roots.push(root);
  const config: OAuthAccountConfig = { provider: "google-antigravity", directory: join(root, "opencodex-account") };
  const dependencies: OAuthAccountDependencies = {
    login: mock(async (controller, options) => {
      expect(options).toEqual({ forceAccountSelect: true });
      controller.onAuth?.({ url: "https://accounts.google.com/o/oauth2/v2/auth?state=private-fixture" });
      controller.onProgress?.("Waiting for browser authentication...");
      return fresh();
    }),
    refresh: mock(async () => fresh()),
    discoverProject: mock(async () => "discovered-project"),
    fetch: mock(async (url, init) => {
      expect(url).toBe("https://www.googleapis.com/oauth2/v2/userinfo");
      expect(init.redirect).toBe("error");
      expect(init.method).toBe("GET");
      expect(new Headers(init.headers).get("authorization")).toBe("Bearer access-fixture");
      expect(init.signal).toBeInstanceOf(AbortSignal);
      return userinfo();
    }),
  };
  const api = createOAuthAccountBoundary(dependencies);
  const file = join(config.directory, "credential.json");
  const saved = async () => JSON.parse(await readFile(file, "utf8"));
  const expire = async () => { const value = { ...await saved(), expiresAt: 1 }; await writeFile(file, JSON.stringify(value)); return readFile(file, "utf8"); };
  return { root, config, dependencies, api, file, saved, expire };
}

test("explicit login owns one private store and verifies Google identity", async () => {
  const f = await fixture();
  expect(await accountStatus(f.config)).toEqual({ signedIn: false, accountLabel: null });
  expect(await readdir(f.root)).toEqual([]);
  const auth = mock(onAuth), progress = mock(onAuth);
  expect(await f.api.loginOAuthAccount(f.config, { onAuth: auth, onProgress: progress })).toEqual({ signedIn: true, accountLabel: "owner@example.test" });
  const first = await f.api.resolveOAuthAccount(f.config);
  expect(first).toEqual({ accessToken: "access-fixture", projectId: "project-fixture", accountId: "google-subject" });
  expect(auth).toHaveBeenCalledTimes(1);
  expect(progress).toHaveBeenCalledTimes(1);
  expect((await stat(f.config.directory)).mode & 0o777).toBe(0o700);
  expect((await stat(f.file)).mode & 0o777).toBe(0o600);
  expect(await readdir(f.config.directory)).toEqual(["credential.json"]);
  expect(await readFile(f.file, "utf8")).not.toMatch(/private-fixture|unverified|https:/);
  await f.api.loginOAuthAccount(f.config, { onAuth });
  expect(await f.api.resolveOAuthAccount(f.config)).toEqual(first);
  expect(f.dependencies.refresh).not.toHaveBeenCalled();
  await writeFile(join(f.config.directory, "unrelated"), "keep");
  expect(await f.api.logoutOAuthAccount(f.config)).toEqual({ signedIn: false, accountLabel: null });
  expect(await readdir(f.config.directory)).toEqual(["unrelated"]);
  expect(await f.api.logoutOAuthAccount(f.config)).toEqual({ signedIn: false, accountLabel: null });
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "not_signed_in" });
});

test("concurrent resolves refresh once with the same identity", async () => {
  const f = await fixture();
  await f.api.loginOAuthAccount(f.config, { onAuth });
  const initial = await f.saved();
  await f.expire();
  f.dependencies.refresh = mock(async (token, signal) => {
    expect(token).toBe("refresh-fixture");
    expect(signal?.aborted).toBe(false);
    await Bun.sleep(50);
    return { ...fresh(), refresh: "rotated-refresh", projectId: undefined };
  });
  const second = createOAuthAccountBoundary(f.dependencies);
  const results = await Promise.all(Array.from({ length: 12 }, (_, i) => (i % 2 ? second : f.api).resolveOAuthAccount(f.config)));
  expect(f.dependencies.refresh).toHaveBeenCalledTimes(1);
  expect(f.dependencies.discoverProject).toHaveBeenCalledTimes(1);
  expect(results.every((value) => value.accountId === initial.accountId && value.projectId === "discovered-project")).toBe(true);
  expect((await f.saved()).refreshToken).toBe("rotated-refresh");
});

test("refresh failures, account changes, missing projects and outside writes never replace the credential", async () => {
  const f = await fixture();
  await f.api.loginOAuthAccount(f.config, { onAuth });
  const previous = await f.expire();
  f.dependencies.refresh = async () => { throw new Error("secret-token https://accounts.google.com/auth?secret=1"); };
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "refresh_failed", message: "Antigravity token refresh failed; sign in again" });
  f.dependencies.refresh = async () => fresh();
  f.dependencies.fetch = async () => Response.json({ id: "other-account", email: "other@example.test" });
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "account_mismatch" });
  f.dependencies.fetch = async () => Response.json({ email: "missing-id@example.test" });
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "identity_failed" });
  f.dependencies.fetch = async () => userinfo();
  f.dependencies.refresh = async () => ({ ...fresh(), projectId: undefined });
  f.dependencies.discoverProject = async () => undefined;
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "project_unavailable" });
  expect(await readFile(f.file, "utf8")).toBe(previous);
  const changed = JSON.stringify({ ...JSON.parse(previous), accountId: "external-account" });
  f.dependencies.refresh = async () => { await writeFile(f.file, changed); return fresh(); };
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "store_changed" });
  expect(await readFile(f.file, "utf8")).toBe(changed);
  expect(await readdir(f.config.directory)).toEqual(["credential.json"]);
});

test("malformed stores and unsafe paths fail closed without repair or network", async () => {
  const f = await fixture();
  await f.api.loginOAuthAccount(f.config, { onAuth });
  const valid = await f.saved();
  const values = ["{broken", "null", "[]", JSON.stringify({ ...valid, provider: "other" }), JSON.stringify({ ...valid, accounts: [] }), JSON.stringify({ ...valid, refreshToken: "" })];
  f.dependencies.login = mock(async () => { throw new Error("Login must not run"); });
  for (const value of values) {
    await writeFile(f.file, value);
    await expect(f.api.accountStatus(f.config)).rejects.toMatchObject({ code: "invalid_store" });
    await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "invalid_store" });
    await expect(f.api.loginOAuthAccount(f.config, { onAuth })).rejects.toMatchObject({ code: "invalid_store" });
    await expect(f.api.logoutOAuthAccount(f.config)).rejects.toMatchObject({ code: "invalid_store" });
    expect(await readFile(f.file, "utf8")).toBe(value);
  }
  await writeFile(f.file, Buffer.from([0xff]));
  await expect(f.api.accountStatus(f.config)).rejects.toMatchObject({ code: "invalid_store" });
  await writeFile(f.file, JSON.stringify(valid));
  await chmod(f.file, 0o644);
  await expect(f.api.accountStatus(f.config)).rejects.toMatchObject({ code: "invalid_store" });
  expect((await stat(f.file)).mode & 0o777).toBe(0o644);
  await chmod(f.file, 0o600);
  await link(f.file, join(f.root, "foreign"));
  await expect(f.api.accountStatus(f.config)).rejects.toMatchObject({ code: "invalid_store" });
  await unlink(f.file);
  await symlink(join(f.root, "foreign"), f.file);
  await expect(f.api.logoutOAuthAccount(f.config)).rejects.toMatchObject({ code: "invalid_store" });
  await chmod(f.config.directory, 0o755);
  await expect(f.api.accountStatus(f.config)).rejects.toMatchObject({ code: "invalid_store" });
  await symlink(f.config.directory, join(f.root, "linked-directory"));
  await expect(f.api.accountStatus({ ...f.config, directory: join(f.root, "linked-directory") })).rejects.toMatchObject({ code: "invalid_store" });
  await expect(f.api.accountStatus({ ...f.config, directory: "relative" })).rejects.toMatchObject({ code: "invalid_config" });
  expect(f.dependencies.login).not.toHaveBeenCalled();
});

test("cancellation releases waiting callers and logout serializes behind an active login", async () => {
  const f = await fixture(), started = gate(), finish = gate();
  f.dependencies.login = async () => { started.resolve(); await finish.promise; return fresh(); };
  const login = f.api.loginOAuthAccount(f.config, { onAuth });
  await started.promise;
  const abort = new AbortController();
  const cancelled = f.api.resolveOAuthAccount(f.config, abort.signal);
  await Bun.sleep(20);
  abort.abort("credential-in-abort-reason");
  await expect(cancelled).rejects.toMatchObject({ name: "AbortError" });
  const logout = f.api.logoutOAuthAccount(f.config);
  finish.resolve();
  await login;
  expect(await logout).toEqual({ signedIn: false, accountLabel: null });
  await expect(f.api.resolveOAuthAccount(f.config, abort.signal)).rejects.toThrow("Antigravity account operation cancelled");
  const duringLogin = new AbortController();
  f.dependencies.login = async () => { duringLogin.abort("secret"); return fresh(); };
  await expect(f.api.loginOAuthAccount(f.config, { onAuth, signal: duringLogin.signal })).rejects.toMatchObject({ name: "AbortError" });
  expect(await readdir(f.config.directory)).toEqual([]);
});

test("a lost lock refuses publication even before its renewal timer notices", async () => {
  const f = await fixture();
  await f.api.loginOAuthAccount(f.config, { onAuth });
  const before = await f.expire();
  f.dependencies.refresh = async () => { await rm(`${f.file}.lock`, { recursive: true }); return fresh(); };
  await expect(f.api.resolveOAuthAccount(f.config)).rejects.toMatchObject({ code: "lock_lost" });
  expect(await readFile(f.file, "utf8")).toBe(before);
});

test("independent processes share the refresh lock", async () => {
  const f = await fixture();
  await f.api.loginOAuthAccount(f.config, { onAuth });
  await f.expire();
  const program = `globalThis.fetch = async () => { throw new Error("Real network is forbidden"); };
    const { createOAuthAccountBoundary } = await import(${JSON.stringify(import.meta.resolve("../src/account.ts"))});
    let refreshes = 0;
    const api = createOAuthAccountBoundary({
      login: async () => { throw new Error("Login forbidden"); },
      refresh: async () => { refreshes++; await Bun.sleep(100); return { access: "child-access", refresh: "child-refresh", expires: Date.now() + 3600000, projectId: "child-project" }; },
      discoverProject: async () => { throw new Error("Discovery forbidden"); },
      fetch: async () => Response.json({ id: "google-subject", email: "owner@example.test" }),
    });
    const { accountId } = await api.resolveOAuthAccount(${JSON.stringify(f.config)});
    process.stdout.write(JSON.stringify({ accountId, refreshes }));`;
  const children = Array.from({ length: 4 }, () => Bun.spawn([process.execPath, "-e", program], { stdout: "pipe", stderr: "pipe" }));
  const results = await Promise.all(children.map(async (child) => {
    const [out, err, exit] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    expect(err).toBe(""); expect(exit).toBe(0); return JSON.parse(out);
  }));
  expect(results.reduce((total, row) => total + row.refreshes, 0)).toBe(1);
  expect(results.every((row) => row.accountId === "google-subject")).toBe(true);
});
