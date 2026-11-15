const path = require("node:path");
const { readOptionalRegularJsonDocument } = require("./jsonFiles.cjs");

const object = (value) => Boolean(value) && typeof value === "object" && !Array.isArray(value);
const text = (value) => (typeof value === "string" ? value : "");
const count = (value) => (Number.isSafeInteger(value) && value >= 0 ? value : null);
const isoTime = (value) => (typeof value === "string" && Number.isFinite(Date.parse(value)) ? value : null);
const unixMsTime = (value) => (Number.isSafeInteger(value) && value >= 0 ? new Date(value).toISOString() : null);
const seconds = (value) => typeof value === "number" && Number.isFinite(value) && value > 0;

function span(value) {
  if (value % 3600 === 0) return `${value / 3600} h`;
  if (value % 60 === 0) return `${value / 60} min`;
  return `${value} s`;
}

// A sensor's own configuration and health files can hold instructions or
// external content, and a JSON parser diagnostic quotes what it could not
// read. Only the fact that the file is unusable crosses into the view.
async function readOwned(file, label) {
  try {
    return (await readOptionalRegularJsonDocument(file, label))?.document;
  } catch (error) {
    throw new Error(`${label} is unreadable, unstable or malformed`, { cause: error });
  }
}

// The sensor reads its configuration from its private state directory first
// and from its registry folder otherwise; the view follows the same order.
async function ownedConfiguration(context, name, label) {
  const overridden = await readOwned(path.join(context.stateDirectory, name), label);
  if (overridden !== undefined) return { document: overridden, source: "private_state" };
  const packaged = await readOwned(path.join(context.directory, name), label);
  if (packaged === undefined) throw new Error(`${label} is missing`);
  return { document: packaged, source: "registry" };
}

const GITHUB_WATCH_TEMPLATE_REPOSITORY = "OWNER/REPOSITORY";
const GITHUB_WATCH_STATES = {
  watching: "Watching. The last poll completed.",
  poll_failed: "The last poll failed; the previous watermarks are kept.",
  auth_unavailable: "The private GitHub credential is unavailable.",
  delivery_refused: "The runtime refused a notice; it is kept and retried.",
  delivery_uncertain: "A notice's delivery verdict is uncertain; it is kept.",
  failed: "The sensor stopped after a boundary failure.",
};

async function githubWatchProjection(context) {
  const { document: config, source } = await ownedConfiguration(context, "github-watch.json", "GitHub watch configuration");
  if (!object(config) || config.schema_version !== 1
    || typeof config.repository !== "string" || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(config.repository)
    || typeof config.branch !== "string" || !config.branch || config.branch.length > 200
    || !Number.isSafeInteger(config.poll_seconds) || config.poll_seconds < 300) {
    throw new Error("Invalid GitHub watch configuration");
  }
  const health = await readOwned(path.join(context.stateDirectory, "health.json"), "GitHub watch health");
  if (health !== undefined && (!object(health) || !Object.hasOwn(GITHUB_WATCH_STATES, health.state))) {
    throw new Error("Invalid GitHub watch health");
  }
  const configured = config.repository !== GITHUB_WATCH_TEMPLATE_REPOSITORY;
  if (!configured) {
    return {
      condition: "GitHub activity sensor without a repository",
      sensor_health: { configured: false, configuration_source: source, state: "unconfigured", attention: true,
        summary: "Not configured: github-watch.json still names the OWNER/REPOSITORY template.", facts: [] },
    };
  }
  const state = health ? health.state : "not_started";
  const reason = text(health?.refusal_reason) || text(health?.error);
  const facts = [];
  if (health) {
    facts.push({ label: "Last successful poll", at: isoTime(health.last_success_at) });
    facts.push({ label: "Health recorded", at: isoTime(health.updated_at) });
    if (count(health.failures) !== null) facts.push({ label: "Consecutive poll failures", value: String(health.failures) });
    if (count(health.resources) !== null) facts.push({ label: "Resources tracked", value: String(health.resources) });
    facts.push({ label: "Notices awaiting admission", value: health.pending === true ? "Yes" : "No" });
    if (reason) facts.push({ label: "Reported reason", value: reason });
  }
  return {
    condition: `GitHub activity on ${config.repository} (branch ${config.branch}), polled every ${span(config.poll_seconds)}`,
    sensor_health: { configured: true, configuration_source: source, state, attention: Boolean(health) && state !== "watching",
      summary: health ? GITHUB_WATCH_STATES[state] : "No poll recorded yet.", facts },
  };
}

const GUARDIAN_TEMPLATE = { watched_run: "RUN_ID", provider_interface: "PROVIDER_INTERFACE_ID" };

function guardianEntry(value) {
  if (!object(value) || !Number.isSafeInteger(value.recorded_at_unix_ms) || typeof value.label !== "string") return null;
  return { at: unixMsTime(value.recorded_at_unix_ms), label: value.label, seq: count(value.seq) };
}

