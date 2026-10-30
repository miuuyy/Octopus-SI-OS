import { useEffect, useLayoutEffect, useMemo, useRef, type ReactNode, type RefObject } from "react";
import { FileTree, useFileTree } from "@pierre/trees/react";
import type { FileTree as FileTreeModel } from "@pierre/trees";
import { OcStatus } from "../design-system/components";
import { Glyph, type GlyphName } from "./IconGlyphs";
import "./octopus-surface.css";

export function OctopusEmptyState({ actions, className = "", contentClassName = "", description, icon, title }: {
  actions?: ReactNode;
  className?: string;
  contentClassName?: string;
  description?: ReactNode;
  icon: ReactNode;
  title: ReactNode;
}) {
  return (
    <div className={`flex w-full flex-col items-center justify-center px-4 py-8 text-center ${className}`.trim()}>
      <div className={`flex w-full max-w-72 flex-col items-center gap-3 ${contentClassName}`.trim()}>
        <div className="flex items-center justify-center [&>svg]:size-8 [&>svg]:text-token-text-secondary">{icon}</div>
        <div className="flex flex-col items-center gap-2">
          <div className="text-lg leading-6 font-medium text-token-foreground">{title}</div>
          {description == null ? null : <div className="text-sm text-token-text-secondary">{description}</div>}
        </div>
        {actions == null ? null : <div className="flex flex-wrap items-center justify-center gap-2">{actions}</div>}
      </div>
    </div>
  );
}

export function OctopusPanelNote({ children }: { children: ReactNode }) {
  return <div className="octopus-panel-note">{children}</div>;
}

export function OctopusFilterField({ autoFocus, inputId = "file-tree-search", inputRef, label, onQueryChange, placeholder, searchQuery, trailing }: {
  autoFocus?: boolean;
  inputId?: string;
  inputRef?: RefObject<HTMLInputElement | null>;
  label: string;
  onQueryChange: (value: string) => void;
  placeholder: string;
  searchQuery: string;
  trailing?: ReactNode;
}) {
  return (
    <div className="octopus-file-filter">
      <label className="sr-only" htmlFor={inputId}>{label}</label>
      <Glyph className="octopus-file-filter-search" name="search" size={16} />
      <input
        autoFocus={autoFocus}
        className="octopus-file-filter-input"
        id={inputId}
        onChange={(event) => onQueryChange(event.currentTarget.value)}
        placeholder={placeholder}
        ref={inputRef}
        type="text"
        value={searchQuery}
      />
      {searchQuery.length > 0 ? (
        <button
          aria-label="Clear file filter"
          className="octopus-file-filter-clear"
          onClick={() => onQueryChange("")}
          type="button"
        >
          <Glyph name="close" size={14} />
        </button>
      ) : trailing}
    </div>
  );
}

