import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

type Effort = "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";

type ProviderSpec = {
  interface: string;
  source_name: string;
  registry_entry?: string;
  adapter?: string;
  name: string;
  developer: string;
  description: string;
  icon?: string;
  icon_dark?: string[];
  logo?: string;
  local?: boolean;
  default_context_window?: number;
  input_modalities?: string[];
  allow_private_network?: boolean;
  headers?: Record<string, string>;
  key?: { env: string; label: string; header?: string; prefix?: string };
  base_url?: { env: string; label: string };
  model?: { env: string; label: string; slug: string; display_name: string };
  cli?: { commands: string[]; env: string[]; home_read: string[]; home_write: string[] };
};

type Spec = { schema_version: 1; template_interface: string; providers: ProviderSpec[] };

type ModelRow = {
  slug: string;
  display_name: string;
  context_window?: number;
  input_modalities: string[];
  efforts: Effort[];
  default_effort?: Effort;
};

const EFFORT_ORDER: Effort[] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const EFFORT_DESCRIPTIONS: Record<Effort, string> = {
  none: "No reasoning",
  minimal: "Minimal reasoning for the quickest answers",
  low: "Fast responses with lighter reasoning",
  medium: "Balances speed and reasoning depth for everyday tasks",
  high: "Greater reasoning depth for complex problems",
  xhigh: "Extra high reasoning depth for complex problems",
  max: "Maximum reasoning depth for the hardest problems",
};
const ADAPTERS = new Set(["anthropic", "azure", "claude-cli", "google", "openai-chat", "openai-responses"]);
const CODEX_ONLY_PRIVATE_CONFIG = ["provider_operations", "web_search", "client_metadata"];
const CODEX_NATIVE_SEARCH_TOOLS = ["web.search", "web.extract"];
const CODEX_ONLY_OVERRIDES = [/^model_context_window=/, /^model_auto_compact_token_limit=/];
const DEFAULT_CONTEXT_WINDOW = 400000;
const LIFE_BIN = "interface://world_provider_codex/packages/octopus-provider-codex/life-bin/octopus-provider-opencodex-life";
const BRIDGE_SUBPATH = "world_provider_codex/packages/octopus-provider-codex/opencodex-bridge/bin/octopus-opencodex-bridge";
const MACHINE_AUTH_ENV = "OCTOPUS_MACHINE_CODEX_AUTH_FILE";
const ACRONYMS = new Map([["gpt", "GPT"], ["glm", "GLM"], ["oss", "OSS"], ["ai", "AI"], ["mimo", "MiMo"], ["cn", "CN"], ["vl", "VL"]]);

const bridgeRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const interfaceRoot = path.resolve(bridgeRoot, "../../../..");
const packageRoot = path.resolve(interfaceRoot, "../../app/distribution/local-packages/packages");
const argv = process.argv.slice(2);
const check = argv.includes("--check");
const sourceFlag = argv.indexOf("--opencodex");
const opencodexRoot = sourceFlag >= 0 ? path.resolve(argv[sourceFlag + 1]) : path.join(bridgeRoot, "node_modules/@bitkyc08/opencodex");

const { PROVIDER_REGISTRY } = await import(path.join(opencodexRoot, "src/providers/registry.ts"));
const { providerConfigSeed } = await import(path.join(opencodexRoot, "src/providers/derive.ts"));
const iconRoot = path.join(opencodexRoot, "gui/dist/provider-icons");

function readJson(file: string): any {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value));
}

