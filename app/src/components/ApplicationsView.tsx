import "./icon-glyphs/chat";
import "./catalog/application-artwork.css";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import type {
  OctopusApplicationConnectionState,
  OctopusApplicationLifecycleProgress,
  OctopusClientAccessProfile,
  OctopusClientSetupState,
} from "../electron";
import type { ClientExtensionActionStatus, ClientExtensionRecord } from "../extensions/types";
import {
  clientExtensionIncidentSnapshot,
  subscribeClientExtensionIncidents,
} from "../extensions/incidents";
import { useClientExtensions } from "../extensions/useClientExtensions";
import {
  canConnectApplications,
  disconnectApplicationInterface,
  formatProjectionError,
  hasProjectionBridge,
  mutateApplicationPrivateConfiguration,
  mutateOctopusOsWorkspace,
  onApplicationLifecycleProgress,
  probeApplicationConnection,
  readCurrentClientSetup,
  readProjection,
} from "../projectionClient";
import { useProjectionStructureGeneration } from "../projectionLive";
import {
  OcAnimatedActionSlot,
  OcButton,
  OcEmptyState,
  OcIconButton,
  OcInlineProgress,
  OcMenuItem,
  OcSearchField,
  OcSegmented,
  OcStatus,
  OcSwitch,
  cx,
} from "../design-system/components";
import { OcDropdown } from "../design-system/OcDropdown";
import { ApplicationAvailabilityList, type ApplicationAvailabilityChange } from "./applications/ApplicationAvailability";
import { ApplicationConnectDialog, type ApplicationConnectMode } from "./applications/ApplicationConnectDialog";
import { ApplicationConnectionControl } from "./applications/ApplicationConnectionControl";
import { ApplicationLogo, ApplicationMiniLogo } from "./applications/ApplicationLogo";
import {
  applicationConnectionSummary,
  configurationFieldKey,
  type ApplicationCategory,
  type ApplicationConfigurationField,
  type ApplicationConnectionProbeState,
  type ApplicationConnectionSummary,
  type ApplicationProject,
  type ApplicationProjectState,
  type ApplicationRecord,
} from "./applications/applicationModel";
import {
  CatalogInstalledRail,
  CatalogRouteTransition,
  CatalogSection,
} from "./catalog";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { OctopusLoadingState } from "./OctopusLoadingLogo";
import { PackageInstallReviewDialog } from "./applications/PackageInstallReviewDialog";
import { SetupDialogShell } from "./SetupDialogShell";
import { appToast, useAppErrorToast } from "../toasts";

type ApplicationsProjection = {
  status?: string;
  root_ee_id?: string;
  applications?: ApplicationRecord[];
  projects?: ApplicationProject[];
  errors?: unknown[];
};

type ApplicationPackComponent = {
  package_id: string;
  selection: "included" | "recommended";
  reason: string;
  product_kind: "application" | "driver" | "artifact";
  product_id: string;
  name: string;
  available: boolean;
  installed: boolean;
  materialized: boolean;
  partially_materialized: boolean;
  receipt_state: string;
  source_digest: string;
  error: string;
};

type ApplicationPackExtension = {
  extension_id: string;
  selection: "recommended";
  reason: string;
};

type ApplicationPackRecord = {
  id: string;
  manifest_digest: string;
  name: string;
  description: string;
  version: string;
  components: ApplicationPackComponent[];
  extensions: ApplicationPackExtension[];
  required_components_materialized: boolean;
  workspace_setup?: {
    selection: "included";
    home: {
      provider_interface_id: string;
      worker_interface_id: string;
      context_artifact_bindings: Array<{ id: string }>;
      selected_worker_environment_kinds: string[];
    };
    project_defaults: Array<{
      package_id: string;
      default_for_new_tasks: boolean;
    }>;
  };
};

type LocalPackagesProjection = {
  packs?: ApplicationPackRecord[];
  errors?: unknown[];
};

type ApplicationLifecycleOperation = {
  operationId: string;
  operation: "install" | "remove" | "repair";
  phase: OctopusApplicationLifecycleProgress["phase"] | "failed";
  percent: number;
  error: string;
  settlesAfterRead: number | null;
  presentation: ApplicationRecord;
};

type ApplicationPackLifecycleOperation = Omit<ApplicationLifecycleOperation, "operation" | "settlesAfterRead" | "presentation"> & {
  operation: "install";
  packageIds: string[];
  settlesAfterRead: { applications: number; packs: number } | null;
  presentation: { pack: ApplicationPackRecord; applications: ApplicationRecord[] };
};

type ApplicationCatalogView = "browse" | "manage";

const CATEGORY_ORDER: ApplicationCategory[] = ["provider", "connector", "plugin", "artifact"];
const INSTALL_LOCKED = "Another installation is in progress.";
const PAGE_LAYOUT_TRANSITION = { type: "spring", duration: 0.5, bounce: 0.1 } as const;
const INSTALLED_SECTIONS_TRANSITION = { duration: 0.24, ease: [0.2, 0, 0, 1] } as const;
const SHA256_DIGEST = /^[a-f0-9]{64}$/;
const CATEGORY_COPY: Record<ApplicationCategory, { title: string; description: string }> = {
  provider: { title: "AI Providers", description: "Models for Home and tasks. Each provider connects through its own sign-in, API key, or local service." },
  connector: { title: "Connectors", description: "Services and triggers that bring messages into a task and carry its replies back out." },
  plugin: { title: "Plugins", description: "Tools and guides a task picks up when it needs them, such as MCP servers, memory and image generation." },
  artifact: { title: "Context", description: "Knowledge packages you can attach to new tasks." },
};

