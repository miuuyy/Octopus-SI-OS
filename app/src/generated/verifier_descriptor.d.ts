/* Generated from schemas/verifier_descriptor.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

export type ProcessSandboxMode = "enforced" | "disabled";

export interface VerifierDescriptor {
  authority: ProcessAuthoritySpec;
  default_params?: {
    [k: string]: unknown;
  };
  id: string;
  package: string;
  script: string;
  /**
   * VERIFIERS.md §3 — a package that reaches outside the system declares how long its check may run; `params.timeout_ms` may only narrow it.
   */
  timeout_ms?: number | null;
}
export interface ProcessAuthoritySpec {
  allow_application_launch?: boolean;
  allow_local_ipc: boolean;
  allow_network: boolean;
  allow_process_signals?: boolean;
  allow_sandbox_delegation?: boolean;
  executable_argument_paths?: ProcessArgumentPathSpec[];
  executable_commands: string[];
  executable_env_paths: string[];
  executable_env_subpaths?: ProcessEnvironmentSubpathSpec[];
  executable_paths: string[];
  executable_root_slots: string[];
  executable_root_subpaths?: ProcessRootSubpathSpec[];
  optional_executable_paths?: string[];
  optional_executable_root_slots?: string[];
  optional_read_env_paths?: string[];
  optional_read_root_slots?: string[];
  optional_write_env_paths?: string[];
  optional_write_root_slots?: string[];
  read_argument_paths?: ProcessArgumentPathSpec[];
  read_env_paths: string[];
  read_env_subpaths?: ProcessEnvironmentSubpathSpec[];
  read_executable_relative_paths?: ProcessExecutableRelativePathSpec[];
  read_paths: string[];
  read_root_slots: string[];
  sandbox_mode?: ProcessSandboxMode & string;
  write_argument_paths?: ProcessArgumentPathSpec[];
  write_env_paths: string[];
  write_env_subpaths?: ProcessEnvironmentSubpathSpec[];
  write_paths: string[];
  write_root_slots: string[];
}
export interface ProcessArgumentPathSpec {
  include_parent?: boolean;
  pointer: string;
  within_root_slots: string[];
}
export interface ProcessEnvironmentSubpathSpec {
  env: string;
  relative_path: string;
}
export interface ProcessRootSubpathSpec {
  relative_path: string;
  root_slot: string;
}
export interface ProcessExecutableRelativePathSpec {
  executable: string;
  relative_path: string;
}
