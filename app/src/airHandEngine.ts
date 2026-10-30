import type { AirHandGestureId, GestureSensitivity } from "./gestureShortcuts";

export type HandSide = "left" | "right";
export type HandPose = "none" | "point" | "two" | "open" | "fist" | "pinch";
export type NavigationDirection = "left" | "right" | "up" | "down";
export type AirHandPoint = Readonly<{ x: number; y: number }>;
export type FingerCurls = { thumb: number; index: number; middle: number; ring: number; little: number };
export type HandLandmark = Readonly<{ x: number; y: number; z: number }>;

export type HandObservation = Readonly<{
  image: readonly HandLandmark[];
  world: readonly HandLandmark[];
  label: string;
  score: number;
}>;

export type AirHandOptions = Readonly<{ aspect: number; mirrorX: boolean; sensitivity: GestureSensitivity }>;

export type TrackedHand = Readonly<{
  id: number;
  side: HandSide;
  pose: HandPose;
  palm: AirHandPoint;
  pinch: number;
  curls: FingerCurls;
  confidence: number;
  coasting: boolean;
  engaged: boolean;
  aim: AirHandPoint;
  landmarks: readonly HandLandmark[];
}>;

export type GestureInteraction =
  | Readonly<{ type: "point"; side: HandSide; phase: "begin" | "move" | "end"; x: number; y: number }>
  | Readonly<{ type: "navigate"; side: HandSide; direction: NavigationDirection }>
  | Readonly<{ type: "press"; side: HandSide }>
  | Readonly<{ type: "grab"; side: HandSide; phase: "begin" | "move"; dx: number; dy: number }>
  | Readonly<{ type: "release"; side: HandSide; outcome: "select" | "drop" | "fling"; dx: number; dy: number }>
  | Readonly<{ type: "cancel"; side: HandSide }>
  | Readonly<{ type: "transform"; phase: "begin" | "move" | "end"; scale: number; dx: number; dy: number }>;

export type AirHandFrame = Readonly<{
  hands: readonly TrackedHand[];
  interactions: readonly GestureInteraction[];
  commands: readonly AirHandGestureId[];
}>;

type Point = { x: number; y: number };

type ParsedHand = {
  image: readonly HandLandmark[];
  observedSide: HandSide | null;
  score: number;
  screen: Point;
  scale: number;
  palmWidth: number;
  curls: FingerCurls;
  pinchRatio: number;
  imagePinchRatio: number;
  indexReach: number;
};

type Mode =
  | { kind: "idle" }
  | { kind: "press"; track: number; origin: Point }
  | { kind: "grab"; track: number; origin: Point; dx: number; dy: number; travel: number }
  | { kind: "transform"; a: number; b: number; distance: number; origin: Point; scale: number; dx: number; dy: number };

const IDLE: Mode = { kind: "idle" };
const MAX_HANDS = 2;
const TRACK_CONFIRM_FRAMES = 3;
const TRACK_GRACE_MS = 300;
const TRACK_ENGAGE_MS = 250;
const ASSOCIATION_GATE = 0.32;
const SIDE_MISMATCH_COST = 0.06;
const SIDE_VOTE_RATE = 0.2;
const SIDE_FLIP_LOW = 0.3;
const SIDE_FLIP_HIGH = 0.7;
const POSE_WINDOW = 5;
const POSE_VOTES = 3;
const PINCH_ENTER_MS = 80;
const PINCH_RELEASE_FRAMES = 2;
const PINCH_ENTER_RATIO = 0.38;
const PINCH_IMAGE_ENTER_RATIO = 0.45;
const PINCH_RELEASE_RATIO = 0.55;
const PINCH_REFRACTORY_MS = 450;
const PINCH_MIN_INDEX_REACH = 1.05;
const PINCH_MAX_HAND_SPEED = 1;
const PINCH_SPEED_WINDOW_MS = 120;
const POINT_GRACE_MS = 220;
const POINT_SETTLE_MS = 150;
const AIM_SIDE_SHIFT = 0.05;
const AIM_CENTER_Y = 0.47;
const AIM_EDGE = 0.001;
const AIM_SIZE: Readonly<Record<GestureSensitivity, Readonly<{ width: number; height: number }>>> = {
  responsive: { width: 0.54, height: 0.56 },
  balanced: { width: 0.62, height: 0.64 },
  deliberate: { width: 0.72, height: 0.74 },
};
const DRAG_START_HANDS = 0.6;
const DRAG_START_MIN = 0.045;
const DRAG_START_MAX = 0.11;
const FLING_SPEED = 1.2;
const FLING_MIN_TRAVEL = 0.08;
const FLING_WINDOW_MS = 160;
const SPEED_SPAN_MS = 50;
const HISTORY_MS = 320;
const TRANSFORM_DEADBAND = 0.06;
const TRANSFORM_MIN_SCALE = 0.35;
const TRANSFORM_MAX_SCALE = 3;
const HOLD_STILL_TRAVEL = 0.035;
const HAND_SCALE_RATE = 0.15;

