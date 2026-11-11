import { createProviderAdapter, supportsAdapter } from "./adapters.ts";
import { isAbsolute } from "node:path";
import { accountStatus, loginOAuthAccount, logoutOAuthAccount, resolveOAuthAccount, OAuthAccountError, type OAuthAccountConfig } from "./account.ts";
import type { ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import { runGrokCaptureMcpServer } from "./grok-cli.ts";
import { runCodeBuddyMcpServer } from "../node_modules/@bitkyc08/opencodex/src/adapters/codebuddy/mcp-server.ts";
import { COALESCE_MAX_CHUNK_LENGTH, createAdapterEventQueue } from "../node_modules/@bitkyc08/opencodex/src/adapters/run-turn-queue.ts";
import {
  bridgeToResponsesSSE,
  buildResponseJSON,
  formatErrorResponse,
} from "../node_modules/@bitkyc08/opencodex/src/bridge.ts";
import { PROVIDER_REGISTRY } from "../node_modules/@bitkyc08/opencodex/src/providers/registry.ts";
import { enrichProviderFromRegistry, providerConfigSeed } from "../node_modules/@bitkyc08/opencodex/src/providers/derive.ts";
import { buildToolBridgeMaps } from "../node_modules/@bitkyc08/opencodex/src/server/responses/collaboration.ts";
import { guardEmptyCompletionEventStream } from "../node_modules/@bitkyc08/opencodex/src/server/responses/empty-completion-guard.ts";
import {
  createTranslatorBudget,
  isTranslatorBudgetExceededError,
  type TranslatorBudget,
} from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";
import {
  assertProviderDestinationAllowed,
  providerDestinationResolvedError,
} from "../node_modules/@bitkyc08/opencodex/src/lib/destination-policy.ts";
import { isLocalUpstream } from "../node_modules/@bitkyc08/opencodex/src/lib/local-upstream.ts";
import { redactSecretString } from "../node_modules/@bitkyc08/opencodex/src/lib/redact.ts";
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import type {
  AdapterEvent,
  OcxParsedRequest,
  OcxProviderConfig,
} from "../node_modules/@bitkyc08/opencodex/src/types.ts";

type AdapterName = string;

type ProviderOptions = {
  annotate_empty_tool_outputs?: boolean;
  api_key_transport?: "bearer" | "x-api-key";
  auth_mode?: "key" | "oauth";
  codex_tool_mode?: "code_mode_only" | "shell";
  decodes_native_compaction_blobs?: boolean;
  direct_gemini_wire_renames?: boolean;
  preserve_responses_reasoning_content?: boolean;
  requires_adjacent_responses_tool_results?: boolean;
  responses_path?: string;
  stateless_responses?: boolean;
  supports_service_tier?: boolean;
  upstream_http_version?: "auto" | "h1" | "h2" | "http1.1" | "http2";
};

type RuntimeProvider = {
  adapter: AdapterName;
  allow_private_network: boolean;
  api_key?: string;
  auth_header?: string;
  auth_value_prefix: string;
  base_url: string;
  cache_retention?: "long" | "none" | "short";
  default_model: string;
  headers: Record<string, string>;
  id: string;
  model_aliases: Record<string, string>;
  models: string[];
  options: ProviderOptions;
  registry_entry?: string;
  account?: OAuthAccountConfig;
};

type BridgeConfig = {
  interface_id: string;
  limits: {
    max_request_bytes: number;
    max_response_bytes: number;
    max_translated_call_bytes: number;
    max_translated_turn_bytes: number;
    request_timeout_ms: number;
  };
  provider: RuntimeProvider;
  schema_version: 1;
};

const optionNames = new Set([
  "annotate_empty_tool_outputs",
  "api_key_transport",
  "auth_mode",
  "codex_tool_mode",
  "decodes_native_compaction_blobs",
  "direct_gemini_wire_renames",
  "preserve_responses_reasoning_content",
  "requires_adjacent_responses_tool_results",
  "responses_path",
  "stateless_responses",
  "supports_service_tier",
  "upstream_http_version",
]);

const isObject = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function exactKeys(value: Record<string, unknown>, allowed: readonly string[], path: string): void {
  const names = new Set(allowed);
  for (const key of Object.keys(value)) {
    if (!names.has(key)) throw new Error(`${path} contains unsupported field '${key}'`);
  }
}

function stringValue(value: unknown, path: string): string {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    throw new Error(`${path} must be a non-empty trimmed string`);
  }
  return value;
}

