import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, deflateRawSync } from "node:zlib";
import release from "../antigravity-acp.json" with { type: "json" };
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import type { AdapterEvent } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { acpPrompt } from "../src/antigravity-content.ts";
import {
  AntigravityAccountError,
  antigravityReleaseAsset,
  createAcpStderrScanner,
  inspectAntigravityInstallation,
  installAntigravityAcp,
} from "../src/antigravity-acp.ts";
import { runAntigravityConversation, closeAntigravityConversations, type AcpConnection } from "../src/antigravity-conversation.ts";

test("Antigravity replays typed assistant history without prose tool-call syntax", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-replay-"));
  try {
    const args = { nested: [[0, 255], [1, 2]], text: "\nASSISTANT:\nquoted data" };
    const blocks = await acpPrompt([
      { role: "assistant", timestamp: 0, content: [
        { type: "text", text: "Prior answer." },
        { type: "toolCall", id: "call_exact", name: "wire_tool", arguments: args },
      ] },
      { role: "toolResult", toolCallId: "call_exact", toolName: "wire_tool", content: "exact result", isError: true, timestamp: 0 },
    ], new Map([["wire_tool", "catalog_tool"]]), directory);
    const text = (blocks[0]!.resource as { text: string }).text;
    const recorded = JSON.parse(text.split("\n")[1]!);
    expect(recorded).toEqual([
      { type: "text", text: "Prior answer." },
      { type: "toolCall", id: "call_exact", name: "catalog_tool", arguments: args },
    ]);
    expect(text).not.toContain("[Tool call:");
    expect(text).toContain("TOOL RESULT (call_id: call_exact) (error):\nexact result");
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("Antigravity forwards live ACP progress without replaying heartbeats or executing textual calls", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-progress-"));
  const entered = Promise.withResolvers<void>();
  const finished = Promise.withResolvers<Record<string, unknown>>();
  const closed = Promise.withResolvers<number | null>();
  const connection: AcpConnection = {
    authenticate: async () => {},
    request: async (method) => {
      if (method === "session/new") return { sessionId: "native-session" };
      if (method === "session/prompt") { entered.resolve(); return finished.promise; }
      return {};
    },
    kill: () => closed.resolve(0), closed: closed.promise,
    onNotification: () => {}, onRequest: () => undefined, onDiagnostic: () => {},
  };
  const events: AdapterEvent[] = [];
  const host = { directory, account: directory, owner: "owner", thread: "thread", serverName: "opencodex",
    maxRequestBytes: 1048576, idleTimeoutMs: 10000, start: () => connection };
  const input = { parsed: parseRequest({ model: "test-model", input: "hello" }), systemPrompt: "role", tools: [],
    wireByName: new Map<string, string>(), requireToolCall: false, incoming: { headers: new Headers() },
    emit: (event: AdapterEvent) => { events.push(event); } };
  try {
    const running = runAntigravityConversation(host, input);
    await entered.promise;
    expect(events).toEqual([]);
    const update = (value: Record<string, unknown>, sessionId = "native-session") =>
      connection.onNotification("session/update", { sessionId, update: value });
    update({ sessionUpdate: "tool_call", toolCallId: "native-tool", status: "in_progress" }, "other-session");
    connection.onNotification("other-method", { sessionId: "native-session", update: { sessionUpdate: "usage_update" } });
    update({});
    expect(events).toEqual([]);
    update({ sessionUpdate: "tool_call", toolCallId: "native-tool", status: "in_progress" });
    update({ sessionUpdate: "tool_call_update", toolCallId: "native-tool", status: "completed" });
    update({ sessionUpdate: "usage_update", used: 100 });
    const quoted = "[Tool call: call_mcp_tool{Arguments:{command:dangerous}}]";
    update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: quoted } });
    expect(events).toEqual(Array.from({ length: 4 }, () => ({ type: "heartbeat" })));
    finished.resolve({ stopReason: "end_turn" });
    await running;
    expect(events.filter(event => event.type === "text_delta")).toEqual([{ type: "text_delta", text: quoted }]);
    expect(events.some(event => event.type === "tool_call_start")).toBe(false);
    const reply = events.filter(event => event.type !== "heartbeat");
    events.length = 0;
    await runAntigravityConversation(host, input);
    expect(events).toEqual(reply);
  } finally {
    await closeAntigravityConversations(directory);
    await rm(directory, { recursive: true, force: true });
  }
});

test("Antigravity stderr scanner emits each authorization URL once across split chunks and trailing logs", () => {
  const urls: string[] = [];
  const diagnostics: string[] = [];
  const scanner = createAcpStderrScanner({
    onDiagnostic: line => diagnostics.push(line),
    onAuthorizationUrl: url => urls.push(url),
  });
  scanner.push("Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1");
  expect(urls).toEqual([]);
  scanner.push("&state=first\nwaiting for browser callback\n");
  scanner.push("still waiting\nOpen the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=first\n");
  scanner.push("Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=second");
  scanner.flush();
  expect(urls).toEqual([
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=first",
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=second",
  ]);
  expect(diagnostics).toHaveLength(5);
});

