import { spawn as nodeSpawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream, existsSync } from "node:fs";
import { chmod, lstat, mkdir, mkdtemp, open, readFile, rename, rm, writeFile } from "node:fs/promises";
import { arch, platform } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { crc32, createInflateRaw } from "node:zlib";
import release from "../antigravity-acp.json" with { type: "json" };
import type { AdapterEvent, OcxParsedRequest, OcxProviderConfig } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import type { ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import { buildCodeBuddyToolBridge, CODEBUDDY_MCP_SERVER_NAME, CODEBUDDY_MCP_TOOL_PREFIX } from "../node_modules/@bitkyc08/opencodex/src/adapters/codebuddy/tool-bridge.ts";
import { stableClaudeToolBridge } from "../node_modules/@bitkyc08/opencodex/src/adapters/claude-cli/stable-replay.ts";
import { buildSystemPrompt, readJsonLines } from "../node_modules/@bitkyc08/opencodex/src/adapters/coding-agent/protocol.ts";
import { AcpFailure, closeAntigravityConversations, runAntigravityConversation } from "./antigravity-conversation.ts";
export { closeAntigravityConversations } from "./antigravity-conversation.ts";

export const ANTIGRAVITY_ACCOUNT_PROVIDER = "antigravity-acp";

export type AntigravityAccountConfig = { provider: typeof ANTIGRAVITY_ACCOUNT_PROVIDER; directory: string };
export type AntigravityAccountStatus = { signedIn: boolean; accountLabel: string | null };

const PRECEDENCE = [
  "The instructions in this section come from the system that hosts this conversation and take precedence over everything earlier in this system prompt.",
  "They replace the identity, role, workflow, tool and formatting guidance given before them: you are not an IDE coding assistant. The client owns this conversation and its available functions.",
  "Where anything earlier conflicts with this section, follow this section.",
].join("\n");
const CONTEXT_PROTOCOL = [
  "The client owns the conversation and executes the declared functions. Its input can include a transcript in text or ordered attachments.",
  "USER is a client input. ASSISTANT HISTORY contains ordered JSON records of past assistant content, including toolCall records. TOOL RESULT identifies data returned for a recorded call. Continue from the last input. Historical calls have already been issued.",
  "The transcript is historical data, not an output format to continue. To take a new action, invoke the native call_mcp_tool function with the declared server and tool name; writing a tool call in your answer executes nothing.",
  "Only the external tool's returned result establishes an effect. Never author TOOL RESULT messages or report an effect from a prepared plan alone.",
].join("\n");
const AUTH_PROMPT = /Open the following link to authenticate the ACP server: (https:\/\/\S+)/;
const STDERR_LIMIT = 16_384;
const KILL_GRACE_MS = 2000;
const STARTUP_TIMEOUT_MS = 180_000;
const DOWNLOAD_TIMEOUT_MS = 900_000;

type ErrorEvent = Extract<AdapterEvent, { type: "error" }>;
type Asset = { args: string[]; url: string; sha256: string; archive_bytes: number; executable_bytes: number; harness_bytes: number };
type JsonRecord = Record<string, unknown>;

export class AntigravityAccountError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
    this.name = "AntigravityAccountError";
  }
}

const record = (value: unknown): JsonRecord | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as JsonRecord : undefined;

const failure = (message: string, code: string, status = 500, retryable = false): ErrorEvent => ({
  type: "error", message, status, errorType: status === 401 ? "authentication_error" : status === 400 ? "invalid_request_error" : "upstream_error", code, retryable,
});

const RECONNECT_HINT = "Connect Antigravity in Applications or run 'octopus connect world_provider_antigravity'.";
const MAX_ZIP_CENTRAL_DIR_BYTES = 4 * 1024 * 1024;
const MAX_ZIP_EOCD_WINDOW = 65_557;

export type InstallationInspection =
  | { ok: true; key: string; asset: Asset; paths: ReturnType<typeof layout> }
  | { ok: false; key: string; code: string; status: number; message: string };

