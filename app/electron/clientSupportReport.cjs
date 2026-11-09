const path = require("node:path");
const { randomUUID } = require("node:crypto");
const { readBounded, sourceFailure } = require("./clientSupportFiles.cjs");
const { collectRuntimeReport } = require("./clientRuntimeReport.cjs");

const STEPS = new Set(["packages", "extensions", "workspace", "provider"]);
const PHASES = new Set(["verifying", "applying", "complete", "sealed", "failed"]);
const EVENTS = new Set(["setup.progress", "setup.failed", "workspace.setup.failed", "workspace.setup.resume_failed", "client.renderer.gone", "client.renderer.unresponsive", "client.renderer.responsive", "renderer.boundary_failure", "renderer.chat_surface_failure", "renderer.surface_failure", "renderer.voice_closure", "renderer.voice_startup", "renderer.topology.read_failed", "renderer.toast", "client.command"]);
const MAX_LOG_BYTES = 256 * 1024;

function time(value) {
  return typeof value === "string" && /^\d{4}-\d\d-\d\dT[\d:.]+Z$/.test(value) && Number.isFinite(Date.parse(value)) ? value : null;
}

function setupProgress(value) {
  if (!STEPS.has(value?.step) || !PHASES.has(value?.phase)) return null;
  return { step: value.step, phase: value.phase, percent: Number.isFinite(value.percent) ? Math.max(0, Math.min(100, value.percent)) : null };
}