async function guardianWatchProjection(context) {
  const { document: config, source } = await ownedConfiguration(context, "guardian_config.json", "Guardian watch configuration");
  if (!object(config) || config.schema_version !== 1
    || typeof config.watched_run !== "string" || !config.watched_run
    || typeof config.provider_interface !== "string" || !config.provider_interface
    || !seconds(config.quiet_seconds)) {
    throw new Error("Invalid guardian watch configuration");
  }
  const state = await readOwned(path.join(context.stateDirectory, "guardian-state.json"), "Guardian watch state");
  if (state !== undefined && (!object(state) || state.schema_version !== 1 || !object(state.watch)
    || state.alert !== null && (!object(state.alert) || !["prepared", "admitted", "refused"].includes(state.alert.status)))) {
    throw new Error("Invalid guardian watch state");
  }
  const configured = config.watched_run !== GUARDIAN_TEMPLATE.watched_run
    && config.provider_interface !== GUARDIAN_TEMPLATE.provider_interface;
  if (!configured) {
    return {
      condition: "Quiet-period guardian without a watched run",
      sensor_health: { configured: false, configuration_source: source, state: "unconfigured", attention: true,
        summary: "Not configured: guardian_config.json still names the RUN_ID template.", facts: [] },
    };
  }
  const condition = `Run ${config.watched_run} emits nothing of its own through ${config.provider_interface} for ${span(config.quiet_seconds)}`;
  // A state file written for another run or provider is not this watch's evidence.
  const current = state && state.watch.run === config.watched_run && state.watch.provider === config.provider_interface ? state : null;
  if (!current) {
    return { condition, sensor_health: { configured: true, configuration_source: source, state: "not_started", attention: false,
      summary: state ? "The recorded state belongs to an earlier watch; nothing is recorded for this one yet." : "No observation recorded yet.", facts: [] } };
  }
  const own = guardianEntry(current.last_own_action);
  const providerError = guardianEntry(current.last_provider_error);
  const alert = current.alert && current.alert.for_seq === (own?.seq ?? -1) ? current.alert : null;
  const reports = alert && count(alert.repeat_index) !== null ? alert.repeat_index + 1 : null;
  const health = !alert ? ["watching", "Watching. No silence is being reported."]
    : alert.status === "admitted" ? ["alerting", `The watched run is silent; ${reports ?? "an"} alert${reports === 1 || reports === null ? "" : "s"} of this silence admitted.`]
      : alert.status === "refused" ? ["alert_refused", "The runtime refused the alert of the current silence; it is retried."]
        : ["alert_prepared", "An alert of the current silence is prepared and awaits admission."];
  const facts = [
    { label: "Last own action of the watched run", at: own?.at ?? null, ...(own ? { value: own.label } : {}) },
    { label: "Observing since", at: unixMsTime(current.started_at_unix_ms) },
  ];
  if (count(current.alerts_sent) !== null) facts.push({ label: "Alerts admitted", value: String(current.alerts_sent) });
  if (providerError) facts.push({ label: "Last provider error", at: providerError.at, value: providerError.label });
  if (alert?.status === "refused" && text(alert.refusal?.reason)) facts.push({ label: "Refusal reason", value: alert.refusal.reason });
  if (count(current.skipped_rows)) facts.push({ label: "Unreadable ledger rows passed over", value: String(current.skipped_rows) });
  return { condition, sensor_health: { configured: true, configuration_source: source, state: health[0], attention: Boolean(alert), summary: health[1], facts } };
}

// Selected by the owner's driver contract, not by an installed interface identity.
const OWNED_SENSOR_WATCHES = { github_watch_life: githubWatchProjection, guardian_watch_life: guardianWatchProjection };

// Optional watch sidecars describe event sensors, not cron schedules. Read the
// firing receipt from the driver's private var directory, never infer it from
// delivery history (which may belong to an earlier arming).
async function sensorWatchProjection(projection, descriptor, interfaceId) {
  if (descriptor.role !== "sensor" || !descriptor.descriptor_path) return {};
  const directory = path.dirname(descriptor.descriptor_path);
  const stateDirectory = path.join(projection.octopusHome, "interfaces", interfaceId);
  const owned = Object.hasOwn(OWNED_SENSOR_WATCHES, descriptor.interface_driver_ref) ? OWNED_SENSOR_WATCHES[descriptor.interface_driver_ref] : null;
  if (owned) return owned({ directory, stateDirectory });
  const read = async (file) => (await readOptionalRegularJsonDocument(file, "sensor watch state"))?.document;
  const watch = await read(path.join(directory, "watch.json"));
  if (!watch) return {};
  let condition;
  if (watch.github_stars) {
    const stars = watch.github_stars;
    if (typeof stars.repository !== "string" || !/^[^/\s]+\/[^/\s]+$/.test(stars.repository)
      || !Number.isSafeInteger(stars.at_least) || stars.at_least < 1) throw new Error("Invalid star watch condition");
    condition = `stars >= ${stars.at_least} on ${stars.repository}`;
  } else if (Array.isArray(watch.terms) && watch.terms.length && watch.terms.every(term => typeof term === "string" && term)
    && Array.isArray(watch.match_fields) && watch.match_fields.length && watch.match_fields.every(field => ["subject", "body"].includes(field))) {
    condition = `Email ${watch.match_fields.join(" / ")} contains any of: ${watch.terms.join(", ")}`;
  } else throw new Error("Unsupported sensor watch condition");
  const armed = await read(path.join(directory, "armed.json"));
  const fired = await read(path.join(stateDirectory, "fired.json"));
  if (armed && (armed.armed !== undefined && typeof armed.armed !== "boolean"
    || armed.armed !== false && !Number.isFinite(Date.parse(armed.armed_at)))) throw new Error("Invalid sensor arming state");
  if (fired && (fired.schema_version !== 1 || !["prepared", "admitted", "refused"].includes(fired.status))) throw new Error("Invalid sensor firing state");
  const status = fired?.status === "admitted" ? "fired" : fired?.status === "refused" ? "disarmed"
    : !armed || armed.armed === false ? "disarmed" : fired?.status === "prepared" ? "prepared" : "armed";
  return { status, condition };
}
module.exports = { sensorWatchProjection };
