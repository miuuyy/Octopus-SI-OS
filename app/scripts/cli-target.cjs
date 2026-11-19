const TARGETS = Object.freeze({
  "aarch64-apple-darwin": Object.freeze({ platform: "darwin", architecture: "arm64" }),
  "x86_64-apple-darwin": Object.freeze({ platform: "darwin", architecture: "x64" }),
  "aarch64-unknown-linux-gnu": Object.freeze({ platform: "linux", architecture: "arm64" }),
  "x86_64-unknown-linux-gnu": Object.freeze({ platform: "linux", architecture: "x64" }),
});
const SUPPORTED_TARGETS = Object.freeze(Object.keys(TARGETS).sort());

const COMPONENTS = Object.freeze({
  darwin: Object.freeze({ client_filesystem: true, desktop_native: true, managed_codex: true, code_signing: true }),
  linux: Object.freeze({ client_filesystem: false, desktop_native: false, managed_codex: true, code_signing: false }),
});

function hostTarget(platform = process.platform, architecture = process.arch) {
  const found = SUPPORTED_TARGETS.find((triple) => TARGETS[triple].platform === platform && TARGETS[triple].architecture === architecture);
  if (!found) throw new Error(`The Octopus CLI distribution is not built on ${platform}-${architecture}; supported targets: ${SUPPORTED_TARGETS.join(", ")}`);
  return found;
}

function resolveCliTarget({ platform = process.platform, architecture = process.arch, target = null } = {}) {
  const host = hostTarget(platform, architecture);
  if (target !== null && !Object.hasOwn(TARGETS, target)) {
    throw new Error(`Unknown CLI target '${target}'; supported targets: ${SUPPORTED_TARGETS.join(", ")}`);
  }
  if (target !== null && target !== host) {
    throw new Error(`CLI target ${target} cannot be built on ${host}: a distribution is built on the machine it targets`);
  }
  return Object.freeze({ triple: host, ...TARGETS[host], components: COMPONENTS[platform] });
}

function takeTargetArgument(argv) {
  let target = null;
  const rest = [];
  for (const argument of argv) {
    if (!argument.startsWith("--target")) { rest.push(argument); continue; }
    const value = argument.startsWith("--target=") ? argument.slice("--target=".length) : "";
    if (!value) throw new Error("--target requires a value, as --target=<triple>");
    if (target !== null) throw new Error("--target is given more than once");
    target = value;
  }
  return { target, rest };
}

module.exports = { SUPPORTED_TARGETS, hostTarget, resolveCliTarget, takeTargetArgument };
