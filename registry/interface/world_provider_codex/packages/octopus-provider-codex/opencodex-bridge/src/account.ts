import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { lstat, mkdir, open, realpath, rename, unlink } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import lockfile from "proper-lockfile";
import {
  discoverAntigravityProject,
  loginAntigravity,
  refreshAntigravityToken,
} from "../node_modules/@bitkyc08/opencodex/src/oauth/google-antigravity.ts";
import type { OAuthController, OAuthCredentials } from "../node_modules/@bitkyc08/opencodex/src/oauth/types.ts";

export type OAuthAccountConfig = { provider: "google-antigravity"; directory: string };
export type OAuthAccountStatus = { signedIn: boolean; accountLabel: string | null };
export type OAuthAccount = { accessToken: string; projectId: string; accountId: string };
export type OAuthLoginOptions = {
  onAuth: NonNullable<OAuthController["onAuth"]>;
  onProgress?: OAuthController["onProgress"];
  signal?: AbortSignal;
};
export type OAuthAccountDependencies = {
  login: typeof loginAntigravity;
  refresh: typeof refreshAntigravityToken;
  discoverProject: typeof discoverAntigravityProject;
  fetch: (url: string, init: RequestInit) => Promise<Response>;
};

type Credential = {
  version: 1;
  provider: "google-antigravity";
  accessToken: string;
  refreshToken: string;
  expiresAt: number;
  projectId: string;
  accountId: string;
  accountLabel: string;
};
type StoredCredential = { credential: Credential; text: string };
type Store = { directory: string; file: string; signal: AbortSignal; check: () => void; verify: () => Promise<void> };

const USERINFO_URL = "https://www.googleapis.com/oauth2/v2/userinfo";
const STORE_LIMIT = 64 * 1024;
const REFRESH_MARGIN_MS = 60_000;
const LOCK_WAIT_MS = 600_000;
const CREDENTIAL_FIELDS = ["version", "provider", "accessToken", "refreshToken", "expiresAt", "projectId", "accountId", "accountLabel"];

export class OAuthAccountError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
    this.name = "OAuthAccountError";
  }
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function textValue(value: unknown, limit: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= limit
    && value.trim() === value && !/[\u0000-\u001f\u007f]/.test(value);
}

function identifier(value: unknown, limit = 512): value is string {
  return textValue(value, limit) && !/\s/.test(value);
}

function email(value: unknown): value is string {
  return textValue(value, 320) && /^[^\s@]+@[^\s@]+$/.test(value);
}

function invalidStore(): never {
  throw new OAuthAccountError("invalid_store", "Antigravity credential store is malformed or is not private");
}

function parseCredential(text: string): Credential {
  let value: unknown;
  try { value = JSON.parse(text); } catch { return invalidStore(); }
  if (!record(value) || Object.keys(value).length !== CREDENTIAL_FIELDS.length
    || CREDENTIAL_FIELDS.some((field) => !Object.hasOwn(value, field))
    || value.version !== 1 || value.provider !== "google-antigravity"
    || !identifier(value.accessToken, 16_384) || !identifier(value.refreshToken, 16_384)
    || !Number.isSafeInteger(value.expiresAt) || (value.expiresAt as number) <= 0
    || !identifier(value.projectId) || !identifier(value.accountId, 256)
    || !email(value.accountLabel) || value.accountLabel !== value.accountLabel.toLowerCase()) {
    return invalidStore();
  }
  return value as Credential;
}

function errorCode(error: unknown, code: string): boolean {
  return record(error) && error.code === code;
}

function checkAbort(signal?: AbortSignal): void {
  if (signal?.aborted) throw new DOMException("Antigravity account operation cancelled", "AbortError");
}

async function privateDirectory(config: OAuthAccountConfig, create: boolean): Promise<string | undefined> {
  if (!record(config) || Object.keys(config).length !== 2 || config.provider !== "google-antigravity"
    || !textValue(config.directory, 4096) || !isAbsolute(config.directory)) {
    throw new OAuthAccountError("invalid_config", "Antigravity requires an explicit absolute credential directory");
  }
  const directory = resolve(config.directory);
  if (create) await mkdir(directory, { recursive: true, mode: 0o700 });
  const stat = await lstat(directory).catch((error: unknown) => {
    if (errorCode(error, "ENOENT")) return undefined;
    throw error;
  });
  if (!stat) return undefined;
  if (!stat.isDirectory() || (stat.mode & 0o777) !== 0o700) invalidStore();
  return realpath(directory);
}