export function ApplicationsView({
  accessProfile,
  searchFocusTick,
}: {
  accessProfile: OctopusClientAccessProfile;
  searchFocusTick: number;
}) {
  const generation = useProjectionStructureGeneration();
  const reducedMotion = useReducedMotion();
  const extensionRegistry = useClientExtensions(accessProfile);
  const extensionIncidents = useSyncExternalStore(
    subscribeClientExtensionIncidents,
    clientExtensionIncidentSnapshot,
  );
  const browseSearchRef = useRef<HTMLInputElement>(null);
  const manageSearchRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const scrollOffsetRef = useRef(0);
  const browseCardRefs = useRef(new Map<string, HTMLElement>());
  const browsePackRefs = useRef(new Map<string, HTMLElement>());
  const manageCardRefs = useRef(new Map<string, HTMLElement>());
  const detailOriginRef = useRef<HTMLElement | null>(null);
  const [projectedApplications, setApplications] = useState<ApplicationRecord[]>([]);
  const [projectedPacks, setApplicationPacks] = useState<ApplicationPackRecord[]>([]);
  const [projects, setProjects] = useState<ApplicationProject[]>([]);
  const [rootEeId, setRootEeId] = useState("");
  const [loadedProfile, setLoadedProfile] = useState<OctopusClientAccessProfile | null>(null);
  const [packsLoadedProfile, setPacksLoadedProfile] = useState<OctopusClientAccessProfile | null>(null);
  const [error, setError] = useState("");
  const [packError, setPackError] = useState("");
  const [clientSetup, setClientSetup] = useState<OctopusClientSetupState | null>(null);
  const [clientSetupError, setClientSetupError] = useState("");
  const [browseQuery, setBrowseQuery] = useState("");
  const [manageQuery, setManageQuery] = useState("");
  const [catalogView, setCatalogView] = useState<ApplicationCatalogView>("browse");
  const [detailReturnView, setDetailReturnView] = useState<ApplicationCatalogView>("browse");
  const [routeDirection, setRouteDirection] = useState<"back" | "forward">("forward");
  const [manageCategory, setManageCategory] = useState<ApplicationCategory>("connector");
  const [selectedId, setSelectedId] = useState("");
  const [selectedPackId, setSelectedPackId] = useState("");
  const [highlightedId, setHighlightedId] = useState("");
  const [packRecommendedSelections, setPackRecommendedSelections] = useState<Record<string, string[]>>({});
  const [connectTarget, setConnectTarget] = useState<{ applicationId: string; mode: ApplicationConnectMode; session: number } | null>(null);
  const [connectionProbes, setConnectionProbes] = useState<Record<string, ApplicationConnectionProbeState>>({});
  const [disconnectingId, setDisconnectingId] = useState("");
  const [availabilitySaving, setAvailabilitySaving] = useState<{ applicationId: string; eeId: string } | null>(null);
  const [configurationSaving, setConfigurationSaving] = useState("");
  const [reloadTick, setReloadTick] = useState(0);
  const probeRequests = useRef(new Map<string, number>());
  const connectAvailable = canConnectApplications();
  const [removeId, setRemoveId] = useState("");
  const [installReviewId, setInstallReviewId] = useState("");
  const [lifecycleOperations, setLifecycleOperations] = useState<Record<string, ApplicationLifecycleOperation>>({});
  const [packLifecycleOperations, setPackLifecycleOperations] = useState<Record<string, ApplicationPackLifecycleOperation>>({});
  const applications = useMemo(
    () => presentApplications(projectedApplications, lifecycleOperations, packLifecycleOperations),
    [lifecycleOperations, packLifecycleOperations, projectedApplications],
  );
  const applicationPacks = useMemo(() => projectedPacks.map((pack) => {
    const operation = packLifecycleOperations[pack.id];
    return operation && lifecycleBusy(operation) ? operation.presentation.pack : pack;
  }), [packLifecycleOperations, projectedPacks]);
  const installLocked = Object.values(lifecycleOperations).some((operation) => operation.operation !== "remove" && lifecycleBusy(operation))
    || Object.values(packLifecycleOperations).some(lifecycleBusy);
  const operationActive = installLocked || Object.values(lifecycleOperations).some(lifecycleBusy);
  const [heldErrors, setHeldErrors] = useState({ application: "", pack: "" });
  const shownError = operationActive ? heldErrors.application : error;
  const shownPackError = operationActive ? heldErrors.pack : packError;
  useAppErrorToast(shownError || shownPackError || clientSetupError);
  const applicationsReadSequence = useRef(0);
  const applicationsReadLanded = useRef(0);
  const packsReadSequence = useRef(0);
  const packsReadLanded = useRef(0);
  const loaded = loadedProfile === accessProfile;
  const packsLoaded = packsLoadedProfile === accessProfile;

  useEffect(() => {
    if (!hasProjectionBridge()) {
      setLoadedProfile(accessProfile);
      return;
    }
    let cancelled = false;
    const sequence = ++applicationsReadSequence.current;
    readProjection<ApplicationsProjection>("projection.operator.os.applications")
      .then((response) => {
        if (cancelled) return;
        setApplications(arrayRecords(response.applications));
        setProjects(arrayRecords(response.projects));
        setRootEeId(typeof response.root_ee_id === "string" ? response.root_ee_id : "");
        setError(arrayStrings(response.errors).join("\n"));
        setLoadedProfile(accessProfile);
        applicationsReadLanded.current = sequence;
        setLifecycleOperations((current) => withoutSettledOperations(current, (operation) => (
          operation.settlesAfterRead !== null && operation.settlesAfterRead < sequence
        )));
        settlePackOperations();
      })
      .catch((failure) => {
        if (cancelled) return;
        if (loadedProfile !== accessProfile) {
          setApplications([]);
          setProjects([]);
          setRootEeId("");
        }
        setError(formatProjectionError(failure));
        setLoadedProfile(accessProfile);
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => {
    if (accessProfile !== "live" || !window.octopusTransport?.clientSetupReadCurrent) {
      setClientSetup(null);
      setClientSetupError("");
      return;
    }
    let cancelled = false;
    readCurrentClientSetup()
      .then((setup) => {
        if (cancelled) return;
        setClientSetup(setup);
        setClientSetupError("");
      })
      .catch(() => {
        if (cancelled) return;
        setClientSetup(null);
        setClientSetupError("Application Pack setup status could not be verified.");
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => {
    if (!hasProjectionBridge()) {
      setPacksLoadedProfile(accessProfile);
      return;
    }
    let cancelled = false;
    const sequence = ++packsReadSequence.current;
    readProjection<LocalPackagesProjection>("projection.operator.os.local_packages")
      .then((response) => {
        if (cancelled) return;
        setApplicationPacks(arrayRecords(response.packs));
        setPackError(arrayStrings(response.errors).length ? "One or more local Application Packs could not be verified." : "");
        setPacksLoadedProfile(accessProfile);
        packsReadLanded.current = sequence;
        settlePackOperations();
      })
      .catch(() => {
        if (cancelled) return;
        if (packsLoadedProfile !== accessProfile) setApplicationPacks([]);
        setPackError("Application Packs could not be loaded.");
        setPacksLoadedProfile(accessProfile);
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => onApplicationLifecycleProgress((progress) => {
    const applicationId = applications.find((application) => application.package_id === progress.packageId)?.id
      ?? progress.applicationId;
    setLifecycleOperations((current) => {
      const active = current[applicationId];
      if (!active || active.operationId !== progress.operationId || active.operation !== progress.operation) return current;
      return {
        ...current,
        [applicationId]: {
          ...active,
          phase: progress.phase,
          percent: progress.percent,
          error: "",
        },
      };
    });
    setPackLifecycleOperations((current) => {
      const matching = Object.entries(current).find(([, operation]) => operation.operationId === progress.operationId);
      if (!matching || progress.operation !== "install") return current;
      const [packId, operation] = matching;
      return {
        ...current,
        [packId]: {
          ...operation,
          phase: progress.phase,
          percent: progress.percent,
          error: "",
        },
      };
    });
  }), [applications]);

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    setSelectedId("");
    setSelectedPackId("");
    setRouteDirection("back");
    setCatalogView("browse");
    window.requestAnimationFrame(() => browseSearchRef.current?.focus());
  }, [searchFocusTick]);

  const visible = useMemo(() => {
    const needle = browseQuery.trim().toLowerCase();
    return applications.filter((application) => !needle || applicationSearchText(application).includes(needle));
  }, [applications, browseQuery]);
  const visiblePacks = useMemo(() => {
    const needle = browseQuery.trim().toLowerCase();
    return applicationPacks.filter((pack) => !needle || applicationPackSearchText(pack).includes(needle));
  }, [applicationPacks, browseQuery]);
  const selected = applications.find((application) => application.id === selectedId) ?? null;
  const selectedPack = applicationPacks.find((pack) => pack.id === selectedPackId) ?? null;
  const connectApplication = connectTarget ? applications.find((application) => application.id === connectTarget.applicationId) ?? null : null;
  const removeApplication = applications.find((application) => application.id === removeId) ?? null;
  const installReview = applications.find((application) => application.id === installReviewId && !application.installed) ?? null;
  const connectionSummaries = useMemo(() => new Map(applications.map((application) => [
    application.id,
    applicationConnectionSummary(application, connectionProbes[application.id]),
  ])), [applications, connectionProbes]);
  const summaryOf = (application: ApplicationRecord) => connectionSummaries.get(application.id) ?? applicationConnectionSummary(application, undefined);
  const probeKey = loaded
    ? applications
        .filter((application) => application.installed && (application.connection_interfaces?.length ?? 0) > 0)
        .map((application) => `${application.id}:${(application.connection_interfaces ?? []).join(",")}:${application.installed_digest}`)
        .join("|")
    : "";

  useEffect(() => {
    probeRequests.current.clear();
    setConnectionProbes({});
  }, [accessProfile]);

  useEffect(() => {
    if (installReviewId && !installReview) setInstallReviewId("");
  }, [installReview, installReviewId]);

  useEffect(() => {
    if (!probeKey) return;
    for (const application of applications) {
      if (application.installed && (application.connection_interfaces?.length ?? 0) > 0) refreshConnection(application);
    }
  }, [probeKey]);
  const groups = CATEGORY_ORDER.flatMap((category) => {
    const rows = visible.filter((application) => application.category === category);
    return rows.length ? [{ category, rows }] : [];
  });
  const manageNeedle = manageQuery.trim().toLowerCase();
  const manageApplications = applications.filter((application) => (
    application.category === manageCategory
    && (!manageNeedle || applicationSearchText(application).includes(manageNeedle))
  ));
  const setupStatusByInterface = useMemo(() => {
    const statuses = new Map(extensionRegistry.actionStatuses.map((status) => [status.interfaceId, status]));
    for (const incident of extensionIncidents) {
      if (incident.operation.status === "succeeded") continue;
      const status = statuses.get(incident.interfaceId);
      if (!status) continue;
      statuses.set(incident.interfaceId, {
        ...status,
        state: "sign_in_required",
        code: incident.code,
        message: incident.message,
      });
    }
    return statuses;
  }, [extensionIncidents, extensionRegistry.actionStatuses]);
  const extensionById = useMemo(() => new Map(
    extensionRegistry.extensions.map((extension) => [extension.manifest.id, extension]),
  ), [extensionRegistry.extensions]);
  function onSearchKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const keyboardIds = catalogView === "manage"
      ? manageApplications.map((application) => application.id).filter((id) => manageCardRefs.current.has(id))
      : [
          ...visiblePacks.map((pack) => applicationPackKey(pack.id)).filter((id) => browsePackRefs.current.has(id)),
          ...groups.flatMap((group) => group.rows).map((application) => application.id).filter((id) => browseCardRefs.current.has(id)),
        ];
    if (!keyboardIds.length || !["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
    event.preventDefault();
    const current = Math.max(0, keyboardIds.indexOf(highlightedId));
    const nextIndex = event.key === "ArrowDown"
      ? (current + (highlightedId ? 1 : 0)) % keyboardIds.length
      : event.key === "ArrowUp"
        ? (current - 1 + keyboardIds.length) % keyboardIds.length
        : current;
    const nextId = highlightedId && event.key === "Enter" ? highlightedId : keyboardIds[nextIndex];
    if (event.key === "Enter") {
      if (catalogView === "browse" && nextId.startsWith("pack:")) openApplicationPack(nextId.slice(5));
      else openApplication(nextId, catalogView);
      return;
    }
    setHighlightedId(nextId);
    (catalogView === "manage"
      ? manageCardRefs.current.get(nextId)
      : nextId.startsWith("pack:")
        ? browsePackRefs.current.get(nextId)
        : browseCardRefs.current.get(nextId)
    )?.scrollIntoView({ block: "nearest" });
  }

  function settlePackOperations() {
    const applicationsLanded = applicationsReadLanded.current;
    const packsLanded = packsReadLanded.current;
    setPackLifecycleOperations((current) => withoutSettledOperations(current, (operation) => (
      operation.settlesAfterRead !== null
      && operation.settlesAfterRead.applications < applicationsLanded
      && operation.settlesAfterRead.packs < packsLanded
    )));
  }

  function lifecycleOf(application: ApplicationRecord): ApplicationLifecycleOperation | undefined {
    const own = lifecycleOperations[application.id];
    if (lifecycleBusy(own) || application.installed) return own;
    const pack = Object.values(packLifecycleOperations).find((operation) => (
      lifecycleBusy(operation) && operation.packageIds.includes(application.package_id)
    ));
    return pack
      ? { operationId: pack.operationId, operation: pack.operation, phase: pack.phase, percent: pack.percent, error: "", settlesAfterRead: null, presentation: application }
      : own;
  }

  function refreshConnection(application: ApplicationRecord) {
    const interfaces = application.connection_interfaces ?? [];
    if (!interfaces.length) return;
    const request = (probeRequests.current.get(application.id) ?? 0) + 1;
    probeRequests.current.set(application.id, request);
    const settle = (states: Record<string, OctopusApplicationConnectionState>) => {
      if (probeRequests.current.get(application.id) !== request) return;
      setConnectionProbes((current) => ({ ...current, [application.id]: { checking: false, interfaces: states } }));
    };
    setConnectionProbes((current) => ({ ...current, [application.id]: { checking: true, interfaces: current[application.id]?.interfaces ?? {} } }));
    if (!connectAvailable) {
      settle(Object.fromEntries(interfaces.map((interfaceId) => [interfaceId, { state: "error", message: "Connect applications from the Octopus desktop client." }])));
      return;
    }
    probeApplicationConnection(application.id).then(
      (result) => settle(Object.fromEntries(result.interfaces.map((entry) => [entry.interfaceId, entry.state]))),
      (failure) => {
        const message = formatProjectionError(failure);
        settle(Object.fromEntries(interfaces.map((interfaceId) => [interfaceId, { state: "error", message }])));
      },
    );
  }

  function openConnect(application: ApplicationRecord, mode: ApplicationConnectMode) {
    if (!application.installed || !application.valid) return;
    setConnectTarget((current) => ({ applicationId: application.id, mode, session: (current?.session ?? 0) + 1 }));
  }

  function connectionChanged(applicationId: string) {
    setReloadTick((current) => current + 1);
    const application = applications.find((candidate) => candidate.id === applicationId);
    if (application) refreshConnection(application);
  }

  function connectionActions(application: ApplicationRecord): ApplicationConnectionActions {
    return {
      summary: summaryOf(application),
      connectAvailable,
      disconnecting: disconnectingId === application.id,
      onConnect: (mode) => openConnect(application, mode),
      onDisconnect: () => void disconnectApplication(application),
      onCheck: () => connectionChanged(application.id),
    };
  }

  function openApplication(id: string, returnView: ApplicationCatalogView = catalogView) {
    scrollOffsetRef.current = scrollRef.current?.scrollTop ?? 0;
    detailOriginRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailReturnView(returnView);
    setRouteDirection("forward");
    setSelectedPackId("");
    setSelectedId(id);
    const application = applications.find((candidate) => candidate.id === id);
    if (application?.installed && connectionProbes[id] && !connectionProbes[id].checking) refreshConnection(application);
  }

  function openApplicationPack(id: string) {
    scrollOffsetRef.current = scrollRef.current?.scrollTop ?? 0;
    detailOriginRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailReturnView("browse");
    setRouteDirection("forward");
    setSelectedId("");
    setSelectedPackId(id);
  }

  function closeApplication() {
    setRouteDirection("back");
    setSelectedId("");
    window.requestAnimationFrame(() => {
      if (scrollRef.current) scrollRef.current.scrollTop = scrollOffsetRef.current;
      if (detailOriginRef.current?.isConnected) {
        detailOriginRef.current.focus();
        return;
      }
      const refs = detailReturnView === "manage" ? manageCardRefs : browseCardRefs;
      const card = refs.current.get(selectedId);
      if (card) {
        card.querySelector<HTMLButtonElement>("[data-application-open]")?.focus();
        return;
      }
      (detailReturnView === "manage" ? manageSearchRef : browseSearchRef).current?.focus();
    });
  }

  function closeApplicationPack() {
    const closingPackId = selectedPackId;
    setRouteDirection("back");
    setSelectedPackId("");
    window.requestAnimationFrame(() => {
      if (scrollRef.current) scrollRef.current.scrollTop = scrollOffsetRef.current;
      if (detailOriginRef.current?.isConnected) {
        detailOriginRef.current.focus();
        return;
      }
      browsePackRefs.current.get(applicationPackKey(closingPackId))
        ?.querySelector<HTMLButtonElement>("[data-application-pack-open]")
        ?.focus();
    });
  }

  function openManage() {
    setHighlightedId("");
    setRouteDirection("forward");
    setCatalogView("manage");
    window.requestAnimationFrame(() => manageSearchRef.current?.focus());
  }

  function closeManage() {
    setHighlightedId("");
    setRouteDirection("back");
    setCatalogView("browse");
    window.requestAnimationFrame(() => document.getElementById("applications-browse-manage")?.focus());
  }

  async function saveConfiguration(application: ApplicationRecord, interfaceId: string, updates: Record<string, string>, clears: string[]) {
    const document = application.configuration?.documents.find((candidate) => candidate.interface_id === interfaceId);
    if (!document || (Object.keys(updates).length === 0 && clears.length === 0)) return;
    setConfigurationSaving(application.id);
    try {
      await mutateApplicationPrivateConfiguration({
        applicationId: application.id,
        interfaceId,
        expectedDigest: document.status === "valid" && document.digest ? document.digest : null,
        updates,
        clears,
      });
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setConfigurationSaving("");
      setReloadTick((current) => current + 1);
    }
  }

  async function disconnectApplication(application: ApplicationRecord) {
    const summary = summaryOf(application);
    const results: Record<string, OctopusApplicationConnectionState> = {};
    setDisconnectingId(application.id);
    try {
      for (const interfaceId of summary.moduleInterfaces) {
        results[interfaceId] = await disconnectApplicationInterface(application.id, interfaceId);
      }
      for (const interfaceId of summary.fieldInterfaces) {
        const saved = summary.fields.filter((field) => field.interface_id === interfaceId && field.private_saved).map((field) => field.name);
        const document = application.configuration?.documents.find((candidate) => candidate.interface_id === interfaceId);
        if (!saved.length || !document) continue;
        await mutateApplicationPrivateConfiguration({
          applicationId: application.id,
          interfaceId,
          expectedDigest: document.status === "valid" && document.digest ? document.digest : null,
          updates: {},
          clears: saved,
        });
      }
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      if (Object.keys(results).length) {
        setConnectionProbes((current) => ({
          ...current,
          [application.id]: { checking: false, interfaces: { ...current[application.id]?.interfaces, ...results } },
        }));
      }
      for (const state of Object.values(results)) announceDisconnectResult(application.name, state);
      setDisconnectingId("");
      connectionChanged(application.id);
    }
  }

  async function changeAvailability(application: ApplicationRecord, state: ApplicationProjectState, next: ApplicationAvailabilityChange) {
    if (accessProfile !== "live") {
      setApplications((current) => current.map((candidate) => candidate.id !== application.id
        ? candidate
        : {
            ...candidate,
            project_states: candidate.project_states.map((projectState) => projectState.ee_id !== state.ee_id
              ? projectState
              : { ...projectState, policy_allowed: next.available, selected_by_default: next.available && next.selected }),
          }));
      return;
    }
    if (next.available === state.policy_allowed && next.selected === state.selected_by_default) return;
    setAvailabilitySaving({ applicationId: application.id, eeId: state.ee_id });
    try {
      await mutateOctopusOsWorkspace({
        action: "configure_application",
        applicationId: application.id,
        projects: [{
          eeId: state.ee_id,
          available: next.available,
          selectedByDefault: next.available && next.selected,
          expectedDigest: state.record_digest,
        }],
      });
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setAvailabilitySaving(null);
      setReloadTick((current) => current + 1);
    }
  }

  async function installApplication(application: ApplicationRecord) {
    if (accessProfile !== "live" || installLocked || application.installed || !application.source_digest || application.install_block_reason || lifecycleBusy(lifecycleOf(application))) return;
    const operationId = crypto.randomUUID();
    if (!operationActive) setHeldErrors({ application: error, pack: packError });
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "install", phase: "preflight", percent: 0, error: "", settlesAfterRead: null, presentation: application },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "install_package",
        packageIds: application.install_packages.map((entry) => entry.id),
        expectedSourceDigests: Object.fromEntries(application.install_packages.map((entry) => [entry.id, entry.source_digest])),
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "install", phase: "complete", percent: 100, error: "", settlesAfterRead: applicationsReadSequence.current, presentation: application },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "install",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
          settlesAfterRead: null,
          presentation: application,
        },
      }));
    }
  }

  async function repairApplication(application: ApplicationRecord) {
    const expectation = applicationRepairExpectation(application);
    if (accessProfile !== "live" || installLocked || !expectation || lifecycleBusy(lifecycleOf(application))) return;
    const operationId = crypto.randomUUID();
    if (!operationActive) setHeldErrors({ application: error, pack: packError });
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "repair", phase: "preflight", percent: 0, error: "", settlesAfterRead: null, presentation: application },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "repair_package",
        packageIds: [application.package_id],
        expectedRepairs: { [application.package_id]: expectation },
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "repair", phase: "complete", percent: 100, error: "", settlesAfterRead: applicationsReadSequence.current, presentation: application },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "repair",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
          settlesAfterRead: null,
          presentation: application,
        },
      }));
    }
  }

  function installOrRepairApplication(application: ApplicationRecord) {
    if (lifecycleBusy(lifecycleOf(application))) return;
    if (applicationRepairExpectation(application)) return repairApplication(application);
    if (application.install_packages.length > 1) setInstallReviewId(application.id);
    else return installApplication(application);
  }

  async function removeInstalledApplication(application: ApplicationRecord) {
    if (accessProfile !== "live" || !application.installed || !application.installed_digest || !application.removal_allowed || lifecycleBusy(lifecycleOf(application))) return;
    const operationId = crypto.randomUUID();
    if (!operationActive) setHeldErrors({ application: error, pack: packError });
    setRemoveId("");
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "remove", phase: "preflight", percent: 0, error: "", settlesAfterRead: null, presentation: application },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "remove_application",
        applicationId: application.id,
        expectedInstalledDigest: application.installed_digest,
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "remove", phase: "complete", percent: 100, error: "", settlesAfterRead: applicationsReadSequence.current, presentation: application },
      }));
      if (selectedId === application.id) closeApplication();
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "remove",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
          settlesAfterRead: null,
          presentation: application,
        },
      }));
    }
  }

  async function applyApplicationPack(pack: ApplicationPackRecord) {
    const selectedRecommended = selectedPackRecommendations(pack, packRecommendedSelections[pack.id]);
    if (accessProfile !== "live" || installLocked || applicationPackInstallBlockReason(pack, selectedRecommended) || lifecycleBusy(packLifecycleOperations[pack.id])) return;
    const operationId = crypto.randomUUID();
    if (!operationActive) setHeldErrors({ application: error, pack: packError });
    const packageIds = pack.components.flatMap((component) => (
      component.selection === "included" || selectedRecommended.includes(component.package_id)
        ? [component.package_id]
        : []
    ));
    const expectedSourceDigests = Object.fromEntries(packageIds.map((packageId) => {
      const component = pack.components.find((candidate) => candidate.package_id === packageId);
      if (!component?.source_digest) throw new Error(`${pack.name} has an unavailable component.`);
      return [packageId, component.source_digest];
    }));
    const presentation = { pack, applications: applications.filter((application) => packageIds.includes(application.package_id)) };
    setPackLifecycleOperations((current) => ({
      ...current,
      [pack.id]: { operationId, operation: "install", phase: "preflight", percent: 0, error: "", packageIds, settlesAfterRead: null, presentation },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "apply_application_pack",
        packId: pack.id,
        selectedRecommended,
        expectedSourceDigests,
        operationId,
      });
      setPackLifecycleOperations((current) => ({
        ...current,
        [pack.id]: {
          operationId,
          operation: "install",
          phase: "complete",
          percent: 100,
          error: "",
          packageIds,
          settlesAfterRead: { applications: applicationsReadSequence.current, packs: packsReadSequence.current },
          presentation,
        },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setPackLifecycleOperations((current) => ({
        ...current,
        [pack.id]: {
          operationId,
          operation: "install",
          phase: "failed",
          percent: current[pack.id]?.percent ?? 0,
          error: formatProjectionError(failure),
          packageIds,
          settlesAfterRead: null,
          presentation,
        },
      }));
    }
  }

  if (!hasProjectionBridge()) return <SurfaceState title="Applications are unavailable" detail="Start Octopus to manage installed applications." />;
  const detailOpen = Boolean(selected || selectedPack);
  return (
    <>
      <div className="relative h-full min-h-0">
        <div aria-hidden={detailOpen || undefined} className={cx("h-full min-h-0", detailOpen && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={`applications-${catalogView}`}>
            {!loaded || catalogView === "browse" && !packsLoaded ? (
              <OctopusLoadingState className="surface-content-loading-state" label="Loading applications" />
            ) : <section className="flex h-full min-h-0 flex-col text-base select-none">
              <header className="surface-titlebar-spacer draggable flex h-toolbar shrink-0 items-center justify-between gap-2 px-panel">
                {catalogView === "manage" ? (
                  <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground focus:outline-none" onClick={closeManage} type="button">
                    <span>Applications</span>
                    <span className="text-token-input-placeholder-foreground">/</span>
                    <span className="text-token-foreground">Manage</span>
                  </button>
                ) : (
                  <span className="no-drag flex min-w-0 items-center rounded-lg px-2 py-1 text-sm text-token-foreground">Applications</span>
                )}
                <div className="no-drag flex min-w-0 items-center justify-end gap-2">
                  {catalogView === "browse" ? (
                    <OcIconButton glyph="settings" id="applications-browse-manage" label="Manage applications" onClick={openManage} size="sm" variant="ghost" />
                  ) : null}
                </div>
              </header>
              <div className="plugins-surface-scroll relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" ref={scrollRef}>
                <div className="flex min-h-full w-full flex-col">
                  {catalogView === "browse" ? (
                    <>
                      <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel">
                        <div className="flex items-start justify-between gap-4 px-2">
                          <div className="flex min-w-0 flex-col gap-2">
                            <h1 className="heading-xl font-normal text-default">Applications</h1>
                            <div className="text-lg leading-6 text-secondary">Connect Octopus to the tools and context you use</div>
                          </div>
                        </div>
                      </div>
                      <div className="sticky top-0 z-30 bg-surface after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-surface after:to-transparent after:content-['']">
                        <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] items-center gap-2 px-panel pt-panel pb-2">
                          <OcSearchField autoFocus className="min-w-0 flex-1" id="applications-page-search" inputRef={browseSearchRef} label="Search applications" onChange={(value) => {
                            setBrowseQuery(value);
                            setHighlightedId("");
                          }} onKeyDown={onSearchKeyDown} placeholder="Search applications" value={browseQuery} />
                        </div>
                      </div>
                    </>
                  ) : null}
                  <motion.div className={cx("mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pb-panel", catalogView === "manage" ? "gap-3 pt-panel" : "gap-8 pt-5")} layout="position" transition={reducedMotion ? { duration: 0 } : PAGE_LAYOUT_TRANSITION}>
                    {catalogView === "manage" ? (
                      <div className="flex flex-wrap items-center justify-between gap-3">
                        <OcSegmented label="Manage applications" onChange={(category) => {
                          setManageCategory(category);
                          setHighlightedId("");
                        }} options={CATEGORY_ORDER.map((category) => ({
                          value: category,
                          label: <>{CATEGORY_COPY[category].title}<span className="ms-0.5 text-tertiary">{applications.filter((application) => application.category === category).length}</span></>,
                        }))} size="sm" value={manageCategory} />
                        <div className="w-56 min-w-0 max-w-full">
                          <OcSearchField className="shadow-none" id="applications-page-manage-search" inputRef={manageSearchRef} label="Search applications" onChange={(value) => {
                            setManageQuery(value);
                            setHighlightedId("");
                          }} onKeyDown={onSearchKeyDown} placeholder="Search" value={manageQuery} />
                        </div>
                      </div>
                    ) : null}
                    {loaded && shownError && applications.length ? <ApplicationProjectionIssue detail={shownError} /> : null}
                    {packsLoaded && shownPackError && applicationPacks.length ? <ApplicationProjectionIssue detail={shownPackError} /> : null}
                    {shownError && applications.length === 0 && (catalogView === "manage" || shownPackError && applicationPacks.length === 0) ? (
                      <SurfaceState compact title="Applications could not be projected" detail={[shownError, shownPackError].filter(Boolean).join("\n")} actionLabel="Retry" onAction={() => setReloadTick((current) => current + 1)} />
                    ) : applications.length === 0 && (catalogView === "manage" || applicationPacks.length === 0) ? (
                      <SurfaceState compact title="No application packages found" detail="No verified source or installed interface package is available." actionLabel="Refresh catalog" onAction={() => setReloadTick((current) => current + 1)} />
                    ) : (catalogView === "browse" && visible.length === 0 && visiblePacks.length === 0) || (catalogView === "manage" && manageApplications.length === 0 && manageNeedle) ? (
                      <SurfaceState compact title="No applications found" />
                    ) : catalogView === "manage" ? (
                      <>
                        {manageApplications.length ? (
                          <ManageApplicationsList accessProfile={accessProfile} applicationRefs={manageCardRefs} applications={manageApplications} connectionFor={connectionActions} highlightedId={highlightedId} installLocked={installLocked} lifecycleFor={lifecycleOf} onInstall={(application) => void installOrRepairApplication(application)} onOpen={(id) => openApplication(id, "manage")} onRemove={(application) => setRemoveId(application.id)} setupStatusByInterface={setupStatusByInterface} />
                        ) : <SurfaceState compact title={`No ${CATEGORY_COPY[manageCategory].title.toLowerCase()} found`} />}
                      </>
                    ) : (
                      <>
                        {visiblePacks.length ? (
                          <CatalogSection
                            description="Install a reviewed set of applications, local capabilities, and context"
                            getItemKey={(pack) => pack.id}
                            getItemLabel={(pack) => pack.name}
                            items={visiblePacks}
                            renderCard={(pack) => (
                              <ApplicationPackCard
                                accessProfile={accessProfile}
                                clientSetup={clientSetup}
                                clientSetupError={clientSetupError}
                                highlighted={highlightedId === applicationPackKey(pack.id)}
                                installLocked={installLocked}
                                lifecycle={packLifecycleOperations[pack.id]}
                                onApply={() => void applyApplicationPack(pack)}
                                onOpen={() => openApplicationPack(pack.id)}
                                pack={pack}
                                setRef={(node) => {
                                  const key = applicationPackKey(pack.id);
                                  if (node) browsePackRefs.current.set(key, node);
                                  else browsePackRefs.current.delete(key);
                                }}
                                setupStatus={applicationPackSetupStatus(pack, setupStatusByInterface)}
                              />
                            )}
                            resetKey={`packs:${browseQuery}`}
                            showCount={Boolean(browseQuery.trim())}
                            title="Application Packs"
                            visibleItemLimit={3}
                          />
                        ) : null}
                        <CatalogInstalledRail
                          emptyLabel="No applications installed yet"
                          getItemKey={(application) => application.id}
                          getItemLabel={(application) => application.name}
                          items={applications.filter((application) => application.installed)}
                          manageLabel="Manage applications"
                          onManage={openManage}
                          onOpen={(application) => openApplication(application.id, "browse")}
                          renderIcon={(application, size) => <ApplicationLogo application={application} className="size-full" logoSize={size === "mini" ? 8 : 18} />}
                        />
                        {groups.map(({ category, rows }) => (
                          <CatalogSection
                            description={CATEGORY_COPY[category].description}
                            getItemKey={(application) => application.id}
                            getItemLabel={(application) => application.name}
                            items={rows}
                            key={category}
                            renderCard={(application) => (
                              <ApplicationCard accessProfile={accessProfile} application={application} connection={connectionActions(application)} highlighted={highlightedId === application.id} installLocked={installLocked} lifecycle={lifecycleOf(application)} onInstall={() => void installOrRepairApplication(application)} onOpen={(id) => openApplication(id, "browse")} onRemove={() => setRemoveId(application.id)} setRef={(node) => {
                                if (node) browseCardRefs.current.set(application.id, node);
                                else browseCardRefs.current.delete(application.id);
                              }} setupStatus={applicationSetupStatus(application, setupStatusByInterface)} />
                            )}
                            renderItemPreview={(application) => <ApplicationMiniLogo application={application} />}
                            resetKey={`${category}:${browseQuery}`}
                            showCount={Boolean(browseQuery.trim())}
                            title={CATEGORY_COPY[category].title}
                          />
                        ))}
                      </>
                    )}
                  </motion.div>
                </div>
              </div>
            </section>}
          </CatalogRouteTransition>
        </div>
        <div aria-hidden={!detailOpen || undefined} className={cx("h-full min-h-0", !detailOpen && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={selectedPack ? `application-pack-detail:${selectedPack.id}` : selected ? `application-detail:${selected.id}` : "application-detail-empty"}>
            {selectedPack ? (
              <ApplicationPackDetail
                accessProfile={accessProfile}
                extensionById={extensionById}
                clientSetup={clientSetup}
                clientSetupError={clientSetupError}
                installLocked={installLocked}
                lifecycle={packLifecycleOperations[selectedPack.id]}
                onApply={() => void applyApplicationPack(selectedPack)}
                onBack={closeApplicationPack}
                onRecommendedChange={(packageId, checked) => setPackRecommendedSelections((current) => ({
                  ...current,
                  [selectedPack.id]: togglePackRecommendation(selectedPack, current[selectedPack.id], packageId, checked),
                }))}
                pack={selectedPack}
                selectedRecommended={selectedPackRecommendations(selectedPack, packRecommendedSelections[selectedPack.id])}
                setupStatus={applicationPackSetupStatus(selectedPack, setupStatusByInterface)}
              />
            ) : selected ? (
              <ApplicationDetail
                accessProfile={accessProfile}
                application={selected}
                availabilitySavingEeId={availabilitySaving?.applicationId === selected.id ? availabilitySaving.eeId : ""}
                configurationSaving={configurationSaving === selected.id}
                connection={connectionActions(selected)}
                installLocked={installLocked}
                lifecycle={lifecycleOf(selected)}
                onAvailabilityChange={(state, next) => void changeAvailability(selected, state, next)}
                onBack={closeApplication}
                onInstall={() => void installOrRepairApplication(selected)}
                onRemove={() => setRemoveId(selected.id)}
                onSaveConfiguration={(interfaceId, updates, clears) => void saveConfiguration(selected, interfaceId, updates, clears)}
                projects={projects}
                rootEeId={rootEeId}
                setupStatus={applicationSetupStatus(selected, setupStatusByInterface)}
              />
            ) : <div className="h-full min-h-0" />}
          </CatalogRouteTransition>
        </div>
      </div>
      <ApplicationConnectDialog
        application={connectApplication}
        availabilitySavingEeId={connectApplication && availabilitySaving?.applicationId === connectApplication.id ? availabilitySaving.eeId : ""}
        mode={connectTarget?.mode ?? "connect"}
        moduleStates={connectApplication ? connectionProbes[connectApplication.id]?.interfaces ?? {} : {}}
        onAvailabilityChange={(application, state, next) => void changeAvailability(application, state, next)}
        onChanged={connectionChanged}
        onClose={() => setConnectTarget(null)}
        projects={projects}
        rootEeId={rootEeId}
        sessionKey={connectTarget?.session ?? 0}
        summary={connectApplication ? summaryOf(connectApplication) : null}
      />
      <ApplicationRemovalDialog application={removeApplication} lifecycle={removeApplication ? lifecycleOf(removeApplication) : undefined} onCancel={() => setRemoveId("")} onConfirm={() => removeApplication && void removeInstalledApplication(removeApplication)} />
      <PackageInstallReviewDialog
        onCancel={() => setInstallReviewId("")}
        onConfirm={() => {
          const application = installReview;
          setInstallReviewId("");
          if (application) void installApplication(application);
        }}
        review={installReview ? {
          developer: installReview.developer,
          logo: <ApplicationLogo application={installReview} className="size-14 overflow-hidden rounded-xl border border-default shadow-sm" logoSize={26} />,
          name: installReview.name,
          note: "After installation, choose where it is available.",
          packages: installReview.install_packages,
        } : null}
      />
    </>
  );
}

function ApplicationPackCard({ accessProfile, clientSetup, clientSetupError, highlighted, installLocked, lifecycle, onApply, onOpen, pack, setRef, setupStatus }: {
  accessProfile: OctopusClientAccessProfile;
  clientSetup: OctopusClientSetupState | null;
  clientSetupError: string;
  highlighted: boolean;
  installLocked: boolean;
  lifecycle?: ApplicationPackLifecycleOperation;
  onApply: () => void;
  onOpen: () => void;
  pack: ApplicationPackRecord;
  setRef: (node: HTMLDivElement | null) => void;
  setupStatus?: ClientExtensionActionStatus;
}) {
  const inventory = applicationPackInventory(pack);
  const needsApply = applicationPackNeedsApply(pack);
  const busy = lifecycle && lifecycle.phase !== "failed";
  const blockReason = applicationPackInstallBlockReason(pack);
  const actionLabel = lifecycle?.phase === "failed"
    ? "Retry"
    : inventory.state === "partial"
      ? "Finish install"
      : inventory.ready
        ? "Add recommended"
        : "Apply pack";
  return (
    <div
      className={cx(
        "group/card flex items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5",
        highlighted && "bg-text/5",
      )}
      data-search-result-active={highlighted || undefined}
      ref={setRef}
      style={{ gridColumn: "1 / -1" }}
    >
      <button className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 rounded-xl p-0 text-start outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0" data-application-pack-open onClick={onOpen} type="button">
        <ApplicationPackLogo />
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-medium text-token-foreground">{pack.name}</span>
            <OcStatus label="Pack" tone="neutral" />
          </span>
          <span className="mt-0.5 block line-clamp-2 text-sm leading-relaxed text-text-description">{pack.description}</span>
          <ApplicationPackFactBadges clientSetup={clientSetup} clientSetupError={clientSetupError} compact pack={pack} setupStatus={setupStatus} />
        </span>
      </button>
      {busy && lifecycle ? <ApplicationLifecycleButton operation={lifecycle} variant="ghost" /> : needsApply ? (
        <div className="flex shrink-0 flex-col items-end gap-1">
          <OcButton
            disabled={accessProfile !== "live" || installLocked || Boolean(blockReason)}
            onClick={(event) => {
              event.stopPropagation();
              onApply();
            }}
            size="sm"
            title={accessProfile !== "live" ? "Apply packs from the Administrator profile." : blockReason || (installLocked ? INSTALL_LOCKED : `Apply ${pack.name}`)}
            variant="outline"
          >
            {actionLabel}
          </OcButton>
          {lifecycle?.phase === "failed" ? <span className="max-w-48 truncate text-xs text-token-error-foreground">Install failed</span> : null}
        </div>
      ) : null}
    </div>
  );
}

function ApplicationPackDetail({ accessProfile, clientSetup, clientSetupError, extensionById, installLocked, lifecycle, onApply, onBack, onRecommendedChange, pack, selectedRecommended, setupStatus }: {
  accessProfile: OctopusClientAccessProfile;
  clientSetup: OctopusClientSetupState | null;
  clientSetupError: string;
  extensionById: Map<string, ClientExtensionRecord>;
  installLocked: boolean;
  lifecycle?: ApplicationPackLifecycleOperation;
  onApply: () => void;
  onBack: () => void;
  onRecommendedChange: (packageId: string, checked: boolean) => void;
  pack: ApplicationPackRecord;
  selectedRecommended: string[];
  setupStatus?: ClientExtensionActionStatus;
}) {
  const included = pack.components.filter((component) => component.selection === "included");
  const recommendedComponents = pack.components.filter((component) => component.selection === "recommended");
  const recommendedExtensions = pack.extensions.filter((extension) => extension.selection === "recommended");
  const inventory = applicationPackInventory(pack);
  const needsApply = applicationPackNeedsApply(pack, selectedRecommended);
  const busy = lifecycle && lifecycle.phase !== "failed";
  const blockReason = applicationPackInstallBlockReason(pack, selectedRecommended);
  const actionLabel = lifecycle?.phase === "failed"
    ? "Retry"
    : inventory.state === "partial"
      ? "Finish install"
      : inventory.ready
        ? "Add recommended"
        : "Apply pack";
  const projectDefaults = pack.workspace_setup?.project_defaults.flatMap((entry) => {
    if (!entry.default_for_new_tasks) return [];
    const component = pack.components.find((candidate) => candidate.package_id === entry.package_id);
    return component ? [component.name] : [];
  }) ?? [];
  const homeProviderName = pack.components.find((component) => (
    component.product_id === pack.workspace_setup?.home.provider_interface_id
  ))?.name;
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground focus:outline-none" onClick={onBack} type="button">
          <span>Applications</span>
          <span className="text-token-input-placeholder-foreground">/</span>
          <span className="truncate text-token-foreground">{pack.name}</span>
        </button>
      </header>
      <div className="plugins-surface-scroll vertical-scroll-fade-mask relative min-h-0 w-full flex-1 overflow-x-hidden overflow-y-auto [--edge-fade-distance:1rem] [scrollbar-gutter:stable]">
        <div className="mx-auto flex min-h-full w-full max-w-[var(--thread-content-max-width)] flex-col px-toolbar pt-panel pb-panel">
          <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
            <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
              <ApplicationPackLogo large />
              <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
                <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
                  <div className="flex min-w-0 items-center gap-2">
                    <h1 className="heading-base min-w-0 break-words text-token-foreground electron:heading-lg">{pack.name}</h1>
                    <OcStatus label="Application Pack" tone="neutral" />
                  </div>
                  <div className="text-base text-token-text-secondary">{pack.description}</div>
                </div>
                <div className="flex max-w-full shrink-0 items-center gap-2">
                  <ApplicationPackFactBadges clientSetup={clientSetup} clientSetupError={clientSetupError} pack={pack} setupStatus={setupStatus} />
                  <OcAnimatedActionSlot>
                    {busy && lifecycle ? <ApplicationLifecycleButton operation={lifecycle} variant="primary" /> : needsApply ? (
                      <OcButton
                        disabled={accessProfile !== "live" || installLocked || Boolean(blockReason)}
                        leadingIcon="download"
                        onClick={onApply}
                        size="sm"
                        title={accessProfile !== "live" ? "Apply packs from the Administrator profile." : blockReason || (installLocked ? INSTALL_LOCKED : `Apply ${pack.name}`)}
                        variant="primary"
                      >
                        {actionLabel}
                      </OcButton>
                    ) : null}
                  </OcAnimatedActionSlot>
                </div>
              </div>
            </header>
            {lifecycle?.phase === "failed" ? (
              <div className="mx-[var(--detail-page-inline-inset,0px)] rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
                <div className="font-medium">Pack installation failed</div>
                <div className="mt-1 text-xs opacity-90">Octopus could not finish applying this pack. Retry after resolving the incomplete item.</div>
              </div>
            ) : null}
            <DetailSection title="Setup">
              <div className="flex flex-col gap-2">
                <DetailRow label="Installed" value={`${inventory.installedCount} of ${inventory.includedCount} included components are fully installed and verified.`} />
                {applicationPackSetupVerified(pack, clientSetup) ? <DetailRow label="Workspace setup" value="Home, selected projects, local capabilities, and provider account access were verified together." /> : null}
                {setupStatus && inventory.ready ? <DetailRow label="Provider setup" value={setupStatus.message} /> : null}
                {homeProviderName ? <DetailRow label="Home" value={`During first setup, this pack connects Home through ${homeProviderName}.`} /> : null}
                <DetailRow label="Welcome defaults" value={projectDefaults.length ? `During first setup: ${projectDefaults.join(", ")}. Applying the pack here does not change your project choices.` : "This pack does not suggest any defaults during first setup."} />
                {recommendedExtensions.length ? <DetailRow label="Optional additions" value="Recommended client additions remain independent and can be installed from Extensions." /> : null}
              </div>
            </DetailSection>
            <DetailSection title="What’s in this pack">
              <div className="flex flex-col gap-2">
                <ApplicationPackDisclosure count={included.length} label="Included" open>
                  {included.map((component) => <ApplicationPackComponentRow component={component} key={component.package_id} />)}
                </ApplicationPackDisclosure>
                {recommendedComponents.length || recommendedExtensions.length ? (
                  <ApplicationPackDisclosure count={recommendedComponents.length + recommendedExtensions.length} label="Recommended">
                    {recommendedComponents.map((component) => (
                      <ApplicationPackComponentRow
                        component={component}
                        key={component.package_id}
                        selection={<OcSwitch checked={selectedRecommended.includes(component.package_id)} disabled={Boolean(busy) || !component.available} label={`Include ${component.name}`} onCheckedChange={(checked) => onRecommendedChange(component.package_id, checked)} />}
                      />
                    ))}
                    {recommendedExtensions.map((extension) => <ApplicationPackExtensionRow extension={extension} extensionRecord={extensionById.get(extension.extension_id)} key={extension.extension_id} />)}
                  </ApplicationPackDisclosure>
                ) : null}
              </div>
            </DetailSection>
            <DetailSection title="Permissions">
              <div className="flex flex-col gap-1">
                {included.map((component) => (
                  <DetailRow key={component.package_id} label={applicationPackPermissionLabel(component)} value={applicationPackPermissionDetail(component)} />
                ))}
                <DetailRow label="Project access" value="Capabilities are available only in Home and projects where you allow them; applying the pack does not turn them on for every task." />
              </div>
            </DetailSection>
            <DetailSection title="Information">
              <div className="flex flex-col gap-1">
                <DetailRow label="Version" value={pack.version} />
                <DetailRow label="Source" value="Reviewed local Application Pack" />
              </div>
            </DetailSection>
          </div>
        </div>
      </div>
    </section>
  );
}

function ApplicationPackDisclosure({ children, count, label, open = false }: { children: ReactNode; count: number; label: string; open?: boolean }) {
  return (
    <details className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]" open={open}>
      <summary className="flex cursor-interaction list-none items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)] marker:hidden [&::-webkit-details-marker]:hidden">
        <span className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">
          {label}
          <Glyph className="icon-2xs -rotate-90 transition-transform group-open/section:rotate-0 motion-reduce:transition-none" name="chevronDown" size={12} />
          <span className="text-tertiary">{count}</span>
        </span>
      </summary>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">{children}</div>
    </details>
  );
}

function ApplicationPackComponentRow({ component, selection }: { component: ApplicationPackComponent; selection?: ReactNode }) {
  const status = applicationPackComponentStatus(component);
  return (
    <div className="flex min-h-14 items-center gap-3 rounded-xl px-2 py-2 hover:bg-token-foreground/3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name={applicationPackComponentGlyph(component)} size={17} /></span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-token-foreground">{component.name}</span>
        <span className="block text-xs leading-4 text-token-description-foreground">{applicationPackComponentDescription(component)}</span>
      </span>
      {selection ?? <OcStatus label={status.label} tone={status.tone} />}
    </div>
  );
}

function ApplicationPackExtensionRow({ extension, extensionRecord }: { extension: ApplicationPackExtension; extensionRecord?: ClientExtensionRecord }) {
  const installed = extensionRecord?.installed === true;
  const configured = extensionRecord?.configured === true;
  return (
    <div className="flex min-h-14 items-center gap-3 rounded-xl px-2 py-2 hover:bg-token-foreground/3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name={extensionRecord?.manifest.glyph ?? "plugins"} size={17} /></span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-token-foreground">{extensionRecord?.manifest.name ?? "Client extension"}</span>
        <span className="block text-xs leading-4 text-token-description-foreground">{extension.reason}</span>
      </span>
      <span className="flex shrink-0 flex-wrap justify-end gap-1">
        <OcStatus label={installed ? "Installed" : "Available in Extensions"} tone={installed ? "success" : "neutral"} />
        {installed && !configured ? <OcStatus label="Needs setup" tone="warning" /> : null}
      </span>
    </div>
  );
}

function ApplicationPackLogo({ large = false }: { large?: boolean }) {
  return (
    <span className={cx(
      "flex shrink-0 items-center justify-center overflow-hidden border border-default text-secondary",
      large ? "size-[60px] rounded-2xl bg-transparent" : "size-10 rounded-lg bg-transparent",
    )}>
      <Glyph name="layers" size={large ? 28 : 24} />
    </span>
  );
}

function ApplicationPackFactBadges({ clientSetup, clientSetupError, compact = false, pack, setupStatus }: { clientSetup: OctopusClientSetupState | null; clientSetupError: string; compact?: boolean; pack: ApplicationPackRecord; setupStatus?: ClientExtensionActionStatus }) {
  return (
    <span className={cx("flex min-w-0 flex-wrap items-center gap-1", compact && "mt-1")}>
      {applicationPackFacts(pack, setupStatus, clientSetup, clientSetupError).map((fact) => <OcStatus key={fact.label} label={fact.label} tone={fact.tone} />)}
    </span>
  );
}

type ApplicationConnectionActions = {
  summary: ApplicationConnectionSummary;
  connectAvailable: boolean;
  disconnecting: boolean;
  onConnect: (mode: ApplicationConnectMode) => void;
  onDisconnect: () => void;
  onCheck: () => void;
};

function ManageApplicationsList({ accessProfile, applicationRefs, applications, connectionFor, highlightedId, installLocked, lifecycleFor, onInstall, onOpen, onRemove, setupStatusByInterface }: {
  accessProfile: OctopusClientAccessProfile;
  applicationRefs: RefObject<Map<string, HTMLElement>>;
  applications: ApplicationRecord[];
  connectionFor: (application: ApplicationRecord) => ApplicationConnectionActions;
  highlightedId: string;
  installLocked: boolean;
  lifecycleFor: (application: ApplicationRecord) => ApplicationLifecycleOperation | undefined;
  onInstall: (application: ApplicationRecord) => void;
  onOpen: (id: string) => void;
  onRemove: (application: ApplicationRecord) => void;
  setupStatusByInterface: Map<string, ClientExtensionActionStatus>;
}) {
  return (
    <div className="grid grid-cols-1 gap-2">
      {applications.map((application) => (
        <ApplicationCard
          accessProfile={accessProfile}
          application={application}
          connection={connectionFor(application)}
          highlighted={highlightedId === application.id}
          installLocked={installLocked}
          key={application.id}
          lifecycle={lifecycleFor(application)}
          onInstall={() => onInstall(application)}
          onOpen={onOpen}
          onRemove={() => onRemove(application)}
          setupStatus={applicationSetupStatus(application, setupStatusByInterface)}
          view="manage"
          setRef={(node) => {
            if (node) applicationRefs.current?.set(application.id, node);
            else applicationRefs.current?.delete(application.id);
          }}
        />
      ))}
    </div>
  );
}

function ApplicationCard({
  accessProfile,
  application,
  connection,
  highlighted,
  installLocked,
  lifecycle,
  onInstall,
  onOpen,
  onRemove,
  setupStatus,
  setRef,
  view = "browse",
}: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  connection: ApplicationConnectionActions;
  highlighted: boolean;
  installLocked: boolean;
  lifecycle?: ApplicationLifecycleOperation;
  onInstall: () => void;
  onOpen: (id: string) => void;
  onRemove: () => void;
  setupStatus?: ClientExtensionActionStatus;
  setRef?: (node: HTMLDivElement | null) => void;
  view?: ApplicationCatalogView;
}) {
  return (
    <div
      className={cx(
        "group/card flex items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5",
        highlighted && "bg-text/5",
      )}
      data-search-result-active={highlighted || undefined}
      ref={setRef}
    >
      <button className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 rounded-xl p-0 text-start outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0" data-application-open onClick={() => onOpen(application.id)} type="button">
        <ApplicationLogo application={application} className="size-10 overflow-hidden rounded-lg border border-default bg-transparent" logoSize={24} />
        <span className={cx("flex min-w-0 flex-1 flex-col justify-center", view === "browse" ? "gap-px" : "gap-0.5")}>
          <span className="block truncate font-medium text-default">{application.name}</span>
          <span className="block line-clamp-1 text-sm leading-relaxed text-text-description">{application.description}</span>
          <ApplicationCardMeta application={application} setupStatus={setupStatus} summary={connection.summary} />
        </span>
      </button>
      <ApplicationCardActions accessProfile={accessProfile} application={application} connection={connection} installLocked={installLocked} lifecycle={lifecycle} onInstall={onInstall} onOpen={() => onOpen(application.id)} onRemove={onRemove} view={view} />
    </div>
  );
}

function ApplicationCardActions({ accessProfile, application, connection, installLocked, lifecycle, onInstall, onOpen, onRemove, view }: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  connection: ApplicationConnectionActions;
  installLocked: boolean;
  lifecycle?: ApplicationLifecycleOperation;
  onInstall: () => void;
  onOpen: () => void;
  onRemove: () => void;
  view: ApplicationCatalogView;
}) {
  const available = application.project_states.some((state) => state.policy_allowed);
  const repairable = Boolean(applicationRepairExpectation(application));
  if (lifecycle && lifecycleBusy(lifecycle)) return <ApplicationLifecycleButton operation={lifecycle} variant="ghost" />;
  if (!application.installed) return (
    <div className="flex shrink-0 flex-col items-end gap-1">
      <OcButton
        disabled={accessProfile !== "live" || installLocked || !repairable && Boolean(application.install_block_reason)}
        onClick={(event) => {
          event.stopPropagation();
          onInstall();
        }}
        size="sm"
        title={accessProfile !== "live"
          ? `${repairable ? "Repair" : "Install"} applications from the Administrator profile.`
          : installLocked ? INSTALL_LOCKED : repairable ? "Repair application package" : application.install_block_reason || "Install application"}
        variant="outline"
      >
        {lifecycle?.phase === "failed" ? `Retry ${repairable ? "repair" : "install"}` : repairable ? "Repair" : "Install"}
      </OcButton>
      {lifecycle?.phase === "failed" ? <span className="max-w-48 truncate text-xs text-token-error-foreground" title={lifecycle.error}>{lifecycle.operation === "repair" ? "Repair failed" : "Install failed"}</span> : null}
    </div>
  );
  return (
    <div className="flex shrink-0 flex-col items-end gap-1">
      <div className="flex items-center gap-1">
        {!application.valid || connection.summary.status === "none" ? (
          <OcButton
            className="shrink-0"
            onClick={(event) => {
              event.stopPropagation();
              onOpen();
            }}
            size="xs"
            variant="ghost"
          >
            {!application.valid ? "View issue" : available ? "Manage" : "Set up"}
          </OcButton>
        ) : (
          <ApplicationConnectionControl
            connectAvailable={connection.connectAvailable}
            disconnecting={connection.disconnecting}
            name={application.name}
            onCheck={connection.onCheck}
            onConnect={connection.onConnect}
            onDisconnect={connection.onDisconnect}
            placement="card"
            summary={connection.summary}
          />
        )}
        {view === "manage" ? <ApplicationMoreActions accessProfile={accessProfile} application={application} onRemove={onRemove} /> : null}
      </div>
      {lifecycle?.phase === "failed" && lifecycle.operation === "remove" ? <span className="max-w-48 truncate text-xs text-token-error-foreground" title={lifecycle.error}>Remove failed</span> : null}
    </div>
  );
}

function ApplicationMoreActions({ accessProfile, application, onRemove }: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  onRemove: () => void;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const blocked = accessProfile !== "live" ? "Remove applications from the Administrator profile." : application.removal_block_reason;
  return (
    <>
      <OcIconButton
        buttonRef={triggerRef}
        glyph="more"
        label="More actions"
        onClick={(event) => {
          event.stopPropagation();
          setOpen(true);
        }}
        size="sm"
        variant="ghost"
      />
      <OcDropdown align="end" anchorRef={triggerRef} contentWidth="menu" onOpenChange={setOpen} open={open}>
        <OcMenuItem
          description={blocked || undefined}
          descriptionAllowWrap
          disabled={Boolean(blocked) || !application.removal_allowed}
          label={`Remove ${application.name}`}
          leadingIcon="trash"
          onClick={() => {
            setOpen(false);
            onRemove();
          }}
          role="menuitem"
          tone="danger"
        />
      </OcDropdown>
    </>
  );
}

function ApplicationDetail({
  accessProfile,
  application,
  availabilitySavingEeId,
  configurationSaving,
  connection,
  installLocked,
  lifecycle,
  onAvailabilityChange,
  onBack,
  onInstall,
  onRemove,
  onSaveConfiguration,
  projects,
  rootEeId,
  setupStatus,
}: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  availabilitySavingEeId: string;
  configurationSaving: boolean;
  connection: ApplicationConnectionActions;
  installLocked: boolean;
  lifecycle?: ApplicationLifecycleOperation;
  onAvailabilityChange: (state: ApplicationProjectState, next: ApplicationAvailabilityChange) => void;
  onBack: () => void;
  onInstall: () => void;
  onRemove: () => void;
  onSaveConfiguration: (interfaceId: string, updates: Record<string, string>, clears: string[]) => void;
  projects: ApplicationProject[];
  rootEeId: string;
  setupStatus?: ClientExtensionActionStatus;
}) {
  const summary = connection.summary;
  const partiallyConnectedRunCount = application.partially_connected_run_ids?.length ?? 0;
  const busy = lifecycle && lifecycle.phase !== "failed";
  const repairable = Boolean(applicationRepairExpectation(application));
  const usable = application.installed && application.valid;
  const environmentFields = (application.configuration?.fields ?? []).filter((field) => field.authored === false);
  const help = application.connection?.help ?? null;
  const reducedMotion = useReducedMotion();
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-secondary hover:bg-primary-ghost-hover hover:text-default focus:outline-none" onClick={onBack} type="button">
          <span>Applications</span>
          <span className="text-tertiary">/</span>
          <span className="truncate text-default">{application.name}</span>
        </button>
      </header>
      <div className="plugins-surface-scroll vertical-scroll-fade-mask relative min-h-0 w-full flex-1 overflow-x-hidden overflow-y-auto [--edge-fade-distance:1rem] [scrollbar-gutter:stable]">
        <div className="mx-auto flex min-h-full w-full max-w-[var(--thread-content-max-width)] flex-col px-toolbar pt-panel pb-panel">
          <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
            <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
              <ApplicationLogo application={application} className="size-[60px] overflow-hidden rounded-2xl border border-default bg-transparent" logoSize={28} />
              <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
                <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
                  <h1 className="heading-base min-w-0 break-words text-default electron:heading-lg">{application.name}</h1>
                  <div className="text-base text-secondary">{application.description}</div>
                </div>
                <div className="flex max-w-full shrink-0 items-center gap-2">
                  <ApplicationFactBadges application={application} setupStatus={setupStatus} summary={summary} />
                  <div className="flex flex-wrap items-center justify-end gap-2">
                    {application.installed && !busy ? <ApplicationMoreActions accessProfile={accessProfile} application={application} onRemove={onRemove} /> : null}
                    <OcAnimatedActionSlot>
                      {busy && lifecycle ? <ApplicationLifecycleButton operation={lifecycle} variant="primary" /> : application.installed ? usable ? (
                        <ApplicationConnectionControl
                          connectAvailable={connection.connectAvailable}
                          disconnecting={connection.disconnecting}
                          name={application.name}
                          onCheck={connection.onCheck}
                          onConnect={connection.onConnect}
                          onDisconnect={connection.onDisconnect}
                          placement="detail"
                          summary={summary}
                        />
                      ) : null : (
                        <OcButton
                          disabled={accessProfile !== "live" || installLocked || !repairable && Boolean(application.install_block_reason)}
                          leadingIcon="download"
                          onClick={onInstall}
                          size="sm"
                          title={accessProfile !== "live"
                            ? `${repairable ? "Repair" : "Install"} applications from the Administrator profile.`
                            : installLocked ? INSTALL_LOCKED : repairable ? "Repair application package" : application.install_block_reason || "Install application"}
                          variant="primary"
                        >
                          {lifecycle?.phase === "failed" ? `Retry ${repairable ? "repair" : "install"}` : repairable ? "Repair" : "Install"}
                        </OcButton>
                      )}
                    </OcAnimatedActionSlot>
                  </div>
                </div>
              </div>
            </header>
            {lifecycle?.phase === "failed" ? (
              <div className="mx-[var(--detail-page-inline-inset,0px)] rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
                <div className="font-medium">{lifecycle.operation === "install" ? "Installation failed" : lifecycle.operation === "repair" ? "Repair failed" : "Removal failed"}</div>
                <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{lifecycle.error}</div>
              </div>
            ) : null}
            <AnimatePresence initial={false}>
              {usable ? (
                <motion.div
                  animate={{ height: "auto", opacity: 1 }}
                  className="-mt-[var(--detail-page-section-gap)] overflow-hidden"
                  exit={{ height: 0, opacity: 0 }}
                  initial={{ height: 0, opacity: 0 }}
                  key="installed-sections"
                  transition={reducedMotion ? { duration: 0 } : INSTALLED_SECTIONS_TRANSITION}
                >
                  <div className="flex flex-col gap-[var(--detail-page-section-gap)] pt-[var(--detail-page-section-gap)]">
                    {summary.status !== "none" ? (
                      <DetailSection title="Connection">
                        <DetailRow label="Status" value={<ConnectionStatusValue summary={summary} />} />
                        {summary.account ? (
                          <DetailRow label="Account" value={(
                            <span className="flex min-w-0 flex-col gap-0.5">
                              <span>{summary.account.label}</span>
                              {summary.account.detail ? <span className="text-xs text-secondary">{summary.account.detail}</span> : null}
                            </span>
                          )} />
                        ) : null}
                        {summary.fields.map((field) => (
                          <DetailRow
                            action={field.private_saved && summary.fieldInterfaces.includes(field.interface_id) ? (
                              <OcButton disabled={configurationSaving || connection.disconnecting} onClick={() => onSaveConfiguration(field.interface_id, {}, [field.name])} size="xs" variant="ghost">Clear</OcButton>
                            ) : null}
                            key={configurationFieldKey(field)}
                            label={field.label}
                            value={configurationFieldStatus(field)}
                          />
                        ))}
                        {help ? (
                          <DetailRow label="Help" value={<a className="cursor-interaction text-default underline underline-offset-2" href={help.url} rel="noreferrer" target="_blank">{help.label}</a>} />
                        ) : null}
                      </DetailSection>
                    ) : null}
                    <DetailSection title="Available in">
                      <ApplicationAvailabilityList
                        application={application}
                        onChange={onAvailabilityChange}
                        projects={projects}
                        rootEeId={rootEeId}
                        savingEeId={availabilitySavingEeId}
                        variant="page"
                      />
                      <div className="px-2 pt-1 text-xs text-secondary">Existing tasks don't change. New tasks started where New tasks is on include {application.name}.</div>
                    </DetailSection>
                    {environmentFields.length ? (
                      <DetailSection title="Environment">
                        <div className="px-2 pb-1 text-xs text-secondary">Private values for this application's processes. A value you don't set here comes from the environment Octopus starts with.</div>
                        {environmentFields.map((field) => (
                          <EnvironmentFieldRow
                            field={field}
                            key={configurationFieldKey(field)}
                            onSave={(updates, clears) => onSaveConfiguration(field.interface_id, updates, clears)}
                            saving={configurationSaving}
                          />
                        ))}
                      </DetailSection>
                    ) : null}
                  </div>
                </motion.div>
              ) : null}
            </AnimatePresence>
            <DetailSection title="Included">
              {application.interfaces.map((interfaceId) => <DetailRow key={interfaceId} label="Interface" value={interfaceId} />)}
              {application.artifact_bindings.map((binding) => <DetailRow key={binding.artifact_id} label="Guide for the model" value={binding.label?.trim() || binding.artifact_id} />)}
              <DetailRow label="Task access" value="A task gets it only when you connect it to that task." />
              <DetailRow label="Tasks" value={partiallyConnectedRunCount
                ? `Connection incomplete in ${partiallyConnectedRunCount} task${partiallyConnectedRunCount === 1 ? "" : "s"}.`
                : application.connected_run_ids.length
                  ? `Used in ${application.connected_run_ids.length} task${application.connected_run_ids.length === 1 ? "" : "s"}.`
                  : "Not used in a task yet."} />
            </DetailSection>
            <DetailSection title="Information">
              <DetailRow label="Installed" value={application.installed
                ? "Package installed in Octopus Home."
                : application.materialized || application.partially_materialized
                  ? "Package files were found, but Octopus could not verify one complete installation."
                  : "Available to install. No interfaces or task bindings have been added."} />
              {!application.valid ? <DetailRow label="Installation issue" value={application.missing_interface_ids.length
                ? `Missing interfaces: ${application.missing_interface_ids.join(", ")}`
                : application.receipt_state && application.receipt_state !== "valid"
                  ? "The package files do not have one matching install receipt."
                  : "The package manifest could not be admitted."} /> : null}
              {setupStatus && application.installed ? <DetailRow label="Provider setup" value={setupStatus.message} /> : null}
              {application.installed && application.removal_block_reason ? <DetailRow label="Removal" value={application.removal_block_reason} /> : null}
              {application.source_kind === "local" ? null : <DetailRow label="Package source" value="This build ships no package for it, so it cannot be repaired or updated." />}
              <DetailRow label="Developer" value={application.developer} />
              <DetailRow label="Category" value={CATEGORY_COPY[application.category].title} />
              <DetailRow label="Version" value={application.version} />
              <DetailRow label="Manifest" value={application.manifest_path} />
            </DetailSection>
          </div>
        </div>
      </div>
    </section>
  );
}

