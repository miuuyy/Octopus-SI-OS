import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import type { OctopusApplicationConnectionProgress, OctopusApplicationConnectionState } from "../../electron";
import { OcButton, OcDialog, OcIconButton } from "../../design-system/components";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import { copyText } from "../../runtimeFormat";
import {
  canConnectApplications,
  cancelApplicationConnection,
  connectApplicationInterface,
  formatProjectionError,
  mutateApplicationPrivateConfiguration,
  onApplicationConnectionProgress,
} from "../../projectionClient";
import { ApplicationAvailabilityList, type ApplicationAvailabilityChange } from "./ApplicationAvailability";
import { ApplicationLogo, ConnectLogoRow } from "./ApplicationLogo";
import {
  configurationFieldKey,
  type ApplicationConfigurationField,
  type ApplicationConnectionSummary,
  type ApplicationProject,
  type ApplicationProjectState,
  type ApplicationRecord,
} from "./applicationModel";

export type ApplicationConnectMode = "connect" | "reconnect";

type ConnectPhase = "ready" | "connecting" | "success" | "error";

type Blurb = { title?: string; description: string };

const DEFAULT_BLURBS = (name: string): Blurb[] => [
  {
    title: "Permissions always respected",
    description: `Octopus uses ${name} only in Home and projects where you allow it, and only in tasks connected to it.`,
  },
  {
    title: "Stays in your Octopus Home",
    description: "Connection details are saved privately on this computer. Disconnect anytime to remove access.",
  },
  {
    title: "Applications may introduce risk",
    description: "Applications are designed to respect your privacy, but content they bring into a task may try to mislead it.",
  },
];

export function ApplicationConnectDialog({
  application,
  availabilitySavingEeId,
  mode,
  moduleStates,
  onAvailabilityChange,
  onChanged,
  onClose,
  projects,
  rootEeId,
  sessionKey,
  summary,
}: {
  application: ApplicationRecord | null;
  availabilitySavingEeId: string;
  mode: ApplicationConnectMode;
  moduleStates: Record<string, OctopusApplicationConnectionState>;
  onAvailabilityChange: (application: ApplicationRecord, state: ApplicationProjectState, next: ApplicationAvailabilityChange) => void;
  onChanged: (applicationId: string) => void;
  onClose: () => void;
  projects: ApplicationProject[];
  rootEeId: string;
  sessionKey: number;
  summary: ApplicationConnectionSummary | null;
}) {
  const titleId = useId();
  const [busy, setBusy] = useState(false);
  return (
    <OcDialog ariaLabelledBy={titleId} busy={busy} onClose={onClose} open={Boolean(application && summary)} size="setup">
      {application && summary ? (
        <ConnectDialogContent
          application={application}
          availabilitySavingEeId={availabilitySavingEeId}
          key={`${application.id}:${sessionKey}`}
          mode={mode}
          moduleStates={moduleStates}
          onAvailabilityChange={onAvailabilityChange}
          onBusyChange={setBusy}
          onChanged={onChanged}
          onClose={onClose}
          projects={projects}
          rootEeId={rootEeId}
          summary={summary}
          titleId={titleId}
        />
      ) : null}
    </OcDialog>
  );
}

