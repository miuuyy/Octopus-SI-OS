import { OcLoadingRows } from "../../design-system/LoadingSkeleton";
import { Fragment, lazy, memo, Suspense, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type DragEvent, type ReactNode } from "react";
import { flushSync } from "react-dom";
import { motion } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import { copyText } from "../../runtimeFormat";
import { Glyph } from "../IconGlyphs";
import { CadencedShimmerText } from "../CadencedShimmerText";
import { MessageContent } from "./MessageContent";
import { ChatAttachmentStrip } from "./ChatAttachmentCard";
import { MessageTooltip } from "./MessageTooltip";
import type { ChatOperatorResponse, ChatRenderMessage, ChatToolPresentationIndex } from "./types";
import type { ChatPinnedContextItem } from "../../chatRuns";
import { subscribeClientCommands } from "../../clientActions";
import { ChatRenderBoundary } from "./ChatRenderBoundary";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader } from "../../design-system/components";
import { CollapsibleMessageBody } from "./CollapsibleMessageBody";
import { OperatorRequestSurface } from "./OperatorRequestSurface";
import { ActivityDisclosure, TerminalErrorActivity, TurnActivity } from "./TurnActivity";
import { McpResourceSurface } from "./McpResourceSurface";
import { ArtifactEndResources, artifactEndResourceFromMessage } from "../artifacts/ArtifactEndResources";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import { baseDynamicToolId, dynamicToolGlyph, dynamicToolOperationLabel, dynamicToolPresentation, dynamicToolReadLabel, dynamicToolSummary, webSearchActivityDetail, webSearchActivityLabel, type DynamicToolPresentation } from "./toolPresentation";
import { conversationTraceAdmission, isFinalAssistantResponse } from "./conversationTrace";
import { commandActivityModel, commandOutcomeLabel, commandResultBody } from "./commandActivityModel";
import { readProjection } from "../../projectionClient";
import { appToast } from "../../toasts";
import type { ArtifactReference } from "../artifacts/types";
import { useOpenArtifactFiles } from "../files/FilesPaneNavigation";
import { AssistantDirectiveSurfaces } from "./AssistantDirectiveSurfaces";
import { projectAssistantDirectives, type AssistantVisualizationTarget } from "./assistantDirectives";
import {
  buildThreadVirtualizerLayout,
  distanceFromBottomAnchoredToTurn,
  distanceFromBottomForTurn,
  rangeAnchoredToTurn,
  threadVirtualizerRange,
  type ThreadVirtualizerRange,
} from "./threadVirtualizer";
import { DevThreadSearch, type DevThreadSearchHost } from "./DevThreadSearch";

const WorkingPatchDiff = lazy(() => import("./WorkingPatchDiff"));

type Props = {
  activityPresentation?: "conversation" | "ledger";
  messages: ChatRenderMessage[];
  activeMessageCount: number;
  conversationId: string;
  contentX?: number;
  findQuery: string;
  findActiveIndex: number;
  onFindMatchCountChange: (count: number) => void;
  onHeaderEdgeScrollChange?: (scrolled: boolean) => void;
  dragOver: boolean;
  onDragOver: (event: DragEvent<HTMLDivElement>) => void;
  onDragLeave: () => void;
  onDrop: (event: DragEvent<HTMLDivElement>) => void;
  footer: ReactNode;
  dropEnabled?: boolean;
  emptyLabel?: string;
  backdrop?: ReactNode;
  emptyState?: "new-task" | "loading-task" | "empty-task";
  onOperatorRequestResponse: (message: ChatRenderMessage, response: ChatOperatorResponse) => Promise<void>;
  onOpenAssistantReview: () => void;
  onOpenAssistantVisualization: (path: string) => void;
  onOpenFile: (path: string, toolId?: string) => void;
  resolveAssistantVisualization: (path: string) => AssistantVisualizationTarget | null;
  pinnedArtifactIds: string[];
  pinningArtifactIds: string[];
  taskActionsEnabled?: boolean;
  listenToGlobalCommands?: boolean;
  toolPresentations: ChatToolPresentationIndex;
  onTogglePinnedMessage: (message: ChatRenderMessage) => Promise<void>;
  onPinMessageExcerpt: (message: ChatRenderMessage, selection: { text: string; start: number; end: number }) => Promise<void>;
  onContinueInNewTask: (message: ChatRenderMessage) => void;
  onEditUserMessage: (message: ChatRenderMessage, editedText?: string) => boolean | Promise<boolean>;
  onSteerUserMessage: (message: ChatRenderMessage, editedText: string) => Promise<boolean>;
  steerableMessageId: string;
  revealPinnedContext: ChatPinnedContextItem | null;
  responseStreaming: boolean;
  blockingReason?: string;
  onPinnedContextRevealed: () => void;
};

const ACTION_BUTTON = "border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full electron:rounded-md text-tertiary enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover border-transparent electron:p-1 electron:[&>svg]:icon-sm flex items-center justify-center p-0.5 focus-visible:ring-2 focus-visible:ring-token-focus-border focus-visible:ring-offset-0";
const ACTION_BUTTON_ACTIVE = "no-drag cursor-interaction flex items-center justify-center select-none p-0.5 electron:p-1 electron:[&>svg]:icon-sm text-secondary focus:outline-none disabled:cursor-not-allowed disabled:opacity-40";
const MAX_THREAD_RESTORE_STATES = 128;
const THREAD_SCROLL_BOTTOM_THRESHOLD_PX = 24;
const THREAD_SCROLL_LATEST_BUTTON_THRESHOLD_PX = 96;
const THREAD_SCROLL_TO_LATEST_DURATION_MS = 260;
const THREAD_USER_SCROLL_INTENT_WINDOW_MS = 1_000;

function reverseScrollDistance(element: HTMLElement | null | undefined): number {
  return element ? Math.max(0, -element.scrollTop) : 0;
}

function setReverseScrollDistance(
  element: HTMLElement,
  distanceFromBottomPx: number,
  behavior: ScrollBehavior = "instant",
) {
  const distance = Math.max(0, distanceFromBottomPx);
  element.scrollTo({
    behavior,
    top: distance === 0 ? 0 : -distance,
  });
}

function resizeObserverBlockSize(entry: ResizeObserverEntry, fallback: HTMLElement): number {
  const borderBox = entry.borderBoxSize;
  const box = Array.isArray(borderBox) ? borderBox[0] : borderBox;
  return box?.blockSize ?? fallback.getBoundingClientRect().height;
}

type ThreadVirtualizerRestoreState = {
  anchorTurnKey: string | null;
  measuredHeightsByKey: Record<string, number>;
  range: ThreadVirtualizerRange;
};

type ThreadRestoreState = {
  distanceFromBottomPx: number;
  virtualizedTurns: ThreadVirtualizerRestoreState | null;
};

const threadRestoreStates = new Map<string, ThreadRestoreState>();

function readThreadRestoreState(conversationId: string): ThreadRestoreState {
  return threadRestoreStates.get(conversationId) ?? {
    distanceFromBottomPx: 0,
    virtualizedTurns: null,
  };
}

function writeThreadRestoreState(conversationId: string, state: ThreadRestoreState) {
  if (!conversationId) return;
  threadRestoreStates.delete(conversationId);
  threadRestoreStates.set(conversationId, state);
  while (threadRestoreStates.size > MAX_THREAD_RESTORE_STATES) {
    const oldestConversationId = threadRestoreStates.keys().next().value;
    if (typeof oldestConversationId !== "string") break;
    threadRestoreStates.delete(oldestConversationId);
  }
}

function useStableCallback<Arguments extends unknown[], Result>(
  callback: (...args: Arguments) => Result,
): (...args: Arguments) => Result {
  const callbackRef = useRef(callback);
  useLayoutEffect(() => {
    callbackRef.current = callback;
  }, [callback]);
  return useCallback((...args: Arguments) => callbackRef.current(...args), []);
}

