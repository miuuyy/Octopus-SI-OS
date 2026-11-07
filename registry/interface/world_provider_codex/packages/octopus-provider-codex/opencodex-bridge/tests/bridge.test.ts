import { afterEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

type SpawnedBridge = {
  child: ReturnType<typeof Bun.spawn>;
  origin: string;
};

const children: Array<ReturnType<typeof Bun.spawn>> = [];
const servers: Array<ReturnType<typeof Bun.serve>> = [];
const directories: string[] = [];

afterEach(async () => {
  for (const child of children.splice(0)) {
    child.kill();
    await child.exited;
  }
  for (const server of servers.splice(0)) server.stop(true);
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function config(baseUrl: string, provider: Record<string, unknown> = {}) {
  return {
    schema_version: 1,
    interface_id: "world_provider_test_exact",
    provider: {
      id: "test-provider",
      adapter: "openai-chat",
      base_url: baseUrl,
      models: ["test-model"],
      default_model: "test-model",
      api_key: "test-secret",
      auth_header: "Authorization",
      auth_value_prefix: "Bearer ",
      headers: { "X-Octopus-Test": "exact" },
      allow_private_network: true,
      options: {},
      ...provider,
    },
    limits: {
      max_request_bytes: 1048576,
      max_response_bytes: 1048576,
      max_translated_call_bytes: 262144,
      max_translated_turn_bytes: 1048576,
      request_timeout_ms: 10000,
    },
  };
}

async function readLine(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  for (;;) {
    const next = await reader.read();
    if (next.done) throw new Error("bridge exited before readiness");
    text += decoder.decode(next.value, { stream: true });
    const newline = text.indexOf("\n");
    if (newline >= 0) {
      reader.releaseLock();
      return text.slice(0, newline);
    }
  }
}

const bridgeBinary = process.env.OCTOPUS_TEST_BRIDGE_BINARY ?? resolve(import.meta.dir, "../bin/octopus-opencodex-bridge");

async function spawnBridge(value: unknown, env?: Record<string, string>): Promise<SpawnedBridge> {
  const child = Bun.spawn([bridgeBinary], { stdin: "pipe", stdout: "pipe", stderr: "pipe", ...(env ? { env } : {}) });
  children.push(child);
  child.stdin.write(JSON.stringify(value));
  child.stdin.end();
  const ready = JSON.parse(await readLine(child.stdout));
  expect(ready).toEqual({
    schema_version: 1,
    status: "ready",
    port: expect.any(Number),
    interface_id: "world_provider_test_exact",
  });
  return { child, origin: `http://127.0.0.1:${ready.port}` };
}

test("actual OpenCodex chat adapter and Responses bridge preserve exact model and stream", async () => {
  let observed: Record<string, unknown> | undefined;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      observed = {
        path: new URL(request.url).pathname,
        authorization: request.headers.get("authorization"),
        marker: request.headers.get("x-octopus-test"),
        body: await request.json(),
      };
      return new Response([
        'data: {"id":"chatcmpl_test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hello"},"finish_reason":null}]}\n\n',
        'data: {"id":"chatcmpl_test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}\n\n',
        "data: [DONE]\n\n",
      ].join(""), { headers: { "Content-Type": "text/event-stream" } });
    },
  });
  servers.push(upstream);
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}/v1`));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "test-model",
      stream: true,
      input: [{ role: "user", content: [{ type: "input_text", text: "hi" }] }],
      tools: [{ type: "function", name: "exact_tool", description: "Exact", parameters: { type: "object", properties: {} } }],
    }),
  });
  expect(response.status).toBe(200);
  const text = await response.text();
  expect(text).toContain("response.output_text.delta");
  expect(text).toContain("hello");
  expect(text).toContain("response.completed");
  expect(observed).toEqual({
    path: "/v1/chat/completions",
    authorization: "Bearer test-secret",
    marker: "exact",
    body: expect.objectContaining({ model: "test-model", stream: true }),
  });
});

test("model outside the exact interface grant is rejected before upstream", async () => {
  let requests = 0;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch() {
      requests += 1;
      return Response.json({});
    },
  });
  servers.push(upstream);
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}/v1`));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "other-model", input: "hi", stream: true }),
  });
  expect(response.status).toBe(400);
  expect(await response.text()).toContain("outside the exact interface model grant");
  expect(requests).toBe(0);
});