function accountValue(value: unknown): OAuthAccountConfig {
  if (!isObject(value)) throw new Error("provider.account must be an object");
  exactKeys(value, ["provider", "directory"], "provider.account");
  if (value.provider !== "google-antigravity") throw new Error("provider.account.provider is unsupported");
  const directory = stringValue(value.directory, "provider.account.directory");
  if (!isAbsolute(directory) || directory.includes("\0")) throw new Error("provider.account.directory must be absolute");
  return { provider: value.provider, directory };
}

function boundedInteger(value: unknown, fallback: number, min: number, max: number, path: string): number {
  const resolved = value === undefined ? fallback : value;
  if (!Number.isInteger(resolved) || (resolved as number) < min || (resolved as number) > max) {
    throw new Error(`${path} must be an integer between ${min} and ${max}`);
  }
  return resolved as number;
}

function stringSet(value: unknown, path: string): string[] {
  if (!Array.isArray(value) || value.length === 0) throw new Error(`${path} must be a non-empty array`);
  const values = value.map((item, index) => stringValue(item, `${path}[${index}]`));
  if (new Set(values).size !== values.length) throw new Error(`${path} must not contain duplicates`);
  return values;
}

function headersValue(value: unknown, path: string): Record<string, string> {
  if (value === undefined) return {};
  if (!isObject(value)) throw new Error(`${path} must be an object`);
  const headers: Record<string, string> = {};
  for (const [name, raw] of Object.entries(value)) {
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)) throw new Error(`${path} contains invalid header name`);
    headers[name] = stringValue(raw, `${path}.${name}`);
  }
  return headers;
}

function modelAliasesValue(value: unknown, models: string[]): Record<string, string> {
  if (value === undefined) return {};
  if (!isObject(value)) throw new Error("provider.model_aliases must be an object");
  const aliases: Record<string, string> = {};
  for (const [declared, upstream] of Object.entries(value)) {
    if (!models.includes(declared)) throw new Error(`provider.model_aliases names undeclared model '${declared}'`);
    aliases[declared] = stringValue(upstream, `provider.model_aliases.${declared}`);
  }
  return aliases;
}

function optionsValue(value: unknown): ProviderOptions {
  if (value === undefined) return {};
  if (!isObject(value)) throw new Error("provider.options must be an object");
  for (const key of Object.keys(value)) {
    if (!optionNames.has(key)) throw new Error(`provider.options contains unsupported field '${key}'`);
  }
  const booleanNames = [
    "annotate_empty_tool_outputs",
    "decodes_native_compaction_blobs",
    "direct_gemini_wire_renames",
    "preserve_responses_reasoning_content",
    "requires_adjacent_responses_tool_results",
    "stateless_responses",
    "supports_service_tier",
  ];
  for (const key of booleanNames) {
    if (value[key] !== undefined && typeof value[key] !== "boolean") {
      throw new Error(`provider.options.${key} must be boolean`);
    }
  }
  if (value.responses_path !== undefined) {
    const path = stringValue(value.responses_path, "provider.options.responses_path");
    if (!path.startsWith("/") || path.includes("?") || path.includes("#") || path.includes("://")) {
      throw new Error("provider.options.responses_path must be an absolute resource path");
    }
  }
  if (value.api_key_transport !== undefined && value.api_key_transport !== "bearer" && value.api_key_transport !== "x-api-key") {
    throw new Error("provider.options.api_key_transport is invalid");
  }
  if (value.auth_mode !== undefined && value.auth_mode !== "key" && value.auth_mode !== "oauth") {
    throw new Error("provider.options.auth_mode is invalid");
  }
  if (value.codex_tool_mode !== undefined && value.codex_tool_mode !== "code_mode_only" && value.codex_tool_mode !== "shell") {
    throw new Error("provider.options.codex_tool_mode is invalid");
  }
  if (value.upstream_http_version !== undefined && !["auto", "h1", "h2", "http1.1", "http2"].includes(String(value.upstream_http_version))) {
    throw new Error("provider.options.upstream_http_version is invalid");
  }
  return value as ProviderOptions;
}

