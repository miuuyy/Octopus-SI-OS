import { createInterface } from "node:readline";
import manifest from "../package.json" with { type: "json" };
import { claudeAccountState, claudeVersion, ChildFailure, resolveClaudeExecutable } from "./claude.ts";
import { command, event, PROTOCOL, type Command, type Event } from "./protocol.ts";
import { errorText, Session } from "./session.ts";

const MAX_FRAME_BYTES = 1024 * 1024;
const SDK_VERSION = manifest.dependencies["@anthropic-ai/claude-agent-sdk"];

type Arguments = { doctor: boolean; claudeExecutable: string };

function parseArguments(argv: string[]): Arguments {
  let doctor = false;
  let claudeExecutable: string | null = null;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--doctor") {
      doctor = true;
    } else if (argument === "--claude-executable" && index + 1 < argv.length && claudeExecutable === null) {
      claudeExecutable = argv[index + 1];
      index += 1;
    } else {
      throw new ChildFailure("invalid_arguments", `unexpected argument '${argument}'`);
    }
  }
  if (!claudeExecutable) throw new ChildFailure("invalid_arguments", "--claude-executable is required");
  return { doctor, claudeExecutable };
}

function write(frame: Event): void {
  const checked = event.parse(frame);
  process.stdout.write(`${JSON.stringify(checked)}\n`);
}

function failure(error: unknown): ChildFailure {
  return error instanceof ChildFailure ? error : new ChildFailure("internal_error", errorText(error));
}

function doctor(claudeExecutable: string): number {
  try {
    const executable = resolveClaudeExecutable(claudeExecutable, process.env.PATH);
    const report = {
      protocol: PROTOCOL,
      sdk_version: SDK_VERSION,
      claude_version: claudeVersion(executable),
      account: claudeAccountState(executable),
    };
    process.stdout.write(`${JSON.stringify(report)}\n`);
    return 0;
  } catch (error) {
    const { code, message } = failure(error);
    process.stdout.write(`${JSON.stringify({ error: { code, message } })}\n`);
    return 1;
  }
}

async function serve(claudeExecutable: string): Promise<number> {
  const executable = resolveClaudeExecutable(claudeExecutable, process.env.PATH);
  write({ type: "ready", protocol: PROTOCOL, sdk_version: SDK_VERSION, claude_version: claudeVersion(executable) });
  const sessions = new Map<string, Session>();
  let fatal: ChildFailure | null = null;

  const reject = (frame: { request_id: number; session?: string; turn?: string }, error: unknown) => {
    const { code, message } = failure(error);
    write({ type: "error", request_id: frame.request_id, session: frame.session ?? null, turn: frame.turn ?? null, code, message, fatal: false });
  };

  const dispatch = async (frame: Command): Promise<void> => {
    switch (frame.type) {
      case "start": {
        if (sessions.has(frame.session)) throw new ChildFailure("session_exists", `session '${frame.session}' is already open`);
        const session = new Session(frame.session, frame.config, executable, write);
        sessions.set(frame.session, session);
        write({ type: "ack", request_id: frame.request_id });
        return;
      }
      case "turn": {
        const session = existing(frame.session);
        session.turn(frame.turn, frame.input.map((part) => part.text).join("\n\n"));
        write({ type: "ack", request_id: frame.request_id });
        return;
      }
      case "steer":
        await existing(frame.session).steer(frame.turn, frame.input.map((part) => part.text).join("\n\n"), frame.as_turn);
        write({ type: "ack", request_id: frame.request_id });
        return;
      case "interrupt":
        await existing(frame.session).interrupt(frame.turn);
        write({ type: "ack", request_id: frame.request_id });
        return;
      case "tool_result":
        existing(frame.session).resolveTool(frame.call_id, frame.content.map((part) => part.text).join("\n\n"), frame.is_error);
        write({ type: "ack", request_id: frame.request_id });
        return;
      case "close": {
        const session = existing(frame.session);
        sessions.delete(frame.session);
        await session.close();
        write({ type: "ack", request_id: frame.request_id });
        return;
      }
    }
  };

  const existing = (id: string): Session => {
    const session = sessions.get(id);
    if (!session) throw new ChildFailure("unknown_session", `session '${id}' is not open`);
    return session;
  };

  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of lines) {
    if (Buffer.byteLength(line, "utf8") > MAX_FRAME_BYTES) {
      fatal = new ChildFailure("frame_too_large", `command frame exceeds ${MAX_FRAME_BYTES} bytes`);
      break;
    }
    let parsed: Command;
    try {
      parsed = command.parse(JSON.parse(line));
    } catch (error) {
      fatal = new ChildFailure("invalid_command", `command frame is not valid ${PROTOCOL}: ${errorText(error)}`);
      break;
    }
    try {
      await dispatch(parsed);
    } catch (error) {
      reject(parsed, error);
    }
  }
  if (fatal) {
    write({ type: "error", request_id: null, session: null, turn: null, code: fatal.code, message: fatal.message, fatal: true });
  }
  await Promise.all([...sessions.values()].map((session) => session.close()));
  return fatal ? 2 : 0;
}

async function main(): Promise<number> {
  let args: Arguments;
  try {
    args = parseArguments(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${failure(error).message}\n`);
    return 64;
  }
  if (args.doctor) return doctor(args.claudeExecutable);
  try {
    return await serve(args.claudeExecutable);
  } catch (error) {
    const { code, message } = failure(error);
    write({ type: "error", request_id: null, session: null, turn: null, code, message, fatal: true });
    return 2;
  }
}

process.exitCode = await main();
