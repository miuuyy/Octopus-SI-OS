import type { OctopusScheduledDraft } from "../../electron";

export type ScheduledInterfaceStatus = "enabled" | "disabled" | "invalid" | "template";

export type ChangeJournalEntry = {
  kind: "file";
  path: string;
  action: "created" | "modified" | "deleted";
  reason: string;
  status: "applied" | "undone" | "changed";
  restorable: boolean;
  unrestorableReason: string;
} | {
  kind: "other";
  description: string;
  undo: string;
};

export type ChangeJournal = {
  digest: string;
  firing: string;
  outcome: "changed" | "unchanged";
  summary: string;
  checks: Array<{ check: string; result: string; evidence: string }>;
  changes: ChangeJournalEntry[];
  undoable: boolean;
  undone: boolean;
  error: string;
};

export type ScheduledDelivery = {
  seq: number;
  atMs: number;
  runId: string;
  chatInterfaceId: string;
  title: string;
  journal: ChangeJournal | null;
};

export type InactivityState = {
  status: "unstarted" | "idle" | "waiting" | "prepared" | "admitted";
  current: boolean;
  lastActivityAtMs: number | null;
};

export type ScheduledScopeOption = { eeId: string; name: string };

export type ScheduledInterfaceProjection = {
  interfaceId: string;
  displayName: string;
  status: ScheduledInterfaceStatus;
  valid: boolean;
  activationDigest: string;
  activationPath: string;
  descriptorPath: string;
  eventSource?: {
    sources: string[];
    targets: Array<{ runId: string; chatInterfaceId: string; title: string }>;
  } | null;
  schedule: {
    type: "interval";
    intervalSeconds: number;
  } | {
    type: "cron";
    cron: string;
  } | {
    type: "once";
    dueAt: string;
  } | {
    type: "inactivity";
    seconds: number;
    interfaceMatch: Record<string, string>;
    excludeInterfaceMatch: Record<string, string> | null;
    withinEe: string;
  } | null;
  timezone: string;
  fireOnStart: boolean;
  instructions: string;
  reminderEnv: string;
  targets: Array<{ runId: string; chatInterfaceId: string; title: string }>;
  cloneEligible: boolean;
  template: boolean;
  inactivityState: InactivityState | null;
  deliveries: ScheduledDelivery[];
  lastRunAtMs: number | null;
  lastMovementSeq: number | null;
  nextRunAtMs: number | null;
  nextRunSource: "driver_state" | "schedule" | "";
  error: string;
};

export type ScheduledEditorDraft = OctopusScheduledDraft & {
  interfaceId: string;
  sourceInterfaceId: string;
};

export type ScheduledEditorMode =
  | { kind: "create"; source: ScheduledInterfaceProjection }
  | { kind: "edit"; task: ScheduledInterfaceProjection };

export type ScheduledProjectionResponse = {
  scheduled_interfaces?: unknown[];
  scope_options?: unknown[];
};