const GIT_ADDED_SVG = `<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M10 6.41797C10.367 6.41815 10.6649 6.71599 10.665 7.08301V9.33496H12.916C13.2832 9.33496 13.5809 9.63287 13.5811 10C13.5809 10.3671 13.2832 10.665 12.916 10.665H10.665V12.916C10.665 13.2832 10.3671 13.5809 10 13.5811C9.63284 13.5809 9.33496 13.2832 9.33496 12.916V10.665H7.08301C6.71596 10.6649 6.41815 10.367 6.41797 10C6.41813 9.63295 6.71595 9.33509 7.08301 9.33496H9.33496V7.08301C9.33513 6.71596 9.63295 6.4181 10 6.41797Z" fill="currentColor"/><path d="M12.666 2.66797C13.3549 2.66797 13.9121 2.66735 14.3623 2.7041C14.8202 2.74151 15.2268 2.82084 15.6035 3.0127C16.1989 3.31608 16.6829 3.80115 16.9863 4.39649C17.1781 4.77304 17.2575 5.17902 17.2949 5.63672C17.3317 6.08689 17.3311 6.64406 17.3311 7.33301V12.666C17.3311 13.3549 17.3317 13.9121 17.2949 14.3623C17.2575 14.8203 17.1782 15.2268 16.9863 15.6035C16.6829 16.1988 16.1988 16.6829 15.6035 16.9863C15.2268 17.1783 14.8203 17.2575 14.3623 17.2949C13.9121 17.3317 13.3549 17.3311 12.666 17.3311H7.33301C6.64403 17.3311 6.08691 17.3317 5.63672 17.2949C5.17899 17.2575 4.77306 17.1781 4.39649 16.9863C3.80114 16.6829 3.31609 16.1989 3.0127 15.6035C2.82085 15.2269 2.74151 14.8202 2.7041 14.3623C2.66736 13.9122 2.66797 13.3548 2.66797 12.666V7.33301C2.66797 6.64406 2.66732 6.08689 2.7041 5.63672C2.74152 5.17904 2.82093 4.77302 3.0127 4.39649C3.31613 3.80104 3.80103 3.31611 4.39649 3.0127C4.77304 2.82092 5.17902 2.74152 5.63672 2.7041C6.08691 2.66732 6.64403 2.66797 7.33301 2.66797H12.666ZM7.33301 3.99805C6.6221 3.99805 6.12859 3.99894 5.74512 4.03027C5.36955 4.06098 5.15798 4.11775 5 4.19824C4.65484 4.37414 4.37416 4.65485 4.19824 5C4.11775 5.15797 4.06098 5.36959 4.03027 5.74512C3.99894 6.12858 3.99805 6.62214 3.99805 7.33301V12.666C3.99805 13.3767 3.99898 13.8705 4.03027 14.2539C4.06094 14.6292 4.11788 14.8411 4.19824 14.999C4.37412 15.3442 4.65487 15.6258 5 15.8018C5.15795 15.8822 5.36972 15.939 5.74512 15.9697C6.12859 16.0011 6.6221 16.001 7.33301 16.001H12.666C13.3767 16.001 13.8705 16.001 14.2539 15.9697C14.6292 15.9391 14.8411 15.8821 14.999 15.8018C15.3443 15.6258 15.6258 15.3443 15.8018 14.999C15.8821 14.8411 15.9391 14.6292 15.9697 14.2539C16.001 13.8705 16.001 13.3767 16.001 12.666V7.33301C16.001 6.62213 16.0011 6.12858 15.9697 5.74512C15.939 5.36976 15.8822 5.15795 15.8018 5C15.6258 4.65488 15.3442 4.37411 14.999 4.19824C14.8411 4.11787 14.6292 4.06094 14.2539 4.03027C13.8705 3.99898 13.3767 3.99805 12.666 3.99805H7.33301Z" fill="currentColor"/></svg>`;
const GIT_DELETED_SVG = `<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M12.916 9.33496C13.2833 9.33496 13.5811 9.63273 13.5811 10C13.5811 10.3673 13.2833 10.665 12.916 10.665H7.08301C6.71585 10.6649 6.41797 10.3672 6.41797 10C6.41797 9.63281 6.71585 9.33509 7.08301 9.33496H12.916Z" fill="currentColor"/><path d="M12.666 2.66797C13.3549 2.66797 13.9121 2.66735 14.3623 2.7041C14.8202 2.74151 15.2268 2.82084 15.6035 3.0127C16.1989 3.31608 16.6829 3.80115 16.9863 4.39649C17.1781 4.77304 17.2575 5.17902 17.2949 5.63672C17.3317 6.08689 17.3311 6.64406 17.3311 7.33301V12.666C17.3311 13.3549 17.3317 13.9121 17.2949 14.3623C17.2575 14.8203 17.1782 15.2268 16.9863 15.6035C16.6829 16.1988 16.1988 16.6829 15.6035 16.9863C15.2268 17.1783 14.8203 17.2575 14.3623 17.2949C13.9121 17.3317 13.3549 17.3311 12.666 17.3311H7.33301C6.64403 17.3311 6.08691 17.3317 5.63672 17.2949C5.17899 17.2575 4.77306 17.1781 4.39649 16.9863C3.80114 16.6829 3.31609 16.1989 3.0127 15.6035C2.82085 15.2269 2.74151 14.8202 2.7041 14.3623C2.66736 13.9122 2.66797 13.3548 2.66797 12.666V7.33301C2.66797 6.64406 2.66732 6.08689 2.7041 5.63672C2.74152 5.17904 2.82093 4.77302 3.0127 4.39649C3.31613 3.80104 3.80103 3.31611 4.39649 3.0127C4.77304 2.82092 5.17902 2.74152 5.63672 2.7041C6.08691 2.66732 6.64403 2.66797 7.33301 2.66797H12.666ZM7.33301 3.99805C6.6221 3.99805 6.12859 3.99894 5.74512 4.03027C5.36955 4.06098 5.15798 4.11775 5 4.19824C4.65484 4.37414 4.37416 4.65485 4.19824 5C4.11775 5.15797 4.06098 5.36959 4.03027 5.74512C3.99894 6.12858 3.99805 6.62214 3.99805 7.33301V12.666C3.99805 13.3767 3.99898 13.8705 4.03027 14.2539C4.06094 14.6292 4.11788 14.8411 4.19824 14.999C4.37412 15.3442 4.65487 15.6258 5 15.8018C5.15795 15.8822 5.36972 15.939 5.74512 15.9697C6.12859 16.0011 6.6221 16.001 7.33301 16.001H12.666C13.3767 16.001 13.8705 16.001 14.2539 15.9697C14.6292 15.9391 14.8411 15.8821 14.999 15.8018C15.3443 15.6258 15.6258 15.3443 15.8018 14.999C15.8821 14.8411 15.9391 14.6292 15.9697 14.2539C16.001 13.8705 16.001 13.3767 16.001 12.666V7.33301C16.001 6.62213 16.0011 6.12858 15.9697 5.74512C15.939 5.36976 15.8822 5.15795 15.8018 5C15.6258 4.65488 15.3442 4.37411 14.999 4.19824C14.8411 4.11787 14.6292 4.06094 14.2539 4.03027C13.8705 3.99898 13.3767 3.99805 12.666 3.99805H7.33301Z" fill="currentColor"/></svg>`;
const GIT_MODIFIED_SVG = `<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M10 8.33301C10.9205 8.33301 11.667 9.07953 11.667 10C11.667 10.9205 10.9205 11.667 10 11.667C9.07961 11.6669 8.334 10.9204 8.33399 10C8.33399 9.07958 9.0796 8.3331 10 8.33301Z" fill="currentColor"/><path d="M12.667 2.66797C13.3558 2.66797 13.9132 2.66734 14.3633 2.7041C14.8211 2.74154 15.2279 2.82079 15.6045 3.0127C16.1998 3.31614 16.6839 3.8011 16.9873 4.39649C17.1791 4.77306 17.2585 5.17899 17.2959 5.63672C17.3327 6.08689 17.332 6.64406 17.332 7.33301V12.666C17.332 13.3548 17.3326 13.9122 17.2959 14.3623C17.2585 14.8201 17.1791 15.2269 16.9873 15.6035C16.684 16.1987 16.1996 16.6829 15.6045 16.9863C15.2279 17.1782 14.8211 17.2575 14.3633 17.2949C13.9132 17.3317 13.3558 17.3311 12.667 17.3311H7.33399C6.64513 17.3311 6.08786 17.3317 5.6377 17.2949C5.17997 17.2575 4.77403 17.1781 4.39746 16.9863C3.80197 16.6829 3.31712 16.199 3.01367 15.6035C2.82172 15.2268 2.7425 14.8203 2.70508 14.3623C2.66833 13.9121 2.66895 13.3549 2.66895 12.666V7.33301C2.66895 6.64406 2.6683 6.08689 2.70508 5.63672C2.7425 5.17907 2.82191 4.77301 3.01367 4.39649C3.31713 3.80115 3.80208 3.31606 4.39746 3.0127C4.77393 2.82108 5.18011 2.7415 5.6377 2.7041C6.08785 2.66736 6.64515 2.66797 7.33399 2.66797H12.667ZM7.33399 3.99805C6.62326 3.99805 6.12954 3.99898 5.74609 4.03027C5.37079 4.06094 5.15893 4.11787 5.00098 4.19824C4.65588 4.37409 4.37516 4.65496 4.19922 5C4.11874 5.15796 4.06196 5.36963 4.03125 5.74512C3.99992 6.12858 3.99902 6.62213 3.99902 7.33301V12.666C3.99902 13.3767 3.99996 13.8705 4.03125 14.2539C4.06193 14.6295 4.11878 14.841 4.19922 14.999C4.37515 15.3443 4.6557 15.6258 5.00098 15.8018C5.15894 15.8821 5.37074 15.9391 5.74609 15.9697C6.12955 16.001 6.62323 16.001 7.33399 16.001H12.667C13.3776 16.001 13.8715 16.001 14.2549 15.9697C14.6299 15.939 14.8421 15.8821 15 15.8018C15.345 15.6258 15.6269 15.3441 15.8027 14.999C15.883 14.8411 15.9401 14.6289 15.9707 14.2539C16.002 13.8705 16.002 13.3767 16.002 12.666V7.33301C16.002 6.62213 16.002 6.12858 15.9707 5.74512C15.94 5.36972 15.8832 5.15795 15.8027 5C15.6269 4.65483 15.3451 4.37417 15 4.19824C14.8421 4.11782 14.6302 4.06097 14.2549 4.03027C13.8715 3.99897 13.3776 3.99805 12.667 3.99805H7.33399Z" fill="currentColor"/></svg>`;