test("unknown config fields fail closed before the listener starts", async () => {
  const value = config("http://127.0.0.1:1/v1") as ReturnType<typeof config> & { fallback_provider?: string };
  value.fallback_provider = "forbidden";
  const binary = resolve(import.meta.dir, "../bin/octopus-opencodex-bridge");
  const child = Bun.spawn([binary], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(child);
  child.stdin.write(JSON.stringify(value));
  child.stdin.end();
  expect(await child.exited).toBe(1);
  expect(await new Response(child.stderr).text()).toContain("unsupported field 'fallback_provider'");
});

function sse(events: Array<[string | null, unknown]>): Response {
  const body = events.map(([event, data]) => `${event ? `event: ${event}\n` : ""}data: ${typeof data === "string" ? data : JSON.stringify(data)}\n\n`).join("");
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
}

const userInput = [{ role: "user", content: [{ type: "input_text", text: "hi" }] }];

async function exchange(provider: Record<string, unknown>, upstreamReply: Response, model = "test-model", tools: unknown[] = [], input: unknown[] = userInput) {
  let observed: { path: string; search: string; headers: Headers; body: Record<string, unknown> } | undefined;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      observed = { path: url.pathname, search: url.search, headers: request.headers, body: await request.json() };
      return upstreamReply;
    },
  });
  servers.push(upstream);
  const { base_url: suffix, ...rest } = provider;
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}${suffix ?? "/v1"}`, rest));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model, stream: true, input, tools }),
  });
  return { status: response.status, text: await response.text(), observed: observed! };
}

const exactTool = { type: "function", name: "exact_tool", description: "Exact", parameters: { type: "object", properties: {} } };

test("chat tool calls come back as Responses function calls", async () => {
  const result = await exchange({}, sse([
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "exact_tool", arguments: "{}" } }] }, finish_reason: null }] }],
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] }],
    [null, "[DONE]"],
  ]), "test-model", [exactTool]);
  expect(result.status).toBe(200);
  expect(result.text).toContain("function_call");
  expect(result.text).toContain("exact_tool");
  expect(result.text).toContain("response.completed");
});

test("root schema alternatives become described fields and the upstream's own refusal reaches Codex", async () => {
  const alternativeTool = {
    type: "function",
    name: "alternative_tool",
    description: "Send",
    parameters: {
      type: "object",
      properties: { interface: { type: "string" } },
      required: ["interface"],
      oneOf: [{ required: ["payload", "label"], properties: { label: { type: "string" } } }, { required: ["ref"], properties: { ref: { type: "string" } } }],
      allOf: [{}],
    },
  };
  const refusal = "tools.0.custom.input_schema: input_schema does not support oneOf, allOf, or anyOf at the top level";
  const result = await exchange({}, Response.json({
    error: { message: "Provider returned error", code: 400, metadata: { raw: JSON.stringify({ type: "error", error: { type: "invalid_request_error", message: refusal } }) } },
  }, { status: 400 }), "test-model", [alternativeTool]);
  const sent = (result.observed.body.tools as Array<{ function: { description: string; parameters: Record<string, unknown> } }>)[0].function;
  expect(Object.keys(sent.parameters).filter(key => ["oneOf", "anyOf", "allOf"].includes(key))).toEqual([]);
  expect(sent.parameters.required).toEqual(["interface"]);
  expect(Object.keys(sent.parameters.properties as object).sort()).toEqual(["interface", "label", "ref"]);
  expect(sent.description).toContain("exactly one of these field sets: (payload + label) | (ref)");
  expect(result.status).toBe(400);
  expect(result.text).toContain(refusal);
});

test("registry entry and model alias reach the upstream as the configured model", async () => {
  const result = await exchange({
    registry_entry: "ollama",
    models: ["ollama-model"],
    default_model: "ollama-model",
    model_aliases: { "ollama-model": "qwen3:8b" },
    api_key: undefined,
    auth_header: undefined,
    auth_value_prefix: undefined,
  }, sse([
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "local" }, finish_reason: null }] }],
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }],
    [null, "[DONE]"],
  ]), "ollama-model");
  expect(result.status).toBe(200);
  expect(result.observed.body.model).toBe("qwen3:8b");
  expect(result.observed.headers.get("authorization")).toBeNull();
  expect(result.text).toContain("local");
  expect(result.text).toContain("\"model\":\"ollama-model\"");
});

test("anthropic messages stream through the bridge", async () => {
  const result = await exchange({ adapter: "anthropic", base_url: "", auth_header: "x-api-key", auth_value_prefix: "" }, sse([
    ["message_start", { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: "test-model", content: [], stop_reason: null, usage: { input_tokens: 3, output_tokens: 0 } } }],
    ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
    ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "claude says hi" } }],
    ["content_block_stop", { type: "content_block_stop", index: 0 }],
    ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 4 } }],
    ["message_stop", { type: "message_stop" }],
  ]));
  expect(result.status).toBe(200);
  expect(result.observed.path).toBe("/v1/messages");
  expect(result.observed.headers.get("x-api-key")).toBe("test-secret");
  expect(result.text).toContain("claude says hi");
  expect(result.text).toContain("response.completed");
});

test("gemini generateContent streams through the bridge", async () => {
  const result = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "" }, sse([
    [null, { candidates: [{ index: 0, content: { role: "model", parts: [{ text: "gemini says hi" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 3, totalTokenCount: 6 } }],
  ]));
  expect(result.status).toBe(200);
  expect(result.observed.path).toBe("/v1beta/models/test-model:streamGenerateContent");
  expect(result.observed.search).toBe("?alt=sse");
  expect(result.text).toContain("gemini says hi");
  expect(result.text).toContain("response.completed");
});

function responsesStream(text: string): Response {
  const item = { id: "msg_1", type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text, annotations: [] }] };
  return sse([
    ["response.created", { type: "response.created", response: { id: "resp_1", object: "response", status: "in_progress", model: "test-model", output: [] } }],
    ["response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { ...item, status: "in_progress", content: [] } }],
    ["response.content_part.added", { type: "response.content_part.added", item_id: "msg_1", output_index: 0, content_index: 0, part: { type: "output_text", text: "", annotations: [] } }],
    ["response.output_text.delta", { type: "response.output_text.delta", item_id: "msg_1", output_index: 0, content_index: 0, delta: text }],
    ["response.output_text.done", { type: "response.output_text.done", item_id: "msg_1", output_index: 0, content_index: 0, text }],
    ["response.content_part.done", { type: "response.content_part.done", item_id: "msg_1", output_index: 0, content_index: 0, part: item.content[0] }],
    ["response.output_item.done", { type: "response.output_item.done", output_index: 0, item }],
    ["response.completed", { type: "response.completed", response: { id: "resp_1", object: "response", status: "completed", model: "test-model", output: [item], usage: { input_tokens: 3, output_tokens: 3, total_tokens: 6 } } }],
  ]);
}

for (const adapter of ["openai-responses", "azure"]) {
  test(`${adapter} Responses stream through the bridge`, async () => {
    const result = await exchange({ adapter, ...(adapter === "azure" ? { base_url: "/openai", auth_header: "api-key", auth_value_prefix: "" } : {}) }, responsesStream("responses says hi"));
    expect(result.status).toBe(200);
    expect(result.observed.path).toBe(adapter === "azure" ? "/openai/v1/responses" : "/v1/responses");
    expect(result.observed.headers.get(adapter === "azure" ? "api-key" : "authorization")).toBe(adapter === "azure" ? "test-secret" : "Bearer test-secret");
    expect(result.text).toContain("responses says hi");
    expect(result.text).toContain("response.completed");
  });
}

test("Codex configuration_update items reach neither a translated nor a passthrough upstream", async () => {
  const input = [...userInput, { type: "configuration_update", reasoning: { effort: "high" } }];
  const chat = await exchange({}, sse([
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "chat ok" }, finish_reason: null }] }],
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }],
    [null, "[DONE]"],
  ]), "test-model", [], input);
  const passthrough = await exchange({ adapter: "openai-responses" }, responsesStream("passthrough ok"), "test-model", [], input);
  for (const result of [chat, passthrough]) {
    expect(result.status).toBe(200);
    expect(result.text).toContain("response.completed");
    expect(JSON.stringify(result.observed.body)).toContain("hi");
    expect(JSON.stringify(result.observed.body)).not.toContain("configuration_update");
  }
});

test("claude-cli turns run through the signed-in CLI with a scoped env and the bridge's own capture MCP", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-cli-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const toolArguments = [JSON.stringify({ value: `${"x".repeat(70000)}🙂\n\"` }), JSON.stringify({ value: "second call" })];
  const frames: unknown[] = [];
  toolArguments.forEach((args, index) => {
    frames.push({ type: "stream_event", event: { type: "content_block_start", index, content_block: { type: "tool_use", id: `call_${index}`, name: "exact_tool", input: {} } } });
    for (let offset = 0; offset < args.length; offset += 16) {
      frames.push({ type: "stream_event", event: { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: args.slice(offset, offset + 16) } } });
    }
    frames.push({ type: "stream_event", event: { type: "content_block_stop", index } });
  });
  frames.push({ type: "stream_event", event: { type: "message_delta", delta: { stop_reason: "tool_use" } } }, { type: "stream_event", event: { type: "message_stop" } });
  writeFileSync(join(home, "tool-frames"), frames.map(frame => JSON.stringify(frame)).join("\n") + "\n");
  writeFileSync(join(bin, "claude"), [
    "#!/bin/sh",
    'printf "%s\\n" "$@" > "$HOME/args"',
    'env > "$HOME/env"',
    'cat > "$HOME/stdin"',
    'previous=""',
    'for argument in "$@"; do if [ "$previous" = "--mcp-config" ]; then cp "$argument" "$HOME/mcp.json"; fi; previous="$argument"; done',
    'catalog=$(sed -n \'s/.*"__codebuddy-mcp","\\([^"]*\\)".*/\\1/p\' "$HOME/mcp.json")',
    'cp "$catalog" "$HOME/catalog.json"',
    `printf '%s\\n' '{"type":"system","subtype":"init","mcp_servers":[{"name":"opencodex","status":"connected"}]}'`,
    `printf '%s\\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"claude cli says hi"}]}}'`,
    'cat "$HOME/tool-frames"',
    `printf '%s\\n' '{"type":"result","subtype":"success","is_error":false,"result":"claude cli says hi","usage":{"input_tokens":3,"output_tokens":4}}'`,
  ].join("\n"), { mode: 0o755 });
  const bridge = await spawnBridge(config("https://api.anthropic.com", {
    id: "world_provider_claude_cli",
    adapter: "claude-cli",
    registry_entry: "claude-cli",
    models: ["claude-sonnet-5"],
    default_model: "claude-sonnet-5",
    api_key: undefined,
    auth_header: undefined,
    auth_value_prefix: undefined,
    headers: {},
    allow_private_network: false,
  }), {
    HOME: home,
    USER: "octopus-test",
    PATH: `${bin}:/usr/bin:/bin`,
    TMPDIR: home,
    ANTHROPIC_API_KEY: "must-not-reach-the-cli",
  });
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "claude-sonnet-5",
      stream: true,
      input: [{ role: "user", content: [
        { type: "input_text", text: "hello claude" },
        { type: "input_image", image_url: "data:image/png;base64,aGVsbG8=" },
      ] }],
      tools: [exactTool],
    }),
  });
  expect(response.status).toBe(200);
  const text = await response.text();
  expect(text).toContain("claude cli says hi");
  const events = text.split("\n").filter(line => line.startsWith("data: {")).map(line => JSON.parse(line.slice(6)));
  expect(events.filter(event => event.type === "response.failed").map(event => event.response.error)).toEqual([]);
  expect(events.some(event => event.type === "response.completed")).toBe(true);
  const calls = events.find(event => event.type === "response.completed").response.output.filter((item: { type: string }) => item.type === "function_call");
  expect(calls.map((call: { call_id: string; name: string; arguments: string }) => [call.call_id, call.name, call.arguments])).toEqual(toolArguments.map((args, index) => [`call_${index}`, "exact_tool", args]));
  const args = readFileSync(join(home, "args"), "utf8").split("\n");
  expect(args).toEqual(expect.arrayContaining(["-p", "--tools", "", "--strict-mcp-config", "--setting-sources", "--no-session-persistence", "--model", "claude-sonnet-5", "--mcp-config"]));
  const env = readFileSync(join(home, "env"), "utf8");
  expect(env).toContain(`HOME=${home}`);
  expect(env).toContain(`CLAUDE_CODE_TMPDIR=${home}`);
  expect(env).toContain("USER=octopus-test");
  expect(env).not.toContain("ANTHROPIC_");
  expect(readFileSync(join(home, "stdin"), "utf8")).toContain("hello claude");
  const nativeInput = JSON.parse(readFileSync(join(home, "stdin"), "utf8"));
  expect(nativeInput.message.content).toContainEqual({ type: "image", source: { type: "base64", media_type: "image/png", data: "aGVsbG8=" } });
  const mcp = JSON.parse(readFileSync(join(home, "mcp.json"), "utf8"));
  const server = Object.values(mcp.mcpServers)[0] as { command: string; args: string[] };
  expect(server.command).toBe(bridgeBinary);
  expect(server.args[0]).toBe("__codebuddy-mcp");
  const capture = Bun.spawn([bridgeBinary, "__codebuddy-mcp", join(home, "catalog.json")], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(capture);
  capture.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "octopus-test", version: "1" } } })}\n`);
  capture.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
  capture.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list" })}\n`);
  const reader = capture.stdout.getReader();
  const decoder = new TextDecoder();
  let buffered = "";
  let listed: { result: { tools: Array<{ name: string }> } } | undefined;
  while (!listed) {
    const next = await reader.read();
    if (next.done) throw new Error("capture MCP exited before listing tools");
    buffered += decoder.decode(next.value, { stream: true });
    for (const line of buffered.split("\n").slice(0, -1)) {
      const message = JSON.parse(line);
      if (message.id === 2) listed = message;
    }
    buffered = buffered.slice(buffered.lastIndexOf("\n") + 1);
  }
  expect(listed.result.tools.map((tool) => tool.name).join(",")).toContain("exact_tool");
});

