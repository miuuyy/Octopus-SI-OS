import { useCallback, useEffect, useId, useMemo, useRef, useState, type RefObject } from "react";
import type { OctopusWorkerEnvironmentFileEntry } from "../../electron";
import { Glyph } from "../IconGlyphs";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import {
  ComposerSuggestionEmpty,
  ComposerSuggestionMenu,
  ComposerSuggestionRow,
  ComposerSuggestionSectionLabel,
  scoreSuggestionQuery,
} from "./ComposerSuggestionMenu";

export type ProjectedFileSource = {
  workerEnvironmentId: string;
  surfaceId: string;
  label: string;
};

export function ProjectedFilePicker({
  autoFocusSearch = true,
  externalQuery,
  ignoreOutsideRefs,
  onAttach,
  onClose,
  source,
}: {
  autoFocusSearch?: boolean;
  externalQuery?: string;
  ignoreOutsideRefs?: ReadonlyArray<RefObject<HTMLElement | null>>;
  onAttach: (file: File, path: string) => void;
  onClose: () => void;
  source: ProjectedFileSource;
}) {
  const listId = useId();
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<OctopusWorkerEnvironmentFileEntry[]>([]);
  const [localQuery, setLocalQuery] = useState(externalQuery ?? "");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [navSource, setNavSource] = useState<"keyboard" | "pointer">("pointer");
  const [loading, setLoading] = useState(true);
  const [attachingPath, setAttachingPath] = useState("");
  const [error, setError] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const inlineDescentStackRef = useRef<string[]>([]);

  const query = externalQuery !== undefined && autoFocusSearch === false && currentPath === "" ? externalQuery : localQuery;

  const filteredEntries = useMemo(() => {
    const needle = query.trim();
    if (!needle) return entries;
    return entries
      .map((entry, index) => ({
        entry,
        index,
        score: scoreSuggestionQuery(needle, [entry.name, entry.path]),
      }))
      .filter((item) => item.score >= 0)
      .sort((a, b) => b.score - a.score || a.index - b.index)
      .map((item) => item.entry);
  }, [entries, query]);

  useEffect(() => {
    inlineDescentStackRef.current = [];
    setCurrentPath("");
    setLocalQuery(externalQuery ?? "");
    setSelectedIndex(0);
  }, [source.surfaceId, source.workerEnvironmentId]);

  useEffect(() => {
    if (externalQuery === undefined) return;
    if (autoFocusSearch === false && currentPath !== "") {
      const base = inlineDescentStackRef.current[inlineDescentStackRef.current.length - 1] ?? "";
      if (externalQuery.startsWith(base)) {
        setLocalQuery(externalQuery.slice(base.length));
        setSelectedIndex(0);
        return;
      }
      inlineDescentStackRef.current = [];
      setCurrentPath("");
    }
    setLocalQuery(externalQuery);
    setSelectedIndex(0);
  }, [autoFocusSearch, currentPath, externalQuery]);

  useEffect(() => {
    let cancelled = false;
    const transport = window.octopusTransport;
    setLoading(true);
    setError("");
    if (!transport?.listWorkerEnvironmentFiles) {
      setEntries([]);
      setLoading(false);
      setError("Projected file attachment requires the Octopus desktop interface.");
      return () => { cancelled = true; };
    }
    void transport.listWorkerEnvironmentFiles({
      workerEnvironmentId: source.workerEnvironmentId,
      surfaceId: source.surfaceId,
      path: currentPath,
    }).then((directory) => {
      if (cancelled) return;
      assertProjectedFileResponse(directory, source, currentPath);
      setEntries(directory.entries);
      setSelectedIndex(0);
    }).catch((failure) => {
      if (cancelled) return;
      setEntries([]);
      setError(formatFailure(failure));
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [currentPath, source.surfaceId, source.workerEnvironmentId]);

  useEffect(() => {
    if (autoFocusSearch) searchRef.current?.focus();
  }, [autoFocusSearch]);

  useEffect(() => {
    setSelectedIndex((current) => Math.min(current, Math.max(0, filteredEntries.length - 1)));
  }, [filteredEntries.length]);

  const parentPath = currentPath.includes("/") ? currentPath.slice(0, currentPath.lastIndexOf("/")) : "";

  const stepBackToParent = useCallback(() => {
    if (!currentPath || attachingPath) return;
    const nextParent = parentPath;
    if (!autoFocusSearch && externalQuery !== undefined) {
      inlineDescentStackRef.current.pop();
      const parentBase = nextParent
        ? (inlineDescentStackRef.current[inlineDescentStackRef.current.length - 1] ?? "")
        : "";
      setLocalQuery(externalQuery.startsWith(parentBase) ? externalQuery.slice(parentBase.length) : externalQuery);
    } else {
      setLocalQuery("");
    }
    setCurrentPath(nextParent);
    setSelectedIndex(0);
  }, [attachingPath, autoFocusSearch, currentPath, externalQuery, parentPath]);

  const choose = useCallback((entry: OctopusWorkerEnvironmentFileEntry) => {
    if (entry.kind === "directory") {
      if (!autoFocusSearch && externalQuery !== undefined) {
        inlineDescentStackRef.current.push(externalQuery);
      }
      setCurrentPath(entry.path);
      setLocalQuery("");
      setSelectedIndex(0);
      return;
    }
    if (entry.kind !== "file" || attachingPath) return;
    const transport = window.octopusTransport;
    if (!transport?.readWorkerEnvironmentFileAttachment) {
      setError("Projected file attachment requires the Octopus desktop interface.");
      return;
    }
    setAttachingPath(entry.path);
    setError("");
    void transport.readWorkerEnvironmentFileAttachment({
      workerEnvironmentId: source.workerEnvironmentId,
      surfaceId: source.surfaceId,
      path: entry.path,
    }).then((attachment) => {
      assertProjectedFileResponse(attachment, source, entry.path);
      const modifiedAt = Date.parse(attachment.modifiedAt);
      const file = new File([new Uint8Array(attachment.bytes)], attachment.path, {
        type: attachment.mimeType,
        lastModified: Number.isFinite(modifiedAt) ? modifiedAt : Date.now(),
      });
      setAttachingPath("");
      onAttach(file, attachment.path);
      onClose();
    }).catch((failure) => {
      setAttachingPath("");
      setError(formatFailure(failure));
    });
  }, [attachingPath, autoFocusSearch, externalQuery, onAttach, onClose, source]);

  useEffect(() => {
    if (autoFocusSearch) return;
    const onGlobalKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }
      if (
        currentPath &&
        !attachingPath &&
        ((event.key === "Backspace" && localQuery === "") ||
          (event.key === "ArrowLeft" && event.altKey))
      ) {
        event.preventDefault();
        event.stopPropagation();
        stepBackToParent();
        return;
      }
      if ((event.key === "ArrowDown" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "n")) && filteredEntries.length) {
        event.preventDefault();
        event.stopPropagation();
        setNavSource("keyboard");
        setSelectedIndex((current) => (current + 1) % filteredEntries.length);
        return;
      }
      if ((event.key === "ArrowUp" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "p")) && filteredEntries.length) {
        event.preventDefault();
        event.stopPropagation();
        setNavSource("keyboard");
        setSelectedIndex((current) => (current - 1 + filteredEntries.length) % filteredEntries.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        const entry = filteredEntries[selectedIndex];
        if (!entry) return;
        event.preventDefault();
        event.stopPropagation();
        choose(entry);
      }
    };
    window.addEventListener("keydown", onGlobalKeyDown, true);
    return () => window.removeEventListener("keydown", onGlobalKeyDown, true);
  }, [attachingPath, autoFocusSearch, choose, currentPath, filteredEntries, localQuery, onClose, selectedIndex, stepBackToParent]);

  const header = autoFocusSearch ? (
    <div className="flex min-w-0 items-center gap-1 px-row-x">
      {currentPath ? (
        <button
          aria-label="Back to parent folder"
          className="cursor-interaction inline-flex size-6 shrink-0 items-center justify-center rounded-lg text-tertiary hover:bg-[var(--color-background-secondary-ghost-hover,var(--color-token-list-hover-background))] hover:text-default disabled:opacity-40"
          disabled={Boolean(attachingPath)}
          onClick={stepBackToParent}
          onMouseDown={(event) => event.preventDefault()}
          type="button"
        >
          <Glyph name="arrowLeft" size={14} />
        </button>
      ) : null}
      <input
        aria-activedescendant={filteredEntries[selectedIndex] ? `${listId}-opt-${selectedIndex}` : undefined}
        aria-controls={listId}
        aria-label="Search projected files"
        className="w-full min-w-0 flex-1 bg-transparent px-0 py-row-y text-sm text-default placeholder:text-tertiary !outline-none"
        onChange={(event) => {
          setLocalQuery(event.currentTarget.value);
          setSelectedIndex(0);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onClose();
            return;
          }
          if (event.key === "Backspace" && !localQuery && currentPath && !attachingPath) {
            event.preventDefault();
            event.stopPropagation();
            stepBackToParent();
            return;
          }
          if ((event.key === "ArrowDown" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "n")) && filteredEntries.length) {
            event.preventDefault();
            event.stopPropagation();
            setNavSource("keyboard");
            setSelectedIndex((current) => (current + 1) % filteredEntries.length);
            return;
          }
          if ((event.key === "ArrowUp" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "p")) && filteredEntries.length) {
            event.preventDefault();
            event.stopPropagation();
            setNavSource("keyboard");
            setSelectedIndex((current) => (current - 1 + filteredEntries.length) % filteredEntries.length);
            return;
          }
          if (event.key === "Enter" || event.key === "Tab") {
            event.preventDefault();
            event.stopPropagation();
            const entry = filteredEntries[selectedIndex];
            if (entry) choose(entry);
          }
        }}
        placeholder="Search files…"
        ref={searchRef}
        type="text"
        value={localQuery}
      />
    </div>
  ) : currentPath ? (
    <div className="flex min-w-0 items-center gap-1 px-row-x py-1">
      <button
        aria-label="Back to parent folder"
        className="cursor-interaction inline-flex size-6 shrink-0 items-center justify-center rounded-lg text-tertiary hover:bg-[var(--color-background-secondary-ghost-hover,var(--color-token-list-hover-background))] hover:text-default disabled:opacity-40"
        disabled={Boolean(attachingPath)}
        onClick={stepBackToParent}
        onMouseDown={(event) => event.preventDefault()}
        type="button"
      >
        <Glyph name="arrowLeft" size={14} />
      </button>
      <span className="min-w-0 flex-1 truncate text-sm text-[var(--color-codex-description,var(--color-text-description))]">
        {currentPath}
      </span>
    </div>
  ) : undefined;

  return (
    <ComposerSuggestionMenu
      ariaLabel="Attach projected file"
      dataAttribute={{ "data-composer-projected-file-picker": "true" }}
      fadeMaskVariant="bottom"
      header={header}
      ignoreOutsideRefs={ignoreOutsideRefs}
      listId={listId}
      listRole="listbox"
      onDismiss={onClose}
      zIndexClassName="z-[60]"
    >
      <ComposerSuggestionSectionLabel>
        {currentPath ? `${source.label} / ${currentPath}` : source.label}
      </ComposerSuggestionSectionLabel>
      {loading ? (
        <div className="flex items-center gap-2 px-row-x py-row-y text-sm text-tertiary">
          <OctopusSpinner className="size-4" />
          <span>Reading projected files…</span>
        </div>
      ) : null}
      {!loading && error ? (
        <div className="flex items-center gap-2 px-row-x py-row-y text-sm text-danger" role="alert">
          <Glyph name="warning" size={16} />
          <span className="min-w-0 truncate">{error}</span>
        </div>
      ) : null}
      {!loading && !error && !filteredEntries.length ? (
        <ComposerSuggestionEmpty>No matching files</ComposerSuggestionEmpty>
      ) : null}
      {!loading && !error ? filteredEntries.map((entry, index) => {
        const disabled = (entry.kind !== "directory" && entry.kind !== "file") || Boolean(attachingPath);
        const detail = entry.kind === "file"
          ? [entry.path !== entry.name ? entry.path : "", formatFileSize(entry.size)].filter(Boolean).join(" · ")
          : entry.path !== entry.name ? entry.path : undefined;
        return (
          <ComposerSuggestionRow
            description={detail}
            disabled={disabled}
            hasSubmenu={entry.kind === "directory"}
            highlighted={index === selectedIndex}
            icon={<Glyph name={entry.kind === "directory" ? "folder" : "file"} size={16} />}
            id={`${listId}-opt-${index}`}
            key={`${entry.kind}:${entry.path}`}
            onHighlight={() => {
              setNavSource("pointer");
              setSelectedIndex(index);
            }}
            onSelect={() => choose(entry)}
            query={query}
            rightAccessory={attachingPath === entry.path ? <OctopusSpinner className="size-4" /> : undefined}
            scrollIntoViewOnHighlight={navSource === "keyboard"}
            title={entry.name}
          />
        );
      }) : null}
    </ComposerSuggestionMenu>
  );
}

function formatFileSize(value: number | null): string {
  if (value == null) return "file";
  if (value < 1_000) return `${value} B`;
  if (value < 1_000_000) return `${Math.max(1, Math.round(value / 1_000))} KB`;
  return `${(value / 1_000_000).toFixed(value < 10_000_000 ? 1 : 0)} MB`;
}

function formatFailure(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function assertProjectedFileResponse(
  response: { workerEnvironmentId: string; surfaceId: string; path: string },
  source: ProjectedFileSource,
  expectedPath: string,
): void {
  if (
    response.workerEnvironmentId !== source.workerEnvironmentId
    || response.surfaceId !== source.surfaceId
    || response.path !== expectedPath
  ) {
    throw new Error("Projected file response no longer matches the selected Worker Environment root.");
  }
}