export function ChatMessageList({
  activityPresentation = "conversation",
  messages,
  activeMessageCount,
  conversationId,
  contentX = 0,
  findQuery,
  findActiveIndex,
  onFindMatchCountChange,
  onHeaderEdgeScrollChange,
  dragOver,
  onDragOver,
  onDragLeave,
  onDrop,
  footer,
  dropEnabled = true,
  emptyLabel,
  backdrop,
  emptyState = "new-task",
  onOperatorRequestResponse,
  onOpenAssistantReview,
  onOpenAssistantVisualization,
  onOpenFile,
  resolveAssistantVisualization,
  pinnedArtifactIds,
  pinningArtifactIds,
  taskActionsEnabled = true,
  listenToGlobalCommands = true,
  toolPresentations,
  onTogglePinnedMessage,
  onPinMessageExcerpt,
  onContinueInNewTask,
  onEditUserMessage,
  onSteerUserMessage,
  steerableMessageId,
  revealPinnedContext,
  responseStreaming,
  blockingReason = "",
  onPinnedContextRevealed,
}: Props) {
  const stableContinueInNewTask = useStableCallback(onContinueInNewTask);
  const stableEditUserMessage = useStableCallback(onEditUserMessage);
  const stableSteerUserMessage = useStableCallback(onSteerUserMessage);
  const stableOperatorRequestResponse = useStableCallback(onOperatorRequestResponse);
  const stableOpenAssistantReview = useStableCallback(onOpenAssistantReview);
  const stableOpenAssistantVisualization = useStableCallback(onOpenAssistantVisualization);
  const stableOpenFile = useStableCallback(onOpenFile);
  const stablePinMessageExcerpt = useStableCallback(onPinMessageExcerpt);
  const stableResolveAssistantVisualization = useStableCallback(resolveAssistantVisualization);
  const stableTogglePinnedMessage = useStableCallback(onTogglePinnedMessage);
  const stableHeaderEdgeScrollChange = useStableCallback((scrolled: boolean) => onHeaderEdgeScrollChange?.(scrolled));
  const footerVisible = footer != null;
  const renderMessages = useMemo(() => (
    activityPresentation === "ledger"
      ? messages
      : messages.filter((message) => conversationRenderRelevant(message, toolPresentations))
  ), [activityPresentation, messages, toolPresentations]);
  const presentationMessages = useMemo(() => (
    activityPresentation === "ledger"
      ? renderMessages
      : renderMessages.filter((message) => conversationProjectionRelevant(message, toolPresentations))
  ), [activityPresentation, renderMessages, toolPresentations]);
  const searchableMessages = useMemo(() => (
    activityPresentation === "ledger"
      ? messages
      : presentationMessages.filter((message) => conversationSearchVisible(message, toolPresentations))
  ), [activityPresentation, messages, presentationMessages, toolPresentations]);
  const turns = useStableTurns(renderMessages, activityPresentation === "conversation");
  const turnKeyByMessageId = useMemo(() => new Map(turns.flatMap((turn, index) => {
    const key = turnKey(turn, index);
    return turn.map((message) => [message.id, key] as const);
  })), [turns]);
  const presentationMessageCount = presentationMessages.length;
  const effectiveActiveMessageCount = activityPresentation === "ledger"
    ? activeMessageCount
    : presentationMessageCount;
  const threadLayoutActive = effectiveActiveMessageCount > 0 || emptyState !== "new-task";
  const listRef = useRef<HTMLDivElement>(null);
  const footerRef = useRef<HTMLDivElement>(null);
  const footerHeightRef = useRef<number | null>(null);
  const pinnedRef = useRef(true);
  const readingAnchorRef = useRef<{ element: HTMLElement; top: number } | null>(null);
  const programmaticLatestScrollRef = useRef(false);
  const userScrollIntentUntilRef = useRef(0);
  const lastScrollDistanceRef = useRef(0);
  const touchStartRef = useRef<{ identifier: number; clientX: number; clientY: number } | null>(null);
  const latestScrollAnimationFrameRef = useRef<number | null>(null);
  const scrollFrameRef = useRef<number | null>(null);
  const observedMessageCountRef = useRef(presentationMessageCount);
  const initialSnapshotRestoredRef = useRef(emptyState !== "loading-task");
  const headerEdgeScrolledRef = useRef<boolean | null>(null);
  const initialThreadRestoreState = readThreadRestoreState(conversationId);
  const initialVirtualizerRestoreState = initialThreadRestoreState.virtualizedTurns;
  const [activeUserMessageId, setActiveUserMessageId] = useState("");
  const [virtualRevealRequest, setVirtualRevealRequest] = useState<{
    id: string;
    instant: boolean;
    turnKey: string;
  } | null>(null);
  const [scrolledFromLatest, setScrolledFromLatest] = useState(false);
  const [scrollbarActive, setScrollbarActive] = useState(false);
  const [scrollbarAwayFromLatest, setScrollbarAwayFromLatest] = useState(false);
  const scrollbarTimerRef = useRef<number | null>(null);
  const [showScrollToLatest, setShowScrollToLatest] = useState(false);
  const [unseenMessageCount, setUnseenMessageCount] = useState(0);
  const reducedMotion = useShouldReduceMotion();
  const devSearchRevealRef = useRef<(() => void) | null>(null);
  const devSearchHost = useMemo<DevThreadSearchHost>(() => ({
    detachFromLatest() {
      pinnedRef.current = false;
      readingAnchorRef.current = null;
    },
    revealTurn(key) {
      pinnedRef.current = false;
      readingAnchorRef.current = null;
      return new Promise<void>((resolve) => {
        devSearchRevealRef.current?.();
        devSearchRevealRef.current = resolve;
        setVirtualRevealRequest({ id: `__devsearch:${key}`, instant: true, turnKey: key });
        window.setTimeout(() => {
          if (devSearchRevealRef.current !== resolve) return;
          devSearchRevealRef.current = null;
          resolve();
        }, 1_500);
      });
    },
  }), []);
  const findState = useMemo(() => {
    const query = findQuery.trim();
    let offset = 0;
    const offsets = new Map<string, number>();
    for (const message of searchableMessages) {
      const count = query ? countSearchMatches(searchableMessageText(message.text), query) : 0;
      offsets.set(message.id, offset);
      offset += count;
    }
    return { offsets, query, total: offset };
  }, [findQuery, searchableMessages]);
  const getLastScrollDistanceFromBottomPx = useCallback(() => lastScrollDistanceRef.current, []);
  const scrollToDistanceFromBottomPx = useCallback((distanceFromBottomPx: number) => {
    const distance = Math.max(0, distanceFromBottomPx);
    lastScrollDistanceRef.current = distance;
    const list = listRef.current;
    if (list) setReverseScrollDistance(list, distance);
  }, []);
  const preserveReadingAnchor = useCallback(() => {
    const anchor = readingAnchorRef.current;
    const list = listRef.current;
    if (!anchor || !list) return false;
    if (!list.contains(anchor.element)) {
      readingAnchorRef.current = null;
      return false;
    }
    const displacement = anchor.element.getBoundingClientRect().top - list.getBoundingClientRect().top - anchor.top;
    if (Math.abs(displacement) > 0.5) {
      scrollToDistanceFromBottomPx(reverseScrollDistance(list) - displacement);
    }
    return true;
  }, [scrollToDistanceFromBottomPx]);
  const updateHeaderEdgeScroll = useCallback(() => {
    const list = listRef.current;
    const distanceFromTop = list ? Math.max(0, list.scrollHeight - list.clientHeight + list.scrollTop) : 0;
    const scrolled = distanceFromTop > 1;
    if (headerEdgeScrolledRef.current === scrolled) return;
    headerEdgeScrolledRef.current = scrolled;
    stableHeaderEdgeScrollChange(scrolled);
  }, [stableHeaderEdgeScrollChange]);

  function revealScrollbar() {
    setScrollbarActive(true);
    if (scrollbarTimerRef.current !== null) window.clearTimeout(scrollbarTimerRef.current);
    scrollbarTimerRef.current = window.setTimeout(() => {
      scrollbarTimerRef.current = null;
      setScrollbarActive(false);
    }, 600);
  }

  useLayoutEffect(() => {
    if (scrollbarTimerRef.current !== null) window.clearTimeout(scrollbarTimerRef.current);
    scrollbarTimerRef.current = null;
    setScrollbarActive(false);
    if (scrollFrameRef.current !== null) {
      window.cancelAnimationFrame(scrollFrameRef.current);
      scrollFrameRef.current = null;
    }
    const restoreState = readThreadRestoreState(conversationId);
    const restoreDistance = restoreState.distanceFromBottomPx > THREAD_SCROLL_BOTTOM_THRESHOLD_PX
      ? restoreState.distanceFromBottomPx
      : 0;
    const hasInitialSnapshot = emptyState !== "loading-task";
    pinnedRef.current = restoreDistance === 0;
    readingAnchorRef.current = null;
    programmaticLatestScrollRef.current = false;
    userScrollIntentUntilRef.current = 0;
    lastScrollDistanceRef.current = restoreDistance;
    touchStartRef.current = null;
    observedMessageCountRef.current = hasInitialSnapshot ? presentationMessageCount : 0;
    initialSnapshotRestoredRef.current = hasInitialSnapshot;
    setActiveUserMessageId("");
    setVirtualRevealRequest(null);
    setScrolledFromLatest(restoreDistance > 0);
    setScrollbarAwayFromLatest(restoreDistance > 1);
    setShowScrollToLatest(restoreDistance > THREAD_SCROLL_LATEST_BUTTON_THRESHOLD_PX);
    setUnseenMessageCount(0);
    const list = listRef.current;
    if (list) setReverseScrollDistance(list, restoreDistance);
    updateHeaderEdgeScroll();
    return () => {
      if (!list || !conversationId) return;
      const current = readThreadRestoreState(conversationId);
      writeThreadRestoreState(conversationId, {
        ...current,
        distanceFromBottomPx: lastScrollDistanceRef.current,
      });
    };
  }, [conversationId, updateHeaderEdgeScroll]);

  useLayoutEffect(() => {
    if (initialSnapshotRestoredRef.current || emptyState === "loading-task") return;
    const restoreState = readThreadRestoreState(conversationId);
    const restoreDistance = restoreState.distanceFromBottomPx > THREAD_SCROLL_BOTTOM_THRESHOLD_PX
      ? restoreState.distanceFromBottomPx
      : 0;
    pinnedRef.current = restoreDistance === 0;
    lastScrollDistanceRef.current = restoreDistance;
    observedMessageCountRef.current = presentationMessageCount;
    initialSnapshotRestoredRef.current = true;
    setScrolledFromLatest(restoreDistance > 0);
    setShowScrollToLatest(restoreDistance > THREAD_SCROLL_LATEST_BUTTON_THRESHOLD_PX);
    const list = listRef.current;
    if (list) setReverseScrollDistance(list, restoreDistance);
  }, [conversationId, emptyState, presentationMessageCount]);

  useEffect(() => {
    onFindMatchCountChange(findState.total);
  }, [findState.total, onFindMatchCountChange]);

  useLayoutEffect(() => {
    if (!findState.query || findState.total <= 0) return;
    const frame = window.requestAnimationFrame(() => {
      const active = listRef.current?.querySelector<HTMLElement>(`[data-octopus-thread-find-index="${findActiveIndex}"]`);
      if (active) {
        pinnedRef.current = false;
        active.scrollIntoView({ block: "center", behavior: reducedMotion ? "instant" : "smooth" });
        return;
      }
      const containing = searchableMessages.find((message) => {
        const start = findState.offsets.get(message.id) ?? 0;
        const count = countSearchMatches(searchableMessageText(message.text), findState.query);
        return findActiveIndex >= start && findActiveIndex < start + count;
      });
      const key = containing ? turnKeyByMessageId.get(containing.id) : "";
      if (key) {
        setVirtualRevealRequest({
          id: `__find:${findActiveIndex}`,
          instant: reducedMotion,
          turnKey: key,
        });
      }
    });
    return () => window.cancelAnimationFrame(frame);
  }, [findActiveIndex, findState, reducedMotion, searchableMessages, turnKeyByMessageId]);

  function onScroll() {
    const list = listRef.current;
    if (!list || emptyState === "loading-task") return;
    updateHeaderEdgeScroll();
    const distanceFromLatest = reverseScrollDistance(list);
    setScrollbarAwayFromLatest(!programmaticLatestScrollRef.current && distanceFromLatest > 1);
    const intentIsCurrent = performance.now() <= userScrollIntentUntilRef.current;
    if (intentIsCurrent) revealScrollbar();
    if (programmaticLatestScrollRef.current && distanceFromLatest > 1) {
      lastScrollDistanceRef.current = distanceFromLatest;
      pinnedRef.current = true;
      setScrolledFromLatest(false);
      setShowScrollToLatest(false);
      setUnseenMessageCount(0);
      return;
    }
    if (distanceFromLatest <= 1) programmaticLatestScrollRef.current = false;
    if (readingAnchorRef.current) {
      pinnedRef.current = false;
    } else if (distanceFromLatest <= THREAD_SCROLL_BOTTOM_THRESHOLD_PX) {
      pinnedRef.current = true;
    } else if (intentIsCurrent || !pinnedRef.current) {
      pinnedRef.current = false;
    }
    const trackedDistance = pinnedRef.current ? 0 : distanceFromLatest;
    lastScrollDistanceRef.current = trackedDistance;
    if (conversationId) {
      const current = readThreadRestoreState(conversationId);
      writeThreadRestoreState(conversationId, {
        ...current,
        distanceFromBottomPx: trackedDistance,
      });
    }
    if (pinnedRef.current) {
      setScrolledFromLatest(false);
      setShowScrollToLatest(false);
      setUnseenMessageCount(0);
    } else {
      setScrolledFromLatest(distanceFromLatest > 1);
      setShowScrollToLatest(distanceFromLatest > THREAD_SCROLL_LATEST_BUTTON_THRESHOLD_PX);
    }
    if (pinnedRef.current) observedMessageCountRef.current = presentationMessageCount;
    if (scrollFrameRef.current !== null) return;
    scrollFrameRef.current = window.requestAnimationFrame(() => {
      scrollFrameRef.current = null;
      const active = nearestUserMessage(list);
      if (active) setActiveUserMessageId(active);
    });
  }

  useEffect(() => () => {
    if (scrollbarTimerRef.current !== null) window.clearTimeout(scrollbarTimerRef.current);
    if (scrollFrameRef.current !== null) window.cancelAnimationFrame(scrollFrameRef.current);
    if (latestScrollAnimationFrameRef.current !== null) {
      window.cancelAnimationFrame(latestScrollAnimationFrameRef.current);
    }
  }, []);

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const content = list.firstElementChild;
    const update = () => updateHeaderEdgeScroll();
    updateHeaderEdgeScroll();
    const observer = new ResizeObserver(update);
    observer.observe(list);
    if (content instanceof HTMLElement) observer.observe(content);
    return () => observer.disconnect();
  }, [conversationId, presentationMessageCount, updateHeaderEdgeScroll]);

  useEffect(() => {
    const list = listRef.current;
    if (!list) return;
    if (!initialSnapshotRestoredRef.current) return;
    const previousCount = observedMessageCountRef.current;
    observedMessageCountRef.current = presentationMessageCount;
    if (!pinnedRef.current) {
      const added = Math.max(0, presentationMessageCount - previousCount);
      if (added) setUnseenMessageCount((current) => current + added);
      return;
    }
    setScrolledFromLatest(false);
    setShowScrollToLatest(false);
    setUnseenMessageCount(0);
  }, [presentationMessageCount]);

  const onVirtualizerRestoreStateChange = useCallback((state: ThreadVirtualizerRestoreState) => {
    if (!conversationId) return;
    const current = readThreadRestoreState(conversationId);
    writeThreadRestoreState(conversationId, {
      ...current,
      virtualizedTurns: state,
    });
  }, [conversationId]);
  const shouldFollowLatest = useCallback(() => pinnedRef.current, []);

  useLayoutEffect(() => {
    const list = listRef.current;
    const footerElement = footerRef.current;
    if (!list || !footerElement) return;

    const updateFooterGeometry = (entry?: ResizeObserverEntry) => {
      const nextHeight = entry
        ? resizeObserverBlockSize(entry, footerElement)
        : footerElement.getBoundingClientRect().height;
      const previousHeight = footerHeightRef.current;
      footerHeightRef.current = nextHeight;
      list.style.setProperty("--thread-scroll-padding-bottom", `${nextHeight + 16}px`);

      if (previousHeight == null || previousHeight === nextHeight) return;
      if (preserveReadingAnchor()) return;
      if (programmaticLatestScrollRef.current || pinnedRef.current) {
        scrollToDistanceFromBottomPx(0);
        return;
      }
      const distanceFromBottomPx = lastScrollDistanceRef.current;
      if (distanceFromBottomPx <= THREAD_SCROLL_BOTTOM_THRESHOLD_PX) return;
      scrollToDistanceFromBottomPx(distanceFromBottomPx + nextHeight - previousHeight);
    };

    updateFooterGeometry();
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) updateFooterGeometry(entry);
    });
    observer.observe(footerElement);
    return () => {
      observer.disconnect();
      footerHeightRef.current = null;
    };
  }, [footerVisible, preserveReadingAnchor, scrollToDistanceFromBottomPx, threadLayoutActive]);

  const navigationItems = turns.flatMap((turn) => {
    const prompt = turn.find((message) => message.role === "user" && message.eventKind !== "steering");
    if (!prompt) return [];
    const response = turn.find((message) => (
      message.role === "assistant"
      && message.text
      && (activityPresentation === "ledger" || conversationSearchVisible(message, toolPresentations))
    ));
    return [{ id: prompt.id, label: prompt.text || "(No content)", preview: response?.text || "" }];
  });

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list || effectiveActiveMessageCount === 0 || navigationItems.length === 0) return;
    const frame = window.requestAnimationFrame(() => {
      setActiveUserMessageId(nearestUserMessage(list) || navigationItems.at(-1)?.id || "");
    });
    return () => window.cancelAnimationFrame(frame);
  }, [effectiveActiveMessageCount, navigationItems.length]);

  useEffect(() => subscribeClientCommands((command) => {
    if (!listenToGlobalCommands) return;
    if (command === "scroll_to_latest") {
      scrollToLatest();
      return;
    }
    if (command !== "previous_operator_message" && command !== "next_operator_message") return;
    if (!navigationItems.length) return;
    const activeIndex = Math.max(0, navigationItems.findIndex((item) => item.id === activeUserMessageId));
    const delta = command === "previous_operator_message" ? -1 : 1;
    const targetIndex = Math.min(navigationItems.length - 1, Math.max(0, activeIndex + delta));
    const target = navigationItems[targetIndex];
    if (target) revealUserMessage(target.id);
  }), [activeUserMessageId, listenToGlobalCommands, navigationItems, reducedMotion]);

  useLayoutEffect(() => {
    if (!revealPinnedContext) return;
    const list = listRef.current;
    const escaped = globalThis.CSS?.escape
      ? globalThis.CSS.escape(revealPinnedContext.sourceMessageId)
      : revealPinnedContext.sourceMessageId.replace(/["\\]/g, "\\$&");
    const target = list?.querySelector<HTMLElement>(`[data-message-id="${escaped}"]`);
    const textRoot = target?.matches("[data-message-text]")
      ? target
      : target?.querySelector<HTMLElement>("[data-message-text]");
    if (!target || !textRoot) {
      const key = turnKeyByMessageId.get(revealPinnedContext.sourceMessageId);
      if (key) {
        setVirtualRevealRequest((current) => current?.id === `__pinned:${revealPinnedContext.sourceMessageId}`
          ? current
          : {
              id: `__pinned:${revealPinnedContext.sourceMessageId}`,
              instant: reducedMotion,
              turnKey: key,
            });
        return;
      }
      onPinnedContextRevealed();
      return;
    }
    pinnedRef.current = false;
    target.scrollIntoView({ block: "center", behavior: reducedMotion ? "instant" : "smooth" });
    textRoot.animate?.(
      [
        { backgroundColor: "transparent" },
        { backgroundColor: "color-mix(in srgb, var(--color-token-foreground) 12%, transparent)", offset: 0.25 },
        { backgroundColor: "color-mix(in srgb, var(--color-token-foreground) 12%, transparent)", offset: 0.55 },
        { backgroundColor: "transparent" },
      ],
      { duration: reducedMotion ? 0 : 1_800, easing: "ease-in-out" },
    );
    onPinnedContextRevealed();
  }, [onPinnedContextRevealed, reducedMotion, revealPinnedContext, turnKeyByMessageId, virtualRevealRequest]);

  if (effectiveActiveMessageCount === 0 && emptyState === "new-task") {
    return (
      <div
        className={`octopus-chat-scroll octopus-chat-home [container-type:size] flex min-h-0 w-full flex-1 flex-col overflow-y-auto [scrollbar-gutter:stable_both-edges] ${dragOver ? "is-drag-over" : ""}`}
        onDragLeave={dropEnabled ? onDragLeave : undefined}
        onDragOver={dropEnabled ? onDragOver : undefined}
        onDrop={dropEnabled ? onDrop : undefined}
        ref={listRef}
      >
        {backdrop ? <div aria-hidden="true" className="octopus-chat-backdrop">{backdrop}</div> : null}
        <div className="relative z-[1] mx-auto flex min-h-full w-full max-w-3xl flex-col justify-center gap-3 px-panel py-6">
          <div
            className="flex max-w-full min-w-0 flex-col items-center gap-4 text-center select-none"
            data-ocean-obstacle=""
          >
            {footer ? <InteractiveHeroMark /> : <Glyph className="text-token-text-tertiary" name="chat" size={22} />}
            <h1 className="heading-xl font-normal whitespace-pre-wrap text-token-foreground">
              {emptyLabel || "What should Octopus work on?"}
            </h1>
          </div>
          {footer ? (
            <div className="mt-6 flex min-w-0 flex-col gap-2" data-ocean-obstacle="">{footer}</div>
          ) : null}
        </div>
      </div>
    );
  }

  function revealUserMessage(id: string, instant = false, virtualRetry = false) {
    const list = listRef.current;
    if (!list) return;
    const target = [...list.querySelectorAll<HTMLElement>("[data-thread-user-message-id]")]
      .find((element) => element.dataset.threadUserMessageId === id);
    if (!target) {
      if (!virtualRetry) {
        const key = turnKeyByMessageId.get(id);
        if (key) setVirtualRevealRequest({ id, instant, turnKey: key });
      }
      return;
    }
    setVirtualRevealRequest(null);
    pinnedRef.current = false;
    target.scrollIntoView({ block: "center", behavior: instant || reducedMotion ? "instant" : "smooth" });
    setActiveUserMessageId(id);
    const bubble = target.querySelector<HTMLElement>("[data-user-message-bubble]");
    bubble?.animate?.(
      [
        { backgroundColor: "color-mix(in srgb, var(--color-token-foreground) 13%, transparent)" },
        { backgroundColor: "color-mix(in srgb, var(--color-token-foreground) 13%, transparent)", offset: 0.35 },
        { backgroundColor: "color-mix(in srgb, var(--color-token-foreground) 5%, transparent)" },
      ],
      { duration: reducedMotion ? 0 : 1_400, easing: "cubic-bezier(0.23, 1, 0.32, 1)" },
    );
  }

  function scrollToLatest() {
    const list = listRef.current;
    if (!list) return;
    pinnedRef.current = true;
    readingAnchorRef.current = null;
    programmaticLatestScrollRef.current = true;
    userScrollIntentUntilRef.current = 0;
    setScrolledFromLatest(false);
    setShowScrollToLatest(false);
    setUnseenMessageCount(0);

    if (latestScrollAnimationFrameRef.current !== null) {
      window.cancelAnimationFrame(latestScrollAnimationFrameRef.current);
      latestScrollAnimationFrameRef.current = null;
    }
    const initialDistance = reverseScrollDistance(list);
    if (reducedMotion || initialDistance <= THREAD_SCROLL_BOTTOM_THRESHOLD_PX) {
      scrollToDistanceFromBottomPx(0);
      programmaticLatestScrollRef.current = false;
      return;
    }

    const startedAtMs = performance.now();
    const step = (nowMs: number) => {
      const progress = Math.min(1, (nowMs - startedAtMs) / THREAD_SCROLL_TO_LATEST_DURATION_MS);
      scrollToDistanceFromBottomPx(initialDistance * ((1 - progress) ** 3));
      if (progress < 1 && reverseScrollDistance(list) > THREAD_SCROLL_BOTTOM_THRESHOLD_PX) {
        latestScrollAnimationFrameRef.current = window.requestAnimationFrame(step);
        return;
      }
      scrollToDistanceFromBottomPx(0);
      latestScrollAnimationFrameRef.current = null;
      programmaticLatestScrollRef.current = false;
    };
    latestScrollAnimationFrameRef.current = window.requestAnimationFrame(step);
  }

  function stopProgrammaticLatestScroll(detachFromLatest = false, scrolling = true) {
    if (scrolling || detachFromLatest) readingAnchorRef.current = null;
    if (scrolling) revealScrollbar();
    userScrollIntentUntilRef.current = performance.now() + THREAD_USER_SCROLL_INTENT_WINDOW_MS;
    if (detachFromLatest) {
      pinnedRef.current = false;
    }
    programmaticLatestScrollRef.current = false;
    if (latestScrollAnimationFrameRef.current !== null) {
      window.cancelAnimationFrame(latestScrollAnimationFrameRef.current);
      latestScrollAnimationFrameRef.current = null;
    }
  }

  return (
    <div className="relative h-full flex-1 [content-visibility:auto]">
      {import.meta.env?.DEV && listenToGlobalCommands ? (
        <DevThreadSearch host={devSearchHost} listRef={listRef} turnKeyOf={turnKey} turns={turns} />
      ) : null}
      <UserMessageNavigationRail
        activeId={activeUserMessageId}
        items={navigationItems}
        onReveal={revealUserMessage}
      />
      <div
        className={`octopus-chat-scroll thread-scroll-container h-full overflow-x-hidden overflow-y-auto [overflow-anchor:none] [scroll-padding-bottom:var(--thread-scroll-padding-bottom,0px)] electron:[scrollbar-gutter:stable_both-edges] pt-(--thread-content-top-inset) [container-name:thread-content] [container-type:inline-size] [&:has([data-thread-scroll-footer='true']:focus-within)]:[scroll-padding-bottom:0px] flex flex-col-reverse focus:outline-none${scrolledFromLatest ? " is-scrolled" : ""}${dragOver ? " is-drag-over" : ""}`}
        data-scrollbar-visible={scrollbarAwayFromLatest || scrollbarActive || undefined}
        onClickCapture={(event) => {
          const target = event.target;
          const disclosure = target instanceof Element ? target.closest<HTMLElement>("button[aria-expanded], summary") : null;
          if (!disclosure || !disclosure.closest("[data-thread-turn-key]")) return;
          stopProgrammaticLatestScroll(true, false);
          lastScrollDistanceRef.current = reverseScrollDistance(event.currentTarget);
          readingAnchorRef.current = {
            element: disclosure,
            top: disclosure.getBoundingClientRect().top - event.currentTarget.getBoundingClientRect().top,
          };
        }}
        onDragLeave={dropEnabled ? onDragLeave : undefined}
        onDragOver={dropEnabled ? onDragOver : undefined}
        onDrop={dropEnabled ? onDrop : undefined}
        onKeyDown={(event) => {
          const target = event.target;
          if (target instanceof HTMLElement && target !== event.currentTarget && (
            target.isContentEditable
            || target.closest("input, select, textarea") != null
            || ((event.key === " " || event.key === "Spacebar") && target.closest("button, [role='button']") != null)
          )) return;
          const away = event.key === "ArrowUp"
            || event.key === "Home"
            || event.key === "PageUp"
            || ((event.key === " " || event.key === "Spacebar") && event.shiftKey);
          const toward = event.key === "ArrowDown"
            || event.key === "End"
            || event.key === "PageDown"
            || ((event.key === " " || event.key === "Spacebar") && !event.shiftKey);
          if (!away && !toward) return;
          stopProgrammaticLatestScroll(away);
        }}
        onPointerDown={(event) => stopProgrammaticLatestScroll(
          event.pointerType === "mouse" && event.target === event.currentTarget,
          event.target === event.currentTarget,
        )}
        onScroll={onScroll}
        onTouchCancel={() => { touchStartRef.current = null; }}
        onTouchEnd={() => { touchStartRef.current = null; }}
        onTouchMove={(event) => {
          const start = touchStartRef.current;
          const touch = event.touches.length === 1 ? event.touches[0] : null;
          if (!start || !touch || touch.identifier !== start.identifier) {
            touchStartRef.current = null;
            return;
          }
          const deltaX = touch.clientX - start.clientX;
          const deltaY = touch.clientY - start.clientY;
          if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < 8) return;
          touchStartRef.current = null;
          if (Math.abs(deltaY) > Math.abs(deltaX)) stopProgrammaticLatestScroll(deltaY > 0);
        }}
        onTouchStart={(event) => {
          stopProgrammaticLatestScroll();
          const touch = event.touches.length === 1 ? event.touches[0] : null;
          touchStartRef.current = touch
            ? { identifier: touch.identifier, clientX: touch.clientX, clientY: touch.clientY }
            : null;
        }}
        onWheel={(event) => stopProgrammaticLatestScroll(event.deltaY < 0)}
        ref={listRef}
        tabIndex={0}
      >
        <motion.div
          animate={{ x: contentX }}
          className="flex min-h-full shrink-0 flex-col justify-start overflow-x-clip"
          initial={false}
          style={{ "--thread-wide-block-inline-shift": `${Math.abs(contentX)}px` } as CSSProperties}
          transition={reducedMotion ? { duration: 0 } : { bounce: 0.01, duration: 0.3, type: "spring" }}
        >
          <div className="relative mx-auto flex w-full max-w-(--thread-content-max-width) flex-1 shrink-0 flex-col px-toolbar pb-8">
            <div className="relative flex flex-1 flex-col gap-3 electron:[--color-token-description-foreground:color-mix(in_srgb,var(--color-token-foreground)_70%,transparent)]">
              {emptyState !== "loading-task" ? <VirtualizedTurns
                initialDistanceFromBottomPx={initialThreadRestoreState.distanceFromBottomPx}
                initialRestoreState={initialVirtualizerRestoreState}
                key={conversationId || "new-task"}
                onRevealReady={(request) => {
                  setVirtualRevealRequest(null);
                  if (request.id.startsWith("__find:")) {
                    const target = listRef.current?.querySelector<HTMLElement>(`[data-octopus-thread-find-index="${findActiveIndex}"]`);
                    pinnedRef.current = false;
                    target?.scrollIntoView({ block: "center", behavior: request.instant ? "instant" : "smooth" });
                    return;
                  }
                  if (request.id.startsWith("__pinned:")) return;
                  if (request.id.startsWith("__devsearch:")) {
                    const resolve = devSearchRevealRef.current;
                    devSearchRevealRef.current = null;
                    pinnedRef.current = false;
                    listRef.current?.querySelector<HTMLElement>(`[data-thread-turn-key="${CSS.escape(request.turnKey)}"]`)
                      ?.scrollIntoView({ block: "start", behavior: "instant" });
                    resolve?.();
                    return;
                  }
                  revealUserMessage(request.id, request.instant, true);
                }}
                onRestoreStateChange={onVirtualizerRestoreStateChange}
                revealRequest={virtualRevealRequest}
                getLastScrollDistanceFromBottomPx={getLastScrollDistanceFromBottomPx}
                scrollElementRef={listRef}
                scrollToDistanceFromBottomPx={scrollToDistanceFromBottomPx}
                shouldFollowLatest={shouldFollowLatest}
                preserveReadingAnchor={preserveReadingAnchor}
                turns={turns}
                renderTurn={(turn, turnIndex) => (
                  <ChatRenderBoundary
                    detail="The rest of the projected task is still available. Retry this local render or copy the details for diagnosis."
                    resetKey={turnRenderResetKey(turn)}
                    title="This turn couldn't render"
                  >
                    <MemoizedChatTurn
                      activityPresentation={activityPresentation}
                      findActiveIndex={findActiveIndex}
                      findState={findState}
                      isLatestTurn={turnIndex === turns.length - 1}
                      onContinueInNewTask={stableContinueInNewTask}
                      onEditUserMessage={stableEditUserMessage}
                      onSteerUserMessage={stableSteerUserMessage}
                      steerableMessageId={steerableMessageId}
                      onOperatorRequestResponse={stableOperatorRequestResponse}
                      onOpenAssistantReview={stableOpenAssistantReview}
                      onOpenAssistantVisualization={stableOpenAssistantVisualization}
                      onOpenFile={stableOpenFile}
                      onPinMessageExcerpt={stablePinMessageExcerpt}
                      onTogglePinnedMessage={stableTogglePinnedMessage}
                      pinnedArtifactIds={pinnedArtifactIds}
                      pinningArtifactIds={pinningArtifactIds}
                      responseStreaming={responseStreaming && turnIndex === turns.length - 1}
                      blockingReason={turnIndex === turns.length - 1 && responseStreaming ? blockingReason : ""}
                      resolveAssistantVisualization={stableResolveAssistantVisualization}
                      taskActionsEnabled={taskActionsEnabled}
                      toolPresentations={toolPresentations}
                      turn={turn}
                    />
                  </ChatRenderBoundary>
                )}
              /> : null}
              {presentationMessageCount === 0 && emptyState === "loading-task" ? (
                <OctopusLoadingState
                  className="flex min-h-28 flex-1 items-center justify-center"
                  label="Loading task"
                />
              ) : null}
              {presentationMessageCount === 0 && emptyState !== "loading-task" && emptyLabel ? (
                <div className="flex min-h-28 flex-1 flex-col items-center justify-center gap-3 text-token-text-secondary">
                  <Glyph className="text-token-text-tertiary" name="chat" size={22} />
                  <span>{emptyLabel}</span>
                </div>
              ) : null}
            </div>
          </div>
          {footer ? (
            <div
              aria-hidden
              className="sticky bottom-0 z-10 mt-auto h-[calc(var(--thread-scroll-padding-bottom)-var(--spacing)*4)] w-full shrink-0"
            >
              <div className="pointer-events-none absolute inset-x-0 bottom-0 z-0 h-full bg-gradient-to-t from-token-main-surface-primary via-token-main-surface-primary" />
            </div>
          ) : null}
        </motion.div>
        {footer ? (
          <motion.div
            animate={{ x: contentX }}
            className="pointer-events-none absolute inset-x-0 bottom-0 z-10 pb-4"
            data-thread-scroll-footer="true"
            initial={false}
            ref={footerRef}
            style={{ "--thread-wide-block-inline-shift": `${Math.abs(contentX)}px` } as CSSProperties}
            transition={reducedMotion ? { duration: 0 } : { bounce: 0.01, duration: 0.3, type: "spring" }}
          >
            <div className="relative z-10 flex flex-col *:pointer-events-auto">
              <div className="relative h-0">
                <button
                  aria-hidden={!showScrollToLatest}
                  aria-label={unseenMessageCount ? `Scroll to bottom · ${unseenMessageCount} new ${unseenMessageCount === 1 ? "message" : "messages"}` : "Scroll to bottom"}
                  className={`cursor-interaction absolute z-30 flex h-8 w-8 translate-x-1/2 items-center justify-center rounded-full border border-token-border-default bg-token-main-surface-primary bg-clip-padding text-token-text-secondary transition-opacity duration-basic ease-in-out end-1/2 print:hidden bottom-[calc(100%+6*var(--spacing))] ${showScrollToLatest ? "opacity-100" : "pointer-events-none opacity-0"}`}
                  onClick={showScrollToLatest ? scrollToLatest : undefined}
                  tabIndex={showScrollToLatest ? undefined : -1}
                  type="button"
                >
                  <Glyph className="icon rotate-180 text-token-text-primary" name="send" size={20} />
                  {unseenMessageCount ? (
                    <span className="absolute -top-1.5 -right-1.5 flex min-w-4 items-center justify-center rounded-full bg-token-foreground px-1 text-[10px] leading-4 text-token-main-surface-primary tabular-nums">
                      {unseenMessageCount > 99 ? "99+" : unseenMessageCount}
                    </span>
                  ) : null}
                </button>
              </div>
              {footer}
            </div>
          </motion.div>
        ) : null}
      </div>
    </div>
  );
}

