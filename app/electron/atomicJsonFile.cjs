const fs = require("node:fs");
const path = require("node:path");
const { randomUUID } = require("node:crypto");

// A document rewritten in place is truncated first, so a crash, a full disk or
// a reader in the middle sees a partial file. A complete temporary sibling
// renamed over the target keeps the previous complete document until the new
// one is on disk. A failure leaves the target untouched and no temporary file.
function writeJsonFileAtomicallySync(file, value, { mode = 0o600 } = {}) {
  const text = JSON.stringify(value, null, 2);
  if (typeof text !== "string") throw new TypeError(`${path.basename(file)} has no JSON representation`);
  const temporary = path.join(path.dirname(file), `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let descriptor = null;
  try {
    descriptor = fs.openSync(temporary, "wx", mode);
    fs.writeFileSync(descriptor, `${text}\n`, "utf8");
    fs.fsyncSync(descriptor);
    fs.closeSync(descriptor);
    descriptor = null;
    fs.renameSync(temporary, file);
  } catch (error) {
    if (descriptor !== null) { try { fs.closeSync(descriptor); } catch {} }
    try { fs.rmSync(temporary, { force: true }); } catch {}
    throw error;
  }
}

module.exports = { writeJsonFileAtomicallySync };
