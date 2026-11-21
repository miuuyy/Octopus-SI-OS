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

async function spawnBridge(value: unknown, env?: Record<string, string>, cwd?: string): Promise<SpawnedBridge> {
  const child = Bun.spawn([bridgeBinary], { stdin: "pipe", stdout: "pipe", stderr: "pipe", cwd, ...(env ? { env } : {}) });
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

async function exchange(provider: Record<string, unknown>, upstreamReply: Response, model = "test-model", tools: unknown[] = [], input: unknown[] = userInput, stream = true) {
  let observed: { path: string; search: string; headers: Headers; body: Record<string, unknown> } | undefined;
  let requests = 0;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      requests += 1;
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
    body: JSON.stringify({ model, stream, input, tools }),
  });
  return { status: response.status, headers: response.headers, text: await response.text(), observed: observed!, requests };
}

test("a spent quota window returns an immediate typed 429 with only observed retry advice", async () => {
  const response = () => ({ error: { code: "insufficient_quota", message: "opaque private upstream detail" } });
  const before = Math.floor(Date.now() / 1000);
  const timed = await exchange({}, Response.json(response(), { status: 429, headers: { "Retry-After": "120" } }));
  expect(timed.status).toBe(429);
  expect(timed.requests).toBe(1);
  expect(timed.headers.get("retry-after")).toBe("120");
  const { error } = JSON.parse(timed.text);
  expect(error).toMatchObject({ type: "usage_limit_reached", code: "usage_limit_reached", message: "Provider quota exhausted" });
  expect(error.resets_at).toBeGreaterThanOrEqual(before + 120);
  expect(timed.text).not.toContain("opaque private upstream detail");
  for (const headers of [undefined, { "Retry-After": "not a delay" }, { "Retry-After": "0" }]) {
    const unknown = await exchange({}, Response.json(response(), { status: 429, headers }));
    expect(unknown.status).toBe(429);
    expect(unknown.requests).toBe(1);
    expect(unknown.headers.get("retry-after")).toBeNull();
    expect(JSON.parse(unknown.text).error).toEqual({
      message: "Provider quota exhausted", type: "usage_limit_reached", code: "usage_limit_reached",
    });
  }
  const resetsAt = Math.floor(Date.now() / 1000) + 60;
  const reset = await exchange({}, Response.json({ error: { ...response().error, resets_at: resetsAt } }, { status: 429 }));
  expect(JSON.parse(reset.text).error.resets_at).toBe(resetsAt);
  expect(Number(reset.headers.get("retry-after"))).toBeGreaterThan(0);
});

test("transient 429 is returned immediately for Codex-owned retries", async () => {
  for (const advice of [undefined, "15"]) {
    const result = await exchange({}, Response.json({
      error: { code: "rate_limit_exceeded", message: "per-minute quota exceeded; opaque private upstream detail" },
    }, { status: 429, headers: advice ? { "Retry-After": advice } : undefined }));
    expect(result.status).toBe(429);
    expect(result.requests).toBe(1);
    expect(result.headers.get("retry-after")).toBe(advice ?? null);
    expect(JSON.parse(result.text).error).toMatchObject({ type: "rate_limit_error", code: "rate_limit_exceeded" });
    expect(result.text).not.toContain("opaque private upstream detail");
  }
});

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

test("gemini generateContent sends the declared safety settings and streams through the bridge", async () => {
  const descriptor = JSON.parse(readFileSync(resolve(import.meta.dir, "../../../../../world_provider_gemini/interface.json"), "utf8"));
  const options = descriptor.interface_driver_config.private_config.opencodex.options;
  const result = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "", options }, sse([
    [null, { candidates: [{ index: 0, content: { role: "model", parts: [{ text: "gemini says hi" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 3, totalTokenCount: 6 } }],
  ]));
  expect(result.status).toBe(200);
  expect(result.observed.path).toBe("/v1beta/models/test-model:streamGenerateContent");
  expect(result.observed.search).toBe("?alt=sse");
  expect(result.observed.body.safetySettings).toEqual([
    "HARASSMENT", "HATE_SPEECH", "SEXUALLY_EXPLICIT", "DANGEROUS_CONTENT", "JAILBREAK",
  ].map(category => ({ category: `HARM_CATEGORY_${category}`, threshold: "OFF" })));
  expect(result.text).toContain("gemini says hi");
  expect(result.text).toContain("response.completed");
});

test("Gemini prompt and response blocks retain Google's reasons for streamed and buffered replies", async () => {
  const ratings = [{ category: "HARM_CATEGORY_DANGEROUS_CONTENT", probability: "HIGH", blocked: true }];
  for (const payload of [
    { promptFeedback: { blockReason: "SAFETY", safetyRatings: ratings } },
    { candidates: [{ finishReason: "PROHIBITED_CONTENT", finishMessage: "Provider's reason", safetyRatings: ratings }] },
  ]) {
    for (const stream of [true, false]) {
      const reply = stream ? sse([[null, payload]]) : Response.json(payload);
      const result = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "" }, reply, "test-model", [], userInput, stream);
      expect(result.text).toContain(stream ? "response.failed" : '"status":"failed"');
      expect(result.text).toContain('"code":"content_filter"');
      expect(result.text).toContain("HARM_CATEGORY_DANGEROUS_CONTENT");
      expect(result.text).toContain("promptFeedback" in payload ? "SAFETY" : "Provider's reason");
      expect(result.text).not.toContain("response.completed");
    }
  }
});