function maskUrl(svg: string): string {
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

function octopusTreeCss(surface: "main" | "dropdown", directScrolling: boolean) {
  const background = surface === "dropdown" ? "var(--color-surface-elevated-secondary)" : "var(--color-surface)";
  return `
      :host {
        --trees-bg-override: ${background};
        --trees-bg-muted-override: var(--color-background-primary-ghost-hover);
        --trees-border-color-override: var(--color-border);
        --trees-fg-override: var(--color-text);
        --trees-font-size-override: 13px;
        --trees-focus-ring-color-override: var(--color-ring);
        --trees-git-added-color-override: var(--color-octopus-git-added);
        --trees-git-deleted-color-override: var(--color-octopus-git-deleted);
        --trees-git-ignored-color-override: var(--color-octopus-git-ignored);
        --trees-git-lane-width-override: 20px;
        --trees-git-modified-color-override: var(--color-octopus-git-modified);
        --trees-git-renamed-color-override: var(--color-octopus-git-modified);
        --trees-git-untracked-color-override: var(--color-octopus-git-added);
        --trees-item-padding-x-override: 6px;
        --trees-item-margin-x-override: 0px;
        --trees-level-gap-override: 0px;
        --trees-padding-inline-override: 0px;
        --trees-scrollbar-gutter-override: 0px;
        --trees-scrollbar-gutter-measured: 0px;
        --trees-selected-bg-override: var(--color-background-primary-soft-active);
        --trees-selected-fg-override: var(--color-text-emphasis);
        --trees-item-row-gap-override: 10px;
      }

      [data-file-tree-sticky-overlay-content='true'],
      [data-file-tree-sticky-row='true'] {
        background-color: ${background};
      }

      [data-file-tree-virtualized-scroll='true'] {
        scrollbar-gutter: auto;
      }

      [role="treeitem"] {
        cursor: var(--cursor-interaction) !important;
      }

      [role="treeitem"] * {
        cursor: var(--cursor-interaction) !important;
      }

      [data-item-type='file'][data-item-path$='/']:has([data-item-section='content']:empty) {
        display: none;
      }

      [data-item-git-status] > [data-item-section='content'] {
        color: inherit;
      }

      [data-item-git-status='added'] > [data-item-section='git'] > span,
      [data-item-git-status='deleted'] > [data-item-section='git'] > span,
      [data-item-git-status='modified'] > [data-item-section='git'] > span {
        font-size: 0;
      }

      [data-item-git-status='added'] > [data-item-section='git'] > span::before,
      [data-item-git-status='deleted'] > [data-item-section='git'] > span::before,
      [data-item-git-status='modified'] > [data-item-section='git'] > span::before {
        width: 20px;
        height: 20px;
        background-color: currentColor;
        content: '';
        mask: var(--base-file-tree-git-status-icon) center / contain no-repeat;
      }

      [data-item-git-status='added'] > [data-item-section='git'] > span::before {
        --base-file-tree-git-status-icon: url("${maskUrl(GIT_ADDED_SVG)}");
      }

      [data-item-git-status='deleted'] > [data-item-section='git'] > span::before {
        --base-file-tree-git-status-icon: url("${maskUrl(GIT_DELETED_SVG)}");
      }

      [data-item-git-status='modified'] > [data-item-section='git'] > span::before {
        --base-file-tree-git-status-icon: url("${maskUrl(GIT_MODIFIED_SVG)}");
      }

      @container measure (height <= calc(1lh + 1px)) {
        [data-truncate-marker] {
          opacity: 0;
        }
      }
      ${directScrolling ? "[data-file-tree-virtualized-scroll='true'] { scroll-behavior: auto; }" : ""}
`;
}

export function pathFromTreeEvent(event: Event): string | null {
  const path = rawPathFromTreeEvent(event);
  return path == null || path.endsWith("/") ? null : path;
}

function rawPathFromTreeEvent(event: Event): string | null {
  for (const node of event.composedPath()) {
    if (node instanceof Element) {
      const path = node.getAttribute("data-item-path");
      if (path != null) return path;
    }
  }
  return null;
}

export function OctopusFileTree({ directScrolling = false, flattenEmptyDirectories = false, initialExpandedPaths, initialSelectedPaths, onExpandedPathsChange, onModelEvent, onOpenPath, onSelectPath, onToggleDirectory, paths, resetKey, revealSelectedPath = false, revealSelectedPathScrollOffset = "nearest", selectedPath, surface = "main" }: {
  directScrolling?: boolean;
  flattenEmptyDirectories?: boolean;
  initialExpandedPaths?: readonly string[];
  initialSelectedPaths?: readonly string[];
  onExpandedPathsChange?: (paths: string[]) => void;
  onModelEvent?: (model: FileTreeModel) => void;
  onOpenPath?: (path: string) => void;
  onSelectPath?: (path: string) => void;
  onToggleDirectory?: (path: string) => void;
  paths: readonly string[];
  resetKey?: string;
  revealSelectedPath?: boolean;
  revealSelectedPathScrollOffset?: "nearest" | "center";
  selectedPath?: string;
  surface?: "main" | "dropdown";
}) {
  const selectionSyncRef = useRef(false);
  const callbacksRef = useRef({ onSelectPath, onExpandedPathsChange, onModelEvent });
  callbacksRef.current = { onSelectPath, onExpandedPathsChange, onModelEvent };
  const { model } = useFileTree({
    fileTreeSearchMode: "hide-non-matches",
    flattenEmptyDirectories,
    initialExpandedPaths,
    initialSelectedPaths: initialSelectedPaths ?? (selectedPath ? [selectedPath] : undefined),
    itemHeight: 28,
    paths,
    search: false,
    stickyFolders: true,
    unsafeCSS: octopusTreeCss(surface, directScrolling),
    onSelectionChange: (selectedPaths) => {
      if (selectionSyncRef.current) return;
      const path = selectedPaths[0];
      if (path && !path.endsWith("/")) callbacksRef.current.onSelectPath?.(path);
    },
  });
  const appliedRef = useRef<{ paths: readonly string[]; expanded: readonly string[]; resetKey?: string } | null>(null);
  const revealedRef = useRef<{ path: string; offset: "nearest" | "center" } | null>(null);

  useLayoutEffect(() => {
    const applied = appliedRef.current;
    const expanded = initialExpandedPaths ?? [];
    if (applied && applied.resetKey === resetKey && sameTreePaths(applied.paths, paths) && sameTreePaths(applied.expanded, expanded)) return;
    appliedRef.current = { paths: [...paths], expanded: [...expanded], resetKey };
    if (applied) {
      selectionSyncRef.current = true;
      try { model.resetPaths(paths, { initialExpandedPaths }); }
      finally { selectionSyncRef.current = false; }
    }
    if (applied?.resetKey !== resetKey || revealedRef.current && !model.getItem(revealedRef.current.path)) revealedRef.current = null;
  }, [initialExpandedPaths, model, paths, resetKey]);

  useLayoutEffect(() => {
    if (!selectedPath) return;
    const item = model.getItem(selectedPath);
    if (!item) return;
    selectionSyncRef.current = true;
    try {
      const selectedPaths = model.getSelectedPaths();
      if (selectedPaths.length !== 1 || selectedPaths[0] !== selectedPath) {
        for (const path of selectedPaths) model.getItem(path)?.deselect();
        item.select();
      }
    } finally { selectionSyncRef.current = false; }
    if (revealSelectedPath && (revealedRef.current?.path !== selectedPath || revealedRef.current.offset !== revealSelectedPathScrollOffset)) {
      model.scrollToPath(selectedPath, { offset: revealSelectedPathScrollOffset });
      revealedRef.current = { path: selectedPath, offset: revealSelectedPathScrollOffset };
    }
  }, [model, paths, resetKey, revealSelectedPath, revealSelectedPathScrollOffset, selectedPath]);

  useEffect(() => {
    let previousExpanded: string[] | null = null;
    const report = () => {
      if (selectionSyncRef.current) return;
      callbacksRef.current.onModelEvent?.(model);
      if (!callbacksRef.current.onExpandedPathsChange) return;
      const expanded = paths.filter((path) => path.endsWith("/")).map((path) => path.slice(0, -1)).filter((path) => {
        const item = model.getItem(path);
        return item && "isExpanded" in item && item.isExpanded();
      });
      if (previousExpanded && sameTreePaths(previousExpanded, expanded)) return;
      previousExpanded = expanded;
      callbacksRef.current.onExpandedPathsChange(expanded);
    };
    report();
    return model.subscribe(report);
  }, [model, paths]);

  const hostHandlers = useMemo(() => ({
    onClick: (event: React.MouseEvent) => {
      const path = rawPathFromTreeEvent(event.nativeEvent);
      if (path == null) return;
      if (path.endsWith("/")) {
        onToggleDirectory?.(path.slice(0, -1));
        return;
      }
    },
    onDoubleClick: (event: React.MouseEvent) => {
      if (!onOpenPath) return;
      const path = pathFromTreeEvent(event.nativeEvent);
      if (path != null) onOpenPath(path);
    },
  }), [onOpenPath, onToggleDirectory]);

  return (
    <div className={surface === "dropdown" ? "h-full min-h-0 w-full" : "h-full min-h-0 w-full px-2"}>
      <FileTree model={model} style={{ display: "block", height: "100%", minHeight: 0 }} {...hostHandlers} />
    </div>
  );
}

function sameTreePaths(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((path, index) => path === right[index]);
}

export function octopusTreeAncestors(paths: readonly string[]): string[] {
  const ancestors = new Set<string>();
  for (const path of paths) {
    const parts = (path.endsWith("/") ? path.slice(0, -1) : path).split("/");
    for (let index = 1; index < parts.length; index += 1) {
      ancestors.add(parts.slice(0, index).join("/"));
    }
  }
  return [...ancestors];
}

export function octopusSurfaceGlyph(name: GlyphName, size = 32): ReactNode {
  return <Glyph name={name} size={size} />;
}

export function OctopusSurfaceShell({ children, glyph, status, statusTone = "muted", subtitle, title }: {
  children: ReactNode;
  glyph: GlyphName;
  status?: string;
  statusTone?: "active" | "error" | "muted";
  subtitle?: string;
  title: string;
}) {
  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-token-main-surface-primary">
      <header className="flex shrink-0 items-center gap-2.5 border-b border-token-border px-3 py-2.5">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-token-border text-token-text-secondary">
          <Glyph name={glyph} size={16} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-sm font-medium text-token-foreground">{title}</span>
          {subtitle ? <span className="truncate text-xs text-token-text-secondary">{subtitle}</span> : null}
        </span>
        {status ? (
          <OcStatus className="shrink-0" label={status} tone={statusTone === "active" ? "success" : statusTone === "error" ? "danger" : "neutral"} />
        ) : null}
      </header>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-3">
        {children}
      </div>
    </div>
  );
}

