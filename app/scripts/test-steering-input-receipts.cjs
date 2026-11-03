const assert = require("node:assert/strict");

const live = {
  ...require("../../registry/interface/world_operator_chat/client/turnStreams.cjs"),
  ...require("../../registry/interface/world_operator_chat/client/chatArtifacts.cjs"),
};
const sealed = require("../electron/projection/operatorChatEvents.cjs");
const header = { type: "header", schema_version: 1, seq: 0, provider_interface: "provider-runtime",
  run: "run-a", thread_id: "native-thread", turn_id: "native-turn", opened_source_movement_seq: 5, opened_at_unix_ms: 10 };
const receipt = { type: "input_recorded", seq: 1, recorded_at_unix_ms: 12, source_movement_seq: 5,
  provider_interface: header.provider_interface, run: header.run, thread_id: header.thread_id, turn_id: header.turn_id,
  item_id: "native-item", client_id: "operator-message", stage: "input_recorded_in_turn", source_event: "item/started", native_event_at_ms: 11 };
const seal = { type: "seal", seq: 2, recorded_at_unix_ms: 13, source_movement_seq: 5,
  terminal_label: "turn.completed", terminal_artifact_payload_blake3: "a".repeat(64) };
const expected = { providerInterfaceId: header.provider_interface, runId: header.run, complete: true };
const encode = (rows) => Buffer.from(rows.map((row) => JSON.stringify(row)).join("\n") + "\n");
async function history(bytes) {
  const row = { artifact: { id: "turn.stream:sealed" }, from: { Interface: header.provider_interface }, source_line: "1" };
  const payloads = new Map([[row, { __raw: bytes.toString("utf8") }]]);
  const rows = await sealed.operatorChatExpandedStreamRows([row], payloads, header.run, (checked) => assert.deepEqual(checked, header));
  return rows.map((row) => ({ row, payload: payloads.get(row) }));
}
async function main() {
for (const source of ["item/started", "item/completed", "thread/resume"]) {
  const input = { ...receipt, source_event: source };
  if (source === "thread/resume") input.item_id = "item-7";
  const bytes = encode([header, input, seal]);
  const parsed = live.parseOperatorTurnStream(bytes, expected);
  const first = live.selectOperatorTurnStreamEvents(parsed, null, 1);
  assert.equal(first.events.length, 1);
  const event = live.operatorChatStreamArtifact({ config: {}, event: first.events[0], interfaceId: "operator-chat",
    providerInterfaceId: header.provider_interface, providerRole: "provider", runId: header.run });
  const historical = (await history(bytes))[0];
  assert.equal(event.event_kind, "input_recorded"); assert.equal(event.label, "turn.stream");
  assert.equal(event.text, ""); assert.equal(event.terminal, false);
  assert.deepEqual(Object.fromEntries(Object.entries(event.event_data).filter(([key]) => key !== "_stream")), historical.payload);
  assert.equal(event.stream_id, historical.row.__stream.stream_id);
  assert.equal(event.byte_start_offset, historical.row.__stream.byte_start_offset);
  assert.equal(event.byte_end_offset, historical.row.__stream.byte_end_offset);
  assert.equal(sealed.operatorChatEventData(historical.payload, "input_recorded", historical.row)._stream.source_movement_seq, 5);
  const tail = live.selectOperatorTurnStreamEvents(parsed, first.cursor, 1);
  assert.deepEqual(tail.events, []); assert.equal(tail.cursor.sealed, true);
}
const mutations = [
  { provider_interface: "other" }, { run: "other" }, { thread_id: "other" }, { turn_id: "other" },
  { client_id: " " }, { item_id: "item\nwrong" }, { client_id: " x" }, { item_id: 1 },
  { stage: "accepted" }, { source_event: "turn/steer" }, { native_event_at_ms: undefined },
  { native_event_at_ms: null }, { native_event_at_ms: -1 }, { native_event_at_ms: 2 ** 53 },
  { seq: 2 }, { recorded_at_unix_ms: 9 }, { source_movement_seq: 4 }, { extra: true },
];
for (const patch of mutations) {
  const bytes = encode([header, { ...receipt, ...patch }, seal]);
  assert.throws(() => live.parseOperatorTurnStream(bytes, expected), JSON.stringify(patch));
  await assert.rejects(history(bytes), JSON.stringify(patch));
}
for (const rows of [[header, receipt], [header, seal, { ...receipt, seq: 3 }], [header, receipt, { ...seal, seq: 1 }]]) {
  assert.throws(() => live.parseOperatorTurnStream(encode(rows), expected)); await assert.rejects(history(encode(rows)));
}
const partial = Buffer.concat([encode([header, receipt]), Buffer.from('{"type":')]);
assert.equal(live.parseOperatorTurnStream(partial, { ...expected, complete: false }).rows.length, 1);
assert.throws(() => live.parseOperatorTurnStream(partial, expected)); await assert.rejects(history(partial));
console.log("PASS steering receipt: actual live/sealed parsers, all observation sources, strict identities, cursor and incomplete-tail boundaries");
}
main().catch(error => { console.error(error); process.exitCode = 1; });