export function antigravityPlatformKey(hostPlatform = platform(), hostArch = arch()): string {
  return `${hostPlatform}-${hostArch}`;
}

export function antigravityReleaseAsset(key = antigravityPlatformKey()): Asset {
  const entry = (release.assets as Record<string, Asset>)[key];
  if (!entry) throw new AntigravityAccountError("unsupported_platform", `Google publishes no Antigravity ACP server for ${key}.`);
  return entry;
}

function assetPlatformForDigest(sha256: string): string | undefined {
  for (const [key, entry] of Object.entries(release.assets as Record<string, Asset>)) {
    if (entry.sha256 === sha256) return key;
  }
  return undefined;
}

function layout(directory: string) {
  const install = join(directory, "acp", release.version);
  return {
    install,
    executable: join(install, release.executable),
    harness: join(install, release.harness),
    receipt: join(install, "receipt.json"),
    home: join(directory, "home"),
  };
}

async function ownedDirectory(path: string, create: boolean): Promise<boolean> {
  if (create) await mkdir(path, { recursive: true, mode: 0o700 });
  const stat = await lstat(path).catch((error: NodeJS.ErrnoException) => {
    if (error.code === "ENOENT") return undefined;
    throw error;
  });
  if (!stat) return false;
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new AntigravityAccountError("invalid_store", `Antigravity account storage '${path}' is not a regular owned directory.`);
  }
  if ((stat.mode & 0o777) !== 0o700) await chmod(path, 0o700);
  return true;
}

function accountDirectory(config: AntigravityAccountConfig): string {
  if (!record(config) || Object.keys(config).length !== 2 || config.provider !== ANTIGRAVITY_ACCOUNT_PROVIDER
    || typeof config.directory !== "string" || !isAbsolute(config.directory) || config.directory.includes("\0")) {
    throw new AntigravityAccountError("invalid_config", "Antigravity requires an explicit absolute account directory.");
  }
  return resolve(config.directory);
}

export async function inspectAntigravityInstallation(
  directory: string,
  hostKey = antigravityPlatformKey(),
): Promise<InstallationInspection> {
  const source = antigravityReleaseAsset(hostKey);
  const paths = layout(directory);
  let text: string;
  try {
    text = await readFile(paths.receipt, "utf8");
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === "ENOENT") {
      return {
        ok: false, key: hostKey, code: "antigravity_not_connected", status: 401,
        message: `Antigravity ACP server ${release.version} for ${hostKey} is not installed. ${RECONNECT_HINT}`,
      };
    }
    return {
      ok: false, key: hostKey, code: "antigravity_invalid_installation", status: 401,
      message: `Installed Antigravity ACP receipt for ${release.version} cannot be read. ${RECONNECT_HINT}`,
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return {
      ok: false, key: hostKey, code: "antigravity_invalid_installation", status: 401,
      message: `Installed Antigravity ACP receipt for ${release.version} is malformed. ${RECONNECT_HINT}`,
    };
  }
  const receipt = record(parsed);
  if (!receipt || receipt.version !== release.version) {
    return {
      ok: false, key: hostKey, code: "antigravity_invalid_installation", status: 401,
      message: `Installed Antigravity ACP server version does not match ${release.version}. ${RECONNECT_HINT}`,
    };
  }
  const recordedPlatform = typeof receipt.platform === "string" && receipt.platform ? receipt.platform : undefined;
  if (receipt.sha256 !== source.sha256 || (recordedPlatform !== undefined && recordedPlatform !== hostKey)) {
    const installedPlatform = recordedPlatform
      ?? (typeof receipt.sha256 === "string" ? assetPlatformForDigest(receipt.sha256) : undefined);
    if (installedPlatform && installedPlatform !== hostKey) {
      return {
        ok: false, key: hostKey, code: "antigravity_incompatible_platform", status: 400,
        message: `Installed Antigravity ACP server ${release.version} is for ${installedPlatform}, not ${hostKey}. ${RECONNECT_HINT}`,
      };
    }
    return {
      ok: false, key: hostKey, code: "antigravity_invalid_installation", status: 401,
      message: `Installed Antigravity ACP server ${release.version} does not match the pinned ${hostKey} digest. ${RECONNECT_HINT}`,
    };
  }
  for (const [name, file] of [[release.executable, paths.executable], [release.harness, paths.harness]] as const) {
    const stat = await lstat(file).catch(() => undefined);
    if (!stat?.isFile() || stat.isSymbolicLink() || (stat.mode & 0o100) === 0) {
      return {
        ok: false, key: hostKey, code: "antigravity_invalid_installation", status: 401,
        message: `Installed Antigravity ACP executable '${name}' is missing or not executable. ${RECONNECT_HINT}`,
      };
    }
  }
  return { ok: true, key: hostKey, asset: source, paths };
}

