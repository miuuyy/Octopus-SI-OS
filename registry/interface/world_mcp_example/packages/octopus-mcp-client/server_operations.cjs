"use strict";

const DESCRIPTION_CHARS = 4096;
const HINTS = ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"];

function description(tool) {
  const text = [tool?.description, tool?.title, tool?.annotations?.title]
    .find((value) => typeof value === "string" && value.trim())?.trim() ?? `Call the server tool ${tool?.name}.`;
  return text.length > DESCRIPTION_CHARS ? `${text.slice(0, DESCRIPTION_CHARS - 1)}…` : text;
}

function annotations(tool) {
  const source = tool?.annotations !== null && typeof tool?.annotations === "object" && !Array.isArray(tool.annotations) ? tool.annotations : {};
  const declared = {};
  if (typeof source.title === "string" && source.title.trim()) declared.title = source.title.trim().slice(0, 256);
  for (const hint of HINTS) {
    if (typeof source[hint] === "boolean") declared[hint] = source[hint];
  }
  return Object.keys(declared).length ? declared : null;
}

function serverToolOperations(tools) {
  return tools.map((tool) => {
    const declared = annotations(tool);
    return {
      name: typeof tool?.name === "string" ? tool.name : "",
      output_label: "mcp.tool.result",
      handler_config: { operation: "call_tool", mcp_tool_name: tool?.name, ...(declared ? { annotations: declared } : {}) },
      description: description(tool),
      input_schema: tool?.inputSchema,
    };
  });
}

function generatedServerOperation(document) {
  return document?.handler_config?.operation === "call_tool" && typeof document.handler_config.mcp_tool_name === "string";
}

function publishServerTools(ctx, tools) {
  return ctx.publishOperations(serverToolOperations(tools), generatedServerOperation);
}

module.exports = { publishServerTools };
