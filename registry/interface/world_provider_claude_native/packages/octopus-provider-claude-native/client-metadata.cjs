const { execFile, spawn } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const SIGN_IN_TIMEOUT_MS = 300_000;

function claudeExecutable(descriptor) {
  const config = descriptor.interface_driver_config?.private_config;
  const declared = config?.sdk_child?.claude_executable;
  if (descriptor.interface_driver_ref !== "claude_agent_sdk" || typeof declared !== "string" || !declared) {
    throw new Error(`Provider '${descriptor.id}' does not use the Claude Code account.`);
  }
  if (declared.includes("/")) {
    if (!path.isAbsolute(declared)) throw new Error("The Claude Code executable must be a bare command or an absolute path.");
    return declared;
  }
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    if (!path.isAbsolute(directory)) continue;
    const candidate = path.join(directory, declared);
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      if (fs.statSync(candidate).isFile()) return candidate;
    } catch {}
  }
  throw new Error(`Claude Code command '${declared}' is not on PATH.`);
}

function authStatus(executable) {
  return new Promise((resolve, reject) => {
    execFile(executable, ["auth", "status", "--json"], { timeout: 10_000, maxBuffer: 1 << 20 }, (error, stdout) => {
      if (error && error.code !== 1) return reject(new Error(`Claude Code account status failed: ${error.message}`));
      let status;
      try { status = JSON.parse(stdout); } catch { return reject(new Error("Claude Code account status is not JSON.")); }
      if (typeof status?.loggedIn !== "boolean") return reject(new Error("Claude Code account status has no loggedIn field."));
      resolve({ signedIn: status.loggedIn, authMethod: typeof status.authMethod === "string" ? status.authMethod : null });
    });
  });
}

function declaredCatalog(descriptor) {
  const config = descriptor.interface_driver_config?.private_config;
  if (!Array.isArray(config?.model_catalog)) throw new Error(`Provider '${descriptor.id}' has no declared model catalogue.`);
  return config.model_catalog.map((entry) => ({
    model: entry?.model,
    displayName: entry?.display_name,
    description: entry?.description ?? "",
    defaultReasoningEffort: entry?.default_reasoning_effort ?? "",
    supportedReasoningEfforts: Array.isArray(entry?.supported_reasoning_efforts)
      ? entry.supported_reasoning_efforts.map((effort) => (typeof effort === "string" ? effort : effort?.effort))
      : undefined,
    showInPicker: entry?.show_in_picker,
    isDefault: entry?.model === config.model,
  }));
}

module.exports = {
  schemaVersion: 1,
  async modelCatalog({ descriptor }) {
    claudeExecutable(descriptor);
    return declaredCatalog(descriptor);
  },
  async probeAccount({ descriptor, reference }) {
    const status = await authStatus(claudeExecutable(descriptor));
    return status.signedIn
      ? [reference("provider_observation", descriptor.id, { source: "claude.auth_status", signed_in: true, auth_method: status.authMethod })]
      : null;
  },
  async signIn({ descriptor }) {
    const executable = claudeExecutable(descriptor);
    if ((await authStatus(executable)).signedIn) return;
    await new Promise((resolve, reject) => {
      const login = spawn(executable, ["auth", "login", "--claudeai"], { stdio: ["ignore", "inherit", "inherit"] });
      const timer = setTimeout(() => {
        login.kill("SIGTERM");
        reject(new Error("Claude Code sign-in was not completed in time."));
      }, SIGN_IN_TIMEOUT_MS);
      login.on("error", (error) => { clearTimeout(timer); reject(error); });
      login.on("exit", (code) => {
        clearTimeout(timer);
        if (code === 0) resolve();
        else reject(new Error(`Claude Code sign-in exited with status ${code}.`));
      });
    });
    if (!(await authStatus(executable)).signedIn) throw new Error("Claude Code reports no signed-in account after sign-in.");
  },
};
