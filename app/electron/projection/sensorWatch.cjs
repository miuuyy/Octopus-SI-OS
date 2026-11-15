const path = require("node:path");
const { readOptionalRegularJsonDocument } = require("./jsonFiles.cjs");

// Optional watch sidecars describe event sensors, not cron schedules. Read the
// firing receipt from the driver's private var directory, never infer it from
// delivery history (which may belong to an earlier arming).
async function sensorWatchProjection(projection, descriptor, interfaceId) {
  if (descriptor.role !== "sensor" || !descriptor.descriptor_path) return {};
  const directory = path.dirname(descriptor.descriptor_path);
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
  const fired = await read(path.join(projection.octopusHome, "interfaces", interfaceId, "fired.json"));
  if (armed && (armed.armed !== undefined && typeof armed.armed !== "boolean"
    || armed.armed !== false && !Number.isFinite(Date.parse(armed.armed_at)))) throw new Error("Invalid sensor arming state");
  if (fired && (fired.schema_version !== 1 || !["prepared", "admitted", "refused"].includes(fired.status))) throw new Error("Invalid sensor firing state");
  const status = fired?.status === "admitted" ? "fired" : fired?.status === "refused" ? "disarmed"
    : !armed || armed.armed === false ? "disarmed" : fired?.status === "prepared" ? "prepared" : "armed";
  return { status, condition };
}
module.exports = { sensorWatchProjection };
