const path = require("node:path");
const { readOptionalRegularJsonDocument, readRegularJsonDocument } = require("./jsonFiles.cjs");

const object = value => value && typeof value === "object" && !Array.isArray(value);
const integer = (value, minimum) => Number.isSafeInteger(value) && value >= minimum;

// Select the owner's contract, not a particular installed interface identity.
// Only observation metadata crosses into the view: never account credentials,
// message keys or message content. A recorded poll is not connection health.
async function emailWatchProjection(projection, descriptor, interfaceId) {
  if (descriptor.interface_driver_ref !== "email_life") return {};
  if (!descriptor.descriptor_path) throw new Error("Email watch descriptor path is missing");
  const { document: config } = await readRegularJsonDocument(path.join(path.dirname(descriptor.descriptor_path), "config.json"), "email watch configuration");
  const mailboxes = config?.intake?.mailboxes;
  if (!Array.isArray(mailboxes) || !mailboxes.length || !mailboxes.every(name => typeof name === "string" && name.trim())
    || new Set(mailboxes).size !== mailboxes.length) throw new Error("Email watch mailboxes are invalid");
  const file = path.join(projection.octopusHome, "interfaces", interfaceId, "mailbox-state.json");
  const saved = await readOptionalRegularJsonDocument(file, "email mailbox state");
  const state = saved?.document;
  if (saved && (!object(state) || state.schema_version !== 1 || !object(state.mailboxes)
    || !Array.isArray(state.admitted) || !state.admitted.every(key => typeof key === "string"))) {
    throw new Error("Email mailbox state has an invalid schema");
  }
  const observations = mailboxes.map(mailbox => {
    const entry = state?.mailboxes[mailbox];
    if (entry === undefined) return { mailbox, last_uid: null, uid_next: null, checked_at: null };
    if (!object(entry) || !integer(entry.uidvalidity, 1) || !integer(entry.last_uid, 0)
      || typeof entry.checked_at !== "string" || !Number.isFinite(Date.parse(entry.checked_at))
      || entry.uid_next !== undefined && !integer(entry.uid_next, 1)) {
      throw new Error("Email mailbox observation is invalid");
    }
    return { mailbox, last_uid: entry.last_uid, uid_next: entry.uid_next ?? null, checked_at: entry.checked_at };
  });
  return {
    condition: `New email in ${mailboxes.join(", ")} (subject to admission filters)`,
    mailbox_watch: { mailboxes: observations, admitted_count: state ? state.admitted.length : null },
  };
}

module.exports = { emailWatchProjection };