async function readCredential(store: Store): Promise<StoredCredential | undefined> {
  await store.verify();
  const file = await open(store.file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
    .catch((error: unknown) => {
      if (errorCode(error, "ENOENT")) return undefined;
      if (errorCode(error, "ELOOP")) invalidStore();
      throw error;
    });
  if (!file) return undefined;
  try {
    const stat = await file.stat();
    if (!stat.isFile() || stat.nlink !== 1 || (stat.mode & 0o777) !== 0o600 || stat.size > STORE_LIMIT) invalidStore();
    let text: string;
    try { text = new TextDecoder("utf-8", { fatal: true }).decode(await file.readFile()); } catch (error) {
      if (error instanceof TypeError) invalidStore();
      throw error;
    }
    if (Buffer.byteLength(text) > STORE_LIMIT) invalidStore();
    store.check();
    return { credential: parseCredential(text), text };
  } finally {
    await file.close();
  }
}

async function unchanged(store: Store, previous: StoredCredential | undefined): Promise<void> {
  if ((await readCredential(store))?.text !== previous?.text) {
    throw new OAuthAccountError("store_changed", "Antigravity credential store changed outside its lock");
  }
}

async function syncDirectory(directory: string): Promise<void> {
  const file = await open(directory, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
  try { await file.sync(); } finally { await file.close(); }
}

async function writeCredential(store: Store, credential: Credential, previous: StoredCredential | undefined): Promise<void> {
  const text = JSON.stringify(credential) + "\n";
  parseCredential(text);
  const temporary = join(store.directory, `.credential.${randomUUID()}.tmp`);
  store.check();
  const file = await open(temporary, "wx", 0o600);
  let published = false;
  try {
    try {
      await file.writeFile(text, "utf8");
      await file.sync();
    } finally {
      await file.close();
    }
    await unchanged(store, previous);
    await store.verify();
    await rename(temporary, store.file);
    published = true;
    await syncDirectory(store.directory);
  } finally {
    if (!published) await unlink(temporary);
  }
}

async function withStore<T>(
  config: OAuthAccountConfig, create: boolean, signal: AbortSignal | undefined,
  operation: (store: Store | undefined) => Promise<T>,
): Promise<T> {
  checkAbort(signal);
  const lost = new AbortController();
  const activeSignal = signal ? AbortSignal.any([signal, lost.signal]) : lost.signal;
  const check = () => {
    if (lost.signal.aborted) throw new OAuthAccountError("lock_lost", "Antigravity credential store lock was lost");
    checkAbort(signal);
  };
  try {
    const directory = await privateDirectory(config, create);
    check();
    if (!directory) return await operation(undefined);
    const file = join(directory, "credential.json");
    const directoryStat = await lstat(directory);
    const deadline = Date.now() + LOCK_WAIT_MS;
    let release: (() => Promise<void>) | undefined;
    while (!release) {
      check();
      try {
        release = await lockfile.lock(file, {
          realpath: false, retries: 0, stale: 120_000, update: 10_000,
          onCompromised: () => lost.abort(),
        });
      } catch (error) {
        if (!errorCode(error, "ELOCKED")) throw error;
        if (Date.now() >= deadline) throw new OAuthAccountError("lock_timeout", "Timed out waiting for the Antigravity credential store");
        await delay(50, undefined, { signal: activeSignal });
      }
    }
    try {
      check();
      const lockStat = await lstat(`${file}.lock`);
      const verify = async () => {
        check();
        const current = await Promise.all([lstat(directory), lstat(`${file}.lock`)]).catch(() => {
          lost.abort();
          throw new OAuthAccountError("lock_lost", "Antigravity credential store lock was lost");
        });
        if (current.some((stat, i) => !stat.isDirectory() || stat.dev !== [directoryStat, lockStat][i].dev || stat.ino !== [directoryStat, lockStat][i].ino)) {
          lost.abort();
          check();
        }
        if ((current[0].mode & 0o777) !== 0o700) invalidStore();
      };
      await verify();
      const result = await operation({ directory, file, signal: activeSignal, check, verify });
      await verify();
      if (lost.signal.aborted) check();
      return result;
    } finally {
      if (!lost.signal.aborted) await release();
    }
  } catch (error) {
    check();
    if (error instanceof OAuthAccountError) throw error;
    throw new OAuthAccountError("store_io", "Antigravity credential store operation failed");
  }
}

function signedIn(credential?: Credential): OAuthAccountStatus {
  return { signedIn: credential !== undefined, accountLabel: credential?.accountLabel ?? null };
}

async function external<T>(store: Store, code: string, message: string, action: () => Promise<T>): Promise<T> {
  store.check();
  try {
    const value = await action();
    store.check();
    return value;
  } catch {
    store.check();
    throw new OAuthAccountError(code, message);
  }
}

export function createOAuthAccountBoundary(dependencies: OAuthAccountDependencies) {
  async function request(store: Store, url: string, init: RequestInit): Promise<unknown> {
    const response = await dependencies.fetch(url, {
      ...init, redirect: "error", signal: AbortSignal.any([store.signal, AbortSignal.timeout(30_000)]),
    });
    if (!response.ok || response.redirected || (response.url && response.url !== url)) {
      await response.body?.cancel();
      throw new Error("Antigravity request refused");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new Error("Antigravity response is empty");
    const chunks: Uint8Array[] = [];
    let size = 0;
    try {
      for (;;) {
        store.check();
        const { done, value } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > 2 * 1024 * 1024) throw new Error("Antigravity response exceeds the size limit");
        chunks.push(value);
      }
      return JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } finally {
      await reader.cancel();
    }
  }

  async function verifiedCredential(store: Store, fresh: OAuthCredentials, previous?: Credential): Promise<Credential> {
    if (!fresh || !identifier(fresh.access, 16_384) || !identifier(fresh.refresh, 16_384)
      || !Number.isSafeInteger(fresh.expires) || fresh.expires <= Date.now() + REFRESH_MARGIN_MS) {
      throw new OAuthAccountError("invalid_credential", "Antigravity returned an unusable credential");
    }
    const identity = await external(store, "identity_failed", "Antigravity account identity verification failed", () => request(store, USERINFO_URL, {
      method: "GET", headers: { Accept: "application/json", Authorization: `Bearer ${fresh.access}` },
    }));
    if (!record(identity) || !identifier(identity.id, 256) || !email(identity.email)) {
      throw new OAuthAccountError("identity_failed", "Antigravity identity response is malformed");
    }
    if (previous && previous.accountId !== identity.id) {
      throw new OAuthAccountError("account_mismatch", "Antigravity refresh returned a different Google account");
    }
    let projectId = fresh.projectId;
    if (projectId === undefined) {
      projectId = await external(store, "project_unavailable", "Antigravity project discovery failed", () => dependencies.discoverProject(fresh.access, store.signal));
    }
    if (!identifier(projectId)) throw new OAuthAccountError("project_unavailable", "Antigravity did not return a usable Cloud Code Assist project");
    if (fresh.expires <= Date.now() + REFRESH_MARGIN_MS) {
      throw new OAuthAccountError("invalid_credential", "Antigravity credential expired during account verification");
    }
    return {
      version: 1, provider: "google-antigravity", accessToken: fresh.access, refreshToken: fresh.refresh,
      expiresAt: fresh.expires, projectId, accountId: identity.id, accountLabel: identity.email.toLowerCase(),
    };
  }

  async function resolveCredential(store: Store | undefined): Promise<Credential> {
    const saved = store && await readCredential(store);
    if (!store || !saved) throw new OAuthAccountError("not_signed_in", "Antigravity is not signed in");
    if (saved.credential.expiresAt > Date.now() + REFRESH_MARGIN_MS) return saved.credential;
    const fresh = await external(store, "refresh_failed", "Antigravity token refresh failed; sign in again", () => dependencies.refresh(saved.credential.refreshToken, store.signal));
    const credential = await verifiedCredential(store, fresh, saved.credential);
    await writeCredential(store, credential, saved);
    return credential;
  }

  return {
    resolveOAuthAccount(config: OAuthAccountConfig, signal?: AbortSignal): Promise<OAuthAccount> {
      return withStore(config, false, signal, async (store) => {
        const { accessToken, projectId, accountId } = await resolveCredential(store);
        return { accessToken, projectId, accountId };
      });
    },
    accountStatus(config: OAuthAccountConfig): Promise<OAuthAccountStatus> {
      return withStore(config, false, undefined, async (store) => signedIn(store ? (await readCredential(store))?.credential : undefined));
    },
    loginOAuthAccount(config: OAuthAccountConfig, options: OAuthLoginOptions): Promise<OAuthAccountStatus> {
      if (typeof options?.onAuth !== "function") return Promise.reject(new OAuthAccountError("invalid_config", "Antigravity login requires an authorization URL callback"));
      return withStore(config, true, options.signal, async (store) => {
        if (!store) throw new OAuthAccountError("store_io", "Antigravity credential directory is unavailable");
        const previous = await readCredential(store);
        const fresh = await external(store, "login_failed", "Antigravity login failed", () => dependencies.login({
          onAuth: (info) => { store.check(); options.onAuth(info); store.check(); },
          onProgress: (message) => { store.check(); options.onProgress?.(message); store.check(); },
          signal: store.signal,
        }, { forceAccountSelect: true }));
        const credential = await verifiedCredential(store, fresh);
        await writeCredential(store, credential, previous);
        return signedIn(credential);
      });
    },
    logoutOAuthAccount(config: OAuthAccountConfig): Promise<OAuthAccountStatus> {
      return withStore(config, false, undefined, async (store) => {
        if (store) {
          const previous = await readCredential(store);
          if (previous) {
            await unchanged(store, previous);
            await store.verify();
            await unlink(store.file);
            await syncDirectory(store.directory);
          }
        }
        return signedIn();
      });
    },
  };
}

export const { resolveOAuthAccount, accountStatus, loginOAuthAccount, logoutOAuthAccount } = createOAuthAccountBoundary({
  login: loginAntigravity, refresh: refreshAntigravityToken, discoverProject: discoverAntigravityProject,
  fetch: (url, init) => fetch(url, init),
});