function parseConfig(value: unknown): BridgeConfig {
  if (!isObject(value)) throw new Error("bridge config must be an object");
  exactKeys(value, ["schema_version", "interface_id", "provider", "limits"], "bridge config");
  if (value.schema_version !== 1) throw new Error("bridge config schema_version must be 1");
  const interfaceId = stringValue(value.interface_id, "interface_id");
  if (!isObject(value.provider)) throw new Error("provider must be an object");
  exactKeys(value.provider, [
    "id",
    "adapter",
    "base_url",
    "models",
    "default_model",
    "api_key",
    "auth_header",
    "auth_value_prefix",
    "headers",
    "allow_private_network",
    "cache_retention",
    "options",
    "registry_entry",
    "account",
    "model_aliases",
  ], "provider");
  const adapter = stringValue(value.provider.adapter, "provider.adapter") as AdapterName;
  if (!supportsAdapter(adapter)) throw new Error(`provider.adapter '${adapter}' is unsupported`);
  const baseUrl = stringValue(value.provider.base_url, "provider.base_url");
  const parsedUrl = new URL(baseUrl);
  if (parsedUrl.protocol !== "http:" && parsedUrl.protocol !== "https:") throw new Error("provider.base_url must use http or https");
  if (parsedUrl.username || parsedUrl.password || parsedUrl.hash) throw new Error("provider.base_url must not contain credentials or a fragment");
  const allowPrivateNetwork = value.provider.allow_private_network === true;
  if (parsedUrl.protocol === "http:" && !allowPrivateNetwork) {
    throw new Error("provider.base_url requires https unless allow_private_network is true");
  }
  assertProviderDestinationAllowed(interfaceId, {
    baseUrl,
    allowPrivateNetwork,
  });
  const models = stringSet(value.provider.models, "provider.models");
  const defaultModel = stringValue(value.provider.default_model, "provider.default_model");
  const account = value.provider.account === undefined ? undefined : accountValue(value.provider.account);
  if (account && value.provider.api_key !== undefined) throw new Error("An account provider cannot also declare an API key");
  if (!models.includes(defaultModel)) throw new Error("provider.default_model is outside provider.models");
  const registryEntry = value.provider.registry_entry === undefined
    ? undefined
    : stringValue(value.provider.registry_entry, "provider.registry_entry");
  if (registryEntry !== undefined && !PROVIDER_REGISTRY.some((entry) => entry.id === registryEntry)) {
    throw new Error(`provider.registry_entry '${registryEntry}' is not an OpenCodex registry entry`);
  }
  const modelAliases = modelAliasesValue(value.provider.model_aliases, models);
  const apiKey = value.provider.api_key === undefined ? undefined : stringValue(value.provider.api_key, "provider.api_key");
  const authHeader = value.provider.auth_header === undefined ? undefined : stringValue(value.provider.auth_header, "provider.auth_header");
  const authValuePrefix = value.provider.auth_value_prefix === undefined
    ? ""
    : typeof value.provider.auth_value_prefix === "string"
      ? value.provider.auth_value_prefix
      : (() => { throw new Error("provider.auth_value_prefix must be a string"); })();
  const cacheRetention = value.provider.cache_retention;
  if (cacheRetention !== undefined && cacheRetention !== "none" && cacheRetention !== "short" && cacheRetention !== "long") {
    throw new Error("provider.cache_retention is invalid");
  }
  if (value.limits !== undefined && !isObject(value.limits)) throw new Error("limits must be an object");
  const limits = isObject(value.limits) ? value.limits : {};
  exactKeys(limits, [
    "max_request_bytes",
    "max_response_bytes",
    "max_translated_call_bytes",
    "max_translated_turn_bytes",
    "request_timeout_ms",
  ], "limits");
  return {
    schema_version: 1,
    interface_id: interfaceId,
    provider: {
      id: stringValue(value.provider.id, "provider.id"),
      adapter,
      ...(registryEntry === undefined ? {} : { registry_entry: registryEntry }),
      ...(account === undefined ? {} : { account }),
      model_aliases: modelAliases,
      base_url: baseUrl,
      models,
      default_model: defaultModel,
      ...(apiKey === undefined ? {} : { api_key: apiKey }),
      ...(authHeader === undefined ? {} : { auth_header: authHeader }),
      auth_value_prefix: authValuePrefix,
      headers: headersValue(value.provider.headers, "provider.headers"),
      allow_private_network: allowPrivateNetwork,
      ...(cacheRetention === undefined ? {} : { cache_retention: cacheRetention }),
      options: optionsValue(value.provider.options),
    },
    limits: {
      max_request_bytes: boundedInteger(limits.max_request_bytes, 32 * 1024 * 1024, 1024, 256 * 1024 * 1024, "limits.max_request_bytes"),
      max_response_bytes: boundedInteger(limits.max_response_bytes, 64 * 1024 * 1024, 1024, 256 * 1024 * 1024, "limits.max_response_bytes"),
      max_translated_call_bytes: boundedInteger(limits.max_translated_call_bytes, 2 * 1024 * 1024, 1024, 32 * 1024 * 1024, "limits.max_translated_call_bytes"),
      max_translated_turn_bytes: boundedInteger(limits.max_translated_turn_bytes, 32 * 1024 * 1024, 1024, 128 * 1024 * 1024, "limits.max_translated_turn_bytes"),
      request_timeout_ms: boundedInteger(limits.request_timeout_ms, 300000, 1000, 1800000, "limits.request_timeout_ms"),
    },
  };
}