async function readExact(handle: Awaited<ReturnType<typeof open>>, length: number, position: number): Promise<Buffer> {
  const buffer = Buffer.alloc(length);
  let total = 0;
  while (total < length) {
    const { bytesRead } = await handle.read(buffer, total, length - total, position + total);
    if (bytesRead <= 0) throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive is truncated.");
    total += bytesRead;
  }
  return buffer;
}

export async function downloadVerifiedArchive(
  url: string,
  expectedBytes: number,
  expectedSha256: string,
  destination: string,
  options: { signal?: AbortSignal; fetchImpl?: typeof fetch } = {},
): Promise<void> {
  const timeout = AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS);
  const signal = options.signal ? AbortSignal.any([options.signal, timeout]) : timeout;
  const response = await (options.fetchImpl ?? fetch)(url, {
    signal,
    redirect: "error",
    headers: { "accept-encoding": "identity" },
  });
  if (!response.ok) throw new AntigravityAccountError("download_failed", `Google's Antigravity ACP server download answered HTTP ${response.status}.`);
  const encoding = response.headers.get("content-encoding")?.trim().toLowerCase() ?? "";
  const compressed = encoding.length > 0 && encoding !== "identity";
  const advertisedLength = compressed
    ? response.headers.get("x-identity-content-length")
    : response.headers.get("content-length");
  if (advertisedLength !== null && /^\d+$/.test(advertisedLength.trim()) && Number(advertisedLength.trim()) !== expectedBytes) {
    await response.body?.cancel().catch(() => undefined);
    throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive does not match its pinned size.");
  }
  if (!response.body) throw new AntigravityAccountError("download_failed", "Google's Antigravity ACP server download returned an empty body.");
  const hash = createHash("sha256");
  const handle = await open(destination, "wx", 0o600);
  const reader = response.body.getReader();
  let received = 0;
  try {
    for (;;) {
      if (signal.aborted) throw signal.reason instanceof Error ? signal.reason : new Error("Antigravity ACP server download was cancelled.");
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = value instanceof Uint8Array ? value : new Uint8Array(value);
      received += chunk.byteLength;
      if (received > expectedBytes) {
        throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive exceeded its pinned size.");
      }
      hash.update(chunk);
      let written = 0;
      while (written < chunk.byteLength) {
        const { bytesWritten } = await handle.write(chunk, written, chunk.byteLength - written);
        if (bytesWritten <= 0) throw new Error("Short write while staging Antigravity ACP archive.");
        written += bytesWritten;
      }
    }
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  } finally {
    reader.releaseLock();
    await handle.close();
  }
  if (received !== expectedBytes || hash.digest("hex") !== expectedSha256) {
    throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive does not match its pinned digest.");
  }
}

