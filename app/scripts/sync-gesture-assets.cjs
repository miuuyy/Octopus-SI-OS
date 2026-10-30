const fs = require("node:fs");
const path = require("node:path");

const appRoot = path.resolve(__dirname, "..");
const source = path.join(appRoot, "node_modules", "@mediapipe", "tasks-vision", "wasm");
const destination = path.join(appRoot, "public", "mediapipe", "wasm");
const model = path.join(appRoot, "public", "models", "hand_landmarker.task");

if (!fs.existsSync(source)) {
  throw new Error("@mediapipe/tasks-vision is not installed");
}
if (!fs.existsSync(model)) {
  throw new Error("bundled hand_landmarker.task is missing");
}
const loadedFileset = ["vision_wasm_module_internal.js", "vision_wasm_module_internal.wasm"];
for (const name of loadedFileset) {
  if (!fs.statSync(path.join(source, name), { throwIfNoEntry: false })?.isFile()) {
    throw new Error(`@mediapipe/tasks-vision no longer ships ${name}`);
  }
}
fs.rmSync(destination, { recursive: true, force: true });
fs.mkdirSync(destination, { recursive: true });
for (const name of loadedFileset) fs.copyFileSync(path.join(source, name), path.join(destination, name));
