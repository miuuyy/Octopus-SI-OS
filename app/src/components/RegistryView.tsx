import { OcLoadingRows, OcLoadingText } from "../design-system/LoadingSkeleton";
import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { OcButton, OcMenuItem, OcMenuSurface, OcStatus } from "../design-system/components";
import { useFloatingMenu } from "../design-system/useFloatingMenu";
import type { OctopusClientAccessProfile, OctopusOsFile, OctopusOsFileEntry } from "../electron";
import { useProjectionStructureGeneration } from "../projectionLive";
import { readProjection } from "../projectionClient";
import { useAppErrorToast } from "../toasts";
import { RegistryArtifactsColumns } from "./RegistryArtifactsColumns";
import { Glyph } from "./IconGlyphs";
import { JsonPanel } from "./JsonPanel";
import type { FilesOpenTarget } from "./files/types";
import { useFixedVirtualRange } from "./useFixedVirtualRange";

type RegistryFrame = {
  path: string;
  selectedPath?: string;
};

type RegistryFileMenu = {
  path: string;
  rootId: "octopus-home";
  x: number;
  y: number;
};

type InterfaceCatalogRecord = Record<string, unknown> & {
  id?: string;
  rule_id?: string;
  source_file?: string;
  interface_type?: string;
  role?: string;
  direction?: string;
  mode?: string;
  source_names?: unknown[];
  description?: string;
  status?: string;
  artifact_transfer_protocol_status?: string;
  artifact_transfer_target_count?: number;
  artifact_transfer_ready_targets?: number;
  artifact_transfer_target_reason?: string;
  application_manifest?: Record<string, unknown> | null;
};

type InterfaceBindingRecord = {
  interface_id?: string;
  run_id?: string;
  ee_id?: string;
  checkpoint_id?: string;
  direction?: string;
  status?: string;
};

type InterfaceApplication = {
  id: string;
  name: string;
  category: string;
};

type InterfaceCatalogItem = {
  id: string;
  path: string;
  record: InterfaceCatalogRecord;
  bindings: InterfaceBindingRecord[];
  bindingsProjected: boolean;
  applications: InterfaceApplication[];
  availableIn: string[];
  availabilityProjected: boolean;
};

type InterfaceCatalogIssue = {
  kind: "fatal" | "partial";
  message: string;
};

type RegistryDescriptorTarget = {
  path: string;
  rootId: "octopus-home";
  interfaceItem?: InterfaceCatalogItem;
};

const REGISTRY_ROOT_ID = "octopus-home";
const REGISTRY_ROOT = "registry";
const DEFAULT_REGISTRY_SECTION = "registry/artifact";

export function RegistryView({ accessProfile, onOpenFiles, query }: {
  accessProfile: OctopusClientAccessProfile;
  onOpenFiles: (target: FilesOpenTarget) => void;
  query: string;
}) {
  const [frames, setFrames] = useState<RegistryFrame[]>([
    { path: REGISTRY_ROOT, selectedPath: DEFAULT_REGISTRY_SECTION },
    { path: DEFAULT_REGISTRY_SECTION },
  ]);
  const [descriptorTarget, setDescriptorTarget] = useState<RegistryDescriptorTarget | null>(null);
  const [fileMenu, setFileMenu] = useState<RegistryFileMenu | null>(null);
  const fileMenuRef = useRef<HTMLDivElement>(null);
  const renderedColumns = Math.max(1, frames.length - 1 + (descriptorTarget ? 1 : 0));

  useFloatingMenu(Boolean(fileMenu), fileMenuRef, () => setFileMenu(null), fileMenu ? { left: fileMenu.x, top: fileMenu.y } : null);

  if (!window.octopusTransport?.listOsFiles) {
    return <RuntimeEmpty title="Files bridge unavailable" detail="Registry is the admitted registry/ directory tree." />;
  }

  return (
    <section
      className="registry-view"
      style={{ "--rg-col-w": `calc((100% - 222px) / ${renderedColumns})` } as CSSProperties}
    >
      {frames.map((frame, index) => frame.path === DEFAULT_REGISTRY_SECTION && index > 0 ? (
        <RegistryArtifactsColumns key={frame.path} onOpenFiles={onOpenFiles} query={query} />
      ) : frame.path === "registry/interface" && index > 0 ? (
        <RegistryInterfaceColumn
          activePath={descriptorTarget?.interfaceItem?.path ?? frame.selectedPath}
          accessProfile={accessProfile}
          key={frame.path}
          onContextMenu={(item, x, y) => {
            setFrames([
              ...frames.slice(0, index),
              { path: frame.path, selectedPath: item.path },
            ]);
            setDescriptorTarget({ path: item.path, rootId: REGISTRY_ROOT_ID, interfaceItem: item });
            setFileMenu({ path: item.path, rootId: REGISTRY_ROOT_ID, x, y });
          }}
          onSelect={(item) => {
            setFileMenu(null);
            setFrames([
              ...frames.slice(0, index),
              { path: frame.path, selectedPath: item.path },
            ]);
            setDescriptorTarget({ path: item.path, rootId: REGISTRY_ROOT_ID, interfaceItem: item });
          }}
          query={query}
        />
      ) : (
        <RegistryDirectoryColumn
          activePath={frames[index + 1]?.path}
          frame={frame}
          index={index}
          key={frame.path}
          onFileContextMenu={(entry, x, y) => {
            const path = registryDescriptorPath(frame.path, entry);
            setFrames([
              ...frames.slice(0, index),
              { path: frame.path, selectedPath: entry.path },
            ]);
            setFileMenu({ path, rootId: REGISTRY_ROOT_ID, x, y });
          }}
          onSelect={(entry) => {
            setFileMenu(null);
            if (index === 0 && entry.kind === "directory") {
              setDescriptorTarget(null);
              setFrames([
                ...frames.slice(0, index),
                { path: frame.path, selectedPath: entry.path },
                { path: entry.path },
              ]);
              return;
            }
            const path = registryDescriptorPath(frame.path, entry);
            setFrames([
              ...frames.slice(0, index),
              { path: frame.path, selectedPath: entry.path },
            ]);
            setDescriptorTarget({ path, rootId: REGISTRY_ROOT_ID });
          }}
          query={query}
        />
      ))}
      {descriptorTarget ? <RegistryJsonColumn onOpenFiles={onOpenFiles} target={descriptorTarget} /> : null}
      {fileMenu ? createPortal(
        <OcMenuSurface
          aria-label={`Registry file actions for ${fileMenu.path}`}
          className="registry-file-context-menu"
          data-settings-layer="true"
          role="menu"
          surfaceRef={fileMenuRef}
          style={{
            left: Math.max(4, Math.min(fileMenu.x, window.innerWidth - 196)),
            top: Math.max(4, Math.min(fileMenu.y, window.innerHeight - 56)),
          }}
        >
          <OcMenuItem
            label="Open in Files"
            leadingIcon="file"
            onClick={() => {
              const path = fileMenu.path;
              const rootId = fileMenu.rootId;
              setFileMenu(null);
              onOpenFiles({ kind: "file", path, rootId });
            }}
            role="menuitem"
          />
        </OcMenuSurface>,
        document.body,
      ) : null}
    </section>
  );
}