export async function extractZipEntriesToFiles(
  archivePath: string,
  targets: Record<string, { destination: string; expectedBytes: number; mode: number }>,
): Promise<void> {
  const handle = await open(archivePath, "r");
  try {
    const fileSize = (await handle.stat()).size;
    if (fileSize < 22) throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive has no zip directory.");
    const tailSize = Math.min(fileSize, MAX_ZIP_EOCD_WINDOW);
    const tailStart = fileSize - tailSize;
    const tail = await readExact(handle, tailSize, tailStart);
    let eocdPos = -1;
    for (let pos = tailSize - 22; pos >= 0; pos--) {
      if (tail.readUInt32LE(pos) === 0x06054b50 && tailStart + pos + 22 + tail.readUInt16LE(pos + 20) === fileSize) {
        eocdPos = pos;
        break;
      }
    }
    if (eocdPos < 0) throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive has no zip directory.");
    const eocdOffset = tailStart + eocdPos;
    const diskNumber = tail.readUInt16LE(eocdPos + 4);
    const cdDisk = tail.readUInt16LE(eocdPos + 6);
    const diskEntries = tail.readUInt16LE(eocdPos + 8);
    const count = tail.readUInt16LE(eocdPos + 10);
    const cdSize = tail.readUInt32LE(eocdPos + 12);
    const cdOffset = tail.readUInt32LE(eocdPos + 16);
    if (diskNumber !== 0 || cdDisk !== 0 || diskEntries !== count || count === 0xffff || cdSize === 0xffffffff
      || cdOffset === 0xffffffff || cdSize > MAX_ZIP_CENTRAL_DIR_BYTES || cdOffset + cdSize > eocdOffset) {
      throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive directory is malformed.");
    }
    const cd = await readExact(handle, cdSize, cdOffset);
    const seen = new Set<string>();
    let offset = 0;
    for (let index = 0; index < count; index++) {
      if (offset + 46 > cd.length || cd.readUInt32LE(offset) !== 0x02014b50) {
        throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive directory is malformed.");
      }
      const method = cd.readUInt16LE(offset + 10);
      const crc = cd.readUInt32LE(offset + 16);
      const compressed = cd.readUInt32LE(offset + 20);
      const size = cd.readUInt32LE(offset + 24);
      const nameLength = cd.readUInt16LE(offset + 28);
      const extraLength = cd.readUInt16LE(offset + 30);
      const commentLength = cd.readUInt16LE(offset + 32);
      const local = cd.readUInt32LE(offset + 42);
      if (offset + 46 + nameLength + extraLength + commentLength > cd.length) {
        throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive directory is malformed.");
      }
      const name = cd.subarray(offset + 46, offset + 46 + nameLength).toString("utf8");
      offset += 46 + nameLength + extraLength + commentLength;
      const target = targets[name];
      if (!target) continue;
      if (seen.has(name)) throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive repeats entry ${name}.`);
      seen.add(name);
      if (size !== target.expectedBytes) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} does not match its declared size.`);
      }
      if (method !== 0 && method !== 8) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} uses an unsupported compression method.`);
      }
      if (local + 30 > cdOffset) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} is malformed.`);
      }
      const localHeader = await readExact(handle, 30, local);
      if (localHeader.readUInt32LE(0) !== 0x04034b50) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} is malformed.`);
      }
      const dataStart = local + 30 + localHeader.readUInt16LE(26) + localHeader.readUInt16LE(28);
      if (dataStart + compressed > cdOffset) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} is malformed.`);
      }
      if (compressed === 0) {
        if (method !== 0 || target.expectedBytes !== 0 || crc !== 0) {
          throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} failed its integrity check.`);
        }
        await writeFile(target.destination, Buffer.alloc(0), { mode: target.mode, flag: "wx" });
        await chmod(target.destination, target.mode);
        continue;
      }
      let written = 0;
      let runningCrc = 0;
      const source = createReadStream(archivePath, { start: dataStart, end: dataStart + compressed - 1 });
      const verifier = new Transform({
        transform(chunk, _encoding, callback) {
          const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
          written += buf.length;
          if (written > target.expectedBytes) {
            callback(new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} exceeded its declared size.`));
            return;
          }
          runningCrc = crc32(buf, runningCrc);
          callback(null, buf);
        },
      });
      const destination = createWriteStream(target.destination, { flags: "wx", mode: target.mode });
      try {
        if (method === 8) await pipeline(source, createInflateRaw(), verifier, destination);
        else await pipeline(source, verifier, destination);
      } catch (error) {
        if (error instanceof AntigravityAccountError) throw error;
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} failed its integrity check.`);
      }
      if (written !== target.expectedBytes || (runningCrc >>> 0) !== crc) {
        throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} failed its integrity check.`);
      }
      await chmod(target.destination, target.mode);
    }
    if (seen.size !== Object.keys(targets).length) {
      throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive lacks its declared executables.");
    }
  } finally {
    await handle.close();
  }
}

