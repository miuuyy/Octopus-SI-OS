import { useEffect, useRef, useState, type RefObject } from "react";
import { useShouldReduceMotion } from "../../appearance";
import { LIGHT_SCENE_PARTICLE_COLOR } from "../../design-system/sceneParticlePalette";
import { SCENE_RIPPLE_IGNORED_TARGETS } from "./sceneRipple";
import type { VoicePhase } from "./useVoiceSession";

export type VoiceOrbActivity =
  | { source: "preview" }
  | { source: "live"; listeningLevel: number; speakingLevel: number; thinkingLevel: number };

type Props = {
  activity: VoiceOrbActivity;
  ambient?: boolean;
  animated?: boolean;
  onDispersed?: () => void;
  formationSurface?: RefObject<HTMLElement | null>;
  phase: VoicePhase;
  quietField?: boolean;
  sceneSurface?: RefObject<HTMLElement | null>;
  rippleSurface?: RefObject<HTMLElement | null>;
  sizeProbe?: RefObject<HTMLElement | null>;
  sphere?: number;
};

type OrbFrame = {
  activity: VoiceOrbActivity;
  ambient: boolean;
  phase: VoicePhase;
  quietField: boolean;
  sphere: number;
};

type ResolvedActivity = {
  speaking: number;
  thinking: number;
  voice: number;
};

type ShaderUniforms = {
  ambient: WebGLUniformLocation;
  background: WebGLUniformLocation;
  departing: WebGLUniformLocation;
  quietField: WebGLUniformLocation;
  scene: WebGLUniformLocation;
  orbCenter: WebGLUniformLocation;
  error: WebGLUniformLocation;
  formTiming: WebGLUniformLocation;
  forms: WebGLUniformLocation;
  pixelRatio: WebGLUniformLocation;
  reveal: WebGLUniformLocation;
  resolution: WebGLUniformLocation;
  ripples: WebGLUniformLocation;
  speaking: WebGLUniformLocation;
  speechTime: WebGLUniformLocation;
  sphere: WebGLUniformLocation;
  thinking: WebGLUniformLocation;
  thoughtTime: WebGLUniformLocation;
  time: WebGLUniformLocation;
  voice: WebGLUniformLocation;
};

const REVEAL_SPAN = 1.8;
const REVEAL_RISE = 1.75;
const REVEAL_DECAY = 3.2;
const AMBIENT_GATE = 0.3;
const MAX_RENDER_PIXEL_RATIO = 2;
const FRAME_INTERVAL_MS = 1000 / 30;
const ACTIVE_FRAME_INTERVAL_MS = 1000 / 60;
const RIPPLE_COUNT = 6;
const RIPPLE_LIFE_SECONDS = 1.8;
const FORM_COUNT = 4;
const FORM_LIFE_SECONDS = 0.9;

const VERTEX_SHADER = `#version 300 es
precision highp float;

void main() {
  vec2 position;
  if (gl_VertexID == 0) {
    position = vec2(-1.0, -1.0);
  } else if (gl_VertexID == 1) {
    position = vec2(3.0, -1.0);
  } else {
    position = vec2(-1.0, 3.0);
  }
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const FRAGMENT_SHADER = `#version 300 es
precision highp float;

uniform vec2 u_resolution;
uniform vec4 u_scene;
uniform vec2 u_orb_center;
uniform float u_pixel_ratio;
uniform float u_time;
uniform float u_thought_time;
uniform float u_thinking;
uniform float u_speaking;
uniform float u_voice;
uniform float u_speech_time;
uniform float u_error;
uniform float u_reveal;
uniform float u_ambient;
uniform float u_departing;
uniform float u_quiet_field;
uniform vec3 u_background;
uniform vec4 u_ripples[${RIPPLE_COUNT}];
uniform vec4 u_forms[${FORM_COUNT}];
uniform vec4 u_form_timing[${FORM_COUNT}];

out vec4 outputColor;

const float PI = 3.141592653589793;
uniform float u_sphere;
#define SPHERE_RADIUS u_sphere

float saturate(float value) {
  return clamp(value, 0.0, 1.0);
}

float interleavedNoise(vec2 pixel) {
  return fract(52.9829189 * fract(dot(pixel, vec2(0.06711056, 0.00583715))));
}

float softLine(float distanceToLine, float halfWidth, float softness) {
  return 1.0 - smoothstep(halfWidth, halfWidth + softness, abs(distanceToLine));
}

float hash21(vec2 p) {
  return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}

float valueNoise(vec2 p) {
  vec2 cellIndex = floor(p);
  vec2 cellFraction = fract(p);
  vec2 smoothing = cellFraction * cellFraction * (3.0 - 2.0 * cellFraction);
  float a = hash21(cellIndex);
  float b = hash21(cellIndex + vec2(1.0, 0.0));
  float c = hash21(cellIndex + vec2(0.0, 1.0));
  float d = hash21(cellIndex + vec2(1.0, 1.0));
  return mix(mix(a, b, smoothing.x), mix(c, d, smoothing.x), smoothing.y);
}

float fbm(vec2 p) {
  float total = 0.0;
  float amplitude = 0.52;
  for (int octave = 0; octave < 4; octave += 1) {
    total += valueNoise(p) * amplitude;
    p = p * 2.03 + vec2(17.31, 9.17);
    amplitude *= 0.5;
  }
  return total;
}

const int GLYPH_COUNT = 57;
const ivec2 GLYPHS[GLYPH_COUNT] = ivec2[GLYPH_COUNT](
  ivec2(476789, 26158),
  ivec2(143492, 4238),
  ivec2(476194, 4383),
  ivec2(1017986, 1582),
  ivec2(72018, 31810),
  ivec2(1033153, 1582),
  ivec2(205342, 17966),
  ivec2(1016900, 8456),
  ivec2(476718, 17966),
  ivec2(476719, 1100),
  ivec2(476721, 32305),
  ivec2(1001022, 17982),
  ivec2(476688, 16942),
  ivec2(936497, 18012),
  ivec2(1032734, 16927),
  ivec2(1032734, 16912),
  ivec2(476695, 17967),
  ivec2(575039, 17969),
  ivec2(231490, 2636),
  ivec2(576152, 21073),
  ivec2(541200, 16927),
  ivec2(585397, 17969),
  ivec2(575285, 20017),
  ivec2(1001022, 16912),
  ivec2(476721, 22093),
  ivec2(1001022, 21073),
  ivec2(508430, 1086),
  ivec2(1020036, 4228),
  ivec2(575025, 17966),
  ivec2(575025, 17732),
  ivec2(575029, 22186),
  ivec2(574788, 10801),
  ivec2(575018, 4228),
  ivec2(1016900, 8735),
  ivec2(449, 15919),
  ivec2(541401, 17982),
  ivec2(464, 16942),
  ivec2(34227, 17967),
  ivec2(465, 32270),
  ivec2(206108, 8456),
  ivec2(15921, 15406),
  ivec2(541401, 17969),
  ivec2(65730, 2636),
  ivec2(541268, 25234),
  ivec2(853, 22065),
  ivec2(729, 17969),
  ivec2(977, 31248),
  ivec2(435, 15393),
  ivec2(729, 16912),
  ivec2(464, 14398),
  ivec2(271240, 8486),
  ivec2(561, 18029),
  ivec2(561, 17732),
  ivec2(561, 22186),
  ivec2(554, 4433),
  ivec2(561, 15406),
  ivec2(994, 4383)
);
const vec3 LIGHT_PARTICLE_INK = vec3(${LIGHT_SCENE_PARTICLE_COLOR.join(", ")}) * vec3(0.62, 0.72, 0.94);

void rippleAt(vec2 pixel, float shortestSide, out float wake, out float crest) {
  wake = 0.0;
  crest = 0.0;
  for (int index = 0; index < ${RIPPLE_COUNT}; index += 1) {
    vec4 ripple = u_ripples[index];
    float age = u_time - ripple.z;
    if (ripple.w <= 0.0 || age < 0.0 || age > ${RIPPLE_LIFE_SECONDS.toFixed(2)}) continue;
    vec2 farthestCorner = max(ripple.xy, u_resolution - ripple.xy);
    float field = length(farthestCorner) / shortestSide;
    float travelled = (1.0 - exp(-age * (1.4 / 1.5))) * field * 1.08;
    float distanceFromSource = length(pixel - ripple.xy) / shortestSide;
    float behind = travelled - distanceFromSource;
    float leadingEdge = smoothstep(-0.014, 0.0, behind);
    float life = 1.0 - smoothstep(${(RIPPLE_LIFE_SECONDS * 0.35).toFixed(2)}, ${RIPPLE_LIFE_SECONDS.toFixed(2)}, age);
    float reach = 1.0 - smoothstep(field * 0.8, field * 1.04, distanceFromSource);
    float strength = leadingEdge * life * reach * ripple.w;
    wake += exp(-max(behind, 0.0) / 0.16) * strength;
    crest += exp(-abs(behind) / 0.024) * strength;
  }
  wake = saturate(wake);
  crest = saturate(crest);
}

