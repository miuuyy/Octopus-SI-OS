import { spawn } from "node:child_process";
import { createRegisteredAdapter, effectiveAdapterContract } from "../node_modules/@bitkyc08/opencodex/src/adapters/registry.ts";
import { resolveWireProtocolOverride } from "../node_modules/@bitkyc08/opencodex/src/server/adapter-resolve.ts";
import { resolveOpenCodeGoTransport } from "../node_modules/@bitkyc08/opencodex/src/providers/opencode-go-transport.ts";
import { getOrAllocateRequestSessionLane } from "../node_modules/@bitkyc08/opencodex/src/server/request-log-conversation.ts";
import type { OcxProviderConfig } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import type { ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import { createClaudeCliAdapter } from "./claude-cli.ts";
import { createGrokCliAdapter } from "./grok-cli.ts";
import { wrapReasoningDetailsAdapter } from "./reasoning-details.ts";
import type { TranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";

type Context = {
  adapter: string;
  providerId: string;
  ownerId: string;
  modelId: string;
  request: Request;
  budget: TranslatorBudget;
  cacheRetention?: "none" | "short" | "long";
};

const cli: Record<string, (provider: OcxProviderConfig) => ProviderAdapter> = {
  "claude-cli": (provider) => {
    const directory = process.env.TMPDIR;
    if (!directory) throw new Error("CLI private temporary directory is missing");
    return createClaudeCliAdapter(provider, {
      spawn: (command, args, options) => spawn(command, args, {
        ...options, env: { ...options.env, CLAUDE_CODE_TMPDIR: directory },
      }),
      usageAdmission: async () => ({ state: "unknown", checkedAt: Date.now() }),
      usageRefusal: () => undefined,
    });
  },
  "grok-cli": createGrokCliAdapter,
};
const registered = new Set(["anthropic", "azure", "google", "openai-chat", "openai-responses"]);

export function supportsAdapter(name: string): boolean {
  return Object.hasOwn(cli, name) || registered.has(name);
}

export function createProviderAdapter(provider: OcxProviderConfig, context: Context): ProviderAdapter {
  if (Object.hasOwn(cli, context.adapter)) return cli[context.adapter]!(provider);
  const wire = resolveWireProtocolOverride(context.providerId, context.modelId, provider, "responses");
  const transport = resolveOpenCodeGoTransport(wire, getOrAllocateRequestSessionLane(context.request), provider);
  if (!registered.has(transport.adapter)) throw new Error(`Adapter '${transport.adapter}' is not served by the bridge`);
  const adapter = createRegisteredAdapter(transport, {
    cacheRetention: context.cacheRetention,
    providerId: context.providerId,
  });
  if (adapter.runTurn || adapter.localTerminal) {
    throw new Error(`Adapter '${transport.adapter}' requires an explicit execution contract`);
  }
  return effectiveAdapterContract(transport.adapter).wire === "openai-chat"
    ? wrapReasoningDetailsAdapter(adapter, { owner: context.ownerId, model: context.modelId, format: "openai-chat-v1" }, context.budget)
    : adapter;
}