export async function installAntigravityAcp(
  directory: string,
  options: { signal?: AbortSignal; onProgress?: (message: string) => void; hostKey?: string; fetchImpl?: typeof fetch; assetOverride?: Asset } = {},
): Promise<ReturnType<typeof layout>> {
  const hostKey = options.hostKey ?? antigravityPlatformKey();
  const source = options.assetOverride ?? antigravityReleaseAsset(hostKey);
  if (!options.assetOverride) {
    const present = await inspectAntigravityInstallation(directory, hostKey);
    if (present.ok) return present.paths;
  }
  const progress = options.onProgress ?? (() => undefined);
  const paths = layout(directory);
  await ownedDirectory(directory, true);
  await ownedDirectory(join(directory, "acp"), true);
  const staging = await mkdtemp(join(directory, "acp", ".install-"));
  await chmod(staging, 0o700);
  const archivePath = join(staging, ".archive.zip");
  let replaced: string | undefined;
  try {
    progress(`Downloading Google's Antigravity ACP server ${release.version}`);
    await downloadVerifiedArchive(source.url, source.archive_bytes, source.sha256, archivePath, {
      signal: options.signal, fetchImpl: options.fetchImpl,
    });
    progress("Verifying the Antigravity ACP server");
    await extractZipEntriesToFiles(archivePath, {
      [release.executable]: { destination: join(staging, release.executable), expectedBytes: source.executable_bytes, mode: 0o700 },
      [release.harness]: { destination: join(staging, release.harness), expectedBytes: source.harness_bytes, mode: 0o700 },
    });
    await rm(archivePath, { force: true });
    const receiptPath = join(staging, "receipt.json");
    await writeFile(receiptPath, `${JSON.stringify({ version: release.version, platform: hostKey, sha256: source.sha256 })}\n`, { mode: 0o600, flag: "wx" });
    await chmod(receiptPath, 0o600);
    const existing = await lstat(paths.install).catch(() => undefined);
    if (existing) {
      replaced = `${paths.install}.replace-${process.pid}-${Date.now()}`;
      await rename(paths.install, replaced);
    }
    try {
      await rename(staging, paths.install);
    } catch (error) {
      if (replaced) await rename(replaced, paths.install).catch(() => undefined);
      throw error;
    }
    if (replaced) await rm(replaced, { recursive: true, force: true }).catch(() => undefined);
  } catch (error) {
    await rm(staging, { recursive: true, force: true });
    throw error;
  }
  return paths;
}

function browserNoop(): string {
  return existsSync("/usr/bin/true") ? "/usr/bin/true" : "/bin/true";
}

function environment(paths: ReturnType<typeof layout>, turn: string): Record<string, string> {
  return {
    GEMINI_HOME: paths.home,
    HOME: join(turn, "home"),
    TMPDIR: join(turn, "tmp"),
    ANTIGRAVITY_HARNESS_PATH: paths.harness,
    AGY_ACP_FORCE_FILE_STORAGE: "1",
    BROWSER: browserNoop(),
    PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
    LANG: process.env.LANG ?? "en_US.UTF-8",
    ...Object.fromEntries(["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR"].flatMap(name => process.env[name] ? [[name, process.env[name]!]] : [])),
  };
}

type Pending = { method: string; resolve: (value: JsonRecord) => void; reject: (error: Error) => void };

