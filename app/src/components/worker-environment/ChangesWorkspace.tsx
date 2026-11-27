import { OcLoadingRows } from "../../design-system/LoadingSkeleton";
import { parseDiffFromFile, preloadHighlighter, type FileDiffMetadata, type SelectedLineRange } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import { useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { OcMenuItem, OcMenuMessage, OcMenuSurface } from "../../design-system/components";
import { useAppearance } from "../../appearance";
import type { WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type { OctopusWorkerEnvironmentGitSnapshot, OctopusWorkerEnvironmentOperatorAction } from "../../electron";
import { copyText } from "../../runtimeFormat";
import { appToast } from "../../toasts";
import { Glyph } from "../IconGlyphs";
import { octopusDiffRawUnsafeCss } from "../settings/octopusDiffUnsafeCss";
import { octopusDiffThemeName, registerOctopusDiffThemes } from "../settings/octopusDiffThemes";
import {
  OperatorDiffReviewAnnotation,
  OperatorDiffReviewBar,
  AssistantCodeReviewBar,
  buildOperatorDiffReviewAnnotations,
  dispatchOperatorDiffReview,
  useOperatorDiffReviewDraft,
  type OperatorDiffReviewAnnotationMetadata,
  type OperatorDiffReviewScope,
  type OperatorDiffReviewSelection,
} from "./OperatorDiffReview";
import { directivePathWithinRoot, normalizeDirectivePath, type ProjectedAssistantCodeComment } from "../chat/assistantDirectives";
import { WorktreeOperatorConfirm, type WorktreeOperatorConfirmation } from "./WorktreeOperatorActions";
import { OctopusFileTree, OctopusFilterField, octopusTreeAncestors } from "../OctopusSurface";
import {
  dispatchWorkerEnvironmentLocalMutation,
  subscribeWorkerEnvironmentLocalMutations,
} from "./workerEnvironmentLocalMutation";
export { surfaceHasProjectedChanges } from "./changesProjection";

type ChangeStage = "branch" | "staged" | "unstaged";
type ChangeStatus = "added" | "deleted" | "modified" | "renamed" | "unknown";
type DiffLayout = "unified" | "split";
type ReviewScopeFilter = ChangeStage;
type OperatorChangeState = { content?: "applied" | "reverted"; stage?: Exclude<ChangeStage, "branch"> };

type ProjectedChange = {
  after: string;
  binary: boolean;
  before: string;
  previewOmitted: boolean;
  fileDiff: FileDiffMetadata | null;
  path: string;
  previousPath: string;
  stage: ChangeStage;
  status: ChangeStatus;
  stats: { additions: number; deletions: number };
};

type ChangesWorkspaceProps = {
  active: boolean;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  emptyIllustration?: ReactNode;
  fileSurface?: WorkerEnvironmentSurfaceProjection;
  onOpenFile?: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  projectionRevision: string;
  runId: string;
  surface: WorkerEnvironmentSurfaceProjection;
  workerEnvironmentId: string;
};

type ReviewFileMenu = {
  path: string;
  x: number;
  y: number;
};

const MAX_REVIEW_EXCERPT_LINES = 20;
const MAX_REVIEW_EXCERPT_CHARS = 2_400;

const diffThemeRegistrationKey = Symbol.for("octopus.diff-themes.registered");
const diffThemeRegistry = globalThis as typeof globalThis & { [diffThemeRegistrationKey]?: boolean };
if (!diffThemeRegistry[diffThemeRegistrationKey]) {
  registerOctopusDiffThemes();
  diffThemeRegistry[diffThemeRegistrationKey] = true;
}

const changesUnsafeCss = `${octopusDiffRawUnsafeCss}
[data-file] {
  --diffs-font-family: var(--font-mono);
  --diffs-font-size: var(--code-font-size, 12px);
  --diffs-line-height: round(nearest, calc(var(--diffs-font-size, 12px) * 1.8), 1px);
  line-height: var(--diffs-line-height);
}`;

export function ChangesWorkspace({ active, assistantCodeComments = [], emptyIllustration, fileSurface, onOpenFile, projectionRevision, runId, surface, workerEnvironmentId }: ChangesWorkspaceProps) {
  const appearance = useAppearance();
  const directGit = isDirectGitSurface(surface);
  const [stageFilter, setStageFilter] = useState<ReviewScopeFilter>("branch");
  const [wrap, setWrap] = useState(() => readReviewBoolean("wrap", false));
  const [wordDiffs, setWordDiffs] = useState(() => readReviewBoolean("word-diffs", true));
  const [hideWhitespace, setHideWhitespace] = useState(() => readReviewBoolean("hide-whitespace", false));
  const [gitSnapshot, setGitSnapshot] = useState<OctopusWorkerEnvironmentGitSnapshot | null>(null);
  const [gitSnapshotError, setGitSnapshotError] = useState("");
  const [gitSnapshotLoading, setGitSnapshotLoading] = useState(directGit);
  const [gitReloadTick, setGitReloadTick] = useState(0);
  const gitSnapshotRef = useRef<OctopusWorkerEnvironmentGitSnapshot | null>(null);
  const gitInspectInFlightRef = useRef(false);
  const operatorOriginIdRef = useRef(crypto.randomUUID());
  const effectiveSurface = useMemo<WorkerEnvironmentSurfaceProjection>(() => directGit && gitSnapshot
    ? {
        ...surface,
        data: {
          ...surface.data,
          base: gitSnapshot.base,
          branch: gitSnapshot.branch,
          branch_diffs: gitSnapshot.branchDiffs,
          branch_error: gitSnapshot.branchError,
          diffs: gitSnapshot.diffs,
          omitted_paths: gitSnapshot.omittedPaths,
          staged_files: gitSnapshot.stagedFiles,
          unstaged_files: gitSnapshot.unstagedFiles,
        },
      }
    : surface, [directGit, gitSnapshot, surface]);
  const effectiveRevision = gitSnapshot?.revision ?? projectionRevision;
  const worktreeChanges = useMemo(() => projectedChanges(effectiveSurface, hideWhitespace), [effectiveSurface, hideWhitespace]);
  const branchChanges = useMemo(() => {
    if (!directGit) return worktreeChanges.map((change) => ({ ...change, stage: "branch" as const }));
    return projectedChanges({
      ...effectiveSurface,
      data: { ...effectiveSurface.data, diffs: effectiveSurface.data.branch_diffs ?? [] },
    }, hideWhitespace);
  }, [directGit, effectiveSurface, hideWhitespace, worktreeChanges]);
  const changes = useMemo(() => stageFilter === "branch"
    ? branchChanges
    : worktreeChanges.filter((change) => change.stage === stageFilter), [branchChanges, stageFilter, worktreeChanges]);
  const assistantReviewComments = useMemo(
    () => resolveAssistantCodeCommentPaths(assistantCodeComments, surface.root, [...branchChanges, ...worktreeChanges]),
    [assistantCodeComments, branchChanges, surface.root, worktreeChanges],
  );
  const reviewScope = useMemo<OperatorDiffReviewScope>(() => ({
    projectionRevision: effectiveRevision,
    runId,
    surfaceId: surface.id,
    workerEnvironmentId,
  }), [effectiveRevision, runId, surface.id, workerEnvironmentId]);
  const review = useOperatorDiffReviewDraft(reviewScope);
  const [layout, setLayout] = useState<DiffLayout>(() => readLayout(surface.id));
  const [query, setQuery] = useState("");
  const [scopeMenuOpen, setScopeMenuOpen] = useState(false);
  const [optionsMenuOpen, setOptionsMenuOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);
  const [jumpMenuOpen, setJumpMenuOpen] = useState(false);
  const scopeMenuRef = useRef<HTMLDivElement>(null);
  const optionsMenuRef = useRef<HTMLDivElement>(null);

  function deleteReviewComment(commentId: string) {
    const index = review.comments.findIndex((comment) => comment.id === commentId);
    if (index < 0) return;
    const comment = review.comments[index];
    review.remove(commentId);
    appToast.info("Review comment deleted", 8, {
      dedupeKey: `operator-review-delete:${reviewScope.runId}:${commentId}`,
      action: {
        label: "Undo",
        onSelect: () => review.restore([{ comment, index }]),
      },
    });
  }

  function clearReviewComments() {
    if (!review.comments.length) return;
    const removed = review.comments.map((comment, index) => ({ comment, index }));
    review.clear();
    cancelPendingReview();
    setReviewExpanded(false);
    appToast.info(`${removed.length} review ${removed.length === 1 ? "comment" : "comments"} cleared`, 8, {
      dedupeKey: `operator-review-clear:${reviewScope.runId}:${reviewScope.workerEnvironmentId}:${reviewScope.surfaceId}`,
      action: {
        label: "Undo",
        onSelect: () => review.restore(removed),
      },
    });
  }
  const jumpMenuRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const fileSearchRef = useRef<HTMLInputElement>(null);
  const jumpSearchRef = useRef<HTMLInputElement>(null);
  const [reviewExpanded, setReviewExpanded] = useState(false);
  const [assistantReviewExpanded, setAssistantReviewExpanded] = useState(false);
  const [reviewSelection, setReviewSelection] = useState<OperatorDiffReviewSelection | null>(null);
  const [pendingReview, setPendingReview] = useState<OperatorDiffReviewSelection | null>(null);
  const [operatorChanges, setOperatorChanges] = useState<Record<string, OperatorChangeState>>({});
  const [pendingOperatorAction, setPendingOperatorAction] = useState<OctopusWorkerEnvironmentOperatorAction | null>(null);
  const [operatorConfirmation, setOperatorConfirmation] = useState<WorktreeOperatorConfirmation | null>(null);
  const [collapsedPaths, setCollapsedPaths] = useState<Set<string>>(() => initialCollapsedPaths(changes));
  const [viewedPaths, setViewedPaths] = useState<Set<string>>(() => readViewedPaths(surface.id, projectionRevision));
  const [activePath, setActivePath] = useState(changes[0]?.path ?? "");
  const [fileMenu, setFileMenu] = useState<ReviewFileMenu | null>(null);
  const [highlightReady, setHighlightReady] = useState(false);
  const knownPathsRef = useRef(new Set(changes.map((change) => change.path)));

  gitSnapshotRef.current = gitSnapshot;

  useEffect(() => {
    if (!directGit) {
      setGitSnapshot(null);
      setGitSnapshotError("");
      setGitSnapshotLoading(false);
      return;
    }
    if (!active) return;
    if (gitInspectInFlightRef.current) return;
    const transport = window.octopusTransport;
    if (!transport?.inspectWorkerEnvironmentGit) {
      setGitSnapshot(null);
      setGitSnapshotError("Git review requires the desktop client.");
      setGitSnapshotLoading(false);
      return;
    }
    let disposed = false;
    const hadSnapshot = Boolean(gitSnapshotRef.current);
    gitInspectInFlightRef.current = true;
    if (!hadSnapshot) setGitSnapshotLoading(true);
    setGitSnapshotError("");
    void transport.inspectWorkerEnvironmentGit({ runId, surfaceId: surface.id, workerEnvironmentId })
      .then((snapshot) => {
        if (disposed) return;
        if (
          snapshot.runId !== runId
          || snapshot.surfaceId !== surface.id
          || snapshot.workerEnvironmentId !== workerEnvironmentId
        ) {
          throw new Error("Git inspection returned state for a different Worker Environment surface.");
        }
        setGitSnapshot((current) => current?.revision === snapshot.revision ? current : snapshot);
      })
      .catch((error) => {
        if (disposed) return;
        if (!gitSnapshotRef.current) setGitSnapshot(null);
        setGitSnapshotError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        gitInspectInFlightRef.current = false;
        if (!disposed) setGitSnapshotLoading(false);
      });
    return () => { disposed = true; };
  }, [active, directGit, gitReloadTick, runId, surface.id, workerEnvironmentId]);

  useEffect(() => {
    if (!directGit || !active) return;
    const refreshOnFocus = () => setGitReloadTick((value) => value + 1);
    window.addEventListener("focus", refreshOnFocus);
    return () => window.removeEventListener("focus", refreshOnFocus);
  }, [active, directGit]);

  const preservesOperatorUndo = Object.values(operatorChanges).some((change) => change.content === "reverted");
  useEffect(() => {
    const boundedForPolling = gitSnapshot != null
      && gitSnapshot.diffs.length <= 200
      && !gitSnapshot.diffs.some((change) => change.preview_omitted);
    if (!active || !directGit || preservesOperatorUndo || !boundedForPolling) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible" && !gitInspectInFlightRef.current) {
        setGitReloadTick((value) => value + 1);
      }
    }, 4_000);
    return () => window.clearInterval(timer);
  }, [active, directGit, gitSnapshot, preservesOperatorUndo]);

  useEffect(() => {
    if (!active || !directGit) return;
    return subscribeWorkerEnvironmentLocalMutations((mutation) => {
      if (mutation.workerEnvironmentId !== workerEnvironmentId || mutation.originId === operatorOriginIdRef.current) return;
      setGitReloadTick((value) => value + 1);
    });
  }, [active, directGit, workerEnvironmentId]);

  useEffect(() => {
    setLayout(readLayout(surface.id));
    setStageFilter("branch");
    setQuery("");
    setScopeMenuOpen(false);
    setOptionsMenuOpen(false);
    setJumpMenuOpen(false);
    setReviewExpanded(false);
    setReviewSelection(null);
    setPendingReview(null);
    setOperatorChanges({});
    setPendingOperatorAction(null);
    setOperatorConfirmation(null);
    setCollapsedPaths(initialCollapsedPaths(changes));
    setActivePath(changes[0]?.path ?? "");
    setFileMenu(null);
    knownPathsRef.current = new Set(changes.map((change) => change.path));
  }, [surface.id]);

  useEffect(() => {
    setReviewSelection(null);
    setPendingReview(null);
    setOperatorChanges({});
    setPendingOperatorAction(null);
    setOperatorConfirmation(null);
    setViewedPaths(readViewedPaths(surface.id, effectiveRevision));
  }, [effectiveRevision]);

  useEffect(() => {
    persistLayout(surface.id, layout);
  }, [layout, surface.id]);

  useEffect(() => persistReviewBoolean("wrap", wrap), [wrap]);
  useEffect(() => persistReviewBoolean("word-diffs", wordDiffs), [wordDiffs]);
  useEffect(() => persistReviewBoolean("hide-whitespace", hideWhitespace), [hideWhitespace]);

  useEffect(() => {
    persistViewedPaths(surface.id, effectiveRevision, viewedPaths);
  }, [effectiveRevision, surface.id, viewedPaths]);

  useEffect(() => {
    if (!scopeMenuOpen && !optionsMenuOpen && !jumpMenuOpen && !fileMenu) return;
    const dismiss = (event: globalThis.PointerEvent) => {
      if (!scopeMenuRef.current?.contains(event.target as Node)) setScopeMenuOpen(false);
      if (!optionsMenuRef.current?.contains(event.target as Node)) setOptionsMenuOpen(false);
      if (!jumpMenuRef.current?.contains(event.target as Node)) setJumpMenuOpen(false);
      const target = event.target as Element | null;
      if (!target?.closest(".we-review-file-menu")) setFileMenu(null);
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        setScopeMenuOpen(false);
        setOptionsMenuOpen(false);
        setJumpMenuOpen(false);
        setFileMenu(null);
      }
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", escape);
    };
  }, [fileMenu, jumpMenuOpen, optionsMenuOpen, scopeMenuOpen]);

  const darkTheme = octopusDiffThemeName("dark", appearance.darkCodeTheme);
  const lightTheme = octopusDiffThemeName("light", appearance.lightCodeTheme);
  useEffect(() => {
    let active = true;
    setHighlightReady(false);
    void preloadHighlighter({
      langs: ["typescript", "tsx", "javascript", "jsx", "json", "markdown", "css", "html", "rust", "python", "shell"],
      themes: [darkTheme, lightTheme],
    }).then(() => {
      if (active) setHighlightReady(true);
    });
    return () => { active = false; };
  }, [darkTheme, lightTheme]);

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const effectiveChanges = changes.map((change) => ({ ...change, stage: operatorChanges[change.path]?.stage ?? change.stage }));
  const visibleChanges = effectiveChanges.filter((change) => (
    !normalizedQuery || `${change.path} ${change.previousPath}`.toLocaleLowerCase().includes(normalizedQuery)
  ));
  const visiblePathSignature = visibleChanges.map((change) => change.path).join("\n");
  useEffect(() => {
    if (!reviewSelection || visibleChanges.some((change) => change.path === reviewSelection.path)) return;
    setReviewSelection(null);
    setPendingReview(null);
  }, [reviewSelection?.path, visiblePathSignature]);
  useEffect(() => {
    if (visibleChanges.some((change) => change.path === activePath)) return;
    setActivePath(visibleChanges[0]?.path ?? "");
  }, [activePath, visiblePathSignature]);
  const total = changes.reduce((sum, change) => ({
    additions: sum.additions + change.stats.additions,
    deletions: sum.deletions + change.stats.deletions,
  }), { additions: 0, deletions: 0 });
  const stagedCount = worktreeChanges.filter((change) => change.stage === "staged").length;
  const unstagedCount = worktreeChanges.filter((change) => change.stage === "unstaged").length;
  const largeDiff = changes.length > 25 || total.additions + total.deletions > 2_000;
  const displayedChanges = largeDiff && activePath
    ? visibleChanges.filter((change) => change.path === activePath)
    : visibleChanges;
  const omittedPreviewCount = changes.filter((change) => change.previewOmitted).length;
  const changesSignature = changes.map((change) => `${change.path}:${change.stats.additions}:${change.stats.deletions}`).join("\n");

  useEffect(() => {
    const nextPaths = new Set(changes.map((change) => change.path));
    setCollapsedPaths((current) => {
      const next = new Set([...current].filter((path) => nextPaths.has(path)));
      if (largeDiff) {
        for (const path of nextPaths) {
          if (!knownPathsRef.current.has(path)) next.add(path);
        }
      }
      return next;
    });
    knownPathsRef.current = nextPaths;
  }, [changesSignature, largeDiff]);

  useEffect(() => {
    if (!largeDiff || !activePath) return;
    setCollapsedPaths((current) => {
      if (!current.has(activePath)) return current;
      const next = new Set(current);
      next.delete(activePath);
      return next;
    });
  }, [activePath, largeDiff]);
  const baseOptions = {
    diffIndicators: appearance.diffMarkerStyle === "symbols" ? "classic" as const : "bars" as const,
    diffStyle: layout,
    disableFileHeader: true,
    expansionLineCount: 8,
    hunkSeparators: "line-info" as const,
    lineDiffType: wordDiffs ? "word" as const : "none" as const,
    overflow: wrap ? "wrap" as const : "scroll" as const,
    theme: { dark: darkTheme, light: lightTheme },
    themeType: appearance.themeMode,
    unsafeCSS: changesUnsafeCss,
  };

  function togglePath(path: string) {
    setCollapsedPaths((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function setAllCollapsed(collapsed: boolean) {
    setCollapsedPaths(collapsed ? new Set(visibleChanges.map((change) => change.path)) : new Set());
  }

  const allVisibleCollapsed = visibleChanges.length > 0 && visibleChanges.every((change) => collapsedPaths.has(change.path));

  function toggleViewed(path: string) {
    setViewedPaths((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function selectReviewLines(change: ProjectedChange, range: SelectedLineRange | null) {
    if (!range) {
      setReviewSelection((current) => current?.path === change.path ? null : current);
      return;
    }
    setReviewSelection(operatorReviewSelection(change, range));
  }

  function beginReviewComment(change: ProjectedChange, range: SelectedLineRange) {
    const selection = operatorReviewSelection(change, range);
    setCollapsedPaths((current) => {
      if (!current.has(change.path)) return current;
      const next = new Set(current);
      next.delete(change.path);
      return next;
    });
    setReviewSelection(selection);
    setPendingReview(selection);
  }

  function cancelPendingReview() {
    setPendingReview(null);
    setReviewSelection(null);
  }

  function createReviewComment(body: string) {
    if (!pendingReview) return;
    review.create(pendingReview, body);
    setPendingReview(null);
    setReviewSelection(null);
  }

  async function performOperatorAction(action: OctopusWorkerEnvironmentOperatorAction, paths: string[]) {
    const transport = window.octopusTransport;
    if (!transport?.performWorkerEnvironmentOperatorAction) {
      appToast.danger("Worker Environment operator actions require the desktop client.");
      return;
    }
    setPendingOperatorAction(action);
    try {
      const outcome = await transport.performWorkerEnvironmentOperatorAction({
        action,
        paths,
        projectionRevision: effectiveRevision,
        runId,
        surfaceId: surface.id,
        workerEnvironmentId,
      });
      if (
        !outcome.outsideOctopusEvidence
        || outcome.action !== action
        || outcome.projectionRevision !== effectiveRevision
        || outcome.runId !== runId
        || outcome.surfaceId !== surface.id
        || outcome.workerEnvironmentId !== workerEnvironmentId
        || !samePathSet(outcome.paths, paths)
      ) {
        throw new Error("Operator action returned an invalid boundary receipt");
      }
      setOperatorChanges((current) => {
        const next = { ...current };
        for (const filePath of paths) {
          const previous = next[filePath] ?? {};
          next[filePath] = action === "stage"
            ? { ...previous, stage: "staged" }
            : action === "unstage"
              ? { ...previous, stage: "unstaged" }
              : { ...previous, content: action === "revert" ? "reverted" : "applied", stage: "unstaged" };
        }
        return next;
      });
      dispatchWorkerEnvironmentLocalMutation({
        operation: `git.${action}`,
        originId: operatorOriginIdRef.current,
        paths,
        surfaceId: surface.id,
        workerEnvironmentId,
      });
      appToast.success(action === "revert"
        ? `${paths.length} ${paths.length === 1 ? "change" : "changes"} reverted outside Octopus evidence.`
        : action === "apply"
          ? `${paths.length} ${paths.length === 1 ? "change" : "changes"} applied outside Octopus evidence.`
          : action === "stage"
            ? `${paths.length} ${paths.length === 1 ? "file" : "files"} staged outside Octopus evidence.`
            : `${paths.length} ${paths.length === 1 ? "file" : "files"} unstaged outside Octopus evidence.`);
      if (directGit && (action === "stage" || action === "unstage")) {
        setGitReloadTick((value) => value + 1);
      }
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingOperatorAction(null);
    }
  }

  function requestContentAction(action: "apply" | "revert", paths: string[]) {
    if (pendingOperatorAction || paths.length === 0) return;
    setOperatorConfirmation({ action, paths });
  }

  const mutableReview = stageFilter !== "branch";
  const revertedVisiblePaths = mutableReview ? visibleChanges.filter((change) => operatorChanges[change.path]?.content === "reverted").map((change) => change.path) : [];
  const appliedVisiblePaths = mutableReview ? visibleChanges.filter((change) => !change.binary && operatorChanges[change.path]?.content !== "reverted").map((change) => change.path) : [];
  const stageableVisiblePaths = mutableReview ? visibleChanges.filter((change) => change.stage === "unstaged" && operatorChanges[change.path]?.content !== "reverted").map((change) => change.path) : [];
  const unstageableVisiblePaths = mutableReview ? visibleChanges.filter((change) => change.stage === "staged" && operatorChanges[change.path]?.content !== "reverted").map((change) => change.path) : [];
  const branch = stringValue(effectiveSurface.data.branch ?? effectiveSurface.data.head ?? effectiveSurface.data.worktree_branch);
  const base = stringValue(effectiveSurface.data.base ?? effectiveSurface.data.base_branch ?? effectiveSurface.data.baseBranch);
  const branchError = stringValue(effectiveSurface.data.branch_error ?? effectiveSurface.data.branchError);
  const omittedPaths = objectList(effectiveSurface.data.omitted_paths ?? effectiveSurface.data.omittedPaths).flatMap((entry) => {
    const path = stringValue(entry.path);
    return path ? [path] : [];
  });
  const scopeLabel = stageFilter === "branch" ? "Branch" : stageFilter === "staged" ? "Staged" : "Unstaged";

  function revealChange(path: string) {
    setActivePath(path);
    setCollapsedPaths((current) => {
      if (!current.has(path)) return current;
      const next = new Set(current);
      next.delete(path);
      return next;
    });
    window.requestAnimationFrame(() => {
      scrollRef.current
        ?.querySelector(`[data-change-path="${CSS.escape(path)}"]`)
        ?.scrollIntoView({ behavior: "smooth", block: "start" });
    });
  }

  function revealAssistantComment(comment: ProjectedAssistantCodeComment) {
    if (!branchChanges.some((change) => change.path === comment.path)) {
      if (fileSurface && onOpenFile) onOpenFile(comment.path, true, fileSurface);
      else appToast.warning(`The projected diff does not contain ${comment.path}.`);
      return;
    }
    setStageFilter("branch");
    setQuery("");
    revealChange(comment.path);
  }

  useEffect(() => {
    const node = scrollRef.current;
    if (!node) return;
    let frame = 0;
    const update = () => {
      frame = 0;
      const top = node.getBoundingClientRect().top + 48;
      const sections = [...node.querySelectorAll<HTMLElement>("[data-change-path]")];
      const current = sections.find((section) => section.getBoundingClientRect().bottom > top) ?? sections.at(-1);
      const path = current?.dataset.changePath;
      if (path) setActivePath((value) => value === path ? value : path);
    };
    const onScroll = () => {
      if (!frame) frame = window.requestAnimationFrame(update);
    };
    node.addEventListener("scroll", onScroll, { passive: true });
    update();
    return () => {
      node.removeEventListener("scroll", onScroll);
      if (frame) window.cancelAnimationFrame(frame);
    };
  }, [visiblePathSignature]);

  async function copyChangePath(path: string) {
    try {
      if (!await copyText(path)) throw new Error("Clipboard access is unavailable");
      appToast.success("Copied file path");
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    }
    setFileMenu(null);
  }

  function openChange(path: string) {
    if (!fileSurface || !onOpenFile) return;
    onOpenFile(path, true, fileSurface);
    setFileMenu(null);
  }

  async function revealChangeInFileManager(path: string) {
    if (!fileSurface) return;
    const transport = window.octopusTransport;
    if (!transport?.performWorkerEnvironmentFileSystemAction) {
      appToast.danger("File reveal requires the desktop client.");
      return;
    }
    try {
      const outcome = await transport.performWorkerEnvironmentFileSystemAction({
        action: "reveal",
        path,
        surfaceId: fileSurface.id,
        workerEnvironmentId,
      });
      if (outcome.path !== path || outcome.surfaceId !== fileSurface.id || outcome.workerEnvironmentId !== workerEnvironmentId) {
        throw new Error("File reveal returned state for a different Worker Environment surface.");
      }
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    }
    setFileMenu(null);
  }

  function openFileMenu(event: ReactMouseEvent, path: string) {
    event.preventDefault();
    setActivePath(path);
    setFileMenu({ path, x: event.clientX, y: event.clientY });
  }

  const fileMenuChange = fileMenu ? effectiveChanges.find((change) => change.path === fileMenu.path) ?? null : null;

  return (
    <div className="we-review">
      <header className={`we-review-toolbar${stageFilter === "branch" && (branch || base) ? " is-branch" : ""}`}>
        <div className="we-review-scope" ref={scopeMenuRef}>
          <button
            aria-expanded={scopeMenuOpen}
            aria-haspopup="menu"
            className="we-review-scope-button"
            onClick={() => {
              setOptionsMenuOpen(false);
              setJumpMenuOpen(false);
              setScopeMenuOpen((open) => !open);
            }}
            type="button"
          >
            <span>{scopeLabel}</span>
            <Glyph name="chevronDown" size={12} />
          </button>
          {scopeMenuOpen ? (
            <OcMenuSurface aria-label="Review scope" className="we-review-scope-menu" role="menu">
              <OcMenuItem label={`Branch · ${branchChanges.length}`} onClick={() => { setStageFilter("branch"); setScopeMenuOpen(false); }} role="menuitemradio" selected={stageFilter === "branch"} />
              <OcMenuItem disabled={unstagedCount === 0} label={`Unstaged · ${unstagedCount}`} onClick={() => { setStageFilter("unstaged"); setScopeMenuOpen(false); }} role="menuitemradio" selected={stageFilter === "unstaged"} />
              <OcMenuItem disabled={stagedCount === 0} label={`Staged · ${stagedCount}`} onClick={() => { setStageFilter("staged"); setScopeMenuOpen(false); }} role="menuitemradio" selected={stageFilter === "staged"} />
            </OcMenuSurface>
          ) : null}
        </div>
        <div className="we-review-toolbar-actions">
          {total.additions || total.deletions ? (
            <span className="we-review-total"><b>+{total.additions}</b><i>−{total.deletions}</i></span>
          ) : null}
          <div className="we-review-options" ref={optionsMenuRef}>
            <button
              aria-expanded={optionsMenuOpen}
              aria-haspopup="menu"
              aria-label="Review options"
              onClick={() => {
                setScopeMenuOpen(false);
                setJumpMenuOpen(false);
                setOptionsMenuOpen((open) => !open);
              }}
              type="button"
            ><Glyph name="more" size={14} /></button>
            {optionsMenuOpen ? (
              <OcMenuSurface aria-label="Review options" className="we-review-options-menu" role="menu">
                {directGit ? <OcMenuItem label="Refresh" leadingIcon="refresh" onClick={() => { setGitReloadTick((value) => value + 1); setOptionsMenuOpen(false); }} /> : null}
                <OcMenuItem label={wrap ? "Disable word wrap" : "Enable word wrap"} leadingIcon="viewAll" onClick={() => { setWrap((value) => !value); setOptionsMenuOpen(false); }} />
                <OcMenuItem label={allVisibleCollapsed ? "Expand all diffs" : "Collapse all diffs"} leadingIcon="expand" onClick={() => { setAllCollapsed(!allVisibleCollapsed); setOptionsMenuOpen(false); }} />
                <OcMenuItem label={wordDiffs ? "Disable word diffs" : "Enable word diffs"} leadingIcon="changes" onClick={() => { setWordDiffs((value) => !value); setOptionsMenuOpen(false); }} />
                <OcMenuItem label={hideWhitespace ? "Show white space" : "Hide white space"} leadingIcon="code" onClick={() => { setHideWhitespace((value) => !value); setOptionsMenuOpen(false); }} />
                {stageableVisiblePaths.length ? <OcMenuItem label={`Stage ${stageableVisiblePaths.length === visibleChanges.length ? "all files" : `${stageableVisiblePaths.length} files`}`} leadingIcon="check" onClick={() => { void performOperatorAction("stage", stageableVisiblePaths); setOptionsMenuOpen(false); }} /> : null}
                {!stageableVisiblePaths.length && unstageableVisiblePaths.length ? <OcMenuItem label="Unstage all files" leadingIcon="arrowLeft" onClick={() => { void performOperatorAction("unstage", unstageableVisiblePaths); setOptionsMenuOpen(false); }} /> : null}
                {revertedVisiblePaths.length ? <OcMenuItem label={`Apply ${revertedVisiblePaths.length === visibleChanges.length ? "all changes" : `${revertedVisiblePaths.length} changes`}`} leadingIcon="changes" onClick={() => { requestContentAction("apply", revertedVisiblePaths); setOptionsMenuOpen(false); }} /> : null}
                {appliedVisiblePaths.length ? <OcMenuItem label={`Revert ${appliedVisiblePaths.length === visibleChanges.length ? "all changes" : `${appliedVisiblePaths.length} changes`}`} leadingIcon="retry" onClick={() => { requestContentAction("revert", appliedVisiblePaths); setOptionsMenuOpen(false); }} /> : null}
                {omittedPaths.length ? <OcMenuMessage>{omittedPaths.length} nested worktree {omittedPaths.length === 1 ? "entry is" : "entries are"} reviewed separately.</OcMenuMessage> : null}
                {omittedPreviewCount ? <OcMenuMessage>{omittedPreviewCount} oversized diff {omittedPreviewCount === 1 ? "preview is" : "previews are"} omitted.</OcMenuMessage> : null}
              </OcMenuSurface>
            ) : null}
          </div>
          <div className="we-review-jump" ref={jumpMenuRef}>
            <button aria-expanded={jumpMenuOpen} aria-haspopup="dialog" aria-label="Jump to file" onClick={() => {
              setOptionsMenuOpen(false);
              setScopeMenuOpen(false);
              setJumpMenuOpen((open) => !open);
              window.requestAnimationFrame(() => jumpSearchRef.current?.focus());
            }} type="button"><Glyph name="search" size={14} /></button>
            {jumpMenuOpen ? (
              <div className="we-review-jump-popover">
                <OctopusFilterField autoFocus inputId="review-jump-file" inputRef={jumpSearchRef} label="Jump to file" onQueryChange={setQuery} placeholder="Search files" searchQuery={query} />
                <div className="we-review-jump-results">
                  {visibleChanges.map((change) => (
                    <button key={change.path} onClick={() => { revealChange(change.path); setJumpMenuOpen(false); }} type="button">
                      <span className={`we-change-file-status is-${change.status}`}>{changeStatusGlyph(change.status)}</span>
                      <span><strong>{basename(change.path)}</strong><small>{directoryName(change.path)}</small></span>
                      <span className="we-review-file-stats"><b>+{change.stats.additions}</b><i>−{change.stats.deletions}</i></span>
                    </button>
                  ))}
                  {!visibleChanges.length ? <span>No matching files</span> : null}
                </div>
              </div>
            ) : null}
          </div>
          {directGit ? <button aria-label="Refresh changes" disabled={gitSnapshotLoading} onClick={() => setGitReloadTick((value) => value + 1)} type="button"><Glyph className={gitSnapshotLoading ? "is-spinning" : ""} name="refresh" size={14} /></button> : null}
          <button aria-label={wrap ? "Disable word wrap" : "Enable word wrap"} aria-pressed={wrap} className={wrap ? "is-active" : ""} onClick={() => setWrap((value) => !value)} type="button"><Glyph name="viewAll" size={14} /></button>
          <button aria-label={allVisibleCollapsed ? "Expand all diffs" : "Collapse all diffs"} onClick={() => setAllCollapsed(!allVisibleCollapsed)} type="button"><Glyph className={allVisibleCollapsed ? "" : "-rotate-90"} name="chevronDown" size={14} /></button>
          <button aria-label={layout === "unified" ? "Switch to split diff" : "Switch to unified diff"} onClick={() => setLayout(layout === "unified" ? "split" : "unified")} type="button">
            {layout === "unified" ? <UnifiedDiffGlyph /> : <SplitDiffGlyph />}
          </button>
          <button
            aria-label={filesOpen ? "Hide file list" : "Show file list"}
            aria-pressed={filesOpen}
            className={filesOpen ? "is-active" : ""}
            onClick={() => setFilesOpen((open) => !open)}
            type="button"
          ><Glyph name={filesOpen ? "folderOpen" : "folder"} size={14} /></button>
        </div>
        {stageFilter === "branch" && (branch || base) ? (
          <div className="we-review-compare">
            <span className="flex h-token-button-composer min-w-0 flex-[0_2_auto] items-center px-1.5 text-base font-normal select-text">
              <span className="min-w-0 truncate">{branch || "Working tree"}</span>
            </span>
            {base ? (
              <>
                <span aria-hidden="true" className="flex-shrink-0 text-sm text-token-description-foreground">→</span>
                <span className="flex max-w-full min-w-0 items-center gap-1 overflow-hidden px-1.5 text-base font-normal">
                  <span className="min-w-0 truncate select-text">{base}</span>
                </span>
              </>
            ) : null}
          </div>
        ) : null}
      </header>

      <AssistantCodeReviewBar comments={assistantReviewComments} expanded={assistantReviewExpanded} onExpandedChange={setAssistantReviewExpanded} onReveal={revealAssistantComment} />

      <OperatorDiffReviewBar
        comments={review.comments}
        expanded={reviewExpanded}
        onBeginComment={() => {
          if (!reviewSelection) return;
          const change = changes.find((candidate) => candidate.path === reviewSelection.path);
          if (change) beginReviewComment(change, reviewSelection.range);
        }}
        onClear={clearReviewComments}
        onDelete={deleteReviewComment}
        onExpandedChange={setReviewExpanded}
        onSend={() => dispatchOperatorDiffReview(reviewScope, review.comments)}
        onUpdate={review.update}
        scope={reviewScope}
        selected={reviewSelection}
        storageError={review.storageError}
        storageStatus={review.storageStatus}
      />

      {largeDiff && changes.length ? (
        <div className="we-review-large-diff" role="status">
          <Glyph name="warning" size={14} />
          <span>This diff is large, showing one file at a time</span>
        </div>
      ) : null}

      <div className="we-review-body">
      <div className="we-changes-scroll we-review-main" data-large-diff={largeDiff || undefined} ref={scrollRef}>
        {gitSnapshotLoading ? (
          <div className="we-review-empty" role="status">
            <span className="we-review-empty-icon"><Glyph name="refresh" size={26} /></span>
            <strong>Reading worktree changes</strong>
            <span>The client is inspecting the exact Git root projected by this Worker Environment.</span>
          </div>
        ) : gitSnapshotError && !gitSnapshot ? (
          <div className="we-review-empty" role="alert">
            <span className="we-review-empty-icon"><Glyph name="warning" size={26} /></span>
            <strong>Changes unavailable</strong>
            <span>{gitSnapshotError}</span>
            <button onClick={() => setGitReloadTick((value) => value + 1)} type="button">Try again</button>
          </div>
        ) : changes.length === 0 ? (
          <div className="we-review-empty">
            <span className={`we-review-empty-icon${emptyIllustration ? " is-illustration" : ""}`}>{emptyIllustration ?? <Glyph name="file" size={26} />}</span>
            <strong>{stageFilter === "branch" && directGit && branchError ? "Branch review unavailable" : stageFilter === "branch" && directGit && !base ? "Comparison branch unavailable" : "No file changes yet"}</strong>
            <span>{stageFilter === "branch" && directGit && branchError
              ? branchError
              : stageFilter === "branch" && directGit && !base
              ? "Project a Git remote default branch or upstream before opening Branch review."
              : stageFilter === "branch"
                ? "Changes on this branch will appear here."
                : `${scopeLabel} changes in this project will appear here.`}</span>
          </div>
        ) : null}
        {displayedChanges.map((change) => {
          const collapsed = collapsedPaths.has(change.path);
          const operatorState = operatorChanges[change.path];
          const viewed = viewedPaths.has(change.path);
          const selectedLines = reviewSelection?.path === change.path ? reviewSelection.range : null;
          const lineAnnotations = buildOperatorDiffReviewAnnotations(review.comments, pendingReview, change.path, effectiveRevision, assistantReviewComments);
          return (
            <section
              className={`we-change-file${collapsed ? " is-collapsed" : ""}${viewed ? " is-viewed" : ""}${operatorState?.content === "reverted" ? " is-operator-reverted" : ""}`}
              data-change-path={change.path}
              key={change.path}
              onContextMenu={(event) => openFileMenu(event, change.path)}
            >
              <div className="we-change-file-header">
                <button
                  aria-label={viewed ? `Mark ${change.path} as unviewed` : `Mark ${change.path} as viewed`}
                  aria-pressed={viewed}
                  className="we-change-file-viewed"
                  onClick={() => toggleViewed(change.path)}
                  title={viewed ? "Mark as unviewed" : "Mark as viewed"}
                  type="button"
                >
                  {viewed ? <Glyph name="check" size={12} /> : null}
                </button>
                <button aria-expanded={!collapsed} className="we-change-file-toggle" onClick={() => togglePath(change.path)} type="button">
                  <Glyph className={collapsed ? "-rotate-90" : ""} name="chevronDown" size={15} />
                  <span className={`we-change-file-status is-${change.status}`}>{changeStatusGlyph(change.status)}</span>
                  <span className="we-change-file-name" title={change.previousPath ? `${change.previousPath} → ${change.path}` : change.path}>
                    <strong>{basename(change.path)}</strong>
                    <small>{directoryName(change.path)}</small>
                    {change.previousPath ? <em>{change.previousPath} →</em> : null}
                  </span>
                  <span className="we-change-file-stats"><b>+{change.stats.additions}</b><i>−{change.stats.deletions}</i></span>
                </button>
                <span className="we-change-file-actions">
                  <button aria-label={`File actions for ${change.path}`} onClick={(event) => openFileMenu(event, change.path)} type="button"><Glyph name="more" size={13} /></button>
                </span>
              </div>
              {!collapsed ? (
                <div className="we-change-file-diff" style={{ "--we-change-layout": layout } as CSSProperties}>
                  {!change.fileDiff ? (
                    <div className="we-change-unavailable"><Glyph name="file" size={16} /><span>{change.previewOmitted ? "Diff preview omitted by the bounded Review memory budget" : change.binary ? "Binary diff preview unavailable" : "Text diff could not be rendered"}</span></div>
                  ) : highlightReady ? (
                    <FileDiff<OperatorDiffReviewAnnotationMetadata>
                      disableWorkerPool
                      fileDiff={change.fileDiff}
                      lineAnnotations={lineAnnotations}
                      options={{
                        ...baseOptions,
                        controlledSelection: true,
                        enableGutterUtility: true,
                        enableLineSelection: true,
                        lineHoverHighlight: "both",
                        onGutterUtilityClick: (range) => beginReviewComment(change, range),
                        onLineSelected: (range) => selectReviewLines(change, range),
                      }}
                      renderAnnotation={(annotation) => (
                        <OperatorDiffReviewAnnotation
                          metadata={annotation.metadata}
                          onCancelPending={cancelPendingReview}
                          onCreate={createReviewComment}
                          onDelete={deleteReviewComment}
                          onUpdate={review.update}
                          projectionRevision={effectiveRevision}
                        />
                      )}
                      selectedLines={selectedLines}
                    />
                  ) : <OcLoadingRows className="we-change-loading" label="Loading diff…" />}
                </div>
              ) : null}
            </section>
          );
        })}
        {changes.length > 0 && !visibleChanges.length ? (
          <div className="we-changes-no-match"><Glyph name="search" size={18} /><strong>No matching changes</strong><span>Adjust the scope or file filter.</span></div>
        ) : null}
      </div>

      {filesOpen ? (
        <aside className="we-review-files">
          <OctopusFilterField inputId="review-file-tree-search" inputRef={fileSearchRef} label="Filter files" onQueryChange={setQuery} placeholder="Filter files…" searchQuery={query} />
          <div className="we-review-octopus-tree">
            {visibleChanges.length ? (
              <OctopusFileTree
                initialExpandedPaths={octopusTreeAncestors(visibleChanges.map((change) => change.path))}
                initialSelectedPaths={activePath ? [activePath] : []}
                onOpenPath={(path) => openChange(path)}
                onSelectPath={revealChange}
                paths={visibleChanges.map((change) => change.path)}
                resetKey={`${effectiveRevision}:${stageFilter}:${query}`}
              />
            ) : <span className="we-review-files-empty">{changes.length ? "No matching files" : "No file changes yet"}</span>}
          </div>
        </aside>
      ) : null}
      </div>
      {operatorConfirmation ? (
        <WorktreeOperatorConfirm
          confirmation={operatorConfirmation}
          onCancel={() => setOperatorConfirmation(null)}
          onConfirm={() => {
            const confirmation = operatorConfirmation;
            void performOperatorAction(confirmation.action, confirmation.paths).then(() => setOperatorConfirmation(null));
          }}
          pending={Boolean(pendingOperatorAction)}
        />
      ) : null}
      {fileMenu ? (
        <OcMenuSurface
          aria-label={`File actions for ${fileMenu.path}`}
          className="we-review-file-menu"
          role="menu"
          style={{ left: Math.min(fileMenu.x, window.innerWidth - 224), top: Math.min(fileMenu.y, window.innerHeight - 260) }}
        >
          <OcMenuItem label="Copy path" leadingIcon="copy" onClick={() => void copyChangePath(fileMenu.path)} />
          {fileSurface && onOpenFile ? <OcMenuItem label="Open file" leadingIcon="file" onClick={() => openChange(fileMenu.path)} /> : null}
          {fileSurface ? <OcMenuItem label="Reveal in Finder" leadingIcon="folderOpen" onClick={() => void revealChangeInFileManager(fileMenu.path)} /> : null}
          {mutableReview && fileMenuChange ? <OcMenuItem disabled={Boolean(pendingOperatorAction) || operatorChanges[fileMenu.path]?.content === "reverted"} label={fileMenuChange.stage === "staged" ? "Unstage file" : "Stage file"} leadingIcon={fileMenuChange.stage === "staged" ? "arrowLeft" : "check"} onClick={() => { void performOperatorAction(fileMenuChange.stage === "staged" ? "unstage" : "stage", [fileMenu.path]); setFileMenu(null); }} /> : null}
          {mutableReview && fileMenuChange && !fileMenuChange.binary ? <OcMenuItem disabled={Boolean(pendingOperatorAction)} label={operatorChanges[fileMenu.path]?.content === "reverted" ? "Apply changes" : "Revert changes"} leadingIcon={operatorChanges[fileMenu.path]?.content === "reverted" ? "changes" : "retry"} onClick={() => { requestContentAction(operatorChanges[fileMenu.path]?.content === "reverted" ? "apply" : "revert", [fileMenu.path]); setFileMenu(null); }} /> : null}
        </OcMenuSurface>
      ) : null}
    </div>
  );
}

function projectedChanges(surface: WorkerEnvironmentSurfaceProjection, hideWhitespace = false): ProjectedChange[] {
  const stagedFiles = new Set(stringList(surface.data.staged_files ?? surface.data.stagedFiles));
  const unstagedFiles = new Set(stringList(surface.data.unstaged_files ?? surface.data.unstagedFiles));
  const raw = objectList(surface.data.diffs ?? surface.data.changes);
  return raw.flatMap((entry): ProjectedChange[] => {
    const path = stringValue(entry.path ?? entry.file ?? entry.name);
    const before = typeof entry.before === "string" ? entry.before : typeof entry.old === "string" ? entry.old : "";
    const after = typeof entry.after === "string" ? entry.after : typeof entry.new === "string" ? entry.new : "";
    if (!path || (before === after && !entry.binary)) return [];
    const previousPath = stringValue(entry.previous_path ?? entry.previousPath ?? entry.old_path ?? entry.oldPath);
    const binary = entry.binary === true;
    const previewOmitted = entry.preview_omitted === true || entry.previewOmitted === true;
    let fileDiff: FileDiffMetadata | null = null;
    if (!binary) {
      try {
        fileDiff = parseDiffFromFile(
          { name: previousPath || path, contents: before },
          { name: path, contents: after },
          { ignoreWhitespace: hideWhitespace },
        );
      } catch {
        fileDiff = null;
      }
    }
    const stats = fileDiff ? fileDiff.hunks.reduce((sum, hunk) => ({
      additions: sum.additions + hunk.additionLines,
      deletions: sum.deletions + hunk.deletionLines,
    }), { additions: 0, deletions: 0 }) : {
      additions: numericValue(entry.additions),
      deletions: numericValue(entry.deletions),
    };
    return [{
      after,
      binary,
      before,
      fileDiff,
      path,
      previousPath,
      previewOmitted,
      stage: projectedStage(entry, path, stagedFiles, unstagedFiles),
      status: projectedStatus(entry, before, after, previousPath),
      stats,
    }];
  });
}

function projectedStage(entry: Record<string, unknown>, path: string, stagedFiles: Set<string>, unstagedFiles: Set<string>): ChangeStage {
  const stage = stringValue(entry.stage ?? entry.index_state ?? entry.indexState).toLowerCase();
  if (stage === "branch" || stage === "commit") return "branch";
  if (entry.staged === true || stagedFiles.has(path) || stage === "staged" || stage === "index") return "staged";
  if (entry.staged === false || unstagedFiles.has(path) || stage === "unstaged" || stage === "working-tree") return "unstaged";
  return "unstaged";
}

function projectedStatus(entry: Record<string, unknown>, before: string, after: string, previousPath: string): ChangeStatus {
  const value = stringValue(entry.status ?? entry.state ?? entry.kind ?? entry.change_type ?? entry.changeType).toLowerCase();
  if (previousPath || ["renamed", "rename", "moved", "move", "r"].includes(value)) return "renamed";
  if (["added", "add", "new", "created", "a"].includes(value) || (!before && Boolean(after))) return "added";
  if (["deleted", "delete", "removed", "remove", "d"].includes(value) || (Boolean(before) && !after)) return "deleted";
  if (["modified", "modify", "updated", "update", "m"].includes(value) || before !== after) return "modified";
  return "unknown";
}

function changeStatusGlyph(status: ChangeStatus): string {
  if (status === "added") return "A";
  if (status === "deleted") return "D";
  if (status === "renamed") return "R";
  if (status === "modified") return "M";
  return "·";
}

function initialCollapsedPaths(changes: ProjectedChange[]): Set<string> {
  const total = changes.reduce((sum, change) => sum + change.stats.additions + change.stats.deletions, 0);
  return changes.length > 25 || total > 2_000 ? new Set(changes.map((change) => change.path)) : new Set();
}

function readLayout(surfaceId: string): DiffLayout {
  try {
    return localStorage.getItem(`octopus.we.changes.layout:${surfaceId}`) === "split" ? "split" : "unified";
  } catch {
    return "unified";
  }
}

function persistLayout(surfaceId: string, layout: DiffLayout) {
  try {
    localStorage.setItem(`octopus.we.changes.layout:${surfaceId}`, layout);
  } catch {}
}

function reviewPreferenceKey(name: string): string {
  return `octopus.we.review.${name}.v1`;
}

function readReviewBoolean(name: string, fallback: boolean): boolean {
  try {
    const value = localStorage.getItem(reviewPreferenceKey(name));
    return value == null ? fallback : value === "true";
  } catch {
    return fallback;
  }
}

function persistReviewBoolean(name: string, value: boolean) {
  try {
    localStorage.setItem(reviewPreferenceKey(name), String(value));
  } catch {}
}

function viewedPathsKey(surfaceId: string, revision: string): string {
  return `octopus.we.review.viewed.v1:${surfaceId}:${revision}`;
}

function readViewedPaths(surfaceId: string, revision: string): Set<string> {
  try {
    const value = JSON.parse(localStorage.getItem(viewedPathsKey(surfaceId, revision)) ?? "[]") as unknown;
    return new Set(Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string" && Boolean(entry)) : []);
  } catch {
    return new Set();
  }
}

function persistViewedPaths(surfaceId: string, revision: string, paths: Set<string>) {
  try {
    localStorage.setItem(viewedPathsKey(surfaceId, revision), JSON.stringify([...paths].sort()));
  } catch {}
}

function objectList(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((entry): entry is Record<string, unknown> => Boolean(entry) && typeof entry === "object" && !Array.isArray(entry))
    : [];
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string" && Boolean(entry.trim())).map((entry) => entry.trim()) : [];
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function samePathSet(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const expected = new Set(right);
  return expected.size === right.length && left.every((path) => expected.has(path));
}

function numericValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function isDirectGitSurface(surface: WorkerEnvironmentSurfaceProjection): boolean {
  return surface.mode === "direct" && surface.protocol.trim().toLowerCase() === "git.worktree";
}

function resolveAssistantCodeCommentPaths(
  comments: ProjectedAssistantCodeComment[],
  root: string,
  changes: ProjectedChange[],
): ProjectedAssistantCodeComment[] {
  const paths = [...new Set(changes.map((change) => normalizeDirectivePath(change.path)))];
  return comments.flatMap((comment) => {
    const relative = directivePathWithinRoot(comment.path, root);
    if (relative === null) return [];
    return [{ ...comment, path: paths.find((path) => path === relative) ?? relative }];
  });
}

function basename(path: string): string {
  return path.replaceAll("\\", "/").split("/").filter(Boolean).pop() ?? path;
}

function directoryName(path: string): string {
  const parts = path.replaceAll("\\", "/").split("/").filter(Boolean);
  return parts.length > 1 ? parts.slice(0, -1).join("/") : "";
}

function operatorReviewSelection(change: ProjectedChange, range: SelectedLineRange): OperatorDiffReviewSelection {
  const { excerpt, truncated } = projectedSelectionExcerpt(change, range);
  return {
    excerpt,
    excerptTruncated: truncated,
    path: change.path,
    range,
    stage: change.stage,
  };
}

function projectedSelectionExcerpt(change: ProjectedChange, range: SelectedLineRange): { excerpt: string; truncated: boolean } {
  const startSide = range.side ?? "additions";
  const endSide = range.endSide ?? startSide;
  if (startSide !== endSide) {
    const startText = projectedLine(change, startSide, range.start);
    const endText = projectedLine(change, endSide, range.end);
    const excerpt = [
      `${startSide === "deletions" ? "L" : "R"}${range.start}: ${startText}`,
      `${endSide === "deletions" ? "L" : "R"}${range.end}: ${endText}`,
    ].join("\n");
    return truncateReviewExcerpt(excerpt, false);
  }
  const lines = projectedLines(change, startSide);
  const first = Math.max(1, Math.min(range.start, range.end));
  const last = Math.min(lines.length, Math.max(range.start, range.end));
  const selected = lines.slice(first - 1, last);
  const lineTruncated = selected.length > MAX_REVIEW_EXCERPT_LINES;
  const excerpt = selected
    .slice(0, MAX_REVIEW_EXCERPT_LINES)
    .map((line, index) => `${startSide === "deletions" ? "L" : "R"}${first + index}: ${line}`)
    .join("\n");
  return truncateReviewExcerpt(excerpt, lineTruncated);
}

function projectedLines(change: ProjectedChange, side: "additions" | "deletions"): string[] {
  return (side === "deletions" ? change.before : change.after).split(/\r?\n/);
}

function projectedLine(change: ProjectedChange, side: "additions" | "deletions", line: number): string {
  return projectedLines(change, side)[Math.max(0, line - 1)] ?? "";
}

function truncateReviewExcerpt(excerpt: string, alreadyTruncated: boolean): { excerpt: string; truncated: boolean } {
  if (excerpt.length <= MAX_REVIEW_EXCERPT_CHARS) return { excerpt, truncated: alreadyTruncated };
  return {
    excerpt: excerpt.slice(0, MAX_REVIEW_EXCERPT_CHARS).trimEnd(),
    truncated: true,
  };
}

function UnifiedDiffGlyph() {
  return (
    <svg aria-hidden="true" fill="none" height="14" viewBox="0 0 16 16" width="14">
      <path d="M13.4746 4.53271C13.4744 3.79209 12.8744 3.19214 12.1338 3.19189H3.86621C3.12559 3.19214 2.52564 3.79209 2.52539 4.53271V11.4663C2.52539 12.2071 3.12544 12.8079 3.86621 12.8081H12.1338C12.8746 12.8079 13.4746 12.2071 13.4746 11.4663V4.53271ZM14.5254 11.4663C14.5254 12.787 13.4545 13.8577 12.1338 13.8579H3.86621C2.54554 13.8577 1.47461 12.787 1.47461 11.4663V4.53271C1.47486 3.2122 2.54569 2.14136 3.86621 2.14111H12.1338C13.4543 2.14136 14.5251 3.2122 14.5254 4.53271V11.4663Z" fill="currentColor" />
      <path d="M11.7613 4.53857C12.0927 4.53857 12.3613 4.8072 12.3613 5.13857L12.3613 6.95692C12.3613 7.28829 12.0927 7.55692 11.7613 7.55692L4.23965 7.55692C3.90828 7.55692 3.63965 7.28829 3.63965 6.95692L3.63965 5.13857C3.63965 4.8072 3.90828 4.53857 4.23965 4.53857L11.7613 4.53857Z" fill="#F84E63" fillOpacity="0.5" />
      <path d="M11.7613 8.43506C12.0927 8.43506 12.3613 8.70369 12.3613 9.03506L12.3613 10.8608C12.3613 11.1921 12.0927 11.4608 11.7613 11.4608L4.23965 11.4608C3.90828 11.4608 3.63965 11.1921 3.63965 10.8608L3.63965 9.03506C3.63965 8.70369 3.90828 8.43506 4.23965 8.43506L11.7613 8.43506Z" fill="#36D958" fillOpacity="0.5" />
    </svg>
  );
}

function SplitDiffGlyph() {
  return (
    <svg aria-hidden="true" fill="none" height="14" viewBox="0 0 16 16" width="14">
      <path d="M13.4746 4.53271C13.4744 3.79209 12.8744 3.19214 12.1338 3.19189H3.86621C3.12559 3.19214 2.52564 3.79209 2.52539 4.53271V11.4663C2.52539 12.2071 3.12544 12.8079 3.86621 12.8081H12.1338C12.8746 12.8079 13.4746 12.2071 13.4746 11.4663V4.53271ZM14.5254 11.4663C14.5254 12.787 13.4545 13.8577 12.1338 13.8579H3.86621C2.54554 13.8577 1.47461 12.787 1.47461 11.4663V4.53271C1.47486 3.2122 2.54569 2.14136 3.86621 2.14111H12.1338C13.4543 2.14136 14.5251 3.2122 14.5254 4.53271V11.4663Z" fill="currentColor" />
      <path d="M3.63916 5.13857C3.63916 4.8072 3.90779 4.53857 4.23916 4.53857H6.84237C7.17374 4.53857 7.44237 4.8072 7.44237 5.13857V10.8604C7.44237 11.1917 7.17374 11.4604 6.84237 11.4604H4.23916C3.90779 11.4604 3.63916 11.1917 3.63916 10.8604V5.13857Z" fill="#F84E63" fillOpacity="0.5" />
      <path d="M8.54834 5.13857C8.54834 4.8072 8.81697 4.53857 9.14834 4.53857H11.7608C12.0922 4.53857 12.3608 4.8072 12.3608 5.13857V10.8604C12.3608 11.1917 12.0922 11.4604 11.7608 11.4604H9.14834C8.81697 11.4604 8.54834 11.1917 8.54834 10.8604V5.13857Z" fill="#36D958" fillOpacity="0.5" />
    </svg>
  );
}