const THREAD_VIRTUALIZATION_MIN_TURNS = 40;
const THREAD_VIRTUALIZATION_OVERSCAN_TURNS = 4;
const THREAD_TURN_GAP_PX = 12;
const threadTurnEstimateCache = new WeakMap<ChatRenderMessage[], number>();
const threadTurnResetKeyCache = new WeakMap<ChatRenderMessage[], string>();

function turnRenderResetKey(turn: ChatRenderMessage[]): string {
  const cached = threadTurnResetKeyCache.get(turn);
  if (cached !== undefined) return cached;
  const key = turn
    .map((message) => `${message.id}:${message.text.length}:${message.pending === true ? 1 : 0}`)
    .join("|");
  threadTurnResetKeyCache.set(turn, key);
  return key;
}

function initialThreadVirtualizerRange({
  layout,
  restoreState,
  turnCount,
}: {
  layout: ReturnType<typeof buildThreadVirtualizerLayout>;
  restoreState: ThreadVirtualizerRestoreState | null;
  turnCount: number;
}): ThreadVirtualizerRange {
  if (turnCount < THREAD_VIRTUALIZATION_MIN_TURNS) {
    return { startIndex: 0, endIndex: turnCount };
  }
  const restoredAnchorIndex = restoreState?.anchorTurnKey
    ? layout.turnIndexByKey.get(restoreState.anchorTurnKey)
    : undefined;
  if (restoredAnchorIndex !== undefined) {
    const restoredSpan = Math.max(
      1,
      (restoreState?.range.endIndex ?? 0) - (restoreState?.range.startIndex ?? 0),
    );
    return {
      startIndex: restoredAnchorIndex,
      endIndex: Math.min(turnCount, restoredAnchorIndex + restoredSpan),
    };
  }
  if (restoreState) {
    const startIndex = Math.max(0, Math.min(restoreState.range.startIndex, turnCount - 1));
    const endIndex = Math.max(
      startIndex + 1,
      Math.min(restoreState.range.endIndex, turnCount),
    );
    return { startIndex, endIndex };
  }
  return {
    startIndex: Math.max(0, turnCount - 12),
    endIndex: turnCount,
  };
}

function VirtualizedTurns({
  getLastScrollDistanceFromBottomPx,
  initialDistanceFromBottomPx,
  initialRestoreState,
  onRevealReady,
  onRestoreStateChange,
  preserveReadingAnchor,
  revealRequest,
  renderTurn,
  scrollElementRef,
  scrollToDistanceFromBottomPx,
  shouldFollowLatest,
  turns,
}: {
  getLastScrollDistanceFromBottomPx: () => number;
  initialDistanceFromBottomPx: number;
  initialRestoreState: ThreadVirtualizerRestoreState | null;
  onRevealReady: (request: { id: string; instant: boolean; turnKey: string }) => void;
  onRestoreStateChange: (state: ThreadVirtualizerRestoreState) => void;
  preserveReadingAnchor?: () => boolean;
  revealRequest: { id: string; instant: boolean; turnKey: string } | null;
  renderTurn: (turn: ChatRenderMessage[], turnIndex: number) => ReactNode;
  scrollElementRef: React.RefObject<HTMLDivElement | null>;
  scrollToDistanceFromBottomPx: (distanceFromBottomPx: number) => void;
  shouldFollowLatest: () => boolean;
  turns: ChatRenderMessage[][];
}) {
  const virtualized = turns.length >= THREAD_VIRTUALIZATION_MIN_TURNS;
  const entries = useMemo(() => turns.map((turn, index) => ({
    estimatedHeightPx: estimatedTurnHeight(turn),
    turnKey: turnKey(turn, index),
  })), [turns]);
  const [measuredHeightsByKey, setMeasuredHeightsByKey] = useState<Record<string, number>>(
    () => initialRestoreState?.measuredHeightsByKey ?? {},
  );
  const measuredHeightsByKeyRef = useRef(measuredHeightsByKey);
  const turnKeyByElementRef = useRef(new Map<Element, string>());
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const layout = useMemo(() => buildThreadVirtualizerLayout({
    entries,
    gapPx: THREAD_TURN_GAP_PX,
    measuredHeightsByKey,
  }), [entries, measuredHeightsByKey]);
  const layoutRef = useRef(layout);
  const initialDistanceFromBottomRef = useRef<number | null>(initialDistanceFromBottomPx);
  const [range, setRange] = useState<ThreadVirtualizerRange>(() => initialThreadVirtualizerRange({
    layout,
    restoreState: initialRestoreState,
    turnCount: turns.length,
  }));

  const updateRange = useCallback(() => {
    if (!virtualized) {
      setRange((current) => current.startIndex === 0 && current.endIndex === turns.length
        ? current
        : { startIndex: 0, endIndex: turns.length });
      return;
    }
    const scrollElement = scrollElementRef.current;
    const initialDistance = initialDistanceFromBottomRef.current;
    const next = threadVirtualizerRange({
      distanceFromBottomPx: initialDistance ?? reverseScrollDistance(scrollElement),
      layout: layoutRef.current,
      overscanCount: THREAD_VIRTUALIZATION_OVERSCAN_TURNS,
      viewportHeightPx: scrollElement?.clientHeight ?? 800,
    });
    setRange((current) => current.startIndex === next.startIndex && current.endIndex === next.endIndex
      ? current
      : next);
    initialDistanceFromBottomRef.current = null;
  }, [scrollElementRef, turns.length, virtualized]);

  useLayoutEffect(() => {
    const previousLayout = layoutRef.current;
    const anchorKey = previousLayout.turnKeys[range.startIndex];
    layoutRef.current = layout;
    if (previousLayout !== layout && !preserveReadingAnchor?.()) {
      if (shouldFollowLatest()) {
        scrollToDistanceFromBottomPx(0);
      } else {
        const distanceFromBottomPx = getLastScrollDistanceFromBottomPx();
        const visibleRange = threadVirtualizerRange({
          distanceFromBottomPx,
          layout: previousLayout,
          overscanCount: 0,
          viewportHeightPx: scrollElementRef.current?.clientHeight ?? 800,
        });
        const visibleAnchorKey = previousLayout.turnKeys
          .slice(visibleRange.startIndex, visibleRange.endIndex)
          .find((key) => layout.turnIndexByKey.has(key));
        if (visibleAnchorKey) {
          const anchoredDistance = distanceFromBottomAnchoredToTurn({
            anchorKey: visibleAnchorKey,
            distanceFromBottomPx,
            nextLayout: layout,
            previousLayout,
          });
          if (anchoredDistance != null && anchoredDistance !== distanceFromBottomPx) {
            scrollToDistanceFromBottomPx(anchoredDistance);
          }
        }
      }
    }
    if (anchorKey && !layout.turnIndexByKey.has(anchorKey)) {
      updateRange();
      return;
    }
    if (anchorKey && turns.length !== previousLayout.turnKeys.length) {
      const anchored = rangeAnchoredToTurn({ anchorKey, layout, previousRange: range });
      if (anchored) setRange(anchored);
      else updateRange();
      return;
    }
    updateRange();
  }, [
    getLastScrollDistanceFromBottomPx,
    layout,
    preserveReadingAnchor,
    range,
    scrollElementRef,
    scrollToDistanceFromBottomPx,
    shouldFollowLatest,
    turns.length,
    updateRange,
  ]);

  useLayoutEffect(() => {
    onRestoreStateChange({
      anchorTurnKey: layout.turnKeys[range.startIndex] ?? null,
      measuredHeightsByKey,
      range,
    });
  }, [layout.turnKeys, measuredHeightsByKey, onRestoreStateChange, range]);

  useEffect(() => {
    const scrollElement = scrollElementRef.current;
    if (!scrollElement || !virtualized) return;
    let frame: number | null = null;
    const schedule = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        updateRange();
      });
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(scrollElement);
    scrollElement.addEventListener("scroll", schedule, { passive: true });
    schedule();
    return () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      observer.disconnect();
      scrollElement.removeEventListener("scroll", schedule);
    };
  }, [scrollElementRef, updateRange, virtualized]);

  useLayoutEffect(() => {
    if (!revealRequest || !virtualized) return;
    const index = layout.turnIndexByKey.get(revealRequest.turnKey);
    if (index === undefined) return;
    if (index < range.startIndex || index >= range.endIndex) {
      const distance = distanceFromBottomForTurn({
        layout,
        turnKey: revealRequest.turnKey,
        viewportHeightPx: scrollElementRef.current?.clientHeight ?? 800,
      });
      if (distance !== null) scrollToDistanceFromBottomPx(distance);
      setRange({
        startIndex: Math.max(0, index - THREAD_VIRTUALIZATION_OVERSCAN_TURNS),
        endIndex: Math.min(turns.length, index + THREAD_VIRTUALIZATION_OVERSCAN_TURNS + 1),
      });
      return;
    }
    const frame = window.requestAnimationFrame(() => onRevealReady(revealRequest));
    return () => window.cancelAnimationFrame(frame);
  }, [layout, onRevealReady, range.endIndex, range.startIndex, revealRequest, scrollElementRef, scrollToDistanceFromBottomPx, turns.length, virtualized]);

  const commitMeasuredHeights = useCallback((measurements: ReadonlyMap<string, number>) => {
    const current = measuredHeightsByKeyRef.current;
    let next = current;
    for (const [key, heightPx] of measurements) {
      const measured = Math.max(1, Math.ceil(heightPx));
      if (next[key] === measured) continue;
      if (next === current) next = { ...current };
      next[key] = measured;
    }
    if (next === current) return;
    measuredHeightsByKeyRef.current = next;
    flushSync(() => setMeasuredHeightsByKey(next));
  }, []);

  const observeTurnElement = useCallback((key: string, element: HTMLElement | null) => {
    if (!element) return;
    turnKeyByElementRef.current.set(element, key);
    if (!resizeObserverRef.current) {
      resizeObserverRef.current = new ResizeObserver((entries) => {
        const measurements = new Map<string, number>();
        for (const entry of entries) {
          const observedKey = turnKeyByElementRef.current.get(entry.target);
          if (observedKey) {
            measurements.set(observedKey, resizeObserverBlockSize(entry, entry.target as HTMLElement));
          }
        }
        if (measurements.size) commitMeasuredHeights(measurements);
      });
    }
    const observer = resizeObserverRef.current;
    observer.observe(element);
    return () => {
      observer.unobserve(element);
      turnKeyByElementRef.current.delete(element);
    };
  }, [commitMeasuredHeights]);

  useEffect(() => () => {
    resizeObserverRef.current?.disconnect();
    resizeObserverRef.current = null;
    turnKeyByElementRef.current.clear();
  }, []);

  useEffect(() => {
    const liveKeys = new Set(entries.map((entry) => entry.turnKey));
    const current = measuredHeightsByKeyRef.current;
    const stale = Object.keys(current).filter((key) => !liveKeys.has(key));
    if (!stale.length) return;
    const next = Object.fromEntries(Object.entries(current).filter(([key]) => liveKeys.has(key)));
    measuredHeightsByKeyRef.current = next;
    setMeasuredHeightsByKey(next);
  }, [entries]);

  if (!virtualized) {
    return turns.map((turn, turnIndex) => {
      const key = turnKey(turn, turnIndex);
      return (
        <MeasuredVirtualTurn key={key} observeTurnElement={observeTurnElement} turnKey={key}>
          {renderTurn(turn, turnIndex)}
        </MeasuredVirtualTurn>
      );
    });
  }

  const visible = turns.slice(range.startIndex, range.endIndex);
  return (
    <div
      className="relative shrink-0"
      data-thread-virtualized="true"
      style={{ height: `${layout.totalHeightPx}px` }}
    >
      <div
        className="flex flex-col"
        style={{
          gap: `${THREAD_TURN_GAP_PX}px`,
          marginTop: `${layout.topOffsetsPx[range.startIndex] ?? 0}px`,
        }}
      >
        {visible.map((turn, visibleIndex) => {
          const turnIndex = range.startIndex + visibleIndex;
          const key = entries[turnIndex]?.turnKey ?? turnKey(turn, turnIndex);
          const constrainedHeight = turnIndex === turns.length - 1 || measuredHeightsByKey[key] !== undefined
            ? undefined
            : layout.heightsPx[turnIndex];
          return (
            <MeasuredVirtualTurn
              constrainedHeight={constrainedHeight}
              key={key}
              observeTurnElement={observeTurnElement}
              turnKey={key}
            >
              {renderTurn(turn, turnIndex)}
            </MeasuredVirtualTurn>
          );
        })}
      </div>
    </div>
  );
}

function MeasuredVirtualTurn({
  children,
  constrainedHeight,
  observeTurnElement,
  turnKey: key,
}: {
  children: ReactNode;
  constrainedHeight?: number;
  observeTurnElement: (key: string, element: HTMLElement | null) => (() => void) | undefined;
  turnKey: string;
}) {
  const elementRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = elementRef.current;
    if (!element) return;
    return observeTurnElement(key, element);
  }, [key, observeTurnElement]);
  return (
    <div
      className="relative shrink-0"
      data-thread-turn-key={key}
      style={constrainedHeight === undefined ? undefined : { height: constrainedHeight, overflow: "hidden" }}
    >
      <div ref={elementRef}>{children}</div>
    </div>
  );
}

function turnKey(turn: readonly ChatRenderMessage[], index: number): string {
  return turn[0]?.id || `turn:${index}`;
}

function estimatedTurnHeight(turn: readonly ChatRenderMessage[]): number {
  const cached = threadTurnEstimateCache.get(turn as ChatRenderMessage[]);
  if (cached !== undefined) return cached;
  const textLength = turn.reduce((total, message) => total + message.text.length, 0);
  const structuralRows = turn.reduce((total, message) => total + (
    ["dynamic_tool", "file_change", "diff", "tool", "mcp_tool", "web_search"].includes(message.eventKind ?? "")
      ? 1
      : 0
  ), 0);
  const estimated = Math.min(1_200, Math.max(160, 160 + Math.ceil(textLength / 110) * 20 + structuralRows * 28));
  threadTurnEstimateCache.set(turn as ChatRenderMessage[], estimated);
  return estimated;
}

type UserNavigationItem = {
  id: string;
  label: string;
  preview: string;
};

function UserMessageNavigationRail({ activeId, items, onReveal }: {
  activeId: string;
  items: UserNavigationItem[];
  onReveal: (id: string, instant?: boolean) => void;
}) {
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null);
  const [scrubbing, setScrubbing] = useState(false);
  const railRef = useRef<HTMLDivElement>(null);
  if (items.length < 4) return null;
  const activeIndex = Math.max(0, items.findIndex((item) => item.id === activeId));
  const previewIndex = hoveredIndex ?? activeIndex;
  const preview = items[previewIndex];

  function revealFromPointer(event: React.PointerEvent<HTMLDivElement>, instant: boolean) {
    const target = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>("[data-user-navigation-index]");
    if (!target || !railRef.current?.contains(target)) return;
    const index = Number(target.dataset.userNavigationIndex);
    if (!Number.isInteger(index) || !items[index]) return;
    setHoveredIndex(index);
    onReveal(items[index].id, instant);
  }

  return (
    <nav aria-label="User message navigation" className="thread-user-navigation absolute top-1/2 left-4 z-20 -translate-y-1/2">
      <div
        className="thread-user-navigation-list vertical-scroll-fade-mask hide-scrollbar flex max-h-[min(70vh,40rem)] flex-col overflow-y-auto overscroll-contain"
        data-scrubbing={scrubbing || undefined}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          setScrubbing(true);
          revealFromPointer(event, true);
        }}
        onPointerLeave={() => {
          if (!scrubbing) setHoveredIndex(null);
        }}
        onPointerMove={(event) => {
          const target = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-user-navigation-index]") : null;
          if (target) setHoveredIndex(Number(target.dataset.userNavigationIndex));
          if (scrubbing) revealFromPointer(event, true);
        }}
        onPointerUp={(event) => {
          if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
          setScrubbing(false);
        }}
        ref={railRef}
      >
        {items.map((item, index) => {
          const distance = hoveredIndex === null ? Number.POSITIVE_INFINITY : Math.abs(hoveredIndex - index);
          const progress = distance === 0 ? 1 : distance === 1 ? 0.7 : distance === 2 ? 0.4 : distance === 3 ? 0.2 : 0;
          return (
            <button
              aria-current={item.id === activeId ? "true" : undefined}
              aria-label={`Jump to user message ${index + 1}`}
              className="thread-user-navigation-row group flex h-2.5 w-9 shrink-0 cursor-interaction items-center outline-none"
              data-user-navigation-index={index}
              key={item.id}
              onClick={() => onReveal(item.id, true)}
              onFocus={() => setHoveredIndex(index)}
              onMouseEnter={() => setHoveredIndex(index)}
              type="button"
            >
              <span className="flex h-0.5 w-[30px] items-center">
                <span
                  className="thread-user-navigation-marker h-0.5 bg-token-description-foreground opacity-40 group-focus-visible:bg-token-foreground group-focus-visible:opacity-100"
                  data-active={item.id === activeId || undefined}
                  style={{ "--marker-progress": progress } as React.CSSProperties}
                />
              </span>
            </button>
          );
        })}
      </div>
      {hoveredIndex !== null && preview ? (
        <div className="thread-user-navigation-preview pointer-events-none absolute top-1/2 left-full ml-1 w-80 max-w-[calc(100vw-1rem)] -translate-y-1/2 overflow-hidden rounded-xl bg-token-dropdown-background/95 p-2 text-sm leading-5 text-token-foreground shadow-xl-spread ring-[0.5px] ring-token-border backdrop-blur-sm">
          <div className="truncate font-medium">{preview.label}</div>
          {preview.preview ? <div className="mt-1 line-clamp-3 text-token-description-foreground">{preview.preview}</div> : null}
        </div>
      ) : null}
    </nav>
  );
}