function upstreamModel(config: RuntimeProvider, model: string): string {
  return config.model_aliases[model] ?? model;
}

function upstreamProvider(config: RuntimeProvider): OcxProviderConfig {
  const entry = config.registry_entry === undefined
    ? undefined
    : PROVIDER_REGISTRY.find((row) => row.id === config.registry_entry);
  const seed: Partial<OcxProviderConfig> = entry ? providerConfigSeed(entry) : {};
  const headers = { ...(seed.headers ?? {}), ...config.headers };
  if (config.api_key !== undefined && config.auth_header !== undefined) {
    headers[config.auth_header] = `${config.auth_value_prefix}${config.api_key}`;
  }
  const models = config.models.map((model) => upstreamModel(config, model));
  const provider: OcxProviderConfig = {
    ...seed,
    adapter: config.adapter,
    baseUrl: config.base_url,
    models,
    selectedModels: [...models],
    defaultModel: upstreamModel(config, config.default_model),
    liveModels: false,
    allowPrivateNetwork: config.allow_private_network,
    authMode: config.api_key !== undefined ? "key" : seed.authMode === "local" ? "local" : "key",
    ...(config.api_key === undefined ? {} : { apiKey: config.api_key }),
    ...(Object.keys(headers).length === 0 ? {} : { headers }),
    ...(config.options.api_key_transport === undefined ? {} : { apiKeyTransport: config.options.api_key_transport }),
    ...(config.options.auth_mode === undefined ? {} : { authMode: config.options.auth_mode }),
    ...(config.options.codex_tool_mode === undefined ? {} : { codexToolMode: config.options.codex_tool_mode }),
    ...(config.options.responses_path === undefined ? {} : { responsesPath: config.options.responses_path }),
    ...(config.options.stateless_responses === undefined ? {} : { statelessResponses: config.options.stateless_responses }),
    ...(config.options.requires_adjacent_responses_tool_results === undefined ? {} : { requiresAdjacentResponsesToolResults: config.options.requires_adjacent_responses_tool_results }),
    ...(config.options.annotate_empty_tool_outputs === undefined ? {} : { annotateEmptyToolOutputs: config.options.annotate_empty_tool_outputs }),
    ...(config.options.preserve_responses_reasoning_content === undefined ? {} : { preserveResponsesReasoningContent: config.options.preserve_responses_reasoning_content }),
    ...(config.options.decodes_native_compaction_blobs === undefined ? {} : { decodesNativeCompactionBlobs: config.options.decodes_native_compaction_blobs }),
    ...(config.options.direct_gemini_wire_renames === undefined ? {} : { directGeminiWireRenames: config.options.direct_gemini_wire_renames }),
    ...(config.options.supports_service_tier === undefined ? {} : { supportsServiceTier: config.options.supports_service_tier }),
    ...(config.options.upstream_http_version === undefined ? {} : { upstreamHttpVersion: config.options.upstream_http_version }),
  };
  if (entry) enrichProviderFromRegistry(entry.id, provider);
  return provider;
}

async function adapterFor(config: BridgeConfig, request: Request, modelId: string, budget: TranslatorBudget, signal = request.signal): Promise<ProviderAdapter> {
  const provider = upstreamProvider(config.provider);
  if (config.provider.account) {
    const account = await resolveOAuthAccount(config.provider.account, signal);
    provider.apiKey = account.accessToken;
    provider.project = account.projectId;
  }
  return createProviderAdapter(provider, {
    adapter: config.provider.adapter,
    providerId: config.provider.registry_entry ?? config.provider.id,
    ownerId: config.interface_id,
    modelId,
    request,
    cacheRetention: config.provider.cache_retention,
    budget,
  });
}

async function boundedBody(request: Request, limit: number): Promise<Uint8Array> {
  const declared = request.headers.get("content-length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > limit)) throw new Error("request body exceeds the configured limit");
  if (request.body === null) return new Uint8Array();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const next = await reader.read();
    if (next.done) break;
    size += next.value.byteLength;
    if (size > limit) {
      await reader.cancel("request body limit exceeded");
      throw new Error("request body exceeds the configured limit");
    }
    chunks.push(next.value);
  }
  const body = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

async function boundedText(response: Response, limit: number): Promise<string> {
  if (response.body === null) return "";
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const next = await reader.read();
    if (next.done) break;
    size += next.value.byteLength;
    if (size > limit) {
      await reader.cancel("response body limit exceeded");
      throw new Error("upstream response exceeds the configured limit");
    }
    chunks.push(next.value);
  }
  const body = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(body);
}