function ConnectionStatusValue({ summary }: { summary: ApplicationConnectionSummary }) {
  const label = summary.status === "checking"
    ? "Checking…"
    : summary.status === "connected"
      ? "Connected"
      : summary.status === "expired"
        ? "Reconnect required"
        : summary.status === "error"
          ? "Connection issue"
          : "Not connected";
  return (
    <span className="flex min-w-0 flex-col gap-0.5">
      <span>
        <OcStatus
          label={label}
          tone={summary.status === "connected" ? "success" : summary.status === "expired" ? "warning" : summary.status === "error" ? "danger" : "neutral"}
        />
      </span>
      {summary.message && summary.status !== "connected" ? <span className="text-xs text-secondary whitespace-pre-line">{summary.message}</span> : null}
    </span>
  );
}

function EnvironmentFieldRow({ field, onSave, saving }: {
  field: ApplicationConfigurationField;
  onSave: (updates: Record<string, string>, clears: string[]) => void;
  saving: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  function close() {
    setEditing(false);
    setValue("");
  }
  return (
    <div className="grid min-h-9 grid-cols-[minmax(7rem,0.35fr)_minmax(0,1fr)] items-center gap-4 rounded-lg px-2 py-1.5 hover:bg-token-foreground/3">
      <span className="truncate font-mono text-xs text-token-text-secondary" title={field.label === field.name ? field.name : `${field.label} (${field.name})`}>{field.name}</span>
      {editing ? (
        <form className="flex min-w-0 items-center gap-1" onSubmit={(event) => {
          event.preventDefault();
          if (!value) return;
          onSave({ [field.name]: value }, []);
          close();
        }}>
          <input
            aria-label={field.label}
            autoComplete="off"
            autoFocus
            className="h-7 min-w-0 flex-1 rounded-md border border-primary-outline bg-primary-soft px-2 text-sm text-default outline-none placeholder:text-tertiary focus:border-ring"
            onChange={(event) => setValue(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.stopPropagation();
                close();
              }
            }}
            placeholder={field.private_saved ? "New value" : "Value"}
            spellCheck={false}
            type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
            value={value}
          />
          <OcButton disabled={!value || saving} size="xs" type="submit" variant="secondary">Save</OcButton>
          <OcButton onClick={close} size="xs" variant="ghost">Cancel</OcButton>
        </form>
      ) : (
        <span className="flex min-w-0 items-center justify-between gap-2 text-sm text-default">
          <span className="min-w-0 truncate">{configurationFieldStatus(field)}</span>
          <span className="flex shrink-0 items-center gap-1">
            {field.private_saved ? <OcButton disabled={saving} onClick={() => onSave({}, [field.name])} size="xs" variant="ghost">Clear</OcButton> : null}
            {field.source === "conflict" ? null : <OcButton disabled={saving} onClick={() => setEditing(true)} size="xs" variant="ghost">{field.private_saved ? "Change" : "Set"}</OcButton>}
          </span>
        </span>
      )}
    </div>
  );
}

