import { randomUUID } from "node:crypto";
import type { McpSdkServerConfigWithInstance } from "@anthropic-ai/claude-agent-sdk";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { CallToolRequestSchema, ListToolsRequestSchema, type CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { ChildFailure } from "./claude.ts";
import type { SessionConfig } from "./protocol.ts";

export const TOOL_SERVER = "octopus";
export const TOOL_TIMEOUT_MS = 30 * 60 * 1000;

type ToolSpec = SessionConfig["tools"][number];

type PendingCall = {
  name: string;
  resolve: (result: CallToolResult) => void;
};

export type ToolCallRequest = {
  callId: string;
  name: string;
  input: Record<string, unknown>;
};

export class OctopusToolServer {
  private readonly pending = new Map<string, PendingCall>();
  private readonly specs: Map<string, ToolSpec>;
  readonly config: McpSdkServerConfigWithInstance;

  constructor(tools: ToolSpec[], private readonly onCall: (call: ToolCallRequest) => void) {
    this.specs = new Map();
    for (const tool of tools) {
      if (this.specs.has(tool.name)) throw new ChildFailure("invalid_tools", `tool '${tool.name}' is declared twice`);
      if (tool.input_schema.type !== "object") {
        throw new ChildFailure("invalid_tools", `tool '${tool.name}' input_schema must be a JSON object schema`);
      }
      this.specs.set(tool.name, tool);
    }
    const server = new McpServer({ name: TOOL_SERVER, version: "1.0.0" }, { capabilities: { tools: {} } });
    server.server.setRequestHandler(ListToolsRequestSchema, async () => ({
      tools: [...this.specs.values()].map((tool) => ({
        name: tool.name,
        description: tool.description,
        inputSchema: tool.input_schema as { type: "object"; [key: string]: unknown },
      })),
    }));
    server.server.setRequestHandler(CallToolRequestSchema, async (request) => this.call(request.params.name, request.params.arguments ?? {}));
    this.config = { type: "sdk", name: TOOL_SERVER, timeout: TOOL_TIMEOUT_MS, instance: server };
  }

  get grantedNames(): string[] {
    return [...this.specs.keys()].map((name) => `mcp__${TOOL_SERVER}__${name}`);
  }

  get hasPending(): boolean {
    return this.pending.size > 0;
  }

  resolve(callId: string, text: string, isError: boolean): void {
    const pending = this.pending.get(callId);
    if (!pending) throw new ChildFailure("unknown_tool_call", `call '${callId}' is not pending`);
    this.pending.delete(callId);
    pending.resolve({ content: [{ type: "text", text }], isError });
  }

  abandonAll(reason: string): void {
    for (const [callId, pending] of this.pending) {
      this.pending.delete(callId);
      pending.resolve({ content: [{ type: "text", text: reason }], isError: true });
    }
  }

  private call(name: string, input: Record<string, unknown>): Promise<CallToolResult> {
    if (!this.specs.has(name)) {
      return Promise.resolve({ content: [{ type: "text", text: `tool '${name}' is not granted to this session` }], isError: true });
    }
    const callId = `call_${randomUUID()}`;
    return new Promise((resolve) => {
      this.pending.set(callId, { name, resolve });
      this.onCall({ callId, name, input });
    });
  }
}