function boundedStream(response: Response, limit: number, abort: AbortController): Response {
  if (response.body === null) return response;
  const reader = response.body.getReader();
  let size = 0;
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const next = await reader.read();
        if (next.done) {
          controller.close();
          return;
        }
        size += next.value.byteLength;
        if (size > limit) {
          abort.abort("response body limit exceeded");
          await reader.cancel("response body limit exceeded");
          controller.error(new Error("upstream response exceeds the configured limit"));
          return;
        }
        controller.enqueue(next.value);
      } catch (error) {
        controller.error(error);
      }
    },
    async cancel(reason) {
      abort.abort(reason);
      await reader.cancel(reason);
    },
  });
  return new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers });
}

function disposingStream(stream: ReadableStream<Uint8Array>, budget: TranslatorBudget, abort: AbortController, cleanup: () => void): ReadableStream<Uint8Array> {
  const reader = stream.getReader();
  let settled = false;
  const settle = () => {
    if (settled) return;
    settled = true;
    cleanup();
    budget.dispose();
  };
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const next = await reader.read();
        if (next.done) {
          settle();
          controller.close();
        } else {
          controller.enqueue(next.value);
        }
      } catch (error) {
        settle();
        controller.error(error);
      }
    },
    async cancel(reason) {
      abort.abort(reason);
      settle();
      await reader.cancel(reason);
    },
  });
}

async function* listedEvents(events: AdapterEvent[]): AsyncGenerator<AdapterEvent> {
  yield* events;
}

const SSE_HEADERS = {
  "Cache-Control": "no-cache",
  "Connection": "keep-alive",
  "Content-Type": "text/event-stream",
  "X-Accel-Buffering": "no",
};

function deliver(
  parsed: OcxParsedRequest,
  responseModel: string,
  events: AsyncIterable<AdapterEvent> | AdapterEvent[],
  maps: ReturnType<typeof buildToolBridgeMaps>,
  budget: TranslatorBudget,
  abort: AbortController,
  stallTimeoutSec: number | undefined,
  cleanup: () => void,
): Promise<Response> | Response {
  const options = {
    translatorBudget: budget,
    replayCacheScope: parsed._reasoningReplayScope,
    hideThinkingSummary: parsed.options.hideThinkingSummary,
    hideRawReasoning: parsed.options.hideRawReasoning,
    declaredToolNames: maps.declaredToolNames,
    bareCustomToolNames: maps.bareCustomToolNames,
    enforceDeclaredToolNames: true,
    terminalRefusalAsFailed: true,
    toolParameterSchemas: maps.toolParameterSchemas,
  };
  if (parsed.stream) {
    const stream = bridgeToResponsesSSE(
      Array.isArray(events) ? listedEvents(events) : events,
      responseModel,
      maps.toolNsMap,
      maps.freeformToolNames,
      maps.toolSearchToolNames,
      () => abort.abort("client cancelled"),
      2000,
      { ...options, stallTimeoutSec },
    );
    return new Response(disposingStream(stream, budget, abort, cleanup), { headers: SSE_HEADERS });
  }
  return (async () => {
    try {
      const collected: AdapterEvent[] = [];
      for await (const event of events) collected.push(event);
      const response = buildResponseJSON(collected, responseModel, {
        ...options,
        toolNsMap: maps.toolNsMap,
        freeformToolNames: maps.freeformToolNames,
        toolSearchToolNames: maps.toolSearchToolNames,
      });
      return Response.json(response);
    } catch (error) {
      return formatErrorResponse(502, "upstream_error", error instanceof Error ? error.message : "provider response translation failed");
    } finally {
      cleanup();
      budget.dispose();
    }
  })();
}

