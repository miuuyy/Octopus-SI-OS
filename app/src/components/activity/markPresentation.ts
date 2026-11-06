import type { GlyphName } from "../IconGlyphs";
import type { ActivityKind, MarkActivityItem } from "./markActivity";

export type ActivityPresentation = { label: string; category: string; glyph: GlyphName; description: string };

const requestKinds: ReadonlySet<ActivityKind> = new Set(["approval", "user_input", "elicitation"]);

const kindPresentation: Record<Exclude<ActivityKind, "mark">, ActivityPresentation> = {
  message: { label: "New reply", category: "Message", glyph: "chat", description: "The task answered. Open it to read the reply and continue." },
  approval: { label: "Approval needed", category: "Request", glyph: "lock", description: "The task is waiting for you to approve or decline an action." },
  user_input: { label: "Your answer is needed", category: "Request", glyph: "user", description: "The task asked a question and is waiting for your answer." },
  elicitation: { label: "Details requested", category: "Request", glyph: "chat", description: "The task needs more information from you to continue." },
};

const markPresentations: Record<string, ActivityPresentation> = {
  "tool.call.allowed": { label: "Tool call allowed", category: "Policy", glyph: "tool", description: "The runtime checked a tool call against the run's formed grant and let it run." },
  "tool.result.corresponds": { label: "Tool result matched its call", category: "Tools", glyph: "tool", description: "A tool result was verified to answer the exact call that requested it." },
  "emit.tool_call.present": { label: "Tool call emitted", category: "Tools", glyph: "tool", description: "The model output contained a well-formed tool call." },
  "memory.experience.admitted": { label: "Experience admitted to memory", category: "Memory", glyph: "memory", description: "A finished run was accepted as an experience for the memory observer to process." },
  "memory.capture.completed": { label: "Memory capture completed", category: "Memory", glyph: "memory", description: "The memory observer processed an admitted experience and kept what later work should not have to rediscover." },
  "operator.approval": { label: "Operator approval recorded", category: "Approvals", glyph: "check", description: "Your approval was recorded and handed back to the run." },
  "operator.input.provided": { label: "Operator answer recorded", category: "Approvals", glyph: "user", description: "Your answer was recorded and handed back to the run." },
  "task.summary.received": { label: "Task summary received", category: "Tasks", glyph: "taskSummary", description: "A task delivered its summary to the run that started it." },
  "coder.build.ok": { label: "Build passed", category: "Code", glyph: "code", description: "The project build verifier passed." },
  "context.recipe.valid": { label: "Context recipe valid", category: "Context", glyph: "layers", description: "The context a run reads was assembled and verified against its recipe." },
  "email.authorization.requested": { label: "Email authorization requested", category: "Access", glyph: "lock", description: "A run asked for permission to use email." },
  "lifecycle.decision.allowed": { label: "Lifecycle decision allowed", category: "Checks", glyph: "check", description: "A lifecycle check allowed the next step." },
  "ocr.text.contains": { label: "OCR text matched", category: "Computer Use", glyph: "desktop", description: "Expected text was recognized on the screen." },
};

const sourcePresentations: Record<string, string> = {
  system_worker_memory_observer: "Memory",
  system_worker_memory_projection: "Memory",
  system_worker_memory_consolidation: "Memory",
  system_worker_operator_home: "Home",
  system_worker_operator_task: "Task",
  system_worker_computer_use: "Computer Use",
};

export function isActivityRequest(item: MarkActivityItem): boolean {
  return requestKinds.has(item.kind);
}

export function activityPresentation(item: MarkActivityItem): ActivityPresentation {
  if (item.kind !== "mark") return kindPresentation[item.kind];
  return markPresentations[item.name] ?? {
    label: sentence(item.name.replaceAll(/[._]+/g, " ")),
    category: "Activity",
    glyph: "activity",
    description: "Octopus recorded this step while your work ran.",
  };
}

export function activityTitle(item: MarkActivityItem): string {
  return activityPresentation(item).label;
}

export function activityHeadline(item: MarkActivityItem): string {
  return item.kind === "mark" ? activityPresentation(item).label : item.sourceTitle ?? activityPresentation(item).label;
}

export function activitySummary(item: MarkActivityItem): string {
  if (item.kind === "mark") return `${activitySource(item)} · ${item.name}`;
  if (item.kind === "message") return item.excerpt ?? activityPresentation(item).label;
  return activityPresentation(item).label;
}

export function activitySource(item: MarkActivityItem): string {
  if (item.agentName) return item.agentName;
  if (item.kind !== "mark" && item.sourceTitle) return item.sourceTitle;
  const original = item.workerOriginal || item.worker;
  if (sourcePresentations[original]) return sourcePresentations[original];
  const readable = original.replace(/^(?:lab_)?system_worker_/, "").replace(/_[0-9a-f]{8,}.*$/, "").replaceAll(/[._]+/g, " ").trim();
  return readable ? sentence(readable) : "Octopus";
}

export type ActivityRowGroup = { id: string; lead: MarkActivityItem; items: MarkActivityItem[] };
export type ActivitySection = { key: string; label: string; rows: ActivityRowGroup[] };

export function activityRows(items: readonly MarkActivityItem[]): ActivityRowGroup[] {
  const rows: ActivityRowGroup[] = [];
  for (const item of items) {
    const previous = rows.at(-1);
    if (previous && item.kind === "mark" && previous.lead.kind === "mark" && previous.lead.name === item.name
      && activitySource(previous.lead) === activitySource(item) && previous.lead.runId === item.runId) {
      previous.items.push(item);
      continue;
    }
    rows.push({ id: item.id, lead: item, items: [item] });
  }
  return rows;
}

export function activitySections(items: readonly MarkActivityItem[], now: number): ActivitySection[] {
  const startOfToday = new Date(now);
  startOfToday.setHours(0, 0, 0, 0);
  const today = startOfToday.getTime();
  const yesterday = today - 86_400_000;
  const buckets: Array<{ key: string; label: string; items: MarkActivityItem[] }> = [
    { key: "requests", label: "Needs you", items: [] },
    { key: "today", label: "Today", items: [] },
    { key: "yesterday", label: "Yesterday", items: [] },
    { key: "earlier", label: "Earlier", items: [] },
  ];
  for (const item of items) {
    const at = item.createdAt ?? -1;
    const bucket = isActivityRequest(item) ? buckets[0] : at >= today ? buckets[1] : at >= yesterday ? buckets[2] : buckets[3];
    bucket.items.push(item);
  }
  return buckets.filter((bucket) => bucket.items.length).map((bucket) => ({ key: bucket.key, label: bucket.label, rows: activityRows(bucket.items) }));
}

export function relativeActivityTime(at: number | null, now: number): string {
  if (at === null) return "";
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 60) return "now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(at);
}

export function activityMoment(at: number, now: number): string {
  const day = new Date(now);
  day.setHours(0, 0, 0, 0);
  const time = new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(at);
  if (at >= day.getTime()) return time;
  if (at >= day.getTime() - 86_400_000) return `Yesterday, ${time}`;
  const date = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(at);
  return `${date}, ${time}`;
}

function sentence(value: string): string {
  return value ? value[0].toLocaleUpperCase() + value.slice(1) : value;
}