function nearestUserMessage(list: HTMLElement): string {
  const items = [...list.querySelectorAll<HTMLElement>("[data-thread-user-message-id]")];
  if (!items.length) return "";
  const bounds = list.getBoundingClientRect();
  const center = bounds.top + bounds.height / 2;
  let nearest = items[0];
  let distance = Number.POSITIVE_INFINITY;
  for (const item of items) {
    const rect = item.getBoundingClientRect();
    const nextDistance = Math.abs(rect.top + rect.height / 2 - center);
    if (nextDistance >= distance) continue;
    nearest = item;
    distance = nextDistance;
  }
  return nearest.dataset.threadUserMessageId || "";
}

function groupIntoTurns(messages: ChatRenderMessage[], linkToolResults = true): ChatRenderMessage[][] {
  const turns: ChatRenderMessage[][] = [];
  const toolTurns = new Map<string, ChatRenderMessage[]>();
  const interruptedTurns = new Map<string, ChatRenderMessage[]>();
  for (const message of messages) {
    const data = eventRecord(message.eventData);
    const turnId = eventString(data.turn_id ?? eventRecord(data._event).turn_id ?? eventRecord(data.turn).id);
    const currentTurn = turns[turns.length - 1];
    const startsProviderTurn = message.eventKind === "turn_started" && Boolean(turnId)
      && currentTurn?.some((previous) => {
        const previousData = eventRecord(previous.eventData);
        const previousId = eventString(previousData.turn_id ?? eventRecord(previousData._event).turn_id ?? eventRecord(previousData.turn).id);
        return previous.eventKind === "turn_completed" && previousId && previousId !== turnId;
      });
    const startsTurn = message.role === "user" && message.eventKind !== "steering";
    if (startsTurn || startsProviderTurn || turns.length === 0) turns.push([]);
    const scope = [message.runId, message.interfaceId,
      eventString(eventRecord(data._activity_origin).interface_id)];
    const callId = eventString(data.call_id ?? data.callId ?? data.correlation_id);
    const coordinates = [...scope, eventString(data.tool), callId];
    const toolKey = linkToolResults && message.role !== "user" && message.eventKind === "dynamic_tool"
      && coordinates.every((value) => typeof value === "string" && value.length > 0)
      ? JSON.stringify(coordinates) : "";
    const interruptedCoordinates = [...scope, callId, eventString(data.thread_id),
      eventString(data.turn_id ?? eventRecord(data._event).turn_id)];
    const interruptedKey = linkToolResults && message.role !== "user"
      && interruptedCoordinates.every((value) => typeof value === "string" && value.length > 0)
      ? JSON.stringify(interruptedCoordinates) : "";
    const turn = (toolKey && toolTurns.get(toolKey))
      || (message.eventKind === "interrupted_result" && interruptedKey && interruptedTurns.get(interruptedKey))
      || turns[turns.length - 1];
    turn.push(message);
    if (toolKey && data.status === "started") {
      toolTurns.set(toolKey, turn);
      if (interruptedKey) interruptedTurns.set(interruptedKey, turn);
    }
  }
  return turns;
}

function useStableTurns(messages: ChatRenderMessage[], linkToolResults: boolean): ChatRenderMessage[][] {
  const previousRef = useRef<ChatRenderMessage[][]>([]);
  const next = groupIntoTurns(messages, linkToolResults);
  const previous = previousRef.current;
  const previousByKey = new Map(previous.map((turn, index) => [turnKey(turn, index), turn]));
  let changed = previous.length !== next.length;
  for (let index = 0; index < next.length; index += 1) {
    const nextTurn = next[index];
    const previousTurn = nextTurn
      ? previousByKey.get(turnKey(nextTurn, index))
      : undefined;
    if (!previousTurn || !nextTurn || !chatTurnsEqual(previousTurn, nextTurn)) {
      changed = true;
      continue;
    }
    next[index] = previousTurn;
    if (previous[index] !== previousTurn) changed = true;
  }
  const stable = changed ? next : previous;
  previousRef.current = stable;
  return stable;
}

function conversationProjectionRelevant(message: ChatRenderMessage, toolPresentations: ChatToolPresentationIndex): boolean {
  return conversationTraceAdmission(message, toolPresentations) === "visible";
}

function conversationRenderRelevant(message: ChatRenderMessage, toolPresentations: ChatToolPresentationIndex): boolean {
  return conversationTraceAdmission(message, toolPresentations) !== "hidden"
    || artifactEndResourceFromMessage(message) !== null;
}

function conversationSearchVisible(message: ChatRenderMessage, toolPresentations: ChatToolPresentationIndex): boolean {
  if (!conversationProjectionRelevant(message, toolPresentations)) return false;
  return message.eventKind !== "reasoning" && message.eventKind !== "reasoning_delta";
}

function ChatTurn({
  activityPresentation,
  findActiveIndex,
  findState,
  isLatestTurn,
  onContinueInNewTask,
  onEditUserMessage,
  onSteerUserMessage,
  steerableMessageId,
  onOperatorRequestResponse,
  onOpenAssistantReview,
  onOpenAssistantVisualization,
  onOpenFile,
  onPinMessageExcerpt,
  onTogglePinnedMessage,
  pinnedArtifactIds,
  pinningArtifactIds,
  responseStreaming,
  blockingReason,
  resolveAssistantVisualization,
  taskActionsEnabled,
  toolPresentations,
  turn,
}: {
  activityPresentation: "conversation" | "ledger";
  findActiveIndex: number;
  findState: { offsets: Map<string, number>; query: string; total: number };
  isLatestTurn: boolean;
  onContinueInNewTask: Props["onContinueInNewTask"];
  onEditUserMessage: Props["onEditUserMessage"];
  onSteerUserMessage: Props["onSteerUserMessage"];
  steerableMessageId: string;
  onOperatorRequestResponse: Props["onOperatorRequestResponse"];
  onOpenAssistantReview: Props["onOpenAssistantReview"];
  onOpenAssistantVisualization: Props["onOpenAssistantVisualization"];
  onOpenFile: Props["onOpenFile"];
  onPinMessageExcerpt: Props["onPinMessageExcerpt"];
  onTogglePinnedMessage: Props["onTogglePinnedMessage"];
  pinnedArtifactIds: string[];
  pinningArtifactIds: string[];
  responseStreaming: boolean;
  blockingReason: string;
  resolveAssistantVisualization: Props["resolveAssistantVisualization"];
  taskActionsEnabled: boolean;
  toolPresentations: ChatToolPresentationIndex;
  turn: ChatRenderMessage[];
}) {
  const user = turn.find((message) => message.role === "user" && message.eventKind !== "steering");
  const visibleUser = user?.eventData?._activity_turn_anchor === true ? null : user;
  const assistants = turn.filter((message) => message.role !== "user");
  const responseCandidates = [...assistants].reverse();
  const successfulResponse = responseCandidates.find(isFinalAssistantResponse);
  const terminalError = responseCandidates.find((message) => (
    message.eventKind === "error"
    && message.terminal === true
  ));
  const response = successfulResponse ?? terminalError;
  const successfulResponseIndex = successfulResponse ? turn.indexOf(successfulResponse) : -1;
  const postResponseTerminalErrors = successfulResponseIndex < 0 ? [] : turn.filter((message, index) => (
    index > successfulResponseIndex
    && message.role !== "user"
    && message.eventKind === "error"
    && message.terminal === true
  ));
  const postResponseTerminalErrorIds = new Set(postResponseTerminalErrors.map((message) => message.id));
  const finalStartedAtMs = assistants.find((message) => (
    message.eventKind === "message_delta" && isFinalAssistantResponse(message)
  ))?.createdAtMs ?? (successfulResponse && !successfulResponse.error ? successfulResponse.createdAtMs : undefined);
  const endResources = activityPresentation === "conversation"
    ? turn.flatMap((message) => {
      if (message.eventData?._movement_evidence === true) return [];
      const resource = artifactEndResourceFromMessage(message);
      return resource ? [resource] : [];
    })
    : [];
  const endResourceIds = new Set(endResources.map((resource) => resource.reference.artifactId));
  const activities = turn.filter((message) => (
    message !== user
    && message !== response
    && !postResponseTerminalErrorIds.has(message.id)
    && (activityPresentation === "ledger" || !endResourceIds.has(message.artifactId ?? ""))
    && !(message.eventKind === "message_delta" && successfulResponse && successfulResponse.eventKind !== "message_delta")
  ));
  const findOf = (message: ChatRenderMessage): MessageFind | undefined => findState.query ? {
    activeIndex: findActiveIndex,
    offset: findState.offsets.get(message.id) ?? 0,
    query: findState.query,
  } : undefined;
  const assistant = (message: ChatRenderMessage, actions = false) => message.role === "user" ? (
    <UserMessage
      find={findOf(message)}
      message={message}
      onPinMessageExcerpt={onPinMessageExcerpt}
      onTogglePinnedMessage={onTogglePinnedMessage}
      onEditUserMessage={onEditUserMessage}
      onSteerUserMessage={onSteerUserMessage}
      steerable={message.id === steerableMessageId}
      editable={isLatestTurn}
      pinned={Boolean(message.artifactId && pinnedArtifactIds.includes(message.artifactId))}
      pinning={Boolean(message.artifactId && pinningArtifactIds.includes(message.artifactId))}
      taskActionsEnabled={taskActionsEnabled}
    />
  ) : actions ? (
    <AssistantMessage
      find={findOf(message)}
      isLatestTurn={isLatestTurn}
      message={message}
      onContinueInNewTask={onContinueInNewTask}
      onOperatorRequestResponse={onOperatorRequestResponse}
      onOpenAssistantReview={onOpenAssistantReview}
      onOpenAssistantVisualization={onOpenAssistantVisualization}
      onOpenFile={onOpenFile}
      onPinMessageExcerpt={onPinMessageExcerpt}
      onTogglePinnedMessage={onTogglePinnedMessage}
      pinned={Boolean(message.artifactId && pinnedArtifactIds.includes(message.artifactId))}
      pinning={Boolean(message.artifactId && pinningArtifactIds.includes(message.artifactId))}
      streaming={!blockingReason && responseStreaming && isLatestTurn && message === response}
      resolveAssistantVisualization={resolveAssistantVisualization}
      taskActionsEnabled={taskActionsEnabled}
      toolPresentations={toolPresentations}
    />
  ) : (
    <SelectableMessageText message={message} onPinMessageExcerpt={onPinMessageExcerpt} taskActionsEnabled={taskActionsEnabled}>
      <AssistantEventContent find={findOf(message)} message={message} onOpenAssistantReview={onOpenAssistantReview} onOpenAssistantVisualization={onOpenAssistantVisualization} onOpenFile={onOpenFile} onOperatorRequestResponse={onOperatorRequestResponse} resolveAssistantVisualization={resolveAssistantVisualization} streaming={!blockingReason && responseStreaming && isLatestTurn && (message === response || message.eventKind === "message_delta")} toolPresentations={toolPresentations} />
    </SelectableMessageText>
  );

  return (
    <div className="flex flex-col gap-0">
      {visibleUser ? (
        <div className="scroll-mt-4" data-thread-user-message-id={visibleUser.id}>
          <div className="flex flex-col items-end gap-2">
            <UserMessage
              find={findOf(visibleUser)}
              message={visibleUser}
              onPinMessageExcerpt={onPinMessageExcerpt}
              onTogglePinnedMessage={onTogglePinnedMessage}
              onEditUserMessage={onEditUserMessage}
              onSteerUserMessage={onSteerUserMessage}
              steerable={visibleUser.id === steerableMessageId}
              editable={isLatestTurn}
              pinned={Boolean(visibleUser.artifactId && pinnedArtifactIds.includes(visibleUser.artifactId))}
              pinning={Boolean(visibleUser.artifactId && pinningArtifactIds.includes(visibleUser.artifactId))}
              taskActionsEnabled={taskActionsEnabled}
            />
          </div>
        </div>
      ) : null}
      {visibleUser && turn.length > 1 ? <div aria-hidden="true" className="w-full" style={{ height: "var(--conversation-item-gap, 16px)" }} /> : null}
      <TurnActivity
        activities={activities}
        blockingReason={blockingReason}
        turnInProgress={responseStreaming && isLatestTurn}
        finalStartedAtMs={finalStartedAtMs}
        presentation={activityPresentation}
        response={response}
        startedAtMs={user?.createdAtMs ?? assistants[0]?.createdAtMs ?? Date.now()}
        renderActivity={assistant}
        toolPresentations={toolPresentations}
      />
      {response && !response.error && response.eventData?._activity_terminal_anchor !== true
        ? assistant(response, true)
        : null}
      {postResponseTerminalErrors.map((message) => (
        <Fragment key={`post-response-error:${message.id}`}>
          <div aria-hidden="true" className="w-full" style={{ height: "var(--conversation-item-gap, 16px)" }} />
          <TerminalErrorActivity message={message} renderActivity={assistant} />
        </Fragment>
      ))}
      <ArtifactEndResources resources={endResources} />
    </div>
  );
}

function chatTurnMessageEqual(previous: ChatRenderMessage, next: ChatRenderMessage): boolean {
  return previous === next || (
    previous.id === next.id
    && previous.runId === next.runId
    && previous.role === next.role
    && previous.text === next.text
    && previous.createdAtMs === next.createdAtMs
    && previous.label === next.label
    && previous.direction === next.direction
    && previous.artifactId === next.artifactId
    && previous.interfaceId === next.interfaceId
    && previous.eventKind === next.eventKind
    && projectionValueEqual(previous.eventData, next.eventData)
    && previous.latencyMs === next.latencyMs
    && projectionValueEqual(previous.attachments, next.attachments)
    && previous.pending === next.pending
    && previous.error === next.error
    && previous.terminal === next.terminal
  );
}

function projectionValueEqual(previous: unknown, next: unknown): boolean {
  if (Object.is(previous, next)) return true;
  if (Array.isArray(previous) || Array.isArray(next)) {
    return Array.isArray(previous)
      && Array.isArray(next)
      && previous.length === next.length
      && previous.every((value, index) => projectionValueEqual(value, next[index]));
  }
  if (!previous || !next || typeof previous !== "object" || typeof next !== "object") return false;
  const previousRecord = previous as Record<string, unknown>;
  const nextRecord = next as Record<string, unknown>;
  const previousKeys = Object.keys(previousRecord);
  const nextKeys = Object.keys(nextRecord);
  return previousKeys.length === nextKeys.length
    && previousKeys.every((key) => Object.prototype.hasOwnProperty.call(nextRecord, key)
      && projectionValueEqual(previousRecord[key], nextRecord[key]));
}

function stringListsEqual(previous: readonly string[], next: readonly string[]): boolean {
  return previous === next || (
    previous.length === next.length
    && previous.every((value, index) => value === next[index])
  );
}

function chatTurnsEqual(previous: readonly ChatRenderMessage[], next: readonly ChatRenderMessage[]): boolean {
  return previous === next || (
    previous.length === next.length
    && previous.every((message, index) => {
      const nextMessage = next[index];
      return Boolean(nextMessage && chatTurnMessageEqual(message, nextMessage));
    })
  );
}

function chatTurnPropsEqual(
  previous: Parameters<typeof ChatTurn>[0],
  next: Parameters<typeof ChatTurn>[0],
): boolean {
  const findEqual = !previous.findState.query && !next.findState.query
    ? true
    : previous.findActiveIndex === next.findActiveIndex
      && previous.findState === next.findState;
  return previous.activityPresentation === next.activityPresentation
    && findEqual
    && previous.isLatestTurn === next.isLatestTurn
    && previous.onContinueInNewTask === next.onContinueInNewTask
    && previous.onEditUserMessage === next.onEditUserMessage
    && previous.onSteerUserMessage === next.onSteerUserMessage
    && previous.steerableMessageId === next.steerableMessageId
    && previous.onOperatorRequestResponse === next.onOperatorRequestResponse
    && previous.onOpenAssistantReview === next.onOpenAssistantReview
    && previous.onOpenAssistantVisualization === next.onOpenAssistantVisualization
    && previous.onOpenFile === next.onOpenFile
    && previous.onPinMessageExcerpt === next.onPinMessageExcerpt
    && previous.onTogglePinnedMessage === next.onTogglePinnedMessage
    && stringListsEqual(previous.pinnedArtifactIds, next.pinnedArtifactIds)
    && stringListsEqual(previous.pinningArtifactIds, next.pinningArtifactIds)
    && previous.responseStreaming === next.responseStreaming
    && previous.blockingReason === next.blockingReason
    && previous.resolveAssistantVisualization === next.resolveAssistantVisualization
    && previous.taskActionsEnabled === next.taskActionsEnabled
    && previous.toolPresentations === next.toolPresentations
    && chatTurnsEqual(previous.turn, next.turn);
}

const MemoizedChatTurn = memo(ChatTurn, chatTurnPropsEqual);

type MessageFind = { query: string; offset: number; activeIndex: number };