function ApplicationRemovalDialog({ application, lifecycle, onCancel, onConfirm }: {
  application: ApplicationRecord | null;
  lifecycle?: ApplicationLifecycleOperation;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const busy = lifecycle && lifecycle.phase !== "failed";
  return (
    <SetupDialogShell ariaLabel={application ? `Remove ${application.name}` : "Remove application"} busy={Boolean(busy)} onClose={onCancel} open={Boolean(application)} panelClassName="max-w-md">
      {application ? (
        <>
          <div className="px-6 pt-8 pb-6">
            <h2 className="heading-base text-token-foreground electron:heading-lg">Remove {application.name}?</h2>
            <p className="mt-2 text-sm leading-5 text-token-text-secondary">Its interface folders will leave the active registry. Project policy and task bindings must already be removed; private interface data is not deleted.</p>
            {application.removal_block_reason ? <div className="mt-4 rounded-xl border border-token-charts-orange/25 bg-token-charts-orange/10 px-3 py-2 text-sm text-token-charts-orange">{application.removal_block_reason}</div> : null}
          </div>
          <footer className="flex min-h-[76px] shrink-0 items-center justify-end gap-2 border-t border-token-border/40 px-6 py-4">
            <OcButton disabled={Boolean(busy)} onClick={onCancel} variant="ghost">Cancel</OcButton>
            <OcButton busy={Boolean(busy)} disabled={!application.removal_allowed} onClick={onConfirm} tone="danger" variant="secondary">{busy ? "Removing" : "Remove"}</OcButton>
          </footer>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ApplicationLifecycleButton({ operation, variant }: {
  operation: Pick<ApplicationLifecycleOperation, "operation" | "percent" | "phase">;
  variant: "ghost" | "primary";
}) {
  const label = operation.operation === "remove" ? "Removing" : operation.phase === "complete" ? "Loading" : operation.operation === "install" ? "Installing" : "Repairing";
  const progress = Math.max(0, Math.min(100, operation.percent));
  return (
    <OcButton busy className="shrink-0" size="sm" variant={variant}>
      {operation.operation === "remove" ? label : <OcInlineProgress label={label} progressLabel={`${label} ${Math.round(progress)}%`} value={progress} />}
    </OcButton>
  );
}

function lifecycleBusy(operation: { phase: ApplicationLifecycleOperation["phase"] } | undefined): boolean {
  return Boolean(operation && operation.phase !== "failed");
}

function presentApplications(
  applications: ApplicationRecord[],
  operations: Record<string, ApplicationLifecycleOperation>,
  packOperations: Record<string, ApplicationPackLifecycleOperation>,
): ApplicationRecord[] {
  const presented = new Map<string, ApplicationRecord>();
  for (const operation of Object.values(packOperations)) {
    if (!lifecycleBusy(operation)) continue;
    for (const application of operation.presentation.applications) presented.set(application.id, application);
  }
  for (const [applicationId, operation] of Object.entries(operations)) {
    if (lifecycleBusy(operation)) presented.set(applicationId, operation.presentation);
  }
  return presented.size ? applications.map((application) => presented.get(application.id) ?? application) : applications;
}

function withoutSettledOperations<T extends { phase: ApplicationLifecycleOperation["phase"] }>(
  operations: Record<string, T>,
  settled: (operation: T) => boolean,
): Record<string, T> {
  const remaining = Object.entries(operations).filter(([, operation]) => operation.phase !== "complete" || !settled(operation));
  return remaining.length === Object.keys(operations).length ? operations : Object.fromEntries(remaining);
}

function announceDisconnectResult(name: string, state: OctopusApplicationConnectionState) {
  const manage = state.manage;
  const options = manage ? { action: { label: manage.label, onSelect: () => { window.open(manage.url, "_blank", "noopener,noreferrer"); } } } : undefined;
  const duration = manage ? 10 : undefined;
  if (state.state === "error") appToast.danger(state.message || `${name} could not be disconnected.`, duration, options);
  else if (state.state === "expired") appToast.info(state.message || `${name} needs to be reconnected.`, duration, options);
  else if (state.state === "connected") appToast.info(state.message || `${name} is still connected.`, duration, options);
  else if (manage) appToast.info(state.message || `${name} is disconnected.`, duration, options);
  else appToast.success(state.message || `${name} is disconnected.`);
}

function applicationRepairExpectation(application: ApplicationRecord): {
  sourceDigest: string;
  installedDigest: string;
  receiptState: "missing" | "stale";
  receiptDigest: string | null;
} | null {
  if (
    application.installed
    || !application.source_available
    || !application.materialized
    || application.partially_materialized
    || !application.package_id
    || !SHA256_DIGEST.test(application.source_digest)
    || !SHA256_DIGEST.test(application.installed_digest)
    || !["missing", "stale"].includes(application.receipt_state)
  ) return null;
  if (application.receipt_state === "missing") {
    if (application.receipt_digest) return null;
    return {
      sourceDigest: application.source_digest,
      installedDigest: application.installed_digest,
      receiptState: "missing",
      receiptDigest: null,
    };
  }
  if (!SHA256_DIGEST.test(application.receipt_digest)) return null;
  return {
    sourceDigest: application.source_digest,
    installedDigest: application.installed_digest,
    receiptState: "stale",
    receiptDigest: application.receipt_digest,
  };
}

function DetailSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]">
      <div className="flex items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)]">
        <h2 className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">{title}</h2>
      </div>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">{children}</div>
    </section>
  );
}

function DetailRow({ action, label, value }: { action?: ReactNode; label: string; value: ReactNode }) {
  return (
    <div className="grid min-h-9 grid-cols-[minmax(7rem,0.35fr)_minmax(0,1fr)] items-start gap-4 rounded-lg px-2 py-1.5 hover:bg-token-foreground/3">
      <span className="text-sm text-token-text-secondary">{label}</span>
      <span className="flex min-w-0 items-center justify-between gap-3">
        <span className="min-w-0 text-sm text-default select-text [overflow-wrap:anywhere]">{value}</span>
        {action ? <span className="-my-1 shrink-0">{action}</span> : null}
      </span>
    </div>
  );
}

function ApplicationFactBadges({ application, setupStatus, summary }: { application: ApplicationRecord; setupStatus?: ClientExtensionActionStatus; summary: ApplicationConnectionSummary }) {
  const attention = applicationAttention(application, summary, setupStatus);
  return attention ? <OcStatus label={attention.label} tone={attention.tone} /> : null;
}

function ApplicationCardMeta({ application, setupStatus, summary }: { application: ApplicationRecord; setupStatus?: ClientExtensionActionStatus; summary: ApplicationConnectionSummary }) {
  const attention = applicationAttention(application, summary, setupStatus);
  if (!attention) return null;
  return (
    <span className="mt-1 flex min-w-0">
      <OcStatus label={attention.label} tone={attention.tone} />
    </span>
  );
}

function applicationAttention(
  application: ApplicationRecord,
  summary: ApplicationConnectionSummary,
  setupStatus?: ClientExtensionActionStatus,
): { label: string; tone: "neutral" | "warning" | "danger" } | null {
  const materializationIncomplete = !application.installed && (
    application.materialized
    || application.partially_materialized
    || application.receipt_state === "stale"
  );
  if (materializationIncomplete || (application.installed && !application.valid)) return { label: "Needs repair", tone: "danger" };
  if (!application.installed) return application.source_available ? null : { label: "Unavailable", tone: "danger" };
  if (application.partially_connected_run_ids?.length) return { label: "Connection incomplete", tone: "danger" };
  if (setupStatus?.state === "unavailable") return { label: "Setup unavailable", tone: "danger" };
  if (setupStatus?.state === "repair_available" || setupStatus?.state === "sign_in_required") return { label: "Sign in required", tone: "warning" };
  if (setupStatus?.state === "waiting_for_interface") return { label: "Finishing setup", tone: "neutral" };
  if (summary.status === "none" && application.configuration?.status === "issue") return { label: "Check settings", tone: "danger" };
  if (summary.status === "none" && application.configuration?.status === "required") return { label: "Needs setup", tone: "warning" };
  if (summary.status !== "not_connected" && !application.project_states.some((state) => state.policy_allowed)) return { label: "Not enabled", tone: "neutral" };
  return null;
}

function configurationFieldStatus(field: ApplicationConfigurationField): string {
  if (field.source === "conflict") return "Saved value conflicts with the environment Octopus starts with";
  if (field.private_saved) return "Saved";
  if (field.source === "environment") return "Set in environment";
  if (field.source === "unknown") return "Not checked";
  return field.required ? "Not set" : "Optional";
}

function applicationSetupStatus(
  application: ApplicationRecord,
  byInterface: Map<string, ClientExtensionActionStatus>,
): ClientExtensionActionStatus | undefined {
  for (const interfaceId of application.interfaces) {
    const status = byInterface.get(interfaceId);
    if (status) return status;
  }
  return byInterface.get(application.id);
}

function SurfaceState({ title, detail = "", compact = false, actionLabel = "", onAction }: { title: string; detail?: string; compact?: boolean; actionLabel?: string; onAction?: () => void }) {
  return (
    <OcEmptyState
      action={actionLabel && onAction ? <OcButton onClick={onAction} size="xs" variant="secondary">{actionLabel}</OcButton> : undefined}
      className={compact ? "flex-1" : "h-full min-h-0"}
      detail={detail || undefined}
      size={compact ? "compact" : "default"}
      title={title}
    />
  );
}

function ApplicationProjectionIssue({ detail }: { detail: string }) {
  return (
    <div className="rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
      <div className="font-medium">Some application packages need attention</div>
      <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{detail}</div>
    </div>
  );
}

function applicationSearchText(application: ApplicationRecord): string {
  return [
    application.id,
    application.name,
    application.description,
    application.category,
    application.developer,
    ...application.interfaces,
  ].join(" ").toLowerCase();
}

function applicationPackKey(packId: string): string {
  return `pack:${packId}`;
}

function applicationPackSearchText(pack: ApplicationPackRecord): string {
  return [
    pack.id,
    pack.name,
    pack.description,
    ...pack.components.flatMap((component) => [component.name, component.reason]),
  ].join(" ").toLowerCase();
}

function applicationPackInventory(pack: ApplicationPackRecord): {
  ready: boolean;
  state: "available" | "partial" | "installed";
  includedCount: number;
  installedCount: number;
} {
  const included = pack.components.filter((component) => component.selection === "included");
  const installedCount = included.filter((component) => component.installed && component.receipt_state === "valid").length;
  const ready = included.length > 0
    && pack.required_components_materialized
    && installedCount === included.length;
  const partial = !ready && (
    installedCount > 0
    || included.some((component) => (
      component.materialized
      || component.partially_materialized
      || component.receipt_state === "stale"
      || Boolean(component.error)
    ))
  );
  return {
    ready,
    state: ready ? "installed" : partial ? "partial" : "available",
    includedCount: included.length,
    installedCount,
  };
}

function selectedPackRecommendations(pack: ApplicationPackRecord, selected: string[] | undefined): string[] {
  const recommended = pack.components.filter((component) => component.selection === "recommended");
  const allowed = new Set(recommended.map((component) => component.package_id));
  const source = selected ?? recommended.filter((component) => component.available).map((component) => component.package_id);
  return [...new Set(source.filter((packageId) => allowed.has(packageId)))].sort();
}

function togglePackRecommendation(
  pack: ApplicationPackRecord,
  current: string[] | undefined,
  packageId: string,
  checked: boolean,
): string[] {
  const selected = new Set(selectedPackRecommendations(pack, current));
  if (checked) selected.add(packageId);
  else selected.delete(packageId);
  return selectedPackRecommendations(pack, [...selected]);
}

function applicationPackInstallBlockReason(pack: ApplicationPackRecord, selectedRecommended?: string[]): string {
  const selected = new Set(selectedRecommended ?? selectedPackRecommendations(pack, undefined));
  const components = pack.components.filter((component) => (
    component.selection === "included" || selected.has(component.package_id)
  ));
  if (!components.length) return "This pack contains no installable components.";
  if (components.some((component) => !component.available || !component.source_digest)) {
    return "One or more selected components are unavailable in the local library.";
  }
  if (components.some((component) => (
    !component.installed
    && (
      component.materialized
      || component.partially_materialized
      || component.receipt_state === "stale"
      || Boolean(component.error)
    )
  ))) {
    return "An installed component needs attention before this pack can be applied.";
  }
  return "";
}

function applicationPackNeedsApply(pack: ApplicationPackRecord, selectedRecommended?: string[]): boolean {
  const selected = new Set(selectedRecommended ?? selectedPackRecommendations(pack, undefined));
  return pack.components.some((component) => (
    (component.selection === "included" || selected.has(component.package_id))
    && (!component.installed || component.receipt_state !== "valid")
  ));
}

function applicationPackSetupStatus(
  pack: ApplicationPackRecord,
  byInterface: Map<string, ClientExtensionActionStatus>,
): ClientExtensionActionStatus | undefined {
  const providerInterfaceId = pack.workspace_setup?.home.provider_interface_id;
  return providerInterfaceId ? byInterface.get(providerInterfaceId) : undefined;
}

function applicationPackFacts(
  pack: ApplicationPackRecord,
  setupStatus?: ClientExtensionActionStatus,
  clientSetup?: OctopusClientSetupState | null,
  clientSetupError = "",
): Array<{ label: string; tone: "neutral" | "success" | "warning" | "danger" }> {
  const inventory = applicationPackInventory(pack);
  const setupVerified = applicationPackSetupVerified(pack, clientSetup);
  const currentSetupFailure = setupStatus && setupStatus.state !== "configured";
  const facts: Array<{ label: string; tone: "neutral" | "success" | "warning" | "danger" }> = [
    inventory.state === "installed"
      ? { label: "Installed", tone: "success" }
      : inventory.state === "partial"
        ? { label: "Partially installed", tone: "warning" }
        : { label: "Not installed", tone: "neutral" },
  ];
  if (inventory.ready && clientSetupError) facts.push({ label: "Setup unknown", tone: "danger" });
  else if (inventory.ready && setupStatus?.state === "unavailable") facts.push({ label: "Setup unavailable", tone: "danger" });
  else if (inventory.ready && (currentSetupFailure || !setupVerified)) facts.push({ label: "Needs setup", tone: "warning" });
  else if (inventory.ready) facts.push({ label: "Ready", tone: "success" });
  return facts;
}

function applicationPackSetupVerified(
  pack: ApplicationPackRecord,
  setup: OctopusClientSetupState | null | undefined,
): boolean {
  return setup?.status === "complete"
    && Boolean(setup.receiptDigest)
    && setup.plan.pack.id === pack.id
    && setup.plan.pack.manifest_digest === pack.manifest_digest
    && setup.plan.provider.required_state === "account_readable";
}

function applicationPackComponentStatus(component: ApplicationPackComponent): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  if (component.installed && component.receipt_state === "valid") return { label: "Installed", tone: "success" };
  if (
    component.installed
    || component.materialized
    || component.partially_materialized
    || component.receipt_state === "stale"
    || component.error
  ) return { label: "Needs repair", tone: "danger" };
  if (component.available) return { label: component.selection === "included" ? "Included" : "Optional", tone: "neutral" };
  return { label: "Unavailable", tone: "danger" };
}

function applicationPackComponentGlyph(component: ApplicationPackComponent): GlyphName {
  if (component.product_kind === "driver") return "environment";
  if (component.product_kind === "artifact") return "book";
  return "interfaces";
}

function applicationPackComponentDescription(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return "Local capability available only where you allow it.";
  if (component.product_kind === "artifact") return "Reference context for tasks that include it.";
  return "Communication capability for Home and selected projects.";
}

function applicationPackPermissionLabel(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return `Use ${component.name}`;
  if (component.product_kind === "artifact") return `Read ${component.name}`;
  return `Connect through ${component.name}`;
}

function applicationPackPermissionDetail(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return "Allows local operations only inside projects where this capability is enabled.";
  if (component.product_kind === "artifact") return "Shares this reference context only with tasks that include it.";
  return "Allows task messages and results to use this connection only in projects where you turn it on.";
}

function arrayRecords<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

function arrayStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.map((item) => typeof item === "string" ? item : "").filter(Boolean) : [];
}