export function OctopusSection({ children, count, title }: {
  children: ReactNode;
  count?: number | string;
  title: string;
}) {
  return (
    <section className="flex min-w-0 flex-col gap-1">
      <header className="flex items-center justify-between px-1 pb-0.5">
        <h3 className="m-0 text-xs font-medium text-token-text-secondary">{title}</h3>
        {count == null ? null : <span className="text-xs text-token-input-placeholder-foreground">{count}</span>}
      </header>
      <div className="overflow-hidden rounded-xl border border-token-border">
        {children}
      </div>
    </section>
  );
}

export function OctopusRow({ label, mono = false, value }: {
  label: string;
  mono?: boolean;
  value: ReactNode;
}) {
  return (
    <div className="flex min-h-9 items-center justify-between gap-3 border-b border-token-border px-3 py-1.5 text-sm last:border-b-0">
      <span className="shrink-0 text-token-text-secondary">{label}</span>
      <span className={`min-w-0 truncate text-right text-token-foreground ${mono ? "font-mono text-xs" : ""}`.trim()}>{value}</span>
    </div>
  );
}

export function OctopusNoteLine({ children }: { children: ReactNode }) {
  return <p className="m-0 px-1 text-xs leading-5 text-token-text-secondary">{children}</p>;
}

export function OctopusLogBlock({ children, label, tone = "default" }: {
  children: string;
  label: string;
  tone?: "default" | "error";
}) {
  return (
    <pre
      aria-label={label}
      className={`m-0 max-h-52 overflow-auto rounded-lg border border-token-border bg-token-bg-fog p-2.5 font-mono text-xs leading-5 whitespace-pre-wrap ${tone === "error" ? "text-token-error-foreground" : "text-token-text-secondary"}`}
    >{children}</pre>
  );
}

export function OctopusPillRow({ values }: { values: string[] }) {
  if (!values.length) return null;
  return (
    <div className="flex min-w-0 flex-wrap gap-1.5 px-1">
      {values.map((value) => (
        <span
          className="flex max-w-56 shrink-0 items-center gap-1.5 rounded-full border border-token-border bg-transparent px-2 py-0.5 text-sm leading-5 text-token-text-primary"
          key={value}
        >
          <span className="min-w-0 truncate">{value}</span>
        </span>
      ))}
    </div>
  );
}