export function createAcpStderrScanner(handlers: {
  onDiagnostic: (line: string) => void;
  onAuthorizationUrl: (url: string) => void;
}) {
  let pendingLine = "";
  let lastAuthorizationUrl = "";
  const emitLine = (line: string) => {
    handlers.onDiagnostic(line);
    const url = AUTH_PROMPT.exec(line)?.[1];
    if (url && url !== lastAuthorizationUrl) {
      lastAuthorizationUrl = url;
      handlers.onAuthorizationUrl(url);
    }
  };
  return {
    push(text: string): void {
      pendingLine += text;
      let newline: number;
      while ((newline = pendingLine.indexOf("\n")) >= 0) {
        emitLine(pendingLine.slice(0, newline));
        pendingLine = pendingLine.slice(newline + 1);
      }
      if (pendingLine.length > STDERR_LIMIT) pendingLine = pendingLine.slice(-STDERR_LIMIT);
    },
    flush(): void {
      if (!pendingLine) return;
      const line = pendingLine;
      pendingLine = "";
      emitLine(line);
    },
  };
}

class AcpProcess {
  readonly child: ChildProcess;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private stderr = "";
  private killed = false;
  readonly closed: Promise<number | null>;
  onNotification: (method: string, params: JsonRecord) => void = () => undefined;
  onRequest: (method: string, params: JsonRecord) => JsonRecord | undefined = () => undefined;
  onDiagnostic: (line: string) => void = () => undefined;
  onAuthorizationUrl: (url: string) => void = () => undefined;

  constructor(paths: ReturnType<typeof layout>, turn: string, spawn: typeof nodeSpawn, private readonly maxFrameBytes = 8 * 1024 * 1024) {
    this.child = spawn(paths.executable, antigravityReleaseAsset().args, {
      cwd: join(turn, "workspace"), env: environment(paths, turn), stdio: ["pipe", "pipe", "pipe"], detached: true,
    });
    this.closed = new Promise(resolve => {
      this.child.once("close", code => resolve(code));
      this.child.once("error", () => resolve(null));
    });
    this.child.stdin?.on("error", () => this.kill());
    const scanner = createAcpStderrScanner({
      onDiagnostic: line => this.onDiagnostic(line),
      onAuthorizationUrl: url => this.onAuthorizationUrl(url),
    });
    this.child.stderr?.on("data", (chunk: Buffer) => {
      const text = chunk.toString("utf8");
      this.stderr = (this.stderr + text).slice(-STDERR_LIMIT);
      scanner.push(text);
    });
    this.child.stderr?.on("end", () => scanner.flush());
    void this.read();
    void this.closed.then(code => {
      const error = new AcpFailure(`Antigravity ACP server exited${code === null ? "" : ` with ${code}`}: ${this.diagnostic()}`, "antigravity_connection_closed", 502, true);
      for (const entry of this.pending.values()) entry.reject(error);
      this.pending.clear();
    });
  }

  authenticate(): Promise<JsonRecord> {
    const signedOut = Promise.withResolvers<never>();
    const auth = connect(this, () => {
      signedOut.reject(new AcpFailure(`Antigravity is signed out. ${RECONNECT_HINT}`, "antigravity_not_signed_in", 401));
      this.kill();
    });
    return withTimeout(Promise.race([auth, signedOut.promise]), STARTUP_TIMEOUT_MS, "Antigravity sign-in");
  }

  diagnostic(): string {
    return this.stderr.trim().split("\n").filter(line => !AUTH_PROMPT.test(line)).slice(-6).join(" | ").slice(-1200);
  }

  private send(message: JsonRecord): void {
    if (!this.child.stdin?.writable) throw new Error(`Antigravity ACP server is not accepting input: ${this.diagnostic()}`);
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
  }