async function collectSupportReport({ application, profile, clientRoot, home, runtime, progress, includeDetails = false, now = Date.now() }) {
  const report = {
    schema: "octopus.client_support_report",
    schema_version: 2,
    report_id: randomUUID(),
    captured_at: new Date(now).toISOString(),
    application,
    access: profile ?? "locked",
    disclosure: includeDetails ? "includes_error_text_review_before_sharing" : "technical_metadata_only",
    limitations: ["Bounded local observations, not an atomic snapshot or a recovery verdict. Missing, changed and omitted evidence is reported.", "Runtime evidence contains technical fields and report-local identifiers. Conversation text, tool arguments and results, project files, credentials files and environment variables are excluded.", "Nothing is uploaded. Explicitly included error text and stderr excerpts can contain private data; review them before sharing."],
  };
  if (profile !== "live") return report;
  if (home && runtime?.home !== home) runtime = null;
  report.runtime = runtime ? {
    phase: ["probing", "ready", "unmanaged", "degraded", "registry_install", "registry_ready", "state_ready", "starting_runtime", "stopping", "access_required"].includes(runtime.phase) ? runtime.phase : "unknown",
    ready: runtime.ready === true,
    observed_at: Number.isFinite(runtime.updatedAtMs) ? new Date(runtime.updatedAtMs).toISOString() : null,
    ...(includeDetails && runtime.error ? { error: String(runtime.error).slice(0, 8000) } : {}),
  } : { phase: "not_observed" };
  report.setup = { current_attempt: progress ? { ...setupProgress(progress), state: ["running", "paused", "complete", "failed"].includes(progress.state) ? progress.state : "unknown", observed_at: time(progress.at), seconds_since_progress: Math.max(0, Math.floor((now - Date.parse(progress.at)) / 1000)) } : null };
  try {
    const current = JSON.parse((await readBounded(clientRoot, "setup-state/current.json", 16384)).text);
    if (!/^[A-Za-z0-9_-]{8,96}$/.test(current.operation_id)) throw new Error("invalid_setup_identity");
    const file = `setup-state/operations/${current.operation_id}.json`;
    const operation = JSON.parse((await readBounded(clientRoot, file, 2 * 1024 * 1024)).text);
    if (operation.operation_id !== current.operation_id || operation.plan_digest !== current.plan_digest) throw new Error("setup_identity_mismatch");
    report.setup.journal = {
      source: "observed",
      recorded_at: time(operation.completed_at ?? operation.updated_at),
      steps: Array.isArray(operation.steps) ? operation.steps.filter((s) => STEPS.has(s?.step)).map((s) => ({ step: s.step, state: s.state === "complete" ? "complete" : "pending" })) : [],
      last_error: operation.last_error ? {
        step: STEPS.has(operation.last_error.step) ? operation.last_error.step : "unknown",
        at: time(operation.last_error.observed_at),
        ...(includeDetails ? { message: String(operation.last_error.message ?? "").slice(0, 8000) } : {}),
      } : null,
    };
    try {
      const receipt = JSON.parse((await readBounded(clientRoot, `setup-state/receipts/${current.operation_id}.json`, 2 * 1024 * 1024)).text);
      if (receipt.operation_id !== current.operation_id || receipt.plan_digest !== current.plan_digest) throw new Error("setup_identity_mismatch");
      report.setup.journal.receipt = { source: "observed", completed_at: time(receipt.completed_at) };
    } catch (error) { report.setup.journal.receipt = { source: sourceFailure(error) }; }
  } catch (error) {
    report.setup.journal = { source: sourceFailure(error) };
  }
  const events = [];
  report.client_logs = { sources: [], events };
  for (const name of ["octopus-client.jsonl.1", "octopus-client.jsonl"]) {
    try {
      const source = await readBounded(clientRoot, path.join("logs", name), MAX_LOG_BYTES, true);
      let malformed = 0;
      for (const line of source.text.split("\n").filter(Boolean)) {
        let entry;
        try { entry = JSON.parse(line); } catch { malformed++; continue; }
        const event = EVENTS.has(entry.event) ? entry.event : "client.event";
        const progress = entry.event === "setup.progress" ? setupProgress(entry.detail) : null;
        const sourceSequence = /\bmovement seq (\d+)\b/.exec(String(entry.detail?.message ?? ""));
        events.push({ at: time(entry.at), level: ["info", "warn", "error"].includes(entry.level) ? entry.level : "info", event,
          ...(progress ? { progress } : {}),
          ...(sourceSequence && Number.isSafeInteger(Number(sourceSequence[1])) ? { source_movement_seq: Number(sourceSequence[1]) } : {}),
          ...(includeDetails && (entry.level === "error" || entry.level === "warn") ? { message: String(entry.detail?.message ?? "").slice(0, 8000) } : {}),
        });
      }
      report.client_logs.sources.push({ file: name, state: "read", truncated: source.truncated, malformed_lines: malformed });
    } catch (error) {
      report.client_logs.sources.push({ file: name, state: sourceFailure(error) });
    }
  }
  const failures = events.filter(event => event.level === "error" || event.level === "warn");
  const selected = new Set([...failures.slice(0, 8), ...failures.slice(-40), ...events.filter(event => event.level === "info").slice(-24)]);
  report.client_logs.events = events.filter(event => selected.has(event));
  report.client_logs.omitted_events = events.length - report.client_logs.events.length;
  report.runtime_evidence = home ? await collectRuntimeReport({ home, includeDetails,
    sourceSequences: [...new Set(events.flatMap(event => event.source_movement_seq === undefined ? [] : [event.source_movement_seq]))],
    interfaceFailures: runtime?.interfaceFailures ?? [],
  }) : { source: "not_selected" };
  return report;
}

function supportReportText(report, description) {
  return `# Octopus problem report\n\n${String(description ?? "").slice(0, 4000).trim() || "No description provided."}\n\n\`\`\`json\n${JSON.stringify(report, null, 2)}\n\`\`\`\n`;
}

function supportIssueUrl(report, description) {
  const url = new URL("https://github.com/miuuyy/Octopus-SI-OS/issues/new");
  const text = String(description ?? "").trim();
  url.searchParams.set("title", text.split("\n")[0].slice(0, 100) || "Octopus problem report");
  url.searchParams.set("body", `${text.slice(0, 1000) || "Describe what happened and what you expected."}${text.length > 1000 ? "\n[Description continues in the saved report.]" : ""}\n\nVersion: ${report.application.version}\nBuild: ${report.application.revision ?? "not recorded"}\nPlatform: ${report.application.platform} ${report.application.arch}\nReport: ${report.report_id}\n\nAttach the saved diagnostic report after reviewing it. Do not include credentials or private conversations.`);
  return url.toString();
}

module.exports = { collectSupportReport, supportReportText, supportIssueUrl };