test("an incomplete claude-cli tool call fails closed and names the frame sequence that left it open", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-cli-open-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const frames = [
    { type: "system", subtype: "init", mcp_servers: [{ name: "opencodex", status: "connected" }] },
    { type: "stream_event", event: { type: "message_start" } },
    { type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "call_0", name: "exact_tool", input: {} } } },
    { type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: "{\"value\":" } } },
    { type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: "\"secret text\"" } } },
    { type: "result", subtype: "success", is_error: false, result: "", usage: { input_tokens: 3, output_tokens: 4 } },
  ];
  writeFileSync(join(home, "frames"), frames.map(frame => JSON.stringify(frame)).join("\n") + "\n");
  writeFileSync(join(bin, "claude"), ["#!/bin/sh", 'cat > /dev/null', 'cat "$HOME/frames"'].join("\n"), { mode: 0o755 });
  const bridge = await spawnBridge(config("https://api.anthropic.com", {
    id: "world_provider_claude_cli", adapter: "claude-cli", registry_entry: "claude-cli",
    models: ["claude-sonnet-5"], default_model: "claude-sonnet-5", api_key: undefined, auth_header: undefined,
    auth_value_prefix: undefined, headers: {}, allow_private_network: false,
  }), { HOME: home, USER: "octopus-test", PATH: `${bin}:/usr/bin:/bin`, TMPDIR: home });
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "claude-sonnet-5", stream: true, input: [{ role: "user", content: [{ type: "input_text", text: "hello" }] }], tools: [exactTool] }),
  });
  const text = await response.text();
  expect(text).toContain("Coding-agent CLI ended with an incomplete tool call.");
  expect(text).toContain("CLI frames: system:init message_start block_start[0]:tool_use(");
  expect(text).toContain("delta[0]:input_json_deltax2 result:success");
  expect(text).not.toContain("secret text");
});

