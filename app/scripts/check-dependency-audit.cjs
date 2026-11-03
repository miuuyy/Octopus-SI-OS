const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const repoRoot = path.resolve(__dirname, "../..");
const configRelative = "app/dependency-audit.json";
const gatingSeverities = new Set(["high", "critical"]);
const advisoryPattern = /^GHSA(?:-[a-z0-9]{4}){3}$/;

function main() {
  if (process.argv.length > 2) throw new Error(`unknown arguments: ${process.argv.slice(2).join(", ")}`);
  const config = readConfig();
  const issues = [];
  const matched = new Set();
  for (const { root, shipped } of config.packages) {
    const reported = auditPackage(root, shipped);
    for (const finding of reported) {
      const key = acceptanceKey(finding);
      if (config.accepted.has(key)) matched.add(key);
      else issues.push(`${root}: ${finding.severity} ${finding.advisory} in ${finding.dependency} (${finding.title}) is not accepted in ${configRelative}`);
    }
    const accepted = [...config.accepted.values()].filter((entry) => entry.root === root).length;
    const scope = shipped.length > 0 ? `production dependencies and shipped ${shipped.join(", ")}` : "production dependencies";
    process.stdout.write(`${root}: ${reported.length} high or critical advisories in ${scope}, ${accepted} accepted\n`);
  }
  for (const [key, entry] of config.accepted) {
    if (!matched.has(key)) issues.push(`${entry.root}: accepted ${entry.advisory} in ${entry.dependency} is no longer reported; remove it from ${configRelative}`);
  }
  if (issues.length > 0) throw new Error(`dependency audit has ${issues.length} issue(s):\n${issues.map((issue) => `- ${issue}`).join("\n")}`);
  process.stdout.write("Dependency audit OK\n");
}

function readConfig() {
  const value = JSON.parse(fs.readFileSync(path.join(repoRoot, configRelative), "utf8"));
  requireExactKeys(value, ["packages", "accepted"], configRelative);
  if (!Array.isArray(value.packages) || value.packages.length === 0) throw new Error(`${configRelative} must declare at least one package root`);
  const packages = [];
  for (const entry of value.packages) {
    requireExactKeys(entry, ["root", "shippedDevDependencies"], `${configRelative} package`);
    const { root } = entry;
    if (typeof root !== "string" || root.length === 0 || path.isAbsolute(root) || root.split("/").includes("..")) {
      throw new Error(`${configRelative} has an invalid package root: ${JSON.stringify(root)}`);
    }
    if (packages.some((pkg) => pkg.root === root)) throw new Error(`${configRelative} declares ${root} twice`);
    const lockPath = path.join(repoRoot, root, "package-lock.json");
    if (!fs.statSync(lockPath, { throwIfNoEntry: false })?.isFile()) throw new Error(`${root} has no package-lock.json to audit`);
    const manifest = JSON.parse(fs.readFileSync(path.join(repoRoot, root, "package.json"), "utf8"));
    const lock = JSON.parse(fs.readFileSync(lockPath, "utf8"));
    if (!Array.isArray(entry.shippedDevDependencies)) throw new Error(`${configRelative} ${root} shippedDevDependencies must be an array`);
    const shipped = [];
    for (const name of entry.shippedDevDependencies) {
      if (typeof name !== "string" || name.length === 0) throw new Error(`${configRelative} ${root} has an invalid shipped dev dependency: ${JSON.stringify(name)}`);
      if (shipped.includes(name)) throw new Error(`${configRelative} ${root} declares shipped ${name} twice`);
      if (typeof manifest.devDependencies?.[name] !== "string") throw new Error(`${root}: shipped ${name} is not a devDependency in package.json`);
      if (!lock.packages?.[`node_modules/${name}`]) throw new Error(`${root}: shipped ${name} is not in package-lock.json`);
      shipped.push(name);
    }
    packages.push({ root, shipped });
  }
  if (!Array.isArray(value.accepted)) throw new Error(`${configRelative} accepted must be an array`);
  const accepted = new Map();
  for (const entry of value.accepted) {
    requireExactKeys(entry, ["root", "dependency", "advisory", "reason"], `${configRelative} accepted entry`);
    if (!packages.some((pkg) => pkg.root === entry.root)) throw new Error(`${configRelative} accepts an advisory for undeclared root ${JSON.stringify(entry.root)}`);
    if (typeof entry.dependency !== "string" || entry.dependency.length === 0) throw new Error(`${configRelative} accepted entry needs a dependency name`);
    if (typeof entry.advisory !== "string" || !advisoryPattern.test(entry.advisory)) throw new Error(`${configRelative} accepted entry needs a GHSA advisory id`);
    if (typeof entry.reason !== "string" || entry.reason.trim().length === 0) throw new Error(`${configRelative} accepts ${entry.advisory} without a reason`);
    const key = acceptanceKey(entry);
    if (accepted.has(key)) throw new Error(`${configRelative} accepts ${entry.advisory} in ${entry.dependency} for ${entry.root} twice`);
    accepted.set(key, entry);
  }
  return { packages, accepted };
}

function auditPackage(root, shipped) {
  const findings = new Map();
  for (const finding of npmAudit(root, ["--omit=dev"], () => true)) findings.set(acceptanceKey(finding), finding);
  if (shipped.length > 0) {
    for (const finding of npmAudit(root, [], (name) => shipped.includes(name))) findings.set(acceptanceKey(finding), finding);
  }
  return [...findings.values()];
}

function npmAudit(root, flags, gates) {
  const result = spawnSync("npm", ["audit", ...flags, "--json"], {
    cwd: path.join(repoRoot, root),
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) throw new Error(`${root}: npm audit could not start: ${result.error.message}`);
  if (result.status !== 0 && result.status !== 1) throw new Error(`${root}: npm audit exited with ${result.status}: ${result.stderr.trim()}`);
  let report;
  try { report = JSON.parse(result.stdout); }
  catch { throw new Error(`${root}: npm audit did not return a JSON report: ${result.stderr.trim()}`); }
  if (report.error) throw new Error(`${root}: npm audit failed: ${report.error.summary || report.error.code || "unknown error"}`);
  if (report.auditReportVersion !== 2 || !report.vulnerabilities || typeof report.vulnerabilities !== "object") {
    throw new Error(`${root}: npm audit returned an unsupported report format`);
  }
  const findings = [];
  for (const vulnerability of Object.values(report.vulnerabilities)) {
    for (const via of vulnerability.via) {
      if (typeof via !== "object" || !gatingSeverities.has(via.severity) || !gates(via.name)) continue;
      const advisory = typeof via.url === "string" ? via.url.split("/").pop() : "";
      if (!advisoryPattern.test(advisory)) throw new Error(`${root}: ${via.severity} advisory in ${via.name} has no GHSA id: ${via.url}`);
      findings.push({ root, dependency: via.name, advisory, severity: via.severity, title: via.title });
    }
  }
  return findings;
}

function acceptanceKey(entry) {
  return `${entry.root}\0${entry.dependency}\0${entry.advisory}`;
}

function requireExactKeys(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  const actual = Object.keys(value).sort();
  if (actual.join(",") !== [...keys].sort().join(",")) throw new Error(`${label} must have exactly ${keys.join(", ")}`);
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
