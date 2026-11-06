module.exports = { activationDriverRef, cronScheduleFingerprint, parseCronActivationDocument, parseUnsignedCronInteger, scheduledInterfaceDisplayName, validateCronExpression, validateCronField, validateCronSchedule };

const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const crypto = require("node:crypto");
const { stableJson } = require("./jsonFiles.cjs");
const { exactObjectKeys, requiredSafeRegistryId, stringValue } = require("./values.cjs");

function activationDriverRef(activation) {
  return stringValue(
    activation?.activation_driver_ref
    ?? activation?.handler_ref
    ?? activation?.activation_transport_ref,
  );
}

function scheduledInterfaceDisplayName(interfaceId) {
  const source = interfaceId.replace(/^(world|system)_/, "");
  const words = source.replace(/[_-]+/g, " ").trim();
  return words ? `${words[0].toUpperCase()}${words.slice(1)}` : interfaceId;
}

function parseCronActivationDocument(document, interfaceId) {
  exactObjectKeys(document, ["schema_version", "interface", "activations"], ["schema_version", "interface", "activations"], "activation document");
  if (document.schema_version !== 1) throw new Error("activation schema_version must be 1");
  if (document.interface !== interfaceId) throw new Error("activation interface does not match its registry folder");
  if (!Array.isArray(document.activations) || document.activations.length === 0) throw new Error("activation document must contain activations");
  const matches = [];
  document.activations.forEach((activation, index) => {
    exactObjectKeys(activation, ["activation_driver_ref", "enabled", "config"], ["activation_driver_ref", "config"], `activation #${index}`);
    if (activation.enabled !== undefined && typeof activation.enabled !== "boolean") throw new Error(`activation #${index}.enabled must be boolean`);
    if (activationDriverRef(activation) === "cron_life") matches.push({ activation, index });
  });
  if (matches.length !== 1) throw new Error(`expected exactly one cron_life activation; found ${matches.length}`);
  const { activation, index } = matches[0];
  const config = activation.config;
  exactObjectKeys(config, ["schedule"], ["schedule"], "cron activation config");
  validateCronSchedule(config.schedule);
  return { activation, activationIndex: index, config };
}

function validateCronSchedule(schedule) {
  exactObjectKeys(
    schedule,
    ["interval_seconds", "cron", "due_at", "inactivity", "fire_on_start", "timezone", "reminder_env", "reminder_default"],
    ["fire_on_start", "timezone", "reminder_env", "reminder_default"],
    "cron schedule",
  );
  if (typeof schedule.fire_on_start !== "boolean") throw new Error("schedule.fire_on_start must be boolean");
  if (typeof schedule.reminder_env !== "string" || !/^(?:[A-Z_][A-Z0-9_]*)?$/.test(schedule.reminder_env)) throw new Error("schedule.reminder_env is invalid");
  if (typeof schedule.reminder_default !== "string") throw new Error("schedule.reminder_default must be a string");
  const hasInterval = schedule.interval_seconds !== null && schedule.interval_seconds !== undefined;
  const hasCron = schedule.cron !== null && schedule.cron !== undefined;
  const hasDueAt = schedule.due_at !== null && schedule.due_at !== undefined;
  const hasInactivity = schedule.inactivity != null;
  if (Number(hasInterval) + Number(hasCron) + Number(hasDueAt) + Number(hasInactivity) !== 1) throw new Error("schedule must declare exactly one of interval_seconds, cron, due_at or inactivity");
  if (hasInactivity) {
    exactObjectKeys(schedule.inactivity, ["seconds", "interface_match", "exclude_interface_match", "within_ee"], ["seconds", "interface_match"], "schedule.inactivity");
    if (schedule.inactivity.within_ee !== undefined) requiredSafeRegistryId(schedule.inactivity.within_ee, "inactivity.within_ee");
    if (typeof schedule.inactivity.seconds !== "number" || !Number.isFinite(schedule.inactivity.seconds) || schedule.inactivity.seconds < 0.001) throw new Error("inactivity.seconds must be finite and at least 0.001");
    for (const field of ["interface_match", "exclude_interface_match"]) {
      const selector = schedule.inactivity[field];
      if (field === "exclude_interface_match" && selector === undefined) continue;
      exactObjectKeys(selector, ["interface_type", "role", "interface_driver_ref"], [], `inactivity.${field}`);
      if (!Object.keys(selector).length || Object.values(selector).some((value) => typeof value !== "string" || !value)) throw new Error(`inactivity.${field} requires non-empty descriptor fields`);
    }
    if (schedule.fire_on_start) throw new Error("inactivity schedule requires fire_on_start=false");
  }
  if (hasDueAt) {
    parseScheduledDueAt(schedule.due_at);
    if (schedule.fire_on_start) throw new Error("one-shot schedule.due_at requires fire_on_start=false");
  }
  if (hasInterval && (typeof schedule.interval_seconds !== "number" || !Number.isFinite(schedule.interval_seconds) || schedule.interval_seconds <= 0)) {
    throw new Error("schedule.interval_seconds must be a finite positive number");
  }
  if (hasCron) validateCronExpression(schedule.cron);
  if (!stringValue(schedule.timezone)) throw new Error("schedule.timezone must be non-empty");
  if (!["UTC", "local"].includes(schedule.timezone)) {
    try {
      new Intl.DateTimeFormat("en-US", { timeZone: schedule.timezone }).format(new Date());
    } catch {
      throw new Error(`schedule.timezone is unknown: ${schedule.timezone}`);
    }
  }
}