test("a reasoning-only completion is retried once as the identical turn instead of passing as an empty success", async () => {
  const bodies: string[] = [];
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      bodies.push(await request.text());
      return bodies.length === 1
        ? sse([[null, { candidates: [{ index: 0, content: { role: "model", parts: [{ text: "thinking only", thought: true }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 2, totalTokenCount: 5 } }]])
        : sse([[null, { candidates: [{ index: 0, content: { role: "model", parts: [{ text: "the retried answer" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 3, totalTokenCount: 6 } }]]);
    },
  });
  servers.push(upstream);
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}`, { adapter: "google", auth_header: "x-goog-api-key", auth_value_prefix: "" }));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "test-model", stream: true, input: [{ role: "user", content: [{ type: "input_text", text: "hi" }] }] }),
  });
  const text = await response.text();
  expect(bodies).toHaveLength(2);
  expect(bodies[1]).toBe(bodies[0]);
  expect(text).toContain("the retried answer");
  expect(text).toContain("response.completed");
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

test("claude-cli deadline fails the response and closes the child", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-deadline-"));
  directories.push(home);
  mkdirSync(join(home, "bin"));
  writeFileSync(join(home, "bin/claude"), '#!/bin/sh\necho $$ > "$HOME/child.pid"\ncat >/dev/null\nexec sleep 30\n', { mode: 0o755 });
  const settings = config("https://api.anthropic.com", { id: "world_provider_claude_cli", adapter: "claude-cli",
    registry_entry: "claude-cli", models: ["claude-sonnet-5"], default_model: "claude-sonnet-5",
    api_key: undefined, auth_header: undefined, auth_value_prefix: undefined, headers: {} });
  settings.limits.request_timeout_ms = 1000;
  const bridge = await spawnBridge(settings, { HOME: home, USER: "octopus-test", PATH: `${home}/bin:/usr/bin:/bin`, TMPDIR: home });
  const response = await fetch(`${bridge.origin}/v1/responses`, { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "claude-sonnet-5", stream: true, input: "wait for the response" }) });
  const events = (await response.text()).split("\n").filter(line => line.startsWith("data: {")).map(line => JSON.parse(line.slice(6)));
  const failed = events.filter(event => event.type === "response.failed");
  expect(failed).toHaveLength(1);
  expect(failed[0].response.error.message).toBe("Provider request deadline exceeded (1000 ms).");
  expect(failed[0].response.error.code).toBe("timeout");
  expect(events.some(event => event.type === "response.completed")).toBe(false);
  const pid = Number(readFileSync(join(home, "child.pid"), "utf8"));
  await Bun.sleep(50);
  expect(() => process.kill(pid, 0)).toThrow();
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

test("a claude-cli continuation after a cut assistant message is discarded with a named reason", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-cli-cut-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const frames = [
    { type: "system", subtype: "init", mcp_servers: [{ name: "opencodex", status: "connected" }] },
    { type: "stream_event", event: { type: "message_start" } },
    { type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "call_0", name: "exact_tool", input: {} } } },
    { type: "stream_event", event: { type: "message_start" } },
    { type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } } },
    { type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "The previous step was stopped" } } },
    { type: "stream_event", event: { type: "content_block_stop", index: 0 } },
    { type: "stream_event", event: { type: "message_stop" } },
    { type: "result", subtype: "success", is_error: false, result: "The previous step was stopped", usage: { input_tokens: 3, output_tokens: 4 } },
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
  expect(text).toContain("Claude Code started another assistant message before the previous one finished");
  expect(text).toContain("CLI frames: system:init message_start block_start[0]:tool_use(");
  expect(text).not.toContain("The previous step was stopped");
});

test("a Claude CLI subscription limit is a sanitized terminal Responses failure", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-cli-quota-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const frames = [
    { type: "system", subtype: "init", mcp_servers: [{ name: "opencodex", status: "connected" }] },
    { type: "result", subtype: "error_during_execution", is_error: true,
      result: "You've hit your limit · opaque upstream detail",
      errors: ["You've hit your limit · opaque upstream detail"] },
  ];
  writeFileSync(join(home, "frames"), frames.map(frame => JSON.stringify(frame)).join("\n") + "\n");
  writeFileSync(join(bin, "claude"), ["#!/bin/sh", "cat > /dev/null", 'cat "$HOME/frames"'].join("\n"), { mode: 0o755 });
  const bridge = await spawnBridge(config("https://api.anthropic.com", {
    id: "world_provider_claude_cli", adapter: "claude-cli", registry_entry: "claude-cli",
    models: ["claude-sonnet-5"], default_model: "claude-sonnet-5",
    api_key: undefined, auth_header: undefined, auth_value_prefix: undefined, headers: {}, allow_private_network: false,
  }), { HOME: home, USER: "octopus-test", PATH: `${bin}:/usr/bin:/bin`, TMPDIR: home });
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "claude-sonnet-5", stream: true, input: userInput }),
  });
  const body = await response.text();
  expect(body).toContain("response.failed");
  expect(body).toContain('"code":"insufficient_quota"');
  expect(body).not.toContain("opaque upstream detail");
  expect(body).not.toContain("response.completed");
});

test("a claude-cli turn follows the CLI past a refusal frame and ends as refused only when its final response is one", async () => {
  const home = mkdtempSync(join(tmpdir(), "octopus-claude-cli-refusal-"));
  directories.push(home);
  const bin = join(home, "bin");
  mkdirSync(bin);
  const init = { type: "system", subtype: "init", mcp_servers: [{ name: "opencodex", status: "connected" }] };
  const refused = [
    { type: "stream_event", event: { type: "message_start" } },
    { type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } } },
    { type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "refused partial" } } },
    { type: "assistant", message: { role: "assistant", content: [{ type: "text", text: "refused partial" }] } },
    { type: "stream_event", event: { type: "message_delta", delta: { stop_reason: "refusal" } } },
  ];
  const answer = [
    { type: "stream_event", event: { type: "message_start" } },
    { type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } } },
    { type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "CLI continuation" } } },
    { type: "stream_event", event: { type: "content_block_stop", index: 0 } },
    { type: "stream_event", event: { type: "message_delta", delta: { stop_reason: "end_turn" } } },
    { type: "stream_event", event: { type: "message_stop" } },
  ];
  const result = { type: "result", subtype: "success", is_error: false, result: "CLI continuation", usage: { input_tokens: 3, output_tokens: 4 } };
  writeFileSync(join(bin, "claude"), ["#!/bin/sh", 'cat > /dev/null', 'cat "$HOME/frames"'].join("\n"), { mode: 0o755 });
  const bridge = await spawnBridge(config("https://api.anthropic.com", {
    id: "world_provider_claude_cli", adapter: "claude-cli", registry_entry: "claude-cli",
    models: ["claude-sonnet-5"], default_model: "claude-sonnet-5", api_key: undefined, auth_header: undefined,
    auth_value_prefix: undefined, headers: {}, allow_private_network: false,
  }), { HOME: home, USER: "octopus-test", PATH: `${bin}:/usr/bin:/bin`, TMPDIR: home });
  const turn = async (frames: unknown[]) => {
    writeFileSync(join(home, "frames"), frames.map(frame => JSON.stringify(frame)).join("\n") + "\n");
    const response = await fetch(`${bridge.origin}/v1/responses`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: "claude-sonnet-5", stream: true, input: [{ role: "user", content: [{ type: "input_text", text: "hello" }] }] }),
    });
    return response.text();
  };
  const retried = await turn([init, ...refused, ...answer, result]);
  expect(retried).toContain("CLI continuation");
  expect(retried).toContain("response.completed");
  expect(retried).not.toContain("response.failed");
  expect(retried).not.toContain("refused partial");
  const final = await turn([init, ...answer, ...refused, result]);
  expect(final).toContain("response.failed");
  expect(final).toContain("invalid_prompt");
  expect(final).not.toContain("protocol_error");
  expect(final).not.toContain("refused partial");
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

test("a hosted web_search is refused because the bridge serves no search backend", async () => {
  const hosted = [{ type: "web_search", external_web_access: true }];
  const refused = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "" }, sse([]), "test-model", hosted);
  expect(refused.status).toBe(400);
  expect(refused.observed).toBeUndefined();
  expect(refused.text).toContain("serves no hosted web_search backend");
  const declared = Bun.spawn([bridgeBinary], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(declared);
  declared.stdin.write(JSON.stringify(config("http://127.0.0.1:9/v1", { web_search: { backend: "gemini", model: "gemini-4-hydrogen" } })));
  declared.stdin.end();
  expect(await declared.exited).toBe(1);
  expect(await new Response(declared.stderr).text()).toContain("web_search");
});

const FAKE_ACP_SERVER = String.raw`
const fs = require("node:fs");
const { createInterface } = require("node:readline");
const root = require("node:path").dirname(process.env.GEMINI_HOME);
const log = (value) => fs.appendFileSync(root + "/acp-log.jsonl", JSON.stringify(value) + "\n");
const send = (value) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...value }) + "\n");
let session, mcp;
async function receive(message) {
  const { id, method, params } = message;
  if (!method) { log({ id, outcome: message.result?.outcome }); return; }
  if (method === "initialize") return send({ id, result: { protocolVersion: 1 } });
  if (method === "authenticate" && fs.existsSync(process.env.GEMINI_HOME + "/signed-out")) {
    process.stderr.write("Open the following link to authenticate the ACP server: https://accounts.google.com/signin\n"); return;
  }
  if (method === "session/new" || method === "session/resume") {
    session = params.sessionId ?? crypto.randomUUID();
    mcp = params.mcpServers[0];
    const catalog = mcp ? await fetch(mcp.url, { method: "POST", headers: { "content-type": "application/json", authorization: mcp.headers[0].value },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }) }).then(r => r.json()) : undefined;
    log({ method, session, tools: catalog?.result.tools ?? [], meta: params._meta.antigravity, env: { home: process.env.HOME, gemini: process.env.GEMINI_HOME, secret: process.env.OCTOPUS_TEST_SECRET } });
    return send({ id, result: { sessionId: session } });
  }
  if (method === "session/set_config_option") log({ method, ...params });
  if (method !== "session/prompt") return send({ id, result: {} });
  const text = params.prompt.map(p => p.resource?.text ?? p.text ?? "").join("");
  log({ method, session, text, mediaBytes: params.prompt.filter(p => p.type === "image").map(p => Buffer.from(p.data, "base64").length) });
  const update = (update) => send({ method: "session/update", params: { sessionId: session, update } });
  const say = (text) => update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text } });
  const overflow = "The input token count exceeds the maximum number of tokens allowed (1048576); input token count: 1257138";
  if (text.includes("native-error") || text.includes("native-overflow")) {
    say(text.includes("native-overflow") ? overflow : "Agent execution error: trajectory converted to zero chat messages");
    process.stderr.write("E1117 01:02:03.004 12345 server.py:5737] Antigravity SDK error processing prompt for session " + session + "\n");
  } else if (text.includes("call-inspect") && !text.includes("TOOL RESULT")) {
    for (const [id, server, tool] of [["exact_tool", "opencodex", "exact_tool"], ["unoffered", "opencodex", "workspace_build"], ["other_server", "other", "exact_tool"]]) {
      send({ id, method: "session/request_permission", params: { sessionId: session,
        toolCall: { _meta: { mcp: { server, tool } } }, options: [{ kind: "allow_once", optionId: "yes" }] } });
    }
    const unknown = await fetch(mcp.url, { method: "POST", headers: { "content-type": "application/json", authorization: mcp.headers[0].value },
      body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "workspace_build" } }) }).then(r => r.json());
    log({ unknown: unknown.error });
    update({ sessionUpdate: "usage_update", used: 41, size: 1048576 });
    const response = await fetch(mcp.url, { method: "POST", headers: { "content-type": "application/json", authorization: mcp.headers[0].value },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "exact_tool", arguments: { value: "a" } } }) });
    const returned = (await response.json()).result;
    log({ returned });
    say("Result received.");
  } else say(text.includes("quote-overflow") ? overflow : text.includes("quote-error") ? "Agent execution error: trajectory converted to zero chat messages" : "Continued.");
  if (!text.includes("no-checkpoint")) update({ sessionUpdate: "usage_update", used: 128, size: 1048576 });
  const aggregate = text.includes("aggregate") || text.includes("no-checkpoint");
  send({ id, result: { stopReason: "end_turn", usage: { totalTokens: aggregate ? 1500000 : 128, inputTokens: aggregate ? 1499972 : 100, cachedReadTokens: 20, outputTokens: 8, thoughtTokens: 2 } } });
}
const input = createInterface({ input: process.stdin });
input.on("line", line => receive(JSON.parse(line)).catch(error => { process.stderr.write(String(error)); process.exit(1); }));
input.on("close", () => process.exit(0));
`;

test("Antigravity retains native context, delivers exact external results and distinguishes native failures from model text", async () => {
  const pin = JSON.parse(readFileSync(resolve(import.meta.dir, "../antigravity-acp.json"), "utf8"));
  const directory = mkdtempSync(join(tmpdir(), "octopus-antigravity-"));
  directories.push(directory);
  const account = join(directory, "opencodex-account"), install = join(account, "acp", pin.version);
  mkdirSync(install, { recursive: true, mode: 0o700 });
  mkdirSync(join(account, "home"), { mode: 0o700 });
  writeFileSync(join(install, pin.executable), `#!${process.execPath}\n${FAKE_ACP_SERVER}`, { mode: 0o755 });
  writeFileSync(join(install, pin.harness), "", { mode: 0o755 });
  writeFileSync(join(install, "receipt.json"), JSON.stringify({ version: pin.version, sha256: pin.assets[`${process.platform}-${process.arch}`].sha256 }));
  const start = () => spawnBridge(config("https://cloudcode-pa.googleapis.com", {
    id: "world_provider_antigravity", adapter: "antigravity-acp", models: ["gemini-4-hydrogen"], default_model: "gemini-4-hydrogen",
    api_key: undefined, auth_header: undefined, auth_value_prefix: undefined, headers: {}, allow_private_network: false,
    account: { provider: "antigravity-acp", directory: account },
  }), { PATH: "/usr/bin:/bin", OCTOPUS_TEST_SECRET: "private" }, directory);
  let bridge = await start();
  const write = JSON.parse(readFileSync(resolve(import.meta.dir, "../../../../../../tool/file.write/tool.json"), "utf8"));
  const granted = [exactTool, { ...exactTool, name: "formed_interfaces__workspace_build" }, { type: "function", name: "file_write", description: write.description, parameters: write.args_schema }];
  const turn = async (thread: string, input: unknown[], tools: unknown[] = granted) => {
    const response = await fetch(`${bridge.origin}/v1/responses`, { method: "POST", headers: { "Content-Type": "application/json", "thread-id": thread },
      body: JSON.stringify({ model: "gemini-4-hydrogen", stream: true, instructions: "The worker's role.", reasoning: { effort: "high" }, input, tools }) });
    return (await response.text()).split("\n").filter(line => line.startsWith("data: {")).map(line => JSON.parse(line.slice(6)));
  };
  const user = (text: string) => ({ role: "user", content: text });
  const end = (events: any[]) => events.find(e => e.type === "response.completed")?.response;
  const logs = () => readFileSync(join(account, "acp-log.jsonl"), "utf8").trim().split("\n").map(line => JSON.parse(line));
  expect(JSON.stringify(await turn("", [user("hi")]))).toContain("thread-id");
  const initial = [user("call-inspect")];
  const [one, again] = await Promise.all([turn("alpha", initial), turn("alpha", initial)]);
  const call = end(one).output.find((item: any) => item.type === "function_call");
  expect(call).toMatchObject({ name: "exact_tool", arguments: '{"value":"a"}' });
  expect(end(again).output.find((item: any) => item.type === "function_call").call_id).toBe(call.call_id);
  const result = "BEGIN\n" + "Ж".repeat(40_000) + "\nEND";
  const history = [...initial, ...end(one).output, { type: "function_call_output", call_id: call.call_id, output: result }];
  const done = end(await turn("alpha", history));
  expect(JSON.stringify(done.output)).toContain("Result received.");
  const first = logs().find(row => row.method === "session/new");
  expect(first.meta.enabledTools).toEqual(["call_mcp_tool"]);
  expect(first.meta.eagerMcpServers ?? []).toEqual([]);
  expect(first.tools).toContainEqual({ name: "file_write", description: write.description, inputSchema: write.args_schema });
  expect(first.meta.systemPrompt.append).toContain(JSON.stringify(first.tools));
  expect(first.meta.systemPrompt.append).toContain("The worker's role.");
  expect(first.env.gemini).toBe(join(account, "home"));
  expect(first.env.secret).toBeUndefined();
  expect(logs().filter(row => row.method === "session/set_config_option").map(row => row.value)).toContain("high");
  expect(logs().find(row => row.id === "exact_tool").outcome.outcome).toBe("selected");
  expect(logs().find(row => row.id === "unoffered").outcome.outcome).toBe("cancelled");
  expect(logs().find(row => row.unknown).unknown).toEqual({ code: -32602, message:
    'Unknown tool "workspace_build". Use the exact ToolName from tools/list.' });
  expect(logs().find(row => row.id === "other_server").outcome.outcome).toBe("cancelled");
  expect(logs().filter(row => row.method === "session/prompt")).toHaveLength(1);
  const attached = logs().find(row => row.returned).returned.content[0].resource;
  expect(attached.mimeType).toBe("text/plain");
  expect(Buffer.from(attached.blob, "base64").toString()).toBe(result);
  const next = end(await turn("alpha", [...history, ...done.output, user("next")]));
  expect(next.usage).toMatchObject({ native_context_tokens: 128, output_tokens: 8, input_tokens_details: { cached_tokens: 20 }, output_tokens_details: { reasoning_tokens: 2 } });
  expect(logs().find(row => row.method === "session/resume").session).toBe(first.session);
  expect(logs().filter(row => row.method === "session/prompt").at(-1).text).toBe("USER:\nnext");
  const interrupted = end(await turn("beta", initial));
  const pending = interrupted.output.find((item: any) => item.type === "function_call");
  bridge.child.kill(); await bridge.child.exited;
  bridge = await start();
  const recovered = end(await turn("beta", [...initial, ...interrupted.output, { type: "function_call_output", call_id: pending.call_id, output: "admitted result" }]));
  expect(JSON.stringify(recovered.output)).toContain("Continued.");
  expect(logs().filter(row => row.method === "session/prompt").at(-1).text).toContain(`TOOL RESULT (call_id: ${pending.call_id}):\nadmitted result`);
  expect(end(await turn("beta", initial)).output.find((item: any) => item.type === "function_call").call_id).toBe(pending.call_id);
  const longHistory = [user("retained history " + "a".repeat(400000))];
  const long = end(await turn("replay-growth", longHistory));
  expect(long.usage.native_context_tokens).toBe(128);
  expect(long.usage.total_tokens).toBeGreaterThan(100000);
  expect(long.usage.context_estimated).toBe(true);
  const replayed = end(await turn("replay-growth", longHistory));
  expect(replayed.usage).toEqual(long.usage);
  const compacted = end(await turn("replay-growth", [user("short compacted summary")]));
  expect(compacted.usage.total_tokens).toBeLessThan(10000);
  const image = end(await turn("media", [{ role: "user", content: [
    { type: "input_image", image_url: "data:image/png;base64," + Buffer.alloc(300000).toString("base64") },
  ] }], []));
  expect(image.usage).toMatchObject({ native_context_tokens: 128, replay_unmeasured_media: 1 });
  expect(image.usage.total_tokens).toBeLessThan(1000);
  expect(logs().filter(row => row.method === "session/prompt").at(-1).mediaBytes).toEqual([300000]);
  const multi = end(await turn("aggregate", [user("aggregate")]));
  expect(multi.usage).toMatchObject({ native_context_tokens: 128, output_tokens: 0 });
  expect(multi.usage.total_tokens).toBeLessThan(1500000);
  expect(end(await turn("unknown", [user("no-checkpoint")])).usage).toMatchObject({ context_estimated: true, native_context_tokens: null });
  const unknownJson = await fetch(bridge.origin + "/v1/responses", { method: "POST", headers: {
    "Content-Type": "application/json", "thread-id": "unknown-json" },
    body: JSON.stringify({ model: "gemini-4-hydrogen", input: [user("no-checkpoint")], stream: false }) });
  expect((await unknownJson.json()).usage).toMatchObject({ context_estimated: true, native_context_tokens: null });
  const asking = [user("call-inspect no-checkpoint")];
  const asked = end(await turn("stale", asking));
  expect(asked.usage.native_context_tokens).toBe(41);
  const waiting = asked.output.find((item: any) => item.type === "function_call");
  const stale = end(await turn("stale", [...asking, ...asked.output, { type: "function_call_output", call_id: waiting.call_id, output: "result" }]));
  expect(stale.usage).toMatchObject({ context_estimated: true, native_context_tokens: null });
  const overflow = await turn("overflow", [user("native-overflow")]);
  expect(overflow.find(e => e.type === "response.failed").response.error.code).toBe("context_length_exceeded");
  expect(end(await turn("overflow-quoted", [user("quote-overflow")])).status).toBe("completed");
  const failed = await turn("alpha", [user("native-error")]);
  expect(failed.some(row => row.type === "response.completed" || row.type === "response.output_text.delta")).toBe(false);
  expect(JSON.stringify(failed.find(row => row.type === "response.failed"))).toContain("trajectory converted to zero chat messages");
  expect(JSON.stringify(end(await turn("alpha", [user("quote-error")])).output)).toContain("trajectory converted to zero chat messages");
  expect(end(await turn("no-tools", [user("hello")], [])).status).toBe("completed");
  const empty = logs().filter(row => row.method === "session/new").at(-1);
  expect(empty.meta.enabledTools).toEqual([]);
  expect(empty.meta.systemPrompt.append).not.toContain("file_write");
  writeFileSync(join(account, "home", "signed-out"), "");
  expect(JSON.stringify((await turn("gamma", [user("new")])).find(e => e.type === "response.failed"))).toContain("signed out");
  const status = Bun.spawn([bridgeBinary, "__account"], { stdin: "pipe", stdout: "pipe", stderr: "pipe", env: {} });
  children.push(status);
  status.stdin.write(JSON.stringify({ operation: "status", account: { provider: "antigravity-acp", directory: account } }));
  status.stdin.end();
  expect(JSON.parse(await new Response(status.stdout).text())).toEqual({ event: "result", result: { signedIn: false, accountLabel: null } });
});