float roundedBoxDistance(vec2 point, vec2 halfSize, float radius) {
  vec2 corner = abs(point) - halfSize + radius;
  return length(max(corner, 0.0)) + min(max(corner.x, corner.y), 0.0) - radius;
}

float formationAt(vec2 pixel, float shortestSide, float pixelRatio) {
  float total = 0.0;
  for (int index = 0; index < ${FORM_COUNT}; index += 1) {
    vec4 box = u_forms[index];
    vec4 timing = u_form_timing[index];
    float age = u_time - timing.x;
    if (box.z <= box.x || age < 0.0 || age > ${FORM_LIFE_SECONDS.toFixed(2)}) continue;
    vec2 halfSize = (box.zw - box.xy) * 0.5;
    float boxDistance = roundedBoxDistance(pixel - (box.xy + box.zw) * 0.5, halfSize, min(timing.y, min(halfSize.x, halfSize.y)));
    float inside = 1.0 - smoothstep(-1.5 * pixelRatio, pixelRatio, boxDistance);
    float rim = exp(-abs(boxDistance) / (1.6 * pixelRatio));
    float islands = valueNoise(pixel / (13.0 * pixelRatio) + vec2(float(index) * 7.3, 3.1));
    float appear = smoothstep(0.0, 0.32, age - islands * 0.18);
    float settle = smoothstep(0.26, 0.46, age);
    float dissolve = 1.0 - smoothstep(0.42, ${FORM_LIFE_SECONDS.toFixed(2)}, age + (islands - 0.5) * 0.2);
    float halo = exp(-max(boxDistance, 0.0) / (22.0 * pixelRatio)) * (1.0 - inside) * sin(PI * clamp(age / 0.7, 0.0, 1.0));
    total += max(max(inside * appear * mix(0.62, 0.78, settle), rim * settle), halo * 0.3) * dissolve;
  }
  return saturate(total);
}

float glyphBit(ivec2 glyph, int column, int rowFromTop) {
  if (column < 0 || column > 4 || rowFromTop < 0 || rowFromTop > 6) return 0.0;
  int packedRow = rowFromTop < 4 ? glyph.x >> (5 * (3 - rowFromTop)) : glyph.y >> (5 * (6 - rowFromTop));
  return float((packedRow >> (4 - column)) & 1);
}

float waveCrest(float x, float layer, float thoughtTime) {
  float movement = thoughtTime * 0.7;
  if (layer < 0.5) {
    return
      0.2 +
      cos(x * 2.08 + movement * 0.34) * 0.205 +
      sin(x * 4.18 - movement * 0.21 + 0.7) * 0.022;
  }
  if (layer < 1.5) {
    return
      -0.015 +
      sin(x * 2.7 - movement * 0.72 + 2.82) * 0.235 +
      sin(x * 1.18 + movement * 0.24 + 0.35) * 0.045;
  }
  if (layer < 2.5) {
    return
      -0.19 +
      sin(x * 2.92 + movement * 0.58 + 0.24) * 0.225 +
      sin(x * 1.36 - movement * 0.27 + 2.0) * 0.04;
  }
  return
    -0.35 +
    sin(x * 2.34 - movement * 0.42 + 3.04) * 0.15 +
    cos(x * 1.17 + movement * 0.19) * 0.026;
}

float waveFieldDepth(float x, float layer, float thoughtTime) {
  float drift = thoughtTime * 0.32;
  if (layer < 0.5) return 0.19 + sin(x * 1.72 + drift) * 0.021;
  if (layer < 1.5) return 0.225 + sin(x * 1.48 - drift * 0.7 + 1.2) * 0.027;
  if (layer < 2.5) return 0.21 + cos(x * 1.63 + drift * 0.82 + 0.4) * 0.024;
  return 0.17 + sin(x * 1.38 - drift * 0.6 + 2.1) * 0.018;
}

vec4 shadeWaveField(
  vec2 point,
  float layer,
  float strength,
  float thoughtTime,
  float thinking,
  float speaking,
  vec3 lightTint,
  float antialias
) {
  vec2 normalizedPoint = point / SPHERE_RADIUS;
  float normalizedAA = max(antialias / SPHERE_RADIUS, 0.0025);
  float waveActivity = max(thinking, speaking);
  float thoughtPresence = smoothstep(0.0, 0.16, waveActivity);
  float thoughtActivity = smoothstep(0.08, 0.72, waveActivity);
  float speechAmplitude = 1.0 + saturate(speaking) * (saturate(u_voice) * 0.26 - 0.16);
  float restingCrest = -0.045 + sin(normalizedPoint.x * 2.18 + 0.62) * 0.052;
  float activeCrest = waveCrest(normalizedPoint.x, layer, thoughtTime) * speechAmplitude;
  float crest = mix(restingCrest, activeCrest, thoughtActivity);
  float distanceBelowCrest = crest - normalizedPoint.y;
  float fieldDepth = mix(
    0.13,
    waveFieldDepth(normalizedPoint.x, layer, thoughtTime),
    thoughtActivity
  );
  float line = softLine(
    normalizedPoint.y - crest,
    normalizedAA * 0.14,
    normalizedAA * 0.74
  );
  float lineGlow = exp(-abs(normalizedPoint.y - crest) / 0.027);
  float belowCrest = smoothstep(-normalizedAA * 0.75, normalizedAA * 1.4, distanceBelowCrest);
  float normalizedFieldDepth = max(0.0, distanceBelowCrest) / max(fieldDepth, 0.001);
  float fieldEnvelope = belowCrest * (1.0 - smoothstep(0.48, 1.16, normalizedFieldDepth));
  float fieldFalloff = exp(-normalizedFieldDepth * 1.18);
  float field = fieldEnvelope * fieldFalloff;

  float epsilon = 0.008;
  float slope = (
    waveCrest(normalizedPoint.x + epsilon, layer, thoughtTime) -
    waveCrest(normalizedPoint.x - epsilon, layer, thoughtTime)
  ) / (epsilon * 2.0) * speechAmplitude;
  float crestFacing = 1.0 / sqrt(1.0 + slope * slope);
  float lowerIllumination = 1.0 - smoothstep(-0.72, 0.34, normalizedPoint.y);
  float fieldSheen = 0.66 + 0.34 * cos(normalizedFieldDepth * PI * 0.86);
  float crestSignal = saturate(
    0.5 +
    sin(normalizedPoint.x * 3.3 + thoughtTime * 1.08 + layer * 1.71) * 0.34 +
    sin(normalizedPoint.x * 6.7 - thoughtTime * 0.47 + layer * 0.93) * 0.16
  );
  float fieldSignal = saturate(
    0.5 +
    sin(normalizedPoint.x * 2.15 - thoughtTime * 0.36 + layer * 2.07) * 0.31 +
    cos(normalizedPoint.x * 4.72 + thoughtTime * 0.29 - layer * 1.12) * 0.19
  );
  float segmentSignal = 0.5 + 0.5 * sin(
    normalizedPoint.x * 1.58 + layer * 1.92 + thoughtTime * 0.18
  );
  float crestAmplitude = 0.42 + crestSignal * 0.72;
  float fieldAmplitude = 0.24 + fieldSignal * 0.82;
  float segmentVisibility = mix(0.28, 1.0, smoothstep(0.24, 0.78, segmentSignal));
  float crestHotspot = pow(crestSignal, 3.4);
  float mistTexture = clamp(
    0.72 +
    sin(normalizedPoint.x * 2.08 + thoughtTime * 0.24 + layer * 1.54) * 0.18 +
    sin(normalizedPoint.x * 5.12 - normalizedPoint.y * 2.65 - thoughtTime * 0.17 + layer) * 0.1,
    0.38,
    1.0
  );
  float colorAmplitude = 0.68 + crestSignal * 0.42 + fieldSignal * 0.12;
  float primaryVisibility = mix(0.58, 1.0, thoughtActivity);
  float secondaryVisibility = smoothstep(0.04, 0.42, waveActivity);
  float layerVisibility = layer < 0.5 ? primaryVisibility : secondaryVisibility;
  float fieldAlpha = field * (0.11 + lowerIllumination * 0.095) * fieldSheen * fieldAmplitude * segmentVisibility;
  float fieldMist = fieldEnvelope * exp(-normalizedFieldDepth * 0.68) * mistTexture;
  float crestMist = exp(-abs(normalizedPoint.y - crest) / 0.082) * belowCrest;
  float mistAlpha = (
    fieldMist * (0.065 + lowerIllumination * 0.052) +
    crestMist * 0.025
  ) * fieldAmplitude * segmentVisibility;
  float glowAlpha = lineGlow * (
    0.03 +
    lowerIllumination * 0.03 +
    crestHotspot * 0.085
  ) * mix(0.5, 1.0, crestSignal);
  float lineAlpha = line * (
    0.2 +
    crestFacing * 0.095 +
    lowerIllumination * 0.06 +
    crestHotspot * 0.16
  ) * crestAmplitude;
  float fieldVisibility = layer < 0.5 ? mix(0.18, 1.0, thoughtActivity) : secondaryVisibility;
  float alpha = (
    (fieldAlpha + mistAlpha) * fieldVisibility +
    (glowAlpha + lineAlpha) * layerVisibility
  ) * strength * thoughtPresence * (1.0 + thoughtActivity * 0.45) * (1.0 + saturate(speaking) * (saturate(u_voice) * 0.5 - 0.2));
  float luminance = 0.62 + lowerIllumination * 0.28 + line * 0.18 + field * 0.09 + mistTexture * mistAlpha * 0.9 + crestHotspot * lineGlow * 0.32;
  return vec4(lightTint * luminance * colorAmplitude, saturate(alpha));
}

