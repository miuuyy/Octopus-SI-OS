import type { OctopusApplicationConnectionState } from "../../electron";

export type ApplicationCategory = "provider" | "connector" | "plugin" | "artifact";

export type ApplicationProjectState = {
  ee_id: string;
  is_root: boolean;
  policy_allowed: boolean;
  ceiling_allowed: boolean;
  selected_by_default: boolean;
  record_digest: string;
};

export type ApplicationConfigurationField = {
  interface_id: string;
  name: string;
  label: string;
  kind: "text" | "url" | "secret";
  required: boolean;
  authored?: boolean;
  placeholder?: string;
  help?: string;
  configured: boolean;
  private_saved: boolean;
  source: "private" | "environment" | "both" | "conflict" | "missing" | "unknown";
};

export type ApplicationConfiguration = {
  status: "not_installed" | "configured" | "required" | "unknown" | "issue";
  fields: ApplicationConfigurationField[];
  documents: Array<{ interface_id: string; status: string; digest: string }>;
  error: string;
};

export type ApplicationConnectionPresentation = {
  disclosure: string[];
  help: { label: string; url: string } | null;
};

export type ApplicationRecord = {
  id: string;
  name: string;
  description: string;
  category: ApplicationCategory;
  developer: string;
  version: string;
  icon: string;
  logo: string;
  logo_dark: string;
  logo_data_url: string;
  logo_dark_data_url: string;
  interfaces: string[];
  artifact_bindings: Array<{ artifact_id: string; label?: string }>;
  installed: boolean;
  materialized: boolean;
  partially_materialized: boolean;
  valid: boolean;
  missing_interface_ids: string[];
  descriptor_path: string;
  manifest_path: string;
  manifest_digest: string;
  package_id: string;
  installed_digest: string;
  source_available: boolean;
  source_kind: "local" | "none";
  source_digest: string;
  install_packages: Array<{ id: string; name: string; source_digest: string; installed: boolean; receipt_state: string }>;
  source_file_count: number;
  receipt_state: string;
  receipt_digest: string;
  managed: boolean;
  install_block_reason: string;
  removal_allowed: boolean;
  removal_block_reason: string;
  connected_run_ids: string[];
  partially_connected_run_ids?: string[];
  project_states: ApplicationProjectState[];
  configuration?: ApplicationConfiguration;
  connection?: ApplicationConnectionPresentation;
  connection_interfaces?: string[];
};

export type ApplicationProject = {
  ee_id: string;
  name: string;
  icon: string;
  color: string;
  goal: string;
  parent_ee_id: string;
  record_digest: string;
};

export type ApplicationConnectionProbeState = {
  checking: boolean;
  interfaces: Record<string, OctopusApplicationConnectionState>;
};

export type ApplicationConnectionStatus = "none" | "checking" | "connected" | "not_connected" | "expired" | "error";

export type ApplicationConnectionSummary = {
  status: ApplicationConnectionStatus;
  account: { label: string; detail?: string } | null;
  message: string;
  moduleInterfaces: string[];
  fieldInterfaces: string[];
  fields: ApplicationConfigurationField[];
};

export function applicationConnectionSummary(
  application: ApplicationRecord,
  probe: ApplicationConnectionProbeState | undefined,
): ApplicationConnectionSummary {
  const fields = (application.configuration?.fields ?? []).filter((field) => field.authored !== false);
  const moduleInterfaces = application.installed ? application.connection_interfaces ?? [] : [];
  const fieldInterfaces = application.installed
    ? [...new Set(fields.map((field) => field.interface_id))].filter((interfaceId) => !moduleInterfaces.includes(interfaceId))
    : [];
  const base = { account: null, message: "", moduleInterfaces, fieldInterfaces, fields };
  if (!application.installed || (moduleInterfaces.length === 0 && fieldInterfaces.length === 0)) {
    return { ...base, status: "none" };
  }
  if (application.configuration?.status === "issue") {
    return { ...base, status: "error", message: application.configuration.error || "Saved connection details could not be verified." };
  }
  const states: Array<OctopusApplicationConnectionState | { state: "checking" }> = [
    ...moduleInterfaces.map((interfaceId) => probe?.interfaces[interfaceId] ?? { state: "checking" as const }),
    ...fieldInterfaces.map((interfaceId): OctopusApplicationConnectionState => {
      const interfaceFields = fields.filter((field) => field.interface_id === interfaceId);
      if (interfaceFields.some((field) => field.source === "conflict")) {
        return { state: "error", message: "A saved value conflicts with the environment Octopus starts with." };
      }
      return { state: interfaceFields.every((field) => !field.required || field.configured) ? "connected" : "not_connected" };
    }),
  ];
  const failure = states.find((state) => state.state === "error") as OctopusApplicationConnectionState | undefined;
  const account = states.flatMap((state) => "account" in state && state.account ? [state.account] : [])[0] ?? null;
  if (failure) return { ...base, account, status: "error", message: failure.message || "The connection could not be checked." };
  if (states.some((state) => state.state === "checking")) return { ...base, account, status: "checking" };
  const expired = states.find((state) => state.state === "expired") as OctopusApplicationConnectionState | undefined;
  if (expired) return { ...base, account, status: "expired", message: expired.message || "" };
  const missing = states.find((state) => state.state === "not_connected") as OctopusApplicationConnectionState | undefined;
  if (missing) return { ...base, account, status: "not_connected", message: missing.message || "" };
  return { ...base, account, status: "connected" };
}

export function configurationFieldKey(field: Pick<ApplicationConfigurationField, "interface_id" | "name">): string {
  return `${field.interface_id}\0${field.name}`;
}

export function projectLabel(project: ApplicationProject | undefined, eeId: string, rootEeId: string): string {
  if (eeId === rootEeId) return "Home";
  return project?.name || "Untitled project";
}
