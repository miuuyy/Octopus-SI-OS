import { expect, test } from "bun:test";
import { createProviderAdapter } from "../src/adapters.ts";
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import { getProviderRegistryEntry } from "../node_modules/@bitkyc08/opencodex/src/providers/registry.ts";
import { providerConfigSeed } from "../node_modules/@bitkyc08/opencodex/src/providers/derive.ts";
import { createTranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";

test("shared factory preserves OpenCode Go mixed wires, caller prompts and conversation affinity", async () => {
  const entry = getProviderRegistryEntry("opencode-go")!;
  expect(entry.defaultModel).toBe("kimi-k2.7-code");
  const provider = { ...providerConfigSeed(entry), apiKey: "fixture-only" };
  const prompt = "You are Codex, an agent based on GPT-6.\nYou are a coding agent. Do not claim to be GPT-5 or to be made by OpenAI.";
  const cases = [
    ["gemini-4-argon-contributor", "google", "https://opencode.ai/inference/go/v1/models/gemini-4-argon-contributor:generateContent"],
    ["claude-haiku-5-5", "anthropic", `${provider.baseUrl}/messages`],
    [entry.defaultModel!, "openai-chat", `${provider.baseUrl}/chat/completions`],
  ] as const;
  for (const [modelId, wire, url] of cases) {
    const sessions: string[] = [];
    for (const conversation of ["conversation-a", "conversation-a", "conversation-b"]) {
      const budget = createTranslatorBudget();
      try {
        const request = new Request("http://127.0.0.1/v1/responses", { method: "POST", headers: { session_id: conversation } });
        const adapter = createProviderAdapter(provider, {
          adapter: provider.adapter, providerId: entry.id, ownerId: "fixture-owner", modelId,
          request, budget, cacheRetention: "none",
        });
        const parsed = parseRequest({ model: modelId, instructions: prompt, input: "hello", stream: false }, { preserveCallerContext: true });
        const built = await adapter.buildRequest(parsed, { headers: request.headers, providerName: entry.id, translatorBudget: budget });
        const body = JSON.parse(built.body), headers = new Headers(built.headers);
        expect(adapter.name).toBe(wire);
        expect(built.url).toBe(url);
        expect(built.method).toBe("POST");
        expect(headers.get("x-opencode-session")).toMatch(/^ocx_[a-f0-9]{32}$/);
        sessions.push(headers.get("x-opencode-session")!);
        expect(headers.get(wire === "anthropic" ? "x-api-key" : "authorization"))
          .toBe(wire === "anthropic" ? "fixture-only" : "Bearer fixture-only");
        if (wire === "google") {
          expect(body.systemInstruction.parts).toEqual([{ text: prompt }]);
          expect(body.contents).toEqual([{ role: "user", parts: [{ text: "hello" }] }]);
        } else {
          expect(body.model).toBe(modelId);
          expect(wire === "anthropic" ? body.system[0].text : body.messages[0].content).toBe(prompt);
          expect(body.messages.at(-1)).toEqual({ role: "user", content: "hello" });
        }
      } finally { budget.dispose(); }
    }
    expect(sessions[0]).toBe(sessions[1]);
    expect(sessions[0]).not.toBe(sessions[2]);
  }
});
