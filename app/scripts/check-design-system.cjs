#!/usr/bin/env node
const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "..", "src");
const foundationCss = fs.readFileSync(path.join(root, "foundation.css"), "utf8");
const statusCss = fs.readFileSync(path.join(root, "design-system", "status.css"), "utf8");
const blockedPatterns = [
  {
    pattern: /components\/ui\/primitives/,
    message: "import the first-party design system from src/design-system instead of components/ui/primitives",
  },
  {
    pattern: /\bui-(control-button|icon-button|segmented|status-pill|surface-header|kbd)\b/,
    message: "legacy ui-* control class found; use oc-* design-system primitives",
  },
  {
    pattern: /\b(toolbar-button|icon-button|segmented)\b/,
    message: "shared control anatomy must use OcButton/OcIconButton/OcSegmented; local classes may only position them",
    tsxOnly: true,
  },
];

function walk(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) return walk(full);
    return full;
  });
}

const failures = [];
const cssFiles = walk(root).filter((file) => file.endsWith(".css"));
const radiusTokens = new Set(cssFiles.flatMap((file) => [...fs.readFileSync(file, "utf8").matchAll(/(--radius[\w-]*)\s*:/g)].map((match) => match[1])));
for (const file of cssFiles) {
  const source = fs.readFileSync(file, "utf8");
  for (const match of source.matchAll(/var\(\s*(--radius[\w-]*)\s*\)/g)) {
    if (!radiusTokens.has(match[1])) failures.push(`${path.relative(path.resolve(__dirname, ".."), file)}: undefined corner radius ${match[1]}`);
  }
}
if (!foundationCss.includes("@layer octopus-foundation, properties, theme, base, components, utilities;")) {
  failures.push("src/foundation.css: components must remain after base in the global cascade layer order");
}
const entryHtml = fs.readFileSync(path.resolve(__dirname, "../index.html"), "utf8");
const cascadeOrder = entryHtml.indexOf("@layer octopus-foundation, properties, theme, base, components, utilities;");
const firstStylesheet = entryHtml.search(/<link\b[^>]*rel=["']stylesheet["']|<script\b[^>]*type=["']module["']/);
if (cascadeOrder < 0 || (firstStylesheet >= 0 && cascadeOrder > firstStylesheet)) {
  failures.push("index.html: register the global cascade before stylesheets and module dependencies");
}
if (!foundationCss.includes('@import "./design-system/status.css" layer(components);')) {
  failures.push("src/foundation.css: OcStatus must load in the components layer after Tailwind base");
}
if (!/\.oc-status\s*\{[\s\S]*?padding:\s*4px 8px;[\s\S]*?border-radius:\s*6px;/.test(statusCss)) {
  failures.push("src/design-system/status.css: a neutral OcStatus must stay an outlined rounded-sm badge");
}
if (!/\.oc-status\.oc-tone-danger\s*\{[\s\S]*?min-height:\s*20px;[\s\S]*?border-radius:\s*999px;/.test(statusCss)) {
  failures.push("src/design-system/status.css: a toned OcStatus must stay a 20px soft pill");
}
if (statusCss.includes("oc-status__dot") || fs.readFileSync(path.join(root, "design-system", "components.tsx"), "utf8").includes("oc-status__dot")) {
  failures.push("src/design-system: OcStatus carries its state in the badge colour, not a second dot");
}
for (const file of walk(root)) {
  if (!/\.(tsx?|css)$/.test(file)) continue;
  if (file.includes(`${path.sep}design-system${path.sep}`)) continue;
  const rel = path.relative(path.resolve(__dirname, ".."), file);
  const text = fs.readFileSync(file, "utf8");
  for (const rule of blockedPatterns) {
    if (rule.tsxOnly && !/\.tsx$/.test(file)) continue;
    const matches = text.match(rule.pattern);
    if (!matches) continue;
    failures.push(`${rel}: ${rule.message}`);
  }
}

if (failures.length > 0) {
  console.error("Design-system contract failed:");
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}
