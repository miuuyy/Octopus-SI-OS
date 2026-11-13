import { useCallback, useEffect, useMemo, useRef, useState, type Dispatch, type DragEvent, type RefObject, type SetStateAction } from "react";
import type {
  OctopusClientAccessProfile,
  OctopusClientAttachmentStageHandle,
} from "../../electron";
import { attachmentLabel } from "./artifacts";
import { captureAttachmentDrop, discardAttachmentDrop, isAttachmentDrop, resolveAttachmentDrop, validateAttachmentDrop } from "./attachmentDrop";
import { MAX_ATTACHMENT_BYTES } from "./constants";
import {
  loadChatDrafts,
  loadChatSettings,
  saveChatDrafts,
} from "./storage";
import type { ChatAttachment } from "./types";
import type { ContinuationDraft } from "./chatTurnObservation";
import { DEFAULT_TASK_ACCESS_MODE, type TaskAccessMode } from "./taskAccess";

export type ChatDraftValue = {
  messageId: string;
  messageHadAttachments: boolean;
  text: string;
  attachments: ChatAttachment[];
  agentPresetId: string;
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  applicationIds: string[];
  driverNames: string[];
  accessMode: TaskAccessMode;
  continuation: ContinuationDraft | null;
};

type ChatDraft = ChatDraftValue & {
  updatedAtMs: number;
};

type ChatDraftState = ChatDraftValue & {
  fileInputRef: RefObject<HTMLInputElement | null>;
  dragOver: boolean;
  setMessageId: Dispatch<SetStateAction<string>>;
  setText: (value: string) => void;
  setRawText: Dispatch<SetStateAction<string>>;
  setAttachments: Dispatch<SetStateAction<ChatAttachment[]>>;
  setAgentPresetId: Dispatch<SetStateAction<string>>;
  setEeId: Dispatch<SetStateAction<string>>;
  setWorkerInterfaceId: Dispatch<SetStateAction<string>>;
  setProviderInterfaceId: Dispatch<SetStateAction<string>>;
  setProviderModel: Dispatch<SetStateAction<string>>;
  setReasoningEffort: Dispatch<SetStateAction<string>>;
  setServiceTier: Dispatch<SetStateAction<string>>;
  setApplicationIds: Dispatch<SetStateAction<string[]>>;
  setDriverNames: Dispatch<SetStateAction<string[]>>;
  setAccessMode: Dispatch<SetStateAction<TaskAccessMode>>;
  setContinuation: Dispatch<SetStateAction<ContinuationDraft | null>>;
  applyDraft: (draft: ChatDraftValue) => void;
  clearSubmissionIntent: (runId: string, messageId: string) => void;
  completeSubmissionIntent: (runId: string, messageId: string) => void;
  consumeDraft: (runId: string, text: string, attachments: ChatAttachment[]) => void;
  deleteDraft: (runId: string) => void;
  migrateDraft: (fromRunId: string, toRunId: string) => void;
  readDraft: (runId: string) => ChatDraft | undefined;
  resetInputHistory: () => void;
  saveCurrentDraft: (runId: string) => void;
  persistSubmissionIntent: (runId: string, messageId: string, hadAttachments: boolean) => void;
  writeDraft: (runId: string, draft: ChatDraftValue) => void;
  navigateInputHistory: (event: globalThis.KeyboardEvent, editor: HTMLElement, inputHistory: string[]) => boolean;
  onDragLeave: () => void;
  onDragOver: (event: DragEvent<HTMLElement>) => void;
  onFilesPicked: (files: FileList | readonly (File | OctopusClientAttachmentStageHandle)[] | null) => void;
  dropFiles: (event: DragEvent<HTMLElement>) => void;
  removeAttachment: (id: string) => void;
};

export function chatDraftKey(runId: string): string {
  return runId || "__new_chat__";
}