function flattenRootSchemaAlternatives(parsed: OcxParsedRequest): void {
  for (const tool of parsed.context.tools ?? []) {
    const schema = tool.parameters;
    if (!isObject(schema) || !("oneOf" in schema || "anyOf" in schema || "allOf" in schema)) continue;
    const { oneOf, anyOf, allOf, ...rest } = schema;
    const properties: Record<string, unknown> = isObject(rest.properties) ? { ...rest.properties } : {};
    const required = new Set(stringList(rest.required));
    const notes: string[] = [];
    for (const branch of Array.isArray(allOf) ? allOf.filter(isObject) : []) {
      mergeBranchProperties(properties, branch);
      for (const name of stringList(branch.required)) required.add(name);
    }
    for (const [alternatives, quantity] of [[oneOf, "exactly one"], [anyOf, "at least one"]] as const) {
      if (!Array.isArray(alternatives)) continue;
      const shapes = alternatives.filter(isObject).map(branch => {
        mergeBranchProperties(properties, branch);
        const names = stringList(branch.required);
        return names.length ? `(${names.join(" + ")})` : "(no further fields)";
      });
      notes.push(`Arguments must include ${quantity} of these field sets: ${shapes.join(" | ")}.`);
    }
    tool.parameters = { ...rest, type: "object", properties, ...(required.size ? { required: [...required] } : {}) };
    if (notes.length) tool.description = [tool.description, ...notes].filter(Boolean).join("\n\n");
  }
}