export class AirHandGestureEngine {
  private tracks: HandTrack[] = [];
  private nextId = 1;
  private mode: Mode = IDLE;
  private pointerId: number | null = null;
  private pointerBegun = false;
  private spent = new Set<number>();

  reset(): GestureInteraction[] {
    const interactions: GestureInteraction[] = [];
    for (const track of [...this.tracks]) this.endFor(track, interactions, false);
    this.endPointer(interactions);
    this.tracks = [];
    this.mode = IDLE;
    this.spent.clear();
    return interactions;
  }

  process(observations: readonly HandObservation[], now: number, options: AirHandOptions): AirHandFrame {
    const interactions: GestureInteraction[] = [];
    const commands: AirHandGestureId[] = [];
    const hands = Number.isFinite(options.aspect) && options.aspect > 0
      ? observations.map((observation) => parseObservation(observation, options)).filter((hand): hand is ParsedHand => hand != null)
      : [];
    const matched = this.associate(hands, now, options, interactions);
    for (const track of this.tracks) {
      if (!matched.has(track)) this.miss(track, now, options, commands);
    }
    for (const track of [...this.tracks]) {
      if (now - track.lastSeen > TRACK_GRACE_MS) this.drop(track, interactions);
    }
    this.resolveSides();
    const idle = this.mode.kind === "idle";
    for (const track of this.tracks) {
      if (track.confirmed && !track.coasting) this.trackGestures(track, now, options, idle, interactions, commands);
    }
    this.arbitrate(now, options.aspect, interactions);
    return {
      hands: this.tracks.filter((track) => track.confirmed).map((track) => track.view(this.engaged(track.id))),
      interactions,
      commands,
    };
  }

  private associate(hands: ParsedHand[], now: number, options: AirHandOptions, interactions: GestureInteraction[]): Set<HandTrack> {
    const matched = new Set<HandTrack>();
    const used = new Set<number>();
    for (const [trackIndex, handIndex] of bestAssignment(this.tracks, hands, options.aspect)) {
      const track = this.tracks[trackIndex];
      const hand = hands[handIndex];
      if (!track || !hand) continue;
      track.observe(hand, now, options);
      matched.add(track);
      used.add(handIndex);
    }
    hands.forEach((hand, index) => {
      if (used.has(index)) return;
      if (this.tracks.length >= MAX_HANDS) {
        const stale = this.tracks.filter((track) => !matched.has(track)).sort((left, right) => left.lastSeen - right.lastSeen)[0];
        if (!stale) return;
        this.drop(stale, interactions);
      }
      const track = new HandTrack(this.nextId, hand, now);
      this.nextId += 1;
      track.observe(hand, now, options);
      this.tracks.push(track);
      matched.add(track);
    });
    return matched;
  }

  private miss(track: HandTrack, now: number, options: AirHandOptions, commands: AirHandGestureId[]): void {
    track.coasting = true;
    if (!track.confirmed) return;
    const swiped = swipeCommand(track.swipe.lost(now, swipeThreshold(options.sensitivity), swipeSpeed(options.sensitivity)));
    if (swiped && this.mode.kind === "idle") commands.push(swiped);
  }

  private drop(track: HandTrack, interactions: GestureInteraction[]): void {
    this.endFor(track, interactions, true);
    if (this.pointerId === track.id) this.endPointer(interactions);
    this.tracks = this.tracks.filter((candidate) => candidate !== track);
    this.spent.delete(track.id);
  }

  private endPointer(interactions: GestureInteraction[]): void {
    const track = this.pointerId == null ? undefined : this.track(this.pointerId);
    if (track && this.pointerBegun) interactions.push({ type: "point", side: track.side, phase: "end", x: track.aim.x, y: track.aim.y });
    this.pointerId = null;
    this.pointerBegun = false;
  }

  private endFor(track: HandTrack, interactions: GestureInteraction[], allowFling: boolean): void {
    const mode = this.mode;
    if (mode.kind === "press" && mode.track === track.id) {
      interactions.push({ type: "cancel", side: track.side });
      this.mode = IDLE;
    } else if (mode.kind === "grab" && mode.track === track.id) {
      const fling = allowFling && mode.travel >= FLING_MIN_TRAVEL && track.peakSpeed(track.lastSeen, FLING_WINDOW_MS) >= FLING_SPEED;
      interactions.push({ type: "release", side: track.side, outcome: fling ? "fling" : "drop", dx: mode.dx, dy: mode.dy });
      this.mode = IDLE;
    } else if (mode.kind === "transform" && (mode.a === track.id || mode.b === track.id)) {
      interactions.push({ type: "transform", phase: "end", scale: mode.scale, dx: mode.dx, dy: mode.dy });
      this.spent.add(mode.a === track.id ? mode.b : mode.a);
      this.mode = IDLE;
    }
  }

  private resolveSides(): void {
    const [first, second] = this.tracks.filter((track) => track.confirmed);
    if (!first || !second || first.side !== second.side) return;
    const side = first.side;
    const firstOuter = side === "right" ? first.stable.x >= second.stable.x : first.stable.x <= second.stable.x;
    (firstOuter ? second : first).assignSide(side === "right" ? "left" : "right");
  }