test("grok-cli turns run the signed-in Grok CLI in an isolated home and hand back the catalog calls it makes", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-grok-cli-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const calls = { type: "assistant", message: { stop_reason: "tool_use", usage: { input_tokens: 5, output_tokens: 6 }, content: [
    { type: "thinking", thinking: "two calls" },
    { type: "tool_use", id: "call_a", name: "use_tool", input: { tool_name: "opencodex__exact_tool", tool_input: { value: "a" } } },
    { type: "tool_use", id: "call_b", name: "use_tool", input: { tool_name: "opencodex__exact_tool", tool_input: { value: "b" } } },
  ] } };
  writeFileSync(join(home, "frames"), JSON.stringify(calls) + "\n");
  writeFileSync(join(bin, "grok"), [
    "#!/bin/sh",
    `printf "%s\\n" "$@" > "${home}/args"`,
    `env > "${home}/env"`,
    `cp "$HOME/.claude.json" "${home}/mcp.json"`,
    `cp "$(dirname "$HOME")/catalog.json" "${home}/catalog.json"`,
    'previous=""',
    `for argument in "$@"; do if [ "$previous" = "--prompt-file" ]; then cp "$argument" "${home}/prompt"; fi; previous="$argument"; done`,
    `cat "${home}/frames"`,
    "sleep 30",
  ].join("\n"), { mode: 0o755 });
  const bridge = await spawnBridge(config("https://cli-chat-proxy.grok.com/v1", {
    id: "world_provider_grok_cli",
    adapter: "grok-cli",
    models: ["grok-4.7"],
    default_model: "grok-4.7",
    api_key: undefined,
    auth_header: undefined,
    auth_value_prefix: undefined,
    headers: {},
    allow_private_network: false,
  }), { HOME: home, USER: "octopus-test", PATH: `${bin}:/usr/bin:/bin`, TMPDIR: home, XAI_API_KEY: "must-not-reach-the-cli" });
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "grok-4.7", stream: true, reasoning: { effort: "xhigh" }, input: [{ role: "user", content: [{ type: "input_text", text: "hello grok" }] }], tools: [exactTool] }),
  });
  expect(response.status).toBe(200);
  const events = (await response.text()).split("\n").filter(line => line.startsWith("data: {")).map(line => JSON.parse(line.slice(6)));
  expect(events.filter(event => event.type === "response.failed").map(event => event.response.error)).toEqual([]);
  const completed = events.find(event => event.type === "response.completed").response.output.filter((item: { type: string }) => item.type === "function_call");
  expect(completed.map((call: { call_id: string; name: string; arguments: string }) => [call.call_id, call.name, call.arguments])).toEqual([
    ["call_a", "exact_tool", JSON.stringify({ value: "a" })],
    ["call_b", "exact_tool", JSON.stringify({ value: "b" })],
  ]);
  const args = readFileSync(join(home, "args"), "utf8").split("\n");
  expect(args).toEqual(expect.arrayContaining(["--prompt-file", "--agent", "-m", "grok-4.7", "--effort", "xhigh", "--max-turns", "1", "--tools", "use_tool", "--output-format", "streaming-messages-json"]));
  const env = readFileSync(join(home, "env"), "utf8");
  expect(env).toContain(`GROK_HOME=${join(home, ".grok")}`);
  expect(env).not.toContain(`HOME=${home}\n`);
  expect(env).not.toContain("XAI_API_KEY");
  expect(readFileSync(join(home, "prompt"), "utf8")).toContain("hello grok");
  const server = Object.values(JSON.parse(readFileSync(join(home, "mcp.json"), "utf8")).mcpServers)[0] as { command: string; args: string[] };
  expect(server.command).toBe(bridgeBinary);
  expect(server.args[0]).toBe("__grok-mcp");
  const capture = Bun.spawn([bridgeBinary, "__grok-mcp", join(home, "catalog.json")], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(capture);
  capture.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" })}\n`);
  capture.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "exact_tool", arguments: { value: "a" } } })}\n`);
  capture.stdin.end();
  const replies = (await new Response(capture.stdout).text()).trim().split("\n").map(line => JSON.parse(line));
  expect(replies[0].result.tools.map((tool: { name: string }) => tool.name)).toEqual(["exact_tool"]);
  expect(replies[1].result.content[0].type).toBe("text");
});

test("provider descriptors match providers.json and the pinned OpenCodex registry", () => {
  const result = Bun.spawnSync([process.execPath, "run", "scripts/sync-providers.ts", "--check"], { cwd: resolve(import.meta.dir, "..") });
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
});
