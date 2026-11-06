const assert = require("node:assert/strict");
const test = require("node:test");

const { homeStateClientContext, renderHomeState } = require("./home-state.cjs");
const { operatorMessagePayload, operatorTurnClientContext } = require("./client/operatorMessages.cjs");

const now = Date.parse("2026-10-19T09:00:00Z");
const projections = {
  "projection.operator.chats.list": { chats: [
    { run_id: "run_home", client_surface_role: "home", title: "Home", updated_at_ms: now },
    { run_id: "run_weather", client_surface_role: "task", title: "Погода в Киеве", writable: true, turn_open: true, ee_id: "ee_life", updated_at_ms: now - 180_000,
      last_terminal_message: { text: "Виджет обновлён." } },
    { run_id: "run_flights", client_surface_role: "task", title: "Flights", writable: true, turn_open: false, turn_state: "failed", ee_id: "ee_existence", updated_at_ms: now - 7_200_000 },
    { run_id: "run_old", client_surface_role: "task", title: "Old", archived: true },
    { run_id: "run_writer_internal", client_surface_role: "task", title: "run_writer_internal", writable: false, updated_at_ms: now },
  ] },
  "projection.scheduled_interfaces.list": { scheduled_interfaces: [
    { display_name: "Hourly weather", valid: true, status: "enabled", schedule: { type: "interval", interval_seconds: 3600 },
      next_run_at_ms: now + 840_000, last_run_at_ms: now - 2_760_000, targets: [{ run_id: "run_weather" }] },
    { display_name: "Broken", valid: false, error: "activation is malformed" },
    { display_name: "System review", template: true, valid: true, schedule: { type: "inactivity", seconds: 3600 } },
  ] },
  "projection.operator.os.scope": { status: "ready", root_ee_id: "ee_existence",
    projects: [{ ee_id: "ee_life", goal: "Everyday life" }],
    project_presentations: [{ ee_id: "ee_life", name: "Life" }],
    agent_presentations: [{ name: "Planner", project_ee_id: "ee_life", worker_interface: "system_worker_life" }] },
};

test("Home state names the operator's tasks, triggers, projects and agents as the client projects them", async () => {
  const context = await homeStateClientContext(async (method) => projections[method], now);
  assert.equal(context.name, "home_state");
  const lines = context.text.split("\n");
  assert.ok(lines.includes("Tasks (2 active, 1 working, 1 failed, 1 archived; newest first):"));
  assert.ok(lines.includes('- "Погода в Киеве" · working · project Life · 3 min ago · run_weather'));
  assert.ok(lines.includes('  last reply: "Виджет обновлён."'));
  assert.ok(lines.includes('- "Flights" · failed · 2 h ago · run_flights'));
  assert.ok(lines.includes('- "Hourly weather" · every 1 h · next in 14 min · last 46 min ago · wakes run_weather'));
  assert.ok(lines.includes('- "Broken" · invalid: activation is malformed'));
  assert.ok(lines.includes("Projects: Life (ee_life): Everyday life."));
  assert.ok(lines.includes("Agents: Planner in Life (worker system_worker_life, ee ee_life)."));
  assert.ok(lines.includes("Triggers (2):"));
  assert.ok(["run_home", "run_old", "run_writer_internal", "System review"].every((hidden) => !context.text.includes(hidden)));
});

test("Home state stays inside its byte budget and states an unreadable projection explicitly", async () => {
  const many = Array.from({ length: 400 }, (_, index) => ({ run_id: `run_${index}`, client_surface_role: "task", writable: true, title: "Ж".repeat(200),
    last_terminal_message: { text: "Ж".repeat(400) }, updated_at_ms: now }));
  const big = renderHomeState({ chats: many, triggers: [], scope: { projects: Array.from({ length: 50 }, (_, i) => ({ ee_id: `ee_${i}`, goal: "Ж".repeat(300) })) }, nowMs: now });
  assert.ok(Buffer.byteLength(big) <= 12 * 1024);
  const failed = await homeStateClientContext(async () => { throw new Error("projection offline"); }, now);
  assert.match(failed.text, /^Home state could not be read at 2026-10-19T09:00:00.000Z: projection offline\./);
});

test("operator messages carry Home client context only for Home and validate it fail-closed", async () => {
  assert.equal((await operatorTurnClientContext({ readProjection: async (method) => projections[method] }, "task")).length, 0);
  const [context] = await operatorTurnClientContext({ readProjection: async (method) => projections[method] }, "home");
  const payload = { text: "Что дальше?", message_id: "msg_1", attachments: [], client_context: [context] };
  assert.equal(JSON.stringify(operatorMessagePayload(payload, "msg_1", "/tmp/attachments").client_context), JSON.stringify([context]));
  for (const client_context of [[], [{ name: "Home", text: "x" }], [{ name: "home_state", text: " x" }],
    [{ name: "a", text: "x" }, { name: "a", text: "y" }], [{ name: "a", text: "x".repeat(17 * 1024) }], [{ name: "a", text: "x", extra: 1 }]]) {
    assert.throws(() => operatorMessagePayload({ ...payload, client_context }, "msg_1", "/tmp/attachments"));
  }
  assert.throws(() => operatorMessagePayload({ ...payload, context: [] }, "msg_1", "/tmp/attachments"), /invalid field set/);
  await assert.rejects(operatorTurnClientContext({ readProjection: "ambient" }, "home"), /trusted client callback/);
});