  private trackGestures(
    track: HandTrack,
    now: number,
    options: AirHandOptions,
    idle: boolean,
    interactions: GestureInteraction[],
    commands: AirHandGestureId[],
  ): void {
    const hand = track.current;
    const pointing = track.pose === "point";
    const pointHeld = pointing || (track.pose === "none" && now < track.pointUntil);
    const swipeEligible = idle && track.engaged(now) && (track.pose === "open" || track.pose === "two");
    const threshold = Math.min(swipeThreshold(options.sensitivity), Math.max(SWIPE_MIN_TRAVEL, hand.palmWidth * SWIPE_HAND_WIDTHS));
    const swiped = swipeCommand(track.swipe.update(hand.screen, now, swipeEligible, threshold, swipeSpeed(options.sensitivity)));
    if (swiped) commands.push(swiped);

    if (this.pointerId === track.id) {
      if (!idle || !pointHeld) this.endPointer(interactions);
      else if (pointing && !this.pointerBegun && now - track.pointSince >= POINT_SETTLE_MS) {
        this.pointerBegun = true;
        track.aimSent = track.aim;
        interactions.push({ type: "point", side: track.side, phase: "begin", x: track.aim.x, y: track.aim.y });
      } else if (this.pointerBegun && (onEdge(track.aim) || track.aim.x !== track.aimSent.x || track.aim.y !== track.aimSent.y)) {
        track.aimSent = track.aim;
        interactions.push({ type: "point", side: track.side, phase: "move", x: track.aim.x, y: track.aim.y });
      }
    } else if (this.pointerId == null && idle && pointing && track.engaged(now)) {
      this.pointerId = track.id;
      this.pointerBegun = false;
    }

    if (!idle || track.pose !== "fist" || !track.engaged(now)) {
      track.hold = null;
      return;
    }
    const position = isoPoint(track.stable, options.aspect);
    if (!track.hold) track.hold = { since: now, anchor: position, fired: false };
    else if (distance(position, track.hold.anchor) > HOLD_STILL_TRAVEL) track.hold = { ...track.hold, since: now, anchor: position };
    if (!track.hold.fired && now - track.hold.since >= holdDuration(options.sensitivity)) {
      track.hold = { ...track.hold, fired: true };
      commands.push("fist");
    }
  }

  private arbitrate(now: number, aspect: number, interactions: GestureInteraction[]): void {
    for (const id of [...this.spent]) {
      if (this.track(id)?.pose !== "pinch") this.spent.delete(id);
    }
    for (const track of this.tracks) {
      if (track.pose === "pinch" && !track.engaged(now)) this.spent.add(track.id);
    }
    const pinching = this.tracks.filter((track) => track.confirmed && track.pose === "pinch" && !this.spent.has(track.id));
    const mode = this.mode;
    if (mode.kind === "idle") {
      const [first, second] = pinching;
      if (first && second) this.beginTransform(first, second, aspect, interactions);
      else if (first) {
        this.mode = { kind: "press", track: first.id, origin: first.stable };
        interactions.push({ type: "press", side: first.side });
      }
      return;
    }
    if (mode.kind === "press" || mode.kind === "grab") {
      const track = this.track(mode.track);
      if (!track) {
        this.mode = IDLE;
        return;
      }
      const other = pinching.find((candidate) => candidate.id !== track.id);
      if (other && track.pose === "pinch") {
        if (mode.kind === "press") interactions.push({ type: "cancel", side: track.side });
        else interactions.push({ type: "release", side: track.side, outcome: "drop", dx: mode.dx, dy: mode.dy });
        this.beginTransform(track, other, aspect, interactions);
        return;
      }
      if (track.coasting) return;
      if (track.pose !== "pinch") {
        if (mode.kind === "press") {
          interactions.push({ type: "release", side: track.side, outcome: "select", dx: 0, dy: 0 });
        } else {
          const fling = mode.travel >= FLING_MIN_TRAVEL && track.peakSpeed(now, FLING_WINDOW_MS) >= FLING_SPEED;
          interactions.push({ type: "release", side: track.side, outcome: fling ? "fling" : "drop", dx: mode.dx, dy: mode.dy });
        }
        this.mode = IDLE;
        return;
      }
      const dx = track.stable.x - mode.origin.x;
      const dy = track.stable.y - mode.origin.y;
      const travel = Math.hypot(dx * aspect, dy);
      if (mode.kind === "press") {
        if (travel < clampRange(track.scale * DRAG_START_HANDS, DRAG_START_MIN, DRAG_START_MAX)) return;
        this.mode = { kind: "grab", track: track.id, origin: mode.origin, dx, dy, travel };
        interactions.push({ type: "grab", side: track.side, phase: "begin", dx, dy });
        return;
      }
      if (dx === mode.dx && dy === mode.dy) return;
      this.mode = { ...mode, dx, dy, travel: Math.max(mode.travel, travel) };
      interactions.push({ type: "grab", side: track.side, phase: "move", dx, dy });
      return;
    }
    const first = this.track(mode.a);
    const second = this.track(mode.b);
    if (!first || !second || first.pose !== "pinch" || second.pose !== "pinch") {
      interactions.push({ type: "transform", phase: "end", scale: mode.scale, dx: mode.dx, dy: mode.dy });
      for (const track of [first, second]) {
        if (track?.pose === "pinch") this.spent.add(track.id);
      }
      this.mode = IDLE;
      return;
    }
    if (first.coasting || second.coasting) return;
    const ratio = distanceIso(first.stable, second.stable, aspect) / mode.distance;
    const offset = Math.abs(ratio - 1) <= TRANSFORM_DEADBAND ? 0 : ratio - 1 - Math.sign(ratio - 1) * TRANSFORM_DEADBAND;
    const scale = clampRange(1 + offset, TRANSFORM_MIN_SCALE, TRANSFORM_MAX_SCALE);
    const middle = midpoint(first.stable, second.stable);
    const dx = middle.x - mode.origin.x;
    const dy = middle.y - mode.origin.y;
    if (scale === mode.scale && dx === mode.dx && dy === mode.dy) return;
    this.mode = { ...mode, scale, dx, dy };
    interactions.push({ type: "transform", phase: "move", scale, dx, dy });
  }