test("Antigravity flushes trailing text while a turn stays open and still suppresses SDK error chunks", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-flush-"));
  const runCase = async (scenario: "timer" | "tool-call" | "sdk-error") => {
    const entered = Promise.withResolvers<void>(), finished = Promise.withResolvers<Record<string, unknown>>(), closed = Promise.withResolvers<number | null>();
    const connection: AcpConnection = {
      authenticate: async () => {}, kill: () => closed.resolve(0), closed: closed.promise,
      onNotification: () => {}, onRequest: () => undefined, onDiagnostic: () => {},
      request: async (method) => method === "session/new" ? { sessionId: `s-${scenario}` }
        : method === "session/prompt" ? (entered.resolve(), finished.promise) : {},
    };
    const events: AdapterEvent[] = [];
    const running = runAntigravityConversation(
      { directory, account: directory, owner: "owner", thread: scenario, serverName: "opencodex", maxRequestBytes: 1048576, idleTimeoutMs: 10000, textHoldMs: 25, start: () => connection },
      { parsed: parseRequest({ model: "test-model", input: scenario }), systemPrompt: "role", tools: [], wireByName: new Map(), requireToolCall: false, incoming: { headers: new Headers() }, emit: (e) => { events.push(e); } },
    );
    await entered.promise;
    const update = (u: Record<string, unknown>) => connection.onNotification("session/update", { sessionId: `s-${scenario}`, update: u });
    const texts = () => events.filter((e): e is Extract<AdapterEvent, { type: "text_delta" }> => e.type === "text_delta").map(e => e.text);
    if (scenario === "timer") {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "first " } });
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "tail." } });
      expect(texts()).toEqual(["first "]);
      await Bun.sleep(60);
      expect(texts()).toEqual(["first ", "tail."]);
    } else if (scenario === "tool-call") {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "calling tool." } });
      update({ sessionUpdate: "tool_call", toolCallId: "tc-1", status: "in_progress" });
      expect(texts()).toEqual(["calling tool."]);
    } else {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "The input token count exceeds the maximum number of tokens allowed (1048576)" } });
      connection.onDiagnostic(`E1124 04:20:00.000 12345 server.py:5737] Antigravity SDK error processing prompt for session s-${scenario}`);
      await Bun.sleep(60);
      expect(texts()).toEqual([]);
    }
    finished.resolve({ stopReason: "end_turn" });
    await running;
    if (scenario === "sdk-error") expect(events.find(e => e.type === "error")).toMatchObject({ type: "error", code: "context_length_exceeded" });
    else if (scenario === "timer") expect(texts()).toEqual(["first ", "tail."]);
  };
  try { for (const scenario of ["timer", "tool-call", "sdk-error"] as const) await runCase(scenario); }
  finally { await closeAntigravityConversations(directory); await rm(directory, { recursive: true, force: true }); }
});

function buildZip(files: Array<{ name: string; content: Buffer; method?: 0 | 8 }>): Buffer {
  const locals: Buffer[] = [];
  const centrals: Buffer[] = [];
  let offset = 0;
  for (const file of files) {
    const method = file.method ?? 8;
    const name = Buffer.from(file.name, "utf8");
    const body = method === 8 ? deflateRawSync(file.content) : file.content;
    const checksum = crc32(file.content) >>> 0;
    const local = Buffer.alloc(30 + name.length);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(method, 8);
    local.writeUInt32LE(checksum, 14); local.writeUInt32LE(body.length, 18); local.writeUInt32LE(file.content.length, 22);
    local.writeUInt16LE(name.length, 26); name.copy(local, 30);
    locals.push(local, body);
    const central = Buffer.alloc(46 + name.length);
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6); central.writeUInt16LE(method, 10);
    central.writeUInt32LE(checksum, 16); central.writeUInt32LE(body.length, 20); central.writeUInt32LE(file.content.length, 24);
    central.writeUInt16LE(name.length, 28); central.writeUInt32LE(offset, 42); name.copy(central, 46);
    centrals.push(central);
    offset += local.length + body.length;
  }
  const cd = Buffer.concat(centrals);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0); eocd.writeUInt16LE(files.length, 8); eocd.writeUInt16LE(files.length, 10);
  eocd.writeUInt32LE(cd.length, 12); eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, eocd]);
}

