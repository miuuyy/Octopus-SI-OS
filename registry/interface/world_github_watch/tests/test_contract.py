import copy
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
from github_api import API, PollError, inventory, resource, validate_config
from github_watch import Watcher
from github_watch_life import load_boundary

CONFIG = validate_config(dict(schema_version=1, repository="owner/repo", branch="main",
                              poll_seconds=300, ignore_comment_authors=["maintainer"]))
LIMITS = dict(max_control_frame_bytes=1048576, max_manifest_bytes=1048576,
              max_payload_bytes=8388608, max_stderr_bytes=262144)
T0, T1 = "2026-01-01T00:00:00Z", "2026-01-01T00:00:01Z"


def row(id=1, stamp=T1, **changes):
    return dict(dict(id=id, title="Issue", body="content", state="open", number=id,
                     updated_at=stamp, created_at=T0, html_url="https://github.com/owner/repo/issues/1",
                     user={"login": "contributor"}), **changes)


def event(id=1, **changes):
    return resource("issue", row(id, **changes))


def snapshot(*events):
    return {e["key"]: e for e in events}


class Contract(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        env = dict(OCTOPUS_INTERFACE_ID="world_github_watch", OCTOPUS_VAR_DIR=str(self.root),
                   OCTOPUS_INTERFACE_DIR=str(HERE), OCTOPUS_MOVEMENTS_JSONL=str(self.root / "movements"),
                   OCTOPUS_BLOBS_DIR=str(self.root / "blobs"),
                   OCTOPUS_INTERFACE_HOST_IO_LIMITS=json.dumps(LIMITS))
        with patch.dict(os.environ, env):
            self.b = load_boundary(HERE / "github_watch_boundary.py")
        self.w = Watcher(CONFIG, self.b)
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(lambda: self.w.close())

    def restart(self):
        self.w.close()
        self.w = Watcher(CONFIG, self.b)

    def admit(self, path, evidence):
        manifest = Path(path).read_bytes()
        artifact_id = "github.activity.notice:" + self.b.octopus_content_hash(manifest)
        ledger = self.root / "movements"
        seq = len(ledger.read_text().splitlines()) if ledger.exists() else 0
        with ledger.open("a") as f:
            f.write(json.dumps({"seq": seq, "kind": "InterfaceArrival",
                               "from": {"Interface": "world_github_watch"},
                               "artifact": {"id": artifact_id}}) + "\n")
        return seq

    def seed(self):
        self.w.prepare(snapshot(event()), T0)

    def test_seed_restart_no_history_flood_and_tied_timestamps(self):
        self.seed()
        self.assertIsNone(self.w.read("pending"))
        self.restart()
        self.assertEqual(self.w.prepare(snapshot(event()), T1)[0], 0)
        self.assertEqual(self.w.prepare(snapshot(event(), event(2)), T1)[0], 1)
        self.assertEqual(self.w.read("state")["watermarks"]["issue"]["ids"], ["1"])
        with patch.object(self.b, "hand", side_effect=self.admit):
            self.assertTrue(self.w.deliver())
        self.assertEqual(self.w.read("state")["watermarks"]["issue"]["ids"], ["1", "2"])
        self.restart()
        self.assertEqual(self.w.prepare(snapshot(event(), event(2)), T1)[0], 0)
        self.assertEqual(self.w.prepare(snapshot(event(body="edited"), event(2)), T1)[0], 1)

    def test_refusal_then_repair_retains_manifest_and_watermark(self):
        self.seed()
        before = self.w.read("state")
        self.w.prepare(snapshot(event(2)), T1)
        pending = self.w.read("pending")
        with patch.object(self.b, "hand", side_effect=self.b.Refused(
                {"refused": {"reason": "no_binding", "detail": {}}})):
            self.assertFalse(self.w.deliver())
        self.assertEqual(self.w.read("state"), before)
        self.assertEqual(self.w.read("pending"), pending)
        self.restart()
        with patch.object(self.b, "hand", side_effect=self.admit):
            self.assertTrue(self.w.deliver())
        self.assertIsNone(self.w.read("pending"))

    def test_uncertain_delivery_recovers_from_ledger_without_resend(self):
        self.seed()
        self.w.prepare(snapshot(event(2)), T1)
        def lost(path, evidence):
            self.admit(path, evidence)
            raise RuntimeError("lost verdict")
        with patch.object(self.b, "hand", side_effect=lost):
            with self.assertRaises(RuntimeError):
                self.w.deliver()
        self.assertEqual(self.w.read("state")["generation"], 1)
        self.restart()
        with patch.object(self.b, "hand", side_effect=AssertionError("replayed")):
            self.assertTrue(self.w.deliver())
        self.assertEqual(self.w.read("state")["generation"], 2)

    def test_duplicate_refusal_requires_exact_ledger_evidence(self):
        self.seed()
        self.w.prepare(snapshot(event(2)), T1)
        def duplicate(path, evidence):
            self.admit(path, evidence)
            raise self.b.Refused({"refused": {"reason": "policy", "detail": {
                "artifact_id": evidence[0]["artifact_id"],
                "message": "arrival evidence already exists in the movement ledger"}}})
        with patch.object(self.b, "hand", side_effect=duplicate):
            self.assertTrue(self.w.deliver())

    def test_self_comments_and_parent_timestamp_bumps_do_not_loop(self):
        self.seed()
        comment = resource("issue_comment", row(user={"login": "maintainer"}))
        self.assertEqual(self.w.prepare(snapshot(event(stamp="2026-01-02T00:00:00Z"), comment), T1),
                         (0, 1))
        self.assertIsNone(self.w.read("pending"))
        self.assertEqual(self.w.prepare(snapshot(comment), T1), (0, 0))

    def test_api_failure_keeps_all_watermarks_and_pending_batches_are_not_overwritten(self):
        self.seed()
        before = self.w.read("state")
        api = API(CONFIG, "private-test-token")
        with patch("github_watch.inventory", side_effect=PollError("github_http_403", 2000000000)):
            self.assertGreater(self.w.cycle(api), 300)
        self.assertEqual(before, self.w.read("state"))
        self.assertIsNone(self.w.read("pending"))
        self.restart()
        self.assertEqual(self.w.health["error"], "github_http_403")
        self.w.prepare(snapshot(event(2)), T1)
        with patch.object(self.b, "hand", side_effect=self.b.Refused({"refused": {"reason": "policy"}})), \
             patch("github_watch.inventory", side_effect=AssertionError("new poll")):
            self.w.cycle(api)

    def test_bad_pending_bytes_and_changed_identity_fail_closed(self):
        self.seed()
        self.w.prepare(snapshot(event(2)), T1)
        notice = self.w.read("pending")["notices"][0]
        Path(notice["manifest_path"]).write_text("{}")
        with self.assertRaises(ValueError):
            self.w.deliver()
        with self.assertRaises(ValueError):
            Watcher(dict(CONFIG, branch="other"), self.b)


class APIContract(unittest.TestCase):
    def test_rest_second_page_failure_and_page_budget(self):
        api = API(CONFIG, "")
        with patch.object(api, "request", side_effect=[[row(i) for i in range(100)], PollError("page_failed")]):
            with self.assertRaisesRegex(PollError, "page_failed"):
                list(api.rest("/issues"))
        api.c = dict(CONFIG, max_pages=1)
        with patch.object(api, "request", return_value=[row(i) for i in range(100)]):
            with self.assertRaisesRegex(PollError, "rest_page_limit"):
                list(api.rest("/issues"))

    def test_graphql_pages_partial_errors_and_cursor_cycles(self):
        api = API(CONFIG, "")
        page = {"data": {"node": {"comments": {"nodes": [row()], "pageInfo":
                                             {"hasNextPage": True, "endCursor": "same"}}}}}
        with patch.object(api, "request", return_value=page):
            with self.assertRaisesRegex(PollError, "invalid_graphql_page"):
                list(api.connection("query", {}, ("node", "comments")))
        class Response(io.BytesIO):
            pass
        with patch.object(api.opener, "open", return_value=Response(
                b'{"data":{"node":null},"errors":[{"type":"RATE_LIMITED","message":"private"}]}')):
            with self.assertRaisesRegex(PollError, "github_graphql_errors"):
                api.request("/graphql", {})

    def test_rate_limit_transport_and_invalid_json_are_sanitized(self):
        api = API(CONFIG, "private-test-token")
        error = urllib.error.HTTPError("https://api.github.com", 403, "SECRET",
                                       {"Retry-After": "600"}, io.BytesIO(b"SECRET"))
        with patch.object(api.opener, "open", side_effect=error):
            with self.assertRaises(PollError) as raised:
                api.request("/issues")
        self.assertEqual(str(raised.exception), "github_http_403")
        self.assertIsNotNone(raised.exception.retry_at)
        with patch.object(api.opener, "open", return_value=io.BytesIO(b'{"partial":')):
            with self.assertRaisesRegex(PollError, "invalid_api_json"):
                api.request("/issues")

    def test_every_resource_and_independent_discussion_connections(self):
        api = API(CONFIG, "")
        def rest(path, params=None, collection=None):
            if path.endswith("/issues"):
                return [row()]
            if path.endswith("/issues/comments") or path.endswith("/pulls/comments"):
                return [row()]
            if path.endswith("/pulls"):
                return [row(head={"sha": "abc"}, base={"ref": "main"})]
            if path.endswith("/reviews"):
                return [row(state="APPROVED", updated_at=None, submitted_at=T1)]
            if params["status"] == "failure":
                return [row(head_branch="main", conclusion="failure", run_attempt=1,
                            head_sha="abc", name="quality", status="completed")]
            return []
        queries = []
        def connection(query, variables, route):
            queries.append((query, route))
            return [dict(id=str(len(queries)), title="discussion", body="text", url=row()["html_url"],
                         createdAt=T0, updatedAt=T1, author={"login": "contributor"})]
        with patch.object(api, "rest", side_effect=rest), patch.object(api, "connection", side_effect=connection):
            result = inventory(api)
        self.assertEqual({e["kind"] for e in result.values()}, {
            "issue", "pull_request", "issue_comment", "review_comment", "review",
            "discussion", "discussion_comment", "discussion_reply", "workflow_failure"})
        self.assertEqual([r for _, r in queries], [
            ("repository", "discussions"), ("node", "comments"), ("node", "replies")])
        self.assertTrue(all(q.count("first:100") == 1 for q, _ in queries))

    def test_workflow_api_truncation_is_not_success(self):
        api = API(CONFIG, "")
        with patch.object(api, "request", return_value={"total_count": 1001, "workflow_runs": []}):
            with self.assertRaisesRegex(PollError, "workflow_result_limit"):
                list(api.rest("/runs", collection="workflow_runs"))


if __name__ == "__main__":
    unittest.main()