  private beginTransform(first: HandTrack, second: HandTrack, aspect: number, interactions: GestureInteraction[]): void {
    this.mode = {
      kind: "transform",
      a: first.id,
      b: second.id,
      distance: Math.max(distanceIso(first.stable, second.stable, aspect), 0.02),
      origin: midpoint(first.stable, second.stable),
      scale: 1,
      dx: 0,
      dy: 0,
    };
    interactions.push({ type: "transform", phase: "begin", scale: 1, dx: 0, dy: 0 });
  }

  private engaged(id: number): boolean {
    const mode = this.mode;
    if (this.pointerId === id) return true;
    if (mode.kind === "press" || mode.kind === "grab") return mode.track === id;
    if (mode.kind === "transform") return mode.a === id || mode.b === id;
    return false;
  }

  private track(id: number): HandTrack | undefined {
    return this.tracks.find((track) => track.id === id);
  }
}

class HandTrack {
  readonly id: number;
  side: HandSide;
  current: ParsedHand;
  lastSeen: number;
  coasting = false;
  confirmed = false;
  pose: HandPose = "none";
  stable: Point;
  scale: number;
  pointUntil = 0;
  pointSince = 0;
  aim: Point;
  aimSent: Point;
  hold: { since: number; anchor: Point; fired: boolean } | null = null;
  readonly swipe = new SwipeDetector();
  private vote: number;
  private seen = 0;
  private readonly firstSeen: number;
  private pinched = false;
  private closingSince: number | null = null;
  private openingFrames = 0;
  private releasedAt = Number.NEGATIVE_INFINITY;
  private votes: HandPose[] = [];
  private stabilizer = new PointStabilizer(2.2, 1.2, 0.14, 0.11);
  private aimFilter = new PointStabilizer(0.8, 25, 0.2, 0.15);
  private history: { at: number; x: number; y: number }[] = [];

  constructor(id: number, hand: ParsedHand, now: number) {
    this.id = id;
    this.side = hand.observedSide ?? (hand.screen.x >= 0.5 ? "right" : "left");
    this.vote = this.side === "right" ? 0.75 : 0.25;
    this.current = hand;
    this.lastSeen = now;
    this.firstSeen = now;
    this.stable = hand.screen;
    this.scale = hand.scale;
    this.aim = { x: 0.5, y: 0.5 };
    this.aimSent = this.aim;
  }

  observe(hand: ParsedHand, now: number, options: AirHandOptions): void {
    this.current = hand;
    this.lastSeen = now;
    this.coasting = false;
    this.seen += 1;
    if (this.seen >= TRACK_CONFIRM_FRAMES) this.confirmed = true;
    if (hand.observedSide) {
      this.vote += SIDE_VOTE_RATE * Math.max(hand.score, 0.2) * ((hand.observedSide === "right" ? 1 : 0) - this.vote);
      if (this.vote > SIDE_FLIP_HIGH) this.side = "right";
      else if (this.vote < SIDE_FLIP_LOW) this.side = "left";
    }
    this.scale += (hand.scale - this.scale) * HAND_SCALE_RATE;
    this.stable = this.stabilizer.update(hand.screen, now);
    this.aim = this.aimFilter.update(aimPoint(hand.screen, this.side, options), now);
    this.history.push({ at: now, x: hand.screen.x * options.aspect, y: hand.screen.y });
    while (this.history.length > 2 && now - (this.history[0]?.at ?? now) > HISTORY_MS) this.history.shift();
    const wasPointing = this.pose === "point";
    this.updatePose(hand, now);
    if (this.pose !== "point") return;
    if (!wasPointing) this.pointSince = now;
    this.pointUntil = now + POINT_GRACE_MS;
  }