  request(method: string, params: JsonRecord): Promise<JsonRecord> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { method, resolve, reject });
      try { this.send({ id, method, params }); } catch (error) {
        this.pending.delete(id);
        reject(error as Error);
      }
    });
  }

  notify(method: string, params: JsonRecord): void {
    this.send({ method, params });
  }

  private async read(): Promise<void> {
    try {
      for await (const message of readJsonLines(this.child.stdout!, { maxLineBytes: this.maxFrameBytes, maxTotalBytes: Number.MAX_SAFE_INTEGER })) {
        const id = message.id;
        if (typeof message.method === "string") {
          const params = record(message.params) ?? {};
          if (id === undefined || id === null) {
            this.onNotification(message.method, params);
            continue;
          }
          const result = this.onRequest(message.method, params);
          this.send(result === undefined
            ? { id, error: { code: -32601, message: `${message.method} is not offered by this client` } }
            : { id, result });
        } else if (typeof id === "number" && this.pending.has(id)) {
          const entry = this.pending.get(id)!;
          this.pending.delete(id);
          const error = record(message.error);
          if (error) entry.reject(Number(error.code) === -32000
            ? new AcpFailure(String(error.message ?? "Antigravity requires sign-in."), "antigravity_not_signed_in", 401)
            : new AcpFailure(`Antigravity ${entry.method} failed (${error.code}): ${error.message ?? "no diagnostic"}`));
          else if (record(message.result)) entry.resolve(message.result as JsonRecord);
          else entry.reject(new AcpFailure(`Antigravity ${entry.method} returned a malformed RPC response.`));
        }
      }
    } catch (error) {
      for (const entry of this.pending.values()) entry.reject(error as Error);
      this.pending.clear();
      this.kill();
    }
  }

  kill(): void {
    if (this.killed || this.child.exitCode !== null || this.child.signalCode !== null || this.child.pid === undefined) return;
    this.killed = true;
    try { process.kill(-this.child.pid, "SIGTERM"); } catch {}
    setTimeout(() => {
      if (this.child.exitCode === null && this.child.signalCode === null && this.child.pid !== undefined) {
        try { process.kill(-this.child.pid, "SIGKILL"); } catch {}
      }
    }, KILL_GRACE_MS).unref();
  }
}

async function turnDirectory(baseDirectory: string): Promise<string> {
  await ownedDirectory(baseDirectory, true);
  const directory = await mkdtemp(join(baseDirectory, ".login-"));
  await chmod(directory, 0o700);
  for (const name of ["home", "tmp", "workspace"]) {
    const sub = join(directory, name);
    await mkdir(sub, { mode: 0o700 });
    await chmod(sub, 0o700);
  }
  return directory;
}

async function connect(acp: AcpProcess, onAuthorizationUrl: (url: string) => void): Promise<JsonRecord> {
  await acp.request("initialize", {
    protocolVersion: 1,
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
    clientInfo: { name: "octopus", title: "Octopus", version: "1.0.0" },
  });
  acp.onAuthorizationUrl = onAuthorizationUrl;
  return acp.request("authenticate", { methodId: release.auth_method });
}

function tierLabel(response: JsonRecord): string {
  const tier = record(record(record(response._meta)?.antigravity)?.tier);
  return typeof tier?.name === "string" && tier.name.trim() ? `Google · ${tier.name.trim()}` : "Google account";
}

export async function antigravityAccountStatus(config: AntigravityAccountConfig, hostKey = antigravityPlatformKey()): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  const inspection = await inspectAntigravityInstallation(directory, hostKey);
  if (!inspection.ok || !(await ownedDirectory(inspection.paths.home, false))) return { signedIn: false, accountLabel: null };
  const token = await lstat(join(inspection.paths.home, release.agent, "acp_token.json")).catch(() => undefined);
  if (!token?.isFile() || token.size === 0) return { signedIn: false, accountLabel: null };
  let saved: JsonRecord | undefined;
  try {
    saved = record(JSON.parse(await readFile(join(directory, "account.json"), "utf8")));
  } catch {
    saved = undefined;
  }
  return { signedIn: true, accountLabel: typeof saved?.label === "string" && saved.label ? saved.label : "Google account" };
}

export async function antigravityInstall(config: AntigravityAccountConfig, options: {
  onProgress?: (message: string) => void; signal?: AbortSignal; hostKey?: string; fetchImpl?: typeof fetch; assetOverride?: Asset;
} = {}): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  await installAntigravityAcp(directory, options);
  return antigravityAccountStatus(config, options.hostKey);
}

