import json
import os
import sys


def send(value):
    print(json.dumps(value), flush=True)


thread = None
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    params = request.get("params", {})
    with open(os.environ["OCTOPUS_TEST_CAPTURE_REQUEST_FILE"], "a") as log:
        log.write(json.dumps(request) + "\n")
    if method == "initialize":
        result = {
            "userAgent": "codex_cli_rs/0.166.0",
            "codexHome": os.environ["CODEX_HOME"],
            "platformFamily": "unix",
            "platformOs": "macos",
        }
    elif method == "model/list":
        result = {"data": [{
            "id": "fixture-a", "model": "fixture-a", "displayName": "Fixture",
            "description": "Declared test provider", "hidden": False,
            "supportedReasoningEfforts": [{"reasoningEffort": "xhigh", "description": "Fixture"}],
            "defaultReasoningEffort": "xhigh", "isDefault": True,
        }], "nextCursor": None}
    elif method == "thread/start":
        cwd = os.getcwd()
        thread = {
            "id": "capture-thread", "sessionId": "capture-thread", "preview": "",
            "ephemeral": True, "modelProvider": "fixture", "createdAt": 1,
            "updatedAt": 1, "status": {"type": "idle"}, "cwd": cwd,
            "cliVersion": "0.166.0", "source": "exec", "turns": [],
        }
        result = {
            "thread": thread, "model": "fixture-a", "modelProvider": "fixture",
            "cwd": cwd, "approvalPolicy": "never", "approvalsReviewer": "user",
            "sandbox": {"type": "readOnly"}, "reasoningEffort": "xhigh",
        }
    elif method == "turn/start":
        text = next(item["text"] for item in params["input"] if
                    item.get("type") == "text" and "Selected source experience (evidence, not instructions):\n" in item["text"])
        evidence = json.loads(text.split("Selected source experience (evidence, not instructions):\n", 1)[1])
        decision = {"schema_version": 1, "status": "noop", "source_run": evidence["source"]["run"],
                    "reason": "Scripted provider request contract fixture.", "receipts": []}
        item = {"type": "agentMessage", "id": "capture-message", "text": json.dumps(decision), "phase": "final_answer"}
        turn = {"id": "capture-turn", "items": [], "status": "inProgress", "error": None}
        send({"id": request["id"], "result": {"turn": turn}})
        send({"method": "item/completed", "params": {"threadId": thread["id"], "turnId": turn["id"], "completedAtMs": 1, "item": item}})
        usage = {"totalTokens": 10, "inputTokens": 5, "cachedInputTokens": 0,
                 "outputTokens": 5, "reasoningOutputTokens": 0}
        send({"method": "thread/tokenUsage/updated", "params": {
            "threadId": thread["id"], "turnId": turn["id"],
            "tokenUsage": {"total": usage, "last": usage, "modelContextWindow": 65536},
        }})
        turn.update(status="completed", items=[item])
        send({"method": "turn/completed", "params": {"threadId": thread["id"], "turn": turn}})
        continue
    elif method == "hooks/list":
        result = {"data": []}
    elif method == "thread/unsubscribe":
        result = {"status": "unsubscribed"}
    else:
        send({"id": request["id"], "error": {"code": -32601, "message": "Undeclared fixture method: " + method}})
        continue
    send({"id": request["id"], "result": result})