  engaged(now: number): boolean {
    return now - this.firstSeen >= TRACK_ENGAGE_MS;
  }

  assignSide(side: HandSide): void {
    this.side = side;
    this.vote = side === "right" ? SIDE_FLIP_HIGH : SIDE_FLIP_LOW;
  }

  peakSpeed(end: number, window: number): number {
    const samples = this.history.filter((sample) => sample.at <= end && end - sample.at <= window + SPEED_SPAN_MS);
    let peak = 0;
    for (let index = 1; index < samples.length; index += 1) {
      const latest = samples[index];
      if (!latest) continue;
      for (let earlier = index - 1; earlier >= 0; earlier -= 1) {
        const sample = samples[earlier];
        if (!sample) continue;
        const elapsed = latest.at - sample.at;
        if (elapsed < SPEED_SPAN_MS) continue;
        peak = Math.max(peak, Math.hypot(latest.x - sample.x, latest.y - sample.y) / (elapsed / 1000));
        break;
      }
    }
    return peak;
  }

  view(engaged: boolean): TrackedHand {
    return {
      id: this.id,
      side: this.side,
      pose: this.pose,
      palm: { x: clamp(this.stable.x), y: clamp(this.stable.y) },
      pinch: this.current.indexReach < PINCH_MIN_INDEX_REACH ? 0 : clamp(1 - (this.current.pinchRatio - 0.2) / 0.5),
      curls: this.current.curls,
      confidence: this.current.score,
      coasting: this.coasting,
      engaged,
      aim: { x: this.aim.x, y: this.aim.y },
      landmarks: this.current.image,
    };
  }

  private updatePose(hand: ParsedHand, now: number): void {
    const reaching = hand.indexReach >= PINCH_MIN_INDEX_REACH;
    if (this.pinched) {
      this.openingFrames = !reaching || hand.pinchRatio > PINCH_RELEASE_RATIO ? this.openingFrames + 1 : 0;
      if (this.openingFrames >= PINCH_RELEASE_FRAMES) {
        this.pinched = false;
        this.openingFrames = 0;
        this.releasedAt = now;
      }
    } else {
      const closing = reaching
        && hand.pinchRatio < PINCH_ENTER_RATIO
        && hand.imagePinchRatio < PINCH_IMAGE_ENTER_RATIO
        && now - this.releasedAt >= PINCH_REFRACTORY_MS
        && this.peakSpeed(now, PINCH_SPEED_WINDOW_MS) < PINCH_MAX_HAND_SPEED;
      this.closingSince = closing ? this.closingSince ?? now : null;
      if (this.closingSince != null && now - this.closingSince >= PINCH_ENTER_MS) {
        this.pinched = true;
        this.closingSince = null;
      }
    }
    if (this.pinched) {
      this.pose = "pinch";
      this.votes = [];
      return;
    }
    const shape = classifyShape(hand.curls, this.pose);
    this.votes.push(shape);
    if (this.votes.length > POSE_WINDOW) this.votes.shift();
    if (this.pose === "pinch") this.pose = shape;
    else if (shape !== this.pose && this.votes.filter((vote) => vote === shape).length >= POSE_VOTES) this.pose = shape;
  }
}

function bestAssignment(tracks: readonly HandTrack[], hands: readonly ParsedHand[], aspect: number): Array<[number, number]> {
  let best: Array<[number, number]> = [];
  let bestCost = Number.POSITIVE_INFINITY;
  const pairs: Array<[number, number]> = [];
  const used = new Set<number>();
  const visit = (trackIndex: number, cost: number) => {
    if (trackIndex === tracks.length) {
      const total = cost + (tracks.length + hands.length - pairs.length * 2) * ASSOCIATION_GATE;
      if (total < bestCost) {
        bestCost = total;
        best = pairs.map((pair) => [pair[0], pair[1]]);
      }
      return;
    }
    visit(trackIndex + 1, cost);
    const track = tracks[trackIndex];
    if (!track) return;
    hands.forEach((hand, handIndex) => {
      if (used.has(handIndex)) return;
      const gap = distanceIso(track.current.screen, hand.screen, aspect);
      if (gap > ASSOCIATION_GATE) return;
      const mismatch = hand.observedSide && hand.observedSide !== track.side ? SIDE_MISMATCH_COST : 0;
      used.add(handIndex);
      pairs.push([trackIndex, handIndex]);
      visit(trackIndex + 1, cost + gap + mismatch);
      pairs.pop();
      used.delete(handIndex);
    });
  };
  visit(0, 0);
  return best;
}

