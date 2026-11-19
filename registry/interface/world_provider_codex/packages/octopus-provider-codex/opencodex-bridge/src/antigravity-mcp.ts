import { randomBytes } from "node:crypto";
import { object, type Row } from "./antigravity-content.ts";

export function openToolEndpoint(serverName: string, tools: Row[], maxRequestBytes: number, call: (name: string, args: Row, signal: AbortSignal) => Promise<Row>) {
  const authorization = `Bearer ${randomBytes(32).toString("hex")}`;
  const names = new Set(tools.map(tool => tool.name));
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0, maxRequestBodySize: maxRequestBytes,
    async fetch(request, server) {
      if (request.headers.get("authorization") !== authorization || request.headers.has("origin")) return new Response(null, { status: 403 });
      if (new URL(request.url).pathname !== "/mcp") return new Response(null, { status: 404 });
      if (request.method !== "POST") return new Response(null, { status: 405, headers: { Allow: "POST" } });
      let message: Row | undefined;
      try { message = object(await request.json()); } catch {}
      const id = message?.id ?? null;
      const result = (result: unknown) => Response.json({ jsonrpc: "2.0", id, result });
      const error = (code: number, message: string) => Response.json({ jsonrpc: "2.0", id, error: { code, message } });
      if (message?.jsonrpc !== "2.0" || typeof message.method !== "string") return error(-32600, "Invalid MCP request.");
      if (id === null) return new Response(null, { status: 202 });
      const params = object(message.params);
      switch (message.method) {
        case "initialize": return result({ protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "octopus", version: "1.0" } });
        case "ping": return result({});
        case "tools/list": return result({ tools });
        case "tools/call": {
          const args = params?.arguments === undefined ? {} : object(params.arguments);
          if (typeof params?.name !== "string" || !args) return error(-32602, "Invalid tool name or arguments.");
          if (!names.has(params.name)) {
            return error(-32602, `Unknown tool ${JSON.stringify(params.name.slice(0, 128))}. Use the exact ToolName from tools/list.`);
          }
          server.timeout(request, 0);
          try { return result(await call(params.name, args, request.signal)); }
          catch (cause) { return error(-32603, cause instanceof Error ? cause.message : String(cause)); }
        }
        default: return error(-32601, "Method not found.");
      }
    },
  });
  return {
    descriptor: { type: "http", name: serverName, url: `http://127.0.0.1:${server.port}/mcp`, headers: [{ name: "Authorization", value: authorization }] },
    close: () => server.stop(true),
  };
}