test("a Vertex provider binds exactly one declared credential and a provider status no retry can change ends the turn", async () => {
  const directory = mkdtempSync(join(tmpdir(), "octopus-vertex-"));
  directories.push(directory);
  const credentials = join(directory, "adc.json");
  writeFileSync(credentials, JSON.stringify({ type: "authorized_user", client_id: "id", client_secret: "secret", refresh_token: "refresh" }));
  const vertex = (overrides: Record<string, unknown>, provider: Record<string, unknown> = {}) => config("https://aiplatform.googleapis.com", {
    adapter: "google",
    api_key: undefined,
    auth_header: undefined,
    allow_private_network: false,
    headers: {},
    vertex: { project: "octopus-test-1", location: "global", credentials_file: credentials, ...overrides },
    ...provider,
  });
  const refused = async (value: unknown) => {
    const child = Bun.spawn([bridgeBinary], { stdin: "pipe", stdout: "pipe", stderr: "pipe", env: {} });
    children.push(child);
    child.stdin.write(JSON.stringify(value));
    child.stdin.end();
    expect(await child.exited).toBe(1);
    return new Response(child.stderr).text();
  };
  expect(await refused(vertex({}, { api_key: "key", auth_header: "x-goog-api-key" }))).toContain("exactly one of an API key or a credentials file");
  expect(await refused(vertex({ location: "us-central1" }))).toContain("must be https://us-central1-aiplatform.googleapis.com");
  expect(await refused(vertex({ credentials_file: join(directory, "absent.json") }))).toContain("cannot be read (ENOENT)");
  writeFileSync(join(directory, "key.json"), JSON.stringify({ type: "api_key" }));
  expect(await refused(vertex({ credentials_file: join(directory, "key.json") }))).toContain("neither a service_account nor an authorized_user");
  const denied = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "" },
    Response.json({ error: { code: 403, message: "Billing is disabled for this project.", status: "PERMISSION_DENIED" } }, { status: 403 }));
  expect(denied.status).toBe(400);
  expect(JSON.parse(denied.text).error.message).toStartWith("Provider HTTP 403: ");
  expect(denied.text).toContain("Billing is disabled for this project.");
});
