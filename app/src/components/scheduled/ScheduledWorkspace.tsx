import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import type { OctopusScheduledMutationRequest } from "../../electron";
import { canMutateScheduledInterfaces, mutateScheduledInterface } from "../../projectionClient";
import { appToast } from "../../toasts";
import { describeSchedule, draftFromScheduledInterface, mutationDraft } from "./scheduleModel";
import type { ScheduledSuggestion } from "./scheduledSuggestions";
import type { ScheduledDelivery, ScheduledEditorDraft, ScheduledEditorMode, ScheduledInterfaceProjection } from "./types";
import { useScheduledInterfaces } from "./useScheduledInterfaces";

function useScheduledWorkspaceState(active: boolean, onCreateTask: (text: string) => void) {
  const projection = useScheduledInterfaces(active);
  const [selectedId, setSelectedId] = useState("");
  const [editor, setEditor] = useState<{ mode: ScheduledEditorMode; draft: ScheduledEditorDraft; revision: number } | null>(null);
  const [pending, setPending] = useState(false);
  const [editorError, setEditorError] = useState("");
  const [deleteCandidate, setDeleteCandidate] = useState<ScheduledInterfaceProjection | null>(null);
  const [undoCandidate, setUndoCandidate] = useState<ScheduledDelivery | null>(null);
  const [searchRequest, setSearchRequest] = useState(0);
  const requestSearch = useCallback(() => setSearchRequest((current) => current + 1), []);
  const canEdit = canMutateScheduledInterfaces() && !projection.error;
  const triggers = projection.tasks.filter((task) => !task.template);
  const templates = projection.tasks.filter((task) => task.valid && task.cloneEligible)
    .sort((left, right) => Number(right.template) - Number(left.template) || left.displayName.localeCompare(right.displayName));
  const selected = triggers.find((task) => task.interfaceId === selectedId) ?? null;
  const createUnavailableReason = !canMutateScheduledInterfaces() ? "Open a connected desktop client to create a trigger." : "";
  const timeSuggestionUnavailableReason = createUnavailableReason;

  async function mutate(request: OctopusScheduledMutationRequest, failureTitle: string, after: () => void) {
    setPending(true);
    setEditorError("");
    try {
      await mutateScheduledInterface(request);
      after();
      await projection.reload();
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      if (request.action === "create" || request.action === "update") setEditorError(message);
      else appToast.danger(`${failureTitle}: ${message}`);
    } finally {
      setPending(false);
    }
  }

  function selectTask(interfaceId: string) {
    if (pending) return;
    setSelectedId(interfaceId);
  }

  function startCreate(suggestion?: ScheduledSuggestion) {
    if (pending || createUnavailableReason) return;
    const source = templates.find((task) => task.interfaceId === suggestion?.templateInterfaceId);
    const timing = source ? describeSchedule(source) : suggestion?.schedule;
    onCreateTask(suggestion
      ? `Create a trigger: ${suggestion.title}.\n\n${suggestion.instructions}${timing ? `\n\nWhen: ${timing}.` : ""}`
      : "Create a trigger that ");
  }

  function startEdit(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid || task.eventSource) return;
    setEditor((previous) => ({ mode: { kind: "edit", task }, draft: draftFromScheduledInterface(task), revision: (previous?.revision ?? 0) + 1 }));
    setEditorError("");
  }

  function save(draft: ScheduledEditorDraft, source: ScheduledInterfaceProjection) {
    if (!editor || pending || !canEdit) return;
    const request: OctopusScheduledMutationRequest = editor.mode.kind === "create"
      ? { action: "create", sourceInterfaceId: source.interfaceId, sourceActivationDigest: source.activationDigest, interfaceId: draft.interfaceId, draft: mutationDraft(draft) }
      : { action: "update", interfaceId: editor.mode.task.interfaceId, expectedDigest: editor.mode.task.activationDigest, draft: mutationDraft(draft) };
    void mutate(request, editor.mode.kind === "create" ? "Could not create trigger" : "Could not update trigger", () => { setSelectedId(draft.interfaceId); setEditor(null); });
  }

  function toggleTask(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid || task.eventSource) return;
    void mutate({ action: "set_enabled", interfaceId: task.interfaceId, expectedDigest: task.activationDigest, enabled: task.status === "disabled" }, "Could not update trigger", () => {});
  }

  function confirmDelete() {
    const task = deleteCandidate;
    if (!task || pending || !canEdit || task.eventSource) return;
    void mutate({ action: "delete", interfaceId: task.interfaceId, expectedDigest: task.activationDigest }, "Could not delete trigger", () => {
      setDeleteCandidate(null);
      if (selectedId === task.interfaceId) setSelectedId("");
    });
  }

  function confirmUndo() {
    const delivery = undoCandidate;
    if (!delivery?.journal?.undoable || pending || !canEdit) return;
    void mutate({ action: "undo_changes", runId: delivery.runId, firing: delivery.journal.firing, expectedJournalDigest: delivery.journal.digest }, "Could not undo changes", () => setUndoCandidate(null));
  }

  return {
    ...projection, tasks: triggers, selected, selectedId, selectTask, editor, editorError, templates, pending, canEdit, createUnavailableReason,
    timeSuggestionUnavailableReason, startCreate, startEdit, save, toggleTask,
    deleteCandidate, requestDelete: (task: ScheduledInterfaceProjection) => { if (!pending && canEdit && !task.eventSource) setDeleteCandidate(task); },
    cancelDelete: () => { if (!pending) setDeleteCandidate(null); }, confirmDelete,
    undoCandidate, requestUndo: (delivery: ScheduledDelivery) => { if (!pending && delivery.journal?.undoable) setUndoCandidate(delivery); },
    cancelUndo: () => { if (!pending) setUndoCandidate(null); }, confirmUndo,
    cancelEdit: () => { if (!pending) { setEditor(null); setEditorError(""); } },
    searchRequest, requestSearch,
  };
}

const ScheduledWorkspaceContext = createContext<ReturnType<typeof useScheduledWorkspaceState> | null>(null);

export function ScheduledWorkspaceProvider({ active, onCreateTask, children }: { active: boolean; onCreateTask: (text: string) => void; children: ReactNode }) {
  const state = useScheduledWorkspaceState(active, onCreateTask);
  return <ScheduledWorkspaceContext.Provider value={state}>{children}</ScheduledWorkspaceContext.Provider>;
}

export function useScheduledWorkspace() {
  const state = useContext(ScheduledWorkspaceContext);
  if (!state) throw new Error("Triggers must be rendered inside their workspace provider.");
  return state;
}
