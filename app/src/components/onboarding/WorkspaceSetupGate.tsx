import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { providerServiceTierAvailable } from "../chat/chatWorkspaceCapabilities";
import type {
  OctopusProviderInterface,
  OctopusClientAccessProfile,
  OctopusClientSetupPlan,
  OctopusClientSetupProgress,
  OctopusClientSetupState,
} from "../../electron";
import type {
  ClientExtensionActionStatus,
  ClientExtensionRecord,
  ClientExtensionRegistryState,
} from "../../extensions/types";
import { useClientExtensions } from "../../extensions/useClientExtensions";
import {
  clearWorkspaceSetupDraft,
  compileWorkspaceInitializeRequest,
  readWorkspaceSetupDraft,
  saveWorkspaceSetupDraft,
  workspaceInitializeRequestDigest,
  type WorkspaceSetupCapabilityKind,
  type WorkspaceSetupDraft,
  type WorkspaceSetupHomeRecipe,
  type WorkspaceSetupAgentPreset,
} from "../../onboarding/workspaceSetup";
import {
  beginClientSetup,
  onClientSetupProgress,
  openSetupFile,
  readProjection,
  readClientSetup,
  readCurrentClientSetup,
  readWorkspaceSetupState,
  resumeClientSetup,
  saveSetupFile,
} from "../../projectionClient";
import { useProjectionStructureGeneration } from "../../projectionLive";
import { useChatgptAccount } from "../../account/useChatgptAccount";
import { ChatGptSignInGate } from "./ChatGptSignInGate";
import { chatgptSignInCompleted } from "../../onboarding/chatgptSignIn";
import { OcButton } from "../../design-system/components";
import { appToast } from "../../toasts";
import { OctopusLoadingLogo, OctopusLoadingState } from "../OctopusLoadingLogo";
import {
  WorkspaceSetupOverlay,
  WorkspaceSetupProgress,
  createDefaultWorkspaceSetupDraft,
  type WorkspaceSetupApplyState,
  type WorkspaceSetupCatalogCapability,
} from "./WorkspaceSetupOverlay";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  children: ReactNode;
  runtimeHome: string;
};

type WorkspaceReceipt = {
  operationId: string;
  requestDigest: string;
  rootEeId: string;
  homeRunId: string;
  homeInterfaceId: string;
  projects: Array<{ key: string; eeId: string }>;
};

type WorkspaceProjection = {
  status: "not_configured" | "configured" | "degraded";
  receipt: WorkspaceReceipt | null;
  errors: string[];
};

type ScopeProjection = {
  status: "unbound" | "awaiting_home" | "ready" | "invalid";
  rootEeId: string;
  homeRunId: string;
  homeInterfaceId: string;
  rootEnvironment: ScopeProject | null;
  projects: ScopeProject[];
  errors: string[];
};

type ScopeProject = {
  eeId: string;
  recordDigest: string;
  applicationDefaults: string[];
  driverDefaults: string[];
};

type PackComponent = {
  packageId: string;
  selection: "included" | "recommended";
  reason: string;
  productKind: "application" | "driver" | "artifact";
  productId: string;
  name: string;
  available: boolean;
  installed: boolean;
  receiptState: string;
  sourceDigest: string;
};

type PackExtension = {
  extensionId: string;
  selection: "recommended";
  reason: string;
};

type WorkspacePack = {
  id: string;
  manifestDigest: string;
  name: string;
  description: string;
  components: PackComponent[];
  extensions: PackExtension[];
  recipe: WorkspaceSetupHomeRecipe;
  setupProvider: OctopusProviderInterface | null;
};

type LocalPackagesProjection = {
  packs: WorkspacePack[];
  errors: string[];
};

type GateSnapshot = {
  workspace: WorkspaceProjection;
  scope: ScopeProjection;
  localPackages: LocalPackagesProjection | null;
  clientSetup: OctopusClientSetupState | null;
};

type DraftState = {
  draft: WorkspaceSetupDraft | null;
  error: string;
};

type CatalogResult = {
  catalog: WorkspaceSetupCatalogCapability[];
  pack: WorkspacePack | null;
  error: string;
  warning: string;
};

type ApplySelection = {
  packageIds: string[];
  selectedRecommended: string[];
  extensions: Array<{
    extensionId: string;
    version: string;
    sourceDigest: string;
    desiredState: "installed" | "available";
  }>;
  projectKeysByPackage: Record<string, string[]>;
};

type GateMatrix = {
  mode: "shell" | "setup" | "resume" | "blocked";
  title: string;
  detail: string;
  recovery?: "discard_draft";
};

const ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/;
const DIGEST = /^[a-f0-9]{64}$/;
const APPLY_IDLE: WorkspaceSetupApplyState = {
  state: "idle",
  label: "",
  detail: "",
  percent: null,
  error: "",
};

const AWAITING_SIGN_IN: WorkspaceSetupApplyState = {
  state: "awaiting_sign_in",
  label: "Your space is ready. Connect ChatGPT.",
  detail: "Continue with your ChatGPT account to start working with Home. No API key is needed.",
  percent: null,
  error: "",
};

const SETUP_FILES = { open: openSetupFile, save: saveSetupFile };

export function WorkspaceSetupGate({ accessProfile, runtimeHome, children }: Props) {
  if (accessProfile !== "live") return <>{children}</>;
  if (!runtimeHome) return <OpeningState />;
  return <WorkspaceHomeGate key={runtimeHome} runtimeHome={runtimeHome}>{children}</WorkspaceHomeGate>;
}

function WorkspaceHomeGate({ runtimeHome, children }: Pick<Props, "runtimeHome" | "children">) {
  useChatgptAccount(false, runtimeHome);
  const structureGeneration = useProjectionStructureGeneration();
  const [snapshot, setSnapshot] = useState<GateSnapshot | null>(null);
  const [setup, setSetup] = useState<OctopusClientSetupState | null>(null);
  const [error, setError] = useState("");
  const [failedReads, setFailedReads] = useState(0);
  const [retry, setRetry] = useState(0);
  const issue = useRef<{ close: () => void } | null>(null);
  useEffect(() => {
    let active = true;
    void readGateSnapshot(false, (current) => { if (active) setSetup(current); }).then((next) => {
      if (!active) return;
      setSnapshot(next);
      setError("");
    }, (failure) => {
      if (!active) return;
      setSnapshot((current) => current && shellGateSnapshot(current) ? current : null);
      setError(failure instanceof Error ? failure.message : String(failure));
      setFailedReads((value) => value + 1);
    });
    return () => { active = false; };
  }, [structureGeneration, retry]);
  const shellMounted = snapshot !== null && shellGateSnapshot(snapshot);
  useEffect(() => {
    if (!error || !shellMounted) return;
    issue.current = appToast.danger(`Workspace could not be checked: ${error}`, 0, {
      action: { label: "Try again", onSelect: () => setRetry((value) => value + 1) },
      dedupeKey: `workspace-gate:${runtimeHome}`,
    });
  }, [error, failedReads, runtimeHome, shellMounted]);
  useEffect(() => {
    if (!error || !shellMounted) return;
    return () => {
      issue.current?.close();
      issue.current = null;
    };
  }, [error, runtimeHome, shellMounted]);
  if (error && !shellMounted) return <GateState title="Workspace could not be checked" detail={error} tone="danger"
    action={<OcButton onClick={() => setRetry((value) => value + 1)}>Try again</OcButton>} />;
  if (!snapshot) return setup?.status === "in_progress"
    ? <WorkspaceSetupProgress applyState={savedSetupProgress(setup)} />
    : <OpeningState />;
  if (snapshot.clientSetup?.status === "complete") {
    const matrix = completedSetupMatrix(snapshot, null);
    if (matrix.mode === "shell") return <>{children}</>;
    return <GateState title={matrix.title} detail={matrix.detail} tone="danger"
      action={<OcButton onClick={() => setRetry((value) => value + 1)}>Check again</OcButton>} />;
  }
  return <WorkspaceAccountSetupGate runtimeHome={runtimeHome} setup={snapshot.clientSetup}>{children}</WorkspaceAccountSetupGate>;
}

function WorkspaceAccountSetupGate({ runtimeHome, children, setup }: Pick<Props, "runtimeHome" | "children"> & { setup: OctopusClientSetupState | null }) {
  const account = useChatgptAccount();
  if (setup?.status !== "in_progress" && (!account.account?.signedIn || account.account.octopusHome !== runtimeHome)) return <ChatGptSignInGate />;
  return <WorkspaceSetupFlow accessProfile="live" initialSetup={setup}>{children}</WorkspaceSetupFlow>;
}