function mergeBranchProperties(properties: Record<string, unknown>, branch: Record<string, unknown>): void {
  if (!isObject(branch.properties)) return;
  for (const [name, value] of Object.entries(branch.properties)) {
    if (!(name in properties)) properties[name] = value;
  }
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function upstreamErrorReason(text: string, detail: string): string {
  let body: unknown;
  try { body = JSON.parse(text); } catch { return detail; }
  const metadata = isObject(body) && isObject(body.error) && isObject(body.error.metadata) ? body.error.metadata : undefined;
  const raw = typeof metadata?.raw === "string" ? metadata.raw.trim() : "";
  let reason = raw || (isObject(body) && isObject(body.error) && typeof body.error.message === "string" ? body.error.message
    : isObject(body) && typeof body.message === "string" ? body.message : detail);
  for (let depth = 0; depth < 3; depth++) {
    try {
      const inner: unknown = JSON.parse(reason);
      const message = isObject(inner) && isObject(inner.error) && typeof inner.error.message === "string" ? inner.error.message
        : isObject(inner) && typeof inner.message === "string" ? inner.message : undefined;
      if (!message || message === reason) break;
      reason = message;
    } catch { break; }
  }
  reason = redactSecretString(reason).slice(0, 600);
  return raw && detail && detail !== reason ? `${detail}: ${reason}` : reason;
}

async function responsesRequest(request: Request, config: BridgeConfig): Promise<Response> {
  let raw: unknown;
  try {
    const bytes = await boundedBody(request, config.limits.max_request_bytes);
    raw = JSON.parse(new TextDecoder().decode(bytes));
  } catch (error) {
    return formatErrorResponse(400, "invalid_request_error", error instanceof Error ? error.message : "invalid request body");
  }
  if (!isObject(raw)) return formatErrorResponse(400, "invalid_request_error", "request body must be an object");
  if (typeof raw.model !== "string" || !config.provider.models.includes(raw.model)) {
    return formatErrorResponse(400, "invalid_request_error", "request model is outside the exact interface model grant");
  }
  const responseModel = raw.model;
  const modelId = upstreamModel(config.provider, responseModel);
  const abort = new AbortController();
  const cancel = () => abort.abort(request.signal.reason);
  request.signal.addEventListener("abort", cancel, { once: true });
  if (request.signal.aborted) cancel();
  const timer = setTimeout(() => abort.abort("request timeout"), config.limits.request_timeout_ms);
  const cleanup = () => {
    clearTimeout(timer);
    request.signal.removeEventListener("abort", cancel);
  };
  let parsed: OcxParsedRequest;
  const budget = createTranslatorBudget({
    maxCallArgumentBytes: config.limits.max_translated_call_bytes,
    maxTurnBytes: config.limits.max_translated_turn_bytes,
  });
  let maps: ReturnType<typeof buildToolBridgeMaps>;
  let adapter: ProviderAdapter;
  try {
    parsed = parseRequest(modelId === responseModel ? raw : { ...raw, model: modelId }, { preserveCallerContext: true });
    if (parsed.modelId !== modelId) throw new Error("parsed model disagrees with the exact interface model grant");
    flattenRootSchemaAlternatives(parsed);
    maps = buildToolBridgeMaps(parsed, budget);
    adapter = await adapterFor(config, request, modelId, budget, abort.signal);
  } catch (error) {
    cleanup();
    budget.dispose();
    const message = isTranslatorBudgetExceededError(error)
      ? "request translation buffer exceeded the configured limit"
      : error instanceof Error ? error.message : "request parsing failed";
    return formatErrorResponse(isTranslatorBudgetExceededError(error) ? 413 : error instanceof OAuthAccountError ? 401 : 400,
      error instanceof OAuthAccountError ? "authentication_error" : "invalid_request_error", message);
  }
  const stallTimeoutSec = isLocalUpstream(config.provider.base_url)
    ? Math.ceil(config.limits.request_timeout_ms / 1000)
    : undefined;
  if (adapter.localTerminal?.(parsed)) {
    return deliver(parsed, responseModel, [{ type: "done", endTurn: true, usage: { inputTokens: 0, outputTokens: 0, totalTokens: 0 } }], maps, budget, abort, stallTimeoutSec, cleanup);
  }
  if (adapter.runTurn !== undefined) {
    return deliver(parsed, responseModel, guardEmptyCompletionEventStream({
      firstEvents: turnEvents(adapter, parsed, budget, abort),
      continuation: () => turnEvents(adapter, parsed, budget, abort),
    }), maps, budget, abort, stallTimeoutSec, cleanup);
  }
  const first = await upstreamAttempt(config, request, adapter, parsed, budget, abort);
  if (first instanceof Response) {
    cleanup();
    budget.dispose();
    return first;
  }
  clearTimeout(timer);
  return deliver(parsed, responseModel, guardEmptyCompletionEventStream({
    firstEvents: first,
    continuation: async () => {
      const retried = await upstreamAttempt(config, request, adapter, parsed, budget, abort);
      if (retried instanceof Response) throw new Error(`retry of an empty completion failed upstream with ${retried.status}`);
      return retried;
    },
  }), maps, budget, abort, stallTimeoutSec, cleanup);
}

async function upstreamAttempt(
  config: BridgeConfig,
  request: Request,
  adapter: ProviderAdapter,
  parsed: OcxParsedRequest,
  budget: TranslatorBudget,
  abort: AbortController,
): Promise<Response | AsyncIterable<AdapterEvent>> {
  let upstream: Response;
  let built: Awaited<ReturnType<ProviderAdapter["buildRequest"]>> | undefined;
  try {
    built = await adapter.buildRequest(parsed, {
      headers: request.headers,
      providerName: config.provider.registry_entry ?? config.provider.id,
      translatorBudget: budget,
      abortSignal: abort.signal,
    });
    const target = new URL(built.url);
    const declared = new URL(config.provider.base_url);
    if (target.origin !== declared.origin) throw new Error("adapter target escaped the declared provider origin");
    upstream = adapter.fetchResponse
      ? await adapter.fetchResponse(built, { abortSignal: abort.signal, stream: parsed.stream, returnRawErrors: true })
      : await fetch(target, {
        method: built.method,
        headers: built.headers,
        body: built.body,
        signal: abort.signal,
        redirect: "error",
      });
  } catch (error) {
    built?.releaseBodyObservation?.();
    const message = error instanceof Error ? error.message : "provider request failed";
    return isTranslatorBudgetExceededError(error)
      ? formatErrorResponse(413, "request_too_large", "request translation buffer exceeded the configured limit")
      : formatErrorResponse(built ? 502 : 400, built ? "upstream_error" : "invalid_request_error", message);
  }
  built.releaseBodyObservation?.();
  if (!upstream.ok) {
    try {
      const text = await boundedText(upstream, Math.min(config.limits.max_response_bytes, 1024 * 1024));
      const detail = upstreamErrorReason(text, adapter.formatErrorBody?.(upstream.status, upstream.headers, text) ?? "");
      return formatErrorResponse(upstream.status, "upstream_error", detail || `Provider error ${upstream.status}`, {
        retryAfter: upstream.headers.get("retry-after"),
      });
    } catch (error) {
      return formatErrorResponse(502, "upstream_error", error instanceof Error ? error.message : "provider error body was unreadable");
    }
  }
  const bounded = boundedStream(upstream, config.limits.max_response_bytes, abort);
  if (!parsed.stream && adapter.parseResponse) {
    try {
      return listedEvents(await adapter.parseResponse(bounded, budget, built.tierLog));
    } catch (error) {
      return formatErrorResponse(502, "upstream_error", error instanceof Error ? error.message : "provider response translation failed");
    }
  }
  return adapter.parseStream(bounded, budget, built.tierLog);
}

function turnEvents(
  adapter: ProviderAdapter,
  parsed: OcxParsedRequest,
  budget: TranslatorBudget,
  abort: AbortController,
): AsyncIterable<AdapterEvent> {
  const queue = createAdapterEventQueue({ onBacklogExceeded: () => abort.abort("adapter event backlog exceeded") });
  let pendingArguments = "";
  let flushScheduled = false;
  const flushArguments = () => {
    if (pendingArguments.length === 0) return;
    const argumentsText = pendingArguments;
    pendingArguments = "";
    queue.push({ type: "tool_call_delta", arguments: argumentsText });
  };
  const emit = (event: AdapterEvent) => {
    if (event.type !== "tool_call_delta") {
      flushArguments();
      queue.push(event);
      return;
    }
    if (pendingArguments.length + event.arguments.length > COALESCE_MAX_CHUNK_LENGTH) flushArguments();
    if (event.arguments.length >= COALESCE_MAX_CHUNK_LENGTH) {
      queue.push(event);
      return;
    }
    pendingArguments += event.arguments;
    if (!flushScheduled && pendingArguments.length > 0) {
      flushScheduled = true;
      queueMicrotask(() => {
        flushScheduled = false;
        flushArguments();
      });
    }
  };
  adapter.runTurn!(parsed, { headers: new Headers(), translatorBudget: budget, abortSignal: abort.signal }, emit)
    .catch((error) => {
      emit({ type: "error", message: error instanceof Error ? error.message : "provider turn failed", retryable: false });
    })
    .finally(() => {
      flushArguments();
      queue.close();
    });
  return queue.stream();
}

async function serve(config: BridgeConfig): Promise<void> {
  const budget = createTranslatorBudget();
  try {
    await adapterFor(config, new Request("http://127.0.0.1/health"), upstreamModel(config.provider, config.provider.default_model), budget);
  } finally { budget.dispose(); }
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      if (request.method === "GET" && url.pathname === "/health") {
        return Response.json({ status: "ready", interface_id: config.interface_id });
      }
      if (request.method === "GET" && url.pathname === "/v1/models") {
        return Response.json({
          object: "list",
          data: config.provider.models.map((model) => ({ id: model, object: "model", owned_by: config.provider.id })),
        });
      }
      if (request.method === "POST" && url.pathname === "/v1/responses") {
        return responsesRequest(request, config);
      }
      return formatErrorResponse(404, "not_found", "route is not declared by this provider interface");
    },
  });
  process.stdout.write(`${JSON.stringify({ schema_version: 1, status: "ready", port: server.port, interface_id: config.interface_id })}\n`);
  const stop = () => {
    server.stop(true);
    process.exit(0);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  await new Promise(() => undefined);
}

