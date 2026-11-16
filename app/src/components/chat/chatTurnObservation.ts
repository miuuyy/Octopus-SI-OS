import { loadChatRunMessages, type ChatRunMessage } from "../../chatRuns";
import type {
  OctopusClientAccessProfile,
  OctopusOperatorChatArtifact,
  OctopusOperatorChatCursor,
  OctopusOperatorChatMovementArtifact,
} from "../../electron";
import { readOperatorChatArtifacts } from "../../projectionClient";
import { currentProjectionGeneration, nextProjectionChange } from "../../projectionLive";
import type { ChatRenderMessage } from "./types";
import { isChatReplyTerminal } from "./chatTurnControl";

export type ObservedTerminalArtifact = {
  artifactId: string;
  eventData: Record<string, unknown>;
};

export type ContinuationDraft = {
  sourceRunId: string;
  sourceArtifactId: string;
};

export async function observeChatArtifacts(input: {
  runId: string;
  interfaceId: string;
  afterSeq: number;
  isCurrent: () => boolean;
  onArtifacts: (artifacts: OctopusOperatorChatArtifact[]) => void;
  onFailure: (failure: unknown) => void;
  onRecovered: () => void;
}): Promise<void> {
  let cursor: OctopusOperatorChatCursor | undefined;
  let afterSeq = input.afterSeq;
  while (input.isCurrent()) {
    const generation = currentProjectionGeneration();
    try {
      const batch = await readOperatorChatArtifacts({
        interfaceId: input.interfaceId, runId: input.runId, afterSeq, cursor, waitMs: 20_000, limit: 256,
      });
      if (!input.isCurrent()) return;
      if (batch.artifacts.length) input.onArtifacts(batch.artifacts);
      if (!input.isCurrent()) return;
      afterSeq = batch.artifacts.reduce((latest, artifact) => artifact.source === "movement"
        ? Math.max(latest, artifact.movement_seq) : latest, afterSeq);
      cursor = batch.cursor;
      input.onRecovered();
    } catch (failure) {
      if (!input.isCurrent()) return;
      input.onFailure(failure);
      await nextProjectionChange(1_000, generation);
    }
  }
}

export function chatObservationFailureMessage(input: {
  id: string;
  runId: string;
  detail: string;
  afterSeq: number;
  createdAtMs: number;
}): ChatRenderMessage {
  return {
    id: input.id,
    runId: input.runId,
    role: "assistant",
    text: `Task updates are unavailable. The current outcome is unconfirmed. ${input.detail}`,
    createdAtMs: input.createdAtMs,
    direction: "out",
    eventKind: "warning",
    eventData: { _observation_error: true, after_seq: input.afterSeq },
    pending: false,
    error: true,
    terminal: false,
  };
}

export async function observeChatReply(input: {
  accessProfile: OctopusClientAccessProfile;
  knownRunId: string;
  interfaceId: string;
  afterSeq: number;
  cursor: OctopusOperatorChatCursor;
  isCurrent: () => boolean;
  onArtifacts: (artifacts: OctopusOperatorChatArtifact[]) => void;
}): Promise<{ runId: string; terminal: ObservedTerminalArtifact; messages?: ChatRunMessage[] } | null> {
  if (!input.knownRunId) throw new Error("operator turn returned no run id");
  const currentFailure = (failure: unknown) => {
    if (input.isCurrent()) throw failure;
    return null;
  };
  if (input.accessProfile === "live") {
    let cursor = input.cursor;
    while (input.isCurrent()) {
      const batch = await readOperatorChatArtifacts({
        interfaceId: input.interfaceId,
        runId: input.knownRunId,
        afterSeq: input.afterSeq,
        cursor,
        waitMs: 30_000,
        limit: 256,
      }).catch(currentFailure);
      if (!batch || !input.isCurrent()) return null;
      cursor = batch.cursor;
      input.onArtifacts(batch.artifacts);
      const terminal = batch.artifacts.find((artifact): artifact is OctopusOperatorChatMovementArtifact => (
        artifact.source === "movement" && isChatReplyTerminal(artifact.event_kind, artifact.event_data, artifact.terminal)
      ));
      if (!terminal) continue;
      if (terminal.error) {
        throw new Error(terminal.text || "The provider emitted a terminal error artifact.");
      }
      return {
        runId: input.knownRunId,
        terminal: {
          artifactId: terminal.artifact_id,
          eventData: terminal.event_data,
        },
      };
    }
    return null;
  }
  let observedGeneration = currentProjectionGeneration();
  while (input.isCurrent()) {
    const messages = await loadChatRunMessages(input.knownRunId).catch(currentFailure);
    if (!messages || !input.isCurrent()) return null;
    const reply = messages.find((message) => (
      message.direction === "out"
      && message.movementSeq > input.afterSeq
      && isChatReplyTerminal(message.eventKind, message.eventData, message.terminal)
    ));
    if (reply?.error) throw new Error(reply.text || "The provider emitted a terminal error artifact.");
    if (reply) {
      return {
        runId: input.knownRunId,
        terminal: {
          artifactId: reply.artifactId,
          eventData: reply.eventData,
        },
        // This path reads the task history and reports no live artifacts, so the turn's own messages travel with its
        // terminal: the caller decides from the same read that showed the turn had ended.
        messages: messages.filter((message) => message.movementSeq > input.afterSeq),
      };
    }
    const currentGeneration = currentProjectionGeneration();
    if (currentGeneration !== observedGeneration) {
      observedGeneration = currentGeneration;
      continue;
    }
    await nextProjectionChange(500, observedGeneration);
    observedGeneration = currentProjectionGeneration();
  }
  return null;
}
