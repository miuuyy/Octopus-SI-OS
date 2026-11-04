import { useEffect, useState } from "react";
import { providerServiceTiers, providerServiceTierAvailable } from "../chat/chatWorkspaceCapabilities";
import type { OctopusProviderInterface } from "../../electron";
import {
  compileWorkspaceSetupDraft,
  createWorkspaceSetupDraft,
  replaceWorkspaceSetupPreset,
  type WorkspaceSetupCapability,
  type WorkspaceSetupCapabilityKind,
  type WorkspaceSetupDraft,
  type WorkspaceSetupAgentPreset,
  type WorkspaceSetupPreset,
  type WorkspaceSetupProject,
  type WorkspaceSetupStep,
} from "../../onboarding/workspaceSetup";
import { OcCheckbox } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { OctopusLoadingLogo } from "../OctopusLoadingLogo";
import {
  draftFromSetupFile,
  parseSetupFile,
  serializeSetupFile,
  setupFileFromDraft,
  setupFileName,
} from "../../onboarding/setupFile";

export type WorkspaceSetupCatalogCapability = {
  id: string;
  kind: WorkspaceSetupCapabilityKind;
  name: string;
  description: string;
  included: boolean;
  recommended: boolean;
  installed: boolean;
  configured: boolean;
  setupDetail?: string;
  projectSelectable: boolean;
  permissions: string[];
  components: Array<{
    id: string;
    name: string;
    description: string;
    selection: "included" | "recommended";
    installed: boolean;
    configured: boolean;
  }>;
};

export type WorkspaceSetupApplyState = {
  state: "idle" | "running" | "awaiting_sign_in" | "failed";
  label: string;
  detail: string;
  percent: number | null;
  error: string;
};

type Props = {
  provider: OctopusProviderInterface | null;
  agentPresets: WorkspaceSetupAgentPreset[];
  draft: WorkspaceSetupDraft;
  catalog: WorkspaceSetupCatalogCapability[];
  catalogError: string;
  catalogLoading: boolean;
  applyState: WorkspaceSetupApplyState;
  onApply: (draft: WorkspaceSetupDraft) => Promise<void>;
  onSignIn: () => Promise<void>;
  onChange: (draft: WorkspaceSetupDraft) => void;
  onRetryCatalog: () => void;
  setupFiles: WorkspaceSetupFiles;
};

export type WorkspaceSetupFiles = {
  open: () => Promise<{ opened: false } | { opened: true; name: string; text: string }>;
  save: (request: { text: string; suggestedName: string }) => Promise<{ saved: false } | { saved: true; name: string }>;
};

const STEPS: Array<{ id: Exclude<WorkspaceSetupStep, "applying">; tag: string; title: string; body: string }> = [
  { id: "space", tag: "Your space", title: "How will you use Octopus?", body: "This only prepares a starting structure. You can change every project later." },
  { id: "projects", tag: "Projects", title: "Name the areas you care about", body: "Projects keep related tasks together. Nest one only when it really belongs inside another." },
  { id: "capabilities", tag: "Capabilities", title: "Choose what Octopus can use", body: "Included components form the working experience. Recommended additions stay optional." },
  { id: "review", tag: "Review", title: "Ready to set up", body: "Octopus installs the selected components, creates Home and these projects, then verifies the result." },
];

const PRESETS: Array<{
  id: WorkspaceSetupPreset;
  label: string;
  description: string;
  glyph: GlyphName;
}> = [
  { id: "life", label: "Life", description: "Personal plans, study and travel.", glyph: "globe" },
  { id: "work", label: "Work", description: "A focused home for your active work.", glyph: "briefcase" },
  { id: "founder", label: "Founder", description: "Life and work, with products nested under Work.", glyph: "rocket" },
  { id: "custom", label: "Custom", description: "Start with a small tree and shape it yourself.", glyph: "folder" },
];

const KIND_LABELS: Record<WorkspaceSetupCapabilityKind, string> = {
  application: "Application",
  driver: "Driver",
  artifact: "Context",
  extension: "Extension",
  pack: "Application Pack",
};

