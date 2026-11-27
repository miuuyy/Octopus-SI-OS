import { useState } from "react";
import { motion, type TargetAndTransition, type Transition } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";

export type ComposerPrimaryActionState = "voice" | "send" | "stop";

type Point = readonly [number, number];

type MorphPreset = {
  rest: TargetAndTransition;
  transform: TargetAndTransition;
  transformTransition: Transition;
  pathTransition: Transition;
};

const EASE_STANDARD: [number, number, number, number] = [0.4, 0, 0.2, 1];
const REST_TRANSFORM = { rotate: 0, y: 0, scale: 1 };

const VOICE_TRANSFORM_TRANSITION: Transition = {
  duration: 0.52,
  ease: EASE_STANDARD,
  times: [0, 0.22, 0.72, 1],
};

const STOP_TRANSFORM_TRANSITION: Transition = {
  duration: 0.48,
  ease: EASE_STANDARD,
  times: [0, 0.22, 0.78, 1],
  rotate: { duration: 0.48, ease: EASE_STANDARD, times: [0, 0.78, 1] },
};

const STOP_PATH_TRANSITION: Transition = {
  duration: 0.34,
  delay: 0.055,
  ease: EASE_STANDARD,
};

const MORPH_PRESETS: Record<"toVoice" | "voiceToSend" | "toStop" | "stopToSend", MorphPreset> = {
  toVoice: {
    rest: REST_TRANSFORM,
    transform: {
      rotate: [null, 3, -1, 0],
      y: [null, -0.5, 0.4, 0],
      scale: [null, 0.9, 1.04, 1],
    },
    transformTransition: VOICE_TRANSFORM_TRANSITION,
    pathTransition: { ...VOICE_TRANSFORM_TRANSITION, times: [0, 0.43, 0.67, 1] },
  },
  voiceToSend: {
    rest: REST_TRANSFORM,
    transform: {
      rotate: [null, -3, 1, 0],
      y: [null, 0.5, -1.4, 0],
      scale: [null, 0.88, 1.055, 1],
    },
    transformTransition: VOICE_TRANSFORM_TRANSITION,
    pathTransition: { ...VOICE_TRANSFORM_TRANSITION, times: [0, 0.21, 0.43, 1] },
  },
  toStop: {
    rest: { ...REST_TRANSFORM, rotate: 90 },
    transform: {
      rotate: [null, 96, 90],
      y: [null, -2.5, 0.2, 0],
      scale: [null, 1.07, 0.96, 1],
    },
    transformTransition: STOP_TRANSFORM_TRANSITION,
    pathTransition: STOP_PATH_TRANSITION,
  },
  stopToSend: {
    rest: REST_TRANSFORM,
    transform: {
      rotate: [null, -6, 0],
      y: [null, 0.5, -0.8, 0],
      scale: [null, 0.94, 1.03, 1],
    },
    transformTransition: STOP_TRANSFORM_TRANSITION,
    pathTransition: STOP_PATH_TRANSITION,
  },
};

function lerpPoint(a: Point, b: Point, t: number): Point {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
}

function cornerArc(prev: Point, vertex: Point, next: Point, radius: number) {
  const before = lerpPoint(
    vertex,
    prev,
    radius === 0 ? 0 : Math.min(0.5, radius / Math.hypot(prev[0] - vertex[0], prev[1] - vertex[1])),
  );
  const after = lerpPoint(
    vertex,
    next,
    radius === 0 ? 0 : Math.min(0.5, radius / Math.hypot(next[0] - vertex[0], next[1] - vertex[1])),
  );
  return { before, after, vertex };
}

function roundedQuadPath([p0, p1, p2, p3]: readonly [Point, Point, Point, Point], radius: number): string {
  const corners = [
    cornerArc(p3, p0, p1, radius),
    cornerArc(p0, p1, p2, radius),
    cornerArc(p1, p2, p3, radius),
    cornerArc(p2, p3, p0, radius),
  ];
  const start = corners[3].after;
  const segments = corners.flatMap(({ before, after, vertex }) => [
    [before, before, before],
    [lerpPoint(before, vertex, 0.552285), lerpPoint(after, vertex, 0.552285), after],
  ]);
  return `M${start.map((n) => n.toFixed(4)).join(" ")} ${segments
    .map((seg) => `C${seg.flat().map((n) => n.toFixed(4)).join(" ")}`)
    .join(" ")} Z`;
}