function parseObservation(observation: HandObservation, options: AirHandOptions): ParsedHand | null {
  const { image, world } = observation;
  if (image.length < 21 || world.length < 21) return null;
  if (!image.slice(0, 21).every(finiteLandmark) || !world.slice(0, 21).every(finiteLandmark)) return null;
  const control = handControlPoint(image);
  const shape = handShape(world);
  if (!shape) return null;
  const wrist = image[0];
  const middleMcp = image[9];
  const indexMcp = image[5];
  const littleMcp = image[17];
  const thumbTip = image[4];
  const indexTip = image[8];
  if (!wrist || !middleMcp || !indexMcp || !littleMcp || !thumbTip || !indexTip) return null;
  const aspect = options.aspect;
  const length = Math.hypot((wrist.x - middleMcp.x) * aspect, wrist.y - middleMcp.y);
  const width = Math.hypot((indexMcp.x - littleMcp.x) * aspect, indexMcp.y - littleMcp.y);
  const scale = Math.max(length, width * 1.15, 0.02);
  return {
    image,
    observedSide: observedSide(observation.label, options.mirrorX),
    score: clamp(observation.score),
    screen: { x: options.mirrorX ? 1 - control.x : control.x, y: control.y },
    scale,
    palmWidth: Math.max(distance(indexMcp, littleMcp), 0.04),
    curls: shape.curls,
    pinchRatio: shape.pinchRatio,
    imagePinchRatio: Math.hypot((thumbTip.x - indexTip.x) * aspect, thumbTip.y - indexTip.y) / scale,
    indexReach: shape.indexReach,
  };
}

function observedSide(label: string, mirrorX: boolean): HandSide | null {
  const normalized = label.trim().toLowerCase();
  if (normalized !== "left" && normalized !== "right") return null;
  if (!mirrorX) return normalized;
  return normalized === "left" ? "right" : "left";
}

function handShape(world: readonly HandLandmark[]): { curls: FingerCurls; pinchRatio: number; indexReach: number } | null {
  const wrist = world[0];
  const middleMcp = world[9];
  const thumbTip = world[4];
  const indexTip = world[8];
  const indexMcp = world[5];
  if (!wrist || !middleMcp || !thumbTip || !indexTip || !indexMcp) return null;
  const length = distance3(wrist, middleMcp);
  if (length < 1e-4) return null;
  return {
    curls: {
      thumb: thumbCurl(world, length),
      index: fingerCurl(world, 5, 6, 7, 8),
      middle: fingerCurl(world, 9, 10, 11, 12),
      ring: fingerCurl(world, 13, 14, 15, 16),
      little: fingerCurl(world, 17, 18, 19, 20),
    },
    pinchRatio: distance3(thumbTip, indexTip) / length,
    indexReach: distance3(wrist, indexTip) / Math.max(distance3(wrist, indexMcp), 1e-4),
  };
}

function classifyShape(curls: FingerCurls, previous: HandPose): HandPose {
  const sticky = previous !== "none" && previous !== "pinch";
  const extended = (value: number) => value < (sticky ? 0.48 : 0.4);
  const curled = (value: number) => value > (sticky ? 0.5 : 0.58);
  const { index, middle, ring, little } = curls;
  if (curled(index) && curled(middle) && curled(ring) && curled(little)) return "fist";
  if (extended(index) && curled(middle) && curled(ring) && curled(little)) return "point";
  if (extended(index) && extended(middle) && curled(ring) && curled(little)) return "two";
  if (extended(index) && extended(middle) && !curled(ring) && !curled(little) && (extended(ring) || extended(little))) return "open";
  return "none";
}

function fingerCurl(world: readonly HandLandmark[], mcp: number, pip: number, dip: number, tip: number): number {
  const wrist = world[0];
  const base = world[mcp];
  const middle = world[pip];
  const upper = world[dip];
  const end = world[tip];
  if (!wrist || !base || !middle || !upper || !end) return 0;
  const bend = bendDegrees(wrist, base, middle) * 0.5 + bendDegrees(base, middle, upper) + bendDegrees(middle, upper, end);
  return clamp((bend - 25) / 175);
}

function thumbCurl(world: readonly HandLandmark[], length: number): number {
  const base = world[1];
  const joint = world[2];
  const upper = world[3];
  const tip = world[4];
  const indexMcp = world[5];
  if (!base || !joint || !upper || !tip || !indexMcp) return 0;
  const bend = bendDegrees(base, joint, upper) + bendDegrees(joint, upper, tip);
  const tucked = 1 - clamp((distance3(tip, indexMcp) / length - 0.35) / 0.6);
  return clamp(tucked * 0.55 + clamp((bend - 20) / 110) * 0.45);
}

const SWIPE_WINDOW_MS = 400;
const SWIPE_STALE_MS = 250;
const SWIPE_POSE_GRACE_MS = 200;
const SWIPE_LOST_GRACE_MS = 220;
const SWIPE_MIN_TRAVEL = 0.1;
const SWIPE_HAND_WIDTHS = 2.5;
const SWIPE_DOMINANCE = 1.8;
const SWIPE_STRAIGHTNESS = 0.8;
const SWIPE_SAME_DIRECTION_MS = 380;
const SWIPE_RETURN_SUPPRESS_MS = 750;

type SwipeSample = { at: number; x: number; y: number };

class SwipeDetector {
  private samples: SwipeSample[] = [];
  private ineligibleSince: number | null = null;
  private lostAt: number | null = null;
  private lastFired: { at: number; direction: -1 | 1 } | null = null;
  private threshold = 0;