function RegistryJsonColumn({ onOpenFiles, target }: { onOpenFiles: (target: FilesOpenTarget) => void; target: RegistryDescriptorTarget }) {
  const transport = window.octopusTransport;
  const [file, setFile] = useState<OctopusOsFile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [showRaw, setShowRaw] = useState(false);
  const [reloadTick, setReloadTick] = useState(0);
  useAppErrorToast(error);
  const { path, rootId } = target;

  useEffect(() => {
    let cancelled = false;
    setFile(null);
    setLoading(true);
    setError("");
    setShowRaw(false);
    if (!transport?.readOsFile) {
      setLoading(false);
      setError("The desktop Files bridge is unavailable.");
      return () => { cancelled = true; };
    }
    void transport.readOsFile({ path, rootId })
      .then((response) => {
        if (cancelled) return;
        if (response.rootId !== rootId || response.path !== path) {
          throw new Error("Registry returned a different descriptor file.");
        }
        setFile(response);
      })
      .catch((failure) => {
        if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [path, reloadTick, rootId, transport]);

  const body = useMemo(() => formatRegistryFile(file), [file]);
  const title = target.interfaceItem ? interfaceProductName(target.interfaceItem) : registryDisplayName(path.split("/").at(-1) ?? path);

  return (
    <aside aria-busy={loading} className="rg-col rg-col-detail">
      <header className="rg-col-head">
        <div>
          <div className="rg-col-title">{title}</div>
          <div className="rg-col-meta">{loading ? <OcLoadingText label="Loading descriptor metadata…">Metadata</OcLoadingText> : error ? "unavailable" : registryFileMeta(file)}</div>
        </div>
      </header>
      <div className="rg-detail-body">
        {target.interfaceItem ? (
          <InterfaceDescriptorOverview
            item={target.interfaceItem}
            onOpenFiles={() => onOpenFiles({ kind: "file", path, rootId })}
          />
        ) : null}
        {error ? target.interfaceItem ? (
          <RegistryFailureState
            detail={error}
            onRetry={() => setReloadTick((value) => value + 1)}
            retrying={loading}
            title="Descriptor unavailable"
          />
        ) : <div className="rg-error">{error}</div> : null}
        {!error && loading ? <OcLoadingRows label="Loading descriptor…" /> : null}
        {!error && !loading && file?.kind === "binary" ? (
          <div className="rg-empty">Binary registry files cannot be rendered as JSON.</div>
        ) : null}
        {!error && !loading && file?.truncated ? (
          <div className="rg-error">The descriptor exceeds the safe Registry preview limit.</div>
        ) : null}
        {!error && !loading && file?.kind === "text" && !file.truncated ? (
          <>
            {target.interfaceItem ? (
              <OcButton className="self-start" onClick={() => setShowRaw((current) => !current)} size="sm" variant="ghost">
                {showRaw ? "Hide raw JSON" : "Show raw JSON"}
              </OcButton>
            ) : null}
            {!target.interfaceItem || showRaw ? (
              <>
                <div className="rg-detail-title">
                  <span>Registry descriptor</span>
                  <strong>{file.path}</strong>
                </div>
                <JsonPanel text={body} copyLabel="Copy descriptor JSON" />
              </>
            ) : null}
          </>
        ) : null}
      </div>
    </aside>
  );
}

function RegistryInterfaceColumn({ accessProfile, activePath, onContextMenu, onSelect, query }: {
  accessProfile: OctopusClientAccessProfile;
  activePath?: string;
  onContextMenu: (item: InterfaceCatalogItem, x: number, y: number) => void;
  onSelect: (item: InterfaceCatalogItem) => void;
  query: string;
}) {
  const projectionGeneration = useProjectionStructureGeneration();
  const [items, setItems] = useState<InterfaceCatalogItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [issue, setIssue] = useState<InterfaceCatalogIssue | null>(null);
  const [localFilter, setLocalFilter] = useState("");
  const [reloadTick, setReloadTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void Promise.allSettled([
      readProjection<unknown>("projection.interfaces.list", { limit: 5_000 }),
      readProjection<unknown>("projection.bindings.list", { limit: 50_000 }),
      readProjection<unknown>("projection.operator.os.scope"),
    ]).then(([interfaces, bindings, scope]) => {
      if (cancelled) return;
      if (interfaces.status === "rejected") {
        setIssue({ kind: "fatal", message: projectionFailureMessage(interfaces.reason) });
        return;
      }
      const incomplete = [
        bindings.status === "rejected" ? `Bindings: ${projectionFailureMessage(bindings.reason)}` : "",
        scope.status === "rejected" ? `Octopus OS scope: ${projectionFailureMessage(scope.reason)}` : "",
      ].filter(Boolean);
      setItems(buildInterfaceCatalog(
        interfaces.value,
        bindings.status === "fulfilled" ? bindings.value : { bindings: [] },
        scope.status === "fulfilled" ? scope.value : {},
        {
          availabilityProjected: scope.status === "fulfilled",
          bindingsProjected: bindings.status === "fulfilled",
        },
      ));
      setIssue(incomplete.length ? { kind: "partial", message: incomplete.join(" · ") } : null);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [accessProfile, projectionGeneration, reloadTick]);

  const filteredItems = useMemo(() => {
    return items.filter((item) => registryFilterMatches(interfaceSearchText(item), query, localFilter));
  }, [items, localFilter, query]);
  const rowHeight = 68;
  const virtualRows = useFixedVirtualRange({ count: filteredItems.length, itemSize: rowHeight });
  const activeIndex = filteredItems.findIndex((item) => item.path === activePath);
  const [focusedIndex, setFocusedIndex] = useState(() => Math.max(0, activeIndex));

  useEffect(() => {
    setFocusedIndex(Math.max(0, activeIndex));
  }, [activeIndex, filteredItems.length]);

  const focusItem = (nextIndex: number) => {
    const target = Math.max(0, Math.min(filteredItems.length - 1, nextIndex));
    setFocusedIndex(target);
    const viewport = virtualRows.scrollRef.current;
    if (!viewport) return;
    const top = target * rowHeight;
    const bottom = top + rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    window.requestAnimationFrame(() => {
      viewport.querySelector<HTMLButtonElement>(`[data-interface-row-index="${target}"]`)?.focus();
    });
  };

  const onRowKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, rowIndex: number) => {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    const page = Math.max(1, Math.floor(virtualRows.viewportHeight / rowHeight));
    const target = event.key === "ArrowDown" ? rowIndex + 1
      : event.key === "ArrowUp" ? rowIndex - 1
        : event.key === "PageDown" ? rowIndex + page
          : event.key === "PageUp" ? rowIndex - page
            : event.key === "Home" ? 0
              : event.key === "End" ? filteredItems.length - 1
                : null;
    if (target === null) return;
    event.preventDefault();
    focusItem(target);
  };

  return (
    <section aria-busy={loading} className="rg-col min-w-[360px] flex-[0_0_max(360px,var(--rg-col-w))]">
      <header className="rg-col-head">
        <div>
          <div className="rg-col-title">Interfaces</div>
          <div className="rg-col-meta">
            {loading
              ? items.length ? "refreshing" : "loading"
              : issue?.kind === "fatal" && !items.length
                ? "unavailable"
                : `${filteredItems.length} registered${issue ? " · incomplete" : ""}`}
          </div>
        </div>
        <input
          aria-label="Filter interfaces"
          className="rg-col-filter"
          onChange={(event) => setLocalFilter(event.currentTarget.value)}
          placeholder="filter"
          spellCheck={false}
          value={localFilter}
        />
      </header>
      {issue && (items.length > 0 || issue.kind === "partial") ? (
        <RegistryPartialFailure
          detail={issue.message}
          onRetry={() => setReloadTick((value) => value + 1)}
          retrying={loading}
          title={issue.kind === "fatal" ? "Interfaces could not be refreshed" : "Some interface facts could not be loaded"}
        />
      ) : null}
      <div className="rg-col-body" onScroll={virtualRows.onScroll} ref={virtualRows.setScrollRef}>
        {issue?.kind === "fatal" && !items.length && !loading ? (
          <RegistryFailureState
            detail={issue.message}
            onRetry={() => setReloadTick((value) => value + 1)}
            retrying={loading}
            title="Interfaces unavailable"
          />
        ) : null}
        {loading && !items.length ? <OcLoadingRows label="Loading registered interfaces…" /> : null}
        {!loading && issue?.kind !== "fatal" && !items.length ? <div className="rg-empty">No interfaces are registered.</div> : null}
        {!loading && items.length > 0 && !filteredItems.length ? <div className="rg-empty">No interfaces match this filter.</div> : null}
        {filteredItems.length ? (
          <div style={{ height: virtualRows.totalSize, position: "relative" }}>
            <div style={{ position: "absolute", top: virtualRows.start * rowHeight, width: "100%" }}>
              {filteredItems.slice(virtualRows.start, virtualRows.end).map((item, offset) => {
                const rowIndex = virtualRows.start + offset;
                const active = item.path === activePath;
                const readiness = interfaceArtifactHandInFact(item.record);
                return (
                  <button
                    className={`flex w-full items-center gap-2.5 border-0 border-b border-token-border/60 px-3 text-left outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-token-focus-border ${active ? "bg-token-list-active-selection-background" : "bg-transparent hover:bg-token-list-hover-background focus-visible:bg-token-list-hover-background"}`}
                    data-interface-row-index={rowIndex}
                    key={item.id}
                    onClick={() => onSelect(item)}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      onContextMenu(item, event.clientX, event.clientY);
                    }}
                    onFocus={() => setFocusedIndex(rowIndex)}
                    onKeyDown={(event) => onRowKeyDown(event, rowIndex)}
                    style={{ height: rowHeight }}
                    tabIndex={focusedIndex === rowIndex ? 0 : -1}
                    title={item.id}
                    type="button"
                  >
                    <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-token-border text-token-text-secondary">
                      <Glyph name={interfaceGlyph(item)} size={15} />
                    </span>
                    <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                      <span className="truncate text-xs font-medium text-token-foreground">{interfaceProductName(item)}</span>
                      <span className="truncate text-[10px] text-token-text-secondary">{interfaceClassification(item.record)}</span>
                      <span className="truncate font-mono text-[9px] text-token-description-foreground">{item.id}</span>
                    </span>
                    <span className="flex max-w-28 shrink-0 flex-col items-end gap-1 text-[9px] text-token-description-foreground">
                      <OcStatus detail={readiness.detail} label={readiness.label} tone={readiness.tone === "ready" ? "success" : readiness.tone === "error" ? "danger" : "neutral"} />
                      <span>{item.bindingsProjected
                        ? item.bindings.length ? `${item.bindings.length} ${item.bindings.length === 1 ? "binding" : "bindings"}` : "No bindings"
                        : "Bindings unavailable"}</span>
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}

function InterfaceDescriptorOverview({ item, onOpenFiles }: { item: InterfaceCatalogItem; onOpenFiles: () => void }) {
  const record = item.record;
  const description = interfaceDescription(item);
  const handIn = interfaceArtifactHandInFact(record);
  return (
    <div className="mb-4 flex flex-col gap-4">
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-xl border border-token-border text-token-text-secondary">
          <Glyph name={interfaceGlyph(item)} size={18} />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
            <strong className="truncate text-sm font-medium text-token-foreground">{interfaceProductName(item)}</strong>
            <span className="shrink-0 rounded-full bg-token-foreground/5 px-2 py-0.5 text-[10px] text-token-text-secondary">Registered</span>
          </div>
          {description ? <p className="mt-1 text-xs leading-4 text-token-text-secondary">{description}</p> : null}
        </div>
      </div>
      <div className="overflow-hidden rounded-lg border border-token-border/70">
        <InterfaceFactRow label="Exact interface" value={item.id} mono />
        <InterfaceFactRow label="Classification" value={interfaceClassification(record)} />
        <InterfaceFactRow label="Source" value={interfaceSourceNames(record).join(", ") || "Not declared"} />
        {item.applications.length ? <InterfaceFactRow label="Applications" value={item.applications.map((application) => `${application.name} · ${registryDisplayName(application.category)}`).join(", ")} /> : null}
        <InterfaceFactRow label="Available in" value={item.availabilityProjected
          ? item.availableIn.length ? item.availableIn.join(", ") : "No Octopus OS project policy currently admits this exact id"
          : "Project availability projection unavailable"} />
        <InterfaceFactRow label="Artifact hand-in" value={handIn.detail} />
        <InterfaceFactRow label="Bindings" value={item.bindingsProjected
          ? item.bindings.length ? `${item.bindings.length} exact checkpoint ${item.bindings.length === 1 ? "binding" : "bindings"}` : "No run checkpoint currently binds this interface"
          : "Binding projection unavailable"} />
      </div>
      {item.bindings.length ? (
        <section className="flex flex-col gap-2">
          <div className="text-[10px] font-medium uppercase tracking-wide text-token-description-foreground">Exact bindings</div>
          <div className="overflow-hidden rounded-lg border border-token-border/70">
            {item.bindings.slice(0, 20).map((binding, index) => (
              <div className="border-b border-token-border/60 px-3 py-2 last:border-b-0" key={`${binding.run_id}:${binding.checkpoint_id}:${binding.direction}:${index}`}>
                <div className="flex items-center justify-between gap-2 text-[10px] text-token-text-primary">
                  <span className="truncate">{registryDisplayName(binding.direction || "binding")} · {registryDisplayName(binding.status || "projected")}</span>
                  <span className="shrink-0 font-mono text-token-description-foreground">{binding.checkpoint_id || "checkpoint"}</span>
                </div>
                <div className="mt-0.5 truncate font-mono text-[9px] text-token-description-foreground">{binding.run_id || "unknown run"}</div>
              </div>
            ))}
            {item.bindings.length > 20 ? <div className="px-3 py-2 text-[9px] text-token-description-foreground">+{item.bindings.length - 20} more bindings</div> : null}
          </div>
        </section>
      ) : null}
      <OcButton className="self-start" onClick={onOpenFiles} size="sm" variant="secondary">Open in Files</OcButton>
    </div>
  );
}

function InterfaceFactRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="grid grid-cols-[104px_minmax(0,1fr)] gap-3 border-b border-token-border/60 px-3 py-2 text-[10px] last:border-b-0">
      <span className="text-token-description-foreground">{label}</span>
      <span className={`min-w-0 break-words text-token-text-primary ${mono ? "font-mono" : ""}`}>{value}</span>
    </div>
  );
}

function RegistryDirectoryColumn({ activePath, frame, index, onFileContextMenu, onSelect, query }: {
  activePath?: string;
  frame: RegistryFrame;
  index: number;
  onFileContextMenu: (entry: OctopusOsFileEntry, x: number, y: number) => void;
  onSelect: (entry: OctopusOsFileEntry) => void;
  query: string;
}) {
  const transport = window.octopusTransport;
  const [entries, setEntries] = useState<OctopusOsFileEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [localFilter, setLocalFilter] = useState("");
  useAppErrorToast(error);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError("");
    setEntries([]);
    if (!transport?.listOsFiles) {
      setLoading(false);
      setError("The desktop Files bridge is unavailable.");
      return () => { cancelled = true; };
    }
    void transport.listOsFiles({ path: frame.path, rootId: REGISTRY_ROOT_ID })
      .then((response) => {
        if (cancelled) return;
        if (response.path !== frame.path || response.rootId !== REGISTRY_ROOT_ID) {
          throw new Error("Registry Files returned a different directory.");
        }
        setEntries(response.entries);
      })
      .catch((failure) => {
        if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [frame.path, transport]);

  const packageEntries = useMemo(() => entries.filter((entry) => entry.kind === "directory"), [entries]);
  const filteredEntries = useMemo(() => {
    return packageEntries.filter((entry) => registryFilterMatches(entry.name, query, localFilter));
  }, [localFilter, packageEntries, query]);
  const sortedEntries = useMemo(() => [...filteredEntries].sort((left, right) => (
    registryEntryRank(left) - registryEntryRank(right)
    || left.name.localeCompare(right.name, undefined, { numeric: true, sensitivity: "base" })
  )), [filteredEntries]);
  const root = index === 0;
  const activeEntryPath = activePath ?? frame.selectedPath;
  const activeIndex = sortedEntries.findIndex((entry) => entry.path === activeEntryPath);
  const rowHeight = root ? 52 : 42;
  const virtualRows = useFixedVirtualRange({ count: sortedEntries.length, itemSize: rowHeight });
  const [focusedIndex, setFocusedIndex] = useState(() => Math.max(0, activeIndex));

  useEffect(() => {
    setFocusedIndex(Math.max(0, activeIndex));
  }, [activeIndex, frame.path, sortedEntries.length]);

  const focusEntry = (nextIndex: number) => {
    const target = Math.max(0, Math.min(sortedEntries.length - 1, nextIndex));
    setFocusedIndex(target);
    const viewport = virtualRows.scrollRef.current;
    if (!viewport) return;
    const top = target * rowHeight;
    const bottom = top + rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    window.requestAnimationFrame(() => {
      viewport.querySelector<HTMLButtonElement>(`[data-registry-row-index="${target}"]`)?.focus();
    });
  };

  const onRowKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, rowIndex: number) => {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    const page = Math.max(1, Math.floor(virtualRows.viewportHeight / rowHeight));
    const target = event.key === "ArrowDown" ? rowIndex + 1
      : event.key === "ArrowUp" ? rowIndex - 1
        : event.key === "PageDown" ? rowIndex + page
          : event.key === "PageUp" ? rowIndex - page
            : event.key === "Home" ? 0
              : event.key === "End" ? sortedEntries.length - 1
                : null;
    if (target === null) return;
    event.preventDefault();
    focusEntry(target);
  };
  const title = root ? "Registry" : registryDisplayName(frame.path.split("/").at(-1) ?? frame.path);
  const meta = loading
    ? "loading"
    : error
      ? "unavailable"
      : root
        ? `${sortedEntries.length} sections`
        : `${sortedEntries.length} ${sortedEntries.length === 1 ? "item" : "items"}`;

  return (
    <section className={`rg-col${root ? " rg-col-root" : ""}`}>
      <header className="rg-col-head">
        {root ? (
          <div className="rg-col-meta">{meta}</div>
        ) : (
          <>
            <div>
              <div className="rg-col-title">{title}</div>
              <div className="rg-col-meta">{meta}</div>
            </div>
            <input
              aria-label={`Filter ${title}`}
              className="rg-col-filter"
              onChange={(event) => setLocalFilter(event.currentTarget.value)}
              placeholder="filter"
              spellCheck={false}
              value={localFilter}
            />
          </>
        )}
      </header>
      <div className="rg-col-body" ref={virtualRows.setScrollRef} onScroll={virtualRows.onScroll}>
        {error ? <div className="rg-error">{error}</div> : null}
        {!error && !loading && sortedEntries.length === 0 ? <div className="rg-empty">No registry entries</div> : null}
        {sortedEntries.length ? (
          <div style={{ height: virtualRows.totalSize, position: "relative" }}>
            <div style={{ position: "absolute", top: virtualRows.start * rowHeight, width: "100%" }}>
              {sortedEntries.slice(virtualRows.start, virtualRows.end).map((entry, offset) => {
                const rowIndex = virtualRows.start + offset;
                const active = activeEntryPath === entry.path;
                if (root) {
                  return (
                    <button
                      className={`rg-root-row${active ? " is-active" : ""}`}
                      data-registry-row-index={rowIndex}
                      key={entry.path}
                      onClick={() => onSelect(entry)}
                      onFocus={() => setFocusedIndex(rowIndex)}
                      onKeyDown={(event) => onRowKeyDown(event, rowIndex)}
                      style={{ height: rowHeight }}
                      tabIndex={focusedIndex === rowIndex ? 0 : -1}
                      title={entry.path}
                      type="button"
                    >
                      <span className="rg-root-row-label">{registryDisplayName(entry.name)}</span>
                      <span className="rg-root-row-sub">{registrySectionDescription(entry.name)}</span>
                    </button>
                  );
                }
                return (
                  <button
                    className={`rg-row${active ? " is-active" : ""}`}
                    data-registry-row-index={rowIndex}
                    key={entry.path}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                      onFileContextMenu(entry, event.clientX, event.clientY);
                    }}
                    onClick={() => onSelect(entry)}
                    onFocus={() => setFocusedIndex(rowIndex)}
                    onKeyDown={(event) => onRowKeyDown(event, rowIndex)}
                    style={{ height: rowHeight }}
                    tabIndex={focusedIndex === rowIndex ? 0 : -1}
                    title={entry.path}
                    type="button"
                  >
                    <strong>{entry.name}</strong>
                  </button>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}

function buildInterfaceCatalog(
  interfacesValue: unknown,
  bindingsValue: unknown,
  scopeValue: unknown,
  projection: { availabilityProjected: boolean; bindingsProjected: boolean },
): InterfaceCatalogItem[] {
  const interfaceRows = objectValue(interfacesValue).interfaces;
  const bindingRows = objectValue(bindingsValue).bindings;
  const scope = objectValue(scopeValue);
  const records = arrayValue(interfaceRows).flatMap((value): InterfaceCatalogRecord[] => {
    const record = objectValue(value);
    return Object.keys(record).length ? [record as InterfaceCatalogRecord] : [];
  });
  const bindings = arrayValue(bindingRows).flatMap((value): InterfaceBindingRecord[] => {
    const record = objectValue(value);
    return Object.keys(record).length ? [record as InterfaceBindingRecord] : [];
  });
  const applicationsByInterface = new Map<string, InterfaceApplication[]>();
  for (const record of records) {
    const manifest = objectValue(record.application_manifest);
    const application: InterfaceApplication = {
      id: stringValue(manifest.id),
      name: stringValue(manifest.name),
      category: stringValue(manifest.category),
    };
    if (!application.id || !application.name) continue;
    for (const interfaceId of arrayValue(manifest.interfaces).map(stringValue).filter(Boolean)) {
      const current = applicationsByInterface.get(interfaceId) ?? [];
      if (!current.some((candidate) => candidate.id === application.id)) current.push(application);
      applicationsByInterface.set(interfaceId, current);
    }
  }
  const bindingsByInterface = new Map<string, InterfaceBindingRecord[]>();
  for (const binding of bindings) {
    const interfaceId = stringValue(binding.interface_id);
    if (!interfaceId) continue;
    const current = bindingsByInterface.get(interfaceId) ?? [];
    current.push(binding);
    bindingsByInterface.set(interfaceId, current);
  }
  const rootEeId = stringValue(scope.root_ee_id);
  const projectNames = new Map(arrayValue(scope.project_presentations).flatMap((value): Array<[string, string]> => {
    const presentation = objectValue(value);
    const eeId = stringValue(presentation.ee_id);
    const name = stringValue(presentation.name);
    return eeId && name ? [[eeId, name]] : [];
  }));
  const environments = arrayValue(scope.environments).map(objectValue).filter((value) => Object.keys(value).length);
  const availableProjectsFor = (interfaceId: string) => environments.flatMap((environment): string[] => {
    const eeId = stringValue(environment.ee_id);
    if (!eeId) return [];
    const allowed = objectValue(environment.formed_envelope).allowed_interfaces;
    if (Array.isArray(allowed) && !allowed.map(stringValue).includes(interfaceId)) return [];
    if (eeId === rootEeId) return ["Home"];
    return [projectNames.get(eeId) || `Project ${compactIdentity(eeId)}`];
  });
  return records.flatMap((record): InterfaceCatalogItem[] => {
    const id = stringValue(record.rule_id ?? record.id);
    if (!id) return [];
    const declaredPath = stringValue(record.source_file);
    const path = declaredPath.startsWith("registry/interface/")
      ? declaredPath
      : `registry/interface/${id}/interface.json`;
    return [{
      id,
      path,
      record,
      bindings: [...(bindingsByInterface.get(id) ?? [])].sort((left, right) => (
        stringValue(left.run_id).localeCompare(stringValue(right.run_id))
        || stringValue(left.checkpoint_id).localeCompare(stringValue(right.checkpoint_id))
        || stringValue(left.direction).localeCompare(stringValue(right.direction))
      )),
      bindingsProjected: projection.bindingsProjected,
      applications: applicationsByInterface.get(id) ?? [],
      availableIn: Array.from(new Set(availableProjectsFor(id))),
      availabilityProjected: projection.availabilityProjected,
    }];
  }).sort((left, right) => (
    interfaceProductName(left).localeCompare(interfaceProductName(right), undefined, { sensitivity: "base" })
    || left.id.localeCompare(right.id)
  ));
}

function interfaceProductName(item: InterfaceCatalogItem): string {
  if (item.applications.length === 1 && item.applications[0].name) return item.applications[0].name;
  const source = interfaceSourceNames(item.record)[0];
  return source ? registryDisplayName(source) : registryDisplayName(item.id);
}

function interfaceDescription(item: InterfaceCatalogItem): string {
  const declared = stringValue(item.record.description);
  if (declared) return declared;
  const manifest = objectValue(item.record.application_manifest);
  return stringValue(manifest.description);
}

function interfaceSourceNames(record: InterfaceCatalogRecord): string[] {
  return arrayValue(record.source_names).map(stringValue).filter(Boolean);
}

function interfaceClassification(record: InterfaceCatalogRecord): string {
  return [
    stringValue(record.interface_type),
    stringValue(record.role),
    stringValue(record.direction),
    stringValue(record.mode),
  ].filter(Boolean).map(registryDisplayName).join(" · ") || "Interface";
}

function interfaceGlyph(item: InterfaceCatalogItem): Parameters<typeof Glyph>[0]["name"] {
  const role = stringValue(item.record.role).toLocaleLowerCase();
  const type = stringValue(item.record.interface_type).toLocaleLowerCase();
  if (role === "worker") return "ee";
  if (role === "provider" || type === "world") return "globe";
  if (item.applications.some((application) => application.category === "plugin")) return "plugins";
  return "interfaces";
}

function interfaceArtifactHandInFact(record: InterfaceCatalogRecord): {
  detail: string;
  label: string;
  tone: "ready" | "error" | "muted";
} {
  const status = stringValue(record.artifact_transfer_protocol_status).toLocaleLowerCase();
  if (!status) {
    return {
      detail: "No artifact transfer readiness fact is projected for this interface",
      label: "Not projected",
      tone: "muted",
    };
  }
  const readyTargets = numberValue(record.artifact_transfer_ready_targets);
  const targetCount = numberValue(record.artifact_transfer_target_count);
  if (status === "ready") {
    const count = readyTargets || targetCount;
    return {
      detail: count ? `Ready · ${count} ${count === 1 ? "target" : "targets"}` : "Ready",
      label: count ? `${count} ready ${count === 1 ? "target" : "targets"}` : "Ready",
      tone: "ready",
    };
  }
  if (status === "not_declared") {
    return {
      detail: "Artifact hand-in is not declared for this interface",
      label: "Not declared",
      tone: "muted",
    };
  }
  const reason = stringValue(record.artifact_transfer_target_reason);
  const label = registryDisplayName(status);
  return {
    detail: `${label}${reason ? ` · ${reason}` : ""}`,
    label,
    tone: ["failed", "error", "unavailable", "blocked", "denied"].includes(status) ? "error" : "muted",
  };
}

function interfaceSearchText(item: InterfaceCatalogItem): string {
  return [
    item.id,
    interfaceProductName(item),
    interfaceDescription(item),
    interfaceClassification(item.record),
    interfaceArtifactHandInFact(item.record).detail,
    ...interfaceSourceNames(item.record),
    ...item.applications.flatMap((application) => [application.id, application.name, application.category]),
    ...item.availableIn,
    ...item.bindings.flatMap((binding) => [binding.run_id, binding.ee_id, binding.checkpoint_id, binding.direction, binding.status]),
  ].filter(Boolean).join(" ").toLocaleLowerCase();
}

function objectValue(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function numberValue(value: unknown): number {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}

function compactIdentity(value: string): string {
  return value.length <= 18 ? value : `${value.slice(0, 11)}…${value.slice(-5)}`;
}

function registryEntryRank(entry: OctopusOsFileEntry): number {
  return entry.kind === "directory" ? 0 : entry.kind === "file" ? 1 : 2;
}

function registryDescriptorPath(sectionPath: string, entry: OctopusOsFileEntry): string {
  if (entry.kind === "file") return entry.path;
  const descriptorKind = sectionPath.split("/").at(-1) ?? "";
  return `${entry.path}/${descriptorKind}.json`;
}

function registryDisplayName(value: string): string {
  const withoutExtension = value.replace(/\.(?:json|md)$/i, "");
  const words = withoutExtension.replace(/[_-]+/g, " ").trim();
  return words ? `${words[0].toUpperCase()}${words.slice(1)}` : value;
}

function registryEntryMeta(entry: OctopusOsFileEntry): string {
  if (entry.kind === "directory") return "Folder";
  if (entry.kind !== "file") return entry.kind;
  const extension = entry.name.includes(".") ? entry.name.split(".").at(-1)?.toUpperCase() : "File";
  return [extension, formatBytes(entry.size)].filter(Boolean).join(" · ");
}

function registrySectionDescription(sectionName: string): string {
  if (sectionName === "ee_template") return "Execution environment templates";
  if (sectionName === "worker_environment_kind") return "Worker environment drivers";
  if (sectionName === "interface") return "Ways Octopus receives, sends, thinks, and acts";
  return `${registryDisplayName(sectionName)} packages`;
}

function registryFileMeta(file: OctopusOsFile | null): string {
  if (!file) return "unavailable";
  const extension = file.name.includes(".") ? file.name.split(".").at(-1)?.toUpperCase() : "File";
  return [file.kind === "text" ? extension : "Binary", formatBytes(file.size)].filter(Boolean).join(" · ");
}

function formatRegistryFile(file: OctopusOsFile | null): string {
  if (!file || file.kind !== "text") return "";
  try {
    return JSON.stringify(JSON.parse(file.text), null, 2);
  } catch {
    return file.text;
  }
}

function formatBytes(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "";
  if (value < 1_024) return `${value} B`;
  if (value < 1_048_576) return `${Math.round(value / 1_024)} KB`;
  return `${(value / 1_048_576).toFixed(1)} MB`;
}

function RegistryPartialFailure({ detail, onRetry, retrying, title }: {
  detail: string;
  onRetry: () => void;
  retrying: boolean;
  title: string;
}) {
  return (
    <div className="m-3 flex flex-wrap items-start justify-between gap-3 rounded-lg border border-token-border bg-token-main-surface-secondary p-3" role="alert">
      <div className="min-w-0 flex-1">
        <div className="text-xs font-medium text-token-foreground">{title}</div>
        <div className="mt-1 break-words text-[10px] leading-4 text-token-text-secondary">{detail}</div>
      </div>
      <OcButton disabled={retrying} onClick={onRetry} size="xs" variant="secondary">
        {retrying ? "Retrying…" : "Retry"}
      </OcButton>
    </div>
  );
}

function RegistryFailureState({ detail, onRetry, retrying, title }: {
  detail: string;
  onRetry: () => void;
  retrying: boolean;
  title: string;
}) {
  return (
    <div className="flex min-h-40 flex-col items-center justify-center gap-3 px-5 py-8 text-center" role="alert">
      <Glyph className="text-token-input-placeholder-foreground" name="warning" size={22} />
      <div className="flex max-w-80 flex-col gap-1">
        <strong className="text-xs font-medium text-token-foreground">{title}</strong>
        <span className="break-words text-[10px] leading-4 text-token-text-secondary">{detail}</span>
      </div>
      <OcButton disabled={retrying} leadingIcon="retry" onClick={onRetry} size="sm" variant="secondary">
        {retrying ? "Retrying…" : "Retry"}
      </OcButton>
    </div>
  );
}

function projectionFailureMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function RuntimeEmpty({ title, detail }: { title: string; detail: string }) {
  return (
    <section className="workspace-empty">
      <Glyph name="registry" size={28} />
      <strong>{title}</strong>
      <span>{detail}</span>
    </section>
  );
}

function registryFilterMatches(text: string, query: string, localFilter: string): boolean {
  const searchable = text.toLocaleLowerCase();
  return [query, localFilter].every((value) => searchable.includes(value.trim().toLocaleLowerCase()));
}