function ConnectDialogContent({
  application,
  availabilitySavingEeId,
  mode,
  moduleStates,
  onAvailabilityChange,
  onBusyChange,
  onChanged,
  onClose,
  projects,
  rootEeId,
  summary,
  titleId,
}: {
  application: ApplicationRecord;
  availabilitySavingEeId: string;
  mode: ApplicationConnectMode;
  moduleStates: Record<string, OctopusApplicationConnectionState>;
  onAvailabilityChange: (application: ApplicationRecord, state: ApplicationProjectState, next: ApplicationAvailabilityChange) => void;
  onBusyChange: (busy: boolean) => void;
  onChanged: (applicationId: string) => void;
  onClose: () => void;
  projects: ApplicationProject[];
  rootEeId: string;
  summary: ApplicationConnectionSummary;
  titleId: string;
}) {
  const formId = useId();
  const [phase, setPhase] = useState<ConnectPhase>("ready");
  const [values, setValues] = useState<Record<string, string>>({});
  const [invalid, setInvalid] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState<OctopusApplicationConnectionProgress | null>(null);
  const [account, setAccount] = useState<{ label: string; detail?: string } | null>(null);
  const operationRef = useRef<{ operationId: string; interfaceId: string } | null>(null);
  const attemptRef = useRef(0);
  const mountedRef = useRef(true);
  const connectAvailable = canConnectApplications();
  const name = application.name;
  const fields = summary.fields;
  const blurbs: Blurb[] = application.connection?.disclosure.length
    ? application.connection.disclosure.map((description) => ({ description }))
    : DEFAULT_BLURBS(name);
  const help = application.connection?.help ?? null;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      const operation = operationRef.current;
      operationRef.current = null;
      if (operation) void cancelApplicationConnection(operation.operationId).catch(() => {});
    };
  }, []);

  useEffect(() => onApplicationConnectionProgress((update) => {
    if (update.operationId === operationRef.current?.operationId) setProgress(update);
  }), []);

  useEffect(() => {
    onBusyChange(saving);
    return () => onBusyChange(false);
  }, [onBusyChange, saving]);

  function typedValues(interfaceId: string): Record<string, string> {
    return Object.fromEntries(fields.flatMap((field) => {
      const value = (values[configurationFieldKey(field)] ?? "").trim();
      return field.interface_id === interfaceId && value ? [[field.name, value]] : [];
    }));
  }

  function fieldSatisfied(field: ApplicationConfigurationField): boolean {
    return !field.required || field.configured || Boolean((values[configurationFieldKey(field)] ?? "").trim());
  }

  function cancelAttempt() {
    attemptRef.current += 1;
    const operation = operationRef.current;
    operationRef.current = null;
    if (operation) void cancelApplicationConnection(operation.operationId).catch(() => {});
    setProgress(null);
    setPhase((current) => current === "connecting" ? "ready" : current);
  }

  async function run(event?: FormEvent) {
    event?.preventDefault();
    if (phase === "connecting" || phase === "success" || !connectAvailable) return;
    if (fields.some((field) => !fieldSatisfied(field) || (field.kind === "url" && !validUrl((values[configurationFieldKey(field)] ?? "").trim())))) {
      setInvalid(true);
      return;
    }
    const attempt = ++attemptRef.current;
    const current = () => mountedRef.current && attemptRef.current === attempt;
    setInvalid(false);
    setError("");
    setProgress(null);
    setPhase("connecting");
    let connectedAccount: { label: string; detail?: string } | null = null;
    try {
      for (const interfaceId of summary.fieldInterfaces) {
        const updates = typedValues(interfaceId);
        if (Object.keys(updates).length === 0) continue;
        const document = application.configuration?.documents.find((candidate) => candidate.interface_id === interfaceId);
        setSaving(true);
        await mutateApplicationPrivateConfiguration({
          applicationId: application.id,
          interfaceId,
          expectedDigest: document?.status === "valid" && document.digest ? document.digest : null,
          updates,
          clears: [],
        });
        if (mountedRef.current) setSaving(false);
      }
      const targets = summary.moduleInterfaces.filter((interfaceId) => mode === "reconnect" || moduleStates[interfaceId]?.state !== "connected");
      for (const interfaceId of targets) {
        if (!current()) return;
        const operationId = crypto.randomUUID();
        operationRef.current = { operationId, interfaceId };
        setProgress(null);
        const result = await connectApplicationInterface({ operationId, applicationId: application.id, interfaceId, input: typedValues(interfaceId) });
        if (!current() || operationRef.current?.operationId !== operationId) return;
        operationRef.current = null;
        if (result.state !== "connected") throw new Error(result.message || connectionFailureCopy(result.state, name));
        connectedAccount = result.account ?? connectedAccount;
      }
      if (!current()) return;
      setAccount(connectedAccount ?? summary.account);
      setProgress(null);
      setPhase("success");
    } catch (failure) {
      if (!current()) return;
      operationRef.current = null;
      setSaving(false);
      setProgress(null);
      setError(formatProjectionError(failure) || `Couldn't connect ${name}. Try again.`);
      setPhase("error");
    } finally {
      if (mountedRef.current) setSaving(false);
      onChanged(application.id);
    }
  }

  const connecting = phase === "connecting";
  const primaryLabel = phase === "success"
    ? "Done"
    : phase === "error"
      ? "Try again"
      : connecting
        ? "Connecting…"
        : `Connect ${name}`;
  const header = (
    <div className="flex flex-col items-center gap-4">
      <ConnectLogoRow>
        <ApplicationLogo application={application} className="size-14 overflow-hidden rounded-xl border border-default shadow-sm" logoSize={26} />
      </ConnectLogoRow>
      <div className="flex flex-col items-center gap-1 text-center">
        <h2 className="heading-dialog m-0 font-semibold text-default" id={titleId}>
          {phase === "success" ? `${name} is now connected` : `Connect ${name}`}
        </h2>
        {phase === "success" ? (
          account ? (
            <>
              <div className="text-sm text-default">{account.label}</div>
              {account.detail ? <div className="text-sm text-secondary">{account.detail}</div> : null}
            </>
          ) : <div className="text-sm text-secondary">Saved privately in this Octopus Home</div>
        ) : application.developer ? <div className="text-sm text-secondary">Developed by {application.developer}</div> : null}
      </div>
    </div>
  );

  return (
    <div className="relative flex min-h-0 flex-col gap-6 overflow-hidden px-6 pt-8 pb-6">
      <OcIconButton
        appearance="dialog-close"
        className="absolute top-4 right-4"
        disabled={saving}
        glyph="close"
        label="Close"
        onClick={onClose}
      />
      {header}
      <div className="relative grid min-h-0 flex-1 overflow-hidden">
        <div className="min-h-0 overflow-y-auto">
          {phase === "success" ? (
            <div className="flex flex-col gap-2">
              <div className="text-sm font-medium text-default select-none">Use {name} in</div>
              <ApplicationAvailabilityList
                application={application}
                onChange={(state, next) => onAvailabilityChange(application, state, next)}
                projects={projects}
                rootEeId={rootEeId}
                savingEeId={availabilitySavingEeId}
                variant="dialog"
              />
              <div className="text-xs text-secondary">Existing tasks don't change.</div>
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              {fields.length ? (
                <form className="flex flex-col gap-4" id={formId} noValidate onSubmit={(event) => void run(event)}>
                  {fields.map((field) => (
                    <ConnectField
                      disabled={connecting || !connectAvailable}
                      field={field}
                      invalid={invalid && (!fieldSatisfied(field) || (field.kind === "url" && !validUrl((values[configurationFieldKey(field)] ?? "").trim())))}
                      key={configurationFieldKey(field)}
                      onChange={(value) => {
                        setInvalid(false);
                        setValues((currentValues) => ({ ...currentValues, [configurationFieldKey(field)]: value }));
                      }}
                      value={values[configurationFieldKey(field)] ?? ""}
                    />
                  ))}
                  {invalid ? <div className="text-sm text-danger" role="alert">Check the setup fields and try again</div> : null}
                </form>
              ) : null}
              {help ? (
                <div className="text-center text-sm text-secondary">
                  <a className="cursor-interaction underline underline-offset-2 hover:text-default" href={help.url} rel="noreferrer" target="_blank">{help.label}</a>
                </div>
              ) : null}
              {progress ? <ConnectProgress progress={progress} /> : null}
              <ConnectDisclosure blurbs={blurbs} />
              {phase === "error" && error ? <div className="text-center text-sm text-danger" role="alert">{error}</div> : null}
              {!connectAvailable ? <div className="text-center text-sm text-token-description-foreground">Connect applications from the Octopus desktop client.</div> : null}
            </div>
          )}
        </div>
      </div>
      <div className="flex flex-col gap-2">
        <OcButton
          className="w-full justify-center rounded-full! px-5 py-2 text-base"
          disabled={!connectAvailable || connecting || (phase !== "success" && fields.some((field) => !fieldSatisfied(field)))}
          form={fields.length && phase !== "success" ? formId : undefined}
          onClick={phase === "success" ? onClose : fields.length ? undefined : () => void run()}
          size="md"
          type={fields.length && phase !== "success" ? "submit" : "button"}
          variant="primary"
        >
          <span className="flex items-center gap-2">
            {connecting ? <OctopusSpinner className="icon-xs" /> : null}
            {primaryLabel}
          </span>
        </OcButton>
        {connecting && !saving ? (
          <OcButton className="w-full justify-center rounded-full! px-5 py-2 text-base" onClick={cancelAttempt} size="md" variant="ghost">Cancel</OcButton>
        ) : null}
      </div>
    </div>
  );
}