  reset(): void {
    this.samples = [];
    this.ineligibleSince = null;
    this.lostAt = null;
  }

  update(position: Point, now: number, eligible: boolean, threshold: number, speed: number): -1 | 0 | 1 {
    this.lostAt = null;
    this.threshold = threshold;
    if (!eligible) {
      this.ineligibleSince ??= now;
      if (now - this.ineligibleSince > SWIPE_POSE_GRACE_MS) this.samples = [];
      return 0;
    }
    this.ineligibleSince = null;
    const latest = this.samples[this.samples.length - 1];
    if (latest && now - latest.at > SWIPE_STALE_MS) this.samples = [];
    this.samples.push({ at: now, x: position.x, y: position.y });
    while (this.samples.length > 2 && now - (this.samples[0]?.at ?? now) > SWIPE_WINDOW_MS) this.samples.shift();
    return this.evaluate(now, threshold, speed);
  }

  lost(now: number, threshold: number, speed: number): -1 | 0 | 1 {
    if (this.samples.length === 0) return 0;
    if (this.lostAt == null) {
      this.lostAt = now;
      return this.evaluate(now, Math.min(threshold, this.threshold || threshold), speed);
    }
    if (now - this.lostAt > SWIPE_LOST_GRACE_MS) this.samples = [];
    return 0;
  }

  private evaluate(now: number, threshold: number, speed: number): -1 | 0 | 1 {
    const last = this.samples[this.samples.length - 1];
    if (!last || this.samples.length < 2) return 0;
    for (let index = 0; index < this.samples.length - 1; index += 1) {
      const start = this.samples[index];
      if (!start) continue;
      const dx = last.x - start.x;
      const dy = last.y - start.y;
      if (Math.abs(dx) < threshold || Math.abs(dx) < Math.abs(dy) * SWIPE_DOMINANCE) continue;
      const elapsed = Math.max((last.at - start.at) / 1000, 1 / 60);
      if (Math.abs(dx) / elapsed < speed) continue;
      let path = 0;
      for (let step = index + 1; step < this.samples.length; step += 1) {
        path += Math.abs((this.samples[step]?.x ?? 0) - (this.samples[step - 1]?.x ?? 0));
      }
      if (Math.abs(dx) < path * SWIPE_STRAIGHTNESS) continue;
      const direction: -1 | 1 = dx > 0 ? 1 : -1;
      this.samples = [last];
      const previous = this.lastFired;
      if (previous) {
        const since = now - previous.at;
        if (since < (previous.direction === direction ? SWIPE_SAME_DIRECTION_MS : SWIPE_RETURN_SUPPRESS_MS)) return 0;
      }
      this.lastFired = { at: now, direction };
      return direction;
    }
    return 0;
  }
}

function swipeCommand(direction: -1 | 0 | 1): AirHandGestureId | null {
  if (direction === 0) return null;
  return direction > 0 ? "swipe_right" : "swipe_left";
}

class PointStabilizer {
  private lastRaw: Point | null = null;
  private lastUpdateAt: number | null = null;
  private jumpCandidate: Point | null = null;
  private jumpCandidateCount = 0;
  private output: Point | null = null;
  private xFilter: OneEuroFilter;
  private yFilter: OneEuroFilter;

  constructor(minCutoff: number, beta: number, private readonly jump: number, private readonly settle: number) {
    this.xFilter = new OneEuroFilter(minCutoff, beta);
    this.yFilter = new OneEuroFilter(minCutoff, beta);
  }

  update(raw: Point, now: number): Point {
    if (!this.output || !this.lastRaw) {
      this.output = raw;
      this.lastRaw = raw;
      this.lastUpdateAt = now;
      this.xFilter.prime(raw.x);
      this.yFilter.prime(raw.y);
      return raw;
    }
    const previous = this.output;
    const dt = Math.min(Math.max((now - (this.lastUpdateAt ?? now)) / 1000, 1 / 120), 1 / 15);
    this.lastUpdateAt = now;
    const rawDelta = distance(raw, this.lastRaw);
    const distanceFromOutput = distance(raw, previous);
    const rawJumpThreshold = Math.max(dt * 2.8, this.jump);
    if (this.jumpCandidate && distance(this.jumpCandidate, raw) < 0.045 && distanceFromOutput > this.settle) {
      this.jumpCandidateCount += 1;
      this.lastRaw = raw;
      if (this.jumpCandidateCount < 4) return previous;
      this.jumpCandidate = null;
      this.jumpCandidateCount = 0;
      this.xFilter.prime(raw.x);
      this.yFilter.prime(raw.y);
      this.output = raw;
      return raw;
    }
    if (rawDelta > rawJumpThreshold && distanceFromOutput > this.settle) {
      this.jumpCandidate = raw;
      this.jumpCandidateCount = 1;
      this.lastRaw = raw;
      return previous;
    }
    this.jumpCandidate = null;
    this.jumpCandidateCount = 0;
    this.lastRaw = raw;
    this.output = {
      x: this.xFilter.filter(raw.x, dt),
      y: this.yFilter.filter(raw.y, dt),
    };
    return this.output;
  }
}