export async function antigravityLogin(config: AntigravityAccountConfig, options: {
  onAuth: (url: string) => void; onProgress?: (message: string) => void; signal?: AbortSignal;
}): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  await ownedDirectory(directory, true);
  const paths = await installAntigravityAcp(directory, { signal: options.signal, onProgress: options.onProgress });
  await ownedDirectory(paths.home, true);
  const turn = await turnDirectory(directory);
  const acp = new AcpProcess(paths, turn, nodeSpawn);
  const abort = () => acp.kill();
  options.signal?.addEventListener("abort", abort, { once: true });
  try {
    options.onProgress?.("Waiting for Google sign-in");
    const response = await withTimeout(connect(acp, url => options.onAuth(url)), DOWNLOAD_TIMEOUT_MS, "Antigravity sign-in");
    const label = tierLabel(response);
    await writeFile(join(directory, "account.json"), `${JSON.stringify({ label })}\n`, { mode: 0o600 });
    return { signedIn: true, accountLabel: label };
  } catch (error) {
    throw new AntigravityAccountError("login_failed", `Antigravity sign-in failed: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    options.signal?.removeEventListener("abort", abort);
    acp.kill();
    await acp.closed;
    await rm(turn, { recursive: true, force: true });
  }
}

export async function antigravityLogout(config: AntigravityAccountConfig): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  const paths = layout(directory);
  await closeAntigravityConversations(directory);
  await rm(join(paths.home, release.agent, "acp_token.json"), { force: true });
  await rm(join(directory, "account.json"), { force: true });
  return { signedIn: false, accountLabel: null };
}

async function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out.`)), ms);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

export function createAntigravityAcpAdapter(_provider: OcxProviderConfig, account: AntigravityAccountConfig,
  scope: { owner: string; thread: string; directory: string; maxRequestBytes: number; idleTimeoutMs: number }): ProviderAdapter {
  return {
    name: "antigravity-acp",
    buildRequest() { throw new Error("Antigravity runs through Google's ACP server."); },
    async *parseStream() { throw new Error("Antigravity runs through Google's ACP server."); },
    async runTurn(parsed, incoming, emit) {
      try {
        if (!scope.owner || !scope.thread) {
          emit(failure("Antigravity requests require an explicit native thread-id for context continuity.", "antigravity_thread_required", 400));
          return;
        }
        const directory = accountDirectory(account);
        const inspection = await inspectAntigravityInstallation(directory);
        if (!inspection.ok) {
          emit(failure(inspection.message, inspection.code, inspection.status));
          return;
        }
        const paths = inspection.paths;
        for (const message of parsed.context.messages) {
          if (message.role === "developer" && Array.isArray(message.content) && message.content.some(part => part.type !== "text")) {
            emit(failure("Antigravity requires text in developer instructions.", "antigravity_developer_content", 400));
            return;
          }
        }
        const bridge = stableClaudeToolBridge(buildCodeBuddyToolBridge(parsed));
        const wireByName = new Map([...bridge.emittedNameMap].map(([emitted, wire]) => [emitted.slice(CODEBUDDY_MCP_TOOL_PREFIX.length), wire]));
        const systemPrompt = [PRECEDENCE, buildSystemPrompt(parsed), CONTEXT_PROTOCOL].filter(Boolean).join("\n\n");
        await runAntigravityConversation({
          account: directory, ...scope, serverName: CODEBUDDY_MCP_SERVER_NAME,
          start: root => new AcpProcess(paths, root, nodeSpawn, Math.max(8 * 1024 * 1024, scope.maxRequestBytes * 2)),
        }, {
          parsed, systemPrompt, tools: bridge.tools, wireByName, requireToolCall: bridge.requireToolCall, incoming, emit,
        });
      } catch (error) {
        if (error instanceof AntigravityAccountError) {
          emit(failure(error.message, error.code, error.code === "unsupported_platform" ? 400 : 401));
          return;
        }
        emit(failure(error instanceof Error ? error.message : String(error), "antigravity_turn_failed", 502));
      }
    },
  };
}