function ConnectField({ disabled, field, invalid, onChange, value }: {
  disabled: boolean;
  field: ApplicationConfigurationField;
  invalid: boolean;
  onChange: (value: string) => void;
  value: string;
}) {
  const id = useId();
  const environment = field.source === "environment" || field.source === "both";
  const placeholder = field.private_saved
    ? "Leave blank to keep the saved value"
    : environment
      ? "Set in the environment Octopus starts with"
      : field.placeholder || (field.required ? "" : "Optional");
  return (
    <div className="flex flex-col gap-2">
      <label className="text-sm font-medium text-default select-none" htmlFor={id}>{field.label}</label>
      <input
        aria-describedby={field.help ? `${id}-description` : undefined}
        aria-invalid={invalid || undefined}
        autoCapitalize="off"
        autoComplete="off"
        className="h-9 w-full min-w-0 rounded-md border border-primary-outline bg-primary-soft px-2.5 text-sm text-default outline-none placeholder:text-tertiary focus:border-ring aria-invalid:border-text-danger aria-invalid:ring-2 aria-invalid:ring-text-danger/20 disabled:cursor-not-allowed disabled:bg-text/5 disabled:text-secondary"
        disabled={disabled || field.source === "conflict"}
        id={id}
        inputMode={field.kind === "url" ? "url" : undefined}
        name={field.name}
        onChange={(event) => onChange(event.currentTarget.value)}
        placeholder={placeholder}
        required={field.required && !field.configured}
        spellCheck={false}
        type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
        value={value}
      />
      {field.help ? <div className="text-xs text-secondary" id={`${id}-description`}>{field.help}</div> : null}
    </div>
  );
}

