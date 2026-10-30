import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import "../icon-glyphs/chat";
import "../icon-glyphs/settings";
import "../icon-glyphs/voice";
import "../icon-glyphs/worker";
import {
  normalizeSettingsSection,
  useClientPreferences,
  useSetClientPreferences,
} from "../../clientPreferences";
import { Glyph } from "../IconGlyphs";
import { OcSelect } from "../../design-system/components";
import { restoreModalFocus } from "../../design-system/modalFocus";
import { clientDialogOwnsCommands } from "../../keyboardShortcuts";
import type { ClientExtensionRegistryState } from "../../extensions/types";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { AdmittedClientSurfaceDocument, ClientSurfaceAdmissionVerdict } from "../../client-surfaces/types";
import type { OctopusClientAccessProfile } from "../../electron";
import type { ClientExtensionDestination } from "./ExtensionsSection";
import { SettingsSectionContent } from "./SettingsSectionContent";
import {
  SETTINGS_SECTION_GROUPS,
  availableSettingsSections,
  searchSettings,
  settingsTargetDomId,
  type SettingsSearchResult,
  type SettingsSectionId,
} from "./settingsCatalog";
import "./settings.css";

export function SettingsPage({
  accessProfile,
  documentRoot,
  documentVerdicts,
  documents,
  extensions,
  onClose,
  onOpenExtensionDestination,
  onSectionChange,
  sectionId,
  recordingId = "",
  sidebarWidth,
  surfaceErrors,
  surfaces,
}: {
  accessProfile: OctopusClientAccessProfile;
  documentRoot: string;
  documentVerdicts: ClientSurfaceAdmissionVerdict[];
  documents: AdmittedClientSurfaceDocument[];
  extensions: ClientExtensionRegistryState;
  onClose: () => void;
  onOpenExtensionDestination: (destination: ClientExtensionDestination) => void;
  onSectionChange: (sectionId: string) => void;
  sectionId: string;
  recordingId?: string;
  sidebarWidth: number;
  surfaceErrors: string[];
  surfaces: ClientSurfaceDescriptor[];
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const mainRef = useRef<HTMLElement>(null);
  const searchResultRefs = useRef(new Map<number, HTMLButtonElement>());
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [highlightedSearchIndex, setHighlightedSearchIndex] = useState(-1);
  const [pendingDestinationId, setPendingDestinationId] = useState("");
  const [selectedRecordingId, setSelectedRecordingId] = useState(recordingId);
  useEffect(() => {
    setSelectedRecordingId(recordingId);
    if (recordingId) setPendingDestinationId("voice.recordings");
  }, [recordingId]);
  const storedSection = normalizeSettingsSection(sectionId) as SettingsSectionId;
  const query = preferences.settingsSearch.trim();
  const visibleSections = useMemo(() => availableSettingsSections(accessProfile), [accessProfile]);
  const searchResults = useMemo(() => searchSettings(query, accessProfile), [accessProfile, query]);
  const activeSection = visibleSections.some((section) => section.id === storedSection)
    ? storedSection
    : visibleSections[0]?.id;

  function closeSettings() {
    setClientPreferences({ settingsSearch: "" });
    onClose();
  }

  useEffect(() => {
    return () => {
      restoreModalFocus(previousFocusRef.current);
    };
  }, []);

  useEffect(() => {
    mainRef.current?.scrollTo({ top: 0, behavior: "instant" });
  }, [activeSection]);

  useLayoutEffect(() => {
    if (!pendingDestinationId || !activeSection) return;
    const frame = window.requestAnimationFrame(() => {
      const target = mainRef.current?.querySelector<HTMLElement>(`#${settingsTargetDomId(pendingDestinationId)}`)
        ?? mainRef.current?.querySelector<HTMLElement>("[data-settings-section]")
        ?? null;
      if (!target || !mainRef.current?.contains(target)) return;
      target.scrollIntoView({ block: "center", inline: "nearest" });
      target.focus({ preventScroll: true });
      setPendingDestinationId("");
    });
    return () => window.cancelAnimationFrame(frame);
  }, [activeSection, pendingDestinationId]);

  useEffect(() => {
    setHighlightedSearchIndex(-1);
  }, [query]);

  function selectSearchResult(result: SettingsSearchResult) {
    setPendingDestinationId(result.destination.targetId ?? result.destination.id);
    setClientPreferences({ settingsSearch: "" });
    onSectionChange(result.section.id);
  }

  function onSearchKeyDown(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (!query || searchResults.length === 0) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = highlightedSearchIndex < 0
        ? (event.key === "ArrowDown" ? 0 : searchResults.length - 1)
        : (highlightedSearchIndex + (event.key === "ArrowDown" ? 1 : -1) + searchResults.length) % searchResults.length;
      setHighlightedSearchIndex(next);
      searchResultRefs.current.get(next)?.scrollIntoView({ block: "nearest" });
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      selectSearchResult(searchResults[Math.max(0, highlightedSearchIndex)]);
    }
  }

  function onSectionKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>, sectionId: SettingsSectionId) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const current = visibleSections.findIndex((section) => section.id === sectionId);
    const nextIndex = (current + (event.key === "ArrowDown" ? 1 : -1) + visibleSections.length) % visibleSections.length;
    const next = visibleSections[nextIndex];
    onSectionChange(next.id);
    document.getElementById(`settings-section-nav-${next.id}`)?.focus();
  }

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || clientDialogOwnsCommands() || event.target instanceof Element && event.target.closest('[data-settings-layer="true"]')) return;
      if (document.querySelector('[data-shortcut-capture="true"]')) return;
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        event.stopPropagation();
        document.getElementById("settings-search")?.focus();
        return;
      }
      if (event.key !== "Escape") return;
      if (document.querySelector('[data-settings-layer="true"]')) return;
      event.preventDefault();
      event.stopPropagation();
      if (preferences.settingsSearch) {
        setClientPreferences({ settingsSearch: "" });
      } else {
        closeSettings();
      }
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose, preferences.settingsSearch]);

  return (
    <div
      aria-label="Settings"
      className="settings-page flex h-full min-h-0"
      ref={dialogRef}
      role="region"
      tabIndex={-1}
    >
      <aside
        className={`settings-rail app-shell-left-panel relative flex min-h-0 w-token-sidebar shrink-0 flex-col overflow-hidden${query ? " is-searching" : ""}`}
        style={{ "--settings-sidebar-width": `${sidebarWidth}px` } as CSSProperties}
      >
        <nav className="flex min-h-0 flex-1 flex-col px-row-x select-none" aria-label="Settings sections">
          <div className="flex min-h-0 flex-1 flex-col">
          <div className="mb-4 flex shrink-0 items-center gap-2">
          <div className="settings-search sidebar-item no-drag flex h-[var(--height-token-row)] min-w-0 flex-1 items-center bg-background-secondary-soft-alpha py-0 pe-[var(--padding-row-cell-x,var(--padding-row-x))] text-base leading-[18px]">
            <div className="flex shrink-0 items-center ps-1 pe-1">
              <span className="icon-leading-slot -mx-[3px] flex size-[var(--height-token-row)] shrink-0 items-center justify-center" role="presentation">
                <Glyph className="icon-sm text-secondary" name="search" size={16} />
              </span>
            </div>
            <label className="sr-only" htmlFor="settings-search">Search settings</label>
            <input
              aria-activedescendant={highlightedSearchIndex >= 0 ? `settings-search-result-${highlightedSearchIndex}` : undefined}
              aria-controls="settings-search-results"
              aria-label="Search settings"
              autoComplete="off"
              autoFocus
              className="min-w-0 flex-1 bg-transparent text-base leading-[18px] text-default outline-none select-text placeholder:text-tertiary [&::placeholder]:select-none"
              id="settings-search"
              onChange={(event) => setClientPreferences({ settingsSearch: event.currentTarget.value })}
              onKeyDown={onSearchKeyDown}
              placeholder="Search settings…"
              role="searchbox"
              type="text"
              value={preferences.settingsSearch}
            />
            {preferences.settingsSearch ? (
              <button
                aria-label="Clear settings search"
                className="flex size-6 shrink-0 cursor-interaction items-center justify-center text-secondary hover:text-default"
                onClick={() => setClientPreferences({ settingsSearch: "" })}
                type="button"
              >
                <Glyph className="icon-sm" name="close" size={16} />
              </button>
            ) : null}
          </div>
          </div>

          <div className="settings-rail-nav min-h-0 flex-1 overflow-y-auto pb-2">
            {query ? (
              <div className="flex flex-col gap-3 pt-1" id="settings-search-results" role="listbox">
                {searchResults.map((result, index) => (
                  <div className="flex flex-col gap-0.5" key={result.destination.id}>
                    <div className="flex items-center gap-2 px-row-x py-1 text-base text-secondary">
                      <Glyph className="icon-sm shrink-0" name={result.section.glyph} size={16} />
                      <span className="truncate">{result.section.label}</span>
                    </div>
                    <button
                      aria-label={`${result.destination.label}, ${result.section.label}`}
                      aria-selected={highlightedSearchIndex === index}
                      className={`sidebar-item flex cursor-interaction items-center py-1.5 pe-2 ps-8.5 text-start text-base text-default hover:bg-primary-ghost-hover focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 focus-visible:ring-inset focus-visible:outline-none ${highlightedSearchIndex === index ? "bg-primary-ghost-hover" : ""}`}
                      id={`settings-search-result-${index}`}
                      onClick={() => selectSearchResult(result)}
                      onFocus={() => setHighlightedSearchIndex(index)}
                      onPointerEnter={() => setHighlightedSearchIndex(index)}
                      ref={(node) => {
                        if (node) searchResultRefs.current.set(index, node);
                        else searchResultRefs.current.delete(index);
                      }}
                      role="option"
                      type="button"
                    >
                      <span className="truncate">{result.destination.label}</span>
                    </button>
                  </div>
                ))}
                {searchResults.length === 0 ? (
                  <div className="px-row-x pt-2 text-sm text-secondary">No results found</div>
                ) : null}
              </div>
            ) : SETTINGS_SECTION_GROUPS.map((group) => {
              const items = visibleSections.filter((section) => section.group === group);
              if (items.length === 0) return null;
              return (
                <div className="settings-rail-group flex flex-col gap-1" key={group}>
                  <div className="settings-rail-group-title group/nav-section-title flex items-center justify-between gap-2 pr-0.5 pl-2">
                    <div className="min-w-0 flex-1 text-base font-medium text-token-input-placeholder-foreground opacity-75">{group}</div>
                  </div>
                  <div className="flex flex-col gap-px">
                    {items.map((item) => (
                      <button
                        aria-current={activeSection === item.id ? "page" : undefined}
                        className="settings-rail-item sidebar-item relative flex h-[var(--height-token-row)] w-full shrink-0 cursor-interaction items-center gap-2 overflow-hidden px-[var(--padding-row-cell-x,var(--padding-row-x))] py-row-y text-left text-sm hover:bg-primary-ghost-hover focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-[-2px]"
                        data-active={activeSection === item.id}
                        id={`settings-section-nav-${item.id}`}
                        key={item.id}
                        onClick={() => onSectionChange(item.id)}
                        onKeyDown={(event) => onSectionKeyDown(event, item.id)}
                        type="button"
                      >
                        <span className="icon-leading-slot flex w-4 shrink-0 items-center justify-center">
                          <Glyph className="icon-sm inline-block align-middle" name={item.glyph} size={16} />
                        </span>
                        <span className="text-fade-truncate text-base font-normal">{item.label}</span>
                      </button>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
          </div>
        </nav>
      </aside>

      <div className="relative isolate min-h-0 min-w-0 flex-1 overflow-visible">
        <div className="main-surface flex h-full min-h-0 flex-col">
          <div className="settings-compact-navigation">
            <OcSelect label="Settings section" value={activeSection ?? ""} options={visibleSections.map((section) => ({ value: section.id, label: section.label }))} onChange={(section) => { setClientPreferences({ settingsSearch: "" }); onSectionChange(section); }} />
          </div>
          <main className="settings-main scrollbar-stable flex-1 overflow-y-auto p-panel" ref={mainRef}>
            <div className="settings-content mx-auto flex w-full min-w-0 max-w-3xl flex-col" data-settings-section={activeSection} tabIndex={-1}>
              {activeSection ? (
                <SettingsSectionContent
                  accessProfile={accessProfile}
                  documentRoot={documentRoot}
                  documentVerdicts={documentVerdicts}
                  documents={documents}
                  extensions={extensions}
                  onOpenExtensionDestination={onOpenExtensionDestination}
                  sectionId={activeSection}
                  dictationRecordingId={selectedRecordingId}
                  onShowAllRecordings={() => setSelectedRecordingId("")}
                  surfaceErrors={surfaceErrors}
                  surfaces={surfaces}
                />
              ) : null}
              {!activeSection ? (
                <section className="settings-section settings-empty-section">
                  <Glyph name="search" size={22} />
                  <h1>No matching settings</h1>
                  <p>Clear the search or use a different term.</p>
                </section>
              ) : null}
            </div>
          </main>
        </div>
      </div>
    </div>
  );
}
