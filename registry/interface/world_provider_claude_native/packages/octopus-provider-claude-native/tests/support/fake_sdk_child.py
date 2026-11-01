import json
import sys


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


emit({"type": "ready", "protocol": "octopus.claude_sdk_child/1", "sdk_version": "0.0.0", "claude_version": "0.0.0"})
for line in sys.stdin:
    command = json.loads(line)
    kind, request_id = command["type"], command["request_id"]
    if kind == "start":
        emit({"type": "ack", "request_id": request_id})
    elif kind == "turn":
        emit({"type": "ack", "request_id": request_id})
        session, turn = command["session"], command["turn"]
        text = command["input"][0]["text"]
        if text == "die":
            sys.exit(3)
        emit({"type": "session", "session": session, "claude_session_id": "c-1", "model": "m", "tools": []})
        for part in ("echo: ", text):
            emit({"type": "text_delta", "session": session, "turn": turn, "message_id": "msg_fake", "text": part})
        emit({"type": "item", "session": session, "turn": turn, "item": {"kind": "text", "text": "echo: " + text, "message_id": "msg_fake"}})
        emit({"type": "usage", "session": session, "turn": turn, "usage": {"input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}, "total_cost_usd": 0.0009085, "duration_ms": 1033.5, "num_turns": 1})
        emit({"type": "turn_end", "session": session, "turn": turn, "status": "completed", "result": "echo: " + text, "claude_session_id": "c-1"})
    elif kind == "close":
        emit({"type": "ack", "request_id": request_id})
    else:
        emit({"type": "error", "request_id": request_id, "session": command.get("session"), "turn": command.get("turn"), "code": "unknown_tool_call", "message": "no pending call", "fatal": False})
