import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import "./icon-glyphs/chat";
import "./icon-glyphs/worker";
import type { OctopusApplicationLifecycleProgress, OctopusClientAccessProfile, OctopusProviderInterface, OctopusOsWorkspaceMutationRequest } from "../electron";
import {
  formatProjectionError,
  hasProjectionBridge,
  mutateOctopusOsWorkspace,
  onApplicationLifecycleProgress,
  readProjection,
  readProviderInterfaces,
} from "../projectionClient";
import { useProjectionStructureGeneration } from "../projectionLive";
import {
  OcAnimatedActionSlot,
  OcButton,
  OcDialogFooter,
  OcEmptyState,
  OcIconButton,
  OcInlineProgress,
  OcSearchField,
  OcSelect,
  OcStatus,
  OcSwitch,
  OcTextField,
  cx,
} from "../design-system/components";
import {
  CatalogInstalledRail,
  CatalogRouteTransition,
  CatalogSection,
} from "./catalog";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { OctopusLoadingState } from "./OctopusLoadingLogo";
import { SetupDialogShell } from "./SetupDialogShell";
import { useAppErrorToast } from "../toasts";

type DriverProjectState = {
  ee_id: string;
  is_root: boolean;
  policy_allowed: boolean;
  ceiling_allowed: boolean;
  selected_by_default: boolean;
  record_digest: string;
  host_slots: Record<string, string>;
};

type DriverAgentPreset = {
  id: string; name: string; pack_id: string; pack_manifest_digest: string;
  provider_interface_id: string; provider_interface_ids: string[]; project_ee_ids: string[];
};

type AgentPresetScope = { status: string; root_ee_id: string; home_run_id: string; home_interface_id: string; home_sidecar_digest: string; agent_catalog_digest: string };

type DriverRecord = {
  install_packages?: Array<{ id: string; name: string; source_digest: string; installed: boolean; receipt_state: string }>;
  agent_presets?: DriverAgentPreset[];
  name: string;
  display_name?: string;
  description?: string;
  icon?: string;
  package_id?: string;
  installed: boolean;
  materialized?: boolean;
  partially_materialized?: boolean;
  source_available?: boolean;
  source_digest?: string;
  installed_digest?: string;
  source_file_count?: number;
  receipt_state?: string;
  install_block_reason?: string;
  removal_allowed?: boolean;
  removal_block_reason?: string;
  substrate_family: string;
  worker_consent: string;
  capabilities: string[];
  supported_executor_kinds: string[];
  instance_config_files: string[];
  door_interfaces: string[];
  door_driver_refs: string[];
  tool_ids: string[];
  boundary_defaults: {
    read_root_refs?: Array<{ path?: string; access?: string[] }>;
    write_root_refs?: Array<{ path?: string; access?: string[] }>;
    protected_read_only_subpaths?: Array<{ path?: string; access?: string[] }>;
    network_ceiling?: string;
  };
  lifecycle: { requires_release?: boolean };
  descriptor_path: string;
  active_instances: Array<Record<string, unknown>>;
  project_states: DriverProjectState[];
};

type DriverProject = {
  ee_id: string;
  name: string;
  icon: string;
  color: string;
  goal: string;
  parent_ee_id: string;
  record_digest: string;
};

type DriversProjection = {
  octopus_home?: string;
  status?: string;
  root_ee_id?: string;
  host_root_refs?: Record<string, string>;
  host_manifest_ready?: boolean;
  drivers?: DriverRecord[];
  projects?: DriverProject[];
  errors?: unknown[];
};

type DriverSetupDraft = Record<string, {
  available: boolean;
  selected: boolean;
  hostSlots: Record<string, string>;
}>;

type DriverCatalogView = "browse" | "manage";

type DriverLifecycleOperation = {
  operationId: string;
  operation: "install" | "remove";
  phase: OctopusApplicationLifecycleProgress["phase"] | "failed";
  percent: number;
  error: string;
};