export function WorkspaceSetupOverlay({
  provider,
  agentPresets,
  draft,
  catalog,
  catalogError,
  catalogLoading,
  applyState,
  onApply,
  onSignIn,
  onChange,
  onRetryCatalog,
  setupFiles,
}: Props) {
  const activeStep = draft.step === "applying" ? "review" : draft.step;
  const activeIndex = Math.max(0, STEPS.findIndex((step) => step.id === activeStep));
  const step = STEPS[activeIndex];
  const applying = draft.step === "applying";
  const model = provider?.models.find((candidate) => candidate.model === draft.providerModel && candidate.showInPicker);
  const canContinue = stepIsComplete(draft, activeStep, catalogLoading, catalog.length)
    && (!["capabilities", "review"].includes(activeStep) || Boolean(model && provider && providerServiceTierAvailable(provider, model.model, draft.serviceTier ?? "default")));
  const [fileNotice, setFileNotice] = useState<SetupFileNotice | null>(null);

  async function importSetup() {
    setFileNotice(null);
    try {
      const picked = await setupFiles.open();
      if (!picked.opened) return;
      onChange(draftFromSetupFile(parseSetupFile(picked.text), draft, catalog, provider));
      setFileNotice({ tone: "info", text: `Imported ${picked.name}. Check each step before setting up.` });
    } catch (error) {
      setFileNotice({ tone: "error", text: setupFileMessage(error) });
    }
  }

  async function exportSetup() {
    setFileNotice(null);
    try {
      const file = setupFileFromDraft(draft);
      const saved = await setupFiles.save({ text: serializeSetupFile(file), suggestedName: setupFileName(file) });
      if (saved.saved) setFileNotice({ tone: "info", text: `Saved ${saved.name}.` });
    } catch (error) {
      setFileNotice({ tone: "error", text: setupFileMessage(error) });
    }
  }

  useEffect(() => {
    if (draft.providerModel !== undefined || draft.step === "applying" || !provider) return;
    const selected = provider.models.find((candidate) => candidate.model === provider.model && candidate.showInPicker);
    if (selected) onChange({ ...draft, providerModel: selected.model, reasoningEffort: provider.reasoningEffort ?? "", serviceTier: provider.serviceTier || selected.defaultServiceTier || "default" });
  }, [draft, provider, onChange]);

  useEffect(() => {
    if (!catalog.length || draft.step === "applying") return;
    const reconciled = reconcileCapabilities(draft, catalog);
    if (capabilitySignature(reconciled.capabilities) !== capabilitySignature(draft.capabilities)) onChange(reconciled);
  }, [catalog, draft, onChange]);

  function move(next: WorkspaceSetupStep) {
    setFileNotice(null);
    onChange({ ...draft, step: next, updatedAt: new Date().toISOString() });
  }

  function continueForward() {
    if (!canContinue || applying) return;
    const next = STEPS[activeIndex + 1]?.id;
    if (next) {
      move(next);
      return;
    }
    const applyingDraft = { ...draft, step: "applying" as const, updatedAt: new Date().toISOString() };
    onChange(applyingDraft);
    void onApply(applyingDraft);
  }

  function goBack() {
    if (applying) {
      if (applyState.state === "failed") move("review");
      return;
    }
    const previous = STEPS[activeIndex - 1]?.id;
    if (previous) move(previous);
  }

  const backVisible = applying ? applyState.state === "failed" : activeIndex > 0;

  return (
    <main className="welcome" aria-labelledby="welcome-title">
      <SetupHeader />

      <div className="welcome-stage setup-stage" data-applying={applying || undefined} key={applying ? "applying" : activeStep}>
        {applying ? (
          <ApplyingStep applyState={applyState} />
        ) : (
          <section className="setup-step">
            <span className="welcome-tag">{step.tag}</span>
            <h1 id="welcome-title" className="welcome-title">{step.title}</h1>
            <p className="welcome-body welcome-body-wide">{step.body}</p>
            {activeStep === "space" ? (
              <>
                <SpaceStep draft={draft} onChange={onChange} />
                <SetupFileAction
                  disabled={catalogLoading || !catalog.length}
                  glyph="file"
                  label="Import setup…"
                  notice={fileNotice}
                  onClick={() => void importSetup()}
                />
              </>
            ) : activeStep === "projects" ? (
              <ProjectsStep draft={draft} onChange={onChange} />
            ) : activeStep === "capabilities" ? (
              <CapabilitiesStep
                catalog={catalog}
                draft={draft}
                error={catalogError}
                loading={catalogLoading}
                onChange={onChange}
                onRetry={onRetryCatalog}
                provider={provider}
              />
            ) : (
              <>
                <ReviewStep agentPresets={agentPresets} catalog={catalog} draft={draft} />
                <SetupFileAction
                  disabled={false}
                  glyph="download"
                  label="Export setup…"
                  notice={fileNotice}
                  onClick={() => void exportSetup()}
                />
              </>
            )}
          </section>
        )}
      </div>

      <footer className="welcome-footer" data-hidden="false">
        <div className="welcome-footer-side welcome-footer-left">
          {backVisible ? (
            <button className="welcome-back" type="button" onClick={goBack}>
              {applying ? "Review choices" : "Back"}
            </button>
          ) : null}
        </div>
        <div className="welcome-step-count" aria-hidden="true">
          {applying ? null : (
            <>
              <span className="welcome-step-now">{String(activeIndex + 1).padStart(2, "0")}</span>
              <span className="welcome-step-sep">/</span>
              <span className="welcome-step-total">{String(STEPS.length).padStart(2, "0")}</span>
            </>
          )}
        </div>
        <div className="welcome-footer-side welcome-footer-right">
          {!applying ? (
            <button className="welcome-next" type="button" disabled={!canContinue} onClick={continueForward}>
              {activeStep === "review" ? "Set up Octopus" : "Continue"}
            </button>
          ) : applyState.state === "awaiting_sign_in" ? (
            <button className="welcome-next" type="button" onClick={() => void onSignIn()}>Sign in with ChatGPT</button>
          ) : applyState.state === "failed" ? (
            <button className="welcome-next" type="button" onClick={() => void onApply(draft)}>Try again</button>
          ) : (
            <button className="welcome-next" type="button" disabled>Working…</button>
          )}
        </div>
      </footer>
    </main>
  );
}

