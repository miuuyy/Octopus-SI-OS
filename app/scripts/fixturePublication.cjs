const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { publishFiles, createStoragePublisher } = require("../../registry/interface/world_operator_chat/publication.cjs");

module.exports = { fixtureFilePublisher, fixtureDirectoryPublisher, withFixturePublisher };

function withFixturePublisher(options) {
  return { ...options,
    publication: Object.hasOwn(options, "publication") ? options.publication : fixtureDirectoryPublisher(options.octopusHomeRoot),
    publishFiles: Object.hasOwn(options, "publishFiles") ? options.publishFiles : fixtureFilePublisher(options.octopusHomeRoot) };
}

function fixtureFilePublisher(home) {
  return (writes, condition) => publishFiles(writes, async request => {
    const receipts = [], root = path.join(home, "client-state", "fixture-file-publications");
    let checked = false;
    await fs.mkdir(root, { recursive: true });
    for (const write of request.writes) {
      const file = path.join(root, crypto.createHash("sha256").update(write.operation_id).digest("hex") + ".json");
      const prior = await fs.readFile(file, "utf8").then(JSON.parse, error => { if (error.code !== "ENOENT") throw error; return null; });
      if (prior) {
        if (JSON.stringify(prior.request) !== JSON.stringify(write)) return { receipts, error: "fixture operation request changed" };
        receipts.push(prior.receipt);
        if (prior.receipt.outcome !== "applied") break;
        continue;
      }
      if (request.condition && !checked) {
        assert.equal(request.condition.octopus_home, await fs.realpath(home));
        if (await fs.stat(path.join(home, "state/fixture-runtime-owned")).then(() => true, error => { if (error.code !== "ENOENT") throw error; return false; })) {
          return { receipts, error: "fixture stopped ownership is busy" };
        }
        for (const pin of request.condition.preimages.filter(pin => !request.writes.some(write => write.path === pin.path))) {
          const current = await fs.readFile(pin.path).catch(error => { if (error.code !== "ENOENT") throw error; return null; });
          if ((current === null ? null : crypto.createHash("sha256").update(current).digest("hex")) !== pin.sha256) {
            return { receipts, error: "fixture publication dependency changed" };
          }
        }
        checked = true;
      }
      const hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
      const receipt = { digest_algorithm: "sha256", operation_id: write.operation_id, path: write.path,
        before_hash: write.expected_before_hash, after_hash: write.content === null ? null : hash(write.content),
        bytes: write.content === null ? 0 : Buffer.byteLength(write.content), outcome: "applied", error: null };
      let opened = false;
      try {
        const before = await fs.readFile(write.path).catch(error => { if (error.code !== "ENOENT") throw error; return null; });
        if ((before === null ? null : hash(before)) !== write.expected_before_hash) throw new Error("fixture preimage changed");
        if (write.content === null) await fs.unlink(write.path);
        else {
          await fs.mkdir(path.dirname(write.path), { recursive: true });
          const handle = await fs.open(write.path, before === null ? "wx" : "w", 0o600);
          opened = true;
          try { await handle.writeFile(write.content); await handle.sync(); } finally { await handle.close(); }
        }
      } catch (error) { receipt.outcome = opened ? "uncertain" : "not_applied"; receipt.error = error.message; }
      await fs.writeFile(file, JSON.stringify({ request: write, receipt }));
      receipts.push(receipt);
      if (receipt.outcome !== "applied") break;
    }
    return { receipts, error: null };
  }, condition);
}

function fixtureDirectoryPublisher(home) {
  const hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
  const snapshot = async directory => {
    const root = await fs.lstat(directory), entries = [];
    assert.ok(root.isDirectory() && !root.isSymbolicLink());
    const collect = async relative => {
      const current = path.join(directory, relative);
      for (const name of (await fs.readdir(current)).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
        const child = path.join(current, name), stat = await fs.lstat(child);
        if (name === ".octopus-publication") { assert.ok(stat.isDirectory() && !stat.isSymbolicLink() && !(stat.mode & 0o077)); continue; }
        const member = relative ? `${relative}/${name}` : name;
        const kind = stat.isSymbolicLink() ? "symlink" : stat.isDirectory() ? "directory" : "file";
        const bytes = kind === "directory" ? null : kind === "symlink" ? await fs.readlink(child, { encoding: "buffer" }) : await fs.readFile(child);
        entries.push({ relative_path: member, kind, identity: { device: stat.dev, inode: stat.ino }, mode: stat.mode & 0o7777,
          bytes: bytes === null ? 0 : bytes.length, digest: bytes === null ? null : hash(bytes) });
        if (kind === "directory") await collect(member);
      }
    };
    await collect("");
    return { identity: { device: root.dev, inode: root.ino }, mode: root.mode & 0o7777, digest_algorithm: "sha256", entries };
  };
  return createStoragePublisher(path.join(home, "fixture-native-publication"), async (_, mode, request) => {
    if (mode === "--snapshot-directory") return { path: request.path, preimage: await snapshot(request.path) };
    const root = path.join(home, "client-state/fixture-directory-publications");
    await fs.mkdir(root, { recursive: true });
    const operation = mode === "--reclaim-directory" ? request.publication.operation_id : request.operation_id;
    const file = path.join(root, hash(operation) + ".json");
    const prior = await fs.readFile(file, "utf8").then(JSON.parse, error => { if (error.code !== "ENOENT") throw error; return null; });
    if (mode === "--publish-directory-moves") {
      if (prior) { assert.deepEqual(prior.request, request, "fixture retained directory request changed"); return { receipts: prior.receipts, error: null }; }
      const receipts = [];
      for (const [index, movement] of request.moves.entries()) {
        assert.deepEqual(await snapshot(movement.source), movement.expected, "fixture directory identity changed");
        await assert.rejects(fs.lstat(movement.destination), { code: "ENOENT" });
        await fs.rename(movement.source, movement.destination);
        receipts.push({ schema_version: 1, operation_id: operation, index, plan_hash: hash(JSON.stringify(request.moves)),
          movement, outcome: "applied", error: null });
      }
      await fs.writeFile(file, JSON.stringify({ request, receipts, reclaimed: [] }));
      return { receipts, error: null };
    }
    assert.equal(mode, "--reclaim-directory");
    assert.deepEqual(prior.receipts[request.publication.index], request.publication);
    if (!prior.reclaimed.includes(request.publication.index)) {
      assert.deepEqual(await snapshot(request.tombstone), request.expected, "fixture reclaim identity changed");
      await fs.rm(request.tombstone, { recursive: true }); prior.reclaimed.push(request.publication.index);
      await fs.writeFile(file, JSON.stringify(prior));
    } else await assert.rejects(fs.lstat(request.tombstone), { code: "ENOENT" });
    return { tombstone: request.tombstone, publication: request.publication, reclaimed: true };
  });
}