function validateCronExpression(expression) {
  if (typeof expression !== "string") throw new Error("schedule.cron must be a string");
  const fields = expression.trim().split(/\s+/);
  if (fields.length !== 5) throw new Error("schedule.cron must contain five fields");
  validateCronField(fields[0], 0, 59, false, "minute");
  validateCronField(fields[1], 0, 23, false, "hour");
  validateCronField(fields[2], 1, 31, false, "day-of-month");
  validateCronField(fields[3], 1, 12, false, "month");
  validateCronField(fields[4], 0, 6, true, "day-of-week");
}

function validateCronField(field, minimum, maximum, sundayAlias, label) {
  const acceptedMaximum = sundayAlias ? 7 : maximum;
  for (const item of field.split(",")) {
    if (!item) throw new Error(`cron ${label} contains an empty list item`);
    const pieces = item.split("/");
    if (pieces.length > 2) throw new Error(`cron ${label} has an invalid step`);
    const step = pieces.length === 2 ? parseUnsignedCronInteger(pieces[1], `${label} step`) : 1;
    if (step <= 0) throw new Error(`cron ${label} step must be greater than zero`);
    const base = pieces[0];
    let lower;
    let upper;
    if (base === "*") {
      lower = minimum;
      upper = acceptedMaximum;
    } else if (base.includes("-")) {
      const bounds = base.split("-");
      if (bounds.length !== 2) throw new Error(`cron ${label} has an invalid range`);
      lower = parseUnsignedCronInteger(bounds[0], `${label} range start`);
      upper = parseUnsignedCronInteger(bounds[1], `${label} range end`);
    } else {
      lower = parseUnsignedCronInteger(base, `${label} value`);
      upper = lower;
    }
    if (lower < minimum || upper > acceptedMaximum || lower > upper) throw new Error(`cron ${label} range is outside ${minimum}-${acceptedMaximum}`);
  }
}

function parseUnsignedCronInteger(value, label) {
  if (!/^[0-9]+$/.test(value)) throw new Error(`cron ${label} must be an unsigned integer`);
  return Number(value);
}

function cronScheduleFingerprint(interfaceId, schedule) {
  const serialized = stableJson({
    interface: interfaceId,
    interval_seconds: schedule.interval_seconds ?? null,
    cron: schedule.cron ?? null,
    fire_on_start: schedule.fire_on_start,
    timezone: schedule.timezone,
    reminder_env: schedule.reminder_env,
    reminder_default: schedule.reminder_default,
  }).replace(/[\u007f-\uffff]/g, (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`);
  return crypto.createHash("sha256").update(serialized).digest("hex");
}