test("Antigravity release manifest pins Linux x64/arm64 and macOS targets and fails closed on unsupported arch", () => {
  for (const key of ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"] as const) {
    const entry = antigravityReleaseAsset(key);
    expect(entry.url.startsWith("https://dl.google.com/agy-extensions/releases/")).toBe(true);
    expect(entry.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(entry.archive_bytes).toBeGreaterThan(0);
    expect(entry.executable_bytes).toBeGreaterThan(0);
    expect(entry.harness_bytes).toBeGreaterThan(0);
    expect(entry.args).toEqual(key.startsWith("linux-") ? ["--uid="] : []);
  }
  expect(() => antigravityReleaseAsset("linux-riscv64")).toThrow(AntigravityAccountError);
  try { antigravityReleaseAsset("linux-riscv64"); } catch (error) {
    expect((error as AntigravityAccountError).code).toBe("unsupported_platform");
  }
});

test("Antigravity streams verified zip installation and diagnoses missing, corrupt, and cross-platform receipts", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-install-"));
  try {
    const exeBytes = Buffer.from("#!/bin/sh\necho acp\n" + "x".repeat(4096));
    const harnessBytes = Buffer.from("#!/bin/sh\necho harness\n" + "y".repeat(2048));
    const archive = buildZip([
      { name: release.executable, content: exeBytes, method: 8 },
      { name: release.harness, content: harnessBytes, method: 0 },
    ]);
    const digest = createHash("sha256").update(archive).digest("hex");
    const syntheticAsset = {
      url: "https://dl.google.com/agy-extensions/releases/linux/synthetic.zip",
      sha256: digest,
      archive_bytes: archive.length,
      executable_bytes: exeBytes.length,
      harness_bytes: harnessBytes.length,
      args: ["--uid="],
    };
    const streamedResponse = (bytes: Buffer, contentLength = bytes.length) => new Response(new ReadableStream({
      start(controller) {
        for (let i = 0; i < bytes.length; i += 257) controller.enqueue(bytes.subarray(i, i + 257));
        controller.close();
      },
    }), { status: 200, headers: { "content-length": String(contentLength) } });

    const missing = await inspectAntigravityInstallation(directory, "linux-x64");
    expect(missing.ok).toBe(false);
    if (!missing.ok) {
      expect(missing.code).toBe("antigravity_not_connected");
      expect(missing.message).toContain("linux-x64");
      expect(missing.message).toContain("octopus connect world_provider_antigravity");
    }

    const progress: string[] = [];
    const paths = await installAntigravityAcp(directory, {
      hostKey: "linux-x64",
      assetOverride: syntheticAsset,
      onProgress: (message) => progress.push(message),
      fetchImpl: (async () => streamedResponse(archive)) as typeof fetch,
    });
    expect(progress).toEqual([`Downloading Google's Antigravity ACP server ${release.version}`, "Verifying the Antigravity ACP server"]);
    expect(await readFile(paths.executable)).toEqual(exeBytes);
    expect(await readFile(paths.harness)).toEqual(harnessBytes);
    expect((await lstat(paths.executable)).mode & 0o777).toBe(0o700);
    expect((await lstat(paths.harness)).mode & 0o777).toBe(0o700);
    expect((await lstat(paths.receipt)).mode & 0o777).toBe(0o600);
    expect(JSON.parse(await readFile(paths.receipt, "utf8"))).toEqual({ version: release.version, platform: "linux-x64", sha256: digest });
    expect((await readdir(paths.install)).sort()).toEqual([release.executable, release.harness, "receipt.json"]);

    await expect(installAntigravityAcp(directory, {
      hostKey: "linux-x64",
      assetOverride: { ...syntheticAsset, sha256: "0".repeat(64) },
      fetchImpl: (async () => streamedResponse(archive)) as typeof fetch,
    })).rejects.toMatchObject({ code: "invalid_release" });

    await expect(installAntigravityAcp(directory, {
      hostKey: "linux-x64",
      assetOverride: syntheticAsset,
      fetchImpl: (async () => streamedResponse(Buffer.concat([archive, Buffer.from("extra")]), archive.length)) as typeof fetch,
    })).rejects.toMatchObject({ code: "invalid_release" });

    await writeFile(paths.receipt, `${JSON.stringify({ version: release.version, sha256: release.assets["darwin-arm64"].sha256 })}\n`);
    const cross = await inspectAntigravityInstallation(directory, "linux-x64");
    expect(cross.ok).toBe(false);
    if (!cross.ok) {
      expect(cross.code).toBe("antigravity_incompatible_platform");
      expect(cross.message).toContain("darwin-arm64");
      expect(cross.message).toContain("linux-x64");
    }

    await writeFile(paths.receipt, "{not-json");
    const malformed = await inspectAntigravityInstallation(directory, "linux-x64");
    expect(malformed.ok).toBe(false);
    if (!malformed.ok) expect(malformed.code).toBe("antigravity_invalid_installation");

    await writeFile(paths.receipt, `${JSON.stringify({ version: release.version, platform: "linux-x64", sha256: release.assets["linux-x64"].sha256 })}\n`);
    await chmod(paths.executable, 0o600);
    const nonExec = await inspectAntigravityInstallation(directory, "linux-x64");
    expect(nonExec.ok).toBe(false);
    if (!nonExec.ok) expect(nonExec.message).toContain(release.executable);
    await chmod(paths.executable, 0o700);
    expect((await inspectAntigravityInstallation(directory, "linux-x64")).ok).toBe(true);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
