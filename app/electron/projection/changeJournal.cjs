module.exports = { changeJournalPlan, currentFileDigest, readChangeJournalDocument, undoChangeJournal };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { CHANGE_JOURNAL_MAX_BYTES, CHANGE_JOURNAL_MAX_FILE_BYTES } = require("./constants.cjs");
const { readRegularJsonDocument } = require("./jsonFiles.cjs");
const { arrayValue, exactObjectKeys, isPathInside, requiredDigest, requiredSafeRegistryId, stringValue } = require("./values.cjs");

async function currentFileDigest(file) {
  const stat = await fs.lstat(file).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return null;
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${file} is not a regular file`);
  if (stat.size > CHANGE_JOURNAL_MAX_FILE_BYTES) throw new Error(`${file} exceeds the journal file limit`);
  return crypto.createHash("sha256").update(await fs.readFile(file)).digest("hex");
}

function requiredJournalFiring(value) {
  if (typeof value !== "string" || !/^[0-9]{1,20}$/.test(value)) throw new Error("change journal firing must be the trigger artifact's now_unix_ms");
  return value;
}

async function readChangeJournalDocument(octopusHome, runId, firing) {
  requiredSafeRegistryId(runId, "change journal run");
  const directory = path.join(octopusHome, "state", "change-journals", runId, requiredJournalFiring(firing));
  const file = path.join(directory, "journal.json");
  const stat = await fs.lstat(file).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return null;
  if (stat.size > CHANGE_JOURNAL_MAX_BYTES) throw new Error("change journal exceeds its size limit");
  const { document, digest } = await readRegularJsonDocument(file, "change journal");
  exactObjectKeys(document, ["schema_version", "run", "trigger_artifact", "outcome", "summary", "changes", "checks"], ["schema_version", "run", "outcome", "summary", "changes"], "change journal");
  if (document.schema_version !== 1 || document.run !== runId) throw new Error("change journal belongs to another run or schema");
  if (!["changed", "unchanged"].includes(document.outcome)) throw new Error("change journal outcome must be changed or unchanged");
  if (!Array.isArray(document.changes) || (document.checks !== undefined && !Array.isArray(document.checks))) throw new Error("change journal changes and checks must be arrays");
  return { document, digest, directory };
}

async function changeJournalPlan(octopusHome, registryRoot, runId, firing) {
  const journal = await readChangeJournalDocument(octopusHome, runId, firing);
  if (!journal) return null;
  const { document, digest, directory } = journal;
  const restorableRoots = [
    ...["interface", "tool", "artifact", "ee_template"].map((kind) => path.join(registryRoot, kind)),
    path.join(octopusHome, "state", "runs"),
    path.join(octopusHome, "state", "ee_instances"),
    path.join(octopusHome, "interfaces"),
  ];
  const entries = [];
  const writes = [];
  for (const [index, change] of document.changes.entries()) {
    if (change?.kind !== "file") {
      entries.push({ kind: "other", description: stringValue(change?.description), undo: stringValue(change?.undo), status: "manual" });
      continue;
    }
    exactObjectKeys(change, ["kind", "path", "action", "before_sha256", "after_sha256", "before_copy", "reason"], ["kind", "path", "action", "before_sha256", "after_sha256", "reason"], `change journal entry ${index + 1}`);
    const target = stringValue(change.path);
    if (!path.isAbsolute(target) || path.resolve(target) !== target) throw new Error(`change journal entry ${index + 1} needs a canonical absolute path`);
    if (!["created", "modified", "deleted"].includes(change.action)) throw new Error(`change journal entry ${index + 1} has an invalid action`);
    const digest = (value) => value === null ? null : (/^[0-9a-f]{64}$/.test(stringValue(value)) ? value : (() => { throw new Error(`change journal entry ${index + 1} has an invalid hash`); })());
    const before = digest(change.before_sha256);
    const after = digest(change.after_sha256);
    if ((change.action === "created") !== (before === null) || (change.action === "deleted") !== (after === null)) throw new Error(`change journal entry ${index + 1} hashes do not match its action`);
    const current = await currentFileDigest(target);
    const status = current === after ? "applied" : current === before ? "undone" : "changed";
    let restore = "";
    let reason = "";
    if (before !== null) {
      const copy = stringValue(change.before_copy);
      const copyPath = path.resolve(directory, copy);
      if (!copy || !isPathInside(directory, copyPath)) reason = "its before copy is missing";
      else {
        const bytes = await fs.readFile(copyPath).catch(() => null);
        if (!bytes || crypto.createHash("sha256").update(bytes).digest("hex") !== before) reason = "its before copy does not match the recorded hash";
        else if (!Buffer.from(bytes.toString("utf8")).equals(bytes)) reason = "its before copy is not text";
        else restore = bytes.toString("utf8");
      }
    }
    if (!reason && !restorableRoots.some((root) => isPathInside(root, target))) reason = "it lies outside the Home folders the client restores";
    entries.push({ kind: "file", path: target, action: change.action, reason: stringValue(change.reason), status, restorable: !reason, unrestorable_reason: reason });
    if (!reason) writes.push({ file: target, beforeDigest: after, ...(before === null ? { next: null } : { content: restore }), label: `restore ${path.basename(target)}` });
  }
  const files = entries.filter((entry) => entry.kind === "file");
  return {
    digest,
    run_id: runId,
    firing,
    outcome: document.outcome,
    summary: stringValue(document.summary),
    trigger_artifact: stringValue(document.trigger_artifact),
    checks: arrayValue(document.checks).map((check) => ({ check: stringValue(check?.check), result: stringValue(check?.result), evidence: stringValue(check?.evidence) })),
    changes: entries,
    undoable: files.length > 0 && files.every((entry) => entry.status === "applied" && entry.restorable),
    undone: files.length > 0 && files.every((entry) => entry.status === "undone"),
    writes,
  };
}

async function undoChangeJournal(registryRoot, octopusHomeRoot, request, commitClientAuthoring) {
  if (typeof commitClientAuthoring !== "function") throw new Error("client authoring transaction is unavailable");
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const firing = requiredJournalFiring(request?.firing);
  const plan = await changeJournalPlan(octopusHomeRoot, registryRoot, runId, firing);
  if (!plan) throw new Error(`run ${runId} kept no change journal for firing ${firing}`);
  if (plan.digest !== requiredDigest(request?.expectedJournalDigest, "expectedJournalDigest")) throw new Error("the change journal changed before undo");
  if (!plan.undoable) {
    const blocked = plan.changes.filter((entry) => entry.kind === "file" && (entry.status !== "applied" || !entry.restorable));
    throw new Error(`undo refused: ${blocked.map((entry) => `${entry.path} ${entry.status !== "applied" ? `is ${entry.status}` : `cannot be restored because ${entry.unrestorable_reason}`}`).join("; ")}`);
  }
  await commitClientAuthoring({
    label: `undo changes recorded by ${runId}`,
    writes: plan.writes,
    removals: [],
    publishBarrierPath: plan.writes.at(-1).file,
  });
  return { action: "undo_changes", runId, firing, restored: plan.writes.map((write) => write.file) };
}