if (process.argv[2] === "__codebuddy-mcp") {
  await runCodeBuddyMcpServer(process.argv[3] ?? "");
  await new Promise<never>(() => undefined);
}

if (process.argv[2] === "__grok-mcp") {
  await runGrokCaptureMcpServer(process.argv[3] ?? "");
  process.exit(0);
}

try {
  const source = await Bun.stdin.text();
  if (Buffer.byteLength(source) > 1024 * 1024) throw new Error("bridge config exceeds 1 MiB");
  const raw: unknown = JSON.parse(source);
  if (process.argv[2] === "__account") {
    if (!isObject(raw)) throw new Error("Account request must be an object");
    exactKeys(raw, ["operation", "account"], "account request");
    const account = accountValue(raw.account);
    const publish = (event: Record<string, unknown>) => process.stdout.write(`${JSON.stringify(event)}\n`);
    const abort = new AbortController();
    process.on("SIGTERM", () => abort.abort());
    process.on("SIGINT", () => abort.abort());
    let result: unknown;
    switch (raw.operation) {
      case "status": result = await accountStatus(account); break;
      case "login": result = await loginOAuthAccount(account, {
        signal: abort.signal,
        onAuth: ({ url }) => { publish({ event: "auth", url }); },
        onProgress: (message) => { publish({ event: "progress", message }); },
      }); break;
      case "logout": result = await logoutOAuthAccount(account); break;
      default: throw new Error("Unsupported account operation");
    }
    publish({ event: "result", result });
    process.exit(0);
  }
  const config = parseConfig(raw);
  const destinationError = await providerDestinationResolvedError(config.provider.id, {
    baseUrl: config.provider.base_url,
    allowPrivateNetwork: config.provider.allow_private_network,
  });
  if (destinationError) throw new Error(`provider ${config.provider.id} ${destinationError}`);
  if (config.provider.account) process.env.OPENCODEX_HOME = config.provider.account.directory;
  await serve(config);
} catch (error) {
  const message = redactSecretString(error instanceof Error ? error.message : "unknown error").slice(0, 600);
  if (process.argv[2] === "__account") {
    process.stdout.write(`${JSON.stringify({ event: "error", code: error instanceof OAuthAccountError ? error.code : "account_error", message })}\n`);
  }
  process.stderr.write(`opencodex bridge failed closed: ${message}\n`);
  process.exit(1);
}
