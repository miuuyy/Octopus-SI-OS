import type { OcxMessage } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import type { TranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";
import { jsonUtf8Bytes } from "../node_modules/@bitkyc08/opencodex/src/lib/json-byte-size.ts";

export type NativeReplayScope = { owner: string; model: string; format: string };

const PREFIX = "octopus-native-v1:";

function scopeValue(value: unknown): NativeReplayScope {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).sort().join(",") !== "format,model,owner") {
    throw new Error("Invalid native replay scope");
  }
  const scope = value as NativeReplayScope;
  if ([scope.owner, scope.model, scope.format].some(item => typeof item !== "string" || !item.trim())) {
    throw new Error("Invalid native replay scope");
  }
  return scope;
}

function jsonValue(value: unknown, ancestors = new Set<object>(), depth = 0): void {
  if (depth > 128) throw new Error("Native replay JSON exceeds its nesting limit");
  if (value === null || typeof value === "string" || typeof value === "boolean") return;
  if (typeof value === "number" && Number.isFinite(value)) return;
  if (typeof value !== "object" || value === null || ancestors.has(value)) {
    throw new Error("Native replay payload must be finite, acyclic JSON");
  }
  if (!Array.isArray(value) && ![Object.prototype, null].includes(Object.getPrototypeOf(value))) {
    throw new Error("Native replay payload must contain plain JSON objects");
  }
  ancestors.add(value);
  if (Array.isArray(value)) {
    for (const key of Reflect.ownKeys(value)) {
      if (key === "length") continue;
      const descriptor = Object.getOwnPropertyDescriptor(value, key)!;
      if (typeof key !== "string" || !/^(0|[1-9][0-9]*)$/.test(key) || !descriptor.enumerable || !Object.hasOwn(descriptor, "value")) {
        throw new Error("Native replay arrays must contain ordinary JSON elements");
      }
    }
    for (const item of value) jsonValue(item, ancestors, depth + 1);
  } else {
    for (const descriptor of Object.values(Object.getOwnPropertyDescriptors(value))) {
      if (!descriptor.enumerable || !Object.hasOwn(descriptor, "value")) {
        throw new Error("Native replay payload must contain ordinary JSON properties");
      }
      jsonValue(descriptor.value, ancestors, depth + 1);
    }
    if (Object.getOwnPropertySymbols(value).length) throw new Error("Native replay JSON cannot contain symbol keys");
  }
  ancestors.delete(value);
}

export function encodeNativeReplay(scope: NativeReplayScope, payload: unknown, budget: TranslatorBudget): string {
  scopeValue(scope);
  jsonValue(payload);
  const envelope = { scope, payload };
  const bytes = jsonUtf8Bytes(envelope);
  const reservation = budget.reserveTransient(8 * (PREFIX.length + 4 * Math.ceil(bytes / 3)), { kind: "reasoning" });
  try {
    return PREFIX + Buffer.from(JSON.stringify(envelope), "utf8").toString("base64");
  } finally {
    reservation.release();
  }
}

export function takeNativeReplay(message: OcxMessage, scope: NativeReplayScope, budget: TranslatorBudget): unknown | undefined {
  scopeValue(scope);
  if (message.role !== "assistant") return undefined;
  const carriers = message.content.filter(part => part.type === "thinking" && part.signature?.startsWith(PREFIX));
  if (carriers.length === 0) return undefined;
  if (carriers.length !== 1) throw new Error("Duplicate native replay carriers");
  const carrier = carriers[0]!;
  if (carrier.type !== "thinking" || carrier.thinking !== "" || carrier.redacted?.length) {
    throw new Error("Native replay carrier must not replace model thinking");
  }
  const encoded = carrier.signature!;
  const reservation = budget.reserveTransient(8 * encoded.length, { kind: "reasoning" });
  try {
    const base64 = encoded.slice(PREFIX.length);
    if (!base64 || base64.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64)) {
      throw new Error("Malformed native replay encoding");
    }
    const bytes = Buffer.from(base64, "base64");
    if (bytes.toString("base64") !== base64) throw new Error("Malformed native replay encoding");
    let decoded: unknown;
    try {
      decoded = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
    } catch {
      throw new Error("Malformed native replay JSON");
    }
    if (!decoded || typeof decoded !== "object" || Array.isArray(decoded)
      || Object.keys(decoded).sort().join(",") !== "payload,scope") throw new Error("Invalid native replay envelope");
    const envelope = decoded as { scope: unknown; payload: unknown };
    const actual = scopeValue(envelope.scope);
    jsonValue(envelope.payload);
    message.content = message.content.filter(part => part !== carrier);
    return actual.owner === scope.owner && actual.model === scope.model && actual.format === scope.format
      ? envelope.payload : undefined;
  } finally {
    reservation.release();
  }
}
