const assert = require("node:assert/strict");
const test = require("node:test");
const { nativePublication } = require("./native-publication.cjs");

test("lost native publication verdict retains coordinates without payload or fabricated receipts", async () => {
  const timeout = Object.assign(new Error("spawnSync ETIMEDOUT"), { code: "ETIMEDOUT", signal: "SIGTERM", status: null });
  let calls = 0;
  const publisher = nativePublication("/fixture/topology", (_binary, flags, options) => {
    calls++;
    assert.deepEqual(flags, ["--publish-files"]);
    assert.equal(options.timeout, 30000);
    assert.equal(options.maxBuffer, 4 * 1024 * 1024);
    throw timeout;
  });
  await assert.rejects(publisher.publishFiles([{ filePath: "/private/tmp/host.json", operationId: "fixture:host:1",
    expectedBeforeHash: null, content: "private-payload-sentinel" }]), failure => {
    assert.equal(failure.cause, timeout);
    assert.equal(failure.code, "client_publication_failed");
    assert.equal(failure.publicationCommand.mode, "--publish-files");
    assert.equal(failure.publicationCommand.write_count, 1);
    assert.deepEqual(failure.publicationCommand.targets, [{ path: "/private/tmp/host.json", operation_id: "fixture:host:1" }]);
    assert.deepEqual(failure.publicationReceipts, []);
    assert.match(failure.message, /ETIMEDOUT/);
    assert.ok(!failure.message.includes("private-payload-sentinel"));
    assert.ok(!JSON.stringify(failure.publicationCommand).includes("private-payload-sentinel"));
    return true;
  });
  assert.equal(calls, 1);
});