function json(value: unknown): string {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function humanize(slug: string): string {
  const words = slug.replace(/^.*\//, "").split(/[-_\s]+/).filter(Boolean);
  const out: string[] = [];
  for (const word of words) {
    const previous = out.at(-1);
    if (/^\d+$/.test(word) && previous && /\d$/.test(previous) && !/\./.test(previous)) {
      out[out.length - 1] = `${previous}.${word}`;
      continue;
    }
    out.push(ACRONYMS.get(word.toLowerCase()) ?? (/^[a-z]/.test(word) ? word[0].toUpperCase() + word.slice(1) : word));
  }
  return out.join(" ");
}

function efforts(values: unknown): Effort[] {
  if (!Array.isArray(values)) return [];
  const known = new Set(values.filter((value): value is Effort => EFFORT_ORDER.includes(value as Effort)));
  return EFFORT_ORDER.filter((effort) => known.has(effort) && effort !== "none");
}

function registryModels(spec: ProviderSpec, seed: any): ModelRow[] {
  const models: string[] = Array.isArray(seed?.models) ? seed.models : [];
  if (!models.length) throw new Error(`${spec.interface}: registry entry ${spec.registry_entry} has no static models; declare a model setup field`);
  const noReasoning = new Set<string>(seed.noReasoningModels ?? []);
  return models.map((slug) => {
    const supported = noReasoning.has(slug) ? [] : efforts(seed.modelReasoningEfforts?.[slug] ?? seed.reasoningEfforts);
    const declaredDefault = seed.modelDefaultReasoningEfforts?.[slug];
    const fallback = supported.includes("medium") ? "medium" : supported[0];
    return {
      slug,
      display_name: seed.modelDisplayNames?.[slug] ?? humanize(slug),
      ...(Number.isInteger(seed.modelContextWindows?.[slug] ?? seed.contextWindow)
        ? { context_window: seed.modelContextWindows?.[slug] ?? seed.contextWindow }
        : {}),
      input_modalities: spec.input_modalities ?? seed.modelInputModalities?.[slug] ?? ["text"],
      efforts: supported,
      ...(supported.length ? { default_effort: supported.includes(declaredDefault) ? declaredDefault : fallback } : {}),
    };
  });
}

function setupModel(spec: ProviderSpec): ModelRow[] {
  return [{ slug: spec.model!.slug, display_name: spec.model!.display_name, input_modalities: ["text"], efforts: [] }];
}

function selectableEfforts(row: ModelRow): Effort[] {
  return row.efforts.length ? row.efforts : ["none"];
}

function defaultEffort(row: ModelRow): Effort {
  return row.default_effort ?? "none";
}

function effortRows(row: ModelRow) {
  return selectableEfforts(row).map((effort) => ({ effort, description: EFFORT_DESCRIPTIONS[effort] }));
}

function codexModelInfo(row: ModelRow, priority: number, description: string, contextWindow: number) {
  return {
    slug: row.slug,
    display_name: row.display_name,
    description,
    default_reasoning_level: defaultEffort(row),
    supported_reasoning_levels: effortRows(row),
    shell_type: "shell_command",
    visibility: "list",
    supported_in_api: true,
    priority,
    base_instructions: "",
    supports_reasoning_summaries: row.efforts.length > 0,
    default_reasoning_summary: "auto",
    support_verbosity: false,
    truncation_policy: { mode: "tokens", limit: 10000 },
    supports_parallel_tool_calls: false,
    ...(row.context_window ? { context_window: Math.min(row.context_window, contextWindow), max_context_window: row.context_window } : {}),
    effective_context_window_percent: 95,
    experimental_supported_tools: [],
    input_modalities: row.input_modalities.filter((value) => value === "text" || value === "image"),
    supports_search_tool: false,
    use_responses_lite: false,
  };
}

function pickerModel(row: ModelRow, description: string) {
  return {
    model: row.slug,
    display_name: row.display_name,
    description,
    ...(row.context_window ? { context_window: row.context_window } : {}),
    input_modalities: row.input_modalities,
    output_modalities: ["text"],
    supports_tools: true,
    supports_reasoning: row.efforts.length > 0,
    default_reasoning_effort: defaultEffort(row),
    supported_reasoning_efforts: effortRows(row),
    show_in_picker: true,
  };
}

function authTransport(spec: ProviderSpec, adapter: string) {
  if (!spec.key) return {};
  const [header, prefix] = spec.key.header !== undefined
    ? [spec.key.header, spec.key.prefix ?? ""]
    : adapter === "anthropic" ? ["x-api-key", ""]
      : adapter === "google" ? ["x-goog-api-key", ""]
        : adapter === "azure" ? ["api-key", ""]
          : ["Authorization", "Bearer "];
  return { auth_env_key: spec.key.env, auth_header: header, auth_value_prefix: prefix };
}

function descriptor(spec: ProviderSpec, template: any, rows: ModelRow[], adapter: string, baseUrl: string) {
  const out = clone(template);
  out.id = spec.interface;
  out.source_names = [spec.source_name];
  const config = out.interface_driver_config;
  const privateConfig = config.private_config;
  for (const key of CODEX_ONLY_PRIVATE_CONFIG) delete privateConfig[key];
  privateConfig.model_hidden_tools = privateConfig.model_hidden_tools.filter((tool: string) => !CODEX_NATIVE_SEARCH_TOOLS.includes(tool));
  const preferred = spec.model ? rows[0] : rows.find((row) => row.slug === spec.default_model) ?? rows[0];
  const description = (row: ModelRow) => `${row.display_name} through ${spec.name}.`;
  privateConfig.model = preferred.slug;
  privateConfig.reasoning_effort = defaultEffort(preferred);
  config.supported_reasoning_efforts = EFFORT_ORDER.filter((effort) => rows.some((row) => selectableEfforts(row).includes(effort)));
  privateConfig.model_catalog = rows.map((row) => pickerModel(row, description(row)));
  privateConfig.process_env_names = [];
  privateConfig.opencodex = {
    schema_version: 1,
    adapter,
    credential_owner_interface: spec.interface,
    ...(spec.registry_entry ? { registry_entry: spec.registry_entry } : {}),
    ...(spec.model ? { model_env: spec.model.env } : {}),
    ...(spec.cli ? { bridge_env_names: spec.cli.env } : {}),
    allow_private_network: spec.local === true || spec.allow_private_network === true,
    ...(adapter === "anthropic" ? { cache_retention: "short" } : {}),
    limits: {
      max_request_bytes: 33554432,
      max_translated_call_bytes: 2097152,
      max_translated_turn_bytes: 33554432,
    },
    model_catalog: { models: rows.map((row, index) => codexModelInfo(row, index, description(row), spec.default_context_window ?? DEFAULT_CONTEXT_WINDOW)) },
  };
  config.transport = {
    base_url: baseUrl,
    ...(spec.base_url ? { base_url_env: spec.base_url.env } : {}),
    ...authTransport(spec, adapter),
    headers: spec.headers ?? {},
    request_timeout_ms: 600000,
    max_response_bytes: 67108864,
  };
  const ambient = config.residual_ambient;
  delete ambient.private_auth_store;
  ambient.spawn.isolation_overrides = [
    ...ambient.spawn.isolation_overrides.filter((value: string) => !CODEX_ONLY_OVERRIDES.some((pattern) => pattern.test(value))),
    "web_search=disabled",
  ];
  const variables = out.interface_policy.variables;
  variables.models.default = rows.map((row) => row.slug);
  if (variables.web_search_modes) variables.web_search_modes.default = ["disabled"];
  return out;
}

function life(spec: ProviderSpec, template: any) {
  const out = clone(template);
  const pointers = [
    "/interface_driver_config/private_config/process_env_names",
    ...(spec.key ? ["/interface_driver_config/transport/auth_env_key"] : []),
    ...(spec.base_url ? ["/interface_driver_config/transport/base_url_env"] : []),
    ...(spec.model ? ["/interface_driver_config/private_config/opencodex/model_env"] : []),
  ];
  for (const process of out.processes) {
    process.bin = LIFE_BIN;
    process.inherit_env = process.inherit_env.filter((name: string) => name !== MACHINE_AUTH_ENV);
    process.inherit_env_from_interface = pointers;
    const authority = process.authority;
    authority.read_env_paths = authority.read_env_paths.filter((name: string) => name !== MACHINE_AUTH_ENV);
    authority.write_env_paths = authority.write_env_paths.filter((name: string) => name !== MACHINE_AUTH_ENV);
    authority.executable_root_subpaths = [
      ...authority.executable_root_subpaths,
      { root_slot: "registry_interface_root", relative_path: BRIDGE_SUBPATH },
    ];
    if (spec.cli) {
      process.inherit_env = [...process.inherit_env, ...spec.cli.env.filter((name) => !process.inherit_env.includes(name))];
      authority.executable_commands = [...authority.executable_commands, ...spec.cli.commands];
      authority.read_env_subpaths = [...authority.read_env_subpaths, ...spec.cli.home_read.map((relative_path) => ({ env: "HOME", relative_path }))];
      authority.write_env_subpaths = [...(authority.write_env_subpaths ?? []), ...spec.cli.home_write.map((relative_path) => ({ env: "HOME", relative_path }))];
    }
  }
  return out;
}

function application(spec: ProviderSpec, logos: { logo?: string; logo_dark?: string }) {
  const fields = [
    ...(spec.key ? [{ interface: spec.interface, pointer: "/interface_driver_config/transport/auth_env_key", label: spec.key.label, kind: "secret", required: true }] : []),
    ...(spec.base_url ? [{ interface: spec.interface, pointer: "/interface_driver_config/transport/base_url_env", label: spec.base_url.label, kind: "url", required: true }] : []),
    ...(spec.model ? [{ interface: spec.interface, pointer: "/interface_driver_config/private_config/opencodex/model_env", label: spec.model.label, kind: "text", required: true }] : []),
  ];
  return {
    schema_version: 3,
    id: spec.interface,
    name: spec.name,
    description: spec.description,
    category: "provider",
    developer: spec.developer,
    version: "1.0.0",
    icon: "sparkles",
    ...logos,
    interfaces: [spec.interface],
    ...(fields.length ? { setup: { fields } } : {}),
  };
}

function localPackage(spec: ProviderSpec, requiredPackage: string) {
  return {
    schema: "octopus.local_package",
    schema_version: 1,
    id: spec.interface,
    version: "1.0.0",
    product_kind: "application",
    product_id: spec.interface,
    release_status: "optional",
    display: { name: spec.name, description: spec.description, developer: spec.developer, category: "provider", icon: "sparkles" },
    members: [{
      registry_kind: "interface",
      owner: spec.interface,
      primary: true,
      configuration: [{
        path: "interface.json",
        fields: [
          { pointer: "/interface_driver_config/private_config/model", schema: { type: "string", minLength: 1 } },
          { pointer: "/interface_driver_config/private_config/reasoning_effort", schema: { type: "string", enum: EFFORT_ORDER } },
        ],
      }],
    }],
    required_packages: [requiredPackage],
  };
}

function darkLogo(svg: string, colors: string[]): string {
  return colors.reduce((body, color) => body.split(color).join("#FFFFFF").split(color.toLowerCase()).join("#FFFFFF"), svg);
}

const spec = readJson(path.join(bridgeRoot, "providers.json")) as Spec;
if (spec.schema_version !== 1) throw new Error("providers.json schema_version must be 1");
const templateRoot = path.join(interfaceRoot, spec.template_interface);
const templateDescriptor = readJson(path.join(templateRoot, "interface.json"));
const templateLife = readJson(path.join(templateRoot, "life.json"));
const outputs = new Map<string, string | Buffer>();
const seen = new Set<string>();

for (const provider of spec.providers) {
  if (seen.has(provider.interface)) throw new Error(`providers.json repeats ${provider.interface}`);
  seen.add(provider.interface);
  const entry = provider.registry_entry ? PROVIDER_REGISTRY.find((row: any) => row.id === provider.registry_entry) : undefined;
  if (provider.registry_entry && !entry) throw new Error(`${provider.interface}: OpenCodex has no registry entry ${provider.registry_entry}`);
  const seed = entry ? providerConfigSeed(entry) : undefined;
  const adapter = provider.adapter ?? seed?.adapter;
  if (!ADAPTERS.has(adapter)) throw new Error(`${provider.interface}: adapter ${adapter} is not served by the bridge`);
  if (!provider.key && !provider.local && !provider.cli) throw new Error(`${provider.interface}: a remote provider needs a key field or a signed-in CLI`);
  if (provider.cli && provider.key) throw new Error(`${provider.interface}: a CLI provider keeps its sign-in inside the CLI`);
  const baseUrl = provider.base_url ? "" : seed?.baseUrl;
  if (!provider.base_url && !baseUrl) throw new Error(`${provider.interface}: no base URL`);
  const rows = provider.model ? setupModel(provider) : registryModels(provider, seed);
  const folder = path.join(interfaceRoot, provider.interface);
  const logos: { logo?: string; logo_dark?: string } = {};
  if (provider.icon && provider.logo) throw new Error(`${provider.interface}: a logo comes either from an OpenCodex icon or from the interface folder`);
  if (provider.logo) {
    if (!fs.existsSync(path.join(folder, provider.logo))) throw new Error(`${provider.interface}: logo ${provider.logo} is not in the interface folder`);
    logos.logo = provider.logo;
  }
  if (provider.icon) {
    const svg = fs.readFileSync(path.join(iconRoot, provider.icon), "utf8");
    outputs.set(path.join(folder, "application-logo.svg"), svg);
    logos.logo = "application-logo.svg";
    if (provider.icon_dark?.length) {
      outputs.set(path.join(folder, "application-logo-dark.svg"), darkLogo(svg, provider.icon_dark));
      logos.logo_dark = "application-logo-dark.svg";
    }
  }
  outputs.set(path.join(folder, "interface.json"), json(descriptor({ ...provider, default_model: seed?.defaultModel } as any, templateDescriptor, rows, adapter, baseUrl)));
  outputs.set(path.join(folder, "life.json"), json(life(provider, templateLife)));
  outputs.set(path.join(folder, "application.json"), json(application(provider, logos)));
  outputs.set(path.join(packageRoot, `${provider.interface}.json`), json(localPackage(provider, spec.template_interface)));
}

const drift: string[] = [];
for (const [file, body] of outputs) {
  const current = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : null;
  if (current === body) continue;
  if (check) drift.push(path.relative(path.resolve(interfaceRoot, "../.."), file));
  else {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body);
  }
}
if (check && drift.length) {
  process.stderr.write(`OpenCodex provider descriptors drifted from providers.json and OpenCodex:\n${drift.join("\n")}\n`);
  process.exit(1);
}
process.stdout.write(`${check ? "checked" : "wrote"} ${spec.providers.length} OpenCodex providers\n`);