export function DriversView({
  accessProfile,
  searchFocusTick,
}: {
  accessProfile: OctopusClientAccessProfile;
  searchFocusTick: number;
}) {
  const generation = useProjectionStructureGeneration();
  const browseSearchRef = useRef<HTMLInputElement>(null);
  const manageSearchRef = useRef<HTMLInputElement>(null);
  const browseScrollRef = useRef<HTMLDivElement>(null);
  const manageScrollRef = useRef<HTMLDivElement>(null);
  const browseSurfaceRef = useRef<HTMLElement>(null);
  const detailBackRef = useRef<HTMLButtonElement>(null);
  const detailOriginRef = useRef<{
    element: HTMLElement | null;
    kind: "card" | "rail" | "search";
    name: string;
    view: DriverCatalogView;
  } | null>(null);
  const scrollOffsetsRef = useRef<Record<DriverCatalogView, number>>({ browse: 0, manage: 0 });
  const browseCardRefs = useRef(new Map<string, HTMLDivElement>());
  const manageCardRefs = useRef(new Map<string, HTMLDivElement>());
  const [home, setHome] = useState("");
  const [installReview, setInstallReview] = useState<DriverRecord | null>(null);
  const [addingPreset, setAddingPreset] = useState<{ preset: DriverAgentPreset; scope: AgentPresetScope; home: string; operationId: string } | null>(null);
  const [presetProviders, setPresetProviders] = useState<OctopusProviderInterface[]>([]);
  const [presetProject, setPresetProject] = useState("");
  const [presetProvider, setPresetProvider] = useState("");
  const [presetEffort, setPresetEffort] = useState("");
  const [presetSaving, setPresetSaving] = useState(false);
  const [drivers, setDrivers] = useState<DriverRecord[]>([]);
  const [projects, setProjects] = useState<DriverProject[]>([]);
  const [rootEeId, setRootEeId] = useState("");
  const [hostRootRefs, setHostRootRefs] = useState<Record<string, string>>({});
  const [hostManifestReady, setHostManifestReady] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const [browseQuery, setBrowseQuery] = useState("");
  const [manageQuery, setManageQuery] = useState("");
  const [catalogView, setCatalogView] = useState<DriverCatalogView>("browse");
  const [detailReturnView, setDetailReturnView] = useState<DriverCatalogView>("browse");
  const [routeDirection, setRouteDirection] = useState<"back" | "forward">("forward");
  const [selectedName, setSelectedName] = useState("");
  const [highlightedName, setHighlightedName] = useState("");
  const [setupName, setSetupName] = useState("");
  const [setupDraft, setSetupDraft] = useState<DriverSetupDraft>({});
  const [selectingFolder, setSelectingFolder] = useState("");
  const [saving, setSaving] = useState(false);
  const [reloadTick, setReloadTick] = useState(0);
  const [removeName, setRemoveName] = useState("");
  const [lifecycleOperations, setLifecycleOperations] = useState<Record<string, DriverLifecycleOperation>>({});
  useAppErrorToast(error);

  useEffect(() => {
    if (!hasProjectionBridge()) {
      setLoaded(true);
      return;
    }
    let cancelled = false;
    setLoaded(false);
    readProjection<DriversProjection>("projection.operator.os.drivers")
      .then((response) => {
        if (cancelled) return;
        setDrivers(arrayRecords(response.drivers));
        setHome(typeof response.octopus_home === "string" ? response.octopus_home : "");
        setProjects(arrayRecords(response.projects));
        setRootEeId(typeof response.root_ee_id === "string" ? response.root_ee_id : "");
        setHostRootRefs(cleanStringRecord(response.host_root_refs ?? {}));
        setHostManifestReady(response.host_manifest_ready === true);
        setError(arrayStrings(response.errors).join("\n"));
        setLoaded(true);
      })
      .catch((failure) => {
        if (cancelled) return;
        setError(formatProjectionError(failure));
        setLoaded(true);
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => onApplicationLifecycleProgress((progress) => {
    setLifecycleOperations((current) => {
      const active = current[progress.packageId];
      if (!active || active.operationId !== progress.operationId || active.operation !== progress.operation) return current;
      return {
        ...current,
        [progress.packageId]: {
          ...active,
          phase: progress.phase,
          percent: progress.percent,
          error: "",
        },
      };
    });
  }), []);

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    setRouteDirection("back");
    setSelectedName("");
    setCatalogView("browse");
    setHighlightedName("");
    scheduleFocus(() => browseSearchRef.current?.focus());
  }, [searchFocusTick]);

  const visible = useMemo(() => {
    const needle = browseQuery.trim().toLowerCase();
    return drivers.filter((driver) => !needle || driverSearchText(driver).includes(needle));
  }, [browseQuery, drivers]);
  const installed = useMemo(() => drivers.filter((driver) => driver.installed), [drivers]);
  const visibleInstalled = useMemo(() => {
    const needle = manageQuery.trim().toLowerCase();
    return installed.filter((driver) => !needle || driverSearchText(driver).includes(needle));
  }, [installed, manageQuery]);
  const selected = drivers.find((driver) => driver.name === selectedName) ?? null;
  const setupDriver = drivers.find((driver) => driver.name === setupName) ?? null;
  const removeDriver = drivers.find((driver) => driver.name === removeName) ?? null;
  const keyboardNames = (catalogView === "manage" ? visibleInstalled : visible).map((driver) => driver.name);
  const keyboardCardRefs = catalogView === "manage" ? manageCardRefs : browseCardRefs;

  useEffect(() => {
    if (!loaded || !selectedName || selected) return;
    setSelectedName("");
    setRouteDirection("back");
    restoreScroll(detailReturnView);
    focusCatalogSearch(detailReturnView);
  }, [detailReturnView, loaded, selected, selectedName]);

  function onSearchKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const renderedNames = keyboardNames.filter((name) => keyboardCardRefs.current.get(name)?.isConnected);
    if (!renderedNames.length || !["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
    event.preventDefault();
    if (event.key === "Enter") {
      openDriver(highlightedName && renderedNames.includes(highlightedName) ? highlightedName : renderedNames[0], catalogView, "search");
      return;
    }
    moveCardFocus("", event.key === "ArrowDown" ? 1 : -1);
  }

  function onCardKeyDown(event: KeyboardEvent<HTMLButtonElement>, name: string) {
    if (["ArrowDown", "ArrowUp"].includes(event.key)) {
      event.preventDefault();
      moveCardFocus(name, event.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (event.key !== "Escape") return;
    event.preventDefault();
    setHighlightedName("");
    focusCatalogSearch(catalogView);
  }

  function moveCardFocus(name: string, direction: 1 | -1) {
    const renderedNames = keyboardNames.filter((candidate) => keyboardCardRefs.current.get(candidate)?.isConnected);
    if (!renderedNames.length) return;
    const currentIndex = name ? renderedNames.indexOf(name) : -1;
    const nextIndex = currentIndex >= 0
      ? (currentIndex + direction + renderedNames.length) % renderedNames.length
      : direction > 0 ? 0 : renderedNames.length - 1;
    const nextName = renderedNames[nextIndex];
    const card = keyboardCardRefs.current.get(nextName);
    setHighlightedName(nextName);
    card?.querySelector<HTMLButtonElement>("[data-driver-open]")?.focus({ preventScroll: true });
    card?.scrollIntoView({ block: "nearest" });
  }

  function catalogScrollRef(view: DriverCatalogView) {
    return view === "manage" ? manageScrollRef : browseScrollRef;
  }

  function catalogSearchRef(view: DriverCatalogView) {
    return view === "manage" ? manageSearchRef : browseSearchRef;
  }

  function rememberScroll(view: DriverCatalogView) {
    scrollOffsetsRef.current[view] = catalogScrollRef(view).current?.scrollTop ?? 0;
  }

  function restoreScroll(view: DriverCatalogView) {
    window.requestAnimationFrame(() => {
      const scroller = catalogScrollRef(view).current;
      if (scroller) scroller.scrollTop = scrollOffsetsRef.current[view];
    });
  }

  function focusCatalogSearch(view: DriverCatalogView) {
    scheduleFocus(() => catalogSearchRef(view).current?.focus());
  }

  function openDriver(name: string, returnView: DriverCatalogView = catalogView, originKind: "card" | "rail" | "search" = "card") {
    rememberScroll(returnView);
    detailOriginRef.current = {
      element: document.activeElement instanceof HTMLElement ? document.activeElement : null,
      kind: originKind,
      name,
      view: returnView,
    };
    setDetailReturnView(returnView);
    setRouteDirection("forward");
    setSelectedName(name);
    scheduleFocus(() => detailBackRef.current?.focus());
  }

  function closeDriver() {
    setCatalogView(detailReturnView);
    setRouteDirection("back");
    setSelectedName("");
    restoreScroll(detailReturnView);
    focusDetailOrigin();
  }

  function focusDetailOrigin() {
    const origin = detailOriginRef.current;
    detailOriginRef.current = null;
    if (!origin) {
      focusCatalogSearch(detailReturnView);
      return;
    }
    scheduleFocus(() => {
      if (origin.kind === "rail") {
        const label = titleCase(origin.name);
        if (origin.element?.isConnected && origin.element.getAttribute("aria-label") === label) {
          origin.element.focus();
          return;
        }
        const railButton = [...(browseSurfaceRef.current?.querySelectorAll<HTMLButtonElement>("button[aria-label]") ?? [])]
          .find((button) => button.getAttribute("aria-label") === label);
        if (railButton) {
          railButton.focus();
          return;
        }
      }
      if (origin.element?.isConnected) {
        origin.element.focus();
        return;
      }
      if (origin.kind === "search") {
        catalogSearchRef(origin.view).current?.focus();
        return;
      }
      const card = (origin.view === "manage" ? manageCardRefs : browseCardRefs).current.get(origin.name);
      (card?.querySelector<HTMLButtonElement>("[data-driver-open]") ?? catalogSearchRef(origin.view).current)?.focus();
    });
  }

  function openManage() {
    rememberScroll("browse");
    setHighlightedName("");
    setRouteDirection("forward");
    setCatalogView("manage");
    restoreScroll("manage");
    focusCatalogSearch("manage");
  }

  function closeManage() {
    rememberScroll("manage");
    setHighlightedName("");
    setRouteDirection("back");
    setCatalogView("browse");
    restoreScroll("browse");
    scheduleFocus(() => document.getElementById("drivers-browse-manage")?.focus());
  }

  function openSetup(driver: DriverRecord) {
    if (!driver.installed) return;
    setSetupDraft(Object.fromEntries(driver.project_states.map((state) => [
      state.ee_id,
      {
        available: state.policy_allowed,
        selected: state.selected_by_default,
        hostSlots: { ...state.host_slots },
      },
    ])));
    setSetupName(driver.name);
  }

  async function selectDriverFolder(eeId: string, slot: string) {
    if (accessProfile !== "live") {
      setSetupDraft((current) => {
        const project = current[eeId];
        if (!project) return current;
        return {
          ...current,
          [eeId]: {
            ...project,
            hostSlots: { ...project.hostSlots, [slot]: `fixture://workspace/${slot}` },
          },
        };
      });
      return;
    }
    const transport = window.octopusTransport;
    if (!transport) {
      setError("Folder selection is unavailable.");
      return;
    }
    const selectionKey = `${eeId}:${slot}`;
    setSelectingFolder(selectionKey);
    try {
      const selection = await transport.selectClientDirectory();
      if (selection.canceled) return;
      setSetupDraft((current) => {
        const project = current[eeId];
        if (!project) return current;
        return {
          ...current,
          [eeId]: {
            ...project,
            hostSlots: { ...project.hostSlots, [slot]: selection.path },
          },
        };
      });
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setSelectingFolder("");
    }
  }

  async function saveSetup() {
    if (!setupDriver) return;
    if (accessProfile !== "live") {
      setDrivers((current) => current.map((driver) => driver.name !== setupDriver.name
        ? driver
        : {
            ...driver,
            project_states: driver.project_states.map((state) => {
              const next = setupDraft[state.ee_id];
              return next ? {
                ...state,
                host_slots: { ...next.hostSlots },
                policy_allowed: next.available,
                selected_by_default: next.available && next.selected,
              } : state;
            }),
          }));
      setSetupName("");
      return;
    }
    setSaving(true);
    try {
      const changedProjects = setupDriver.project_states.flatMap((state) => {
        const next = setupDraft[state.ee_id];
        const hostSlots = cleanStringRecord(next?.hostSlots ?? {});
        if (!next || (
          next.available === state.policy_allowed
          && next.selected === state.selected_by_default
          && sameStringRecord(hostSlots, state.host_slots)
        )) return [];
        return [{
          eeId: state.ee_id,
          available: next.available,
          selectedByDefault: next.selected,
          hostSlots,
          expectedDigest: state.record_digest,
        }];
      });
      if (changedProjects.length) {
        await mutateOctopusOsWorkspace({
          action: "configure_driver",
          driverName: setupDriver.name,
          projects: changedProjects,
        });
      }
      setSetupName("");
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setSaving(false);
    }
  }

  function requestDriverInstall(driver: DriverRecord) {
    if ((driver.install_packages?.length ?? 0) > 1) setInstallReview(driver);
    else void installDriver(driver);
  }

  async function openAgentPreset(preset: DriverAgentPreset) {
    try {
      const capturedHome = home;
      const scope = await readProjection<AgentPresetScope>("projection.operator.os.scope");
      const response = await readProviderInterfaces();
      if (scope.status !== "ready" || !capturedHome) throw new Error("A configured Home is required to add an Agent.");
      const providers = response.providers.filter((provider) => preset.provider_interface_ids.includes(provider.id));
      setPresetProviders(providers);
      setPresetProject("");
      const declared = providers.find((provider) => provider.id === preset.provider_interface_id);
      setPresetProvider(declared?.id ?? "");
      setPresetEffort(declared?.reasoningEffort ?? "");
      setAddingPreset({ preset, scope, home: capturedHome, operationId: crypto.randomUUID() });
    } catch (failure) { setError(formatProjectionError(failure)); }
  }

  const selectedPresetProvider = presetProviders.find((provider) => provider.id === presetProvider);
  const selectedPresetModel = selectedPresetProvider
    ? selectedPresetProvider.models.find((model) => model.model === selectedPresetProvider.model && model.showInPicker)
    : undefined;

  async function addAgentPreset() {
    if (!addingPreset || !selectedPresetProvider || !selectedPresetModel || !presetProject) return;
    const { scope, preset } = addingPreset;
    setPresetSaving(true);
    try {
      const request: OctopusOsWorkspaceMutationRequest = {
        action: "add_agent_preset", operationId: addingPreset.operationId, home: addingPreset.home,
        workspace: { rootEeId: scope.root_ee_id, homeRunId: scope.home_run_id, homeInterfaceId: scope.home_interface_id, expectedHomeSidecarDigest: scope.home_sidecar_digest, expectedAgentCatalogDigest: scope.agent_catalog_digest },
        projectEeId: presetProject, packId: preset.pack_id, expectedPackManifestDigest: preset.pack_manifest_digest, presetId: preset.id,
        providerInterfaceId: selectedPresetProvider.id, providerModel: selectedPresetModel.model, reasoningEffort: presetEffort,
      };
      await mutateOctopusOsWorkspace(request);
      setAddingPreset(null);
      setReloadTick((value) => value + 1);
    } catch (failure) { setError(formatProjectionError(failure)); }
    finally { setPresetSaving(false); }
  }

  async function installDriver(driver: DriverRecord) {
    if (
      accessProfile !== "live"
      || driver.installed
      || driver.source_available !== true
      || !driver.source_digest
      || driver.install_block_reason
    ) return;
    const packageId = driverPackageId(driver);
    const operationId = crypto.randomUUID();
    setLifecycleOperations((current) => ({
      ...current,
      [packageId]: { operationId, operation: "install", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "install_package",
        packageIds: (driver.install_packages ?? []).map((entry) => entry.id),
        expectedSourceDigests: Object.fromEntries((driver.install_packages ?? []).map((entry) => [entry.id, entry.source_digest])),
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [packageId]: { operationId, operation: "install", phase: "complete", percent: 100, error: "" },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [packageId]: {
          operationId,
          operation: "install",
          phase: "failed",
          percent: current[packageId]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  async function removeInstalledDriver(driver: DriverRecord) {
    if (
      accessProfile !== "live"
      || !driver.installed
      || !driver.installed_digest
      || driver.removal_allowed !== true
    ) return;
    const packageId = driverPackageId(driver);
    const operationId = crypto.randomUUID();
    setRemoveName("");
    setLifecycleOperations((current) => ({
      ...current,
      [packageId]: { operationId, operation: "remove", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "remove_driver",
        driverName: driver.name,
        expectedInstalledDigest: driver.installed_digest,
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [packageId]: { operationId, operation: "remove", phase: "complete", percent: 100, error: "" },
      }));
      if (selectedName === driver.name) {
        setSelectedName("");
        setRouteDirection("back");
        restoreScroll(detailReturnView);
        focusCatalogSearch(detailReturnView);
      }
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [packageId]: {
          operationId,
          operation: "remove",
          phase: "failed",
          percent: current[packageId]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  if (!hasProjectionBridge()) return (
    <OcEmptyState className="h-full" detail="Start Octopus to browse and manage local driver packages." glyph="warning" title="Drivers are unavailable" />
  );

  return (
    <>
      <SetupDialogShell open={Boolean(installReview)} busy={false} onClose={() => setInstallReview(null)} ariaLabel="Install driver and required packages" panelClassName="max-w-md">
        <div className="flex flex-col gap-3 p-5"><h2 className="text-lg font-medium">Install driver</h2><p className="text-sm text-token-text-secondary">The following packages are required:</p>
          {installReview?.install_packages?.map((entry) => <div key={entry.id} className="text-sm">{entry.name}{entry.installed ? " · Installed" : ""}</div>)}
          <OcDialogFooter><OcButton variant="secondary" onClick={() => setInstallReview(null)}>Cancel</OcButton><OcButton onClick={() => { if (installReview) void installDriver(installReview); setInstallReview(null); }}>Install</OcButton></OcDialogFooter>
        </div>
      </SetupDialogShell>
      <SetupDialogShell open={Boolean(addingPreset)} busy={presetSaving} onClose={() => setAddingPreset(null)} ariaLabel="Add Agent preset" panelClassName="max-w-md">
        <div className="flex flex-col gap-4 p-5"><h2 className="text-lg font-medium">Add {addingPreset?.preset.name} Agent</h2>
          <OcSelect label="Project" value={presetProject} placeholder="Choose a Project" options={projects.filter((project) => addingPreset?.preset.project_ee_ids.includes(project.ee_id)).map((project) => ({ value: project.ee_id, label: project.name }))} onChange={setPresetProject} />
          <OcSelect label="Provider" value={presetProvider} placeholder="Choose a provider" options={presetProviders.map((provider) => ({ value: provider.id, label: provider.label }))} onChange={(value) => { setPresetProvider(value); setPresetEffort(presetProviders.find((provider) => provider.id === value)?.reasoningEffort ?? ""); }} />
          <p className="text-sm text-token-text-secondary">Current model: {selectedPresetModel?.displayName ?? "Unavailable"}. Each new task has its own model selection.</p>
          {selectedPresetModel?.supportedReasoningEfforts.length ? <OcSelect label="Reasoning" value={presetEffort} options={selectedPresetModel.supportedReasoningEfforts.map((effort) => ({ value: effort, label: effort }))} onChange={setPresetEffort} /> : null}
          <OcDialogFooter><OcButton variant="secondary" onClick={() => setAddingPreset(null)} disabled={presetSaving}>Cancel</OcButton><OcButton disabled={presetSaving || !presetProject || !selectedPresetModel} onClick={() => void addAgentPreset()}>Add Agent</OcButton></OcDialogFooter>
        </div>
      </SetupDialogShell>
      <div className="relative h-full min-h-0">
        <div aria-hidden={Boolean(selected) || undefined} className={cx("h-full min-h-0", selected && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={`drivers-${catalogView}`}>
            {catalogView === "manage" ? (
              <DriverManageView
                accessProfile={accessProfile}
                cardRefs={manageCardRefs}
                drivers={drivers}
                error={error}
                highlightedName={highlightedName}
                installed={installed}
                lifecycleOperations={lifecycleOperations}
                loaded={loaded}
                onBack={closeManage}
                onOpen={(name) => openDriver(name, "manage", "card")}
                onCardKeyDown={onCardKeyDown}
                onHighlight={setHighlightedName}
                onQueryChange={(value) => {
                  setManageQuery(value);
                  setHighlightedName("");
                }}
                onRemove={(driver) => setRemoveName(driver.name)}
                onSearchKeyDown={onSearchKeyDown}
                onSetup={openSetup}
                query={manageQuery}
                searchRef={manageSearchRef}
                scrollRef={manageScrollRef}
                visible={visibleInstalled}
              />
            ) : (
              <DriverBrowseView
                accessProfile={accessProfile}
                cardRefs={browseCardRefs}
                drivers={drivers}
                error={error}
                highlightedName={highlightedName}
                installed={installed}
                lifecycleOperations={lifecycleOperations}
                loaded={loaded}
                onInstall={requestDriverInstall}
                onCardKeyDown={onCardKeyDown}
                onHighlight={setHighlightedName}
                onManage={openManage}
                onOpen={(name, origin) => openDriver(name, "browse", origin)}
                onQueryChange={(value) => {
                  setBrowseQuery(value);
                  setHighlightedName("");
                }}
                onSearchKeyDown={onSearchKeyDown}
                onSetup={openSetup}
                query={browseQuery}
                searchRef={browseSearchRef}
                scrollRef={browseScrollRef}
                surfaceRef={browseSurfaceRef}
                visible={visible}
              />
            )}
          </CatalogRouteTransition>
        </div>
        <div aria-hidden={!selected || undefined} className={cx("h-full min-h-0", !selected && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={selected ? `driver-detail:${selected.name}` : "driver-detail-empty"}>
            {selected ? (
              <DriverDetail
                accessProfile={accessProfile}
                backButtonRef={detailBackRef}
                driver={selected}
                lifecycle={lifecycleOperations[driverPackageId(selected)]}
                onBack={closeDriver}
                onInstall={() => requestDriverInstall(selected)}
                onAddPreset={(preset) => void openAgentPreset(preset)}
                onRemove={() => setRemoveName(selected.name)}
                onSetup={() => openSetup(selected)}
                projects={projects}
                returnView={detailReturnView}
                rootEeId={rootEeId}
              />
            ) : <div className="h-full min-h-0" />}
          </CatalogRouteTransition>
        </div>
      </div>
      <DriverSetupDialog driver={setupDriver} draft={setupDraft} hostManifestReady={hostManifestReady} hostRootRefs={hostRootRefs} onCancel={() => setSetupName("")} onChange={setSetupDraft} onSave={saveSetup} onSelectFolder={selectDriverFolder} projects={projects} rootEeId={rootEeId} saving={saving} selectingFolder={selectingFolder} />
      <DriverRemovalDialog
        accessProfile={accessProfile}
        driver={removeDriver}
        lifecycle={removeDriver ? lifecycleOperations[driverPackageId(removeDriver)] : undefined}
        onCancel={() => setRemoveName("")}
        onConfirm={() => removeDriver && void removeInstalledDriver(removeDriver)}
      />
    </>
  );
}

function DriverBrowseView({
  accessProfile,
  cardRefs,
  drivers,
  error,
  highlightedName,
  installed,
  lifecycleOperations,
  loaded,
  onCardKeyDown,
  onHighlight,
  onInstall,
  onManage,
  onOpen,
  onQueryChange,
  onSearchKeyDown,
  onSetup,
  query,
  searchRef,
  scrollRef,
  surfaceRef,
  visible,
}: {
  accessProfile: OctopusClientAccessProfile;
  cardRefs: RefObject<Map<string, HTMLDivElement>>;
  drivers: DriverRecord[];
  error: string;
  highlightedName: string;
  installed: DriverRecord[];
  lifecycleOperations: Record<string, DriverLifecycleOperation>;
  loaded: boolean;
  onCardKeyDown: (event: KeyboardEvent<HTMLButtonElement>, name: string) => void;
  onHighlight: (name: string) => void;
  onInstall: (driver: DriverRecord) => void;
  onManage: () => void;
  onOpen: (name: string, origin: "card" | "rail") => void;
  onQueryChange: (value: string) => void;
  onSearchKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  onSetup: (driver: DriverRecord) => void;
  query: string;
  searchRef: RefObject<HTMLInputElement | null>;
  scrollRef: RefObject<HTMLDivElement | null>;
  surfaceRef: RefObject<HTMLElement | null>;
  visible: DriverRecord[];
}) {
  if (!loaded) return <OctopusLoadingState className="surface-content-loading-state" label="Loading drivers" />;
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none" ref={surfaceRef}>
      <header className="surface-titlebar-spacer draggable flex h-toolbar shrink-0 items-center justify-between gap-2 px-panel">
        <span className="no-drag flex min-w-0 items-center rounded-lg px-2 py-1 text-sm text-token-foreground">Drivers</span>
        <OcIconButton className="no-drag" glyph="settings" id="drivers-browse-manage" label="Manage installed drivers" onClick={onManage} size="sm" variant="ghost" />
      </header>
      <div className="plugins-surface-scroll relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" ref={scrollRef}>
        <div className="flex min-h-full w-full flex-col">
          <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel">
            <div className="flex items-start justify-between gap-4 px-2">
              <div className="flex min-w-0 flex-col gap-2">
                <h1 className="heading-xl font-normal text-default">Drivers</h1>
                <div className="text-lg leading-6 text-secondary">Add local environments for files, terminals, browsers, and other systems</div>
              </div>
            </div>
          </div>
          <DriverCatalogSearch id="drivers-page-search" inputRef={searchRef} onChange={onQueryChange} onKeyDown={onSearchKeyDown} placeholder="Search drivers" value={query} />
          <div className="mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col gap-8 px-panel pt-5 pb-panel">
            {loaded && error && drivers.length ? <DriverProjectionIssue detail={error} /> : null}
            {error && drivers.length === 0 ? (
              <OcEmptyState detail={error} glyph="warning" size="compact" title="Drivers could not be loaded" tone="danger" />
            ) : drivers.length === 0 ? (
              <OcEmptyState detail="No verified local source or installed Worker Environment kind package is available." glyph="terminal" size="compact" title="No driver packages found" />
            ) : visible.length === 0 ? (
              <OcEmptyState glyph="search" size="compact" title="No drivers found" />
            ) : (
              <>
                {installed.length ? (
                  <CatalogInstalledRail
                    getItemKey={driverKey}
                    getItemLabel={driverName}
                    items={installed}
                    manageLabel="Manage installed drivers"
                    onManage={onManage}
                    onOpen={(driver) => onOpen(driver.name, "rail")}
                    renderIcon={(driver, size) => <Glyph name={driverGlyph(driver)} size={size === "tile" ? 18 : 8} />}
                  />
                ) : null}
                <CatalogSection
                  batchSize={12}
                  description="Install a local package, then choose the projects where it may be used."
                  getItemKey={driverKey}
                  getItemLabel={driverName}
                  items={visible}
                  renderCard={(driver) => (
                    <DriverCard
                      accessProfile={accessProfile}
                      driver={driver}
                      highlighted={highlightedName === driver.name}
                      lifecycle={lifecycleOperations[driverPackageId(driver)]}
                      onFocus={() => onHighlight(driver.name)}
                      onInstall={() => onInstall(driver)}
                      onKeyDown={(event) => onCardKeyDown(event, driver.name)}
                      onOpen={() => onOpen(driver.name, "card")}
                      onSetup={() => onSetup(driver)}
                      setRef={(node) => setDriverCardRef(cardRefs, driver.name, node)}
                      view="browse"
                    />
                  )}
                  renderItemPreview={(driver) => <Glyph name={driverGlyph(driver)} size={8} />}
                  resetKey={`browse:${query}`}
                  showCount={Boolean(query.trim())}
                  title="Driver catalog"
                  visibleItemLimit={6}
                />
              </>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

function DriverManageView({
  accessProfile,
  cardRefs,
  drivers,
  error,
  highlightedName,
  installed,
  lifecycleOperations,
  loaded,
  onBack,
  onCardKeyDown,
  onHighlight,
  onOpen,
  onQueryChange,
  onRemove,
  onSearchKeyDown,
  onSetup,
  query,
  searchRef,
  scrollRef,
  visible,
}: {
  accessProfile: OctopusClientAccessProfile;
  cardRefs: RefObject<Map<string, HTMLDivElement>>;
  drivers: DriverRecord[];
  error: string;
  highlightedName: string;
  installed: DriverRecord[];
  lifecycleOperations: Record<string, DriverLifecycleOperation>;
  loaded: boolean;
  onBack: () => void;
  onCardKeyDown: (event: KeyboardEvent<HTMLButtonElement>, name: string) => void;
  onHighlight: (name: string) => void;
  onOpen: (name: string) => void;
  onQueryChange: (value: string) => void;
  onRemove: (driver: DriverRecord) => void;
  onSearchKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  onSetup: (driver: DriverRecord) => void;
  query: string;
  searchRef: RefObject<HTMLInputElement | null>;
  scrollRef: RefObject<HTMLDivElement | null>;
  visible: DriverRecord[];
}) {
  if (!loaded) return <OctopusLoadingState className="surface-content-loading-state" label="Loading installed drivers" />;
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground focus:outline-none" onClick={onBack} type="button">
          <span>Drivers</span><span className="text-token-input-placeholder-foreground">/</span><span className="text-token-foreground">Manage</span>
        </button>
      </header>
      <div className="plugins-surface-scroll relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" ref={scrollRef}>
        <div className="flex min-h-full w-full flex-col">
          <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel">
            <div className="flex min-w-0 flex-col gap-2 px-2">
              <h1 className="heading-xl font-normal text-default">Manage drivers</h1>
              <div className="text-lg leading-6 text-secondary">Review setup or remove driver packages already installed in Octopus Home</div>
            </div>
          </div>
          <DriverCatalogSearch id="drivers-manage-search" inputRef={searchRef} onChange={onQueryChange} onKeyDown={onSearchKeyDown} placeholder="Search installed drivers" value={query} />
          <div className="mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col gap-8 px-panel pt-5 pb-panel">
            {loaded && error && drivers.length ? <DriverProjectionIssue detail={error} /> : null}
            {error && drivers.length === 0 ? (
              <OcEmptyState detail={error} glyph="warning" size="compact" title="Drivers could not be loaded" tone="danger" />
            ) : visible.length === 0 ? (
              <OcEmptyState
                glyph={query ? "search" : "terminal"}
                size="compact"
                title={query && installed.length ? "No installed drivers found" : "No drivers installed"}
              />
            ) : (
              <div className="[&_.oc-catalog-grid]:!grid-cols-1">
                <CatalogSection
                  batchSize={12}
                  description="Open a driver to review project access, current activity, or removal requirements."
                  getItemKey={driverKey}
                  getItemLabel={driverName}
                  items={visible}
                  renderCard={(driver) => (
                    <DriverCard
                      accessProfile={accessProfile}
                      driver={driver}
                      highlighted={highlightedName === driver.name}
                      lifecycle={lifecycleOperations[driverPackageId(driver)]}
                      onFocus={() => onHighlight(driver.name)}
                      onKeyDown={(event) => onCardKeyDown(event, driver.name)}
                      onOpen={() => onOpen(driver.name)}
                      onRemove={() => onRemove(driver)}
                      onSetup={() => onSetup(driver)}
                      setRef={(node) => setDriverCardRef(cardRefs, driver.name, node)}
                      view="manage"
                    />
                  )}
                  renderItemPreview={(driver) => <Glyph name={driverGlyph(driver)} size={8} />}
                  resetKey={`manage:${query}`}
                  showCount
                  title="Installed"
                  visibleItemLimit={12}
                />
              </div>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

function DriverCatalogSearch({ id, inputRef, onChange, onKeyDown, placeholder, value }: {
  id: string;
  inputRef: RefObject<HTMLInputElement | null>;
  onChange: (value: string) => void;
  onKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  placeholder: string;
  value: string;
}) {
  return (
    <div className="sticky top-0 z-30 bg-surface after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-surface after:to-transparent after:content-['']">
      <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] items-center gap-2 px-panel pt-panel pb-2">
        <OcSearchField className="min-w-0 flex-1" id={id} inputRef={inputRef} label={placeholder} onChange={onChange} onKeyDown={onKeyDown} placeholder={placeholder} value={value} />
      </div>
    </div>
  );
}

function DriverCard({ accessProfile, driver, highlighted, lifecycle, onFocus, onInstall, onKeyDown, onOpen, onRemove, onSetup, setRef, view }: {
  accessProfile: OctopusClientAccessProfile;
  driver: DriverRecord;
  highlighted: boolean;
  lifecycle?: DriverLifecycleOperation;
  onFocus: () => void;
  onInstall?: () => void;
  onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
  onOpen: () => void;
  onRemove?: () => void;
  onSetup?: () => void;
  setRef: (node: HTMLDivElement | null) => void;
  view: DriverCatalogView;
}) {
  const busy = driverLifecycleBusy(lifecycle);
  return (
    <div
      className={cx(
        "group/card flex items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5",
        highlighted && "bg-text/5",
      )}
      data-search-result-active={highlighted || undefined}
      ref={setRef}
    >
      <button
        className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 rounded-xl p-0 text-start outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0"
        data-driver-open
        id={driverOpenButtonId(view, driver.name)}
        onClick={onOpen}
        onFocus={onFocus}
        onKeyDown={onKeyDown}
        type="button"
      >
        <DriverVisual driver={driver} size={view === "browse" ? "browse" : "card"} />
        <span className={cx("flex min-w-0 flex-1 flex-col justify-center", view === "browse" ? "gap-px" : "gap-0.5")}>
          <span className="block truncate font-medium text-default">{driverName(driver)}</span>
          <span className="block line-clamp-1 text-sm leading-relaxed text-text-description">{driverSummary(driver)}</span>
          <DriverCardMeta driver={driver} />
        </span>
      </button>
      <div className="flex shrink-0 flex-col items-end gap-1">
        {busy && lifecycle ? (
          <DriverLifecycleProgress operation={lifecycle} />
        ) : !driver.installed && onInstall ? (
          <OcAnimatedActionSlot>
            <OcButton
              disabled={accessProfile !== "live" || driver.source_available !== true || Boolean(driver.install_block_reason)}
              onClick={(event) => {
                event.stopPropagation();
                onInstall();
              }}
              size="xs"
              title={driverInstallTitle(driver, accessProfile)}
              variant="secondary"
            >
              {lifecycle?.phase === "failed" && lifecycle.operation === "install" ? "Retry" : "Install"}
            </OcButton>
          </OcAnimatedActionSlot>
        ) : driver.installed && onSetup ? (
          <div className="flex items-center gap-1">
            <OcButton onClick={(event) => {
              event.stopPropagation();
              onSetup();
            }} size="xs" variant="ghost">{driverAvailableScopeCount(driver) ? "Manage" : "Set up"}</OcButton>
            {onRemove ? <OcIconButton disabled={accessProfile !== "live" || driver.removal_allowed !== true} glyph="trash" label={`Remove ${driverName(driver)}`} onClick={(event) => {
              event.stopPropagation();
              onRemove();
            }} size="xs" title={driverRemovalTitle(driver, accessProfile)} variant="ghost" /> : null}
          </div>
        ) : null}
        {lifecycle?.phase === "failed" ? (
          <span className="max-w-48 truncate text-xs text-token-error-foreground" title={lifecycle.error}>{lifecycle.operation === "install" ? "Install failed" : "Remove failed"}</span>
        ) : null}
      </div>
    </div>
  );
}

function DriverStateStatuses({ driver }: { driver: DriverRecord }) {
  const activeInstances = driver.active_instances.length;
  const status = driverPrimaryStatus(driver);
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-1">
      <OcStatus label={status.label} tone={status.tone} />
      {activeInstances ? <OcStatus label={`In use · ${activeInstances}`} tone="success" /> : null}
    </span>
  );
}

function DriverCardMeta({ driver }: { driver: DriverRecord }) {
  const status = driverPrimaryStatus(driver);
  const availableHome = driver.project_states.some((state) => state.is_root && state.policy_allowed);
  const availableProjects = driverAvailableProjectCount(driver);
  const activeInstances = driver.active_instances.length;
  const scope = [
    availableHome ? "Home" : "",
    availableProjects ? `${availableProjects} project${availableProjects === 1 ? "" : "s"}` : "",
  ].filter(Boolean).join(" + ");
  return (
    <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs leading-4 text-token-description-foreground">
      <i aria-hidden="true" className={cx(
        "size-1.5 shrink-0 rounded-full",
        status.tone === "success" ? "bg-token-charts-green"
          : status.tone === "warning" ? "bg-token-charts-orange"
            : status.tone === "danger" ? "bg-token-charts-red"
              : "bg-token-description-foreground",
      )} />
      <span className={cx(
        "shrink-0",
        status.tone === "warning" && "text-token-charts-orange",
        status.tone === "danger" && "text-token-charts-red",
      )}>{status.label}</span>
      {scope ? <><span aria-hidden="true">·</span><span className="truncate">{scope}</span></> : null}
      {activeInstances ? <><span aria-hidden="true">·</span><span className="truncate text-token-charts-green">In use {activeInstances}</span></> : null}
    </span>
  );
}

function driverPrimaryStatus(driver: DriverRecord): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  const materializationIncomplete = !driver.installed && (
    driver.materialized === true
    || driver.partially_materialized === true
    || driver.receipt_state === "stale"
  );
  if (materializationIncomplete) return { label: "Needs repair", tone: "danger" };
  if (driver.installed && !driverAvailableScopeCount(driver)) return { label: "Not enabled", tone: "neutral" };
  if (driver.installed) return { label: "Enabled", tone: "success" };
  if (driver.source_available === true) return { label: "Not installed", tone: "neutral" };
  return { label: "Unavailable", tone: "danger" };
}

function DriverLifecycleProgress({ operation }: { operation: DriverLifecycleOperation }) {
  const label = operation.operation === "install"
    ? operation.phase === "preflight" || operation.phase === "verifying"
      ? "Checking"
      : operation.phase === "copying"
        ? "Copying"
        : operation.phase === "finalizing" || operation.phase === "complete"
          ? "Finishing"
          : "Installing"
    : operation.phase === "preflight" || operation.phase === "verifying"
      ? "Checking"
      : operation.phase === "finalizing" || operation.phase === "complete"
        ? "Finishing"
        : "Removing";
  return <OcInlineProgress label={label} progressLabel={`${label} ${titleCase(operation.operation)}`} showValue value={operation.percent} />;
}

function DriverProjectionIssue({ detail }: { detail: string }) {
  return (
    <div className="rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
      <div className="font-medium">Some driver package information could not be loaded</div>
      <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{detail}</div>
    </div>
  );
}

function DriverDetail({ accessProfile, backButtonRef, driver, lifecycle, onBack, onInstall, onRemove, onSetup, onAddPreset, projects, returnView, rootEeId }: {
  accessProfile: OctopusClientAccessProfile;
  backButtonRef: RefObject<HTMLButtonElement | null>;
  driver: DriverRecord;
  lifecycle?: DriverLifecycleOperation;
  onBack: () => void;
  onInstall: () => void;
  onAddPreset: (preset: DriverAgentPreset) => void;
  onRemove: () => void;
  onSetup: () => void;
  projects: DriverProject[];
  returnView: DriverCatalogView;
  rootEeId: string;
}) {
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const availableScopes = driver.project_states.filter((state) => state.policy_allowed);
  const availableHome = availableScopes.some((state) => state.is_root);
  const availableProjects = availableScopes.filter((state) => !state.is_root);
  const defaultProjects = driver.project_states.filter((state) => state.selected_by_default);
  const busy = driverLifecycleBusy(lifecycle);
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-secondary hover:bg-primary-ghost-hover hover:text-default focus:outline-none" onClick={onBack} ref={backButtonRef} type="button">
          <span>{returnView === "manage" ? "Manage drivers" : "Drivers"}</span><span className="text-tertiary">/</span><span className="truncate text-default">{driverName(driver)}</span>
        </button>
      </header>
      <div className="plugins-surface-scroll min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex min-h-full flex-col px-toolbar pt-panel pb-panel">
          <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
            <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
              <DriverVisual driver={driver} size="detail" />
              <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
                <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
                  <h1 className="heading-base min-w-0 break-words text-default electron:heading-lg">{driverName(driver)}</h1>
                  <div className="text-base text-secondary">{driverSummary(driver)}</div>
                  <DriverStateStatuses driver={driver} />
                </div>
                <div className="flex max-w-full shrink-0 items-center gap-2">
                  {busy && lifecycle ? <DriverLifecycleProgress operation={lifecycle} /> : null}
                  {driver.installed ? (
                    <>
                      <OcButton disabled={Boolean(busy) || accessProfile !== "live" || driver.removal_allowed !== true} onClick={onRemove} size="sm" title={driverRemovalTitle(driver, accessProfile)} tone="danger" variant="ghost">Remove</OcButton>
                      <OcButton disabled={Boolean(busy)} onClick={onSetup} size="sm" variant={availableScopes.length ? "secondary" : "primary"}>{availableScopes.length ? "Manage setup" : "Set up"}</OcButton>
                    </>
                  ) : (
                    <OcButton
                      disabled={Boolean(busy) || accessProfile !== "live" || driver.source_available !== true || Boolean(driver.install_block_reason)}
                      leadingIcon={busy ? undefined : "download"}
                      onClick={onInstall}
                      size="sm"
                      title={driverInstallTitle(driver, accessProfile)}
                      variant="primary"
                    >
                      {lifecycle?.phase === "failed" ? "Retry install" : "Install"}
                    </OcButton>
                  )}
                </div>
              </div>
            </header>
            {lifecycle?.phase === "failed" ? (
              <div className="mx-[var(--detail-page-inline-inset,0px)] rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
                <div className="font-medium">{lifecycle.operation === "install" ? "Installation failed" : "Removal failed"}</div>
                <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{lifecycle.error}</div>
              </div>
            ) : null}
            <DriverDetailSection title="Setup">
              <DriverDetailRow label="Package" value={driver.installed
                ? "Installed in Octopus Home."
                : driver.materialized || driver.partially_materialized
                  ? "Driver files were found, but Octopus could not verify one complete installation."
                  : driver.source_available === true
                    ? "Not installed. Installing adds it to no project until you enable it."
                    : "This build ships no package for it."} />
              <DriverDetailRow label="Enabled in Home" value={availableHome ? "Yes" : "No"} />
              <DriverDetailRow label="Enabled in projects" value={availableProjects.length ? availableProjects.map((state) => driverProjectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId)).join(", ") : "Not enabled in any project yet."} />
              <DriverDetailRow label="Default for new tasks" value={defaultProjects.length ? defaultProjects.map((state) => driverProjectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId)).join(", ") : "Not selected by default in any project."} />
              {driver.materialized || driver.partially_materialized ? driver.receipt_state === "valid" ? null : <DriverDetailRow label="Install receipt" value={driverReceiptDetail(driver)} /> : null}
              {driver.installed && (driver.removal_block_reason || driver.removal_allowed === undefined) ? <DriverDetailRow label="Removal" value={driver.removal_block_reason || "Removal availability is not reported by this projection."} /> : null}
            </DriverDetailSection>
            {driver.agent_presets?.length ? <DriverDetailSection title="Agent presets">
              {driver.agent_presets.map((preset) => <OcButton key={`${preset.pack_id}:${preset.id}`} disabled={accessProfile !== "live" || !driver.installed || !preset.project_ee_ids.length} onClick={() => onAddPreset(preset)} variant="secondary">Add {preset.name} Agent</OcButton>)}
            </DriverDetailSection> : null}
            <DriverDetailSection title="Activity">
              {driver.active_instances.length ? driver.active_instances.map((instance, index) => (
                <DriverDetailRow
                  key={driverInstanceKey(instance, index)}
                  label={driverInstanceLabel(instance, index)}
                  value={driverInstanceDetail(instance)}
                />
              )) : <DriverDetailRow label="Current use" value="No task is using this driver right now." />}
            </DriverDetailSection>
            <DriverDetailSection title="Project setup">
              {driver.project_states.map((state) => (
                <DriverDetailRow key={state.ee_id} label={driverProjectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId)} value={state.policy_allowed ? state.selected_by_default ? "Available · included in new tasks" : "Available" : state.ceiling_allowed ? "Not available" : "Not supported here"} />
              ))}
            </DriverDetailSection>
            <DriverDetailSection title="Capabilities">
              <div className="mt-3 flex flex-wrap gap-2 px-2">
                {driver.capabilities.map((capability) => <span className="rounded-full border border-token-border/40 px-2 py-1 text-sm text-token-description-foreground" key={capability}>{titleCase(capability)}</span>)}
              </div>
            </DriverDetailSection>
            <DriverAdvancedDetails driver={driver} />
          </div>
        </div>
      </div>
    </section>
  );
}

function DriverRemovalDialog({ accessProfile, driver, lifecycle, onCancel, onConfirm }: {
  accessProfile: OctopusClientAccessProfile;
  driver: DriverRecord | null;
  lifecycle?: DriverLifecycleOperation;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const busy = driverLifecycleBusy(lifecycle);
  return (
    <SetupDialogShell ariaLabel={driver ? `Remove ${driver.name}` : "Remove driver"} busy={Boolean(busy)} onClose={onCancel} open={Boolean(driver)} panelClassName="max-w-md">
      {driver ? (
        <>
          <div className="px-6 pt-8 pb-6">
            <h2 className="heading-base text-token-foreground electron:heading-lg">Remove {driverName(driver)}?</h2>
            <p className="mt-2 text-sm leading-5 text-token-text-secondary">This removes the Driver and its package-owned tools from the active registry. Project access must already be removed, and no worker environment may still be active.</p>
            {driver.removal_block_reason || driver.removal_allowed !== true ? <div className="mt-4 rounded-xl border border-token-charts-orange/25 bg-token-charts-orange/10 px-3 py-2 text-sm text-token-charts-orange">{driver.removal_block_reason || (driver.removal_allowed === false ? "This driver cannot be removed in its current state." : "Removal availability is not reported by this projection.")}</div> : null}
          </div>
          <OcDialogFooter>
            <OcButton disabled={Boolean(busy)} onClick={onCancel} variant="ghost">Cancel</OcButton>
            <OcButton disabled={Boolean(busy) || accessProfile !== "live" || driver.removal_allowed !== true} onClick={onConfirm} tone="danger" variant="secondary">{busy && lifecycle ? <DriverLifecycleProgress operation={lifecycle} /> : "Remove"}</OcButton>
          </OcDialogFooter>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function DriverSetupDialog({
  driver,
  draft,
  onCancel,
  onChange,
  onSave,
  onSelectFolder,
  hostManifestReady,
  hostRootRefs,
  projects,
  rootEeId,
  saving,
  selectingFolder,
}: {
  driver: DriverRecord | null;
  draft: DriverSetupDraft;
  onCancel: () => void;
  onChange: (draft: DriverSetupDraft) => void;
  onSave: () => void;
  onSelectFolder: (eeId: string, slot: string) => void;
  hostManifestReady: boolean;
  hostRootRefs: Record<string, string>;
  projects: DriverProject[];
  rootEeId: string;
  saving: boolean;
  selectingFolder: string;
}) {
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const rootSlots = driver ? driverRootSlots(driver) : [];
  return (
    <SetupDialogShell ariaLabel={driver ? `Set up ${driver.name}` : "Set up driver"} busy={saving} onClose={onCancel} open={Boolean(driver)} size="xwide">
      {driver ? (
        <>
        <div className="flex items-start gap-4 px-6 pt-8 pb-6">
          <DriverVisual driver={driver} size="detail" />
          <div className="min-w-0 flex-1">
            <h2 className="heading-base text-token-foreground electron:heading-lg">Set up {driverName(driver)}</h2>
            <p className="mt-1 text-sm leading-5 text-token-text-secondary">Choose the projects that may use this driver and provide any folders or machine settings it needs. An environment starts only when a task uses it.</p>
          </div>
          <OcIconButton disabled={saving} glyph="close" label="Close" onClick={onCancel} variant="ghost" />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
          <div className="flex flex-col gap-3">
            {driver.project_states.map((state) => {
              const current = draft[state.ee_id] ?? { available: state.policy_allowed, selected: state.selected_by_default, hostSlots: { ...state.host_slots } };
              const project = projectById.get(state.ee_id);
              return (
                <section className="overflow-hidden rounded-2xl border border-token-border/60" key={state.ee_id}>
                  <div className="flex items-center gap-4 px-4 py-3">
                    <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name="project" size={17} /></span>
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium text-token-foreground">{driverProjectLabel(project, state.ee_id, rootEeId)}</div>
                      <div className="truncate text-xs text-token-description-foreground">{state.ee_id === rootEeId ? "Root life area" : "Project"}</div>
                    </div>
                    <OcSwitch checked={current.available} disabled={saving || (!state.ceiling_allowed && !current.available)} label={state.ceiling_allowed ? "Available" : current.available ? "Remove" : "Not supported here"} onCheckedChange={(available) => onChange({ ...draft, [state.ee_id]: { ...current, available, selected: available ? current.selected : false } })} showLabel />
                    <OcSwitch checked={current.selected} disabled={saving || !current.available} label="New task default" onCheckedChange={(selected) => onChange({ ...draft, [state.ee_id]: { ...current, selected } })} showLabel />
                  </div>
                  {current.available && rootSlots.length ? (
                    <div className="grid gap-3 border-t border-token-border/40 bg-token-foreground/[0.015] px-4 py-3 sm:grid-cols-2">
                      {rootSlots.map((slot) => (
                        <div className="min-w-0" key={slot}>
                          <div className="flex min-w-0 items-end gap-2">
                            <OcTextField
                              className="min-w-0 flex-1"
                              disabled={saving || Boolean(selectingFolder)}
                              label={titleCase(slot)}
                              onChange={(event) => onChange({
                                ...draft,
                                [state.ee_id]: {
                                  ...current,
                                  hostSlots: { ...current.hostSlots, [slot]: event.currentTarget.value },
                                },
                              })}
                              placeholder={hostRootRefs[slot] || (hostManifestReady ? "No machine default" : "Machine defaults unavailable")}
                              showLabel
                              value={current.hostSlots[slot] ?? ""}
                            />
                            <OcIconButton
                              className="mb-px"
                              disabled={saving || Boolean(selectingFolder)}
                              glyph="folder"
                              label={`Choose ${titleCase(slot)} folder`}
                              onClick={() => onSelectFolder(state.ee_id, slot)}
                              variant="secondary"
                            />
                          </div>
                          <div className="mt-1 truncate text-xs text-token-description-foreground" title={driverSlotDefaultDetail(slot, current.hostSlots, hostRootRefs, hostManifestReady)}>{driverSlotDefaultDetail(slot, current.hostSlots, hostRootRefs, hostManifestReady)}</div>
                        </div>
                      ))}
                    </div>
                  ) : null}
                </section>
              );
            })}
          </div>
        </div>
        <div className="flex min-h-[84px] items-center justify-end gap-2 border-t border-token-border/40 px-6">
          <OcButton disabled={saving} onClick={onCancel} variant="ghost">Cancel</OcButton>
          <OcButton disabled={saving} onClick={onSave} variant="primary">{saving ? "Saving…" : "Save setup"}</OcButton>
        </div>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function DriverVisual({ driver, size }: { driver: DriverRecord; size: "browse" | "card" | "detail" }) {
  const detail = size === "detail";
  return (
    <span className={cx(
      "relative flex shrink-0 items-center justify-center overflow-hidden border border-default text-secondary",
      detail ? "size-[60px] rounded-2xl bg-transparent" : "size-10 rounded-lg bg-transparent",
    )}>
      <Glyph name={driverGlyph(driver)} size={detail ? 28 : 24} />
    </span>
  );
}

function DriverDetailSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]">
      <div className="flex items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)]">
        <h2 className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">{title}</h2>
      </div>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">{children}</div>
    </section>
  );
}

function DriverAdvancedDetails({ driver }: { driver: DriverRecord }) {
  return (
    <details className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]">
      <summary className="flex cursor-interaction list-none items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)] marker:hidden [&::-webkit-details-marker]:hidden">
        <span className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">
          Advanced
          <Glyph className="icon-2xs -rotate-90 transition-transform group-open/section:rotate-0 motion-reduce:transition-none" name="chevronDown" size={12} />
        </span>
        <span className="text-sm text-tertiary">Package details</span>
      </summary>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">
        <DriverDetailRow label="Kind" value={driver.name} />
        <DriverDetailRow label="Substrate" value={driver.substrate_family} />
        <DriverDetailRow label="Door interfaces" value={driver.door_interfaces.join(", ") || "None"} />
        <DriverDetailRow label="Driver refs" value={driver.door_driver_refs.join(", ") || "None"} />
        <DriverDetailRow label="Executors" value={driver.supported_executor_kinds.join(", ") || "None"} />
        <DriverDetailRow label="Tool ids" value={driver.tool_ids.join(", ") || "None"} />
        <DriverDetailRow label="Network ceiling" value={driver.boundary_defaults.network_ceiling || "unspecified"} />
        <DriverDetailRow label="Worker consent" value={driver.worker_consent} />
        <DriverDetailRow label="Instance config" value={driver.instance_config_files.join(", ") || "None"} />
        <DriverDetailRow label="Release required" value={driver.lifecycle.requires_release ? "Yes" : "No"} />
        <DriverDetailRow label="Descriptor" value={driver.descriptor_path} />
      </div>
    </details>
  );
}

function DriverDetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid min-h-9 grid-cols-[minmax(8rem,0.35fr)_minmax(0,1fr)] items-start gap-4 rounded-lg px-2 py-1.5 hover:bg-token-foreground/3">
      <span className="text-sm text-token-text-secondary">{label}</span>
      <span className="min-w-0 text-sm text-default select-text [overflow-wrap:anywhere]">{value}</span>
    </div>
  );
}

const DRIVER_GLYPHS = new Set<GlyphName>([
  "artifact",
  "browser",
  "code",
  "desktop",
  "folderOpen",
  "layers",
  "microphone",
  "plugins",
  "search",
  "sites",
  "terminal",
  "worktree",
]);

function driverGlyph(driver: DriverRecord): GlyphName {
  const icon = driver.icon as GlyphName;
  return DRIVER_GLYPHS.has(icon) ? icon : "environment";
}

function driverSummary(driver: DriverRecord): string {
  if (driver.description?.trim()) return driver.description.trim();
  const capabilities = driver.capabilities.slice(0, 3).map(titleCase).join(" · ");
  return capabilities || `${driver.tool_ids.length} tool${driver.tool_ids.length === 1 ? "" : "s"}`;
}

function driverInstallTitle(driver: DriverRecord, accessProfile: OctopusClientAccessProfile): string {
  if (accessProfile !== "live") return "Install drivers from the Administrator profile.";
  if (driver.install_block_reason) return driver.install_block_reason;
  if (driver.source_available === true) return `Install ${driverName(driver)}`;
  if (driver.source_available === false) return "No verified local package source is available.";
  return "Local package availability is not reported by this projection.";
}

function driverRemovalTitle(driver: DriverRecord, accessProfile: OctopusClientAccessProfile): string {
  if (accessProfile !== "live") return "Remove drivers from the Administrator profile.";
  if (driver.removal_block_reason) return driver.removal_block_reason;
  if (driver.removal_allowed === true) return `Remove ${driverName(driver)}`;
  if (driver.removal_allowed === false) return "This driver cannot be removed in its current state.";
  return "Removal availability is not reported by this projection.";
}

function driverReceiptDetail(driver: DriverRecord): string {
  if (driver.receipt_state === "valid") return "Valid local install receipt.";
  if (driver.receipt_state) return titleCase(driver.receipt_state);
  return "Install receipt status is not reported by this projection.";
}

function driverAvailableProjectCount(driver: DriverRecord): number {
  return driver.project_states.filter((state) => !state.is_root && state.policy_allowed).length;
}

function driverAvailableScopeCount(driver: DriverRecord): number {
  return driver.project_states.filter((state) => state.policy_allowed).length;
}

function driverLifecycleBusy(operation: DriverLifecycleOperation | undefined): boolean {
  return Boolean(operation && operation.phase !== "failed" && operation.phase !== "complete");
}

function driverPackageId(driver: DriverRecord): string {
  return driver.package_id || `driver.${driver.name}`;
}

function driverKey(driver: DriverRecord): string {
  return driver.name;
}

function driverName(driver: DriverRecord): string {
  return driver.display_name?.trim() || titleCase(driver.name);
}

function driverOpenButtonId(view: DriverCatalogView, name: string): string {
  return `drivers-${view}-open-${name}`;
}

function setDriverCardRef(refs: RefObject<Map<string, HTMLDivElement>>, name: string, node: HTMLDivElement | null) {
  if (node) refs.current?.set(name, node);
  else refs.current?.delete(name);
}

function scheduleFocus(focus: () => void) {
  window.requestAnimationFrame(() => window.requestAnimationFrame(focus));
}

function driverSlotDefaultDetail(slot: string, overrides: Record<string, string>, defaults: Record<string, string>, ready: boolean): string {
  const machineDefault = defaults[slot];
  if (overrides[slot] && machineDefault) return `Overrides machine default · ${machineDefault}`;
  if (overrides[slot]) return "Project-specific folder";
  if (machineDefault) return `Machine default · ${machineDefault}`;
  return ready ? "No machine default is configured" : "Machine defaults could not be read";
}

function driverRootSlots(driver: DriverRecord): string[] {
  const refs = [
    ...(driver.boundary_defaults.read_root_refs ?? []),
    ...(driver.boundary_defaults.write_root_refs ?? []),
    ...(driver.boundary_defaults.protected_read_only_subpaths ?? []),
  ];
  return [...new Set(refs.map((entry) => entry.path ?? "").filter(Boolean))].sort();
}

function driverSearchText(driver: DriverRecord): string {
  return [driver.name, driver.substrate_family, ...driver.capabilities, ...driver.tool_ids, ...driver.door_interfaces].join(" ").toLowerCase();
}

function titleCase(value: string): string {
  return value.replace(/[_-]+/g, " ").replace(/\b\w/g, (character) => character.toUpperCase());
}

function sameStringRecord(left: Record<string, string>, right: Record<string, string>): boolean {
  const leftEntries = Object.entries(left).filter(([, value]) => value).sort(([a], [b]) => a.localeCompare(b));
  const rightEntries = Object.entries(right).filter(([, value]) => value).sort(([a], [b]) => a.localeCompare(b));
  return JSON.stringify(leftEntries) === JSON.stringify(rightEntries);
}

function cleanStringRecord(value: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(value)
    .map(([key, entry]) => [key, entry.trim()] as const)
    .filter(([, entry]) => Boolean(entry)));
}

function driverProjectLabel(project: DriverProject | undefined, eeId: string, rootEeId: string): string {
  if (eeId === rootEeId) return "Home";
  return project?.name || "Untitled project";
}

function driverInstanceKey(instance: Record<string, unknown>, index: number): string {
  return recordString(instance.worker_environment_id) || recordString(instance.id) || `${index}`;
}

function driverInstanceLabel(instance: Record<string, unknown>, index: number): string {
  return recordString(instance.worker_environment_id) || recordString(instance.id) || `Environment ${index + 1}`;
}

function driverInstanceDetail(instance: Record<string, unknown>): string {
  const run = recordString(instance.run_id);
  const environment = recordString(instance.ee_id);
  const status = recordString(instance.status) || recordString(instance.lifecycle) || "Active";
  return [status, run ? `Task ${run}` : "", environment ? `Project ${environment}` : ""].filter(Boolean).join(" · ");
}

function recordString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function arrayRecords<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

function arrayStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.map((item) => typeof item === "string" ? item : "").filter(Boolean) : [];
}
