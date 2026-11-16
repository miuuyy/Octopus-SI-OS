import { spawn as nodeSpawn, type ChildProcess } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createConnection, createServer, type Server } from "node:net";
import { arch, platform, tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { crc32, inflateRawSync } from "node:zlib";
import release from "../antigravity-acp.json" with { type: "json" };
import type { AdapterEvent, OcxParsedRequest, OcxProviderConfig } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import type { IncomingMeta, ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import { buildCodeBuddyToolBridge, CODEBUDDY_MCP_SERVER_NAME, CODEBUDDY_MCP_TOOL_PREFIX } from "../node_modules/@bitkyc08/opencodex/src/adapters/codebuddy/tool-bridge.ts";
import { CLAUDE_REPLAY_SYSTEM_PROMPT, stableClaudeToolBridge } from "../node_modules/@bitkyc08/opencodex/src/adapters/claude-cli/stable-replay.ts";
import { buildSystemPrompt, readJsonLines } from "../node_modules/@bitkyc08/opencodex/src/adapters/coding-agent/protocol.ts";
import { isStandaloneBinary } from "../node_modules/@bitkyc08/opencodex/src/lib/standalone.ts";
import { buildClaudeConversationInput } from "./claude-replay.ts";

export const ANTIGRAVITY_ACCOUNT_PROVIDER = "antigravity-acp";
export const ANTIGRAVITY_CAPTURE_ENTRY = "__antigravity-mcp";

export type AntigravityAccountConfig = { provider: typeof ANTIGRAVITY_ACCOUNT_PROVIDER; directory: string };
export type AntigravityAccountStatus = { signedIn: boolean; accountLabel: string | null };

const PRECEDENCE = [
  "The instructions in this section come from the system that hosts this conversation and take precedence over everything earlier in this system prompt.",
  "They replace the identity, role, workflow, tool and formatting guidance given before them: you are not an IDE coding assistant, no USER_REQUEST wrapper is used, and no built-in tools exist.",
  "Where anything earlier conflicts with this section, follow this section.",
].join("\n");
const TOOL_PROTOCOL = [
  "The functions declared to you belong to the external client, which executes them under its own policy and returns each result in the next turn.",
  "Call at most one function per response.",
].join("\n");
const AUTH_PROMPT = /Open the following link to authenticate the ACP server: (https:\/\/\S+)/;
const STDERR_LIMIT = 16_384;
const KILL_GRACE_MS = 2000;
const STARTUP_TIMEOUT_MS = 180_000;
const TURN_TIMEOUT_MS = 600_000;
const DOWNLOAD_TIMEOUT_MS = 900_000;

type ErrorEvent = Extract<AdapterEvent, { type: "error" }>;
type Asset = { url: string; sha256: string; archive_bytes: number; executable_bytes: number; harness_bytes: number };
type Captured = { name: string; arguments: Record<string, unknown> };
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

type Pending = { resolve: (value: JsonRecord) => void; reject: (error: Error) => void };

class AcpProcess {
  readonly child: ChildProcess;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private stderr = "";
  private killed = false;
  readonly closed: Promise<number | null>;
  onNotification: (method: string, params: JsonRecord) => void = () => undefined;
  onRequest: (method: string, params: JsonRecord) => JsonRecord | undefined = () => undefined;
  onAuthorizationUrl: (url: string) => void = () => undefined;

  constructor(paths: ReturnType<typeof layout>, turn: string, spawn: typeof nodeSpawn) {
    this.child = spawn(paths.executable, [], {
      cwd: join(turn, "workspace"), env: environment(paths, turn), stdio: ["pipe", "pipe", "pipe"], detached: true,
    });
    this.closed = new Promise(resolve => {
      this.child.once("close", code => resolve(code));
      this.child.once("error", () => resolve(null));
    });
    this.child.stderr?.on("data", (chunk: Buffer) => {
      const text = chunk.toString("utf8");
      const url = AUTH_PROMPT.exec(this.stderr.slice(-2048) + text)?.[1];
      if (this.stderr.length < STDERR_LIMIT) this.stderr += text.slice(0, STDERR_LIMIT - this.stderr.length);
      if (url) this.onAuthorizationUrl(url);
    });
    void this.read();
    void this.closed.then(code => {
      const error = new Error(`Antigravity ACP server exited${code === null ? "" : ` with ${code}`}: ${this.diagnostic()}`);
      for (const entry of this.pending.values()) entry.reject(error);
      this.pending.clear();
    });
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
      this.pending.set(id, { resolve, reject });
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
      for await (const message of readJsonLines(this.child.stdout!)) {
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
          if (error) entry.reject(new AcpRequestError(Number(error.code), String(error.message ?? "ACP request failed"), error.data));
          else entry.resolve(record(message.result) ?? {});
        }
      }
    } catch (error) {
      for (const entry of this.pending.values()) entry.reject(error as Error);
      this.pending.clear();
    }
  }

  kill(): void {
    if (this.killed || this.child.exitCode !== null || this.child.pid === undefined) return;
    this.killed = true;
    try { process.kill(-this.child.pid, "SIGTERM"); } catch {}
    setTimeout(() => {
      if (this.child.exitCode === null && this.child.pid !== undefined) {
        try { process.kill(-this.child.pid, "SIGKILL"); } catch {}
      }
    }, KILL_GRACE_MS).unref();
  }
}