export function useChatDraftState(input: {
  accessProfile: OctopusClientAccessProfile;
  activeRunId: string;
  onError: (message: string) => void;
  preserveSubmissionIntentOnEdit: (runId: string, messageId: string) => boolean;
  storageScope: string;
}): ChatDraftState {
  const storedSettingsRef = useMemo(() => ({ current: loadChatSettings(input.accessProfile) }), [input.accessProfile]);
  const draftsRef = useMemo(() => ({ current: new Map<string, ChatDraft>(
    loadChatDrafts(input.accessProfile, input.storageScope).map((draft) => [draft.key, {
      messageId: draft.messageId,
      messageHadAttachments: draft.messageHadAttachments,
      text: draft.text,
      attachments: [],
      agentPresetId: draft.agentPresetId,
      eeId: draft.eeId,
      workerInterfaceId: draft.workerInterfaceId,
      providerInterfaceId: draft.providerInterfaceId,
      providerModel: draft.providerModel,
      reasoningEffort: draft.reasoningEffort,
      serviceTier: draft.serviceTier,
      applicationIds: draft.applicationIds,
      driverNames: draft.driverNames,
      accessMode: draft.accessMode,
      continuation: draft.continuation,
      updatedAtMs: draft.updatedAtMs,
    }]),
  ) }), [input.accessProfile, input.storageScope]);
  const initialDraftRef = useMemo(() => ({ current: draftsRef.current.get(chatDraftKey(input.activeRunId)) }), [draftsRef]);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const inputHistoryIndexRef = useRef(-1);
  const inputHistoryDraftRef = useRef("");
  const dropScope = JSON.stringify([input.accessProfile, input.storageScope, input.activeRunId]);
  const dropOwnerRef = useRef({ scope: dropScope });
  if (dropOwnerRef.current.scope !== dropScope) dropOwnerRef.current = { scope: dropScope };
  const pendingDropRef = useRef<object | null>(null);
  const mountedRef = useRef(true);
  const onErrorRef = useRef(input.onError);
  onErrorRef.current = input.onError;

  const [messageId, setMessageId] = useState(initialDraftRef.current?.messageId ?? "");
  const [messageHadAttachments, setMessageHadAttachments] = useState(
    initialDraftRef.current?.messageHadAttachments ?? false,
  );
  const [text, setRawText] = useState(initialDraftRef.current?.text ?? "");
  const [attachments, setAttachments] = useState<ChatAttachment[]>([]);
  const attachmentsRef = useRef(attachments);
  attachmentsRef.current = attachments;
  const [agentPresetId, setAgentPresetId] = useState(initialDraftRef.current?.agentPresetId || "");
  const [providerInterfaceId, setProviderInterfaceId] = useState(
    initialDraftRef.current?.providerInterfaceId || (input.activeRunId ? "" : storedSettingsRef.current.providerInterfaceId),
  );
  const [providerModel, setProviderModel] = useState(
    initialDraftRef.current?.providerModel || (input.activeRunId ? "" : storedSettingsRef.current.providerModel),
  );
  const [reasoningEffort, setReasoningEffort] = useState(
    initialDraftRef.current?.reasoningEffort ?? (input.activeRunId ? "" : storedSettingsRef.current.reasoningEffort),
  );
  const [serviceTier, setServiceTier] = useState(initialDraftRef.current?.serviceTier ?? (input.activeRunId ? "" : storedSettingsRef.current.serviceTier ?? ""));
  const [eeId, setEeId] = useState(initialDraftRef.current?.eeId || "");
  const [workerInterfaceId, setWorkerInterfaceId] = useState(initialDraftRef.current?.workerInterfaceId || "");
  const [applicationIds, setApplicationIds] = useState<string[]>(initialDraftRef.current?.applicationIds ?? []);
  const [driverNames, setDriverNames] = useState<string[]>(initialDraftRef.current?.driverNames ?? []);
  const [accessMode, setAccessMode] = useState<TaskAccessMode>(initialDraftRef.current?.accessMode ?? DEFAULT_TASK_ACCESS_MODE);
  const [continuation, setContinuation] = useState<ContinuationDraft | null>(initialDraftRef.current?.continuation ?? null);
  const [dragOver, setDragOver] = useState(false);

  const scopeIdentity = JSON.stringify([input.accessProfile, input.storageScope]);
  const [loadedScope, setLoadedScope] = useState(scopeIdentity);
  if (loadedScope !== scopeIdentity) {
    setLoadedScope(scopeIdentity);
    applyDraft(initialDraftRef.current ?? {
      messageId: "", messageHadAttachments: false, text: "", attachments: [], agentPresetId: "", eeId: "", workerInterfaceId: "",
      providerInterfaceId: input.activeRunId ? "" : storedSettingsRef.current.providerInterfaceId,
      providerModel: input.activeRunId ? "" : storedSettingsRef.current.providerModel,
      reasoningEffort: input.activeRunId ? "" : storedSettingsRef.current.reasoningEffort,
      serviceTier: input.activeRunId ? "" : storedSettingsRef.current.serviceTier,
      applicationIds: [], driverNames: [], accessMode: DEFAULT_TASK_ACCESS_MODE, continuation: null,
    });
  }
  const liveDraftRef = useRef({ runId: input.activeRunId, text, attachments, scopeIdentity });
  liveDraftRef.current = { runId: input.activeRunId, text, attachments, scopeIdentity };
  function persistDrafts() {
    try { saveChatDrafts(input.accessProfile, serializedDrafts(draftsRef.current), input.storageScope); }
    catch (error) {
      if (mountedRef.current) onErrorRef.current(`Your draft is kept in this window but could not be saved to disk. Keep this window open and free disk space. ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  function consumeDraft(runId: string, submittedText: string, submittedAttachments: ChatAttachment[]) {
    const current = liveDraftRef.current;
    if (current.runId !== runId || current.scopeIdentity !== scopeIdentity) return;
    const sameText = current.text.trim() === submittedText.trim();
    const sameAttachments = current.attachments.length === submittedAttachments.length
      && current.attachments.every((item, index) => item.id === submittedAttachments[index]?.id);
    if (!sameText || !sameAttachments) return;
    setMessageId(""); setMessageHadAttachments(false); setRawText(""); setAttachments([]); setContinuation(null);
    const draft = { ...currentDraft(), messageId: "", messageHadAttachments: false, text: "", attachments: [], continuation: null, updatedAtMs: Date.now() };
    draftsRef.current.set(chatDraftKey(runId), draft);
    persistDrafts();
  }

  function currentDraft(): ChatDraft {
    return {
      messageId,
      messageHadAttachments,
      text,
      attachments,
      agentPresetId,
      eeId,
      workerInterfaceId,
      providerInterfaceId,
      providerModel,
      reasoningEffort,
      serviceTier,
      applicationIds,
      driverNames,
      accessMode,
      continuation,
      updatedAtMs: Date.now(),
    };
  }

  function currentDraftForStorage(runId: string): ChatDraft {
    const current = currentDraft();
    const existing = draftsRef.current.get(chatDraftKey(runId));
    return current.messageId && !input.preserveSubmissionIntentOnEdit(runId, current.messageId) && existing?.messageId === current.messageId
      ? { ...existing, updatedAtMs: current.updatedAtMs }
      : current;
  }

  function resetInputHistory() {
    inputHistoryIndexRef.current = -1;
    inputHistoryDraftRef.current = "";
  }

  function applyDraft(draft: ChatDraftValue) {
    dropOwnerRef.current = { scope: dropScope };
    setMessageId(draft.messageId);
    setMessageHadAttachments(draft.messageHadAttachments);
    setRawText(draft.text);
    setAttachments(draft.attachments);
    setAgentPresetId(draft.agentPresetId);
    setEeId(draft.eeId);
    setWorkerInterfaceId(draft.workerInterfaceId);
    setProviderInterfaceId(draft.providerInterfaceId);
    setProviderModel(draft.providerModel);
    setReasoningEffort(draft.reasoningEffort);
    setServiceTier(draft.serviceTier ?? "");
    setApplicationIds(draft.applicationIds);
    setDriverNames(draft.driverNames);
    setAccessMode(draft.accessMode);
    setContinuation(draft.continuation);
  }

  function saveCurrentDraft(runId: string) {
    draftsRef.current.set(chatDraftKey(runId), currentDraftForStorage(runId));
  }

  function persistSubmissionIntent(runId: string, nextMessageId: string, hadAttachments: boolean) {
    const draft = {
      ...currentDraft(),
      messageId: nextMessageId,
      messageHadAttachments: hadAttachments,
      updatedAtMs: Date.now(),
    };
    draftsRef.current.set(chatDraftKey(runId), draft);
    persistDrafts();
    setMessageId(nextMessageId);
    setMessageHadAttachments(hadAttachments);
  }

  function clearSubmissionIntent(runId: string, expectedMessageId: string) {
    const key = chatDraftKey(runId);
    const draft = draftsRef.current.get(key);
    if (!draft || draft.messageId !== expectedMessageId) return;
    draftsRef.current.set(key, {
      ...draft,
      messageId: "",
      messageHadAttachments: false,
      updatedAtMs: Date.now(),
    });
    persistDrafts();
    setMessageId((current) => current === expectedMessageId ? "" : current);
    setMessageHadAttachments(false);
  }

  function completeSubmissionIntent(runId: string, expectedMessageId: string) {
    const key = chatDraftKey(runId);
    const draft = draftsRef.current.get(key);
    if (!draft || draft.messageId !== expectedMessageId) return;
    if (runId === input.activeRunId) dropOwnerRef.current = { scope: dropScope };
    draftsRef.current.set(key, {
      ...draft,
      messageId: "",
      messageHadAttachments: false,
      text: "",
      attachments: [],
      continuation: null,
      updatedAtMs: Date.now(),
    });
    persistDrafts();
    setMessageId((current) => current === expectedMessageId ? "" : current);
    setMessageHadAttachments(false);
  }

  function writeDraft(runId: string, draft: ChatDraftValue) {
    draftsRef.current.set(chatDraftKey(runId), { ...draft, updatedAtMs: Date.now() });
    persistDrafts();
  }

  function readDraft(runId: string) {
    return draftsRef.current.get(chatDraftKey(runId));
  }

  function deleteDraft(runId: string) {
    draftsRef.current.delete(chatDraftKey(runId));
  }

  const migrateDraft = useCallback((fromRunId: string, toRunId: string) => {
    const sourceKey = chatDraftKey(fromRunId);
    const targetKey = chatDraftKey(toRunId);
    const source = draftsRef.current.get(sourceKey);
    if (source && !draftsRef.current.has(targetKey)) draftsRef.current.set(targetKey, source);
    draftsRef.current.delete(sourceKey);
  }, []);

  function setText(value: string) {
    if (
      messageId
      && !input.preserveSubmissionIntentOnEdit(input.activeRunId, messageId)
    ) {
      clearSubmissionIntent(input.activeRunId, messageId);
    }
    resetInputHistory();
    setRawText(value);
  }

  function navigateInputHistory(event: globalThis.KeyboardEvent, editor: HTMLElement, inputHistory: string[]): boolean {
    if (
      inputHistory.length === 0
      || event.altKey
      || event.ctrlKey
      || event.metaKey
      || event.shiftKey
      || (event.key !== "ArrowUp" && event.key !== "ArrowDown")
    ) return false;
    const direction = event.key === "ArrowUp" ? "older" : "newer";
    if (!caretAtComposerBoundary(editor, direction === "older" ? "start" : "end")) return false;
    const currentIndex = inputHistoryIndexRef.current;
    if (direction === "older") {
      if (currentIndex < 0) inputHistoryDraftRef.current = text;
      const nextIndex = Math.min(inputHistory.length - 1, currentIndex + 1);
      if (nextIndex === currentIndex) return false;
      inputHistoryIndexRef.current = nextIndex;
      setRawText(inputHistory[nextIndex] ?? "");
    } else {
      if (currentIndex < 0) return false;
      const nextIndex = currentIndex - 1;
      inputHistoryIndexRef.current = nextIndex;
      setRawText(nextIndex < 0 ? inputHistoryDraftRef.current : inputHistory[nextIndex] ?? "");
    }
    event.preventDefault();
    window.requestAnimationFrame(() => placeComposerCaret(editor, "end"));
    return true;
  }

  function onFilesPicked(files: FileList | readonly (File | OctopusClientAttachmentStageHandle)[] | null) {
    const tooLarge: string[] = [];
    const picked: ChatAttachment[] = Array.from(files ?? []).flatMap<ChatAttachment>((value) => {
      if (value.size > MAX_ATTACHMENT_BYTES) {
        tooLarge.push(value.name);
        return [];
      }
      if (!(value instanceof File)) {
        if (!value.contentHash) {
          tooLarge.push(value.name);
          return [];
        }
        return [{
          id: `stage:${value.id}`,
          stage: value,
          name: value.name,
          size: value.size,
          type: value.type,
          label: value.label,
          lastModified: value.lastModified,
        }];
      }
      return [{
        id: `${value.name}:${value.size}:${value.lastModified}`,
        file: value,
        name: value.name,
        size: value.size,
        type: value.type || "application/octet-stream",
        label: attachmentLabel(value),
        lastModified: value.lastModified,
      }];
    });
    if (tooLarge.length) input.onError(`Attachment too large: ${tooLarge.join(", ")}`);
    if (picked.length === 0) return;
    if (
      messageId
      && !messageHadAttachments
      && !input.preserveSubmissionIntentOnEdit(input.activeRunId, messageId)
    ) {
      clearSubmissionIntent(input.activeRunId, messageId);
    }
    setAttachments((current) => {
      const known = new Set(current.map((file) => file.id));
      const unique = picked.filter((file) => !known.has(file.id));
      const remaining = Math.max(0, 12 - current.length);
      if (unique.length > remaining) input.onError(`A task can include up to 12 attachments; ${unique.length - remaining} ${unique.length - remaining === 1 ? "item was" : "items were"} not added.`);
      return [...current, ...unique.slice(0, remaining)];
    });
    if (fileInputRef.current) fileInputRef.current.value = "";
  }

  function removeAttachment(id: string) {
    if (
      messageId
      && !messageHadAttachments
      && !input.preserveSubmissionIntentOnEdit(input.activeRunId, messageId)
    ) {
      clearSubmissionIntent(input.activeRunId, messageId);
    }
    setAttachments((current) => current.filter((item) => item.id !== id));
  }

  function onDragOver(event: DragEvent<HTMLElement>) {
    if (!isAttachmentDrop(event.dataTransfer)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "copy";
    setDragOver(true);
  }

  const filesPickedRef = useRef(onFilesPicked);
  filesPickedRef.current = onFilesPicked;

  function dropFiles(event: DragEvent<HTMLElement>) {
    setDragOver(false);
    if (!isAttachmentDrop(event.dataTransfer)) return;
    event.preventDefault();
    const owner = dropOwnerRef.current;
    if (pendingDropRef.current === owner) {
      input.onError("Wait for the current attachment drop to finish.");
      return;
    }
    let entries: ReturnType<typeof captureAttachmentDrop>;
    try {
      entries = captureAttachmentDrop(event.dataTransfer);
      validateAttachmentDrop(entries.map((entry) => entry.file), Math.max(0, 12 - attachmentsRef.current.length));
    } catch (error) {
      input.onError(error instanceof Error ? error.message : String(error));
      return;
    }
    const transport = window.octopusTransport;
    pendingDropRef.current = owner;
    void (async () => {
      const selected = await resolveAttachmentDrop(entries, transport);
      if (!selected) return;
      if (!mountedRef.current || dropOwnerRef.current !== owner) {
        await discardAttachmentDrop(selected, transport);
        return;
      }
      try {
        validateAttachmentDrop(selected, Math.max(0, 12 - attachmentsRef.current.length));
        filesPickedRef.current(selected);
      } catch (error) {
        await discardAttachmentDrop(selected, transport);
        throw error;
      }
    })().catch((error) => {
      if (mountedRef.current && dropOwnerRef.current === owner) onErrorRef.current(error instanceof Error ? error.message : String(error));
    }).finally(() => {
      if (pendingDropRef.current === owner) pendingDropRef.current = null;
    });
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      dropOwnerRef.current = { scope: dropOwnerRef.current.scope };
    };
  }, []);

  useEffect(() => {
    if (!input.storageScope) return;
    draftsRef.current.set(
      chatDraftKey(input.activeRunId),
      currentDraftForStorage(input.activeRunId),
    );
    const timeout = window.setTimeout(
      persistDrafts,
      350,
    );
    return () => window.clearTimeout(timeout);
  }, [
    input.accessProfile,
    input.activeRunId,
    input.storageScope,
    applicationIds,
    accessMode,
    agentPresetId,
    attachments,
    continuation,
    driverNames,
    eeId,
    providerInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    messageId,
    messageHadAttachments,
    text,
    workerInterfaceId,
  ]);

  useEffect(() => {
    if (!input.storageScope) return;
    const persist = persistDrafts;
    window.addEventListener("beforeunload", persist);
    return () => {
      window.removeEventListener("beforeunload", persist);
      persist();
    };
  }, [input.accessProfile, input.storageScope]);

  return {
    messageId,
    messageHadAttachments,
    text,
    attachments,
    agentPresetId,
    eeId,
    workerInterfaceId,
    providerInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    applicationIds,
    driverNames,
    accessMode,
    continuation,
    fileInputRef,
    dragOver,
    setMessageId,
    setText,
    setRawText,
    setAttachments,
    setAgentPresetId,
    setEeId,
    setWorkerInterfaceId,
    setProviderInterfaceId,
    setProviderModel,
    setReasoningEffort,
    setServiceTier,
    setApplicationIds,
    setDriverNames,
    setAccessMode,
    setContinuation,
    applyDraft,
    clearSubmissionIntent,
    completeSubmissionIntent,
    consumeDraft,
    deleteDraft,
    migrateDraft,
    readDraft,
    resetInputHistory,
    saveCurrentDraft,
    persistSubmissionIntent,
    writeDraft,
    navigateInputHistory,
    onDragLeave: () => setDragOver(false),
    onDragOver,
    onFilesPicked,
    dropFiles,
    removeAttachment,
  };
}

function serializedDrafts(drafts: ReadonlyMap<string, ChatDraft>) {
  return [...drafts.entries()].map(([key, draft]) => ({
    key,
    messageId: draft.messageId,
    messageHadAttachments: draft.messageHadAttachments,
    text: draft.text,
    agentPresetId: draft.agentPresetId,
    eeId: draft.eeId,
    workerInterfaceId: draft.workerInterfaceId,
    providerInterfaceId: draft.providerInterfaceId,
    providerModel: draft.providerModel,
    reasoningEffort: draft.reasoningEffort,
    serviceTier: draft.serviceTier,
    applicationIds: draft.applicationIds,
    driverNames: draft.driverNames,
    accessMode: draft.accessMode,
    continuation: draft.continuation,
    updatedAtMs: draft.updatedAtMs,
  }));
}

function caretAtComposerBoundary(element: HTMLElement, boundary: "start" | "end"): boolean {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) return false;
  const range = selection.getRangeAt(0);
  if (!element.contains(range.startContainer)) return false;
  const probe = range.cloneRange();
  probe.selectNodeContents(element);
  if (boundary === "start") probe.setEnd(range.startContainer, range.startOffset);
  else probe.setStart(range.startContainer, range.startOffset);
  return probe.toString().length === 0;
}

function placeComposerCaret(element: HTMLElement, boundary: "start" | "end") {
  const selection = window.getSelection();
  if (!selection) return;
  const range = document.createRange();
  range.selectNodeContents(element);
  range.collapse(boundary === "start");
  selection.removeAllRanges();
  selection.addRange(range);
}