float voiceContour(
  vec2 point,
  float radius,
  float theta,
  float ring,
  float time,
  float speaking,
  float antialias
) {
  float speechEnergy = saturate(speaking);
  float waveTheta = theta;
  float broadCarrier = 0.5 + sin(waveTheta * 2.0 + 0.38 + time * 0.34) * 0.5;
  float crestCarrier = 0.5 + sin(waveTheta * 5.0 - 0.76 - time * 0.51) * 0.5;
  float scatterCarrier = 0.5 + sin(waveTheta * 3.0 + 1.9 + time * 0.23) * 0.5;
  float highSpeech = smoothstep(0.68, 0.98, speechEnergy);
  float crestWeight = mix(0.58, 0.74, highSpeech);
  float directionalWeight = 0.86 + sin(waveTheta + 1.34 - time * 0.08) * 0.14;
  float coherentBase = broadCarrier * (1.0 - crestWeight) + crestCarrier * crestWeight;
  coherentBase *= 0.62 + 0.38 * scatterCarrier;
  float secondaryCrests = (pow(crestCarrier, 3.0) + pow(scatterCarrier, 4.0) * 0.6) * highSpeech * 0.16;
  float coherentWave = (pow(saturate(coherentBase), 2.08) + secondaryCrests) * directionalWeight;
  float outwardWave = saturate(coherentWave * mix(0.78, 1.0, saturate(u_voice)));
  float waveVisibility = 0.12 + smoothstep(0.02, 0.42, outwardWave) * 0.88;
  float layerGrowth = pow(ring, 0.72);
  float amplitude = mix(0.008, 0.095, layerGrowth);
  float radialReach = (
    0.0015 +
    ring * 0.0065 +
    amplitude * speechEnergy * mix(0.4, 1.0, saturate(u_voice)) * outwardWave
  );
  float ringRadius = SPHERE_RADIUS + radialReach;
  float fadeReach = 0.052 + speechEnergy * 0.028;
  float distanceFade = exp(-radialReach / fadeReach);
  float lineWidth = mix(antialias * 0.065, antialias * 0.13, ring);
  float filamentDrift =
    0.62 +
    sin(waveTheta * 3.0 + ring * 5.2 + time * 0.22) * 0.24 +
    sin(waveTheta * 7.0 - ring * 2.9 - time * 0.13) * 0.14;
  float presence = waveVisibility * (0.3 + outwardWave * 0.7) * distanceFade * 1.32;
  return softLine(radius - ringRadius, lineWidth, antialias * 0.28) * presence * saturate(filamentDrift);
}

float lqHash(vec2 pIn) {
  vec2 p = fract(pIn * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

float lqNoise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(lqHash(i), lqHash(i + vec2(1.0, 0.0)), f.x),
    mix(lqHash(i + vec2(0.0, 1.0)), lqHash(i + vec2(1.0, 1.0)), f.x),
    f.y
  );
}

vec2 lqFbm(vec2 pIn, float blurSigma) {
  vec2 p = pIn;
  float sum = 0.0;
  float amplitude = 0.5;
  float weight = 0.0;
  float variance = 0.0;
  float exponent = -6.0 * blurSigma * blurSigma;
  float growth = 1.0;
  for (int octave = 0; octave < 5; octave += 1) {
    float attenuation = exp(exponent * growth);
    sum += amplitude * (0.5 + attenuation * (lqNoise(p) - 0.5));
    variance += amplitude * amplitude * (1.0 - attenuation * attenuation);
    weight += amplitude;
    amplitude *= 0.5;
    growth *= 4.1209;
    p = vec2(0.8 * p.x - 0.6 * p.y, 0.6 * p.x + 0.8 * p.y) * 2.03;
  }
  return vec2(sum / weight, 0.32 * sqrt(variance) / weight);
}

float lqRidge(float v, float k) {
  return pow(clamp(1.0 - abs(v * 2.0 - 1.0), 0.0, 1.0), k);
}

float lqRidgeS(vec2 vs, float k) {
  float d = 1.73205081 * vs.y;
  return (lqRidge(vs.x - d, k) + 4.0 * lqRidge(vs.x, k) + lqRidge(vs.x + d, k)) / 6.0;
}

float lqStepS(vec2 vs, float a, float b) {
  float d = 1.73205081 * vs.y;
  return (smoothstep(a, b, vs.x - d) + 4.0 * smoothstep(a, b, vs.x) + smoothstep(a, b, vs.x + d)) / 6.0;
}

vec3 lqRamp(float v, vec3 a, vec3 b, vec3 c, vec3 d) {
  vec3 color = mix(a, b, smoothstep(0.0, 0.45, v));
  color = mix(color, c, smoothstep(0.38, 0.72, v));
  return mix(color, d, smoothstep(0.68, 1.0, v));
}

vec3 frostFluid(vec2 p, float t) {
  vec3 frostA = vec3(0.969, 0.984, 1.0);
  vec3 frostB = vec3(0.839, 0.910, 0.969);
  vec3 frostC = vec3(0.659, 0.784, 0.941);
  vec3 frostD = vec3(0.435, 0.620, 0.910);
  vec2 q = p * 0.9912;
  q.y += t * 0.055;
  float blur = 0.01316;
  vec2 warpField = vec2(
    lqFbm(q * 1.14 + vec2(t * 0.055, 0.0), blur).x,
    lqFbm(q * 1.14 + vec2(6.8, -t * 0.048), blur).x
  );
  vec2 warped = q + (warpField - 0.5) * 0.909;
  vec2 body = lqFbm(warped * 1.48 + vec2(t * 0.032, -t * 0.02), blur * 1.48);
  float veins = lqRidgeS(lqFbm(warped * 2.36 + vec2(3.1, -t * 0.024), blur * 2.36), 2.05);
  float value = mix(lqStepS(body, 0.1, 0.9), clamp(veins * 0.8 + body.x * 0.46, 0.0, 1.0), 0.45);
  vec3 color = lqRamp(value, frostA, frostB, frostC, frostD);
  color = mix(color, frostA, 0.08 * smoothstep(0.62, 0.92, body.x));
  color = mix(color, vec3(1.0), 0.066 * smoothstep(0.15, 1.15, dot(p, vec2(-0.32, 0.78))));
  color *= 1.0 - 0.102 * smoothstep(-0.1, 1.2, dot(p, vec2(0.45, -0.62)));
  color *= 1.0 - 0.066 * smoothstep(0.72, 1.08, length(p));
  return clamp(color, 0.0, 1.0);
}

float glassLobe(vec2 normal, vec2 direction, float cut, float power) {
  return pow(clamp((dot(normal, direction) - cut) / max(1.0 - cut, 0.001), 0.0, 1.0), power);
}