class OneEuroFilter {
  private initialized = false;
  private filteredValue = 0;
  private filteredDerivative = 0;

  constructor(private readonly minCutoff: number, private readonly beta: number, private readonly derivativeCutoff = 1) {}

  prime(value: number): void {
    this.initialized = true;
    this.filteredValue = value;
    this.filteredDerivative = 0;
  }

  filter(value: number, dt: number): number {
    const elapsed = Math.max(dt, 1 / 1000);
    if (!this.initialized) {
      this.prime(value);
      return value;
    }
    const derivative = (value - this.filteredValue) / elapsed;
    this.filteredDerivative = lowPass(derivative, this.filteredDerivative, this.derivativeCutoff, elapsed);
    const cutoff = this.minCutoff + this.beta * Math.abs(this.filteredDerivative);
    this.filteredValue = lowPass(value, this.filteredValue, cutoff, elapsed);
    return this.filteredValue;
  }
}

function lowPass(value: number, previous: number, cutoff: number, dt: number): number {
  const tau = 1 / (2 * Math.PI * Math.max(cutoff, 0.0001));
  const alpha = 1 / (1 + tau / dt);
  return previous + alpha * (value - previous);
}

function handControlPoint(landmarks: readonly HandLandmark[]): Point {
  const wrist = landmarks[0] as HandLandmark;
  const indexMcp = landmarks[5] as HandLandmark;
  const middleMcp = landmarks[9] as HandLandmark;
  const ringMcp = landmarks[13] as HandLandmark;
  const littleMcp = landmarks[17] as HandLandmark;
  const knuckleCenter = {
    x: (indexMcp.x + middleMcp.x + ringMcp.x + littleMcp.x) / 4,
    y: (indexMcp.y + middleMcp.y + ringMcp.y + littleMcp.y) / 4,
  };
  const axisPoint = {
    x: wrist.x + (middleMcp.x - wrist.x) * 0.62,
    y: wrist.y + (middleMcp.y - wrist.y) * 0.62,
  };
  const palmControlPoint = {
    x: axisPoint.x * 0.72 + knuckleCenter.x * 0.28,
    y: axisPoint.y * 0.72 + knuckleCenter.y * 0.28,
  };
  return {
    x: indexMcp.x * 0.45 + palmControlPoint.x * 0.55,
    y: indexMcp.y * 0.45 + palmControlPoint.y * 0.55,
  };
}

function aimPoint(screen: Point, side: HandSide, options: AirHandOptions): Point {
  const size = AIM_SIZE[options.sensitivity];
  const center = 0.5 + ((side === "right") === options.mirrorX ? AIM_SIDE_SHIFT : -AIM_SIDE_SHIFT);
  return {
    x: clamp((screen.x - center) / size.width + 0.5),
    y: clamp((screen.y - AIM_CENTER_Y) / size.height + 0.5),
  };
}

function onEdge(aim: Point): boolean {
  return aim.x <= AIM_EDGE || aim.x >= 1 - AIM_EDGE || aim.y <= AIM_EDGE || aim.y >= 1 - AIM_EDGE;
}

function holdDuration(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 220;
  if (sensitivity === "deliberate") return 460;
  return 320;
}

function swipeThreshold(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 0.14;
  if (sensitivity === "deliberate") return 0.24;
  return 0.18;
}

function swipeSpeed(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 0.65;
  if (sensitivity === "deliberate") return 1.05;
  return 0.85;
}

function finiteLandmark(landmark: HandLandmark | undefined): boolean {
  return Boolean(landmark && Number.isFinite(landmark.x) && Number.isFinite(landmark.y) && Number.isFinite(landmark.z));
}

function isoPoint(point: Point, aspect: number): Point {
  return { x: point.x * aspect, y: point.y };
}

function distanceIso(left: Point, right: Point, aspect: number): number {
  return Math.hypot((left.x - right.x) * aspect, left.y - right.y);
}

function midpoint(left: Point, right: Point): Point {
  return { x: (left.x + right.x) / 2, y: (left.y + right.y) / 2 };
}

function distance(left: Point, right: Point): number {
  return Math.hypot(left.x - right.x, left.y - right.y);
}

function distance3(left: HandLandmark, right: HandLandmark): number {
  return Math.hypot(left.x - right.x, left.y - right.y, left.z - right.z);
}

function bendDegrees(start: HandLandmark, joint: HandLandmark, end: HandLandmark): number {
  const ux = joint.x - start.x;
  const uy = joint.y - start.y;
  const uz = joint.z - start.z;
  const vx = end.x - joint.x;
  const vy = end.y - joint.y;
  const vz = end.z - joint.z;
  const denominator = Math.hypot(ux, uy, uz) * Math.hypot(vx, vy, vz);
  if (denominator < 1e-12) return 0;
  return Math.acos(clampRange((ux * vx + uy * vy + uz * vz) / denominator, -1, 1)) * 180 / Math.PI;
}

function clamp(value: number): number {
  return clampRange(value, 0, 1);
}

function clampRange(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}
