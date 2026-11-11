import { expect, test } from "bun:test";
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import { createRegisteredAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/registry.ts";
import { createTranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";
import type { OcxParsedRequest, OcxProviderConfig } from "../node_modules/@bitkyc08/opencodex/src/types.ts";

const caller = { preserveCallerContext: true };
const instruction = "You are Codex, an agent based on GPT-6.\nYou are a coding agent. Do not claim to be GPT-5 or to be made by OpenAI.\nvisualize/fixture/chart.html";
const narration = "I will inspect the history. The second sentence must survive.\nAnd this third line too.";
const description = "Inspect one row. You are a coding agent powered by the parent-model.";
const envelope = (value: object) => `ocxr1:${Buffer.from(JSON.stringify(value)).toString("base64")}`;
const request = (reasoning: unknown[] = []) => ({
  model: "gemini-3-pro", instructions: instruction, stream: false,
  tools: [{ type: "namespace", name: "octopus", tools: [{ type: "function", name: "inspect", description, parameters: { type: "object", properties: { row: { type: "string" } } } }] }],
  input: [
    { role: "user", content: "Original request." },
    ...reasoning,
    { role: "assistant", content: narration, phase: "commentary" },
    { type: "function_call", call_id: "call_one", name: "inspect", namespace: "octopus", arguments: "{\"row\":\"a\"}" },
    { type: "function_call_output", call_id: "call_one", output: "row a" },
    { role: "developer", content: instruction },
    { role: "user", content: "Continue." },
  ],
});

const families = ["openai-chat", "anthropic", "google", "openai-responses", "azure"] as const;
async function wire(family: typeof families[number], parsed: OcxParsedRequest) {
  const adapter = createRegisteredAdapter({
    adapter: family, baseUrl: family === "azure" ? "https://caller-test.openai.azure.com/openai/v1" : "https://caller-test.invalid/v1",
    authMode: "key", apiKey: "contract-test-only", models: ["*"],
  } as OcxProviderConfig, { cacheRetention: "none" });
  const budget = createTranslatorBudget();
  try {
    return JSON.parse((await adapter.buildRequest(parsed, { headers: new Headers(), translatorBudget: budget })).body);
  } finally { budget.dispose(); }
}

for (const family of families) {
  test(`${family}: the caller's instructions, tool descriptions and history reach the wire as written`, async () => {
    const body = await wire(family, parseRequest(request(), caller));
    const prompt = family === "google" ? body.systemInstruction.parts.map((part: any) => part.text).join("")
      : family === "anthropic" ? body.system.map((part: any) => part.text).join("")
      : family === "openai-chat" ? body.messages[0].content : body.instructions;
    expect(prompt).toBe(instruction);
    const text = JSON.stringify(body);
    for (const exact of [instruction, narration, description]) expect(text).toContain(JSON.stringify(exact).slice(1, -1));
    expect(text).not.toContain("Output style for this session");
  });

  test(`${family}: replayed reasoning beside a tool call never refuses the next request`, async () => {
    for (const reasoning of [
      { type: "reasoning", summary: [{ type: "summary_text", text: "plan" }], encrypted_content: envelope({ txt: "plan" }) },
      { type: "reasoning", summary: [], encrypted_content: envelope({ txt: "plan", sig: "fixtureSignature0123456789" }) },
      { type: "reasoning", content: [{ type: "reasoning_text", text: "plan" }] },
    ]) {
      const body = await wire(family, parseRequest(request([reasoning]), caller));
      expect(JSON.stringify(body)).toContain("row a");
    }
  });
}

test("the mode is a parse option the request body cannot set, and upstream defaults stay intact without it", async () => {
  expect(parseRequest({ ...request(), preserveCallerContext: true }).context.preserveCallerContext).toBeUndefined();
  const body = await wire("google", parseRequest(request()));
  const prompt = body.systemInstruction.parts.map((part: any) => part.text).join("");
  expect(prompt).toContain("Output style for this session");
  expect(prompt).not.toContain("based on GPT-6");
});