type SetupFileNotice = { tone: "info" | "error"; text: string };

function SetupFileAction({
  disabled,
  glyph,
  label,
  notice,
  onClick,
}: {
  disabled: boolean;
  glyph: GlyphName;
  label: string;
  notice: SetupFileNotice | null;
  onClick: () => void;
}) {
  return (
    <div className="setup-file-actions">
      <button className="welcome-pill" disabled={disabled} onClick={onClick} type="button">
        <Glyph name={glyph} size={14} />
        {label}
      </button>
      {notice ? (
        <span className="setup-file-note" data-tone={notice.tone} role={notice.tone === "error" ? "alert" : "status"}>
          {notice.text}
        </span>
      ) : null}
    </div>
  );
}

function setupFileMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/^Error invoking remote method '[^']+': (?:Error: )?/, "") || "Octopus could not read or write the setup file.";
}

function SpaceStep({ draft, onChange }: { draft: WorkspaceSetupDraft; onChange: (draft: WorkspaceSetupDraft) => void }) {
  return (
    <>
      <div className="setup-presets">
        {PRESETS.map((preset, index) => (
          <button
            aria-pressed={draft.preset === preset.id}
            className="welcome-card setup-card"
            key={preset.id}
            onClick={() => onChange(replaceWorkspaceSetupPreset(draft, preset.id))}
            style={{ "--i": index } as React.CSSProperties}
            type="button"
          >
            <Glyph name={preset.glyph} size={20} className="welcome-card-glyph" />
            <span className="welcome-card-title">{preset.label}</span>
            <span className="welcome-card-copy">{preset.description}</span>
          </button>
        ))}
      </div>
      <div className="setup-fields">
        <SetupField
          label="Space name"
          maxLength={80}
          onCommit={(spaceName) => onChange({ ...draft, spaceName, updatedAt: new Date().toISOString() })}
          value={draft.spaceName}
        />
        <SetupField
          label="What is it for?"
          maxLength={240}
          onCommit={(spaceGoal) => onChange({ ...draft, spaceGoal, updatedAt: new Date().toISOString() })}
          value={draft.spaceGoal}
        />
      </div>
    </>
  );
}

