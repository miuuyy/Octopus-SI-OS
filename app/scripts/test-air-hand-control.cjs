const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");

const loaded = new Map();
function load(relative) {
  const filename = path.resolve(__dirname, "..", relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
    fileName: filename,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  loaded.set(filename, module);
  const local = (id) => id.startsWith(".") ? load(path.relative(path.resolve(__dirname, ".."), path.resolve(path.dirname(filename), `${id}.ts`))) : require(id);
  Function("module", "exports", "require", code)(module, module.exports, local);
  return module.exports;
}

const { AirHandGestureEngine } = load("src/airHandEngine.ts");
const { rankSpatialCandidates, nearestSpatialCandidate, trackAim, IDLE_AIM } = load("src/gestureSpatialNavigation.ts");
const { traceFrame, traceObservations, parseGestureTrace } = load("src/gestureTrace.ts");

const FINGERS = [[5, -0.45], [9, -0.15], [13, 0.15], [17, 0.42]];
const LENGTHS = [0.4, 0.25, 0.2];
const STRAIGHT = [5, 5, 3];
const CURLED = [75, 100, 60];
const POSES = {
  open: [STRAIGHT, STRAIGHT, STRAIGHT, STRAIGHT],
  point: [STRAIGHT, CURLED, CURLED, CURLED],
  v: [STRAIGHT, STRAIGHT, CURLED, CURLED],
  fist: [CURLED, CURLED, CURLED, CURLED],
  pinch: [[30, 45, 30], CURLED, CURLED, CURLED],
  okPinch: [[30, 45, 30], STRAIGHT, STRAIGHT, STRAIGHT],
  loose: [STRAIGHT, CURLED, STRAIGHT, CURLED],
};

function shape(pose) {
  const points = new Array(21);
  points[0] = [0, 1];
  FINGERS.forEach(([mcp, x], finger) => {
    let position = [x, 0];
    let angle = 0;
    points[mcp] = position;
    POSES[pose][finger].forEach((bend, segment) => {
      angle += bend * Math.PI / 180;
      position = [position[0] + Math.sin(angle) * LENGTHS[segment], position[1] - Math.cos(angle) * LENGTHS[segment]];
      points[mcp + segment + 1] = position;
    });
  });
  const tip = points[8];
  const thumb = pose === "pinch" || pose === "okPinch" ? [[-0.45, 0.75], [-0.7, 0.5], [(tip[0] - 0.7) / 2 - 0.1, (tip[1] + 0.5) / 2], [tip[0] - 0.03, tip[1] + 0.03]]
    : pose === "fist" ? [[-0.45, 0.75], [-0.6, 0.5], [-0.45, 0.3], [-0.2, 0.3]]
      : [[-0.45, 0.75], [-0.75, 0.55], [-1, 0.35], [-1.25, 0.15]];
  thumb.forEach((point, index) => { points[index + 1] = point; });
  return points;
}

function hand(x, y, pose, label = "Left") {
  const points = shape(pose);
  return {
    image: points.map(([px, py]) => ({ x: x + px * 0.1, y: y + py * 0.1, z: 0 })),
    world: points.map(([px, py]) => ({ x: px * 0.09, y: py * 0.09, z: 0 })),
    label,
    score: 0.95,
  };
}

const OPTIONS = { aspect: 1, mirrorX: true, sensitivity: "balanced" };

function run(frames, options = OPTIONS) {
  const engine = new AirHandGestureEngine();
  const out = { commands: [], interactions: [], hands: [], times: [] };
  let now = 0;
  for (const frame of frames) {
    now += frame.dt ?? 33;
    const result = engine.process(frame.hands, now, options);
    out.commands.push(...result.commands);
    out.interactions.push(...result.interactions);
    out.times.push(...result.interactions.map(() => now));
    out.hands.push(result.hands);
  }
  return out;
}

function replay(trace) {
  const engine = new AirHandGestureEngine();
  return trace.frames.map((frame) => engine.process(traceObservations(frame), frame.t, { aspect: frame.aspect, mirrorX: trace.options.mirrorX, sensitivity: trace.options.sensitivity }));
}

const at = (x, pose, y = 0.5) => ({ hands: [hand(x, y, pose)] });
const lost = (count) => Array.from({ length: count }, () => ({ hands: [] }));
const still = (x, count, pose, y = 0.5) => Array.from({ length: count }, () => at(x, pose, y));
const move = (from, to, count, pose, dt) => Array.from({ length: count }, (_, index) => ({ ...at(from + (to - from) * ((index + 1) / count), pose), dt }));
const moveY = (x, from, to, count, pose) => Array.from({ length: count }, (_, index) => at(x, pose, from + (to - from) * ((index + 1) / count)));
const kinds = (interactions) => interactions.map((entry) => entry.type === "release" ? `release:${entry.outcome}` : entry.phase ? `${entry.type}:${entry.phase}` : entry.type);
const aims = (frames) => run(frames).interactions.filter((entry) => entry.type === "point");

test("a quick open-hand or two-finger flick is one swipe in the mirrored direction", () => {
  assert.deepEqual(run([...still(0.35, 12, "open"), ...move(0.35, 0.65, 8, "open")]).commands, ["swipe_left"]);
  assert.deepEqual(run([...still(0.65, 12, "v"), ...move(0.65, 0.35, 8, "v")]).commands, ["swipe_right"]);
});

test("frames lost to motion blur or the frame edge do not drop the swipe", () => {
  assert.deepEqual(run([...still(0.35, 12, "open"), ...move(0.35, 0.45, 3, "open"), ...lost(2), ...move(0.5, 0.65, 4, "open")]).commands, ["swipe_left"]);
  assert.deepEqual(run([...still(0.3, 12, "open"), ...move(0.3, 0.5, 4, "open"), ...lost(2)]).commands, ["swipe_left"]);
});

test("slow drift, the return stroke, and a moving pointing hand are not swipes", () => {
  assert.deepEqual(run(move(0.3, 0.7, 60, "open")).commands, []);
  assert.deepEqual(run([...still(0.35, 12, "open"), ...move(0.35, 0.65, 8, "open"), ...move(0.65, 0.35, 8, "open")]).commands, ["swipe_left"]);
  assert.deepEqual(run([...still(0.35, 12, "point"), ...move(0.35, 0.65, 10, "point")]).commands, []);
});

test("a hand moving around, arriving, or leaving the frame is not a swipe", () => {
  assert.deepEqual(run([...still(0.4, 12, "open"), ...move(0.4, 0.475, 3, "open"), ...lost(2)]).commands, []);
  assert.deepEqual(run([...still(0.4, 12, "open"), ...move(0.4, 0.5, 6, "open")]).commands, []);
  assert.deepEqual(run(move(0.2, 0.6, 7, "open")).commands, []);
});

test("a pointing hand aims where it is inside an inner region shifted toward its own side", () => {
  const settled = (frames, options) => {
    const points = run(frames, options).interactions.filter((entry) => entry.type === "point");
    return points[points.length - 1];
  };
  const center = settled(still(0.474, 15, "point"));
  assert.ok(Math.abs(center.x - 0.5) < 0.02 && Math.abs(center.y - 0.555) < 0.03, `center aim ${center.x}, ${center.y}`);
  assert.ok(settled(still(0.8, 15, "point")).x < 0.001);
  assert.ok(settled(still(0.15, 15, "point")).x > 0.999);
  assert.ok(settled(still(0.5, 15, "point", 0.8)).y > 0.999);
  assert.ok(settled(still(0.8, 15, "point"), { ...OPTIONS, sensitivity: "deliberate" }).x > 0.02);
  const otherHand = settled(still(0.5, 15, "point").map(() => ({ hands: [hand(0.5, 0.5, "point", "Right")] })));
  assert.ok(otherHand.x - settled(still(0.5, 15, "point")).x > 0.12, `left hand aim ${otherHand.x}`);
  assert.ok(settled([...still(0.5, 12, "point"), ...move(0.5, 0.8, 10, "point"), ...still(0.8, 15, "point")]).x < 0.001);
});

test("a pointing finger turned toward the camera does not click, and a flickering pinch clicks once", () => {
  const actions = (frames) => kinds(run(frames).interactions.filter((entry) => entry.type !== "point"));
  const foreshortened = () => {
    const observation = hand(0.5, 0.5, "point");
    const image = observation.image.map((point) => ({ ...point }));
    image[4] = { ...image[8] };
    return { hands: [{ ...observation, image }] };
  };
  assert.deepEqual(actions([...still(0.5, 12, "point"), ...Array.from({ length: 20 }, foreshortened)]), []);
  const flicker = Array.from({ length: 4 }, () => [...still(0.5, 2, "pinch"), ...still(0.5, 2, "point")]).flat();
  const clicks = actions([...still(0.5, 12, "point"), ...flicker, ...still(0.5, 6, "point")]).filter((entry) => entry === "release:select");
  assert.ok(clicks.length <= 2, `flicker produced ${clicks.length} clicks`);
});

test("brief pose dropouts keep pointing, and a hand of no clear pose never swipes", () => {
  const points = aims([...still(0.5, 12, "point"), ...Array.from({ length: 6 }, (_, cycle) => [at(0.5 + cycle * 0.01, "point"), at(0.5, "open"), at(0.5, "open"), at(0.5, "point"), at(0.5, "point")]).flat()]);
  assert.deepEqual([points.filter((entry) => entry.phase === "begin").length, points.filter((entry) => entry.phase === "end").length], [1, 0]);
  assert.deepEqual(run([...still(0.35, 4, "loose"), ...move(0.35, 0.65, 10, "loose")]).commands, []);
});

test("aiming starts only after the pose settles, ignores jitter, and a hand that leaves does not teleport it", () => {
  const started = run(still(0.5, 12, "point"));
  assert.ok(started.times[0] >= 200, `aim began at ${started.times[0]} ms`);
  const jittered = aims([...still(0.5, 10, "point"), ...Array.from({ length: 60 }, (_, index) => at(0.5 + (index % 2 ? 0.006 : -0.006), "point"))]);
  const xs = jittered.slice(10).map((entry) => entry.x);
  assert.ok(Math.max(...xs) - Math.min(...xs) < 0.008, `jitter spread ${Math.max(...xs) - Math.min(...xs)}`);
  const left = aims([...still(0.5, 12, "point"), ...lost(15)]);
  const last = left[left.length - 1];
  assert.deepEqual([last.phase, left.filter((entry) => entry.phase === "begin").length], ["end", 1]);
  assert.ok(Math.abs(last.x - left[left.length - 2].x) < 0.001);
  assert.deepEqual(aims([...still(0.5, 6, "fist"), ...move(0.5, 0.3, 10, "fist")]), []);
  assert.deepEqual(aims([...still(0.5, 6, "open"), ...move(0.5, 0.45, 10, "open")]), []);
});

test("focus switches only to a clearly closer target that stays closer for the dwell", () => {
  const rects = [{ left: 0, top: 0, right: 100, bottom: 40 }, { left: 110, top: 0, right: 210, bottom: 40 }];
  const options = { dwellMs: 90, marginMin: 6, marginMax: 18, marginFraction: 0.3 };
  const step = (state, x, now) => trackAim(state, { x, y: 20 }, rects, now, options);
  let state = step(IDLE_AIM, 50, 0);
  assert.equal(state.current, 0);
  state = step(state, 105, 10);
  assert.equal(state.candidate, -1);
  state = step(step(step(state, 118, 20), 118, 60), 118, 100);
  assert.equal(state.current, 0);
  state = step(state, 118, 115);
  assert.equal(state.current, 1);
  let brief = step(step(IDLE_AIM, 50, 0), 118, 10);
  brief = step(step(brief, 60, 40), 60, 300);
  assert.equal(brief.current, 0);
});

test("both hands keep their side through a reordered result and a flickering label", () => {
  const frames = Array.from({ length: 30 }, (_, index) => {
    const left = hand(0.7 + (index % 3) * 0.002, 0.5, "open", index % 4 === 0 ? "Left" : "Right");
    const right = hand(0.3, 0.5 + (index % 2) * 0.003, "point", "Left");
    return { hands: index % 2 ? [left, right] : [right, left] };
  });
  const seen = run(frames).hands.filter((hands) => hands.length === 2);
  assert.ok(seen.length >= 25);
  const ids = new Set();
  for (const hands of seen) {
    const sorted = [...hands].sort((a, b) => a.palm.x - b.palm.x);
    assert.deepEqual(sorted.map((entry) => entry.side), ["left", "right"]);
    assert.deepEqual(sorted.map((entry) => entry.pose), ["open", "point"]);
    ids.add(sorted.map((entry) => entry.id).join(":"));
  }
  assert.equal(ids.size, 1);
});

test("a pinch tap selects, a held pinch carries, and a thrown pinch flings", () => {
  const actions = (frames) => run(frames).interactions.filter((entry) => entry.type !== "point");
  assert.deepEqual(kinds(actions([...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...still(0.5, 3, "point")])), ["press", "release:select"]);
  const carried = actions([...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...move(0.5, 0.4, 12, "pinch"), ...still(0.4, 3, "pinch"), ...still(0.4, 3, "point")]);
  const carriedKinds = kinds(carried);
  assert.deepEqual([carriedKinds[0], carriedKinds[1], carriedKinds[carriedKinds.length - 1]], ["press", "grab:begin", "release:drop"]);
  assert.ok(carriedKinds.slice(2, -1).every((kind) => kind === "grab:move"));
  assert.ok(Math.abs(carried[carried.length - 1].dx - 0.1) < 0.01, `dropped at ${carried[carried.length - 1].dx}`);
  const thrown = kinds(actions([...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...move(0.5, 0.2, 5, "pinch"), ...still(0.2, 3, "point")]));
  assert.equal(thrown[thrown.length - 1], "release:fling");
});

test("a pinch is recognized whatever the other fingers do, even fast at a low frame rate, and a fist never pinches", () => {
  const tap = (pose, dt, frames) => kinds(run([...still(0.5, Math.ceil(400 / dt), "point").map((frame) => ({ ...frame, dt })), ...still(0.5, frames, pose).map((frame) => ({ ...frame, dt })), ...still(0.5, 3, "point").map((frame) => ({ ...frame, dt }))]).interactions.filter((entry) => entry.type !== "point"));
  assert.deepEqual(tap("pinch", 33, 4), ["press", "release:select"]);
  assert.deepEqual(tap("okPinch", 33, 4), ["press", "release:select"]);
  assert.deepEqual(tap("pinch", 66, 3), ["press", "release:select"]);
  const wobble = kinds(run([...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...move(0.5, 0.47, 4, "pinch"), ...still(0.47, 3, "point")]).interactions.filter((entry) => entry.type !== "point"));
  assert.deepEqual(wobble, ["press", "release:select"]);
  assert.deepEqual(kinds(run(still(0.5, 12, "fist")).interactions), []);
});

test("only a held, deliberate pinch clicks", () => {
  const actions = (frames) => kinds(run(frames).interactions.filter((entry) => entry.type !== "point"));
  assert.deepEqual(actions([...still(0.5, 12, "point"), ...still(0.5, 3, "pinch"), ...still(0.5, 6, "point")]), []);
  assert.deepEqual(actions([...still(0.5, 15, "pinch"), ...still(0.5, 6, "point")]), []);
  assert.deepEqual(actions([...still(0.3, 12, "point"), ...move(0.3, 0.7, 12, "pinch"), ...still(0.7, 6, "point")]), []);
  const worldOnly = () => {
    const observation = hand(0.5, 0.5, "point");
    const world = observation.world.map((point) => ({ ...point }));
    world[4] = { ...world[8] };
    return { hands: [{ ...observation, world }] };
  };
  assert.deepEqual(actions([...still(0.5, 12, "point"), ...Array.from({ length: 12 }, worldOnly)]), []);
  const twitches = Array.from({ length: 4 }, () => [...still(0.5, 4, "pinch"), ...still(0.5, 3, "point")]).flat();
  const clicks = actions([...still(0.5, 12, "point"), ...twitches]).filter((entry) => entry === "release:select");
  assert.equal(clicks.length, 2);
});

test("a pinch survives blurred frames and a hand that leaves never selects", () => {
  const actions = (frames) => run(frames).interactions.filter((entry) => entry.type !== "point");
  assert.deepEqual(kinds(actions([...still(0.5, 12, "point"), ...still(0.5, 5, "pinch"), ...lost(12)])), ["press", "cancel"]);
  const blurred = kinds(actions([...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...move(0.5, 0.42, 6, "pinch"), ...lost(2), ...move(0.42, 0.36, 4, "pinch"), ...still(0.36, 3, "point")]));
  assert.equal(blurred.filter((kind) => kind === "grab:begin").length, 1);
  assert.deepEqual(blurred.filter((kind) => kind.startsWith("release") || kind === "cancel"), ["release:drop"]);
});

test("pinching with both hands and spreading them scales instead of selecting", () => {
  const pair = (left, right, pose) => ({ hands: [hand(1 - left, 0.5, pose, "Right"), hand(1 - right, 0.5, pose, "Left")] });
  const frames = [
    ...Array.from({ length: 12 }, () => pair(0.4, 0.6, "point")),
    ...Array.from({ length: 4 }, () => pair(0.4, 0.6, "pinch")),
    ...Array.from({ length: 10 }, (_, index) => pair(0.4 - 0.015 * (index + 1), 0.6 + 0.015 * (index + 1), "pinch")),
    ...Array.from({ length: 3 }, () => pair(0.25, 0.75, "pinch")),
    ...Array.from({ length: 3 }, () => pair(0.25, 0.75, "point")),
  ];
  const interactions = run(frames).interactions.filter((entry) => entry.type !== "point");
  const sequence = kinds(interactions);
  assert.equal(sequence[0], "transform:begin");
  assert.equal(sequence[sequence.length - 1], "transform:end");
  assert.ok(!sequence.some((kind) => kind === "press" || kind.startsWith("release")));
  assert.ok(Math.max(...interactions.filter((entry) => entry.type === "transform").map((entry) => entry.scale)) > 1.3);
});

test("spatial focus picks the nearest target in the direction, preferring targets in line", () => {
  const cell = (column, row) => ({ left: column * 120, top: row * 60, right: column * 120 + 100, bottom: row * 60 + 40 });
  const grid = [0, 1, 2].flatMap((row) => [0, 1, 2].map((column) => cell(column, row)));
  const center = cell(1, 1);
  assert.equal(rankSpatialCandidates(center, "right", grid)[0], 5);
  assert.equal(rankSpatialCandidates(center, "left", grid)[0], 3);
  assert.equal(rankSpatialCandidates(center, "up", grid)[0], 1);
  assert.equal(rankSpatialCandidates(center, "down", grid)[0], 7);
  const origin = { left: 0, top: 0, right: 100, bottom: 40 };
  assert.deepEqual(rankSpatialCandidates(origin, "right", [{ left: 150, top: 200, right: 250, bottom: 240 }, { left: 400, top: 0, right: 500, bottom: 40 }]), [1, 0]);
  assert.deepEqual(rankSpatialCandidates(origin, "left", [{ left: 150, top: 0, right: 250, bottom: 40 }]), []);
  const row = { left: 0, top: 0, right: 300, bottom: 40 };
  const inner = { left: 260, top: 5, right: 290, bottom: 35 };
  assert.deepEqual(rankSpatialCandidates(row, "right", [inner]), [0]);
  assert.deepEqual(rankSpatialCandidates(inner, "left", [row]), [0]);
  assert.equal(nearestSpatialCandidate({ x: 130, y: 70 }, grid), 4);
});

test("a recorded gesture trace replays through the engine to the same decisions", () => {
  const engine = new AirHandGestureEngine();
  const frames = [...still(0.5, 12, "point"), ...still(0.5, 4, "pinch"), ...still(0.5, 3, "point"), ...lost(3)];
  let now = 0;
  const recorded = frames.map((frame) => {
    now += 33;
    return traceFrame(now, 1, frame.hands, engine.process(frame.hands, now, OPTIONS));
  });
  const trace = parseGestureTrace(JSON.parse(JSON.stringify({ format: "octopus.gesture-trace", version: 1, recordedAt: "", label: "synthetic", options: { mirrorX: true, sensitivity: "balanced" }, camera: { width: 1280, height: 720 }, frames: recorded })));
  const replayed = replay(trace).flatMap((result) => result.interactions);
  assert.deepEqual(kinds(replayed), kinds(recorded.flatMap((frame) => frame.decision.interactions)));
  assert.throws(() => parseGestureTrace({ format: "octopus.gesture-trace", version: 2, frames: [] }), /not octopus.gesture-trace v1/);
});

test("a point interaction moves focus while the Settings gesture page is mounted", async () => {
  const { build } = require("esbuild");
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
  const w = dom.window;
  for (const key of ["window", "document", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "localStorage", "navigator", "getComputedStyle", "MutationObserver"]) {
    Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : key === "getComputedStyle" ? w[key].bind(w) : w[key] });
  }
  Object.defineProperty(w, "innerWidth", { configurable: true, value: 1440 });
  Object.defineProperty(w, "innerHeight", { configurable: true, value: 900 });
  globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
  globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
  globalThis.IntersectionObserver = w.IntersectionObserver = class { observe() {} disconnect() {} };
  globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
  w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} });
  const probes = { left: [100, 400, 120, 40], right: [1200, 400, 120, 40] };
  const box = (element) => {
    const control = element.matches("button, a, input, select, textarea, [role], [tabindex], [contenteditable]");
    const [left, top, width, height] = probes[element.dataset?.probe] ?? (control ? [0, 0, 0, 0] : [0, 0, 1440, 900]);
    return { x: left, y: top, left, top, right: left + width, bottom: top + height, width, height };
  };
  w.Element.prototype.getBoundingClientRect = function () { return box(this); };
  w.Element.prototype.checkVisibility = () => true;
  w.document.elementFromPoint = (x, y) => [...w.document.querySelectorAll("[data-probe]")].find((element) => {
    const rect = box(element);
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }) ?? w.document.body;
  const bundle = await build({
    stdin: {
      contents: [
        'import { createElement } from "react";',
        'import { GestureManager } from "./src/components/settings/GestureManager";',
        'import { useGestureFocus } from "./src/gestureFocus";',
        'export { emitGestureInteraction } from "./src/gestureShortcuts";',
        'export function Page() { useGestureFocus("live", true, true); return createElement("main", null, createElement("button", { "data-probe": "left" }, "Left"), createElement("button", { "data-probe": "right" }, "Right"), createElement(GestureManager, { accessProfile: "live" })); }',
      ].join("\n"),
      resolveDir: path.join(__dirname, ".."),
      loader: "tsx",
    },
    bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
    define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
    loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".woff2": "empty", ".wasm": "empty" },
    external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@mediapipe/tasks-vision"],
  });
  const page = { exports: {} };
  Function("require", "module", "exports", bundle.outputFiles[0].text)(require, page, page.exports);
  const React = require("react");
  const { createRoot } = require("react-dom/client");
  const root = createRoot(w.document.getElementById("root"));
  try {
    root.render(React.createElement(page.exports.Page));
    const focused = () => w.document.querySelector("[data-gesture-focused]")?.dataset.probe ?? null;
    const point = (phase, x) => page.exports.emitGestureInteraction({ type: "point", side: "right", phase, x, y: 0.46 }, "camera");
    const deadline = Date.now() + 5000;
    do {
      await new Promise((resolve) => setTimeout(resolve, 25));
      point("begin", 0.86);
    } while (focused() !== "right" && Date.now() < deadline);
    assert.ok(w.document.querySelector(".gesture-manager"), "the Settings gesture page is mounted");
    assert.equal(focused(), "right");
    point("move", 0.11);
    await new Promise((resolve) => setTimeout(resolve, 120));
    point("move", 0.11);
    assert.equal(focused(), "left");
  } finally {
    root.unmount();
    dom.window.close();
  }
});