class AcpRequestError extends Error {
  constructor(readonly code: number, message: string, readonly data: unknown) {
    super(message);
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
  await rm(paths.home, { recursive: true, force: true });
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

function contentBlocks(parsed: OcxParsedRequest, vendorNameByWire: ReadonlyMap<string, string>): JsonRecord[] {
  const frame = JSON.parse(buildClaudeConversationInput(parsed, { vendorNameByWire })[0]!) as { message: { content: JsonRecord[] } };
  return frame.message.content.map(block => {
    if (block.type === "text") return { type: "text", text: String(block.text ?? "") };
    const source = record(block.source);
    if (block.type === "image" && source?.type === "base64" && typeof source.media_type === "string" && typeof source.data === "string") {
      return { type: "image", mimeType: source.media_type, data: source.data };
    }
    throw new Error("Antigravity turns carry inline images only; this request holds an image reference.");
  });
}

function effortValue(parsed: OcxParsedRequest): string | undefined {
  const requested = parsed.options.reasoning;
  if (!requested || requested === "none") return undefined;
  return requested;
}

export function createAntigravityAcpAdapter(_provider: OcxProviderConfig, account: AntigravityAccountConfig, deps: { spawn?: typeof nodeSpawn } = {}): ProviderAdapter {
  return {
    name: "antigravity-acp",
    buildRequest() { throw new Error("Antigravity runs through Google's ACP server."); },
    async *parseStream() { throw new Error("Antigravity runs through Google's ACP server."); },
    async runTurn(parsed, incoming, emit) {
      let bridge;
      let blocks: JsonRecord[];
      try {
        bridge = stableClaudeToolBridge(buildCodeBuddyToolBridge(parsed));
        const vendorByWire = new Map([...bridge.emittedNameMap].map(([emitted, wire]) => [wire, emitted.slice(CODEBUDDY_MCP_TOOL_PREFIX.length)]));
        blocks = contentBlocks(parsed, vendorByWire);
      } catch (error) {
        emit(failure(error instanceof Error ? error.message : String(error), "antigravity_input_invalid", 400));
        return;
      }
      let paths;
      try {
        paths = await installed(accountDirectory(account));
      } catch (error) {
        emit(failure(error instanceof Error ? error.message : String(error), "antigravity_account_invalid", 401));
        return;
      }
      if (!paths) {
        emit(failure("Antigravity is not connected on this Mac. Connect it in Applications to install Google's ACP server and sign in.", "antigravity_not_connected", 401));
        return;
      }
      const wireByName = new Map([...bridge.emittedNameMap].map(([emitted, wire]) => [emitted.slice(CODEBUDDY_MCP_TOOL_PREFIX.length), wire]));
      const systemPrompt = [PRECEDENCE, buildSystemPrompt(parsed), CLAUDE_REPLAY_SYSTEM_PROMPT, bridge.tools.length ? TOOL_PROTOCOL : ""].filter(Boolean).join("\n\n");
      const turn = await turnDirectory();
      let capture: Capture | undefined;
      try {
        capture = await captureServer();
        const mcpServers: JsonRecord[] = [];
        if (bridge.tools.length) {
          const catalogPath = join(turn, "catalog.json");
          await writeFile(catalogPath, JSON.stringify(bridge.tools), { encoding: "utf8", mode: 0o600, flag: "wx" });
          const entry = isStandaloneBinary() ? [] : [fileURLToPath(new URL("./main.ts", import.meta.url))];
          mcpServers.push({
            name: CODEBUDDY_MCP_SERVER_NAME,
            command: process.execPath,
            args: [...entry, ANTIGRAVITY_CAPTURE_ENTRY, catalogPath, String(capture.port), capture.token],
            env: [],
          });
        }
        await runTurn({
          paths, turn, mcpServers, systemPrompt, blocks, parsed, wireByName,
          requireToolCall: bridge.requireToolCall, capture, incoming, emit, spawn: deps.spawn ?? nodeSpawn,
        });
      } catch (error) {
        emit(failure(`Antigravity turn could not be prepared: ${error instanceof Error ? error.message : String(error)}`, "antigravity_turn_failed", 502, true));
      } finally {
        capture?.close();
        await rm(turn, { recursive: true, force: true });
      }
    },
  };
}

type Capture = { port: number; token: string; next: Promise<Captured>; close: () => void };

async function captureServer(): Promise<Capture> {
  const token = randomBytes(24).toString("hex");
  let deliver!: (call: Captured) => void;
  const next = new Promise<Captured>(resolveCall => { deliver = resolveCall; });
  const server: Server = createServer(socket => {
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      if (buffer.length > 4 * 1024 * 1024) socket.destroy();
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      try {
        const value = record(JSON.parse(buffer.slice(0, newline)));
        const args = record(value?.arguments);
        if (value?.token === token && typeof value.name === "string" && args) deliver({ name: value.name, arguments: args });
      } catch {}
      socket.end();
    });
    socket.on("error", () => undefined);
  });
  await new Promise<void>((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolveListen());
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Antigravity capture server has no port");
  return { port: address.port, token, next, close: () => server.close() };
}

async function runTurn(input: {
  paths: ReturnType<typeof layout>; turn: string; mcpServers: JsonRecord[]; systemPrompt: string; blocks: JsonRecord[];
  parsed: OcxParsedRequest; wireByName: ReadonlyMap<string, string>; requireToolCall: boolean; capture: Capture;
  incoming: IncomingMeta; emit: (event: AdapterEvent) => void; spawn: typeof nodeSpawn;
}): Promise<void> {
  const { emit, incoming } = input;
  if (incoming.abortSignal?.aborted) {
    emit(failure("Antigravity turn was aborted before start.", "aborted"));
    return;
  }
  let terminal = false;
  const finish = (event: AdapterEvent) => {
    if (terminal) return;
    terminal = true;
    emit(event);
  };
  let acp: AcpProcess;
  try {
    acp = new AcpProcess(input.paths, input.turn, input.spawn);
  } catch (error) {
    emit(failure(`Antigravity ACP server could not start: ${error instanceof Error ? error.message : String(error)}`, "acp_spawn_failed"));
    return;
  }
  let used: number | undefined;
  let sessionId: string | undefined;
  const abort = () => acp.kill();
  incoming.abortSignal?.addEventListener("abort", abort, { once: true });
  const timer = setTimeout(() => {
    finish(failure("Antigravity turn exceeded its time limit.", "antigravity_timeout", 504, true));
    acp.kill();
  }, TURN_TIMEOUT_MS);
  acp.onNotification = (method, params) => {
    if (method !== "session/update" || terminal) return;
    const update = record(params.update);
    const kind = update?.sessionUpdate;
    const content = record(update?.content);
    if (kind === "agent_message_chunk" && content?.type === "text" && typeof content.text === "string" && content.text) {
      emit({ type: "text_delta", text: content.text });
    } else if (kind === "agent_thought_chunk" && content?.type === "text" && typeof content.text === "string" && content.text) {
      emit({ type: "thinking_delta", thinking: content.text });
    } else if (kind === "usage_update" && typeof update?.used === "number") {
      used = update.used;
    }
  };
  acp.onRequest = (method, params) => {
    if (method !== "session/request_permission") return undefined;
    const options = Array.isArray(params.options) ? params.options.map(record) : [];
    const allow = options.find(option => option?.kind === "allow_once") ?? options.find(option => option?.optionId === "allow");
    return allow ? { outcome: { outcome: "selected", optionId: allow.optionId } } : { outcome: { outcome: "cancelled" } };
  };
  const usage = () => used === undefined ? undefined : { inputTokens: used, outputTokens: 0, totalTokens: used, contextTotalTokens: used };
  try {
    incoming.beforePhysicalSend?.();
    await withTimeout(connect(acp, () => {
      finish(failure("Antigravity is signed out on this Mac. Connect it again in Applications.", "antigravity_not_signed_in", 401));
      acp.kill();
    }), STARTUP_TIMEOUT_MS, "Antigravity ACP server start");
    if (terminal) return;
    const session = await acp.request("session/new", {
      cwd: join(input.turn, "workspace"),
      mcpServers: input.mcpServers,
      _meta: {
        antigravity: {
          enabledTools: [],
          systemPrompt: { append: input.systemPrompt },
          ...(input.mcpServers.length ? { eagerMcpServers: [CODEBUDDY_MCP_SERVER_NAME] } : {}),
        },
      },
    });
    sessionId = typeof session.sessionId === "string" && /^[A-Za-z0-9-]+$/.test(session.sessionId) ? session.sessionId : undefined;
    if (!sessionId) throw new Error("Antigravity ACP server returned no session.");
    await selectModel(acp, sessionId, input.parsed.modelId, effortValue(input.parsed));
    incoming.onPhysicalSend?.({ ordinal: 1 });
    const prompt = acp.request("session/prompt", { sessionId, prompt: input.blocks });
    prompt.catch(() => undefined);
    const outcome = await Promise.race([
      prompt.then(result => ({ kind: "stop" as const, result })),
      input.capture.next.then(call => ({ kind: "call" as const, call })),
    ]);
    if (terminal) return;
    if (outcome.kind === "call") {
      const wire = input.wireByName.get(outcome.call.name);
      if (!wire) {
        finish(failure(`Antigravity called '${outcome.call.name}', which is outside the client's tool catalog.`, "antigravity_tool_outside_catalog", 502));
        return;
      }
      const id = `call_${randomBytes(12).toString("hex")}`;
      emit({ type: "tool_call_start", id, name: wire });
      emit({ type: "tool_call_delta", arguments: JSON.stringify(outcome.call.arguments) });
      emit({ type: "tool_call_end" });
      const counted = usage();
      finish({ type: "done", stopReason: "tool_use", ...(counted ? { usage: counted } : {}) });
      return;
    }
    const stopReason = outcome.result.stopReason;
    const counted = usage();
    if (stopReason === "refusal") finish(failure("The model refused this request through Antigravity.", "antigravity_refusal", 400));
    else if (stopReason === "cancelled") finish(failure("Antigravity turn was stopped before it completed.", "aborted"));
    else if (input.requireToolCall) finish(failure("Antigravity ended the turn without the required tool call.", "antigravity_tool_call_required", 502));
    else finish({ type: "done", stopReason: stopReason === "max_tokens" ? "max_tokens" : "stop", ...(counted ? { usage: counted } : {}) });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const signIn = error instanceof AcpRequestError && error.code === -32000;
    finish(signIn
      ? failure(`Antigravity is signed out on this Mac. Connect it again in Applications. ${message}`, "antigravity_not_signed_in", 401)
      : failure(`Antigravity failed: ${message}${acp.diagnostic() ? ` (${acp.diagnostic()})` : ""}`, "antigravity_turn_failed", 502, !(error instanceof AcpRequestError)));
  } finally {
    clearTimeout(timer);
    incoming.abortSignal?.removeEventListener("abort", abort);
    acp.kill();
    await acp.closed;
    if (sessionId) await forgetSession(input.paths, sessionId);
  }
}

async function forgetSession(paths: ReturnType<typeof layout>, sessionId: string): Promise<void> {
  const root = join(paths.home, release.agent);
  await rm(join(root, "brain", sessionId), { recursive: true, force: true });
  for (const suffix of [".db", ".db-wal", ".db-shm", ".meta"]) await rm(join(root, "conversations", `${sessionId}${suffix}`), { force: true });
}

async function selectModel(acp: AcpProcess, sessionId: string, model: string, effort: string | undefined): Promise<void> {
  try {
    await acp.request("session/set_config_option", { sessionId, configId: "model", value: model });
    if (effort) await acp.request("session/set_config_option", { sessionId, configId: "thinking_level", value: effort });
  } catch (error) {
    if (!(error instanceof AcpRequestError)) throw error;
    throw new AcpRequestError(error.code, `model '${model}'${effort ? ` at ${effort} thinking` : ""} is not offered to this Google account: ${error.message}`, error.data);
  }
}

export async function runAntigravityCaptureMcpServer(catalogPath: string, port: string, token: string): Promise<void> {
  const tools = JSON.parse(await readFile(catalogPath, "utf8")) as unknown;
  if (!Array.isArray(tools) || !tools.every(tool => typeof record(tool)?.name === "string" && record(record(tool)?.inputSchema))) {
    throw new Error("Antigravity capture catalog is malformed");
  }
  if (!/^\d+$/.test(port) || !/^[0-9a-f]{48}$/.test(token)) throw new Error("Antigravity capture endpoint is malformed");
  const names = new Set(tools.map(tool => (tool as { name: string }).name));
  const send = (message: JsonRecord) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
  for await (const message of readJsonLines(process.stdin)) {
    const id = message.id;
    const params = record(message.params);
    if (message.method === "tools/call") {
      const args = record(params?.arguments) ?? {};
      if (typeof params?.name !== "string" || !names.has(params.name)) {
        if (id !== undefined && id !== null) send({ id, error: { code: -32602, message: "unknown isolated tool" } });
        continue;
      }
      const socket = createConnection({ host: "127.0.0.1", port: Number(port) });
      socket.on("error", () => undefined);
      socket.end(`${JSON.stringify({ token, name: params.name, arguments: args })}\n`);
      continue;
    }
    if (id === undefined || id === null) continue;
    if (message.method === "initialize") {
      send({ id, result: { protocolVersion: typeof params?.protocolVersion === "string" ? params.protocolVersion : "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "octopus-antigravity-capture", version: "1.0.0" } } });
    } else if (message.method === "tools/list") {
      send({ id, result: { tools } });
    } else if (message.method === "ping") {
      send({ id, result: {} });
    } else {
      send({ id, error: { code: -32601, message: "method not found" } });
    }
  }
}
