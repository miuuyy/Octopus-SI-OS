type Detail = Record<string, unknown>;

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const CONCATENATED = new Set(["text", "summary", "data"]);

function mergeDetails(chunks: Detail[]): Detail[] {
  const byIndex = new Map<string, Detail>();
  for (const [position, chunk] of chunks.entries()) {
    const key = typeof chunk.index === "number" ? `${String(chunk.type)}:${chunk.index}` : `p:${position}`;
    const current = byIndex.get(key);
    if (!current) {
      byIndex.set(key, { ...chunk });
      continue;
    }
    for (const [field, value] of Object.entries(chunk)) {
      if (CONCATENATED.has(field) && typeof value === "string") {
        current[field] = `${typeof current[field] === "string" ? current[field] : ""}${value}`;
      } else if (current[field] === undefined || current[field] === null || current[field] === "") {
        current[field] = value;
      }
    }
  }
  return [...byIndex.values()];
}

export interface ReasoningDetailsReplay {
  restore(body: string): string;
  observe(stream: ReadableStream<Uint8Array>, streamed: boolean): Promise<void>;
}

export function createReasoningDetailsReplay(limit: number): ReasoningDetailsReplay {
  const byCall = new Map<string, Detail[]>();

  function remember(callIds: string[], details: Detail[]): void {
    if (callIds.length === 0 || details.length === 0) return;
    for (const id of callIds) {
      byCall.delete(id);
      byCall.set(id, details);
    }
    while (byCall.size > limit) {
      const oldest = byCall.keys().next().value;
      if (oldest === undefined) break;
      byCall.delete(oldest);
    }
  }

  function absorb(message: unknown, chunks: Detail[], callIds: Set<string>): void {
    if (!isObject(message)) return;
    if (Array.isArray(message.reasoning_details)) {
      for (const detail of message.reasoning_details) if (isObject(detail)) chunks.push(detail);
    }
    if (Array.isArray(message.tool_calls)) {
      for (const call of message.tool_calls) {
        if (isObject(call) && typeof call.id === "string" && call.id.length > 0) callIds.add(call.id);
      }
    }
  }

  return {
    restore(body) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(body);
      } catch {
        return body;
      }
      if (!isObject(parsed) || !Array.isArray(parsed.messages)) return body;
      let changed = false;
      for (const message of parsed.messages) {
        if (!isObject(message) || message.role !== "assistant" || !Array.isArray(message.tool_calls)) continue;
        const stored = message.tool_calls
          .map((call) => (isObject(call) && typeof call.id === "string" ? byCall.get(call.id) : undefined))
          .find((details) => details !== undefined);
        if (!stored) continue;
        message.reasoning_details = stored;
        changed = true;
      }
      return changed ? JSON.stringify(parsed) : body;
    },

    async observe(stream, streamed) {
      const chunks: Detail[] = [];
      const callIds = new Set<string>();
      const decoder = new TextDecoder();
      const reader = stream.getReader();
      let text = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        text += decoder.decode(value, { stream: true });
        if (!streamed) continue;
        let newline: number;
        while ((newline = text.indexOf("\n")) >= 0) {
          const line = text.slice(0, newline).trim();
          text = text.slice(newline + 1);
          if (!line.startsWith("data:")) continue;
          const payload = line.slice(5).trim();
          if (payload === "" || payload === "[DONE]") continue;
          try {
            const event: unknown = JSON.parse(payload);
            if (!isObject(event) || !Array.isArray(event.choices)) continue;
            for (const choice of event.choices) if (isObject(choice)) absorb(choice.delta, chunks, callIds);
          } catch {
            continue;
          }
        }
      }
      if (!streamed) {
        try {
          const response: unknown = JSON.parse(text + decoder.decode());
          if (isObject(response) && Array.isArray(response.choices)) {
            for (const choice of response.choices) if (isObject(choice)) absorb(choice.message, chunks, callIds);
          }
        } catch {
          return;
        }
      }
      remember([...callIds], mergeDetails(chunks));
    },
  };
}