function UserMessage({ find, message, onEditUserMessage, onSteerUserMessage, steerable = false, editable = false, onPinMessageExcerpt, onTogglePinnedMessage, pinned, pinning, taskActionsEnabled }: {
  find?: MessageFind;
  message: ChatRenderMessage;
  onEditUserMessage: Props["onEditUserMessage"];
  onSteerUserMessage: Props["onSteerUserMessage"];
  steerable?: boolean;
  editable?: boolean;
  onTogglePinnedMessage: (message: ChatRenderMessage) => Promise<void>;
  onPinMessageExcerpt: Props["onPinMessageExcerpt"];
  pinned: boolean;
  pinning: boolean;
  taskActionsEnabled: boolean;
}) {
  const [editIntent, setEditIntent] = useState<"steer" | "edit" | null>(null);
  const [editText, setEditText] = useState(message.text);
  const [editPending, setEditPending] = useState(false);
  const [editError, setEditError] = useState("");
  const [editExpired, setEditExpired] = useState(false);
  const editRequestRef = useRef(0);
  useEffect(() => {
    editRequestRef.current += 1;
    setEditIntent(null);
    setEditText(message.text);
    setEditPending(false);
    setEditError("");
    setEditExpired(false);
  }, [message.id]);
  useEffect(() => {
    if (!editIntent) setEditText(message.text);
    if (editIntent === "steer" && !steerable) setEditExpired(true);
  }, [editIntent, message.text, steerable]);
  const steering = message.eventKind === "steering";
  const steeringStatus = typeof message.eventData?.steering_status === "string"
    ? message.eventData.steering_status
    : message.pending ? "pending" : "submitted";
  const steeringLabel = steeringStatus === "input_recorded"
    ? "Message included in the current turn"
    : steeringStatus === "acknowledged" ? "Message accepted; waiting to enter the current turn"
    : steeringStatus === "failed" ? "Message delivery could not be confirmed" : "Message delivery pending";
  if (editIntent && !steering) {
    const unavailable = editIntent === "steer" && (editExpired || !steerable);
    const submitEdit = async () => {
      const value = editText.trim();
      if (!value || editPending || unavailable) return;
      const request = ++editRequestRef.current;
      setEditPending(true);
      setEditError("");
      try {
        const accepted = await (editIntent === "steer"
          ? onSteerUserMessage(message, value)
          : onEditUserMessage(message, value));
        if (request !== editRequestRef.current) return;
        if (accepted) setEditIntent(null);
        else setEditError("This edit was not confirmed. Your text has been kept.");
      } catch {
        if (request === editRequestRef.current) setEditError("This edit was not confirmed. Your text has been kept.");
      } finally {
        if (request === editRequestRef.current) setEditPending(false);
      }
    };
    return (
      <div className="group flex w-full flex-col items-end justify-end gap-1">
        <div className="chat-user-message-editor">
          <textarea
            aria-label="Edit operator message"
            autoFocus
            readOnly={editPending}
            onChange={(event) => setEditText(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                if (editPending) return;
                setEditIntent(null);
                setEditText(message.text);
                return;
              }
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && !event.nativeEvent.isComposing) {
                event.preventDefault();
                void submitEdit();
              }
            }}
            value={editText}
          />
          {unavailable || editError ? <div className="chat-user-message-editor-note" role="alert">
            {unavailable ? "This reply is no longer accepting edits. Your text has been kept." : editError}
          </div> : null}
          <div className="chat-user-message-editor-actions">
            <OcButton disabled={editPending} onClick={() => {
              setEditIntent(null);
              setEditText(message.text);
            }} size="xs" variant="ghost">Cancel</OcButton>
            <OcButton disabled={!editText.trim() || editPending || unavailable} onClick={() => void submitEdit()} size="xs" variant="primary">
              {editPending ? "Sending…" : editIntent === "steer" ? "Send to current reply" : "Save and send"}
            </OcButton>
          </div>
        </div>
      </div>
    );
  }
  return (
    <div
      className={`group flex w-full flex-col items-end justify-end gap-2 ${message.error ? "octopus-chat-error" : ""}${steering ? " chat-steering-message" : ""}`}
      data-steering-status={steering ? steeringStatus : undefined}
    >
      {steering ? (
        <div className={steeringStatus === "failed" ? "flex max-w-[77%] items-center gap-1.5 px-1 text-xs text-danger" : "sr-only"} role="status">
          {steeringStatus === "failed" ? <Glyph className="icon-2xs" name="warning" size={12} /> : null}
          <span>{steeringLabel}</span>
        </div>
      ) : null}
      <MessageAttachments message={message} />
      {message.text || !message.attachments?.length ? (
        <div
          className="bg-token-foreground/5 max-w-[77%] min-w-0 overflow-hidden break-words rounded-2xl px-3 py-2 [&_.contain-inline-size]:[contain:initial] text-left focus-visible:ring-2 focus-visible:ring-token-focus-border focus-visible:outline-none"
          data-user-message-bubble="true"
          tabIndex={0}
        >
          <div className="flex flex-col items-end gap-1">
            <div className="chat-user-message-content text-size-chat relative w-full min-w-0">
              <SelectableMessageText message={message} onPinMessageExcerpt={onPinMessageExcerpt} taskActionsEnabled={taskActionsEnabled}>
                <CollapsibleMessageBody collapseIndicator="fade" text={message.text}>
                  <div className="text-size-chat whitespace-pre-wrap">
                    {message.text ? <MessageContent find={find} text={message.text} variant="user" /> : null}
                  </div>
                </CollapsibleMessageBody>
              </SelectableMessageText>
            </div>
          </div>
        </div>
      ) : null}
      <MessageActions
        message={message}
        editLabel={steerable ? "Edit current reply" : "Edit message"}
        onEditUserMessage={editable || steerable ? () => {
          setEditIntent(steerable ? "steer" : "edit");
          setEditText(message.text);
          setEditError("");
          setEditExpired(false);
        } : undefined}
        onTogglePinnedMessage={onTogglePinnedMessage}
        pinned={pinned}
        pinning={pinning}
        taskActionsEnabled={taskActionsEnabled}
        user
      />
    </div>
  );
}

function AssistantMessage({ find, isLatestTurn, message, onContinueInNewTask, onOpenAssistantReview, onOpenAssistantVisualization, onOpenFile, onOperatorRequestResponse, onPinMessageExcerpt, onTogglePinnedMessage, pinned, pinning, resolveAssistantVisualization, streaming, taskActionsEnabled, toolPresentations }: {
  find?: MessageFind;
  isLatestTurn: boolean;
  message: ChatRenderMessage;
  onContinueInNewTask: Props["onContinueInNewTask"];
  onOperatorRequestResponse: (message: ChatRenderMessage, response: ChatOperatorResponse) => Promise<void>;
  onOpenAssistantReview: Props["onOpenAssistantReview"];
  onOpenAssistantVisualization: Props["onOpenAssistantVisualization"];
  onOpenFile: Props["onOpenFile"];
  onTogglePinnedMessage: (message: ChatRenderMessage) => Promise<void>;
  onPinMessageExcerpt: Props["onPinMessageExcerpt"];
  pinned: boolean;
  pinning: boolean;
  resolveAssistantVisualization: Props["resolveAssistantVisualization"];
  streaming: boolean;
  taskActionsEnabled: boolean;
  toolPresentations: ChatToolPresentationIndex;
}) {
  return (
    <div className={`group flex min-w-0 flex-col ${message.error ? "octopus-chat-error" : ""}`}>
      <SelectableMessageText message={message} onPinMessageExcerpt={onPinMessageExcerpt} taskActionsEnabled={taskActionsEnabled}>
        <AssistantEventContent find={find} message={message} onOpenAssistantReview={onOpenAssistantReview} onOpenAssistantVisualization={onOpenAssistantVisualization} onOpenFile={onOpenFile} onOperatorRequestResponse={onOperatorRequestResponse} resolveAssistantVisualization={resolveAssistantVisualization} streaming={streaming} toolPresentations={toolPresentations} />
      </SelectableMessageText>
      <MessageAttachments message={message} />
      {!streaming && message.terminal !== false ? (
        <MessageActions
          message={message}
          isLatestTurn={isLatestTurn}
          onContinueInNewTask={onContinueInNewTask}
          onTogglePinnedMessage={onTogglePinnedMessage}
          pinned={pinned}
          pinning={pinning}
          taskActionsEnabled={taskActionsEnabled}
        />
      ) : null}
    </div>
  );
}