function roundedRectPath(x: number, y: number, w: number, h: number, r: number): string {
  return roundedQuadPath(
    [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
    r,
  );
}

function capsulePath(a: Point, b: Point, thickness: number): string {
  const len = Math.hypot(b[0] - a[0], b[1] - a[1]);
  const nx = ((b[1] - a[1]) / len) * (thickness / 2);
  const ny = ((a[0] - b[0]) / len) * (thickness / 2);
  const tx = -ny;
  const ty = nx;
  return roundedQuadPath(
    [
      [a[0] - nx - tx, a[1] - ny - ty],
      [a[0] + nx - tx, a[1] + ny - ty],
      [b[0] + nx + tx, b[1] + ny + ty],
      [b[0] - nx + tx, b[1] - ny + ty],
    ],
    thickness / 2,
  );
}

function voiceBars(scale: number, width: number, bars: readonly (readonly [number, number, number])[]): readonly string[] {
  return bars.map(([x, y1, y2]) => capsulePath([x * scale, y1 * scale], [x * scale, y2 * scale], width * scale));
}

const COLLAPSED_PATH = roundedRectPath(10, 10, 0, 0, 0);

const STATIC_STATE_PATHS = {
  send: [
    capsulePath([10, 4.5], [10, 16.667], 1.33),
    capsulePath([4.167, 9.167], [10, 3.333], 1.33),
    capsulePath([10, 3.333], [15.833, 9.167], 1.33),
    COLLAPSED_PATH,
  ],
  stop: [roundedRectPath(4.5, 4.5, 11, 11, 1.25), COLLAPSED_PATH, COLLAPSED_PATH, COLLAPSED_PATH],
} as const;

const VOICE_PATHS = {
  16: voiceBars(1.25, 1.5, [
    [6.44434, 2.66675, 13.3337],
    [3, 9.83374, 6.16675],
    [9.88867, 4.49976, 11.1667],
    [13.334, 6.66675, 9.33374],
  ]),
  20: voiceBars(1, 1.75, [
    [8.22266, 3.33325, 16.6663],
    [4, 12.1663, 7.83325],
    [12.4443, 5.50024, 13.8333],
    [16.667, 8.33325, 11.6663],
  ]),
} as const;

const WAYPOINT_SEND_FOLDED = [
  capsulePath([10, 3.333], [10, 16.667], 1.33),
  capsulePath([10, 9.167], [10, 3.333], 1.33),
  capsulePath([10, 3.333], [10, 9.167], 1.33),
  COLLAPSED_PATH,
] as const;

const WAYPOINT_VOICE_BUNDLE = voiceBars(1, 1.5, [
  [10, 4.25, 15.75],
  [7, 12.5, 7.5],
  [11.9, 6.5, 13.5],
  [13.6, 8.5, 11.5],
]);

export function ComposerPrimaryActionIcon({
  className,
  state,
  opticalSize = 16,
}: {
  className?: string;
  state: ComposerPrimaryActionState;
  opticalSize?: 16 | 20;
}) {
  const reduceMotion = useShouldReduceMotion();
  const [transition, setTransition] = useState<{
    from: ComposerPrimaryActionState;
    to: ComposerPrimaryActionState;
  }>(() => ({ from: state, to: state }));

  if (transition.to !== state) {
    setTransition({ from: transition.to, to: state });
  }

  const isVoiceTransition = state === "voice" || (state === "send" && transition.from === "voice");
  const targetPaths = state === "voice" ? VOICE_PATHS[opticalSize] : STATIC_STATE_PATHS[state];

  const preset =
    state === "voice"
      ? MORPH_PRESETS.toVoice
      : isVoiceTransition
        ? MORPH_PRESETS.voiceToSend
        : state === "stop"
          ? MORPH_PRESETS.toStop
          : MORPH_PRESETS.stopToSend;

  return (
    <motion.svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      focusable="false"
      initial={false}
      animate={reduceMotion ? preset.rest : preset.transform}
      transition={reduceMotion ? { duration: 0 } : preset.transformTransition}
      viewBox="0 0 20 20"
    >
      {targetPaths.map((targetD, index) => {
        const d =
          !reduceMotion && isVoiceTransition
            ? state === "voice"
              ? [null, WAYPOINT_SEND_FOLDED[index], WAYPOINT_VOICE_BUNDLE[index], targetD]
              : [null, WAYPOINT_VOICE_BUNDLE[index], WAYPOINT_SEND_FOLDED[index], targetD]
            : targetD;
        return (
          <motion.path
            key={index}
            initial={false}
            animate={{ d }}
            transition={reduceMotion ? { duration: 0 } : preset.pathTransition}
          />
        );
      })}
    </motion.svg>
  );
}
