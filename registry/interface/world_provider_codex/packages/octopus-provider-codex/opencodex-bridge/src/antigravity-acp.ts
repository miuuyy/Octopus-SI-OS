import { spawn as nodeSpawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { arch, platform, tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { crc32, inflateRawSync } from "node:zlib";
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

function asset(): Asset {
  const key = `${platform()}-${arch()}`;
  const entry = (release.assets as Record<string, Asset>)[key];
  if (!entry) throw new AntigravityAccountError("unsupported_platform", `Google publishes no Antigravity ACP server for ${key}.`);
  return entry;
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

async function installed(directory: string): Promise<ReturnType<typeof layout> | undefined> {
  const paths = layout(directory);
  const text = await readFile(paths.receipt, "utf8").catch(() => undefined);
  if (text === undefined) return undefined;
  const receipt = record(JSON.parse(text));
  if (receipt?.version !== release.version || receipt.sha256 !== asset().sha256) return undefined;
  for (const file of [paths.executable, paths.harness]) {
    const stat = await lstat(file).catch(() => undefined);
    if (!stat?.isFile() || (stat.mode & 0o100) === 0) return undefined;
  }
  return paths;
}

function zipEntries(archive: Buffer): Map<string, Buffer> {
  const end = archive.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (end < 0) throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive has no zip directory.");
  const count = archive.readUInt16LE(end + 10);
  let offset = archive.readUInt32LE(end + 16);
  const entries = new Map<string, Buffer>();
  for (let index = 0; index < count; index++) {
    if (archive.readUInt32LE(offset) !== 0x02014b50) throw new AntigravityAccountError("invalid_release", "The Antigravity ACP archive directory is malformed.");
    const method = archive.readUInt16LE(offset + 10);
    const crc = archive.readUInt32LE(offset + 16);
    const compressed = archive.readUInt32LE(offset + 20);
    const size = archive.readUInt32LE(offset + 24);
    const nameLength = archive.readUInt16LE(offset + 28);
    const extraLength = archive.readUInt16LE(offset + 30);
    const commentLength = archive.readUInt16LE(offset + 32);
    const local = archive.readUInt32LE(offset + 42);
    const name = archive.subarray(offset + 46, offset + 46 + nameLength).toString("utf8");
    offset += 46 + nameLength + extraLength + commentLength;
    if (name !== release.executable && name !== release.harness) continue;
    if (archive.readUInt32LE(local) !== 0x04034b50) throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} is malformed.`);
    const start = local + 30 + archive.readUInt16LE(local + 26) + archive.readUInt16LE(local + 28);
    const raw = archive.subarray(start, start + compressed);
    const data = method === 0 ? Buffer.from(raw) : method === 8 ? inflateRawSync(raw) : undefined;
    if (!data || data.length !== size || (crc32(data) >>> 0) !== crc) {
      throw new AntigravityAccountError("invalid_release", `The Antigravity ACP archive entry ${name} failed its integrity check.`);
    }
    entries.set(name, data);
  }
  return entries;
}

async function install(directory: string, signal: AbortSignal | undefined, progress: (message: string) => void): Promise<ReturnType<typeof layout>> {
  const present = await installed(directory);
  if (present) return present;
  const source = asset();
  progress(`Downloading Google's Antigravity ACP server ${release.version}`);
  const timeout = AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS);
  const response = await fetch(source.url, { signal: signal ? AbortSignal.any([signal, timeout]) : timeout, redirect: "error" });
  if (!response.ok) throw new AntigravityAccountError("download_failed", `Google's Antigravity ACP server download answered HTTP ${response.status}.`);
  const archive = Buffer.from(await response.arrayBuffer());
  if (archive.length !== source.archive_bytes || createHash("sha256").update(archive).digest("hex") !== source.sha256) {
    throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive does not match its pinned digest.");
  }
  progress("Verifying the Antigravity ACP server");
  const entries = zipEntries(archive);
  const executable = entries.get(release.executable);
  const harness = entries.get(release.harness);
  if (executable?.length !== source.executable_bytes || harness?.length !== source.harness_bytes) {
    throw new AntigravityAccountError("invalid_release", "Google's Antigravity ACP server archive lacks its declared executables.");
  }
  const paths = layout(directory);
  await ownedDirectory(join(directory, "acp"), true);
  const staging = await mkdtemp(join(directory, "acp", ".install-"));
  try {
    await writeFile(join(staging, release.executable), executable, { mode: 0o700, flag: "wx" });
    await writeFile(join(staging, release.harness), harness, { mode: 0o700, flag: "wx" });
    await writeFile(join(staging, "receipt.json"), `${JSON.stringify({ version: release.version, sha256: source.sha256 })}\n`, { mode: 0o600, flag: "wx" });
    await rm(paths.install, { recursive: true, force: true });
    await rename(staging, paths.install);
  } catch (error) {
    await rm(staging, { recursive: true, force: true });
    throw error;
  }
  return paths;
}

function environment(paths: ReturnType<typeof layout>, turn: string): Record<string, string> {
  return {
    GEMINI_HOME: paths.home,
    HOME: join(turn, "home"),
    TMPDIR: join(turn, "tmp"),
    ANTIGRAVITY_HARNESS_PATH: paths.harness,
    AGY_ACP_FORCE_FILE_STORAGE: "1",
    BROWSER: "/usr/bin/true",
    PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
    LANG: process.env.LANG ?? "en_US.UTF-8",
    ...Object.fromEntries(["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "SSL_CERT_FILE"].flatMap(name => process.env[name] ? [[name, process.env[name]!]] : [])),
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
    this.child = spawn(paths.executable, asset().args, {
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
      signedOut.reject(new AcpFailure("Antigravity is signed out. Connect it again in Applications.", "antigravity_not_signed_in", 401));
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

async function turnDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), "octopus-antigravity-"));
  for (const name of ["home", "tmp", "workspace"]) await mkdir(join(directory, name), { mode: 0o700 });
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

export async function antigravityAccountStatus(config: AntigravityAccountConfig): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  const paths = await installed(directory);
  if (!paths || !(await ownedDirectory(paths.home, false))) return { signedIn: false, accountLabel: null };
  const token = await lstat(join(paths.home, release.agent, "acp_token.json")).catch(() => undefined);
  if (!token?.isFile() || token.size === 0) return { signedIn: false, accountLabel: null };
  const saved = record(JSON.parse(await readFile(join(directory, "account.json"), "utf8").catch(() => "{}")));
  return { signedIn: true, accountLabel: typeof saved?.label === "string" && saved.label ? saved.label : "Google account" };
}

export async function antigravityLogin(config: AntigravityAccountConfig, options: {
  onAuth: (url: string) => void; onProgress?: (message: string) => void; signal?: AbortSignal;
}): Promise<AntigravityAccountStatus> {
  const directory = accountDirectory(config);
  await ownedDirectory(directory, true);
  const paths = await install(directory, options.signal, options.onProgress ?? (() => undefined));
  await ownedDirectory(paths.home, true);
  const turn = await turnDirectory();
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
        const paths = await installed(directory);
        if (!paths) {
          emit(failure(`Connect Antigravity in Applications to install Google's ACP server ${release.version} and use the connected account.`, "antigravity_not_connected", 401));
          return;
        }
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
        emit(failure(error instanceof Error ? error.message : String(error), "antigravity_turn_failed", 502));
      }
    },
  };
}
