const REQUEST_TIMEOUT_MS = 10_000;

function transportOf(descriptor) {
  const transport = descriptor?.interface_driver_config?.transport;
  if (transport?.wire !== "image_json_http" || typeof transport.auth_env_key !== "string" || !transport.auth_env_key.trim()) {
    throw new Error(`Interface '${descriptor?.id}' is not an image generation interface with a key.`);
  }
  return transport;
}

function filled(map) {
  return Object.fromEntries(Object.entries(map ?? {})
    .filter(([, value]) => typeof value === "string" && value.trim())
    .map(([name, value]) => [name, value.trim()]));
}

function endpointOf(transport, values) {
  const override = typeof transport.base_url_env === "string" ? values[transport.base_url_env.trim()] : "";
  let url;
  try {
    url = new URL(override || transport.base_url);
  } catch {
    return { error: "The API URL is not a valid URL." };
  }
  if (url.protocol !== "https:") return { error: "The API URL must use https." };
  const models = url.pathname.endsWith("/images/generations")
    ? new URL(url.pathname.slice(0, -"/images/generations".length) + "/models", url.origin)
    : null;
  return { url, models };
}

async function check(ctx, values) {
  const transport = transportOf(ctx.descriptor);
  const key = values[transport.auth_env_key.trim()];
  if (!key) return { state: "not_connected", message: "Enter the API key." };
  const endpoint = endpointOf(transport, values);
  if (endpoint.error) return { state: "error", message: endpoint.error };
  if (!endpoint.models) {
    return {
      state: "connected",
      account: { label: endpoint.url.host.slice(0, 96), detail: "Key saved; this endpoint offers no account check" },
      message: "This API has no models list to check the key against; the first image request will show whether it works.",
    };
  }
  const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  let response;
  try {
    response = await (ctx.fetch ?? fetch)(endpoint.models, {
      headers: { [transport.auth_header ?? "Authorization"]: `${transport.auth_value_prefix ?? ""}${key}` },
      signal: ctx.signal ? AbortSignal.any([ctx.signal, timeout]) : timeout,
    });
  } catch (error) {
    if (ctx.signal?.aborted) throw error;
    return { state: "error", message: `Could not reach ${endpoint.models.host}.` };
  }
  if (response.status === 401 || response.status === 403) return { state: "expired", message: "The API rejected the key." };
  if (!response.ok) return { state: "error", message: `The API answered HTTP ${response.status}.` };
  return { state: "connected", account: { label: endpoint.url.host.slice(0, 96), detail: "API key accepted" } };
}

async function probe(ctx) {
  return check(ctx, filled(ctx.values));
}

async function connect(ctx) {
  const input = filled(ctx.input);
  const result = await check(ctx, { ...filled(ctx.values), ...input });
  if (result.state === "connected" && Object.keys(input).length) await ctx.saveValues(input);
  return result;
}

async function disconnect(ctx) {
  const transport = transportOf(ctx.descriptor);
  const names = [transport.auth_env_key, transport.base_url_env].filter((name) => typeof name === "string" && name.trim());
  await ctx.saveValues(Object.fromEntries(names.map((name) => [name.trim(), null])));
  return { state: "not_connected" };
}

module.exports = { schemaVersion: 1, probe, connect, disconnect };
