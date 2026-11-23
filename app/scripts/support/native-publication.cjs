const path = require("node:path");
const { execFileSync } = require("node:child_process");
const { createStoragePublisher } = require("../../../registry/interface/world_operator_chat/publication.cjs");

const DEFAULT_NATIVE_PUBLICATION_TIMEOUT_MS = 90_000;

function resolveNativePublicationTimeoutMs(value = process.env.OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS) {
  if (value === undefined || value === null || value === "") return DEFAULT_NATIVE_PUBLICATION_TIMEOUT_MS;
  if (typeof value === "number") {
    if (Number.isSafeInteger(value) && value > 0) return value;
    throw new Error(`Invalid OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS: expected positive integer milliseconds, got ${value}`);
  }
  if (typeof value === "string" && /^[1-9]\d*$/.test(value)) {
    const parsed = Number(value);
    if (Number.isSafeInteger(parsed) && parsed > 0) return parsed;
  }
  throw new Error(`Invalid OCTOPUS_NATIVE_PUBLICATION_TIMEOUT_MS: expected positive integer milliseconds, got ${JSON.stringify(value)}`);
}

function nativePublication(binary = process.env.OCTOPUS_TOPOLOGY_BINARY ?? path.resolve(__dirname, "../../../target/debug/octopus-operator-chat-topology"), execute = execFileSync, options = {}) {
  const timeout = resolveNativePublicationTimeoutMs(options.timeoutMs);
  return createStoragePublisher(binary, (executable, mode, request, maxBuffer) => {
    const input = JSON.stringify(request), started = Date.now();
    try {
      return JSON.parse(execute(executable, [mode], {
        input, encoding: "utf8", timeout, maxBuffer,
      }));
    } catch (cause) {
      const command = { mode, timeout_ms: timeout, elapsed_ms: Date.now() - started, request_bytes: Buffer.byteLength(input),
        ...(request.writes ? { write_count: request.writes.length, targets: request.writes.slice(0, 4).map(write => ({ path: write.path, operation_id: write.operation_id })) } : {}),
        ...(request.moves ? { move_count: request.moves.length, targets: request.moves.slice(0, 4).map(move => ({ source: move.source, destination: move.destination, entry_count: move.expected.entries.length })) } : {}),
        ...(request.path ? { path: request.path } : {}),
        ...(request.tombstone ? { tombstone: request.tombstone } : {}),
        code: cause.code ?? null, status: cause.status ?? null, signal: cause.signal ?? null };
      const failure = new Error(`Native storage publication failed ${JSON.stringify(command)}: ${cause.message}`, { cause });
      failure.code = cause.code;
      failure.publicationCommand = command;
      throw failure;
    }
  });
}

const transactionEntries = async directory => (await require("node:fs/promises").readdir(directory)).filter(name => name !== ".octopus-publication");
module.exports = {
  DEFAULT_NATIVE_PUBLICATION_TIMEOUT_MS,
  nativePublication,
  resolveNativePublicationTimeoutMs,
  transactionEntries,
};