function ProjectsStep({ draft, onChange }: { draft: WorkspaceSetupDraft; onChange: (draft: WorkspaceSetupDraft) => void }) {
  return (
    <div className="setup-list">
      <div className="setup-row setup-row-static">
        <Glyph name="home" size={16} className="setup-row-glyph" />
        <span className="setup-row-name">{draft.spaceName}</span>
        <span className="setup-row-note">Home lives here</span>
      </div>
      {orderedProjects(draft.projects).map(({ project, depth }) => (
        <ProjectRow depth={depth} draft={draft} key={project.key} onChange={onChange} project={project} />
      ))}
      <button className="welcome-pill setup-add" type="button" onClick={() => onChange(addProject(draft))}>
        <Glyph name="plus" size={14} />
        Add project
      </button>
    </div>
  );
}

function ProjectRow({
  depth,
  draft,
  project,
  onChange,
}: {
  depth: number;
  draft: WorkspaceSetupDraft;
  project: WorkspaceSetupProject;
  onChange: (draft: WorkspaceSetupDraft) => void;
}) {
  const descendants = descendantKeys(draft.projects, project.key);
  const parents = draft.projects.filter((candidate) => candidate.key !== project.key && !descendants.has(candidate.key));
  return (
    <div className="setup-row" style={{ marginLeft: 22 * (depth + 1) }}>
      <Glyph name={project.icon as GlyphName} size={16} className="setup-row-glyph" />
      <SetupInlineInput
        label="Project name"
        maxLength={80}
        onCommit={(name) => onChange(updateProject(draft, project.key, { name, goal: name }))}
        value={project.name}
      />
      <select
        aria-label={`Parent for ${project.name}`}
        className="setup-select"
        onChange={(event) => onChange(updateProject(draft, project.key, { parentKey: event.target.value === "root" ? null : event.target.value }))}
        value={project.parentKey ?? "root"}
      >
        <option value="root">In {draft.spaceName}</option>
        {parents.map((candidate) => <option key={candidate.key} value={candidate.key}>In {candidate.name}</option>)}
      </select>
      <button aria-label={`Remove ${project.name}`} className="setup-remove" onClick={() => onChange(removeProject(draft, project.key))} type="button">
        <Glyph name="trash" size={14} />
      </button>
    </div>
  );
}

function CapabilitiesStep({
  catalog,
  draft,
  error,
  loading,
  onChange,
  onRetry,
  provider,
}: {
  catalog: WorkspaceSetupCatalogCapability[];
  draft: WorkspaceSetupDraft;
  error: string;
  loading: boolean;
  onChange: (draft: WorkspaceSetupDraft) => void;
  onRetry: () => void;
  provider: OctopusProviderInterface | null;
}) {
  if (loading) return <p className="setup-note">Loading available capabilities…</p>;
  if (!catalog.length) {
    return (
      <div className="setup-list">
        <InlineError detail={error || "The local package library contains no setup recommendations."} title="Capabilities could not be loaded" />
        <button className="welcome-pill setup-add" type="button" onClick={onRetry}>Try again</button>
      </div>
    );
  }
  const models = (provider?.models ?? []).filter((entry) => entry.showInPicker);
  const model = models.find((entry) => entry.model === draft.providerModel);
  const selections = new Map(draft.capabilities.map((capability) => [`${capability.kind}:${capability.id}`, capability]));
  return (
    <div className="setup-list">
      <div className="setup-row setup-row-static">
        <span className="setup-row-name">Model for Home</span>
        <select
          aria-label="Model for Home"
          className="setup-select"
          onChange={(event) => onChange({ ...draft, providerModel: event.target.value, reasoningEffort: models.find((entry) => entry.model === event.target.value)?.defaultReasoningEffort ?? "" })}
          value={draft.providerModel ?? ""}
        >
          {draft.providerModel ? null : <option value="">Choose a model</option>}
          {models.map((entry) => <option key={entry.model} value={entry.model}>{entry.displayName}</option>)}
        </select>
        {model?.supportedReasoningEfforts.length ? (
          <select
            aria-label="Reasoning"
            className="setup-select"
            onChange={(event) => onChange({ ...draft, reasoningEffort: event.target.value })}
            value={draft.reasoningEffort ?? ""}
          >
            {model.supportedReasoningEfforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}
          </select>
        ) : null}
        {provider && model ? <select aria-label="Speed for Home" className="setup-select"
          value={draft.serviceTier || "default"} onChange={(event) => onChange({ ...draft, serviceTier: event.target.value })}>
          {providerServiceTierAvailable(provider, model.model, draft.serviceTier ?? "default") ? null : <option value={draft.serviceTier} disabled>{draft.serviceTier} · unavailable</option>}
          {providerServiceTiers(provider, model.model).map((tier) => <option key={tier.id} value={tier.id}>{tier.name}</option>)}
        </select> : null}
      </div>
      {error ? <InlineError detail={error} title="Some local packages could not be read" /> : null}
      {catalog.map((capability) => {
        const selection = selections.get(`${capability.kind}:${capability.id}`);
        return (
          <CapabilityRow
            capability={capability}
            checked={capability.included || selection?.selected === true}
            draft={draft}
            key={`${capability.kind}:${capability.id}`}
            onChange={onChange}
            selection={selection}
          />
        );
      })}
    </div>
  );
}