function ConnectProgress({ progress }: { progress: OctopusApplicationConnectionProgress }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => setCopied(false), [progress.code]);
  return (
    <div aria-live="polite" className="flex flex-col items-center gap-3 rounded-2xl border border-default px-4 py-4 text-center">
      <p className="m-0 w-full px-2 text-sm leading-normal text-secondary whitespace-pre-line">{progress.message}</p>
      {progress.code ? (
        <div className="flex items-center gap-1">
          <span className="cursor-text text-2xl font-semibold tracking-[0.18em] text-default lining-nums tabular-nums select-text [font-feature-settings:'cv08'_on]">{progress.code}</span>
          <OcIconButton
            glyph={copied ? "check" : "copy"}
            label={copied ? "Copied" : "Copy code"}
            onClick={() => {
              void copyText(progress.code ?? "").then(() => setCopied(true), () => setCopied(false));
            }}
            variant="ghost"
          />
        </div>
      ) : null}
      {progress.url ? (
        <OcButton
          onClick={() => window.open(progress.url, "_blank", "noopener,noreferrer")}
          size="sm"
          trailingIcon="openExternal"
          variant="secondary"
        >
          Open in browser
        </OcButton>
      ) : null}
    </div>
  );
}

function ConnectDisclosure({ blurbs }: { blurbs: Blurb[] }) {
  return (
    <div className="rounded-2xl border border-default px-4 py-2">
      {blurbs.map((blurb, index) => (
        <div className="flex flex-col gap-1 border-b border-default py-2.5 last:border-none" key={`${index}:${blurb.description}`}>
          {blurb.title ? <div className="text-sm font-medium text-default">{blurb.title}</div> : null}
          <div className="text-xs text-secondary">{blurb.description}</div>
        </div>
      ))}
    </div>
  );
}

function connectionFailureCopy(state: OctopusApplicationConnectionState["state"], name: string): string {
  if (state === "expired") return `${name} needs to be connected again.`;
  if (state === "not_connected") return `${name} was not connected.`;
  return `Couldn't connect ${name}. Try again.`;
}

function validUrl(value: string): boolean {
  if (!value) return true;
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}
