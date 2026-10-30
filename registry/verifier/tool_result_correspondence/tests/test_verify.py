import copy
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("correspondence", Path(__file__).parents[1] / "verify.py")
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class Correspondence(unittest.TestCase):
    def test_exact_success_and_failure_are_correspondence_but_not_effect_proof(self):
        call = {"id": "original-call", "content": [0] * 32}
        base = {"params": {"call_artifact": call, "tool": "file.read", "correlation_id": "read-1", "params": {}},
                "artifact_label": "file.read.result", "metadata": {"artifact_binding_refs": [call],
                "grant": {"artifact_access": {call["id"]: {"read": True, "change": False, "create": False, "delete": False}}}}}
        for outcome in [{"ok": True, "body": {"text": "value"}},
                        {"ok": False, "error": {"code": "tool_effect_indeterminate", "message": "Unknown effect"}}]:
            result = {"label": "file.read.result", "tool": "file.read", "correlation_id": "read-1", **outcome}
            request = {**base, "payload_utf8": json.dumps(result)}
            verifier.verify(request)
            source = {"id": "file.read.result:" + "01" * 32, "content": [1] * 32}
            carrier = {"id": "file.read.result:" + "02" * 32, "content": [2] * 32}
            bridge = copy.deepcopy(request)
            bridge["params"]["invocation"] = {"source": {"artifact": source, "source_movement_seq": 17},
                                              "artifact": carrier, "manifest_relative_path": "results/bridge.json"}
            bridge["artifact"] = {"id": carrier["id"], "content": "02" * 32}
            bridge["metadata"]["artifact_binding_refs"].append(source)
            bridge["metadata"]["grant"]["artifact_access"][source["id"]] = {"read": True, "change": False, "create": False, "delete": False}
            verifier.verify(bridge)
            for field in ("artifact", "source", "manifest_relative_path"):
                changed = copy.deepcopy(bridge)
                changed["params"]["invocation"][field] = None
                with self.assertRaises(AssertionError): verifier.verify(changed)
            for change in ["call", "duplicate", "rights", "correlation", "unknown", "status"]:
                altered = copy.deepcopy(request)
                payload = copy.deepcopy(result)
                if change == "call": altered["metadata"]["artifact_binding_refs"][0] = {**call, "content": [1] * 32}
                elif change == "duplicate": altered["metadata"]["artifact_binding_refs"].append(call)
                elif change == "rights": altered["metadata"]["grant"]["artifact_access"][call["id"]]["change"] = True
                elif change == "correlation": payload["correlation_id"] = "read-2"
                elif change == "unknown": payload["unrecognized"] = True
                else: payload["ok"] = "true"
                altered["payload_utf8"] = json.dumps(payload)
                with self.assertRaises(AssertionError, msg=change): verifier.verify(altered)

    def test_operation_results_correspond_by_operation_correlation_and_declared_label(self):
        call = {"id": "operation-call", "content": [3] * 32}
        base = {"params": {"call_artifact": call, "tool": "gmail.send_email", "correlation_id": "call-7", "params": {}},
                "artifact_label": "mcp.tool.result", "metadata": {"artifact_binding_refs": [call],
                "grant": {"artifact_access": {call["id"]: {"read": True, "change": False, "create": False, "delete": False}}}}}
        gated = {"label": "mcp.tool.result", "operation": "gmail.send_email", "correlation_id": "call-7",
                 "session_disposition": "awaiting_operator", "ok": True,
                 "result": {"effect_performed": False, "operator_request": {"request_id": "blake3:00"}}}
        declined = {"label": "mcp.tool.result", "operation": "gmail.send_email", "correlation_id": "call-7",
                    "ok": False, "error": {"code": "operator_declined"}}
        for result in (gated, declined):
            verifier.verify({**base, "payload_utf8": json.dumps(result)})
        for change in ["operation", "correlation", "label", "both", "neither", "status"]:
            payload = copy.deepcopy(gated)
            if change == "operation": payload["operation"] = "gmail.read_email"
            elif change == "correlation": payload["correlation_id"] = "call-8"
            elif change == "label": payload["label"] = "mcp.result"
            elif change == "both": payload["error"] = {"code": "x"}
            elif change == "neither": del payload["result"]
            else: payload["ok"] = "true"
            with self.assertRaises(AssertionError, msg=change):
                verifier.verify({**base, "payload_utf8": json.dumps(payload)})


if __name__ == "__main__":
    unittest.main()
