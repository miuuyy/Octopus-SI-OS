const TASK_LIMIT = 8;
const TRIGGER_LIMIT = 8;
const PROJECT_LIMIT = 12;
const AGENT_LIMIT = 12;
const TITLE_CHARS = 72;
const REPLY_CHARS = 160;
const GOAL_CHARS = 96;
const INSTRUCTION_CHARS = 72;
const ERROR_CHARS = 200;
const TEXT_BYTES = 12 * 1024;

const HOME_STATE_PROJECTIONS = [
  "projection.operator.chats.list",
  "projection.scheduled_interfaces.list",
  "projection.operator.os.scope",
];

async function homeStateClientContext(readProjection, nowMs) {
  if (typeof readProjection !== "function") throw new Error("Home state requires a trusted projection reader");
  if (!Number.isSafeInteger(nowMs) || nowMs <= 0) throw new Error("Home state requires an exact current time");
  let text;
  try {
    const [chats, scheduled, scope] = await Promise.all(HOME_STATE_PROJECTIONS.map((method) => readProjection(method)));
    text = renderHomeState({
      chats: list(chats?.chats, "chats"),
      triggers: list(scheduled?.scheduled_interfaces, "scheduled_interfaces"),
      scope: scope && typeof scope === "object" ? scope : {},
      nowMs,
    });
  } catch (error) {
    text = `Home state could not be read at ${new Date(nowMs).toISOString()}: ${clip(error?.message ?? error, ERROR_CHARS)}. Use list_runs and file.read for current state.`;
  }
  return { name: "home_state", text };
}

function renderHomeState({ chats, triggers, scope, nowMs }) {
  const projects = projectRows(scope);
  const projectName = (eeId) => projects.get(eeId)?.name ?? "";
  const lines = [`Snapshot ${new Date(nowMs).toISOString()}. Detail on demand: list_runs, followup_task, file.read, client.mcp list_widgets.`];
  if (text(scope.status) && scope.status !== "ready") lines.push(`Client scope: ${scope.status}.`);

  const tasks = chats.filter((chat) => chat?.client_surface_role === "task" && (chat.writable === true || chat.archived === true));
  const active = tasks.filter((chat) => chat.archived !== true);
  const archived = tasks.length - active.length;
  const failed = active.filter((chat) => taskState(chat) === "failed").length;
  const working = active.filter((chat) => taskState(chat) === "working").length;
  if (active.length === 0) {
    lines.push(`Tasks: none active${archived ? `; ${archived} archived` : ""}.`);
  } else {
    const counts = [`${active.length} active`, working && `${working} working`, failed && `${failed} failed`, archived && `${archived} archived`].filter(Boolean);
    lines.push(`Tasks (${counts.join(", ")}; newest first):`);
    for (const chat of active.slice(0, TASK_LIMIT)) {
      const parts = [quote(chat.title || chat.run_id, TITLE_CHARS), taskState(chat)];
      const project = chat.ee_id && chat.ee_id !== scope.root_ee_id ? projectName(chat.ee_id) || clip(chat.ee_goal, GOAL_CHARS) : "";
      if (project) parts.push(`project ${project}`);
      if (text(chat.agent_name)) parts.push(`agent ${clip(chat.agent_name, TITLE_CHARS)}`);
      if (Number(chat.updated_at_ms) > 0) parts.push(age(chat.updated_at_ms, nowMs));
      parts.push(text(chat.run_id));
      lines.push(`- ${parts.join(" · ")}`);
      const reply = text(chat.last_terminal_message?.text);
      if (reply) lines.push(`  last reply: ${quote(reply, REPLY_CHARS)}`);
    }
    if (active.length > TASK_LIMIT) lines.push(`- +${active.length - TASK_LIMIT} more`);
  }

  const armed = triggers.filter((trigger) => trigger?.template !== true);
  if (armed.length === 0) {
    lines.push("Triggers: none.");
  } else {
    lines.push(`Triggers (${armed.length}):`);
    for (const trigger of armed.slice(0, TRIGGER_LIMIT)) lines.push(`- ${triggerLine(trigger, nowMs)}`);
    if (armed.length > TRIGGER_LIMIT) lines.push(`- +${armed.length - TRIGGER_LIMIT} more`);
  }

  const projectList = [...projects.values()].filter((project) => project.eeId !== scope.root_ee_id);
  if (projectList.length) {
    lines.push(`Projects: ${projectList.slice(0, PROJECT_LIMIT).map((project) => (
      project.goal && project.goal !== project.name ? `${project.name} (${project.eeId}): ${project.goal}` : `${project.name} (${project.eeId})`
    )).join("; ")}${projectList.length > PROJECT_LIMIT ? `; +${projectList.length - PROJECT_LIMIT} more` : ""}.`);
  }

  const agents = list(scope.agent_presentations, "agent_presentations")
    .filter((agent) => !Number(agent?.deleted_at_ms) && text(agent?.name) && text(agent?.worker_interface));
  if (agents.length) {
    lines.push(`Agents: ${agents.slice(0, AGENT_LIMIT).map((agent) => {
      const project = projectName(text(agent.project_ee_id));
      return `${clip(agent.name, TITLE_CHARS)}${project ? ` in ${project}` : ""} (worker ${text(agent.worker_interface)}, ee ${text(agent.project_ee_id)})`;
    }).join("; ")}${agents.length > AGENT_LIMIT ? `; +${agents.length - AGENT_LIMIT} more` : ""}.`);
  }
  return fitLines(lines, TEXT_BYTES);
}

