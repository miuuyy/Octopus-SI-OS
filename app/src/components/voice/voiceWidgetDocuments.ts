import type { GlyphName } from "../IconGlyphs";
import type { AdmittedClientSurfaceDocument, ClientSurfaceDocument, ClientSurfaceNode, ClientSurfaceTone } from "../../client-surfaces/types";
import { admitClientSurfaceDocument } from "../../client-surfaces/validation";

export type VoiceWidgetItem = {
  detail?: string;
  glyph?: GlyphName;
  label: string;
  state?: string;
  tone?: ClientSurfaceTone;
};

export type TaskProgressWidget = {
  completion: number;
  failed: number;
  running: number;
  scheduled: number;
  unseenDone: number;
  waiting: number;
};

export type UpcomingScheduledWidgetItem = {
  at: string;
  label: string;
};

export type InsightsWidget = {
  change: string;
  changeDetail: string;
  label: string;
  series: number[];
  value: string;
};

const WIDGET_ROW_LIMIT = 24;

export function buildVoiceWidgetDocuments(input: {
  context: VoiceWidgetItem[];
  goals: VoiceWidgetItem[];
  insights: InsightsWidget;
  memory: VoiceWidgetItem[];
  scheduled: UpcomingScheduledWidgetItem[];
  taskProgress: TaskProgressWidget;
  tools: VoiceWidgetItem[];
}): AdmittedClientSurfaceDocument[] {
  return [
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.live-context",
      revision: 5,
      title: "Live context",
      glyph: "layers",
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 0,
        layout: {
          min_width: 184,
          preferred_width: 240,
          max_width: 420,
          min_height: 190,
          preferred_height: 230,
          max_height: 320,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "md",
          children: [
            ...widgetRows(input.context, (item) => ({
              type: "key_value",
              label: item.label,
              tone: item.tone,
              value: item.detail || "—",
            })),
          ],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.insights",
      revision: 4,
      title: "Insights",
      glyph: "insights",
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 5,
        layout: {
          min_width: 240,
          preferred_width: 292,
          max_width: 420,
          min_height: 120,
          preferred_height: 148,
          max_height: 240,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "row",
          gap: "md",
          align: "center",
          justify: "between",
          children: [
            {
              type: "stack",
              gap: "xs",
              children: [
                { type: "text", value: input.insights.label, role: "caption", tone: "muted" },
                { type: "text", value: input.insights.value, role: "title" },
                {
                  type: "row",
                  gap: "xs",
                  align: "center",
                  children: [
                    { type: "text", value: input.insights.change, role: "caption", tone: "positive" },
                    { type: "text", value: input.insights.changeDetail, role: "caption", tone: "muted" },
                  ],
                },
              ],
            },
            { type: "sparkline", values: input.insights.series, tone: "accent" },
          ],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.task-progress",
      revision: 3,
      title: "Task progress",
      glyph: "progress",
      header: { show_glyph: true, tone: "accent" },
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 10,
        layout: {
          min_width: 230,
          preferred_width: 280,
          max_width: 360,
          min_height: 145,
          preferred_height: 180,
          max_height: 180,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "none",
          children: input.taskProgress.unseenDone + input.taskProgress.running + input.taskProgress.waiting
            + input.taskProgress.scheduled + input.taskProgress.failed === 0
            ? [{ type: "text", value: "No tasks yet", role: "caption", tone: "muted" }]
            : [{
            type: "donut",
            value: input.taskProgress.completion,
            label: "Complete",
            segments: [
              { label: "Done", value: input.taskProgress.unseenDone, tone: "positive" },
              { label: "Running", value: input.taskProgress.running, tone: "accent" },
              { label: "Waiting", value: input.taskProgress.waiting, tone: "warning" },
              { label: "Failed", value: input.taskProgress.failed, tone: "danger" },
              { label: "Scheduled", value: input.taskProgress.scheduled, tone: "muted" },
            ],
          }],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.upcoming-scheduled",
      revision: 2,
      title: "Upcoming scheduled",
      glyph: "scheduled",
      header: { show_glyph: true, tone: "accent" },
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 20,
        layout: {
          min_width: 190,
          preferred_width: 260,
          max_width: 420,
          min_height: 150,
          preferred_height: 210,
          max_height: 300,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "none",
          children: input.scheduled.length
            ? input.scheduled.slice(0, 4).map((item): ClientSurfaceNode => ({
                type: "status",
                label: item.label,
                detail: item.at,
              }))
            : [{ type: "text", value: "No scheduled tasks", role: "caption", tone: "muted" }],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.active-tools",
      revision: 6,
      title: "Active tools",
      glyph: "plugins",
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 30,
        layout: {
          min_width: 190,
          preferred_width: 260,
          max_width: 420,
          min_height: 150,
          preferred_height: 210,
          max_height: 300,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "none",
          children: [
            ...widgetRows(input.tools, (item) => ({
              type: "key_value",
              label: item.state || "Ready",
              tone: item.tone,
              value: item.label,
            })),
          ],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.current-goals",
      revision: 5,
      title: "Current goals",
      glyph: "target",
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 40,
        layout: {
          min_width: 190,
          preferred_width: 280,
          max_width: 460,
          min_height: 140,
          preferred_height: 200,
          max_height: 300,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "xs",
          children: [
            ...input.goals.map((item, index): ClientSurfaceNode => ({
              type: "key_value",
              label: `Goal ${index + 1}`,
              tone: item.tone,
              value: item.label,
            })),
          ],
        },
      },
    }),
    compileWidget({
      schema: "octopus.client_surface",
      schema_version: 1,
      release_channel: "lab",
      id: "builtin.voice.memory",
      revision: 6,
      title: "Memory",
      glyph: "memory",
      target: {
        kind: "voice.widget",
        slot: "right",
        order: 50,
        layout: {
          min_width: 190,
          preferred_width: 260,
          max_width: 420,
          min_height: 150,
          preferred_height: 200,
          max_height: 280,
          content_fit: "reflow",
        },
      },
      render: {
        kind: "native",
        density: "compact",
        root: {
          type: "stack",
          gap: "xs",
          children: [
            { type: "key_value", label: "Files", value: { path: "sources.vault.files", fallback: memorySummary(input.memory), format: "number" } },
            { type: "key_value", label: "Recent", value: { path: "sources.vault.entries.0.path", fallback: input.memory[0]?.label || "Nothing available" } },
            { type: "key_value", label: "Source", value: { path: "sources.vault.name", fallback: input.memory.length ? "Bound artifacts" : "Not configured" } },
            ...widgetRows(input.memory.slice(1), (item) => ({
              type: "key_value",
              label: item.state || "Artifact",
              tone: item.tone,
              value: item.label,
            })),
          ],
        },
      },
    }),
  ];
}

function widgetRows(items: VoiceWidgetItem[], row: (item: VoiceWidgetItem) => ClientSurfaceNode): ClientSurfaceNode[] {
  if (items.length <= WIDGET_ROW_LIMIT) return items.map(row);
  const shown = items.slice(0, WIDGET_ROW_LIMIT - 1);
  return [...shown.map(row), { type: "key_value", label: "More", tone: "muted", value: String(items.length - shown.length) }];
}

function compileWidget(document: ClientSurfaceDocument): AdmittedClientSurfaceDocument {
  const digest = `compiled:${document.id}@${document.revision}`;
  const result = admitClientSurfaceDocument(document, digest, "compiled:voice");
  if (!result.admitted) {
    const details = result.verdict.details.issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ");
    throw new Error(`compiled voice widget ${document.id} failed admission: ${details}`);
  }
  return result.admitted;
}

function memorySummary(items: VoiceWidgetItem[]): string {
  if (!items.length) return "No memory";
  return `${items.length} bound artifact${items.length === 1 ? "" : "s"}`;
}
