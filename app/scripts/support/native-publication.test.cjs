const assert = require("node:assert/strict");
const test = require("node:test");
const {
  DEFAULT_NATIVE_PUBLICATION_TIMEOUT_MS,
  nativePublication,
  resolveNativePublicationTimeoutMs,
} = require("./native-publication.cjs");

test("lost native publication verdict retains coordinates without payload or fabricated receipts", async () => {
  const previous = process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS;
  delete process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS;
  try {
  const timeout = Object.assign(new Error("spawnSync ETIMEDOUT"), { code: "ETIMEDOUT", signal: "SIGTERM", status: null });
  let calls = 0;
  const publisher = nativePublication("/fixture/topology", (_binary, flags, options) => {
    calls++;
    assert.deepEqual(flags, ["--publish-files"]);
    assert.equal(options.timeout, DEFAULT_NATIVE_PUBLICATION_TIMEOUT_MS);
    assert.equal(options.timeout, 90_000);
    assert.equal(options.maxBuffer, 4 * 1024 * 1024);
    throw timeout;
  });
  await assert.rejects(publisher.publishFiles([{ filePath: "/private/tmp/host.json", operationId: "fixture:host:1",
    expectedBeforeHash: null, content: "private-payload-sentinel" }]), failure => {
    assert.equal(failure.cause, timeout);
    assert.equal(failure.code, "client_publication_failed");
    assert.equal(failure.publicationCommand.mode, "--publish-files");
    assert.equal(failure.publicationCommand.timeout_ms, 90_000);
    assert.equal(failure.publicationCommand.write_count, 1);
    assert.deepEqual(failure.publicationCommand.targets, [{ path: "/private/tmp/host.json", operation_id: "fixture:host:1" }]);
    assert.deepEqual(failure.publicationReceipts, []);
    assert.match(failure.message, /ETIMEDOUT/);
    assert.ok(!failure.message.includes("private-payload-sentinel"));
    assert.ok(!JSON.stringify(failure.publicationCommand).includes("private-payload-sentinel"));
    return true;
  });
  assert.equal(calls, 1);
  } finally {
    if (previous === undefined) delete process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS;
    else process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS = previous;
  }
});

test("native publication timeout honors validated env and option overrides and fails closed on invalid values", async () => {
  const previous = process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS;
  try {
    process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS = "45000";
    assert.equal(resolveNativePublicationTimeoutMs(), 45_000);
    const seen = [];
    const publisher = nativePublication("/fixture/topology", (_binary, _flags, options) => {
      seen.push(options.timeout);
      throw new Error("stop");
    });
    await assert.rejects(publisher.publishFiles([{ filePath: "/private/tmp/host.json", operationId: "fixture:host:2", expectedBeforeHash: null, content: "hello" }]));
    assert.deepEqual(seen, [45_000]);

    const explicit = nativePublication("/fixture/topology", (_binary, _flags, options) => {
      seen.push(options.timeout);
      throw new Error("stop");
    }, { timeoutMs: 120_000 });
    await assert.rejects(explicit.publishFiles([{ filePath: "/private/tmp/host.json", operationId: "fixture:host:3", expectedBeforeHash: null, content: "hello" }]));
    assert.deepEqual(seen, [45_000, 120_000]);

    for (const invalid of ["0", "-1", "abc", "12.5", " 30000", 0, -5, 1.5, Number.NaN]) {
      assert.throws(() => resolveNativePublicationTimeoutMs(invalid), /Invalid OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS/);
    }
  } finally {
    if (previous === undefined) delete process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS;
    else process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS = previous;
  }
});