function fitLines(lines, limit) {
  const kept = [];
  let bytes = 0;
  for (const line of lines) {
    const size = Buffer.byteLength(line, "utf8") + 1;
    if (bytes + size > limit - 64) {
      kept.push(`(${lines.length - kept.length} more lines omitted)`);
      break;
    }
    kept.push(line);
    bytes += size;
  }
  return kept.join("\n");
}

function projectRows(scope) {
  const names = new Map(list(scope.project_presentations, "project_presentations")
    .map((presentation) => [text(presentation?.ee_id), clip(presentation?.name, TITLE_CHARS)]));
  const rows = new Map();
  for (const environment of list(scope.projects, "projects")) {
    const eeId = text(environment?.ee_id);
    if (!eeId) continue;
    const goal = clip(environment.goal, GOAL_CHARS);
    rows.set(eeId, { eeId, name: names.get(eeId) || goal || eeId, goal });
  }
  return rows;
}

function taskState(chat) {
  if (chat.turn_open === true) return "working";
  return text(chat.turn_state) || "idle";
}

function triggerLine(trigger, nowMs) {
  const name = quote(trigger?.display_name || trigger?.interface_id, TITLE_CHARS);
  if (trigger?.valid === false) return `${name} · invalid: ${clip(trigger.error, ERROR_CHARS)}`;
  const parts = [name, scheduleText(trigger?.schedule, trigger?.timezone)];
  if (trigger?.status === "disabled") parts.push("paused");
  if (Number(trigger?.next_run_at_ms) > 0) parts.push(`next ${until(trigger.next_run_at_ms, nowMs)}`);
  if (Number(trigger?.last_run_at_ms) > 0) parts.push(`last ${age(trigger.last_run_at_ms, nowMs)}`);
  const targets = Array.isArray(trigger?.targets) ? trigger.targets.map((target) => text(target?.title) || text(target?.run_id)).filter(Boolean) : [];
  if (targets.length) parts.push(`wakes ${targets.join(", ")}`);
  if (text(trigger?.instructions)) parts.push(`prompt ${quote(trigger.instructions, INSTRUCTION_CHARS)}`);
  return parts.join(" · ");
}

function scheduleText(schedule, timezone) {
  const zone = text(timezone) ? ` ${timezone}` : "";
  switch (schedule?.type) {
    case "interval": return `every ${duration(schedule.interval_seconds)}`;
    case "cron": return `cron ${text(schedule.cron)}${zone}`;
    case "once": return `once at ${text(schedule.due_at)}${zone}`;
    case "inactivity": return `after ${duration(schedule.seconds)} of quiet`;
    default: return "unrecognized schedule";
  }
}

function duration(seconds) {
  const value = Number(seconds);
  if (!Number.isFinite(value) || value <= 0) return "an unknown period";
  if (value % 86400 === 0) return `${value / 86400} d`;
  if (value % 3600 === 0) return `${value / 3600} h`;
  if (value % 60 === 0) return `${value / 60} min`;
  return `${value} s`;
}

function age(ms, nowMs) {
  return relative(nowMs - Number(ms), "ago");
}

function until(ms, nowMs) {
  const delta = Number(ms) - nowMs;
  return delta <= 0 ? "due now" : `in ${relative(delta, "")}`.trim();
}

function relative(deltaMs, suffix) {
  const minutes = Math.floor(Math.max(0, deltaMs) / 60000);
  const unit = minutes < 1 ? "under 1 min"
    : minutes < 60 ? `${minutes} min`
      : minutes < 2880 ? `${Math.floor(minutes / 60)} h`
        : `${Math.floor(minutes / 1440)} d`;
  return suffix ? `${unit} ${suffix}` : unit;
}

function quote(value, limit) {
  return JSON.stringify(clip(value, limit));
}

function clip(value, limit) {
  const normalized = text(value).replace(/\s+/g, " ");
  return normalized.length > limit ? `${normalized.slice(0, limit - 1)}…` : normalized;
}

function text(value) {
  return typeof value === "string" ? value.trim() : "";
}

function list(value, field) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error(`Home state projection field '${field}' is not a list`);
  return value;
}

module.exports = { HOME_STATE_PROJECTIONS, homeStateClientContext, renderHomeState };