function CapabilityRow({
  capability,
  checked,
  draft,
  selection,
  onChange,
}: {
  capability: WorkspaceSetupCatalogCapability;
  checked: boolean;
  draft: WorkspaceSetupDraft;
  selection: WorkspaceSetupCapability | undefined;
  onChange: (draft: WorkspaceSetupDraft) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const projectChoice = checked && capability.projectSelectable && draft.projects.length > 0;
  return (
    <div className="setup-capability" data-expanded={expanded ? "true" : "false"}>
      <div className="setup-row setup-row-static">
        <button className="setup-capability-main" onClick={() => setExpanded((value) => !value)} type="button">
          <span className="setup-row-name">{capability.name}</span>
          <span className="setup-row-note">{capability.description}</span>
        </button>
        {capability.included ? (
          <span className="setup-badge">Included</span>
        ) : (
          <span className="setup-check">
            {capability.recommended ? "Recommended" : "Add"}
            <OcCheckbox
              checked={checked}
              label={`${checked ? "Remove" : "Add"} ${capability.name}`}
              onCheckedChange={(selected) => onChange(updateCapability(draft, capability, selected === true))}
            />
          </span>
        )}
      </div>
      {expanded ? (
        <div className="setup-capability-detail">
          {capability.components.map((component) => (
            <div className="setup-detail-line" key={component.id}>
              <span className="setup-detail-name">{component.name}</span>
              <span className="setup-detail-copy">{component.description}</span>
            </div>
          ))}
          {!capability.components.length ? (
            <p className="setup-detail-copy">
              {capability.setupDetail || (capability.installed ? (capability.configured ? "Already installed and configured." : "Installed; setup is still required.") : "Available on this device.")}
            </p>
          ) : capability.setupDetail ? <p className="setup-detail-copy">{capability.setupDetail}</p> : null}
          {projectChoice ? (
            <div className="setup-detail-projects">
              <span className="setup-detail-name">Available in projects</span>
              {draft.projects.map((project) => (
                <OcCheckbox
                  checked={selection?.projectKeys.includes(project.key) === true}
                  key={project.key}
                  label={project.name}
                  onCheckedChange={(available) => onChange(updateCapabilityProject(draft, capability, project.key, available === true))}
                  showLabel
                />
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function ReviewStep({ catalog, draft, agentPresets }: { catalog: WorkspaceSetupCatalogCapability[]; draft: WorkspaceSetupDraft; agentPresets: WorkspaceSetupAgentPreset[] }) {
  const intent = compileWorkspaceSetupDraft(draft);
  const names = new Map(catalog.map((capability) => [`${capability.kind}:${capability.id}`, capability.name]));
  return (
    <div className="setup-review">
      <section className="welcome-card setup-review-card" style={{ "--i": 0 } as React.CSSProperties}>
        <span className="welcome-card-title">{intent.root.name}</span>
        <div className="setup-review-line">
          <Glyph name="home" size={14} className="setup-row-glyph" />
          Home · {draft.providerModel}
        </div>
        {orderedProjects(draft.projects).map(({ project, depth }) => (
          <div className="setup-review-line" key={project.key} style={{ paddingLeft: 18 * (depth + 1) }}>
            <Glyph name={project.icon as GlyphName} size={14} className="setup-row-glyph" />
            {project.name}
          </div>
        ))}
      </section>
      <section className="welcome-card setup-review-card" style={{ "--i": 1 } as React.CSSProperties}>
        <span className="welcome-card-title">Capabilities</span>
        {intent.capabilities.map((selection) => (
          <div className="setup-review-line" key={`${selection.kind}:${selection.id}`}>
            <span className="setup-review-name">{names.get(`${selection.kind}:${selection.id}`) ?? selection.id}</span>
            <span className="setup-row-note">
              {KIND_LABELS[selection.kind]}{selection.projectKeys.length ? ` · ${selection.projectKeys.length} projects` : ""}
            </span>
          </div>
        ))}
        {agentPresets.map((preset) => {
          const selected = draft.capabilities.find((entry) => entry.id === preset.component_package_id && entry.selected);
          const projects = draft.projects.filter((project) => selected?.projectKeys.includes(project.key)).map((project) => project.name);
          return projects.length ? (
            <div className="setup-review-line" key={preset.id}>
              <span className="setup-review-name">{preset.name} Agent</span>
              <span className="setup-row-note">{projects.join(", ")}</span>
            </div>
          ) : null;
        })}
      </section>
    </div>
  );
}

export function WorkspaceSetupProgress({ applyState }: { applyState: WorkspaceSetupApplyState }) {
  return <main className="welcome" aria-labelledby="welcome-title">
    <SetupHeader />
    <div className="welcome-stage setup-stage" data-applying><ApplyingStep applyState={applyState} /></div>
    <footer className="welcome-footer">
      <div className="welcome-footer-side welcome-footer-left" />
      <div className="welcome-footer-side welcome-footer-right">
        <button className="welcome-next" type="button" disabled>Working…</button>
      </div>
    </footer>
  </main>;
}

function SetupHeader() {
  return <header className="welcome-top">
    <div className="welcome-brand">
      <img className="welcome-brand-mark" src="./octopus-logo.png" alt="" />
      <span className="welcome-brand-name">Octopus&nbsp;OS</span>
    </div>
  </header>;
}

function ApplyingStep({ applyState }: { applyState: WorkspaceSetupApplyState }) {
  const failed = applyState.state === "failed";
  const running = applyState.state === "running";
  const shown = useShownPercent(running, applyState.percent);
  const [waiting, setWaiting] = useState(false);
  useEffect(() => {
    setWaiting(false);
    if (!running) return;
    const timer = window.setTimeout(() => setWaiting(true), 45_000);
    return () => window.clearTimeout(timer);
  }, [running, applyState.label, applyState.percent]);
  return (
    <section className="welcome-centered">
      {running ? (
        <span className="setup-loading-logo"><OctopusLoadingLogo /></span>
      ) : (
        <span className="welcome-tag">{failed ? "Needs attention" : "Account"}</span>
      )}
      <h1 id="welcome-title" className="welcome-title">
        {failed ? "Setup needs attention" : applyState.label || "Preparing your space"}
      </h1>
      <p className="welcome-body" data-tone={failed ? "danger" : undefined}>
        {failed ? applyState.error : applyState.detail || "Your choices are being verified and applied. You can safely reopen Octopus if this window closes."}
      </p>
      {running ? (
        <div aria-label={applyState.label || "Applying setup"} className="setup-progress" role="status">
          <span className="setup-progress-track"><span className="setup-progress-fill" style={{ width: `${shown}%` }} /></span>
          <span className="setup-progress-value">{Math.round(shown)}%</span>
        </div>
      ) : null}
      {waiting && running ? <p className="mt-4 text-sm text-token-description-foreground">Still waiting for this step. You can collect diagnostics from Help → Report a Problem.</p> : null}
    </section>
  );
}

function useShownPercent(running: boolean, reported: number | null): number {
  const [shown, setShown] = useState(reported ?? 0);
  useEffect(() => {
    if (!running) return;
    const target = reported ?? 0;
    setShown((current) => Math.max(current, target));
    const ceiling = Math.min(99, target + 22);
    const timer = window.setInterval(() => {
      setShown((current) => current >= ceiling ? current : current + Math.max(0.05, (ceiling - current) * 0.012));
    }, 200);
    return () => window.clearInterval(timer);
  }, [reported, running]);
  return reported === 100 ? 100 : shown;
}

function SetupField({ label, maxLength, onCommit, value }: { label: string; maxLength: number; onCommit: (value: string) => void; value: string }) {
  return (
    <label className="setup-field">
      <span className="setup-field-label">{label}</span>
      <span className="welcome-lock-row">
        <SetupInlineInput className="welcome-lock-input" label={label} maxLength={maxLength} onCommit={onCommit} value={value} />
      </span>
    </label>
  );
}

function SetupInlineInput({
  className = "setup-inline-input",
  label,
  maxLength,
  onCommit,
  value,
}: {
  className?: string;
  label: string;
  maxLength: number;
  onCommit: (value: string) => void;
  value: string;
}) {
  const [draftValue, setDraftValue] = useState(value);
  useEffect(() => setDraftValue(value), [value]);
  function commit() {
    const normalized = draftValue.trim();
    if (!normalized) {
      setDraftValue(value);
      return;
    }
    if (normalized !== value) onCommit(normalized);
  }
  return (
    <input
      aria-label={label}
      className={className}
      maxLength={maxLength}
      onBlur={commit}
      onChange={(event) => setDraftValue(event.currentTarget.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          event.currentTarget.blur();
        }
      }}
      spellCheck={false}
      value={draftValue}
    />
  );
}

function InlineError({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="setup-error" role="alert">
      <strong>{title}</strong>
      <span>{detail}</span>
    </div>
  );
}

function orderedProjects(projects: readonly WorkspaceSetupProject[]): Array<{ project: WorkspaceSetupProject; depth: number }> {
  const ordered: Array<{ project: WorkspaceSetupProject; depth: number }> = [];
  const visit = (parentKey: string | null, depth: number) => {
    for (const project of projects) {
      if ((project.parentKey ?? null) !== parentKey) continue;
      ordered.push({ project, depth });
      visit(project.key, depth + 1);
    }
  };
  visit(null, 0);
  return ordered;
}

function addProject(draft: WorkspaceSetupDraft): WorkspaceSetupDraft {
  const key = `project-${crypto.randomUUID()}`;
  const number = draft.projects.length + 1;
  return {
    ...draft,
    projects: [...draft.projects, {
      key,
      parentKey: null,
      name: `Project ${number}`,
      goal: `Project ${number}`,
      icon: "folder",
      color: "violet",
    }],
    updatedAt: new Date().toISOString(),
  };
}

function updateProject(
  draft: WorkspaceSetupDraft,
  key: string,
  update: Partial<WorkspaceSetupProject>,
): WorkspaceSetupDraft {
  return {
    ...draft,
    projects: draft.projects.map((project) => project.key === key ? { ...project, ...update } : project),
    updatedAt: new Date().toISOString(),
  };
}

function removeProject(draft: WorkspaceSetupDraft, key: string): WorkspaceSetupDraft {
  const project = draft.projects.find((candidate) => candidate.key === key);
  if (!project) return draft;
  return {
    ...draft,
    projects: draft.projects
      .filter((candidate) => candidate.key !== key)
      .map((candidate) => candidate.parentKey === key ? { ...candidate, parentKey: project.parentKey } : candidate),
    capabilities: draft.capabilities.map((capability) => ({
      ...capability,
      projectKeys: capability.projectKeys.filter((projectKey) => projectKey !== key),
    })),
    updatedAt: new Date().toISOString(),
  };
}

function updateCapability(
  draft: WorkspaceSetupDraft,
  item: WorkspaceSetupCatalogCapability,
  selected: boolean,
): WorkspaceSetupDraft {
  const identity = `${item.kind}:${item.id}`;
  const existing = draft.capabilities.find((capability) => `${capability.kind}:${capability.id}` === identity);
  const next: WorkspaceSetupCapability = {
    id: item.id,
    kind: item.kind,
    selected: item.included || selected,
    included: item.included,
    projectKeys: existing?.projectKeys ?? (item.projectSelectable && (item.included || selected) ? draft.projects.map((project) => project.key) : []),
  };
  return {
    ...draft,
    capabilities: existing
      ? draft.capabilities.map((capability) => `${capability.kind}:${capability.id}` === identity ? next : capability)
      : [...draft.capabilities, next],
    updatedAt: new Date().toISOString(),
  };
}

function updateCapabilityProject(
  draft: WorkspaceSetupDraft,
  item: WorkspaceSetupCatalogCapability,
  projectKey: string,
  available: boolean,
): WorkspaceSetupDraft {
  const identity = `${item.kind}:${item.id}`;
  return {
    ...draft,
    capabilities: draft.capabilities.map((capability) => {
      if (`${capability.kind}:${capability.id}` !== identity) return capability;
      const projectKeys = available
        ? [...new Set([...capability.projectKeys, projectKey])]
        : capability.projectKeys.filter((candidate) => candidate !== projectKey);
      return { ...capability, projectKeys };
    }),
    updatedAt: new Date().toISOString(),
  };
}

function reconcileCapabilities(
  draft: WorkspaceSetupDraft,
  catalog: readonly WorkspaceSetupCatalogCapability[],
): WorkspaceSetupDraft {
  const existing = new Map(draft.capabilities.map((capability) => [`${capability.kind}:${capability.id}`, capability]));
  const projectKeys = new Set(draft.projects.map((project) => project.key));
  const capabilities = catalog.map((item): WorkspaceSetupCapability => {
    const current = existing.get(`${item.kind}:${item.id}`);
    const selected = item.included || current?.selected === true || (!current && item.recommended);
    return {
      id: item.id,
      kind: item.kind,
      included: item.included,
      selected,
      projectKeys: item.projectSelectable && selected
        ? current
          ? current.projectKeys.filter((key) => projectKeys.has(key))
          : draft.projects.map((project) => project.key)
        : [],
    };
  });
  return { ...draft, capabilities, updatedAt: new Date().toISOString() };
}

function capabilitySignature(capabilities: readonly WorkspaceSetupCapability[]): string {
  return capabilities.map((capability) => (
    `${capability.kind}:${capability.id}:${capability.included}:${capability.selected}:${[...capability.projectKeys].sort().join(",")}`
  )).join("|");
}

function descendantKeys(projects: readonly WorkspaceSetupProject[], key: string): Set<string> {
  const descendants = new Set<string>();
  let changed = true;
  while (changed) {
    changed = false;
    for (const project of projects) {
      if (project.parentKey !== key && (!project.parentKey || !descendants.has(project.parentKey))) continue;
      if (!descendants.has(project.key)) {
        descendants.add(project.key);
        changed = true;
      }
    }
  }
  return descendants;
}

function stepIsComplete(
  draft: WorkspaceSetupDraft,
  step: Exclude<WorkspaceSetupStep, "applying">,
  catalogLoading: boolean,
  catalogSize: number,
): boolean {
  if (step === "space") return Boolean(draft.spaceName.trim() && draft.spaceGoal.trim());
  if (step === "projects") {
    try {
      compileWorkspaceSetupDraft(draft);
      return true;
    } catch {
      return false;
    }
  }
  if (step === "capabilities") return !catalogLoading && catalogSize > 0;
  try {
    compileWorkspaceSetupDraft(draft);
    return true;
  } catch {
    return false;
  }
}

export function createDefaultWorkspaceSetupDraft(): WorkspaceSetupDraft {
  return createWorkspaceSetupDraft("founder");
}