vec3 frostGlass(vec2 p, float t) {
  float pd = length(p);
  vec2 normal = pd > 0.0001 ? p / pd : vec2(0.0, 1.0);
  float edgeDepth = max(1.0 - pd, 0.0);
  float refractionDepth = clamp(edgeDepth / 0.205, 0.0, 1.0);
  float refractionProfile = pow(1.0 - sqrt(max(1.0 - (1.0 - refractionDepth) * (1.0 - refractionDepth), 0.0)), 0.68);
  vec2 refracted = p - normal * 0.672 * refractionProfile;
  float channelSplit = 0.016464 * refractionProfile;
  vec3 fluid;
  if (channelSplit > 0.0004) {
    fluid = vec3(
      frostFluid(refracted - normal * channelSplit, t).r,
      frostFluid(refracted, t).g,
      frostFluid(refracted + normal * channelSplit, t).b
    );
  } else {
    fluid = frostFluid(refracted, t);
  }
  float luminance = dot(fluid, vec3(0.213, 0.715, 0.072));
  vec3 color = clamp(vec3(luminance) + (fluid - luminance) * 1.22, 0.0, 1.0);
  float opticalRim = pow(1.0 - smoothstep(0.0, 0.0381, edgeDepth), 1.8);
  color = mix(color, vec3(1.0), opticalRim * 0.189);
  float dispersion = opticalRim * 0.28 * 0.976;
  color = mix(color, vec3(0.839, 0.910, 0.969), dispersion * glassLobe(normal, normalize(vec2(0.84, 0.54)), -0.32, 1.8));
  color = mix(color, vec3(0.435, 0.620, 0.910), dispersion * glassLobe(normal, normalize(vec2(-0.62, -0.78)), -0.28, 2.0));
  color *= 1.0 - opticalRim * 0.048 * (0.15 + 0.85 * max(dot(normal, vec2(0.45, -0.89)), 0.0));
  color = mix(color, vec3(0.918, 0.957, 1.0), opticalRim * glassLobe(normal, normalize(vec2(-0.68, 0.73)), 0.2, 2.8) * 0.476);
  color = mix(color, vec3(0.863, 0.918, 1.0), opticalRim * glassLobe(normal, normalize(vec2(0.74, -0.67)), 0.4, 3.6) * 0.34);
  return color;
}