function WorkspaceSetupFlow({ accessProfile, children, initialSetup }: Omit<Props, "runtimeHome"> & { initialSetup: OctopusClientSetupState | null }) {
  const extensions = useClientExtensions(accessProfile);
  const structureGeneration = useProjectionStructureGeneration();
  const [snapshot, setSnapshot] = useState<GateSnapshot | null>(null);
  const [snapshotError, setSnapshotError] = useState("");
  const [loading, setLoading] = useState(accessProfile === "live");
  const [draftState, setDraftState] = useState<DraftState>(loadDraft);
  const [applyState, setApplyState] = useState<WorkspaceSetupApplyState>(APPLY_IDLE);
  const refreshSequence = useRef(0);
  const inFlight = useRef<Promise<void> | null>(null);
  const activeOperationId = useRef(initialSetup?.operationId ?? "");
  const extensionsRef = useRef(extensions);
  extensionsRef.current = extensions;

  const refresh = useCallback(async (
    showLoading = false,
    beforePublish?: (next: GateSnapshot) => void,
  ): Promise<GateSnapshot> => {
    const sequence = ++refreshSequence.current;
    if (showLoading) setLoading(true);
    let next: GateSnapshot;
    try {
      next = await readGateSnapshot();
    } catch (error) {
      if (sequence === refreshSequence.current) {
        console.error("Workspace setup projection could not be read", error);
        const message = error instanceof Error ? error.message : String(error);
        setSnapshotError(message.replace(/^Error invoking remote method '[^']+': (?:Error: )?/, "") || "Octopus could not verify the current workspace state.");
        setLoading(false);
      }
      throw error;
    }
    beforePublish?.(next);
    if (sequence === refreshSequence.current) {
      setSnapshot(next);
      setSnapshotError("");
      setLoading(false);
    }
    return next;
  }, []);

  useEffect(() => {
    if (accessProfile !== "live") return;
    void refresh(snapshot === null).catch(() => {});
  }, [accessProfile, refresh, structureGeneration]);

  useEffect(() => onClientSetupProgress((progress) => {
    if (!activeOperationId.current || progress.operationId !== activeOperationId.current) return;
    setApplyState({
      state: "running",
      label: progress.phase === "sealed" ? "Opening Home" : clientSetupProgressLabel(progress.step, progress.phase),
      detail: progress.phase === "sealed" ? "Checking that the prepared workspace is ready to open." : clientSetupProgressDetail(progress.step),
      percent: Math.min(progress.percent, 99),
      error: "",
    });
  }), []);

  const catalogResult = useMemo<CatalogResult>(() => {
    if (!snapshot?.localPackages || extensions.loading) return { catalog: [], pack: null, error: "", warning: "" };
    try {
      return buildCatalog(snapshot.localPackages, extensions, snapshot.clientSetup);
    } catch (error) {
      console.error("Workspace setup catalog was refused", error);
      return {
        catalog: [],
        pack: null,
        error: "The local setup catalog could not be verified.",
        warning: "",
      };
    }
  }, [extensions, snapshot]);

  const matrix = useMemo(() => (
    snapshot ? resolveGateMatrix(snapshot, draftState.draft, catalogResult, extensions) : null
  ), [catalogResult, draftState.draft, extensions, snapshot]);

  const changeDraft = useCallback((draft: WorkspaceSetupDraft) => {
    if (inFlight.current) return;
    try {
      const saved = saveWorkspaceSetupDraft(draft);
      setDraftState({ draft: saved, error: "" });
      if (saved.step !== "applying") setApplyState(APPLY_IDLE);
    } catch (error) {
      console.error("Workspace setup choices could not be saved", error);
      setDraftState({ draft: null, error: "Saved setup choices could not be verified." });
    }
  }, []);

  const apply = useCallback((draft: WorkspaceSetupDraft): Promise<void> => {
    if (inFlight.current) return inFlight.current;
    let persisted: WorkspaceSetupDraft;
    try {
      if (draft.step !== "applying") throw new Error("workspace setup Apply requires an applying draft");
      persisted = saveWorkspaceSetupDraft(draft);
      setDraftState({ draft: persisted, error: "" });
    } catch (error) {
      console.error("Workspace setup could not persist its resume point", error);
      setDraftState({ draft: null, error: "Setup could not save a safe resume point." });
      return Promise.resolve();
    }
    setApplyState({
      state: "running",
      label: "Resuming setup",
      detail: "Your saved choices are being verified before Octopus continues.",
      percent: 0,
      error: "",
    });
    const operation = performApply({
      draft: persisted,
      extensions: extensionsRef,
      refresh,
      setApplyState,
    }).then((complete) => {
      if (!complete) return;
      const currentDraft = readWorkspaceSetupDraft();
      if (!currentDraft || JSON.stringify(currentDraft) !== JSON.stringify(persisted)) {
        throw new ProductSetupError("Setup choices changed while Octopus was working. The configured workspace was not opened automatically.");
      }
      clearWorkspaceSetupDraft();
      setDraftState({ draft: null, error: "" });
    }).catch((error) => {
      console.error("Workspace setup apply failed", error);
      void window.octopusTransport?.logClientEvent?.({
        level: "error", event: "workspace.setup.failed",
        message: error instanceof Error ? error.message : String(error),
      }).catch(() => {});
      setApplyState({
        state: "failed",
        label: "Setup needs attention",
        detail: "",
        percent: null,
        error: error instanceof ProductSetupError
          ? error.productMessage
          : "Octopus could not finish setup. Your choices are saved, so it is safe to try again.",
      });
    }).finally(() => {
      activeOperationId.current = "";
      inFlight.current = null;
    });
    activeOperationId.current = persisted.operationId;
    inFlight.current = operation;
    return operation;
  }, [refresh]);

  const resumeCurrent = useCallback((
    setup: OctopusClientSetupState,
    trustedDraft: WorkspaceSetupDraft | null = draftState.draft,
    allowSignIn = false,
  ): Promise<void> => {
    if (inFlight.current) return inFlight.current;
    setApplyState(allowSignIn ? {
      ...savedSetupProgress(setup),
      label: "Connecting ChatGPT",
      detail: "Finish signing in in your browser. Octopus will continue once your account is verified.",
    } : savedSetupProgress(setup));
    activeOperationId.current = setup.operationId;
    const operation = Promise.resolve().then(async () => {
      const currentDraft = readDraftForHostResume(trustedDraft);
      if (currentDraft) {
        assertSavedDraftMatchesSetup(
          setup,
          currentDraft,
          await readGateSnapshot(),
          extensionsRef.current,
        );
      }
      const resumed = await resumeClientSetup(setup.operationId, { allowSignIn });
      return resumed.status === "in_progress" && chatgptSignInCompleted() ? verifyExistingSignIn(setup.operationId) : resumed;
    }).then(async (completed) => {
      if (completed.status === "in_progress") {
        await refresh(false);
        setApplyState(AWAITING_SIGN_IN);
        return;
      }
      await refresh(false, (proven) => proveCompletedClientSetup(completed, proven));
      clearWorkspaceSetupDraft();
      setDraftState({ draft: null, error: "" });
      setApplyState({
        state: "running",
        label: "Workspace verified",
        detail: "Home, projects, local capabilities, and sign-in are ready.",
        percent: 100,
        error: "",
      });
    }).catch((error) => {
      console.error("Saved client setup could not resume", error);
      void window.octopusTransport?.logClientEvent?.({
        level: "error", event: "workspace.setup.resume_failed",
        message: error instanceof Error ? error.message : String(error),
      }).catch(() => {});
      setApplyState({
        state: "failed",
        label: "Setup needs attention",
        detail: "",
        percent: null,
        error: error instanceof ProductSetupError
          ? error.productMessage
          : "Octopus could not finish the saved setup. Nothing was marked ready without proof.",
      });
    }).finally(() => {
      activeOperationId.current = "";
      inFlight.current = null;
    });
    inFlight.current = operation;
    return operation;
  }, [draftState.draft, refresh]);

  useEffect(() => {
    const draft = draftState.draft;
    if (
      accessProfile === "live"
      && draft?.step === "applying"
      && applyState.state === "idle"
      && snapshot
      && !snapshotError
      && catalogResult.pack
      && !catalogResult.error
      && snapshot.clientSetup === null
    ) {
      void apply(draft);
    }
  }, [accessProfile, apply, applyState.state, catalogResult.error, catalogResult.pack, draftState.draft, snapshot, snapshotError]);

  useEffect(() => {
    const setup = snapshot?.clientSetup;
    const draft = draftState.draft;
    if (
      setup?.status === "complete"
      && draft?.operationId === setup.operationId
      && matrix?.mode === "shell"
      && !inFlight.current
    ) {
      clearWorkspaceSetupDraft();
      setDraftState({ draft: null, error: "" });
    }
  }, [draftState.draft, matrix?.mode, snapshot]);

  useEffect(() => {
    if (
      matrix?.mode === "shell"
      && snapshot?.clientSetup?.status === "complete"
      && draftState.error
      && !inFlight.current
    ) {
      clearWorkspaceSetupDraft();
      setDraftState({ draft: null, error: "" });
    }
  }, [draftState.error, matrix?.mode, snapshot]);

  useEffect(() => {
    if (
      accessProfile === "live"
      && snapshot?.clientSetup?.status === "in_progress"
      && applyState.state === "idle"
      && !snapshotError
      && matrix?.mode === "resume"
      && (!draftState.draft || !extensions.loading && Boolean(catalogResult.pack))
    ) {
      void resumeCurrent(snapshot.clientSetup);
    }
  }, [accessProfile, applyState.state, matrix?.mode, resumeCurrent, snapshot, snapshotError, draftState.draft, extensions.loading, catalogResult.pack]);

  useEffect(() => {
    if (
      draftState.draft?.step === "applying"
      && !extensions.loading
      && catalogResult.error
      && applyState.state === "idle"
    ) {
      setApplyState({
        state: "failed",
        label: "Setup needs attention",
        detail: "",
        percent: null,
        error: catalogResult.error,
      });
    }
  }, [applyState.state, catalogResult.error, draftState.draft?.step, extensions.loading]);

  if (accessProfile !== "live") return <>{children}</>;

  if (draftState.error && snapshot !== null && !snapshot.clientSetup) {
    return (
      <GateState
        action={<OcButton onClick={() => resetDraft(setDraftState, setApplyState)}>Start setup again</OcButton>}
        detail="The saved setup draft is malformed or incomplete. Reset only these unfinished setup choices to continue."
        title="Setup choices could not be opened"
        tone="danger"
      />
    );
  }

  if (snapshotError) {
    return (
      <GateState
        action={<OcButton onClick={() => void refresh(true).catch(() => {})}>Try again</OcButton>}
        detail={snapshotError}
        title="Workspace could not be checked"
        tone="danger"
      />
    );
  }

  if (loading || !snapshot) {
    return initialSetup?.status === "in_progress"
      ? <WorkspaceSetupProgress applyState={applyState.state === "idle" ? savedSetupProgress(initialSetup) : applyState} />
      : <OpeningState />;
  }

  if (!matrix) return <OpeningState />;
  if (matrix.mode === "shell") return <>{children}</>;
  if (matrix.mode === "resume" && draftState.draft?.step !== "applying") {
    return (
      <GateState
        action={applyState.state === "awaiting_sign_in" && snapshot.clientSetup
          ? <OcButton variant="primary" onClick={() => void resumeCurrent(snapshot.clientSetup!, draftState.draft, true)}>Sign in with ChatGPT</OcButton>
          : applyState.state === "failed" && snapshot.clientSetup
          ? <OcButton onClick={() => void resumeCurrent(snapshot.clientSetup!)}>Try again</OcButton>
          : undefined}
        detail={applyState.error || applyState.detail || matrix.detail}
        title={applyState.label || matrix.title}
        tone={applyState.state === "failed" ? "danger" : "neutral"}
      />
    );
  }
  if (matrix.mode === "blocked") {
    return (
      <GateState
        action={matrix.recovery === "discard_draft"
          ? <OcButton onClick={() => resetApplyingDraft(setDraftState, setApplyState)}>Continue with existing setup</OcButton>
          : <OcButton onClick={() => void refresh(true).catch(() => {})}>Check again</OcButton>}
        detail={matrix.detail}
        title={matrix.title}
        tone="danger"
      />
    );
  }

  const draft = draftState.draft ?? createDefaultWorkspaceSetupDraft();
  return (
    <WorkspaceSetupOverlay
      applyState={applyState.state === "idle" && snapshot.clientSetup?.status === "in_progress" ? savedSetupProgress(snapshot.clientSetup) : applyState}
      catalog={catalogResult.catalog}
      catalogError={catalogResult.error || catalogResult.warning}
      catalogLoading={extensions.loading}
      draft={draft}
      provider={catalogResult.pack?.setupProvider ?? null}
      agentPresets={catalogResult.pack?.recipe.agent_presets ?? []}
      onApply={apply}
      onSignIn={async () => {
        try {
          const setup = await readCurrentClientSetup();
          if (!setup) throw new Error("The prepared setup is no longer available.");
          await resumeCurrent(setup, draftState.draft, true);
        } catch {
          setApplyState({ state: "failed", label: "Sign-in could not start", detail: "", percent: null, error: "Octopus could not reopen the prepared setup. Your choices are saved; try again." });
        }
      }}
      onChange={changeDraft}
      onRetryCatalog={() => {
        void refresh(true).catch(() => {});
      }}
      setupFiles={SETUP_FILES}
    />
  );
}

async function performApply({
  draft,
  extensions,
  refresh,
  setApplyState,
}: {
  draft: WorkspaceSetupDraft;
  extensions: { current: ClientExtensionRegistryState };
  refresh: (
    showLoading?: boolean,
    beforePublish?: (next: GateSnapshot) => void,
  ) => Promise<GateSnapshot>;
  setApplyState: (state: WorkspaceSetupApplyState) => void;
}): Promise<boolean> {
  const current = await refresh();
  assertApplyBoundary(current, draft.operationId);
  const pack = requireWorkspacePack(current.localPackages);
  const selection = compileApplySelection(draft, pack, extensions.current);
  const plan = await compileClientSetupPlan(draft, pack, selection);
  assertConfiguredRequestBoundary(current, draft.operationId, plan.workspace.request_digest);
  let setup = await readClientSetup(draft.operationId);
  if (!setup) {
    setup = await beginClientSetup({ operationId: draft.operationId, plan });
  } else if (stableSetupJson(setup.plan) !== stableSetupJson(plan)) {
    throw new ProductSetupError("Saved setup choices differ from the immutable setup already in progress. Octopus kept the earlier plan instead of replacing it.");
  }
  const completed = setup.status === "complete"
    ? setup
    : await resumeClientSetup(draft.operationId);
  let finished = completed;
  if (finished.status === "in_progress" && chatgptSignInCompleted()) {
    await refresh(false);
    finished = await verifyExistingSignIn(draft.operationId);
  }
  if (finished.status === "in_progress") {
    await refresh(false);
    setApplyState(AWAITING_SIGN_IN);
    return false;
  }
  await refresh(false, (proven) => proveCompletedClientSetup(finished, proven, draft));
  setApplyState({
    state: "running",
    label: "Workspace verified",
    detail: "Home, projects, local capabilities, and sign-in are ready.",
    percent: 100,
    error: "",
  });
  return true;
}

async function verifyExistingSignIn(operationId: string): Promise<OctopusClientSetupState> {
  return resumeClientSetup(operationId, { allowSignIn: true });
}

async function compileClientSetupPlan(
  draft: WorkspaceSetupDraft,
  pack: WorkspacePack,
  selection: ApplySelection,
): Promise<OctopusClientSetupPlan> {
  if (!pack.setupProvider?.models.some((model) => model.model === draft.providerModel && model.showInPicker)) {
    throw new ProductSetupError("Choose a model from the current provider catalog.");
  }
  if (!providerServiceTierAvailable(pack.setupProvider, draft.providerModel!, draft.serviceTier ?? "default")) {
    throw new ProductSetupError(`Model '${draft.providerModel}' does not advertise service tier '${draft.serviceTier}'. Choose an available speed.`);
  }
  const workspaceRequest = compileWorkspaceInitializeRequest(draft, pack.recipe, pack);
  const workspaceRequestDigest = await workspaceInitializeRequestDigest(workspaceRequest);
  return compileClientSetupPlanWithDigest(draft, pack, selection, workspaceRequestDigest);
}

function compileClientSetupPlanWithDigest(
  draft: WorkspaceSetupDraft,
  pack: WorkspacePack,
  selection: ApplySelection,
  workspaceRequestDigest: string,
): OctopusClientSetupPlan {
  const workspaceRequest = compileWorkspaceInitializeRequest(draft, pack.recipe, pack);
  const selectedPackageIds = new Set(selection.packageIds);
  const packages = selection.packageIds.map((packageId) => {
    const matches = pack.components.filter((component) => component.packageId === packageId);
    if (matches.length !== 1 || !DIGEST.test(matches[0].sourceDigest)) {
      throw new ProductSetupError(`The local component '${packageId}' no longer has one verified package snapshot.`);
    }
    return {
      id: packageId,
      source_digest: matches[0].sourceDigest,
      desired_state: "installed" as const,
    };
  });
  const projectDefaults = pack.recipe.project_defaults.flatMap((projectDefault) => {
    if (!selectedPackageIds.has(projectDefault.package_id)) return [];
    const matches = pack.components.filter((component) => component.packageId === projectDefault.package_id);
    if (matches.length !== 1 || !["application", "driver"].includes(matches[0].productKind)) {
      throw new ProductSetupError(`Project setup could not resolve '${projectDefault.package_id}' to one Application or Driver.`);
    }
    const component = matches[0];
    const projectKeys = selection.projectKeysByPackage[projectDefault.package_id];
    if (!projectKeys) {
      throw new ProductSetupError(`Project availability for '${component.name}' is missing from the reviewed setup choices.`);
    }
    return [{
      package_id: projectDefault.package_id,
      product_kind: component.productKind as "application" | "driver",
      product_id: component.productId,
      default_for_new_tasks: projectDefault.default_for_new_tasks,
      project_keys: [...projectKeys].sort(),
    }];
  });
  return {
    pack: {
      id: pack.id,
      manifest_digest: pack.manifestDigest,
      selected_recommended: [...selection.selectedRecommended].sort(),
    },
    packages: packages.sort((left, right) => left.id.localeCompare(right.id)),
    extensions: selection.extensions.map((extension) => ({
      id: extension.extensionId,
      version: extension.version,
      source_digest: extension.sourceDigest,
      desired_state: extension.desiredState,
    })).sort((left, right) => left.id.localeCompare(right.id)),
    provider: {
      interface_id: pack.recipe.provider_interface_id,
      required_state: "account_readable",
    },
    workspace: {
      request_digest: workspaceRequestDigest,
      request: workspaceRequest,
      project_defaults: projectDefaults.sort((left, right) => left.package_id.localeCompare(right.package_id)),
    },
  };
}

function assertSavedDraftMatchesSetup(
  setup: OctopusClientSetupState,
  draft: WorkspaceSetupDraft,
  snapshot: GateSnapshot,
  extensions: ClientExtensionRegistryState,
): void {
  if (draft.operationId !== setup.operationId) {
    throw new ProductSetupError("The saved choices belong to a different setup and were not applied.");
  }
  if (extensions.loading) {
    throw new ProductSetupError("Client additions are still being checked. Octopus did not resume before those choices were verified.");
  }
  const pack = requireWorkspacePack(snapshot.localPackages);
  const selection = compileApplySelection(draft, pack, extensions);
  const expected = compileClientSetupPlanWithDigest(
    draft,
    pack,
    selection,
    setup.plan.workspace.request_digest,
  );
  if (stableSetupJson(expected) !== stableSetupJson(setup.plan)) {
    throw new ProductSetupError("The saved choices differ from the immutable setup already on this device. Review the two versions instead of replacing either one.");
  }
}

function proveCompletedClientSetup(
  setup: OctopusClientSetupState,
  snapshot: GateSnapshot,
  draft?: WorkspaceSetupDraft,
): void {
  if (setup.status !== "complete" || !setup.receiptDigest) {
    throw new ProductSetupError("Setup stopped before Octopus could verify every selected part.");
  }
  const current = snapshot.clientSetup;
  if (
    current?.status !== "complete"
    || current.operationId !== setup.operationId
    || current.planDigest !== setup.planDigest
    || current.receiptDigest !== setup.receiptDigest
  ) {
    throw new ProductSetupError("The completed setup receipt could not be read back from this device.");
  }
  const receipt = snapshot.workspace.receipt;
  if (
    snapshot.workspace.status !== "configured"
    || receipt?.operationId !== setup.operationId
    || receipt.requestDigest !== setup.plan.workspace.request_digest
    || !workspaceIdentityMatchesScope(receipt, snapshot.scope)
    || !workspaceProjectMappingsMatchScope(receipt, snapshot.scope)
    || draft && !sameProjectKeys(receipt, draft)
  ) {
    throw new ProductSetupError("Setup finished, but Home and the selected project tree do not match its receipt.");
  }
}

function shellGateSnapshot(snapshot: GateSnapshot): boolean {
  return snapshot.clientSetup?.status === "complete" && completedSetupMatrix(snapshot, null).mode === "shell";
}

function completedSetupMatrix(snapshot: GateSnapshot, draft: WorkspaceSetupDraft | null): GateMatrix {
  const failure = workspaceFailure(snapshot);
  if (failure) return failure;
  const setup = snapshot.clientSetup!;
  const receipt = snapshot.workspace.receipt;
  if (
    !setup.receiptDigest
    || snapshot.workspace.status !== "configured"
    || !receipt
    || receipt.operationId !== setup.operationId
    || receipt.requestDigest !== setup.plan.workspace.request_digest
    || !workspaceIdentityMatchesScope(receipt, snapshot.scope)
  ) {
    return {
      mode: "blocked",
      title: "Completed setup does not match Home",
      detail: "The aggregate setup receipt and the visible workspace disagree, so Octopus will not guess which one is current.",
    };
  }
  if (draft?.step === "applying") {
    if (draft.operationId !== setup.operationId) {
      return {
        mode: "blocked",
        title: "Another workspace is already configured",
        detail: "The unfinished choices belong to a different setup and were not applied.",
        recovery: "discard_draft",
      };
    }
    if (!workspaceProjectMappingsMatchScope(receipt, snapshot.scope) || !sameProjectKeys(receipt, draft)) {
      return {
        mode: "blocked",
        title: "Completed setup does not match the reviewed projects",
        detail: "The initial project tree differs from the choices being applied, so Octopus did not open the workspace.",
      };
    }
  }
  return { mode: "shell", title: "", detail: "" };
}

function workspaceFailure(snapshot: GateSnapshot): GateMatrix | null {
  if (snapshot.workspace.status === "degraded") {
    return {
      mode: "blocked",
      title: "Workspace setup needs repair",
      detail: snapshot.workspace.errors.join("\n") || "The saved workspace setup could not be verified.",
    };
  }
  if (snapshot.scope.status === "invalid") {
    return {
      mode: "blocked",
      title: "Home and projects need attention",
      detail: snapshot.scope.errors.join("\n") || "The current workspace does not form one valid Octopus OS scope.",
    };
  }
  return null;
}

function resolveGateMatrix(
  snapshot: GateSnapshot,
  draft: WorkspaceSetupDraft | null,
  catalogResult: CatalogResult,
  extensions: ClientExtensionRegistryState,
): GateMatrix {
  const setup = snapshot.clientSetup;
  if (setup?.status === "in_progress") {
    if (draft && draft.operationId !== setup.operationId) {
      return {
        mode: "blocked",
        title: "Another setup is already in progress",
        detail: "Octopus kept the earlier setup plan and will not mix it with different saved choices.",
        recovery: "discard_draft",
      };
    }
    if (snapshot.workspace.status === "configured" && snapshot.workspace.receipt?.operationId !== setup.operationId) {
      return {
        mode: "blocked",
        title: "Setup proof does not match Home",
        detail: "The unfinished setup and the configured Home belong to different operations.",
      };
    }
    if (draft?.step === "applying") {
      const mismatch = savedDraftMismatch(setup, draft, snapshot, catalogResult, extensions);
      if (mismatch) return mismatch;
    }
    return { mode: "resume", title: "Finishing setup", detail: "Octopus is continuing from the last verified step." };
  }
  const failure = workspaceFailure(snapshot);
  if (failure) return failure;
  if (setup?.status === "complete") return completedSetupMatrix(snapshot, draft);
  if (snapshot.workspace.status === "configured") {
    const receipt = snapshot.workspace.receipt;
    if (!receipt || !workspaceIdentityMatchesScope(receipt, snapshot.scope)) {
      return {
        mode: "blocked",
        title: "Workspace proof does not match Home",
        detail: "The configured receipt and the visible Home identity disagree. Octopus will not open a different workspace silently.",
      };
    }
    if (draft?.step === "applying" && receipt.operationId !== draft.operationId) {
      return {
        mode: "blocked",
        title: "Another workspace is already configured",
        detail: "The unfinished setup draft belongs to a different operation and was not applied.",
      };
    }
    if (draft?.step === "applying") return { mode: "setup", title: "", detail: "" };
    return {
      mode: "blocked",
      title: "Workspace verification is incomplete",
      detail: "Home exists, but Octopus has no complete setup receipt for its local capabilities and provider account.",
    };
  }
  if (snapshot.scope.status === "ready") {
    return {
      mode: "blocked",
      title: "Home has no setup receipt",
      detail: draft?.step === "applying"
        ? "The visible Home cannot be merged with the saved setup choices without durable proof."
        : "Octopus found Home data without the durable setup proof required to open the workspace.",
    };
  }
  if (snapshot.scope.status === "unbound") return { mode: "setup", title: "", detail: "" };
  return {
    mode: "blocked",
    title: "Home is not ready",
    detail: "Octopus found workspace data without a valid Home surface. Check the state again after local recovery finishes.",
  };
}

function savedDraftMismatch(
  setup: OctopusClientSetupState,
  draft: WorkspaceSetupDraft,
  snapshot: GateSnapshot,
  catalogResult: CatalogResult,
  extensions: ClientExtensionRegistryState,
): GateMatrix | null {
  if (extensions.loading) {
    return {
      mode: "resume",
      title: "Checking saved choices",
      detail: "Octopus is verifying the saved setup choices against the setup already on this device.",
    };
  }
  if (catalogResult.error || !catalogResult.pack) {
    return {
      mode: "blocked",
      title: "Saved setup choices could not be checked",
      detail: catalogResult.error || "The local setup catalog is unavailable, so Octopus did not discard or resume the saved choices.",
      recovery: "discard_draft",
    };
  }
  try {
    assertSavedDraftMatchesSetup(setup, draft, snapshot, extensions);
    return null;
  } catch (error) {
    return {
      mode: "blocked",
      title: "Saved setup choices changed",
      detail: error instanceof ProductSetupError
        ? error.productMessage
        : "The saved choices no longer match the immutable setup on this device. Octopus did not replace either version.",
      recovery: "discard_draft",
    };
  }
}

function buildCatalog(
  localPackages: LocalPackagesProjection,
  extensions: ClientExtensionRegistryState,
  setup: OctopusClientSetupState | null,
): CatalogResult {
  const pack = requireWorkspacePack(localPackages);
  const extensionById = new Map(extensions.extensions.map((extension) => [extension.manifest.id, extension]));
  const providerStatus = findProviderCredentialStatus(pack, extensions);
  const components: WorkspaceSetupCatalogCapability["components"] = pack.components.map((component) => ({
    id: component.packageId,
    name: component.name,
    description: component.reason,
    selection: component.selection,
    installed: component.installed && component.receiptState === "valid",
    configured: componentConfigured(component, pack, setup),
  }));
  for (const extension of pack.extensions) {
    const record = extensionById.get(extension.extensionId);
    if (!record) throw new Error(`workspace pack extension '${extension.extensionId}' is unavailable`);
    components.push({
      id: extension.extensionId,
      name: record.manifest.name,
      description: extension.reason,
      selection: extension.selection,
      installed: record.installed,
      configured: record.configured,
    });
  }

  const included = pack.components.filter((component) => component.selection === "included");
  const packInstalled = included.every((component) => component.installed && component.receiptState === "valid");
  const packConfigured = packInstalled && included.every((component) => (
    componentConfigured(component, pack, setup)
  ));
  const projectDefaultPackages = new Set([...pack.recipe.project_defaults.map((entry) => entry.package_id), ...(pack.recipe.agent_presets ?? []).map((entry) => entry.component_package_id)]);
  const catalog: WorkspaceSetupCatalogCapability[] = [{
    id: pack.id,
    kind: "pack",
    name: pack.name,
    description: pack.description,
    included: true,
    recommended: false,
    installed: packInstalled,
    configured: packConfigured,
    projectSelectable: false,
    permissions: pack.components.map((component) => component.reason),
    components,
    setupDetail: providerStatusText(providerStatus, setup),
  }];

  for (const component of pack.components.filter((candidate) => (
    candidate.selection === "recommended" || projectDefaultPackages.has(candidate.packageId)
  ))) {
    catalog.push({
      id: component.packageId,
      kind: capabilityKind(component.productKind),
      name: component.name,
      description: component.reason,
      included: component.selection === "included",
      recommended: component.selection === "recommended",
      installed: component.installed && component.receiptState === "valid",
      configured: componentConfigured(component, pack, setup),
      projectSelectable: projectDefaultPackages.has(component.packageId),
      permissions: [],
      components: [],
    });
  }
  for (const extension of pack.extensions) {
    const record = requiredExtension(extensionById, extension.extensionId);
    catalog.push({
      id: extension.extensionId,
      kind: "extension",
      name: record.manifest.name,
      description: extension.reason || record.manifest.description,
      included: false,
      recommended: true,
      installed: record.installed,
      configured: record.configured,
      projectSelectable: false,
      permissions: [],
      components: [],
    });
  }
  return {
    catalog,
    pack,
    error: "",
    warning: pack.setupProvider?.modelCatalogError || (localPackages.errors.length || extensions.errors.length
      ? "Some optional local capability records could not be verified."
      : ""),
  };
}

function compileApplySelection(
  draft: WorkspaceSetupDraft,
  pack: WorkspacePack,
  extensions: ClientExtensionRegistryState,
): ApplySelection {
  assertDraftCatalogIdentity(draft, pack);
  const selected = new Map(draft.capabilities.map((capability) => [
    `${capability.kind}:${capability.id}`,
    capability,
  ]));
  const projectDefaultPackages = new Set([...pack.recipe.project_defaults.map((entry) => entry.package_id), ...(pack.recipe.agent_presets ?? []).map((entry) => entry.component_package_id)]);
  const packSelection = selected.get(`pack:${pack.id}`);
  if (!packSelection?.included || !packSelection.selected) {
    throw new ProductSetupError("The required Application Pack is missing from the saved choices. Review capabilities and try again.");
  }
  const packageIds: string[] = [];
  const selectedRecommended: string[] = [];
  const projectKeysByPackage: Record<string, string[]> = {};
  for (const component of pack.components) {
    const key = `${capabilityKind(component.productKind)}:${component.packageId}`;
    const capability = selected.get(key);
    const include = component.selection === "included" || capability?.selected === true;
    if ((component.selection === "recommended" || projectDefaultPackages.has(component.packageId)) && !capability) {
      throw new ProductSetupError("Saved capability choices no longer match the local catalog. Review choices before retrying.");
    }
    if (!include) continue;
    if (!component.available || !DIGEST.test(component.sourceDigest)) {
      throw new ProductSetupError(`${component.name} is not available from a verified local snapshot.`);
    }
    packageIds.push(component.packageId);
    if (component.selection === "recommended") selectedRecommended.push(component.packageId);
    if (projectDefaultPackages.has(component.packageId)) {
      projectKeysByPackage[component.packageId] = [...(capability?.projectKeys ?? [])].sort();
    }
  }
  const extensionById = new Map(extensions.extensions.map((extension) => [extension.manifest.id, extension]));
  const extensionSelections = pack.extensions.map((extension) => {
    const capability = selected.get(`extension:${extension.extensionId}`);
    if (!capability) {
      throw new ProductSetupError("Saved extension choices no longer match the local catalog. Review choices before retrying.");
    }
    const record = requiredExtension(extensionById, extension.extensionId);
    if (!/^sha256:[a-f0-9]{64}$/.test(record.sourceDigest) || !record.manifest.version.trim()) {
      throw new ProductSetupError(`${record.manifest.name} no longer has one verified local revision.`);
    }
    return {
      extensionId: extension.extensionId,
      version: record.manifest.version,
      sourceDigest: record.sourceDigest,
      desiredState: capability.selected
        ? "installed" as const
        : "available" as const,
    };
  });
  return {
    packageIds: packageIds.sort(),
    selectedRecommended: selectedRecommended.sort(),
    extensions: extensionSelections,
    projectKeysByPackage,
  };
}

function assertDraftCatalogIdentity(draft: WorkspaceSetupDraft, pack: WorkspacePack): void {
  const expected = new Map<string, boolean>([[`pack:${pack.id}`, true]]);
  const projectDefaultPackages = new Set([...pack.recipe.project_defaults.map((entry) => entry.package_id), ...(pack.recipe.agent_presets ?? []).map((entry) => entry.component_package_id)]);
  for (const component of pack.components) {
    if (component.selection === "recommended" || projectDefaultPackages.has(component.packageId)) {
      expected.set(
        `${capabilityKind(component.productKind)}:${component.packageId}`,
        component.selection === "included",
      );
    }
  }
  for (const extension of pack.extensions) {
    expected.set(`extension:${extension.extensionId}`, false);
  }
  if (draft.capabilities.length !== expected.size) {
    throw new ProductSetupError("Saved capability choices no longer match the local catalog. Return to review before retrying.");
  }
  for (const capability of draft.capabilities) {
    const identity = `${capability.kind}:${capability.id}`;
    const included = expected.get(identity);
    if (
      included === undefined
      || capability.included !== included
      || (included && !capability.selected)
      || capability.projectKeys.length > 0 && !projectDefaultPackages.has(capability.id)
    ) {
      throw new ProductSetupError("Saved capability choices no longer match the local catalog. Return to review before retrying.");
    }
  }
}

function assertApplyBoundary(snapshot: GateSnapshot, operationId: string): void {
  if (snapshot.workspace.status === "degraded" || snapshot.scope.status === "invalid") {
    throw new ProductSetupError("The current workspace state needs repair before setup can continue.");
  }
  if (snapshot.clientSetup?.status === "in_progress" && snapshot.clientSetup.operationId !== operationId) {
    throw new ProductSetupError("Another saved setup is still being finished. Octopus did not start a second one.");
  }
  if (snapshot.workspace.status === "configured" && snapshot.workspace.receipt?.operationId !== operationId) {
    throw new ProductSetupError("Another workspace is already configured. Octopus did not replace it.");
  }
  if (snapshot.workspace.status === "not_configured" && snapshot.scope.status !== "unbound") {
    throw new ProductSetupError("An existing Home cannot be replaced by Welcome setup.");
  }
}

function assertConfiguredRequestBoundary(
  snapshot: GateSnapshot,
  operationId: string,
  requestDigest: string,
): void {
  if (
    snapshot.workspace.status === "configured"
    && (
      snapshot.workspace.receipt?.operationId !== operationId
      || snapshot.workspace.receipt.requestDigest !== requestDigest
    )
  ) {
    throw new ProductSetupError("The saved setup choices do not match the configured workspace. Octopus did not change its capabilities.");
  }
}

function workspaceIdentityMatchesScope(receipt: WorkspaceReceipt, scope: ScopeProjection): boolean {
  if (scope.status !== "ready") return false;
  return receipt.rootEeId === scope.rootEeId
    && receipt.homeRunId === scope.homeRunId
    && receipt.homeInterfaceId === scope.homeInterfaceId;
}

function workspaceProjectMappingsMatchScope(receipt: WorkspaceReceipt, scope: ScopeProjection): boolean {
  if (scope.status !== "ready") return false;
  const scopeProjects = new Set(scope.projects.map((project) => project.eeId));
  const receiptProjects = new Set(receipt.projects.map((project) => project.eeId));
  return scopeProjects.size === scope.projects.length
    && receiptProjects.size === receipt.projects.length
    && scopeProjects.size === receiptProjects.size
    && [...receiptProjects].every((eeId) => scopeProjects.has(eeId));
}

function sameProjectKeys(receipt: WorkspaceReceipt, draft: WorkspaceSetupDraft): boolean {
  const receiptKeys = receipt.projects.map((project) => project.key).sort();
  const draftKeys = draft.projects.map((project) => project.key).sort();
  return JSON.stringify(receiptKeys) === JSON.stringify(draftKeys);
}

function stableSetupJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableSetupJson).join(",")}]`;
  if (value && typeof value === "object") {
    const row = value as Record<string, unknown>;
    return `{${Object.keys(row).sort().map((key) => `${JSON.stringify(key)}:${stableSetupJson(row[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function componentConfigured(
  component: PackComponent,
  pack: WorkspacePack,
  setup: OctopusClientSetupState | null,
): boolean {
  if (!component.installed || component.receiptState !== "valid") return false;
  if (
    setup?.status === "complete"
    && setup.plan.pack.id === pack.id
    && setup.plan.packages.some((entry) => entry.id === component.packageId)
  ) return true;
  if (component.productId === pack.recipe.provider_interface_id) {
    return false;
  }
  if (component.productKind === "driver") return false;
  return true;
}

function findProviderCredentialStatus(
  pack: WorkspacePack,
  extensions: ClientExtensionRegistryState,
  actionStatuses: readonly ClientExtensionActionStatus[] = extensions.actionStatuses,
): ClientExtensionActionStatus | null {
  const extensionIds = new Set(pack.extensions.map((extension) => extension.extensionId));
  const registrations = extensions.extensions.flatMap((extension) => {
    if (!extensionIds.has(extension.manifest.id) || extension.manifest.contribution.kind !== "builtin_action") return [];
    return extension.manifest.contribution.triggers
      .filter((trigger) => trigger.interface_id === pack.recipe.provider_interface_id)
      .map((trigger) => ({
        extensionId: extension.manifest.id,
        actionId: extension.manifest.contribution.kind === "builtin_action"
          ? extension.manifest.contribution.action_id
          : "",
        interfaceId: trigger.interface_id,
      }));
  });
  const matches = actionStatuses.filter((status) => registrations.some((registration) => (
    registration.extensionId === status.extensionId
    && registration.actionId === status.actionId
    && registration.interfaceId === status.interfaceId
  )));
  if (matches.length > 1) throw new Error("workspace provider has more than one credential readiness status");
  return matches[0] ?? null;
}

function providerStatusText(
  status: ClientExtensionActionStatus | null,
  setup: OctopusClientSetupState | null,
): string {
  if (setup?.status === "complete" && setup.plan.provider.required_state === "account_readable") {
    return "Provider account access was verified during setup.";
  }
  if (!status) return "Provider sign-in details are not available yet.";
  if (status.state === "configured") return "Sign-in details are present and will be verified with the provider during setup.";
  if (status.state === "repair_available") return "Existing sign-in details can be repaired by the optional addition; otherwise Octopus will open the normal provider sign-in during Apply.";
  if (status.state === "sign_in_required") return "Octopus will open the provider sign-in during Apply before Home becomes available.";
  if (status.state === "waiting_for_interface") return "Provider sign-in will be checked after its interface is installed.";
  return "Provider sign-in status is currently unavailable.";
}

function requireWorkspacePack(localPackages: LocalPackagesProjection | null): WorkspacePack {
  if (!localPackages || localPackages.packs.length !== 1) {
    throw new ProductSetupError("Octopus needs exactly one local workspace setup pack before Welcome can continue.");
  }
  return localPackages.packs[0];
}

function requiredExtension(
  byId: Map<string, ClientExtensionRecord>,
  extensionId: string,
): ClientExtensionRecord {
  const extension = byId.get(extensionId);
  if (!extension) throw new ProductSetupError("A selected client Extension is not available on this device.");
  return extension;
}

function capabilityKind(kind: PackComponent["productKind"]): WorkspaceSetupCapabilityKind {
  return kind;
}

function savedSetupProgress(setup: OctopusClientSetupState): WorkspaceSetupApplyState {
  const pending = setup.steps.findIndex((step) => step.state === "pending");
  const index = pending < 0 ? setup.steps.length : pending;
  const step = setup.steps[index]?.step;
  return {
    state: "running",
    label: step ? clientSetupProgressLabel(step, "verifying") : "Verifying setup",
    detail: step ? clientSetupProgressDetail(step) : "Checking the completed steps before opening Home.",
    percent: Math.round(index * 90 / setup.steps.length),
    error: "",
  };
}

function clientSetupProgressLabel(
  step: OctopusClientSetupProgress["step"],
  phase: OctopusClientSetupProgress["phase"],
): string {
  if (phase === "sealed") return "Workspace verified";
  if (step === "packages") return phase === "verifying" ? "Checking local components" : "Installing local components";
  if (step === "extensions") return phase === "verifying" ? "Checking client additions" : "Applying client additions";
  if (step === "provider") return phase === "verifying" ? "Checking provider sign-in" : "Finishing provider sign-in";
  return phase === "verifying" ? "Checking Home and projects" : "Creating Home and projects";
}

function clientSetupProgressDetail(step: OctopusClientSetupProgress["step"]): string {
  if (step === "packages") return "Verifying and installing the selected local capabilities.";
  if (step === "extensions") return "Applying the selected additions to the Octopus client.";
  if (step === "provider") return "Confirming the account and opening the normal provider sign-in only when it is needed.";
  return "Publishing Home, the project tree, and new-task defaults.";
}

function GateState({
  action,
  detail,
  title,
  tone = "neutral",
}: {
  action?: ReactNode;
  detail: string;
  title: string;
  tone?: "neutral" | "danger";
}) {
  return (
    <main className="welcome" aria-labelledby="welcome-title">
      <div className="welcome-stage">
        <section className="welcome-centered">
          {tone === "danger" ? (
            <span className="welcome-tag">Needs attention</span>
          ) : (
            <span className="setup-loading-logo"><OctopusLoadingLogo /></span>
          )}
          <h1 id="welcome-title" className="welcome-title">{title}</h1>
          <p className="welcome-body" data-tone={tone === "danger" ? "danger" : undefined}>{detail}</p>
          {action ? <div className="welcome-support">{action}</div> : null}
        </section>
      </div>
    </main>
  );
}

function OpeningState() {
  return (
    <main className="welcome flex min-h-0 items-center justify-center bg-token-main-surface-primary text-token-foreground">
      <OctopusLoadingState className="app-loading-state" label="Opening Octopus" />
    </main>
  );
}

function loadDraft(): DraftState {
  try {
    return { draft: readWorkspaceSetupDraft(), error: "" };
  } catch (error) {
    console.error("Saved workspace setup draft was refused", error);
    return { draft: null, error: "Saved setup choices could not be verified." };
  }
}

function readDraftForHostResume(trustedDraft: WorkspaceSetupDraft | null): WorkspaceSetupDraft | null {
  try {
    return readWorkspaceSetupDraft();
  } catch (error) {
    if (trustedDraft === null) return null;
    throw new ProductSetupError("The saved setup choices became unreadable. Octopus kept the durable setup plan and did not resume automatically.");
  }
}

function resetDraft(
  setDraftState: (state: DraftState) => void,
  setApplyState: (state: WorkspaceSetupApplyState) => void,
): void {
  clearWorkspaceSetupDraft();
  setDraftState({ draft: createDefaultWorkspaceSetupDraft(), error: "" });
  setApplyState(APPLY_IDLE);
}

function resetApplyingDraft(
  setDraftState: (state: DraftState) => void,
  setApplyState: (state: WorkspaceSetupApplyState) => void,
): void {
  clearWorkspaceSetupDraft();
  setDraftState({ draft: null, error: "" });
  setApplyState(APPLY_IDLE);
}

async function readGateSnapshot(includeCatalog = true, onSetup?: (setup: OctopusClientSetupState | null) => void): Promise<GateSnapshot> {
  const clientSetup = await readCurrentClientSetup();
  onSetup?.(clientSetup);
  const [workspaceRaw, scopeRaw] = await Promise.all([
    readWorkspaceSetupState(),
    readProjection("projection.operator.os.scope"),
  ]);
  const localPackages = includeCatalog && clientSetup?.status !== "complete"
    ? parseLocalPackagesProjection(await readProjection("projection.operator.os.local_packages"))
    : null;
  return {
    workspace: parseWorkspaceProjection(workspaceRaw),
    scope: parseScopeProjection(scopeRaw),
    localPackages,
    clientSetup,
  };
}

function parseWorkspaceProjection(value: unknown): WorkspaceProjection {
  const row = record(value, "workspace setup projection");
  const status = oneOf(row.status, ["not_configured", "configured", "degraded"] as const, "workspace setup status");
  const receipt = row.receipt === null ? null : parseWorkspaceReceipt(row.receipt);
  if (status === "configured" && !receipt) throw new Error("configured workspace has no receipt");
  if (status === "not_configured" && receipt) throw new Error("unconfigured workspace unexpectedly has a receipt");
  return { status, receipt, errors: stringArray(row.errors, "workspace setup errors") };
}

function parseWorkspaceReceipt(value: unknown): WorkspaceReceipt {
  const row = record(value, "workspace setup receipt");
  const projects = array(row.projects, "workspace receipt projects").map((value, index) => {
    const project = record(value, `workspace receipt project ${index + 1}`);
    return {
      key: id(project.key, `workspace receipt project ${index + 1} key`),
      eeId: id(project.eeId, `workspace receipt project ${index + 1} EE`),
    };
  });
  return {
    operationId: id(row.operationId, "workspace receipt operation"),
    requestDigest: digest(row.requestDigest, "workspace receipt request"),
    rootEeId: id(row.rootEeId, "workspace receipt root"),
    homeRunId: id(row.homeRunId, "workspace receipt Home run"),
    homeInterfaceId: id(row.homeInterfaceId, "workspace receipt Home interface"),
    projects,
  };
}

function parseScopeProjection(value: unknown): ScopeProjection {
  const row = record(value, "Octopus OS scope projection");
  const status = oneOf(row.status, ["unbound", "awaiting_home", "ready", "invalid"] as const, "Octopus OS scope status");
  const rootEnvironment = row.root_environment === null
    ? null
    : parseScopeProject(row.root_environment, "Octopus OS root environment");
  const projects = array(row.projects, "Octopus OS projects").map((value, index) => (
    parseScopeProject(value, `Octopus OS project ${index + 1}`)
  ));
  return {
    status,
    rootEeId: optionalId(row.root_ee_id, "Octopus OS root"),
    homeRunId: optionalId(row.home_run_id, "Octopus OS Home run"),
    homeInterfaceId: optionalId(row.home_interface_id, "Octopus OS Home interface"),
    rootEnvironment,
    projects,
    errors: stringArray(row.errors, "Octopus OS scope errors"),
  };
}

function parseScopeProject(value: unknown, label: string): ScopeProject {
  const row = record(value, label);
  const config = optionalRecord(row.config, `${label} config`);
  const octopusOs = optionalRecord(config.octopus_os, `${label} Octopus OS config`);
  return {
    eeId: id(row.ee_id, `${label} EE`),
    recordDigest: digest(row.record_digest, `${label} digest`),
    applicationDefaults: optionalStringArray(octopusOs.application_defaults, `${label} Application defaults`),
    driverDefaults: optionalStringArray(octopusOs.driver_defaults, `${label} Driver defaults`),
  };
}

function parseLocalPackagesProjection(value: unknown): LocalPackagesProjection {
  const row = record(value, "local package projection");
  array(row.packages, "local packages");
  const workspacePacks = array(row.packs, "local Application Packs")
    .filter((candidate) => record(candidate, "local Application Pack").workspace_setup !== undefined)
    .map(parseWorkspacePack);
  return {
    packs: workspacePacks,
    errors: stringArray(row.errors, "local package errors"),
  };
}

function parseWorkspacePack(value: unknown, index: number): WorkspacePack {
  const row = record(value, `workspace Application Pack ${index + 1}`);
  const setup = record(row.workspace_setup, `workspace Application Pack ${index + 1} setup`);
  const home = record(setup.home, `workspace Application Pack ${index + 1} Home recipe`);
  const task = record(setup.task, `workspace Application Pack ${index + 1} Task recipe`);
  const components = array(row.components, `workspace Application Pack ${index + 1} components`).map((value, componentIndex) => {
    const component = record(value, `workspace pack component ${componentIndex + 1}`);
    return {
      packageId: id(component.package_id, `workspace pack component ${componentIndex + 1} package`),
      selection: oneOf(component.selection, ["included", "recommended"] as const, `workspace pack component ${componentIndex + 1} selection`),
      reason: text(component.reason, `workspace pack component ${componentIndex + 1} reason`),
      productKind: oneOf(component.product_kind, ["application", "driver", "artifact"] as const, `workspace pack component ${componentIndex + 1} kind`),
      productId: id(component.product_id, `workspace pack component ${componentIndex + 1} product`),
      name: text(component.name, `workspace pack component ${componentIndex + 1} name`),
      available: boolean(component.available, `workspace pack component ${componentIndex + 1} availability`),
      installed: boolean(component.installed, `workspace pack component ${componentIndex + 1} install state`),
      receiptState: text(component.receipt_state, `workspace pack component ${componentIndex + 1} receipt state`),
      sourceDigest: optionalDigest(component.source_digest, `workspace pack component ${componentIndex + 1} source digest`),
    } satisfies PackComponent;
  });
  const extensions = array(row.extensions, `workspace Application Pack ${index + 1} extensions`).map((value, extensionIndex) => {
    const extension = record(value, `workspace pack extension ${extensionIndex + 1}`);
    return {
      extensionId: id(extension.extension_id, `workspace pack extension ${extensionIndex + 1} id`),
      selection: oneOf(extension.selection, ["recommended"] as const, `workspace pack extension ${extensionIndex + 1} selection`),
      reason: text(extension.reason, `workspace pack extension ${extensionIndex + 1} reason`),
    };
  });
  const contextArtifactBindings = array(home.context_artifact_bindings, "workspace Home context bindings").map((value, bindingIndex) => {
    const binding = record(value, `workspace Home context binding ${bindingIndex + 1}`);
    return { id: id(binding.id, `workspace Home context binding ${bindingIndex + 1}`) };
  });
  const projectDefaults = array(setup.project_defaults, "workspace Project defaults").map((value, defaultIndex) => {
    const projectDefault = record(value, `workspace Project default ${defaultIndex + 1}`);
    return {
      package_id: id(projectDefault.package_id, `workspace Project default ${defaultIndex + 1} package`),
      default_for_new_tasks: boolean(
        projectDefault.default_for_new_tasks,
        `workspace Project default ${defaultIndex + 1} selection`,
      ),
    };
  });
  if (new Set(projectDefaults.map((projectDefault) => projectDefault.package_id)).size !== projectDefaults.length) {
    throw new Error("workspace Project defaults repeat a package");
  }
  return {
    id: id(row.id, `workspace Application Pack ${index + 1} id`),
    manifestDigest: digest(row.manifest_digest, `workspace Application Pack ${index + 1} manifest`),
    name: text(row.name, `workspace Application Pack ${index + 1} name`),
    description: text(row.description, `workspace Application Pack ${index + 1} description`),
    components,
    extensions,
    setupProvider: row.setup_provider == null ? null : parseSetupProvider(row.setup_provider),
    recipe: {
      task: {
        worker_interface_id: id(task.worker_interface_id, "workspace Task worker"),
        context_artifact_bindings: array(task.context_artifact_bindings, "workspace Task context").map((value) => ({
          id: id(record(value, "workspace Task context binding").id, "workspace Task context id"),
        })),
        selected_worker_environment_kinds: array(task.selected_worker_environment_kinds, "workspace Task environments").map((value) => id(value, "workspace Task environment")),
      },
      provider_interface_id: id(home.provider_interface_id, "workspace Home provider"),
      worker_interface_id: id(home.worker_interface_id, "workspace Home worker"),
      ...(home.door_host_slots === undefined ? {} : { door_host_slots: home.door_host_slots as Record<string, Record<string, string>> }),
      context_artifact_bindings: contextArtifactBindings,
      selected_worker_environment_kinds: array(home.selected_worker_environment_kinds, "workspace Home worker environments")
        .map((value, kindIndex) => id(value, `workspace Home worker environment ${kindIndex + 1}`)),
      project_defaults: projectDefaults,
      agent_presets: array(setup.agent_presets ?? [], "workspace Agent presets").map(parseAgentPresetRecipe),
    },
  };
}

function parseSetupProvider(value: unknown): OctopusProviderInterface {
  const provider = record(value, "setup provider");
  return {
    id: id(provider.id, "setup provider id"), label: text(provider.label, "setup provider label"),
    descriptorPath: text(provider.descriptorPath, "setup provider path"), sourceNames: stringArray(provider.sourceNames, "provider sources"),
    model: text(provider.model, "provider selected model"), reasoningEffort: typeof provider.reasoningEffort === "string" ? provider.reasoningEffort : "",
    serviceTier: typeof provider.serviceTier === "string" ? provider.serviceTier : "",
    modelCatalogError: provider.modelCatalogError === undefined || provider.modelCatalogError === ""
      ? "" : text(provider.modelCatalogError, "provider catalogue error"),
    supportedReasoningEfforts: stringArray(provider.supportedReasoningEfforts, "provider reasoning efforts"),
    models: array(provider.models, "provider models").map((value) => {
      const model = record(value, "provider model");
      const serviceTiers = array(model.serviceTiers ?? [], "model service tiers").map((value) => {
        const tier = record(value, "model service tier");
        return { id: text(tier.id, "service tier id"), name: text(tier.name, "service tier name"), description: typeof tier.description === "string" ? tier.description : "" };
      });
      if (new Set(serviceTiers.map((tier) => tier.id)).size !== serviceTiers.length
        || serviceTiers.some((tier) => tier.id === "default")
        || (model.defaultServiceTier != null && !serviceTiers.some((tier) => tier.id === model.defaultServiceTier))) {
        throw new ProductSetupError("Provider returned invalid model service tiers.");
      }
      return { model: text(model.model, "model id"), displayName: text(model.displayName, "model name"), description: typeof model.description === "string" ? model.description : "", defaultReasoningEffort: typeof model.defaultReasoningEffort === "string" ? model.defaultReasoningEffort : "", supportedReasoningEfforts: stringArray(model.supportedReasoningEfforts, "model efforts"), showInPicker: boolean(model.showInPicker, "model visible"), isDefault: boolean(model.isDefault, "model default"), serviceTiers, defaultServiceTier: typeof model.defaultServiceTier === "string" ? model.defaultServiceTier : null };
    }),
  };
}

function parseAgentPresetRecipe(value: unknown): WorkspaceSetupAgentPreset {
  const preset = record(value, "Agent preset recipe");
  return {
    id: id(preset.id, "Agent preset id"), component_package_id: id(preset.component_package_id, "Agent preset component"),
    name: text(preset.name, "Agent preset name"), icon: text(preset.icon, "Agent preset icon"),
    worker_interface_id: id(preset.worker_interface_id, "Agent preset worker"), provider_interface_id: id(preset.provider_interface_id, "Agent preset provider"),
    context_artifact_bindings: array(preset.context_artifact_bindings, "Agent preset context").map((value) => ({ id: id(record(value, "Agent context binding").id, "Agent context id") })),
    selected_worker_environment_kinds: stringArray(preset.selected_worker_environment_kinds, "Agent preset environments"),
    selected_application_ids: stringArray(preset.selected_application_ids, "Agent preset applications"),
  };
}

function optionalRecord(value: unknown, label: string): Record<string, unknown> {
  if (value === undefined || value === null) return {};
  return record(value, label);
}

function optionalStringArray(value: unknown, label: string): string[] {
  if (value === undefined) return [];
  const values = stringArray(value, label);
  if (new Set(values).size !== values.length) throw new Error(`${label} repeats a value`);
  return values;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}

function array(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  return value;
}

function text(value: unknown, label: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${label} must be non-empty text`);
  return value;
}

function id(value: unknown, label: string): string {
  const candidate = text(value, label);
  if (!ID.test(candidate)) throw new Error(`${label} is invalid`);
  return candidate;
}

function optionalId(value: unknown, label: string): string {
  if (value === "" || value === null || value === undefined) return "";
  return id(value, label);
}

function boolean(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new Error(`${label} must be boolean`);
  return value;
}

function optionalDigest(value: unknown, label: string): string {
  if (value === "" || value === null || value === undefined) return "";
  if (typeof value !== "string" || !DIGEST.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function digest(value: unknown, label: string): string {
  if (typeof value !== "string" || !DIGEST.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function stringArray(value: unknown, label: string): string[] {
  return array(value, label).map((entry, index) => text(entry, `${label} ${index + 1}`));
}

function oneOf<const T extends readonly string[]>(value: unknown, values: T, label: string): T[number] {
  if (typeof value !== "string" || !values.includes(value)) throw new Error(`${label} is invalid`);
  return value as T[number];
}

class ProductSetupError extends Error {
  constructor(readonly productMessage: string) {
    super(productMessage);
  }
}
