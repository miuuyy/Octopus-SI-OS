import { expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { event, protocolJsonSchema } from "../src/protocol.ts";

const root = path.resolve(import.meta.dir, "..");
const child = path.join(root, "bin", "octopus-claude-sdk-child");

function fakeClaude(account: string): { dir: string; executable: string } {
  const dir = mkdtempSync(path.join(tmpdir(), "octopus-claude-sdk-child-"));
  const executable = path.join(dir, "claude");
  writeFileSync(executable, `#!/bin/sh
case "$1" in
  --version) echo "2.1.310 (Claude Code)" ;;
  auth) echo '${account}'; case '${account}' in *'"loggedIn":false'*) exit 1 ;; esac ;;
  *) exit 70 ;;
esac
`);
  chmodSync(executable, 0o755);
  return { dir, executable };
}

async function converse(executable: string, commands: string[]): Promise<{ frames: Record<string, unknown>[]; code: number | null }> {
  const process = spawn(child, ["--claude-executable", executable], { stdio: ["pipe", "pipe", "pipe"], env: { PATH: "/usr/bin:/bin" } });
  let stdout = "";
  process.stdout.on("data", (chunk) => { stdout += chunk; });
  for (const line of commands) process.stdin.write(`${line}\n`);
  process.stdin.end();
  const code = await new Promise<number | null>((resolve) => process.on("exit", resolve));
  const frames = stdout.trim().split("\n").map((line) => JSON.parse(line));
  for (const frame of frames) event.parse(frame);
  return { frames, code };
}

test("the committed protocol schema is generated from the protocol module", () => {
  expect(JSON.parse(readFileSync(path.join(root, "protocol.schema.json"), "utf8"))).toEqual(protocolJsonSchema());
});

test("doctor reports Claude Code's own account state and fails closed without an executable", () => {
  const fake = fakeClaude('{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}');
  try {
    const ready = spawnSync(child, ["--doctor", "--claude-executable", fake.executable], { encoding: "utf8" });
    expect(ready.status).toBe(0);
    expect(JSON.parse(ready.stdout)).toEqual({
      protocol: "octopus.claude_sdk_child/1",
      sdk_version: "0.3.296",
      claude_version: "2.1.310",
      account: { logged_in: false, auth_method: "none", api_provider: "firstParty", subscription_type: null },
    });
    const missing = spawnSync(child, ["--doctor", "--claude-executable", path.join(fake.dir, "absent")], { encoding: "utf8" });
    expect(missing.status).toBe(1);
    expect(JSON.parse(missing.stdout).error.code).toBe("claude_executable_missing");
  } finally {
    rmSync(fake.dir, { recursive: true, force: true });
  }
});

test("every command is answered once and a malformed frame ends the child", async () => {
  const fake = fakeClaude('{"loggedIn":true}');
  try {
    const config = { model: "claude-haiku-4-5", system_prompt: "", cwd: fake.dir, resume: null };
    const { frames, code } = await converse(fake.executable, [
      JSON.stringify({ type: "turn", request_id: 1, session: "s", turn: "t", input: [{ type: "text", text: "hi" }] }),
      JSON.stringify({ type: "start", request_id: 2, session: "s", config: { ...config, tools: [{ name: "read_file", description: "d", input_schema: { type: "string" } }] } }),
      JSON.stringify({ type: "tool_result", request_id: 3, session: "s", turn: "t", call_id: "c", content: [], is_error: false }),
      JSON.stringify({ type: "close", request_id: 4, session: "s", extra: true }),
      JSON.stringify({ type: "close", request_id: 5, session: "s" }),
    ]);
    expect(code).toBe(2);
    expect(frames.map((frame) => [frame.type, frame.request_id ?? null, frame.code ?? null, frame.fatal ?? null])).toEqual([
      ["ready", null, null, null],
      ["error", 1, "unknown_session", false],
      ["error", 2, "invalid_tools", false],
      ["error", 3, "unknown_session", false],
      ["error", null, "invalid_command", true],
    ]);
  } finally {
    rmSync(fake.dir, { recursive: true, force: true });
  }
});
