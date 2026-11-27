"use strict";

const TITLEBAR_HEIGHT = 48;
const NATIVE_CONTROL_HEIGHT = 14;
const NATIVE_CONTROL_INSET = 76;

function nativeTrafficLightPosition(zoomFactor) {
  if (!Number.isFinite(zoomFactor) || zoomFactor <= 0) throw new Error("Window zoom must be positive");
  return { x: 16, y: Math.round((TITLEBAR_HEIGHT * zoomFactor - NATIVE_CONTROL_HEIGHT) / 2) };
}

function nativeControlInset(zoomFactor) {
  if (!Number.isFinite(zoomFactor) || zoomFactor <= 0) throw new Error("Window zoom must be positive");
  return NATIVE_CONTROL_INSET / zoomFactor;
}

module.exports = {
  TITLEBAR_HEIGHT,
  NATIVE_CONTROL_HEIGHT,
  NATIVE_CONTROL_INSET,
  nativeTrafficLightPosition,
  nativeControlInset,
};
