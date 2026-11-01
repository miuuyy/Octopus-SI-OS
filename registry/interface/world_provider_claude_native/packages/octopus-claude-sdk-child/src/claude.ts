import { spawnSync } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import path from "node:path";

export class ChildFailure extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export function resolveClaudeExecutable(declared: string, searchPath: string | undefined): string {
  if (declared.includes("/")) {
    if (!path.isAbsolute(declared)) {
      throw new ChildFailure("claude_executable_invalid", "the Claude Code executable must be a bare command or an absolute path");
    }
    assertExecutable(declared);
    return declared;
  }
  for (const directory of (searchPath ?? "").split(":")) {
    if (!path.isAbsolute(directory)) continue;
    const candidate = path.join(directory, declared);
    try {
      assertExecutable(candidate);
      return candidate;
    } catch {
      continue;
    }
  }
  throw new ChildFailure("claude_executable_missing", `Claude Code command '${declared}' is not on PATH`);
}

function assertExecutable(candidate: string): void {
  let isFile: boolean;
  try {
    isFile = statSync(candidate).isFile();
  } catch {
    throw new ChildFailure("claude_executable_missing", `Claude Code executable '${candidate}' does not exist`);
  }
  if (!isFile) {
    throw new ChildFailure("claude_executable_invalid", `'${candidate}' is not a file`);
  }
  try {
    accessSync(candidate, constants.X_OK);
  } catch {
    throw new ChildFailure("claude_executable_invalid", `'${candidate}' is not executable`);
  }
}

function runClaude(executable: string, args: string[], timeoutMs: number, acceptedStatuses: number[] = [0]): string {
  const result = spawnSync(executable, args, { encoding: "utf8", timeout: timeoutMs, stdio: ["ignore", "pipe", "pipe"] });
  if (result.error) {
    throw new ChildFailure("claude_executable_failed", `Claude Code '${args.join(" ")}' failed: ${result.error.message}`);
  }
  if (result.status === null || !acceptedStatuses.includes(result.status)) {
    throw new ChildFailure("claude_executable_failed", `Claude Code '${args.join(" ")}' exited with status ${result.status}`);
  }
  return result.stdout;
}

export function claudeVersion(executable: string): string {
  const match = /^(\d+\.\d+\.\d+)\b/.exec(runClaude(executable, ["--version"], 4000).trim());
  if (!match) throw new ChildFailure("claude_version_unreadable", "Claude Code did not report a version");
  return match[1];
}

export type AccountState = {
  logged_in: boolean;
  auth_method: string | null;
  api_provider: string | null;
  subscription_type: string | null;
};

export function claudeAccountState(executable: string): AccountState {
  let parsed: unknown;
  try {
    parsed = JSON.parse(runClaude(executable, ["auth", "status", "--json"], 4000, [0, 1]));
  } catch (error) {
    if (error instanceof ChildFailure) throw error;
    throw new ChildFailure("claude_account_unreadable", "Claude Code account status is not JSON");
  }
  if (!parsed || typeof parsed !== "object" || typeof (parsed as { loggedIn?: unknown }).loggedIn !== "boolean") {
    throw new ChildFailure("claude_account_unreadable", "Claude Code account status has no loggedIn field");
  }
  const status = parsed as Record<string, unknown>;
  const text = (value: unknown) => (typeof value === "string" && value.length > 0 ? value : null);
  return {
    logged_in: status.loggedIn as boolean,
    auth_method: text(status.authMethod),
    api_provider: text(status.apiProvider),
    subscription_type: text(status.subscriptionType),
  };
}