const tracePath = process.env.AIR_HAND_TRACE ?? (process.argv.includes("--trace") ? process.argv[process.argv.indexOf("--trace") + 1] : "");
if (tracePath) {
  test(`replays ${path.basename(tracePath)}`, () => {
    const trace = parseGestureTrace(JSON.parse(fs.readFileSync(tracePath, "utf8")));
    const results = replay(trace);
    const events = (frames) => frames.flatMap(({ t, decision }) => [
      ...decision.interactions.filter((entry) => entry.type !== "point" || entry.phase !== "move").map((entry) => `${Math.round(t)}ms ${entry.type}${entry.phase ? `:${entry.phase}` : ""}${entry.outcome ? `:${entry.outcome}` : ""}`),
      ...decision.commands.map((command) => `${Math.round(t)}ms command:${command}`),
    ]);
    const recorded = events(trace.frames);
    const replayed = events(trace.frames.map((frame, index) => ({ t: frame.t, decision: results[index] })));
    const poses = {};
    for (const result of results) for (const hand of result.hands) poses[`${hand.side}:${hand.pose}`] = (poses[`${hand.side}:${hand.pose}`] ?? 0) + 1;
    const peakPinch = Math.max(0, ...results.flatMap((result) => result.hands.map((hand) => hand.pinch)));
    console.log(JSON.stringify({
      label: trace.label,
      frames: trace.frames.length,
      seconds: Math.round((trace.frames[trace.frames.length - 1]?.t ?? 0) / 100) / 10,
      handFrames: poses,
      peakPinch: Math.round(peakPinch * 100) / 100,
      recorded,
      replayed,
      changed: recorded.length !== replayed.length || recorded.some((entry, index) => entry !== replayed[index]),
    }, null, 2));
    assert.ok(trace.frames.length > 0);
  });
}