function SelectableMessageText({ children, message, onPinMessageExcerpt, taskActionsEnabled = true }: {
  children: ReactNode;
  message: ChatRenderMessage;
  onPinMessageExcerpt: Props["onPinMessageExcerpt"];
  taskActionsEnabled?: boolean;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [menu, setMenu] = useState<{ text: string; start: number; end: number; x: number; y: number } | null>(null);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", escape);
    };
  }, [menu]);

  const captureSelection = () => {
    const root = rootRef.current;
    const selection = window.getSelection();
    if (!root || !selection || selection.rangeCount === 0 || selection.isCollapsed) return null;
    const range = selection.getRangeAt(0);
    if (!root.contains(range.commonAncestorContainer)) return null;
    const text = selection.toString();
    if (!text.trim()) return null;
    const prefix = range.cloneRange();
    prefix.selectNodeContents(root);
    prefix.setEnd(range.startContainer, range.startOffset);
    const start = prefix.toString().length;
    const rect = range.getBoundingClientRect();
    return {
      text,
      start,
      end: start + text.length,
      x: rect.left + rect.width / 2,
      y: rect.top,
    };
  };

  useEffect(() => {
    if (!menu) return;
    const collapseIfEmpty = () => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) setMenu(null);
    };
    document.addEventListener("selectionchange", collapseIfEmpty);
    window.addEventListener("scroll", () => setMenu(null), { capture: true, once: true });
    return () => document.removeEventListener("selectionchange", collapseIfEmpty);
  }, [menu]);

  const items: Array<{ label: string; glyph: "copy" | "pinOff"; onClick: () => void }> = [
    {
      label: "Copy",
      glyph: "copy",
      onClick: () => {
        void copyText(menu?.text ?? "");
        window.getSelection()?.removeAllRanges();
        setMenu(null);
      },
    },
  ];
  if (taskActionsEnabled && message.artifactId && menu) {
    const selection = menu;
    items.push({
      label: "Pin selection",
      glyph: "pinOff",
      onClick: () => {
        window.getSelection()?.removeAllRanges();
        setMenu(null);
        void onPinMessageExcerpt(message, selection);
      },
    });
  }

  return (
    <div
      data-message-id={message.id}
      data-message-text="true"
      onContextMenu={(event) => {
        const selection = captureSelection();
        if (!selection) return;
        event.preventDefault();
        setMenu({ ...selection, x: event.clientX, y: event.clientY });
      }}
      ref={rootRef}
    >
      {children}
      {menu ? (
        <div
          className="chat-selection-menu"
          onMouseDown={(event) => event.preventDefault()}
          onPointerDown={(event) => event.stopPropagation()}
          role="menu"
          style={{
            left: Math.max(8, Math.min(menu.x, window.innerWidth - 184)),
            top: Math.max(8, Math.min(menu.y, window.innerHeight - 44)),
          }}
        >
          {items.map((item, index) => (
            <Fragment key={item.label}>
              <button className="chat-selection-menu-item" onClick={item.onClick} role="menuitem" type="button">
                <Glyph className="icon-2xs" name={item.glyph} size={14} />
                {item.label}
              </button>
              {index < items.length - 1 ? <span aria-hidden="true" className="chat-selection-menu-separator" /> : null}
            </Fragment>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function textRange(root: HTMLElement, start: number, end: number): Range | null {
  if (end <= start) return null;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let offset = 0;
  let startNode: Text | null = null;
  let endNode: Text | null = null;
  let startOffset = 0;
  let endOffset = 0;
  while (walker.nextNode()) {
    const node = walker.currentNode as Text;
    const next = offset + node.data.length;
    if (!startNode && start >= offset && start <= next) {
      startNode = node;
      startOffset = Math.min(node.data.length, start - offset);
    }
    if (end >= offset && end <= next) {
      endNode = node;
      endOffset = Math.min(node.data.length, end - offset);
      break;
    }
    offset = next;
  }
  if (!startNode || !endNode) return null;
  const range = document.createRange();
  range.setStart(startNode, startOffset);
  range.setEnd(endNode, endOffset);
  return range;
}

function AssistantEventContent({ find, message, onOpenAssistantReview, onOpenAssistantVisualization, onOpenFile, onOperatorRequestResponse, resolveAssistantVisualization, streaming = false, toolPresentations }: {
  find?: MessageFind;
  message: ChatRenderMessage;
  onOpenAssistantReview: Props["onOpenAssistantReview"];
  onOpenAssistantVisualization: Props["onOpenAssistantVisualization"];
  onOpenFile: Props["onOpenFile"];
  onOperatorRequestResponse: (message: ChatRenderMessage, response: ChatOperatorResponse) => Promise<void>;
  resolveAssistantVisualization: Props["resolveAssistantVisualization"];
  streaming?: boolean;
  toolPresentations: ChatToolPresentationIndex;
}) {
  const kind = message.eventKind ?? "message";
  const data = message.eventData ?? {};
  if (data._movement_evidence === true) {
    return <MovementEvidence message={message} />;
  }
  if (kind === "message") {
    return <AssistantResponseContent find={find} message={message} onOpenAssistantReview={onOpenAssistantReview} onOpenAssistantVisualization={onOpenAssistantVisualization} onOpenFile={onOpenFile} resolveAssistantVisualization={resolveAssistantVisualization} streaming={streaming} />;
  }
  if (kind === "error") {
    return message.text ? <MessageContent find={find} onOpenFile={onOpenFile} text={message.text} variant="assistant" /> : null;
  }
  if (kind === "message_delta") {
    return <AssistantResponseContent find={find} message={message} onOpenAssistantReview={onOpenAssistantReview} onOpenAssistantVisualization={onOpenAssistantVisualization} onOpenFile={onOpenFile} resolveAssistantVisualization={resolveAssistantVisualization} streaming={streaming} streamingChrome />;
  }
  if (kind === "commentary" || kind === "commentary_delta") {
    const commentaryStreaming = kind === "commentary_delta";
    return message.text.trim() ? (
      <div className={commentaryStreaming ? "octopus-assistant-commentary octopus-assistant-streaming min-w-0" : "octopus-assistant-commentary min-w-0"}>
        <MessageContent
          find={find}
          streamKey={commentaryStreaming ? message.id : undefined}
          onOpenFile={onOpenFile}
          streaming={commentaryStreaming}
          text={message.text}
          variant="assistant"
        />
      </div>
    ) : null;
  }
  if (kind === "reasoning" || kind === "reasoning_delta") {
    const reasoningStreaming = kind === "reasoning_delta";
    const reasoning = reasoningBody(message.text, reasoningStreaming);
    return reasoning ? (
      <div className={reasoningStreaming ? "octopus-assistant-streaming min-w-0" : "min-w-0"}>
        <MessageContent
          find={find}
          streamKey={reasoningStreaming ? message.id : undefined}
          onOpenFile={onOpenFile}
          streaming={reasoningStreaming}
          text={reasoning}
          variant="reasoning"
        />
      </div>
    ) : null;
  }
  if (kind === "plan" || kind === "plan_delta") {
    return <ChatEventCard eyebrow={kind === "plan_delta" ? "Updating plan" : "Plan"} text={message.text} />;
  }
  if (kind === "tool") {
    return <ExecActivity message={message} onOpenFile={onOpenFile} />;
  }
  if (kind === "web_search") {
    const query = webSearchActivityDetail(message);
    const active = activityLifecycle(message, eventString(data.status) || eventString(data.lifecycle)) === "active";
    return <WebSearchActivity active={active} detail={query} />;
  }
  if (kind === "tool_output_delta" || kind === "terminal_interaction") {
    const label = kind === "terminal_interaction" ? "Terminal input" : "Command output";
    return <ActivityCodeBlock label={label} text={message.text} />;
  }
  if (kind === "file_change" || kind === "file_change_delta" || kind === "diff") {
    return <FileActivity message={message} onOpenFile={onOpenFile} />;
  }
  if (kind === "mcp_tool" || kind === "mcp_progress" || kind === "mcp_status") {
    return <McpResourceSurface message={message} />;
  }
  if (kind === "dynamic_tool") {
    const presentation = dynamicToolPresentation(message, toolPresentations);
    if (presentation.family === "web") {
      const active = activityLifecycle(message, eventString(data.status) || eventString(data.lifecycle)) === "active";
      return <WebSearchActivity active={active} detail={presentation.query} />;
    }
    if (presentation.family === "exec") {
      return (
        <div className="flex w-full min-w-0 flex-col items-stretch">
          <DynamicToolExecActivity message={message} onOpenFile={(path) => onOpenFile(path, presentation.exactToolId)} />
        </div>
      );
    }
    if (presentation.family === "read") {
      const lifecycle = activityLifecycle(message, eventString(data.status));
      const active = lifecycle === "active";
      const label = dynamicToolReadLabel(presentation, lifecycle === "active");
      const action = presentation.kind === "search"
        ? active ? "Searching" : "Searched"
        : presentation.kind === "list"
          ? active ? "Listing" : "Listed"
          : active ? "Reading" : "Read";
      const detail = label.slice(action.length).trim();
      const copy = presentation.kind === "read" && presentation.target && presentation.pathLabel ? (
        <CadencedShimmerText active={active} highlight={label}>
          <span className="octopus-activity-action">{action}</span>{presentation.targetKind === "artifact" ? " artifact " : " "}
          <TraceReadTarget message={message} presentation={presentation} onOpenFile={onOpenFile} />
        </CadencedShimmerText>
      ) : (
        <CadencedShimmerText active={active} highlight={label}>
          <span className="octopus-activity-action">{action}</span>
          {detail ? <>{" "}<span className="octopus-activity-detail">{detail}</span></> : null}
        </CadencedShimmerText>
      );
      return (
        <div className="octopus-inline-activity min-w-0">
          <Glyph
            aria-hidden="true"
            className="icon-xs shrink-0"
            name={presentation.kind === "search" ? "search" : presentation.kind === "list" ? "folder" : "read"}
          />
          {copy}
        </div>
      );
    }
    if (presentation.family === "files") {
      return (
        <div className="flex w-full min-w-0 flex-col items-stretch">
          <FileActivity message={dynamicToolFileMessage(message, presentation)} onOpenFile={(path) => onOpenFile(path, presentation.exactToolId)} />
          {activityLifecycle(message, eventString(data.status)) === "error"
            ? <ActivityCodeBlock label="Error" text={dynamicToolResult(data)} />
            : null}
        </div>
      );
    }
    const lifecycle = activityLifecycle(message, eventString(data.status));
    const label = dynamicToolSummary(presentation);
    return (
      <StructuredActivity
        glyph={dynamicToolGlyph(presentation)}
        label={label || "Tool"}
        status={lifecycle === "active" ? "running" : lifecycle}
        danger={lifecycle === "error"}
        relatedRuns={message.relatedRuns}
        details={presentation.operation ? [
          ...presentation.operation.details,
          ["Details", eventRecord(data._payload_projection).truncated === true
            && eventRecord(data._payload_projection).resolved !== true ? "Partial artifact preview" : ""],
        ] : [
          ["Operation", [eventString(data.namespace), presentation.exactToolId].filter(Boolean).join(".")],
          ["Details", eventRecord(data._payload_projection).truncated === true
            && eventRecord(data._payload_projection).resolved !== true ? "Partial artifact preview" : ""],
          ["Arguments", eventJson(data.arguments)],
          [lifecycle === "error" ? "Error" : "Result", dynamicToolResult(data)],
        ]}
      />
    );
  }
  if (kind === "collaboration" || kind === "subagent_activity") {
    return <CollaborationActivity message={message} />;
  }
  if (kind === "image_view" || kind === "image_generation") {
    const path = eventString(data.path);
    const revisedPrompt = eventString(data.revisedPrompt) || eventString(data.revised_prompt);
    return <StructuredActivity glyph="image" label={kind === "image_generation" ? "Image generation" : "Viewed image"} status={eventString(data.status)} details={[["Path", path], ["Prompt", revisedPrompt], ["Result", eventJson(data.result)]]} />;
  }
  if (kind === "sleep") {
    const duration = eventNumber(data.durationMs ?? data.duration_ms);
    return <StructuredActivity glyph="scheduled" label="Waiting" status={duration ? formatLatency(duration) : ""} details={[]} />;
  }
  if (kind === "review_entered" || kind === "review_exited" || kind === "auto_review_started" || kind === "auto_review_completed") {
    const label = kind === "review_entered"
      ? "Entered review mode"
      : kind === "review_exited"
        ? "Exited review mode"
        : kind === "auto_review_started"
          ? "Reviewing changes"
          : "Reviewed changes";
    return <StructuredActivity glyph="changes" label={label} status={eventString(data.status)} details={[["Review", message.text || eventString(data.review)], ["Decision", eventJson(data.decision)]]} />;
  }
  if (kind === "compaction") {
    const compactionMode = [eventString(data.mode), eventString(data.source), eventString(data.trigger)].join(" ").toLocaleLowerCase();
    const automatic = data.automatic === true || !compactionMode.includes("manual");
    const active = data.completed === false || activityLifecycle(message, eventString(data.status)) === "active";
    const label = active
      ? automatic ? "Context automatically compacting" : "Compacting context"
      : automatic ? "Context automatically compacted" : "Context compacted";
    return (
      <div className="octopus-inline-activity min-w-0">
        <Glyph aria-hidden="true" className="icon-xs shrink-0" name="compact" />
        <CadencedShimmerText active={active}>{label}</CadencedShimmerText>
      </div>
    );
  }
  if (kind === "model_rerouted" || kind === "model_verification") {
    const from = eventString(data.fromModel) || eventString(data.from_model);
    const to = eventString(data.toModel) || eventString(data.to_model);
    return <ConversationNotice glyph="pulse" title={kind === "model_rerouted" ? "Model rerouted" : "Model route verified"} detail={[from, to].filter(Boolean).join(" → ") || message.text} />;
  }
  if (kind === "auth_recovery_started" || kind === "auth_recovery_completed") {
    const completed = kind === "auth_recovery_completed";
    return <ConversationNotice glyph={completed ? "check" : "pulse"} title={message.text || (completed ? "Authentication recovered" : "Recovering authentication")} detail={eventString(data.provider)} />;
  }
  if (kind === "warning") {
    if (eventString(data.source) === "model_safety_buffering") {
      const visible = data.showBufferingUi === true;
      return (
        <ConversationNotice
          glyph="pulse"
          title={eventString(data.message) || message.text || (visible ? "The model is taking extra time to review this request before responding." : "The model's additional review period has ended.")}
          detail={visible ? "No action is required. Octopus will keep waiting for the response." : ""}
        />
      );
    }
    return <ConversationNotice danger glyph="warning" title={eventString(data.message) || message.text || "Warning"} detail={eventString(data.detail)} />;
  }
  if (kind === "rate_limits") {
    return <StructuredActivity glyph="pulse" label="Rate limits updated" status="" details={rateLimitDetails(data)} />;
  }
  if (kind === "turn_started" || kind === "turn_completed" || kind === "item_started") {
    return <LifecycleEvidence kind={kind} message={message} />;
  }
  if (kind === "operator_request_resolved") {
    return <ConversationNotice glyph="check" title="Operator request resolved" detail={message.text} />;
  }
  if (kind === "usage") {
    return <StructuredActivity glyph="pulse" label="Usage recorded" status="complete" details={usageDetails(data)} />;
  }
  if (kind === "hook" || kind === "hook_prompt") {
    const outputs = Array.isArray(data.outputs)
      ? data.outputs.map((entry) => eventString(eventRecord(entry).text)).filter(Boolean).join("\n")
      : "";
    return <ChatEventCard eyebrow={`${eventString(data.event) || (kind === "hook_prompt" ? "Hook prompt" : "Hook")} · ${eventString(data.status) || "updated"}`} text={outputs || eventString(data.status_message) || message.text} />;
  }
  if (kind === "goal") {
    return <ChatEventCard eyebrow={`Goal · ${eventString(data.status) || "updated"}`} text={message.text} />;
  }
  if (kind === "approval" || kind === "elicitation" || kind === "user_input") {
    return <OperatorRequestSurface message={message} onRespond={onOperatorRequestResponse} />;
  }
  return <ChatEventCard eyebrow={message.label || "Event"} text={message.text} />;
}

function AssistantResponseContent({ find, message, onOpenAssistantReview, onOpenAssistantVisualization, onOpenFile, resolveAssistantVisualization, streaming, streamingChrome = false }: {
  find?: MessageFind;
  message: ChatRenderMessage;
  onOpenAssistantReview: Props["onOpenAssistantReview"];
  onOpenAssistantVisualization: Props["onOpenAssistantVisualization"];
  onOpenFile: Props["onOpenFile"];
  resolveAssistantVisualization: Props["resolveAssistantVisualization"];
  streaming: boolean;
  streamingChrome?: boolean;
}) {
  const projection = projectAssistantDirectives(message.text, !streaming && message.terminal !== false && !message.error);
  if (!projection.markdown && !projection.codeComments.length && !projection.inlineVisualizations.length) return null;
  const content = (
    <>
      {projection.markdown ? <MessageContent find={find} onOpenFile={onOpenFile} streamKey={streaming ? message.id : undefined} streaming={streaming} text={projection.markdown} variant="assistant" /> : null}
      <AssistantDirectiveSurfaces
        codeComments={projection.codeComments}
        inlineVisualizations={projection.inlineVisualizations}
        onOpenReview={onOpenAssistantReview}
        onOpenVisualization={onOpenAssistantVisualization}
        resolveVisualization={resolveAssistantVisualization}
      />
    </>
  );
  return <div className={streamingChrome && streaming ? "octopus-assistant-streaming min-w-0" : "min-w-0"}>{content}</div>;
}

function TraceReadTarget({ message, presentation, onOpenFile }: {
  message: ChatRenderMessage;
  presentation: DynamicToolPresentation;
  onOpenFile: Props["onOpenFile"];
}) {
  const openArtifactFiles = useOpenArtifactFiles();
  const [pending, setPending] = useState(false);
  const attempt = useRef(0);
  useEffect(() => {
    setPending(false);
    return () => { attempt.current += 1; };
  }, [message.eventData?.call_id, message.runId, presentation.target]);

  async function openArtifact() {
    if (pending) return;
    const request = ++attempt.current;
    setPending(true);
    try {
      const callId = eventString(message.eventData?.call_id);
      if (!message.runId || !callId) throw new Error("This read has no admitted tool call to open.");
      const response = await readProjection<ArtifactActivityBytes>("projection.artifact.bytes.read", {
        call_id: callId,
        run_id: message.runId,
      });
      if (request !== attempt.current) return;
      openArtifactFiles(traceReadArtifactReference(message, presentation, response));
    } catch (error) {
      if (request === attempt.current) appToast.warning(error instanceof Error ? error.message : String(error));
    } finally {
      if (request === attempt.current) setPending(false);
    }
  }

  return <>
    <MessageTooltip label={<code>{presentation.target}</code>} onActivate={presentation.targetKind === "artifact"
      ? () => void openArtifact()
      : presentation.path ? () => onOpenFile(presentation.path, presentation.exactToolId) : undefined} variant="activity-target">
      <span aria-busy={pending || undefined} className="octopus-activity-detail octopus-activity-file-target">{presentation.pathLabel}</span>
    </MessageTooltip>
  </>;
}

function traceReadArtifactReference(message: ChatRenderMessage, presentation: DynamicToolPresentation, response: ArtifactActivityBytes): ArtifactReference {
  const artifactId = eventString(response.artifact_id);
  const interfaceId = eventString(response.interface_id);
  if (response.run_id !== message.runId || !artifactId || !interfaceId) {
    throw new Error("The read artifact coordinates do not match the selected trace.");
  }
  const data = eventRecord(decodeJsonArtifactPayload(response.base64));
  const body = eventRecord(data.body);
  const callId = eventString(message.eventData?.call_id);
  if (!callId || data.correlation_id !== callId || data.tool !== presentation.exactToolId || data.ok !== true) {
    throw new Error("The admitted read result does not match this artifact target.");
  }
  if (baseDynamicToolId(presentation.exactToolId) === "context.read") {
    const args = eventRecord(message.eventData?.arguments);
    const pin = eventRecord(body.pin);
    const source = eventRecord(pin.source);
    const requested = eventRecord(args.source);
    const entry = eventRecord(body.entry);
    if (!eventString(requested.id) || source.id !== requested.id
      || !Array.isArray(source.content) || JSON.stringify(source.content) !== JSON.stringify(requested.content)
      || pin.entry_id !== presentation.target
      || entry.id !== presentation.target || !/^[a-f0-9]{64}$/.test(eventString(pin.revision))
      || pin.revision !== args.revision) {
      throw new Error("The admitted context entry does not match the selected source and revision.");
    }
    return {
      artifactId, runId: message.runId, interfaceId,
      label: presentation.pathLabel, source: "message", inlinePayload: JSON.stringify(body.entry, null, 2),
    };
  }
  if (baseDynamicToolId(presentation.exactToolId) !== "artifact.read"
    || body.result !== "artifact_read" || body.id !== presentation.target || typeof body.text !== "string") {
    throw new Error("The admitted read result does not match this artifact target.");
  }
  const offset = body.offset_bytes;
  const count = body.bytes_read;
  const total = body.artifact_size_bytes;
  if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(count) || !Number.isSafeInteger(total)
    || (offset as number) < 0 || (count as number) < 0 || (total as number) < 0
    || (offset as number) + (count as number) > (total as number)
    || new TextEncoder().encode(body.text).length !== count || typeof body.truncated !== "boolean") {
    throw new Error("The admitted artifact read has invalid byte coordinates.");
  }
  return {
    artifactId, runId: message.runId, interfaceId,
    label: `${presentation.pathLabel}${offset !== 0 || body.truncated ? " (read excerpt)" : ""}`,
    source: "message", inlinePayload: body.text,
  };
}

function WebSearchActivity({ active, detail }: { active: boolean; detail: string }) {
  const label = webSearchActivityLabel(detail, active);
  return (
    <div className="octopus-inline-activity min-w-0">
      <Glyph className="icon-xs shrink-0" name="search" />
      <CadencedShimmerText active={active} className="min-w-0 truncate" highlight={label}>
        <span className="octopus-activity-action">{active ? "Searching the web" : "Searched the web"}</span>
        {detail ? <>{" "}<span className="octopus-activity-detail">for {detail}</span></> : null}
      </CadencedShimmerText>
    </div>
  );
}

function ExecActivity({ message, onOpenFile }: { message: ChatRenderMessage; onOpenFile: Props["onOpenFile"] }) {
  const data = message.eventData ?? {};
  const model = commandActivityModel(message);
  const parsed = eventRecord(data.parsed_cmd ?? data.parsedCmd);
  const parsedType = eventString(parsed.type).toLocaleLowerCase();
  const command = suppliedShellCommand(data);
  const target = eventString(parsed.path) || eventString(parsed.name) || eventString(parsed.file) || eventString(parsed.target);
  const query = eventString(parsed.query) || eventString(parsed.pattern);
  const active = model.phase === "running";

  if (parsedType === "read" || parsedType === "search" || parsedType === "list_files") {
    const displayTarget = parsedType === "read" ? activityPathLabel(target) : target;
    const action = parsedType === "read"
      ? active ? "Reading" : "Read"
      : parsedType === "search"
        ? active ? "Searching" : "Searched"
        : active ? "Listing" : "Listed";
    const label = parsedType === "read"
      ? [action, displayTarget || command].filter(Boolean).join(" ")
      : parsedType === "search"
        ? query
          ? `${action} for ${query}${target ? ` in ${target}` : ""}`
          : active && target
            ? `${action} files in ${activityPathLabel(target)} folder`
            : `${action} for files`
        : `${action} files${target ? ` in ${target}` : ""}`;
    const detail = label.slice(action.length).trim();
    const copy = parsedType === "read" && target ? (
      <CadencedShimmerText active={active} highlight={label}>
        <span className="octopus-activity-action">{action}</span>{" "}
        <MessageTooltip label={<code>{target}</code>} onActivate={() => onOpenFile(target)} variant="activity-target">
          <span className="octopus-activity-detail octopus-activity-file-target">{displayTarget || command}</span>
        </MessageTooltip>
      </CadencedShimmerText>
    ) : (
      <CadencedShimmerText active={active} highlight={label}>
        <span className="octopus-activity-action">{action}</span>
        {detail ? <>{" "}<span className="octopus-activity-detail">{detail}</span></> : null}
      </CadencedShimmerText>
    );
    return (
      <div className="octopus-inline-activity min-w-0">
        <Glyph
          aria-hidden="true"
          className="icon-xs shrink-0"
          name={parsedType === "search" ? "search" : parsedType === "list_files" ? "folder" : "read"}
        />
        {copy}
      </div>
    );
  }

  const script = eventString(eventRecord(data.arguments).script).trim();
  return <CommandActivity command={command} isCode={Boolean(script && command === script)} message={message} />;
}

function activityPathLabel(value: string): string {
  const normalized = value.replaceAll("\\", "/").replace(/\/+$/, "");
  return normalized.split("/").filter(Boolean).at(-1) ?? value;
}

function CommandActivity({ command, isCode = false, message }: {
  command: string;
  isCode?: boolean;
  message: ChatRenderMessage;
}) {
  const [expanded, setExpanded] = useState(false);
  const [commandExpanded, setCommandExpanded] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [bodyHeight, setBodyHeight] = useState(0);
  const bodyRef = useRef<HTMLDivElement>(null);
  const summaryId = useId();
  const reducedMotion = useShouldReduceMotion();
  const data = message.eventData ?? {};
  const model = commandActivityModel(message);
  const observedRunning = model.yielded && Boolean(model.processId);
  const lifecycle = model.phase === "running" ? observedRunning ? "background" : "active" : model.phase === "interrupted" ? "stopped"
    : model.phase === "failed" || model.phase === "timed_out" || (model.exitCode !== null && model.exitCode !== 0) ? "error"
      : model.phase === "exited" ? "complete" : "unknown";
  const terminalInputs = eventArray(data._terminal_inputs).map(eventString).filter(Boolean);
  const declaredStartedAtMs = data.started_at_ms ?? data.startedAtMs;
  const startedAtMs = typeof declaredStartedAtMs === "number" && Number.isFinite(declaredStartedAtMs)
    ? declaredStartedAtMs
    : message.createdAtMs;
  const completedDurationMs = model.durationMs ?? 0;
  useEffect(() => {
    if (lifecycle !== "active") return;
    const timer = window.setInterval(() => setNowMs(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [lifecycle]);
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    const updateHeight = () => setBodyHeight(body.getBoundingClientRect().height);
    updateHeight();
    const observer = new ResizeObserver(updateHeight);
    observer.observe(body);
    return () => observer.disconnect();
  }, []);
  const elapsedMs = lifecycle === "active"
    ? Math.max(0, nowMs - startedAtMs)
    : completedDurationMs;
  const elapsed = elapsedMs >= 1_000 ? formatActivityElapsed(elapsedMs) : "";
  const summaryCommand = expanded ? "" : command;
  const operation = isCode ? "code" : "command";
  const summary = observedRunning ? `${isCode ? "Code" : "Command"} running at last check` : model.phase === "running"
    ? `${model.yielded ? `${isCode ? "Code" : "Command"} still running` : `Running ${operation}`}${elapsed ? ` for ${elapsed}` : ""}`
    : `${model.phase === "interrupted" ? "Stopped" : model.phase === "timed_out" ? "Timed out" : model.phase === "failed" ? "Failed" : model.phase === "unknown" ? "Outcome unknown for" : "Ran"}${summaryCommand ? ` ${summaryCommand}` : ` ${operation}`}${elapsed ? ` for ${elapsed}` : ""}`;
  const success = model.phase === "exited" && model.exitCode === 0;
  const shellCommand = command;
  const outputSections = model.sections.map((section) => ({ ...section, text: normalizeTerminalControl(section.text) }));
  return (
    <div className="octopus-exec-activity group/exec min-w-0" data-state={lifecycle} data-command-state={model.phase} data-tool-status={model.invocationStatus}>
      <div className="octopus-exec-activity-header group/activity-header">
        <button
          aria-expanded={expanded}
          aria-labelledby={summaryId}
          className="octopus-exec-activity-toggle cursor-interaction"
          onClick={() => setExpanded((current) => !current)}
          type="button"
        />
        <span className="octopus-exec-activity-visual">
          <Glyph aria-hidden="true" className="icon-xs shrink-0" name="terminal" />
          <span className="octopus-exec-activity-summary" id={summaryId}>
            <CadencedShimmerText active={lifecycle === "active"}>{summary}</CadencedShimmerText>
          </span>
        </span>
        <span className="octopus-exec-activity-chevron-wrap">
          <Glyph aria-hidden="true" className={`octopus-exec-activity-chevron icon-2xs shrink-0${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
        </span>
      </div>
      <motion.div
        animate={{ height: expanded ? bodyHeight : 0, opacity: expanded ? 1 : 0 }}
        aria-hidden={!expanded}
        className="octopus-exec-activity-body"
        data-thread-find-skip={expanded ? undefined : true}
        initial={false}
        inert={expanded ? undefined : true}
        style={{ overflow: expanded ? "visible" : "hidden", pointerEvents: expanded ? "auto" : "none" }}
        transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
      >
        <div className="octopus-exec-activity-body-inner" ref={bodyRef}>
          <div className="octopus-exec-shell">
            <div className="octopus-exec-shell-label">{isCode ? "Code" : "Shell"}</div>
            {shellCommand ? <div className="octopus-exec-shell-command-row group/command">
              <button
                aria-label={isCode ? shellCommand : `$ ${shellCommand}`}
                className="octopus-exec-shell-command"
                data-expanded={commandExpanded || undefined}
                onClick={() => setCommandExpanded(true)}
                type="button"
              >
                {isCode ? null : <span aria-hidden="true">$</span>}
                <code>{shellCommand}</code>
              </button>
              <ShellCopyButton label={isCode ? "Copy code" : "Copy command"} value={shellCommand} />
            </div> : <div className="octopus-exec-shell-no-output" role="status">Command unavailable</div>}
            {terminalInputs.map((input, index) => (
              <pre className="octopus-exec-shell-input" key={`${input}:${index}`}><span aria-hidden="true">&gt; </span>{input}</pre>
            ))}
            {(outputSections.length ? outputSections : [{ stream: "", text: "" }]).map((section, index) => <div className="octopus-exec-shell-output-row group/output" key={`${section.stream}:${index}`}>
              <pre className="octopus-exec-shell-output vertical-scroll-fade-mask">
                <span className="octopus-exec-shell-output-content">
                  {section.stream ? <span className="octopus-exec-shell-label">{section.stream}{"\n"}</span> : null}
                  {section.text ? <TerminalAnsiText text={section.text} /> : lifecycle === "active" ? null : <span className="octopus-exec-shell-no-output">{model.notices.length || model.phase === "unknown" ? "No output available" : "No output"}</span>}
                </span>
              </pre>
              <ShellCopyButton label={section.stream ? `Copy ${section.stream}` : "Copy output"} value={section.text} />
            </div>)}
            <div className="octopus-exec-shell-footer">
              {success ? (
                <span className="octopus-exec-shell-success"><Glyph aria-hidden="true" name="check" size={10} />Success</span>
              ) : (
                <span>{commandOutcomeLabel(model, isCode)}</span>
              )}
            </div>
            {model.notices.map((notice) => <div className="octopus-exec-shell-footer" key={notice}>{notice}</div>)}
          </div>
        </div>
      </motion.div>
    </div>
  );
}

function ShellCopyButton({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);
  const mountedRef = useRef(true);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
    };
  }, []);

  function onCopy() {
    void copyText(value).then((succeeded) => {
      if (!succeeded || !mountedRef.current) return;
      setCopied(true);
      if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
      resetTimerRef.current = window.setTimeout(() => {
        setCopied(false);
        resetTimerRef.current = null;
      }, 2_000);
    });
  }

  return (
    <span className="octopus-exec-shell-copy-anchor">
      <MessageTooltip label={copied ? "Copied" : label}>
        <button aria-label={copied ? "Copied" : label} className="octopus-exec-shell-copy" onClick={copied ? undefined : onCopy} type="button">
          <Glyph aria-hidden="true" name={copied ? "check" : "copy"} size={14} />
        </button>
      </MessageTooltip>
    </span>
  );
}

function TerminalAnsiText({ text }: { text: string }) {
  const segments = terminalAnsiSegments(text);
  return <>{segments.map((segment, index) => <span key={`${index}:${segment.text}`} style={segment.style}>{segment.text}</span>)}</>;
}

function terminalAnsiSegments(value: string): Array<{ style: CSSProperties; text: string }> {
  const segments: Array<{ style: CSSProperties; text: string }> = [];
  const expression = /\u001b\[([0-9;]*)m/g;
  let style: CSSProperties = {};
  let offset = 0;
  for (let match = expression.exec(value); match; match = expression.exec(value)) {
    const text = stripUnsupportedTerminalEscapes(value.slice(offset, match.index));
    if (text) segments.push({ style: { ...style }, text });
    const codes = (match[1] || "0").split(";").map((entry) => Number.parseInt(entry || "0", 10));
    style = applyTerminalAnsiCodes(style, codes);
    offset = expression.lastIndex;
  }
  const tail = stripUnsupportedTerminalEscapes(value.slice(offset));
  if (tail) segments.push({ style: { ...style }, text: tail });
  return segments;
}

function applyTerminalAnsiCodes(current: CSSProperties, codes: number[]): CSSProperties {
  let style = { ...current };
  for (let index = 0; index < codes.length; index += 1) {
    const code = codes[index] ?? 0;
    if (code === 0) {
      style = {};
      continue;
    }
    if (code === 1) style.fontWeight = 700;
    else if (code === 2) style.opacity = 0.65;
    else if (code === 3) style.fontStyle = "italic";
    else if (code === 4) style.textDecorationLine = terminalDecoration(style.textDecorationLine, "underline", true);
    else if (code === 9) style.textDecorationLine = terminalDecoration(style.textDecorationLine, "line-through", true);
    else if (code === 22) {
      delete style.fontWeight;
      delete style.opacity;
    } else if (code === 23) delete style.fontStyle;
    else if (code === 24) style.textDecorationLine = terminalDecoration(style.textDecorationLine, "underline", false);
    else if (code === 29) style.textDecorationLine = terminalDecoration(style.textDecorationLine, "line-through", false);
    else if (code === 39) delete style.color;
    else if (code === 49) delete style.backgroundColor;
    else if ((code >= 30 && code <= 37) || (code >= 90 && code <= 97)) {
      style.color = terminalAnsiColor(code >= 90 ? code - 90 + 8 : code - 30);
    } else if ((code >= 40 && code <= 47) || (code >= 100 && code <= 107)) {
      style.backgroundColor = terminalAnsiColor(code >= 100 ? code - 100 + 8 : code - 40);
    } else if (code === 38 || code === 48) {
      const property = code === 38 ? "color" : "backgroundColor";
      const mode = codes[index + 1];
      if (mode === 2 && codes.length > index + 4) {
        style[property] = `rgb(${boundedColor(codes[index + 2])} ${boundedColor(codes[index + 3])} ${boundedColor(codes[index + 4])})`;
        index += 4;
      } else if (mode === 5 && codes.length > index + 2) {
        style[property] = terminalAnsi256Color(codes[index + 2] ?? 0);
        index += 2;
      }
    }
  }
  return style;
}

function terminalDecoration(value: CSSProperties["textDecorationLine"], token: string, enabled: boolean): string | undefined {
  const decorations = new Set(String(value ?? "").split(/\s+/).filter(Boolean));
  if (enabled) decorations.add(token);
  else decorations.delete(token);
  return decorations.size ? [...decorations].join(" ") : undefined;
}

function terminalAnsiColor(index: number): string {
  const names = [
    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
    "BrightBlack", "BrightRed", "BrightGreen", "BrightYellow", "BrightBlue", "BrightMagenta", "BrightCyan", "BrightWhite",
  ];
  const name = names[Math.max(0, Math.min(15, index))] ?? "White";
  return `var(--vscode-terminal-ansi${name})`;
}

function terminalAnsi256Color(value: number): string {
  const index = Math.max(0, Math.min(255, Math.trunc(value)));
  if (index < 16) return terminalAnsiColor(index);
  if (index >= 232) {
    const shade = 8 + (index - 232) * 10;
    return `rgb(${shade} ${shade} ${shade})`;
  }
  const cube = index - 16;
  const component = (part: number) => part === 0 ? 0 : 55 + part * 40;
  const red = component(Math.floor(cube / 36));
  const green = component(Math.floor((cube % 36) / 6));
  const blue = component(cube % 6);
  return `rgb(${red} ${green} ${blue})`;
}

function boundedColor(value: number | undefined): number {
  return Math.max(0, Math.min(255, Math.trunc(value ?? 0)));
}

function normalizeTerminalControl(value: string): string {
  let output = value.replace(/\r+\n/g, "\n");
  while (/\r[^\r\n]/.test(output)) {
    output = output.replace(/^([^\r\n]*)\r+([^\r\n]+)/gm, (_match, before: string, after: string) => after + before.slice(after.length));
  }
  while (/[^\n]\u0008/.test(output)) output = output.replace(/[^\n]\u0008/g, "");
  return output.replace(/\u0000/g, "");
}

function stripUnsupportedTerminalEscapes(value: string): string {
  return value
    .replace(/\u001b\][^\u0007]*(?:\u0007|\u001b\\)/g, "")
    .replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "");
}

function dynamicToolExecMessage(message: ChatRenderMessage): ChatRenderMessage {
  const data = message.eventData ?? {};
  const argumentsValue = eventRecord(data.arguments);
  const body = commandResultBody(data);
  const command = suppliedShellCommand(data);
  return {
    ...message,
    eventData: {
      ...data,
      command,
      cwd: eventString(argumentsValue.workdir ?? argumentsValue.cwd ?? body.workdir ?? body.cwd),
    },
  };
}

function DynamicToolExecActivity({ message, onOpenFile }: {
  message: ChatRenderMessage;
  onOpenFile: Props["onOpenFile"];
}) {
  const sourceData = message.eventData ?? {};
  const payloadProjection = eventRecord(sourceData._payload_projection);
  const exactPayloadNeeded = payloadProjection.truncated === true
    && payloadProjection.contains_process_output === true;
  const [resolvedPayload, setResolvedPayload] = useState<Record<string, unknown> | null>(null);
  const [payloadError, setPayloadError] = useState("");

  useEffect(() => {
    setResolvedPayload(null);
    setPayloadError("");
    if (!exactPayloadNeeded || !message.artifactId || !message.interfaceId || !message.runId) return;
    let cancelled = false;
    readProjection<ArtifactActivityBytes>("projection.artifact.bytes.read", {
      artifact_id: message.artifactId,
      interface_id: message.interfaceId,
      run_id: message.runId,
    }).then((response) => {
      if (cancelled) return;
      if (
        response.artifact_id !== message.artifactId
        || response.interface_id !== message.interfaceId
        || response.run_id !== message.runId
      ) {
        throw new Error("Command artifact coordinates do not match the projected message.");
      }
      setResolvedPayload(eventRecord(decodeJsonArtifactPayload(response.base64)));
    }).catch((error) => {
      if (!cancelled) setPayloadError(error instanceof Error ? error.message : String(error));
    });
    return () => { cancelled = true; };
  }, [exactPayloadNeeded, message.artifactId, message.interfaceId, message.runId]);

  const data = resolvedPayload
    ? { ...sourceData, ...resolvedPayload, _payload_projection: { ...payloadProjection, resolved: true } }
    : sourceData;
  return (
    <>
      <ExecActivity message={dynamicToolExecMessage({ ...message, eventData: data })} onOpenFile={onOpenFile} />
      {payloadError ? <div className="activity-command-error">Full command output unavailable · {payloadError}</div> : null}
    </>
  );
}

function suppliedShellCommand(data: Record<string, unknown>): string {
  const argumentsValue = eventRecord(data.arguments);
  const parsed = eventRecord(data.parsed_cmd ?? data.parsedCmd);
  return shellCommandText(argumentsValue.command)
    || shellCommandText(argumentsValue.argv)
    || shellCommandText(argumentsValue.cmd)
    || shellCommandText(data.command)
    || shellCommandText(parsed.cmd)
    || shellCommandText(argumentsValue.script);
}

function shellCommandText(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) return "";
  return value.map((item) => shellArgument(item)).join(" ");
}

function shellArgument(value: string): string {
  if (/^[A-Za-z0-9_./:=+@%,-]+$/.test(value)) return value;
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

function dynamicToolFileMessage(message: ChatRenderMessage, presentation: DynamicToolPresentation): ChatRenderMessage {
  const data = message.eventData ?? {};
  const argumentsValue = eventRecord(data.arguments);
  const body = eventRecord(data.body ?? data.result);
  const evidence = eventRecord(body.evidence);
  const declaredChanges = eventArray(body.changes).map((entry) => eventRecord(entry));
  const changedPaths = eventArray(
    body.changed_paths
    ?? body.changedPaths
    ?? evidence.changed_paths
    ?? evidence.changedPaths,
  ).map(eventString).filter(Boolean);
  const argumentPath = eventString(argumentsValue.path);
  const target = argumentPath || presentation.target;
  const operation = dynamicToolOperationLabel(presentation);
  const changes = declaredChanges.length
    ? declaredChanges
    : (changedPaths.length ? changedPaths : target ? [target] : []).map((path) => ({
      kind: "update",
      path,
    }));
  return {
    ...message,
    eventKind: "file_change",
    eventData: {
      ...data,
      changes,
    },
    text: [operation, target].filter(Boolean).join(" · "),
  };
}

function MovementEvidence({ message }: { message: ChatRenderMessage }) {
  const data = message.eventData ?? {};
  const movement = eventRecord(data._movement);
  const runtime = eventRecord(eventRecord(movement.metadata).runtime);
  const from = eventEndpoint(movement.from);
  const to = eventEndpoint(movement.to);
  const seq = typeof movement.seq === "number" && Number.isSafeInteger(movement.seq) ? String(movement.seq) : "";
  return (
    <StructuredActivity
      glyph="binding"
      label={[message.label || "Artifact movement", from && to ? `${from} → ${to}` : ""].filter(Boolean).join(" · ")}
      status="complete"
      details={[
        ["Movement", seq],
        ["Artifact", message.artifactId ?? ""],
        ["Run", eventString(runtime.run) || message.runId || ""],
        ["Checkpoint", eventString(runtime.checkpoint)],
        ["Interface", eventString(runtime.interface) || message.interfaceId || ""],
        ["Evidence", message.text],
      ]}
    />
  );
}

function LifecycleEvidence({ kind, message }: {
  kind: "turn_started" | "turn_completed" | "item_started";
  message: ChatRenderMessage;
}) {
  const data = message.eventData ?? {};
  const identity = eventRecord(data._event);
  const movement = eventRecord(data._movement);
  const lifecycle = eventString(data.lifecycle) || eventString(identity.lifecycle) || (kind === "turn_completed" ? "completed" : "started");
  return (
    <StructuredActivity
      glyph="pulse"
      label={humanizeStatus(kind)}
      status={lifecycle}
      details={[
        ["Turn", eventString(data.turn_id ?? data.turnId ?? identity.turn_id)],
        ["Item", eventString(data.item_id ?? data.itemId ?? identity.item_id)],
        ["Movement", typeof movement.seq === "number" ? String(movement.seq) : ""],
        ["Artifact", message.artifactId ?? ""],
      ]}
    />
  );
}

function FileActivity({ message, onOpenFile }: { message: ChatRenderMessage; onOpenFile: Props["onOpenFile"] }) {
  const sourceData = message.eventData ?? {};
  const payloadProjection = eventRecord(sourceData._payload_projection);
  const exactPayloadNeeded = payloadProjection.truncated === true
    && payloadProjection.contains_unified_diff === true;
  const [resolvedPayload, setResolvedPayload] = useState<Record<string, unknown> | null>(null);
  const [payloadError, setPayloadError] = useState("");

  useEffect(() => {
    setResolvedPayload(null);
    setPayloadError("");
    if (!exactPayloadNeeded || !message.artifactId || !message.interfaceId || !message.runId) return;
    let cancelled = false;
    readProjection<ArtifactActivityBytes>("projection.artifact.bytes.read", {
      artifact_id: message.artifactId,
      interface_id: message.interfaceId,
      run_id: message.runId,
    }).then((response) => {
      if (cancelled) return;
      if (
        response.artifact_id !== message.artifactId
        || response.interface_id !== message.interfaceId
        || response.run_id !== message.runId
      ) {
        throw new Error("Activity artifact coordinates do not match the projected message.");
      }
      const payload = decodeJsonArtifactPayload(response.base64);
      setResolvedPayload(eventRecord(payload));
    }).catch((error) => {
      if (!cancelled) setPayloadError(error instanceof Error ? error.message : String(error));
    });
    return () => { cancelled = true; };
  }, [exactPayloadNeeded, message.artifactId, message.interfaceId, message.runId]);

  const data = resolvedPayload
    ? { ...sourceData, ...resolvedPayload, _payload_projection: sourceData._payload_projection }
    : sourceData;
  if (message.eventKind === "file_change_delta") {
    return (
      <div className="octopus-inline-activity min-w-0">
        <Glyph aria-hidden="true" className="icon-xs shrink-0" name="edit" />
        <CadencedShimmerText>Editing files</CadencedShimmerText>
      </div>
    );
  }
  const changes = projectedActivityFileChanges(data, Boolean(exactPayloadNeeded && !resolvedPayload));
  const projectedDiff = exactPayloadNeeded && !resolvedPayload
    ? ""
    : activityUnifiedDiff(data) || eventString(data._projected_diff) || eventString(data.diff);
  if (!changes.length) {
    if (!projectedDiff && message.eventKind !== "diff") return null;
    return (
      <FileChangeActivity
        changeKind="update"
        detailsLoading={Boolean(exactPayloadNeeded && !resolvedPayload && !payloadError)}
        diff={projectedDiff || message.text}
        lifecycle="complete"
        onOpenFile={onOpenFile}
        path={eventString(data.path) || "files"}
      />
    );
  }
  const lifecycle = activityLifecycle(message, eventString(data.status));
  return (
    <div className="activity-file-list">
      {changes.map((change, index) => {
        const path = change.path || `File ${index + 1}`;
        const changeKind = change.kind || "edit";
        return (
          <FileChangeActivity
            added={change.added}
            changeKind={changeKind}
            deleted={change.deleted}
            detailsLoading={Boolean(exactPayloadNeeded && !resolvedPayload && !payloadError)}
            diff={change.diff || (changes.length === 1 ? projectedDiff : "")}
            key={`${path}:${index}`}
            lifecycle={lifecycle}
            onOpenFile={onOpenFile}
            path={path}
          />
        );
      })}
      {payloadError ? <div className="activity-file-error">Full diff unavailable · {payloadError}</div> : null}
    </div>
  );
}

function FileChangeActivity({ added, changeKind, deleted, detailsLoading = false, diff, lifecycle, onOpenFile, path }: {
  added?: number;
  changeKind: ActivityFileChangeKind;
  deleted?: number;
  detailsLoading?: boolean;
  diff: string;
  lifecycle: ActivityLifecycle;
  onOpenFile: Props["onOpenFile"];
  path: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const reducedMotion = useShouldReduceMotion();
  const operation = changeKind === "add"
    ? "creating"
    : changeKind === "delete"
      ? "deleting"
      : "editing";
  const action = lifecycle === "active"
    ? `${operation[0].toLocaleUpperCase()}${operation.slice(1)}`
    : lifecycle === "stopped"
      ? `Stopped ${operation}`
      : lifecycle === "error"
        ? "Rejected"
        : operation === "creating" ? "Created" : operation === "deleting" ? "Deleted" : "Edited";
  const active = lifecycle === "active";
  const hasStats = added !== undefined || deleted !== undefined;
  const verifiedNoContentChange = lifecycle === "complete"
    && changeKind === "update"
    && added === 0
    && deleted === 0;
  const completedExpanded = expanded && lifecycle === "complete";
  const summary = completedExpanded
    ? changeKind === "add" ? "Created file" : changeKind === "delete" ? "Deleted file" : "Edited file"
    : action;
  const contents = (
    <>
      <Glyph aria-hidden="true" className="icon-xs shrink-0" name="edit" />
      <span className="activity-file-summary">
        <CadencedShimmerText active={active} className="octopus-activity-action">{summary}</CadencedShimmerText>
        {!completedExpanded ? <>{" "}
          <MessageTooltip label={<code>{path}</code>} onActivate={() => onOpenFile(path)} variant="activity-target">
            <span className="octopus-activity-file-target">{activityPathLabel(path)}</span>
          </MessageTooltip>
        </> : null}
      </span>
      {hasStats && !completedExpanded ? (
        <span className="activity-file-stats">
          <b>+{added ?? 0}</b>
          <i>−{deleted ?? 0}</i>
        </span>
      ) : null}
      {hasStats && !completedExpanded && changeKind !== "update" ? <span className={`activity-file-state-dot is-${changeKind}`} /> : null}
      <Glyph className={`activity-file-chevron icon-2xs${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
    </>
  );
  return (
    <div className="activity-file-row" data-state={lifecycle}>
      <div className="activity-file-header group/activity-header">
        <button
          aria-expanded={expanded}
          aria-label={`Toggle diff for ${activityPathLabel(path)}`}
          className="activity-file-disclosure-button cursor-interaction"
          onClick={() => setExpanded((current) => !current)}
          type="button"
        />
        {contents}
      </div>
      <motion.div
        animate={{ height: expanded ? "auto" : 0, opacity: expanded ? 1 : 0 }}
        className="activity-file-body"
        initial={false}
        style={{ overflow: expanded ? "visible" : "hidden", pointerEvents: expanded ? "auto" : "none" }}
        transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
      >
        {detailsLoading ? (
          <OcLoadingRows className="activity-file-empty-detail" label="Loading full diff…" />
        ) : diff.trim() ? (
          <InlineUnifiedDiff added={added} changeKind={changeKind} deleted={deleted} diff={diff} path={path} />
        ) : (
          <div className="activity-file-empty-detail">{verifiedNoContentChange ? "No changes" : "Diff unavailable"}</div>
        )}
      </motion.div>
    </div>
  );
}

type ArtifactActivityBytes = {
  artifact_id?: unknown;
  base64?: unknown;
  interface_id?: unknown;
  run_id?: unknown;
};

type ProjectedActivityFileChange = {
  added?: number;
  deleted?: number;
  diff?: string;
  kind: ActivityFileChangeKind;
  path: string;
};

type ActivityFileChangeKind = "add" | "delete" | "update";

function decodeJsonArtifactPayload(value: unknown): unknown {
  const base64 = eventString(value);
  if (!base64) throw new Error("Activity artifact bytes are missing.");
  const binary = atob(base64);
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  const text = new TextDecoder().decode(bytes);
  return JSON.parse(text);
}

function activityUnifiedDiff(data: Record<string, unknown>): string {
  const body = eventRecord(data.body ?? data.result);
  const evidence = eventRecord(body.evidence);
  return eventString(
    evidence.unified_diff
    ?? evidence.unifiedDiff
    ?? body.unified_diff
    ?? body.unifiedDiff,
  );
}

function projectedActivityFileChanges(
  data: Record<string, unknown>,
  suppressProjectedDiff: boolean,
): ProjectedActivityFileChange[] {
  const body = eventRecord(data.body ?? data.result);
  const declared = eventArray(body.changes ?? data.changes).map((entry) => eventRecord(entry));
  const evidence = eventRecord(body.evidence);
  const changedPaths = eventArray(
    body.changed_paths
    ?? body.changedPaths
    ?? evidence.changed_paths
    ?? evidence.changedPaths,
  ).map(eventString).filter(Boolean);
  const unified = suppressProjectedDiff
    ? ""
    : activityUnifiedDiff(data) || eventString(data._projected_diff) || eventString(data.diff);
  const parsed = unifiedDiffFileChanges(unified);
  const parsedByPath = new Map(parsed.map((change) => [change.path, change]));
  const base = declared.length
    ? declared.map((change, index) => ({
      added: optionalEventNumber(change.added ?? change.lines_added ?? change.linesAdded),
      deleted: optionalEventNumber(change.deleted ?? change.lines_removed ?? change.linesRemoved),
      diff: eventString(change.diff ?? change.unified_diff ?? change.unifiedDiff),
      kind: activityFileChangeKind(change.kind ?? change.type),
      path: activityFileChangePath(change) || `File ${index + 1}`,
    }))
    : changedPaths.map((path) => ({ kind: "update" as const, path }));
  if (!base.length) return parsed;
  const merged = base.map((change) => ({ ...change, ...parsedByPath.get(change.path) }));
  const declaredPaths = new Set(merged.map((change) => change.path));
  return [...merged, ...parsed.filter((change) => !declaredPaths.has(change.path))];
}

function activityFileChangePath(change: Record<string, unknown>): string {
  const kind = eventRecord(change.kind);
  return eventString(kind.movePath ?? kind.move_path) || eventString(change.path ?? change.file);
}

function activityFileChangeKind(value: unknown): ActivityFileChangeKind {
  const declared = eventString(value) || eventString(eventRecord(value).type);
  if (declared === "add") return "add";
  if (declared === "delete") return "delete";
  return "update";
}

function unifiedDiffFileChanges(diff: string): ProjectedActivityFileChange[] {
  if (!diff.trim()) return [];
  const starts = [...diff.matchAll(/^diff --git /gm)].map((match) => match.index ?? 0);
  const sections = starts.length
    ? starts.map((start, index) => diff.slice(start, starts[index + 1] ?? diff.length))
    : [diff];
  return sections.flatMap((section, index) => {
    const path = unifiedDiffFilePath(section) || (sections.length === 1 ? "files" : `File ${index + 1}`);
    let added = 0;
    let deleted = 0;
    let inHunk = false;
    for (const line of section.split("\n")) {
      if (line.startsWith("@@")) {
        inHunk = true;
        continue;
      }
      if (!inHunk) continue;
      if (line.startsWith("+") && !line.startsWith("+++")) added += 1;
      if (line.startsWith("-") && !line.startsWith("---")) deleted += 1;
    }
    const kind: ActivityFileChangeKind = /^new file mode /m.test(section)
      ? "add"
      : /^deleted file mode /m.test(section)
        ? "delete"
        : "update";
    return [{ added, deleted, diff: section, kind, path }];
  });
}

function unifiedDiffFilePath(section: string): string {
  const added = section.match(/^\+\+\+ (.+)$/m)?.[1]?.trim() ?? "";
  const removed = section.match(/^--- (.+)$/m)?.[1]?.trim() ?? "";
  const candidate = added !== "/dev/null" ? added : removed !== "/dev/null" ? removed : "";
  return candidate.replace(/^[ab]\//, "").replace(/^"|"$/g, "");
}

function InlineUnifiedDiff({ added, changeKind, deleted, diff, path }: {
  added?: number;
  changeKind: ActivityFileChangeKind;
  deleted?: number;
  diff: string;
  path: string;
}) {
  return (
    <Suspense fallback={<OcLoadingRows className="activity-file-diff-loading" label="Loading diff…" />}>
      <WorkingPatchDiff added={added} changeKind={changeKind} deleted={deleted} patch={diff} path={path} />
    </Suspense>
  );
}

function CollaborationActivity({ message }: { message: ChatRenderMessage }) {
  const data = message.eventData ?? {};
  const agentPath = eventString(data.agentPath) || eventString(data.agent_path);
  const receivers = eventArray(data.receiverThreadIds ?? data.receiver_thread_ids).map(eventString).filter(Boolean);
  const tool = eventString(data.tool);
  const kind = eventString(data.kind);
  return (
    <StructuredActivity
      glyph="user"
      label={message.eventKind === "subagent_activity"
        ? `${humanizeStatus(kind || "activity")} ${agentPath || "subagent"}`
        : `${humanizeStatus(tool || "collaboration")} ${receivers.length ? receivers.join(", ") : agentPath}`.trim()}
      status={eventString(data.status)}
      details={[["Prompt", eventString(data.prompt)], ["Model", eventString(data.model)], ["Reasoning", eventString(data.reasoningEffort ?? data.reasoning_effort)]]}
    />
  );
}

function StructuredActivity({ glyph, label, status, details, danger = false, relatedRuns }: {
  glyph: Parameters<typeof Glyph>[0]["name"] | null;
  label: string;
  status: string;
  details: Array<[string, string]>;
  danger?: boolean;
  relatedRuns?: ChatRenderMessage["relatedRuns"];
}) {
  const visible = details.filter((entry) => entry[1]);
  const lifecycle = activityLifecycleFromStatus(status, danger);
  return (
    <ActivityDisclosure
      active={lifecycle === "active"}
      canExpand={visible.length > 0}
      className="structured-activity"
      icon={glyph}
      summary={label}
      summaryKey={label}
      tone={danger ? "danger" : "default"}
    >
      {() => <div className="structured-activity-body">
        {relatedRuns?.map((run) => <button className="text-token-text-link cursor-interaction" key={run.runId}
          onClick={() => void window.octopusTransport?.openRunInMain(run.runId)} type="button">{run.title}</button>)}
        <dl>
          {visible.map(([name, value], index) => <div key={`${name}:${index}`}><dt>{name}</dt><dd>{value}</dd></div>)}
        </dl>
      </div>}
    </ActivityDisclosure>
  );
}

function ActivityCodeBlock({ label, text }: { label: string; text: string }) {
  const [expanded, setExpanded] = useState(false);
  const reducedMotion = useShouldReduceMotion();
  return (
    <div className="activity-code-block">
      <button
        aria-expanded={expanded}
        className="group/activity-header cursor-interaction"
        onClick={() => setExpanded((current) => !current)}
        type="button"
      >
        <Glyph aria-hidden="true" className="icon-xs shrink-0" name="terminal" />
        <span>{label}</span>
        <Glyph className={`activity-code-block-chevron icon-2xs${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
      </button>
      <motion.div
        animate={{ height: expanded ? "auto" : 0, opacity: expanded ? 1 : 0 }}
        initial={false}
        style={{ overflow: expanded ? "visible" : "hidden", pointerEvents: expanded ? "auto" : "none" }}
        transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
      >
        {text ? <pre>{text}</pre> : null}
      </motion.div>
    </div>
  );
}

function ConversationNotice({ detail, glyph, title, danger = false }: {
  detail: string;
  glyph: Parameters<typeof Glyph>[0]["name"];
  title: string;
  danger?: boolean;
}) {
  return (
    <div className={`conversation-notice${danger ? " is-danger" : ""}`}>
      <Glyph name={glyph} size={13} />
      <div><strong>{title}</strong>{detail ? <span>{detail}</span> : null}</div>
    </div>
  );
}

function dynamicToolResult(data: Record<string, unknown>): string {
  const items = eventArray(data.contentItems ?? data.content_items).map((entry) => {
    const item = eventRecord(entry);
    return eventString(item.text) || eventString(item.imageUrl ?? item.image_url) || eventJson(entry);
  }).filter(Boolean);
  if (items.length) return items.join("\n");
  return [data.body, data.result, data.error].map(eventJson).filter(Boolean).join("\n");
}

function rateLimitDetails(data: Record<string, unknown>): Array<[string, string]> {
  const values: Array<[string, string]> = [];
  for (const [key, value] of Object.entries(data)) {
    if (value == null || typeof value === "object") continue;
    values.push([humanizeStatus(key), String(value)]);
    if (values.length === 6) break;
  }
  return values;
}

type ActivityLifecycle = "active" | "complete" | "error" | "stopped";

function activityLifecycle(message: ChatRenderMessage, status: string): ActivityLifecycle {
  if (message.pending) return "active";
  return activityLifecycleFromStatus(status, Boolean(message.error));
}

function activityLifecycleFromStatus(status: string, danger = false): ActivityLifecycle {
  if (danger) return "error";
  const normalized = status.trim().toLocaleLowerCase();
  if (["failed", "failure", "error", "declined", "rejected", "timed_out", "timeout"].includes(normalized)) return "error";
  if (["stopped", "interrupted", "cancelled", "canceled", "aborted"].includes(normalized)) return "stopped";
  if (["started", "starting", "running", "working", "in_progress", "inprogress", "pending", "queued", "streaming", "progress"].includes(normalized)) return "active";
  return "complete";
}

function usageDetails(data: Record<string, unknown>): Array<[string, string]> {
  return Object.entries(data).flatMap(([key, value]) => {
    if (key.startsWith("_")) return [];
    const rendered = eventJson(value);
    return rendered ? [[humanizeStatus(key), rendered] as [string, string]] : [];
  });
}

function eventEndpoint(value: unknown): string {
  const endpoint = eventRecord(value);
  return Object.entries(endpoint)
    .map(([kind, id]) => [kind, eventString(id)] as const)
    .filter((entry) => entry[1])
    .map(([kind, id]) => `${kind}:${id}`)
    .join(", ");
}

function humanizeStatus(value: string): string {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[._-]+/g, " ")
    .trim()
    .replace(/^./, (character) => character.toUpperCase());
}

function eventJson(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "";
  }
}

function ChatEventCard({ eyebrow, text }: { eyebrow: string; text: string }) {
  return (
    <div className="octopus-chat-event">
      <div>{eyebrow}</div>
      {text ? <div>{text}</div> : null}
    </div>
  );
}

function eventRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function eventString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function eventArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function eventNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function optionalEventNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function reasoningBody(value: string, streaming: boolean): string {
  const content = value.trimStart();
  if (streaming) {
    const heading = content.match(/^\*\*([^\n]*?)\*\*/);
    if (heading) return content.slice(heading[0].length).trimStart();
    return content.startsWith("**") ? "" : content;
  }
  const strongHeading = content.match(/^\*\*[^\n]*?\*\*\s*/);
  if (strongHeading) return content.slice(strongHeading[0].length).trim();
  const markdownHeading = content.match(/^#{1,3}[ \t]+([^#\\*_[\]`<>&\r\n]+)(?:\r?\n|$)/);
  return markdownHeading?.[1]?.trim() ? content.slice(markdownHeading[0].length).trim() : content;
}

function MessageAttachments({ message }: { message: ChatRenderMessage }) {
  if (!message.attachments?.length) return null;
  return <ChatAttachmentStrip attachments={message.attachments} className={`octopus-message-attachments${message.role === "user" ? " is-user" : ""}`} scope={message} />;
}

function MessageActions({
  message,
  isLatestTurn = true,
  onContinueInNewTask,
  onEditUserMessage,
  editLabel = "Edit message",
  onTogglePinnedMessage,
  pinned,
  pinning,
  taskActionsEnabled,
  user = false,
}: {
  message: ChatRenderMessage;
  isLatestTurn?: boolean;
  onContinueInNewTask?: Props["onContinueInNewTask"];
  onEditUserMessage?: (message: ChatRenderMessage) => void;
  editLabel?: string;
  onTogglePinnedMessage: (message: ChatRenderMessage) => Promise<void>;
  pinned: boolean;
  pinning: boolean;
  taskActionsEnabled: boolean;
  user?: boolean;
}) {
  if (!message.text && !message.artifactId) return null;
  const canContinue = taskActionsEnabled
    && !user
    && message.terminal !== false
    && ["message", "error"].includes(message.eventKind ?? "message")
    && Boolean(message.artifactId && message.runId);
  const buttons = (
    <div className="flex items-center gap-0.5">
      {message.text ? (
        <CopyActionButton label={user ? "Copy message" : "Copy"} value={message.text} />
      ) : null}
      {taskActionsEnabled && user && onEditUserMessage && message.eventKind !== "steering" && !message.pending ? (
        <MessageActionButton
          glyph="edit"
          label={editLabel}
          onClick={() => onEditUserMessage?.(message)}
        />
      ) : null}
      {!user && message.text ? <ReadAloudActionButton messageId={message.id} text={message.text} /> : null}
      {taskActionsEnabled && message.artifactId ? (
        <MessageActionButton
          active={pinned}
          disabled={pinning}
          glyph={pinned ? "pin" : "pinOff"}
          label={pinned ? "Unpin message" : "Pin message for future turns"}
          onClick={() => void onTogglePinnedMessage(message)}
        />
      ) : null}
      {canContinue ? (
        <ContinueTaskAction
          isLatestTurn={isLatestTurn}
          message={message}
          onContinue={onContinueInNewTask}
        />
      ) : null}
    </div>
  );

  if (user) {
    return (
      <div className="flex flex-row-reverse items-center gap-1">
        <div className="mr-1 ms-1 flex items-center gap-2 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100">
          <span className="flex opacity-0 group-focus-within:opacity-100 group-hover:opacity-100">
            <time className="text-xs text-tertiary" dateTime={new Date(message.createdAtMs).toISOString()}>
              {formatMessageTime(message.createdAtMs)}
            </time>
          </span>
          {buttons}
        </div>
      </div>
    );
  }

  return (
    <div className="mt-1.5 flex h-5 items-center justify-start gap-0.5 electron:-translate-x-1 extension:-translate-x-1.5 [&_button]:focus-visible:ring-2 [&_button]:focus-visible:ring-token-focus-border [&_button]:focus-visible:ring-offset-0">
      <div className="flex h-full items-center gap-0.5">
        {buttons}
      </div>
      {typeof message.eventData?._client_text_edit === "object" && message.eventData._client_text_edit ? (
        <span className="ml-1.5 text-xs text-tertiary" title={`Edited display text. Original: ${String((message.eventData._client_text_edit as Record<string, unknown>).original_text ?? "")}`}>Edited</span>
      ) : null}
      <span className="ml-1.5 flex h-full items-center opacity-0 group-focus-within:opacity-100 group-hover:opacity-100" data-assistant-message-sent-time="true">
        <time className="text-xs text-tertiary" dateTime={new Date(message.createdAtMs).toISOString()}>
          {formatMessageTime(message.createdAtMs)}
        </time>
      </span>
    </div>
  );
}

function ContinueTaskAction({ isLatestTurn, message, onContinue }: {
  isLatestTurn: boolean;
  message: ChatRenderMessage;
  onContinue?: Props["onContinueInNewTask"];
}) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const choiceRef = useRef<HTMLButtonElement>(null);
  const continueTask = () => {
    setConfirmOpen(false);
    onContinue?.(message);
  };
  return (
    <>
      <MessageActionButton
        glyph="worktree"
        label="Save and send"
        onClick={() => isLatestTurn ? continueTask() : setConfirmOpen(true)}
      />
      <OcDialog
        ariaLabelledBy="continue-task-dialog-title"
        backdropClassName="continue-task-dialog-backdrop"
        initialFocusRef={choiceRef}
        onClose={() => setConfirmOpen(false)}
        open={confirmOpen}
        panelClassName="continue-task-dialog"
      >
            <OcDialogHeader
              description="Later artifacts and external changes remain in the current task, so the new task may not match its latest projected state."
              leading={<Glyph name="worktree" size={18} />}
              title="Continue from this message?"
              titleId="continue-task-dialog-title"
            />
            <button autoFocus className="continue-task-dialog-choice" onClick={continueTask} ref={choiceRef} type="button">
              <Glyph name="newTask" size={16} />
              <span><strong>New task</strong><small>Continue from this message in a new Octopus task</small></span>
            </button>
            <OcDialogFooter>
              <OcButton onClick={() => setConfirmOpen(false)} size="sm" variant="secondary">Cancel</OcButton>
            </OcDialogFooter>
      </OcDialog>
    </>
  );
}

function MessageActionButton({ active = false, disabled = false, glyph, label, onClick }: {
  active?: boolean;
  disabled?: boolean;
  glyph: "edit" | "worktree" | "pin" | "pinOff" | "volume" | "stop";
  label: string;
  onClick: () => void;
}) {
  return (
    <MessageTooltip label={label}>
      <button
        aria-label={label}
        aria-pressed={glyph === "worktree" ? undefined : active}
        className={active ? ACTION_BUTTON_ACTIVE : ACTION_BUTTON}
        disabled={disabled}
        onClick={onClick}
        type="button"
      >
        <Glyph className="icon-xs" name={glyph} size={16} />
      </button>
    </MessageTooltip>
  );
}

const READ_ALOUD_START_EVENT = "octopus:read-aloud-start";
let activeReadAloudId = "";

function ReadAloudActionButton({ messageId, text }: { messageId: string; text: string }) {
  const [speaking, setSpeaking] = useState(false);
  const utteranceRef = useRef<SpeechSynthesisUtterance | null>(null);
  const supported = typeof window !== "undefined"
    && "speechSynthesis" in window
    && "SpeechSynthesisUtterance" in window;

  useEffect(() => {
    if (!supported) return;
    const onOtherStart = (event: Event) => {
      if (!(event instanceof CustomEvent) || event.detail?.messageId === messageId) return;
      setSpeaking(false);
    };
    window.addEventListener(READ_ALOUD_START_EVENT, onOtherStart);
    return () => {
      window.removeEventListener(READ_ALOUD_START_EVENT, onOtherStart);
      if (utteranceRef.current) {
        utteranceRef.current.onend = null;
        utteranceRef.current.onerror = null;
        utteranceRef.current = null;
      }
      if (activeReadAloudId === messageId) {
        window.speechSynthesis.cancel();
        activeReadAloudId = "";
      }
    };
  }, [messageId, supported]);

  if (!supported) return null;
  return (
    <MessageActionButton
      active={speaking}
      glyph={speaking ? "stop" : "volume"}
      label={speaking ? "Stop reading" : "Read aloud"}
      onClick={() => {
        if (speaking) {
          window.speechSynthesis.cancel();
          if (activeReadAloudId === messageId) activeReadAloudId = "";
          setSpeaking(false);
          return;
        }
        window.speechSynthesis.cancel();
        const utterance = new SpeechSynthesisUtterance(text.slice(0, 50_000));
        utteranceRef.current = utterance;
        activeReadAloudId = messageId;
        const finish = () => {
          if (activeReadAloudId === messageId) activeReadAloudId = "";
          if (utteranceRef.current === utterance) utteranceRef.current = null;
          setSpeaking(false);
        };
        utterance.onend = finish;
        utterance.onerror = finish;
        setSpeaking(true);
        window.dispatchEvent(new CustomEvent(READ_ALOUD_START_EVENT, { detail: { messageId } }));
        window.speechSynthesis.speak(utterance);
      }}
    />
  );
}

function CopyActionButton({
  label,
  title = "Copy",
  value,
}: {
  label: string;
  title?: string;
  value: string;
}) {
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
  }, []);

  function onCopy() {
    void copyText(value).then((copied) => {
      if (!copied) return;
      setCopied(true);
      if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
      resetTimerRef.current = window.setTimeout(() => {
        setCopied(false);
        resetTimerRef.current = null;
      }, 1_500);
    });
  }

  return (
    <MessageTooltip label={copied ? "Copied" : title}>
      <button
        aria-label={copied ? "Copied" : label}
        className={ACTION_BUTTON}
        onClick={onCopy}
        type="button"
      >
        <Glyph className="icon-xs" name={copied ? "check" : "copy"} size={16} />
      </button>
    </MessageTooltip>
  );
}

function InteractiveHeroMark() {
  const reducedMotion = useShouldReduceMotion();

  return (
    <motion.button
      aria-label="Octopus"
      className="relative size-14 cursor-interaction border-0 bg-transparent p-0 text-token-foreground opacity-30 outline-none transition-opacity duration-basic ease-enter-snappy motion-reduce:transition-none [@media(hover:hover)_and_(pointer:fine)]:hover:opacity-40 focus-visible:opacity-50 focus-visible:ring-2 focus-visible:ring-token-focus-border"
      transition={{ type: "spring", stiffness: 520, damping: 34, mass: 0.55 }}
      type="button"
      whileTap={reducedMotion ? { opacity: 0.42 } : { opacity: 0.56, scale: 0.9 }}
    >
      <img alt="" className="absolute inset-0 size-full object-contain" draggable={false} src="./octopus-logo.png" />
    </motion.button>
  );
}

function formatMessageTime(value: number): string {
  if (!value) return "";
  const sent = new Date(value);
  const now = new Date();
  const sentDay = new Date(sent.getFullYear(), sent.getMonth(), sent.getDate());
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const dayDelta = Math.round((sentDay.getTime() - today.getTime()) / 86_400_000);
  if (dayDelta === 0) {
    return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(sent);
  }
  if (dayDelta < 0 && dayDelta > -7) {
    return new Intl.DateTimeFormat(undefined, {
      weekday: "long",
      hour: "numeric",
      minute: "2-digit",
    }).format(sent);
  }
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(sent);
}

function formatLatency(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "";
  if (value < 1_000) return `${Math.round(value)} ms`;
  return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`;
}

function formatActivityElapsed(value: number): string {
  const seconds = Math.max(0, Math.floor(value / 1_000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`;
}

function searchableMessageText(value: string): string {
  return value
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`]*`/g, "");
}

function countSearchMatches(value: string, query: string): number {
  if (!value || !query) return 0;
  const haystack = value.toLocaleLowerCase();
  const needle = query.toLocaleLowerCase();
  let count = 0;
  let cursor = 0;
  while (cursor <= haystack.length - needle.length) {
    const index = haystack.indexOf(needle, cursor);
    if (index < 0) break;
    count += 1;
    cursor = index + Math.max(1, needle.length);
  }
  return count;
}