void main() {
  vec2 sceneResolution = u_scene.zw;
  float shortestSide = min(sceneResolution.x, sceneResolution.y);
  float antialias = 1.0 / shortestSide;
  vec2 point = (gl_FragCoord.xy - u_orb_center) / shortestSide;

  float surfaceLuminance = dot(u_background, vec3(0.2126, 0.7152, 0.0722));
  float lightSurfaceScale = smoothstep(0.58, 0.84, surfaceLuminance);
  point /= 1.0 + saturate(u_speaking) * 0.06;

  float radius = length(point);
  float theta = atan(point.y, point.x);
  float contourWave =
    sin(theta * 3.0 + u_speech_time * 0.62) * 0.52 +
    sin(theta * 5.0 - u_speech_time * 0.41 + 1.7) * 0.31 +
    sin(theta * 2.0 + u_speech_time * 0.23 + 3.1) * 0.17;
  float contourRadius = SPHERE_RADIUS * (1.0 + lightSurfaceScale * saturate(u_speaking) * 0.045 * contourWave);
  float sphere = 1.0 - smoothstep(contourRadius - antialias, contourRadius + antialias, radius);
  float normalizedRadius = radius / SPHERE_RADIUS;
  float depth = sqrt(max(0.0, 1.0 - normalizedRadius * normalizedRadius));
  float outside = step(SPHERE_RADIUS + antialias * 0.25, radius);

  float backgroundLuminance = dot(u_background, vec3(0.2126, 0.7152, 0.0722));
  float lightSurface = smoothstep(0.58, 0.84, backgroundLuminance);
  vec3 neutral = mix(vec3(0.91, 0.92, 0.95), vec3(0.12, 0.13, 0.16), lightSurface);
  vec3 errorTint = mix(vec3(1.0, 0.42, 0.4), vec3(0.72, 0.12, 0.1), lightSurface);
  vec3 lightTint = mix(neutral, errorTint, u_error);
  vec3 glowTint = mix(lightTint, mix(vec3(0.62, 0.67, 0.97), errorTint, u_error), lightSurface);
  vec3 waveTint = mix(lightTint, mix(vec3(0.31, 0.37, 0.8), errorTint, u_error), lightSurface);

  float bodyReveal = smoothstep(
    mix(0.52, 0.08, u_departing),
    mix(0.985, 0.86, u_departing),
    u_reveal
  );
  float rimBuild = smoothstep(0.3, 0.76, u_reveal);
  float rimProgress = smoothstep(0.34, 0.82, u_reveal);
  float rimCoordinate = abs(theta) / PI;
  float rimSweep = 1.0 - smoothstep(rimProgress, rimProgress + 0.055, rimCoordinate);
  float assemblyRim = exp(-abs(radius - SPHERE_RADIUS) / 0.0042) * rimBuild * rimSweep;
  float structureMask = saturate(max(bodyReveal, assemblyRim));
  float contourLimit = SPHERE_RADIUS + 0.0015 + 0.0065 + 0.095 + antialias;
  vec3 color = u_background;
  if (structureMask > 0.0 && (sphere > 0.0 || (lightSurface < 1.0 && (radius < 0.49 || (outside > 0.0 && u_speaking > 0.0 && radius <= contourLimit))))) {
    float normalizedY = point.y / SPHERE_RADIUS;
    float topPole = smoothstep(0.12, 0.92, normalizedY);
    float bottomPole = 1.0 - smoothstep(-0.92, -0.12, normalizedY);
    float shellDirection = 0.48 + topPole * 0.34 + bottomPole * 0.46;
    float shellDiffraction = 0.86 + cos(theta * 2.0) * 0.14;
    float shellActivity = smoothstep(0.08, 0.84, max(u_speaking * mix(0.5, 1.0, saturate(u_voice)), u_thinking));
    float shellPower = mix(0.42, 1.0, shellActivity);
    float lightFocus = saturate(dot(normalize(point + vec2(0.0001)), vec2(0.0, 1.0)));
    float boostShape = mix(0.3, 1.0, pow(lightFocus, 1.25));
    float shellPowerShaped = 0.42 + max(0.0, shellPower - 0.42) * boostShape;
    float outwardDistance = max(0.0, radius - SPHERE_RADIUS);
    float canvasFade = 1.0 - smoothstep(0.405, 0.49, radius);
    float haloCore = exp(-outwardDistance / 0.010);
    float haloTail = exp(-outwardDistance / mix(0.042, 0.06, shellActivity));
    float haloProfile = haloCore * 0.6 + haloTail * 0.19;
    float haloDirection = 0.64 + shellDirection * 0.36;
    float haloTexture =
      0.93 +
      sin(theta * 3.0 + outwardDistance * 24.0) * 0.045 +
      sin(theta * 7.0 - outwardDistance * 11.0 + 0.74) * 0.025;
    float shellOuterRadiance = haloProfile * haloDirection * shellDiffraction * haloTexture * shellPowerShaped * outside * canvasFade;
    float haloDither = (interleavedNoise(gl_FragCoord.xy) - 0.5) / 255.0;
    shellOuterRadiance = max(0.0, shellOuterRadiance + haloDither * mix(0.7, 1.25, 1.0 - haloProfile) * outside * canvasFade);
    shellOuterRadiance *= 1.0 - lightSurface;

    float contours = 0.0;
    float contourGlow = 0.0;
    if (outside > 0.0 && radius <= contourLimit && u_speaking > 0.0 && lightSurface < 1.0) {
      for (int index = 0; index < 8; index += 1) {
        float ring = float(index) / 7.0;
        float line = voiceContour(point, radius, theta, ring, u_speech_time, u_speaking, antialias);
        float strength = mix(0.98, 0.6, pow(ring, 0.88));
        contours += line * strength;
        contourGlow += line * strength * strength;
      }
    }
    float contourEnergy = saturate(u_speaking) * mix(0.35, 1.15, saturate(u_voice));
    contours = saturate(contours * contourEnergy * 0.68);
    contourGlow = saturate(contourGlow * contourEnergy * 0.12);
    contours *= outside * (1.0 - lightSurface);
    contourGlow *= outside * (1.0 - lightSurface);

    float outerAlpha = saturate(shellOuterRadiance + contours + contourGlow);
    float outerLuminance = 0.73 + shellDirection * 0.055 + sqrt(saturate(contours)) * 0.2 + contourGlow * 0.045;
    vec3 outerColor = glowTint * outerLuminance;

    color = mix(u_background, outerColor, outerAlpha * (1.0 - sphere));
    if (sphere > 0.0) {
      vec3 lightDirection = normalize(vec3(-0.42, 0.7, 0.88));
      vec3 normal = normalize(vec3(point / SPHERE_RADIUS, depth));
      float diffuse = saturate(dot(normal, lightDirection));
      float lowerLight = 1.0 - smoothstep(-0.78, 0.46, normalizedY);
      float bodyLuminance = 0.038 + depth * 0.022 + diffuse * 0.018;
      bodyLuminance += (1.0 - smoothstep(0.28, 0.96, normalizedRadius)) * 0.0075;
      bodyLuminance += lowerLight * (0.04 + depth * 0.05);
      vec3 frostBody = vec3(0.0);
      if (lightSurface > 0.001 && radius < contourRadius * 1.02) {
        frostBody = frostGlass(point / contourRadius, u_speech_time * 0.7 + u_thought_time * 1.6);
      }
      vec3 bodyBase = mix(vec3(bodyLuminance), frostBody, lightSurface);
      vec3 body = mix(bodyBase, bodyBase * vec3(1.2, 0.72, 0.74), u_error * 0.52);

      vec3 sphereColor = body;
      if (lightSurface < 1.0 && max(u_thinking, u_speaking) > 0.0) {
        float waveTime = u_thought_time + u_speech_time;
        vec4 membraneBack = shadeWaveField(point, 0.0, 0.42, waveTime, u_thinking, u_speaking, waveTint, antialias);
        vec4 membraneUpper = shadeWaveField(point, 1.0, 0.76, waveTime, u_thinking, u_speaking, waveTint, antialias);
        vec4 membraneMiddle = shadeWaveField(point, 2.0, 0.94, waveTime, u_thinking, u_speaking, waveTint, antialias);
        vec4 membraneLower = shadeWaveField(point, 3.0, 0.7, waveTime, u_thinking, u_speaking, waveTint, antialias);
        float membranePresence = 1.0 - lightSurface;
        sphereColor = mix(sphereColor, membraneBack.rgb, membraneBack.a * membranePresence);
        sphereColor = mix(sphereColor, membraneUpper.rgb, membraneUpper.a * membranePresence);
        sphereColor = mix(sphereColor, membraneMiddle.rgb, membraneMiddle.a * membranePresence);
        sphereColor = mix(sphereColor, membraneLower.rgb, membraneLower.a * membranePresence);
      }
      float lowerDepth = (1.0 - smoothstep(-0.16, 0.12, point.y)) * sphere;
      sphereColor += waveTint * lowerDepth * depth * 0.032;

      float innerRim = 1.0 - smoothstep(
        antialias * 0.18,
        antialias * 1.35,
        abs(radius - SPHERE_RADIUS)
      );
      float rimAngle = 0.48 + 0.52 * lightFocus;
      float fresnel = pow(1.0 - depth, 3.4);
      float rimCoreDirection = shellDirection * (1.0 - topPole * 0.24);
      float rim = innerRim * rimCoreDirection * (1.42 + rimAngle * 0.58) * shellPowerShaped;
      float inwardDistance = max(0.0, SPHERE_RADIUS - radius);
      float shellInnerCore = exp(-inwardDistance * 115.0) * 0.38;
      float shellInnerScatter = exp(-inwardDistance * 24.0) * 0.34;
      float shellInnerAtmosphere = exp(-inwardDistance * 7.5) * 0.14;
      float shellInnerRadiance = (
        shellInnerCore +
        shellInnerScatter +
        shellInnerAtmosphere
      ) * shellDirection * shellDiffraction * shellPowerShaped;
      float shellFresnel = fresnel * shellDirection * (0.042 + topPole * 0.068 + bottomPole * 0.108) * shellPowerShaped;
      sphereColor += glowTint * (rim + shellInnerRadiance + shellFresnel) * sphere * (1.0 - lightSurface);

      color = mix(color, sphereColor, sphere);
    }
    float displayDither = (interleavedNoise(gl_FragCoord.xy + vec2(17.0, 41.0)) - 0.5) / 255.0;
    float ditherMask = saturate(outerAlpha * 7.0) * (1.0 - sphere) * canvasFade;
    color += vec3(displayDither * 1.7 * ditherMask);
  }

  float dustArrival = smoothstep(0.015, 0.13, u_reveal);
  float dustDeparture = 1.0 - smoothstep(0.72, 0.98, u_reveal);
  float windowDust = dustArrival * dustDeparture * (1.0 - u_departing);
  float gather = smoothstep(0.0, 0.85, u_reveal);
  float ambientPhase = mix(1.0 - gather, 1.0, u_departing);
  float spread = u_ambient * ambientPhase;
  float handoff = u_departing * (1.0 - smoothstep(0.25, 0.9, u_reveal));
  float dustPresence = max(min(1.0, windowDust + spread), handoff);
  float formation = formationAt(gl_FragCoord.xy, shortestSide, max(u_pixel_ratio, 1.0));
  float glyphPitch = max(1.15 * u_pixel_ratio, 1.6);
  vec2 glyphSpan = vec2(6.0, 9.0) * glyphPitch;
  vec2 glyphCell = floor(gl_FragCoord.xy / glyphSpan);
  float rippleWake;
  float rippleCrest;
  rippleAt((glyphCell + 0.5) * glyphSpan, shortestSide, rippleWake, rippleCrest);
  float glyphAlpha = 0.0;
  float particleLuminance = 0.0;
  if (dustPresence > 0.0 || formation > 0.0 || (rippleWake + rippleCrest > 0.002 && sphere * bodyReveal < 1.0)) {
    float particleCell = 2.9 * max(u_pixel_ratio, 1.0);
    vec2 cell = floor(gl_FragCoord.xy / particleCell);
    vec2 localCell = fract(gl_FragCoord.xy / particleCell) - 0.5;
    vec2 jitter = vec2(
      interleavedNoise(cell + vec2(11.0, 37.0)),
      interleavedNoise(cell + vec2(79.0, 19.0))
    ) - 0.5;
    jitter *= 0.39;
    float dotDistance = length(localCell - jitter);
    float fineDot = 1.0 - smoothstep(mix(0.105, 0.17, lightSurface), mix(0.185, 0.27, lightSurface), dotDistance);
    float particleSeed = interleavedNoise(cell + vec2(31.0, 53.0));
    vec2 orbPoint = point / max(SPHERE_RADIUS, 0.001);
    float orbRadius = length(orbPoint);
    float orbTheta = atan(orbPoint.y, orbPoint.x);
    float openness = saturate(spread * (1.0 - gather));
    float squeeze = 1.0 - smoothstep(0.0, 1.0, 1.0 - openness) * 0.965;
    float shellRadius = 1.025;
    float sourceRadius = shellRadius + (orbRadius - shellRadius) / squeeze;
    float sourceValid = step(0.0, sourceRadius);
    float sourceReach = max(sourceRadius, 0.0);
    float swirl = (1.0 - squeeze) * 1.35 / (0.7 + sourceReach);
    float sourceTheta = orbTheta - swirl;
    vec2 source = vec2(cos(sourceTheta), sin(sourceTheta)) * sourceReach;
    float compression = min(3.2, sourceReach / max(orbRadius, 0.02) / squeeze);
    float inkScale = mix(1.0, SPHERE_RADIUS / 0.235, smoothstep(0.0, 1.0, openness));
    vec2 inkBody = source * inkScale;
    float inkCoverage = 0.0;
    if (sourceValid > 0.0 && length(vec2(inkBody.x / 1.7, inkBody.y)) < 2.0 + 0.5 * 0.8) {
      float inkSpin = u_time * 0.012;
      vec2 inkPoint = mat2(cos(inkSpin), sin(inkSpin), -sin(inkSpin), cos(inkSpin)) * inkBody * 1.25;
      vec2 inkWarpA = vec2(
        fbm(inkPoint * 0.75 + vec2(0.0, u_time * 0.02)),
        fbm(inkPoint * 0.75 + vec2(5.2, -u_time * 0.017))
      ) - 0.5;
      vec2 inkWarpB = vec2(
        fbm(inkPoint * 1.6 + inkWarpA * 2.0 + vec2(1.7, u_time * 0.03)),
        fbm(inkPoint * 1.6 + inkWarpA * 2.0 + vec2(8.3, -u_time * 0.025))
      ) - 0.5;
      vec2 inkFlow = inkPoint + inkWarpA * 1.6 + inkWarpB * 0.9;
      float inkLobes = fbm(inkFlow * 0.95 + 3.1);
      float inkRidge = 1.0 - saturate(abs(fbm(inkFlow * 2.1 + 7.7) - 0.48) * 9.0);
      float inkTendril = pow(inkRidge, 3.0) * smoothstep(0.3, 0.5, inkLobes);
      float inkEdge = length(vec2(inkBody.x / 1.7, inkBody.y)) + (fbm(inkBody * 1.1 + 11.0) - 0.5) * 0.8;
      float inkEnvelope = 1.0 - smoothstep(1.0, 2.0, inkEdge);
      float inkHollow = mix(0.5, 1.0, smoothstep(0.1, 1.15, sourceReach));
      float inkDensity = inkEnvelope * inkHollow * (smoothstep(0.46, 0.8, inkLobes) * 0.42 + inkTendril * 0.9);
      inkCoverage = saturate(inkDensity * compression * 1.25) * sourceValid;
    }
    vec2 edgeDistance = (u_resolution * 0.5 - abs(gl_FragCoord.xy - u_resolution * 0.5)) / shortestSide;
    float edgeFade = smoothstep(0.012, 0.09, min(edgeDistance.x, edgeDistance.y));
    vec2 canvasEdgeDistance = (u_resolution * 0.5 - abs(gl_FragCoord.xy - u_resolution * 0.5)) / shortestSide;
    float canvasEdgeFade = smoothstep(0.004, 0.03, min(canvasEdgeDistance.x, canvasEdgeDistance.y));
    float formedCoverage = max(inkCoverage, formation * mix(1.0, 0.5, lightSurface));
    float formationEdge = exp(-abs(formedCoverage - particleSeed) / 0.045) * step(0.02, formation);
    float particleDensity = smoothstep(particleSeed - 0.06, particleSeed + 0.06, formedCoverage);
    float particleEnvelope = mix(0.45, 0.85, formedCoverage) * max(edgeFade, formation);
    float particleSpark = fineDot * particleDensity * particleEnvelope * max(dustPresence, formation);
    particleLuminance = mix(0.23, 0.76, particleSeed) * particleSpark * mix(1.0, mix(0.525, 1.75, lightSurface), spread)
      * mix(1.0, mix(0.85, 0.4, lightSurface), u_quiet_field * saturate(spread))
      * (1.0 + formation * mix(2.4, 0.35, lightSurface) + formationEdge * 0.9);

    if (rippleWake + rippleCrest > 0.002 && sphere * bodyReveal < 1.0) {
      float glyphSeed = hash21(glyphCell + vec2(3.7, 11.3));
      float glyphPatch = valueNoise(glyphCell * vec2(0.21, 0.34) + vec2(u_time * 0.05, 0.0));
      float glyphDensity = saturate(0.3 + glyphPatch * 0.75 + inkCoverage * 0.5);
      float glyphOn = step(glyphSeed, glyphDensity * saturate(rippleWake * 1.35 + rippleCrest));
      float scrambleRate = mix(3.0, 18.0, rippleCrest);
      float glyphPick = hash21(glyphCell * 1.37 + floor(u_time * scrambleRate + glyphSeed * 13.0));
      ivec2 glyph = GLYPHS[min(int(glyphPick * float(GLYPH_COUNT)), GLYPH_COUNT - 1)];
      vec2 glyphLocal = (gl_FragCoord.xy - glyphCell * glyphSpan) / glyphPitch;
      float lit = glyphBit(glyph, int(floor(glyphLocal.x)), 7 - int(floor(glyphLocal.y)));
      float glyphDot = 1.0 - smoothstep(0.26, 0.44, length(fract(glyphLocal) - 0.5));
      glyphAlpha = lit * glyphDot * glyphOn * (1.0 - sphere * bodyReveal) * canvasEdgeFade * saturate(0.2 + rippleWake * 0.32 + rippleCrest * 0.62);
    }
    particleLuminance *= 1.0 + rippleCrest * saturate(spread) * 0.8;
  }

  vec3 revealedColor = mix(u_background, color, structureMask);
  revealedColor += lightTint * particleLuminance * (1.0 - lightSurface);
  revealedColor = mix(revealedColor, LIGHT_PARTICLE_INK, saturate(particleLuminance * 1.3) * lightSurface);
  vec3 darkGlyph = mix(vec3(0.72), vec3(0.97), saturate(rippleCrest * 1.2));
  vec3 lightGlyph = mix(vec3(0.4, 0.47, 0.6), LIGHT_PARTICLE_INK * 0.92, saturate(rippleCrest * 1.2));
  revealedColor = mix(revealedColor, mix(darkGlyph, lightGlyph, lightSurface), saturate(glyphAlpha * mix(1.0, 1.15, lightSurface)));
  outputColor = vec4(clamp(revealedColor, 0.0, 1.0), 1.0);
}
`;

export function VoiceOrbScene({ activity, ambient = false, animated = true, sceneSurface, formationSurface, onDispersed, phase, quietField = false, rippleSurface, sizeProbe, sphere = 0.235 }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const onDispersedRef = useRef(onDispersed);
  onDispersedRef.current = onDispersed;
  const sizeProbeRef = useRef(sizeProbe);
  sizeProbeRef.current = sizeProbe;
  const rippleSurfaceRef = useRef(rippleSurface);
  rippleSurfaceRef.current = rippleSurface;
  const sceneSurfaceRef = useRef(sceneSurface);
  sceneSurfaceRef.current = sceneSurface;
  const formationSurfaceRef = useRef(formationSurface);
  formationSurfaceRef.current = formationSurface;
  const frameRef = useRef<OrbFrame>({ activity, ambient, phase, quietField, sphere });
  const requestRenderRef = useRef<(() => void) | null>(null);
  const [failure, setFailure] = useState("");
  const reducedMotion = useShouldReduceMotion();
  const staticScene = reducedMotion || !animated;
  frameRef.current = { activity, ambient, phase, quietField, sphere };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const gl = canvas.getContext("webgl2", {
      alpha: true,
      antialias: false,
      desynchronized: true,
      powerPreference: "high-performance",
      premultipliedAlpha: false,
    });
    if (!gl) {
      setFailure("WebGL 2 is required for the voice scene");
      return;
    }

    let disposed = false;
    let animationFrame = 0;
    let canvasVisible = true;
    let pageVisible = document.visibilityState !== "hidden";
    const visibilityRoot = rippleSurfaceRef.current?.current ?? canvas.closest(".voice-view") ?? canvas.parentElement;
    const surfaceIsVisible = () => !visibilityRoot?.closest('[hidden], [aria-hidden="true"]');
    let surfaceVisible = surfaceIsVisible();
    let previousTime = performance.now();
    let lastRenderTime = 0;
    let revealElapsed = frameRef.current.ambient ? 0 : REVEAL_SPAN;
    let smoothAmbient = 1;
    let smoothSpeaking = 0;
    let smoothVoice = 0;
    let smoothThinking = 0;
    let thoughtTime = staticScene ? 3.7 : 0;
    let speechTime = staticScene ? 3.7 : 0;
    let renderPixelRatio = 1;
    let measuredSphere = 0;
    const scene = new Float32Array(4);
    const orbCenter = new Float32Array(2);
    const ripples = new Float32Array(RIPPLE_COUNT * 4);
    let nextRipple = 0;
    const forms = new Float32Array(FORM_COUNT * 4);
    const formTiming = new Float32Array(FORM_COUNT * 4);
    const formedElements = new WeakSet<Element>();
    const leavingElements = new WeakSet<Element>();
    let nextForm = 0;
    let activeUntil = 0;
    let background = readBackgroundColor(canvas);
    let program: WebGLProgram;
    let vertexArray: WebGLVertexArrayObject;
    let uniforms: ShaderUniforms;
    try {
      program = createProgram(gl, VERTEX_SHADER, FRAGMENT_SHADER);
      const createdVertexArray = gl.createVertexArray();
      if (!createdVertexArray) {
        gl.deleteProgram(program);
        throw new Error("The voice scene could not allocate its render surface");
      }
      vertexArray = createdVertexArray;
      uniforms = readUniforms(gl, program);
    } catch (error) {
      setFailure(error instanceof Error && error.message ? error.message : "The voice scene could not start");
      return;
    }

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      const pixelRatio = Math.min(window.devicePixelRatio || 1, MAX_RENDER_PIXEL_RATIO);
      renderPixelRatio = pixelRatio;
      const width = Math.max(1, Math.round(rect.width * pixelRatio));
      const height = Math.max(1, Math.round(rect.height * pixelRatio));
      const sceneRect = sceneSurfaceRef.current?.current?.getBoundingClientRect() ?? rect;
      const scaleX = width / Math.max(rect.width, 1);
      const scaleY = height / Math.max(rect.height, 1);
      scene[0] = (sceneRect.left - rect.left) * scaleX;
      scene[1] = (rect.bottom - sceneRect.bottom) * scaleY;
      scene[2] = Math.max(1, sceneRect.width * scaleX);
      scene[3] = Math.max(1, sceneRect.height * scaleY);
      const probe = sizeProbeRef.current?.current;
      const probeWidth = probe?.clientWidth ?? 0;
      const probeRect = probe?.getBoundingClientRect();
      orbCenter[0] = probeRect && probeWidth > 0 ? (probeRect.left + probeRect.width / 2 - rect.left) * scaleX : scene[0] + scene[2] / 2;
      orbCenter[1] = probeRect && probeWidth > 0 ? (rect.bottom - probeRect.top) * scaleY : scene[1] + scene[3] / 2;
      measuredSphere = probeWidth > 0 ? Math.min(0.36, (probeWidth * scaleX * 0.285) / Math.min(scene[2], scene[3])) : 0;
      const cleared = canvas.width !== width || canvas.height !== height;
      if (cleared) {
        canvas.width = width;
        canvas.height = height;
      }
      gl.viewport(0, 0, width, height);
      return cleared;
    };

    const scheduleDraw = () => {
      if (disposed || staticScene || animationFrame || !canvasVisible || !pageVisible || !surfaceVisible) return;
      animationFrame = window.requestAnimationFrame(draw);
    };

    const draw = (now: number) => {
      animationFrame = 0;
      if (disposed) return;
      if (!canvasVisible || !pageVisible || !surfaceVisible) {
        previousTime = now;
        lastRenderTime = now;
        return;
      }
      if (!staticScene && lastRenderTime && now - lastRenderTime < (now < activeUntil || !frameRef.current.ambient ? ACTIVE_FRAME_INTERVAL_MS : FRAME_INTERVAL_MS)) {
        scheduleDraw();
        return;
      }
      const elapsed = Math.max(0.001, Math.min(0.05, (now - previousTime) / 1000));
      previousTime = now;
      lastRenderTime = now;
      const frame = frameRef.current;
      const ambientTarget = frame.ambient && revealElapsed >= AMBIENT_GATE ? 0 : 1;
      smoothAmbient = approach(smoothAmbient, ambientTarget, elapsed, ambientTarget ? 2.8 : 8);
      if (!staticScene) {
        revealElapsed = frame.ambient
          ? Math.max(0, revealElapsed - elapsed * REVEAL_DECAY)
          : Math.min(REVEAL_SPAN, revealElapsed + elapsed);
      }
      const seconds = staticScene ? 3.7 : now / 1000;
      const next = resolveActivity(frame.activity, frame.phase, seconds);
      smoothThinking = approach(smoothThinking, next.thinking, elapsed, next.thinking > smoothThinking ? 6 : 1.5);
      smoothSpeaking = approach(smoothSpeaking, next.speaking, elapsed, next.speaking > smoothSpeaking ? 3.2 : 1.1);
      smoothVoice = approach(smoothVoice, next.voice, elapsed, next.voice > smoothVoice ? 14 : 5);
      if (!staticScene) {
        thoughtTime += elapsed * smoothThinking * 1.8;
        speechTime += elapsed * smoothSpeaking * (0.55 + smoothVoice * 0.8) * 1.35;
      }
      const settledAmbient = frame.ambient
        && revealElapsed === 0
        && smoothAmbient < 0.002
        && smoothSpeaking < 0.002
        && smoothThinking < 0.002;
      if (settledAmbient) {
        smoothAmbient = 0;
        smoothSpeaking = 0;
        smoothVoice = 0;
        smoothThinking = 0;
      }

      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.useProgram(program);
      gl.bindVertexArray(vertexArray);
      gl.uniform3f(uniforms.background, background[0], background[1], background[2]);
      gl.uniform4fv(uniforms.ripples, ripples);
      gl.uniform4fv(uniforms.forms, forms);
      gl.uniform4fv(uniforms.formTiming, formTiming);
      gl.uniform2f(uniforms.resolution, canvas.width, canvas.height);
      gl.uniform4fv(uniforms.scene, scene);
      gl.uniform2fv(uniforms.orbCenter, orbCenter);
      gl.uniform1f(uniforms.pixelRatio, renderPixelRatio);
      gl.uniform1f(uniforms.time, seconds);
      gl.uniform1f(uniforms.thoughtTime, thoughtTime);
      gl.uniform1f(uniforms.thinking, smoothThinking);
      gl.uniform1f(uniforms.speaking, smoothSpeaking);
      gl.uniform1f(uniforms.voice, smoothVoice);
      gl.uniform1f(uniforms.speechTime, speechTime);
      gl.uniform1f(uniforms.error, frame.phase === "error" ? 1 : 0);
      gl.uniform1f(uniforms.ambient, staticScene ? (frame.ambient ? 1 : 0) : smoothAmbient);
      gl.uniform1f(uniforms.sphere, measuredSphere > 0 ? measuredSphere : frame.sphere);
      gl.uniform1f(uniforms.reveal, staticScene ? (frame.ambient ? 0 : 1) : smoothStep(0, REVEAL_RISE, revealElapsed));
      gl.uniform1f(uniforms.departing, frame.ambient ? 1 : 0);
      gl.uniform1f(uniforms.quietField, frame.quietField ? 1 : 0);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      gl.bindVertexArray(null);
      const dispersed = frame.ambient && (staticScene || revealElapsed === 0);
      if (dispersed) onDispersedRef.current?.();
      if (!staticScene && (!settledAmbient || now < activeUntil)) scheduleDraw();
    };

    const loseContext = (event: Event) => {
      event.preventDefault();
      disposed = true;
      window.cancelAnimationFrame(animationFrame);
      animationFrame = 0;
      requestRenderRef.current = null;
      setFailure("The voice scene lost its graphics context");
    };
    requestRenderRef.current = () => {
      if (staticScene) draw(performance.now());
      else scheduleDraw();
    };
    const startRipple = (event: PointerEvent) => {
      if (staticScene || event.button !== 0 || !surfaceVisible || !canvasVisible || !pageVisible) return;
      if (event.target instanceof Element && event.target.closest(SCENE_RIPPLE_IGNORED_TARGETS)) return;
      const rect = canvas.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      const now = performance.now();
      const slot = nextRipple * 4;
      nextRipple = (nextRipple + 1) % RIPPLE_COUNT;
      ripples[slot] = (event.clientX - rect.left) * (canvas.width / rect.width);
      ripples[slot + 1] = canvas.height - (event.clientY - rect.top) * (canvas.height / rect.height);
      ripples[slot + 2] = now / 1000;
      ripples[slot + 3] = 1;
      activeUntil = Math.max(activeUntil, now + RIPPLE_LIFE_SECONDS * 1000);
      lastRenderTime = 0;
      scheduleDraw();
    };
    const formFrom = () => {
      const host = formationSurfaceRef.current?.current;
      if (!host || staticScene) return;
      const canvasRect = canvas.getBoundingClientRect();
      if (canvasRect.width <= 0 || canvasRect.height <= 0) return;
      const scaleX = canvas.width / canvasRect.width;
      const scaleY = canvas.height / canvasRect.height;
      const now = performance.now();
      for (const element of host.querySelectorAll("[data-voice-forming].is-layout-ready, [data-voice-leaving].is-layout-ready")) {
        const formed = element.hasAttribute("data-voice-leaving") ? leavingElements : formedElements;
        if (formed.has(element)) continue;
        formed.add(element);
        const rect = element.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) continue;
        const slot = nextForm * 4;
        nextForm = (nextForm + 1) % FORM_COUNT;
        forms[slot] = (rect.left - canvasRect.left) * scaleX;
        forms[slot + 1] = canvas.height - (rect.bottom - canvasRect.top) * scaleY;
        forms[slot + 2] = (rect.right - canvasRect.left) * scaleX;
        forms[slot + 3] = canvas.height - (rect.top - canvasRect.top) * scaleY;
        formTiming[slot] = now / 1000;
        formTiming[slot + 1] = (Number.parseFloat(getComputedStyle(element).borderTopLeftRadius) || 0) * scaleX;
        activeUntil = Math.max(activeUntil, now + FORM_LIFE_SECONDS * 1000);
        lastRenderTime = 0;
        scheduleDraw();
      }
    };
    const formationHost = formationSurfaceRef.current?.current ?? null;
    const formationObserver = new MutationObserver(formFrom);
    if (formationHost) {
      formationObserver.observe(formationHost, { attributes: true, attributeFilter: ["class", "data-voice-forming", "data-voice-leaving"], childList: true, subtree: true });
      formFrom();
    }
    const rippleTarget = rippleSurfaceRef.current?.current ?? null;
    rippleTarget?.addEventListener("pointerdown", startRipple, { passive: true });
    const resizeAndDraw = () => {
      if (disposed) return;
      window.cancelAnimationFrame(animationFrame);
      animationFrame = 0;
      if (resize()) lastRenderTime = 0;
      draw(performance.now());
    };
    const observer = new ResizeObserver(() => {
      background = readBackgroundColor(canvas);
      resizeAndDraw();
    });
    const themeObserver = new MutationObserver(() => {
      background = readBackgroundColor(canvas);
      requestRenderRef.current?.();
    });
    const intersectionObserver = new IntersectionObserver((entries) => {
      canvasVisible = entries.some((entry) => entry.isIntersecting);
      if (canvasVisible) {
        previousTime = performance.now();
        requestRenderRef.current?.();
      } else if (animationFrame) {
        window.cancelAnimationFrame(animationFrame);
        animationFrame = 0;
      }
    });
    const handleVisibilityChange = () => {
      pageVisible = document.visibilityState !== "hidden";
      if (pageVisible) {
        previousTime = performance.now();
        requestRenderRef.current?.();
      } else if (animationFrame) {
        window.cancelAnimationFrame(animationFrame);
        animationFrame = 0;
      }
    };
    const surfaceObserver = new MutationObserver(() => {
      const visible = surfaceIsVisible();
      if (surfaceVisible === visible) return;
      surfaceVisible = visible;
      window.cancelAnimationFrame(animationFrame);
      animationFrame = 0;
      if (surfaceVisible) {
        previousTime = performance.now();
        lastRenderTime = 0;
        resizeAndDraw();
      }
    });
    for (let ancestor = visibilityRoot; ancestor; ancestor = ancestor.parentElement) {
      surfaceObserver.observe(ancestor, { attributes: true, attributeFilter: ["hidden", "aria-hidden"] });
    }
    observer.observe(canvas);
    if (sizeProbeRef.current?.current) observer.observe(sizeProbeRef.current.current);
    if (sceneSurfaceRef.current?.current) observer.observe(sceneSurfaceRef.current.current);
    intersectionObserver.observe(canvas);
    window.addEventListener("resize", resizeAndDraw);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "data-theme", "style"],
    });
    canvas.addEventListener("webglcontextlost", loseContext);
    resize();
    draw(performance.now());

    return () => {
      disposed = true;
      requestRenderRef.current = null;
      rippleTarget?.removeEventListener("pointerdown", startRipple);
      formationObserver.disconnect();
      observer.disconnect();
      intersectionObserver.disconnect();
      themeObserver.disconnect();
      surfaceObserver.disconnect();
      window.removeEventListener("resize", resizeAndDraw);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      canvas.removeEventListener("webglcontextlost", loseContext);
      window.cancelAnimationFrame(animationFrame);
      gl.deleteVertexArray(vertexArray);
      gl.deleteProgram(program);
    };
  }, [failure, staticScene]);

  useEffect(() => {
    requestRenderRef.current?.();
  }, [activity, ambient, phase, quietField, sphere]);

  useEffect(() => {
    if (ambient && failure) onDispersedRef.current?.();
  }, [ambient, failure]);

  if (failure) return <div className="voice-orb-scene-failure">{failure}</div>;
  return <canvas aria-hidden="true" className="voice-orb-canvas" ref={canvasRef} />;
}

function createProgram(gl: WebGL2RenderingContext, vertexSource: string, fragmentSource: string): WebGLProgram {
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  if (!program) {
    gl.deleteShader(vertex);
    gl.deleteShader(fragment);
    throw new Error("The voice scene could not create its shader program");
  }
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const message = gl.getProgramInfoLog(program) || "The voice scene shader could not be linked";
    gl.deleteProgram(program);
    throw new Error(message);
  }
  return program;
}

function compileShader(gl: WebGL2RenderingContext, kind: number, source: string): WebGLShader {
  const shader = gl.createShader(kind);
  if (!shader) throw new Error("The voice scene could not allocate a shader");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const message = gl.getShaderInfoLog(shader) || "The voice scene shader could not be compiled";
    gl.deleteShader(shader);
    throw new Error(message);
  }
  return shader;
}

function readUniforms(gl: WebGL2RenderingContext, program: WebGLProgram): ShaderUniforms {
  return {
    ambient: requiredUniform(gl, program, "u_ambient"),
    background: requiredUniform(gl, program, "u_background"),
    departing: requiredUniform(gl, program, "u_departing"),
    quietField: requiredUniform(gl, program, "u_quiet_field"),
    error: requiredUniform(gl, program, "u_error"),
    formTiming: requiredUniform(gl, program, "u_form_timing"),
    forms: requiredUniform(gl, program, "u_forms"),
    pixelRatio: requiredUniform(gl, program, "u_pixel_ratio"),
    reveal: requiredUniform(gl, program, "u_reveal"),
    resolution: requiredUniform(gl, program, "u_resolution"),
    scene: requiredUniform(gl, program, "u_scene"),
    orbCenter: requiredUniform(gl, program, "u_orb_center"),
    ripples: requiredUniform(gl, program, "u_ripples"),
    speaking: requiredUniform(gl, program, "u_speaking"),
    speechTime: requiredUniform(gl, program, "u_speech_time"),
    sphere: requiredUniform(gl, program, "u_sphere"),
    thinking: requiredUniform(gl, program, "u_thinking"),
    thoughtTime: requiredUniform(gl, program, "u_thought_time"),
    time: requiredUniform(gl, program, "u_time"),
    voice: requiredUniform(gl, program, "u_voice"),
  };
}

function readBackgroundColor(canvas: HTMLCanvasElement): [number, number, number] {
  const host = canvas.closest<HTMLElement>(".voice-view") ?? canvas;
  const values = getComputedStyle(host).backgroundColor.match(/[\d.]+/g)?.map(Number);
  if (!values || values.length < 3) return [24 / 255, 24 / 255, 24 / 255];
  return [values[0] / 255, values[1] / 255, values[2] / 255];
}

function requiredUniform(gl: WebGL2RenderingContext, program: WebGLProgram, name: string): WebGLUniformLocation {
  const location = gl.getUniformLocation(program, name);
  if (!location) throw new Error(`The voice scene shader is missing ${name}`);
  return location;
}

function resolveActivity(activity: VoiceOrbActivity, phase: VoicePhase, time: number): ResolvedActivity {
  if (activity.source === "live") {
    const voice = speechEnergy(activity.speakingLevel);
    const listening = phase === "listening" ? clampUnit(activity.listeningLevel * 1.6) * 0.6 : 0;
    return {
      speaking: smoothStep(0.12, 0.4, voice),
      thinking: Math.max(clampUnit(activity.thinkingLevel), listening),
      voice,
    };
  }
  if (phase === "error") return { speaking: 0, thinking: 0, voice: 0 };
  if (phase === "idle") {
    const speaking = clampUnit(0.88 + Math.sin(time * 3.8) * 0.09 + Math.sin(time * 6.4 + 0.7) * 0.07);
    return {
      speaking,
      thinking: clampUnit(0.72 + Math.sin(time * 0.74) * 0.08),
      voice: speaking,
    };
  }
  if (phase === "requesting") return { speaking: 0.22, thinking: 0.86, voice: 0.22 };

  const cycle = time % 12;
  const voiceWindow = smoothStep(2.7, 3.65, cycle) * (1 - smoothStep(8.05, 9.1, cycle));
  const voiceTexture = 0.56
    + Math.sin(time * 4.7) * 0.18
    + Math.sin(time * 7.1 + 0.8) * 0.12
    + Math.sin(time * 2.3 + 2.1) * 0.08;
  const thinking = 0.34
    + (1 - voiceWindow) * 0.36
    + Math.sin(time * 0.73) * 0.08
    + Math.sin(time * 1.17 + 1.4) * 0.05;
  const speaking = clampUnit(0.2 + voiceWindow * voiceTexture * 0.8);
  return {
    speaking,
    thinking: clampUnit(thinking),
    voice: speaking,
  };
}

function speechEnergy(meterLevel: number): number {
  const rms = clampUnit(meterLevel) / 4.8;
  if (rms <= 0) return 0;
  return clampUnit((20 * Math.log10(rms) + 48) / 30);
}

function approach(current: number, target: number, elapsed: number, speed: number): number {
  return current + (target - current) * (1 - Math.exp(-elapsed * speed));
}

function clampUnit(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function smoothStep(start: number, end: number, value: number): number {
  const normalized = clampUnit((value - start) / (end - start));
  return normalized * normalized * (3 - 2 * normalized);
}
