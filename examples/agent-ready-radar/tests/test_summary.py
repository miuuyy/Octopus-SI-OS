import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = str(ROOT / "src")
sys.path.insert(0, SOURCE)

from agent_ready_radar.core import card, report, statistics
from agent_ready_radar.summary import expand_summary, summarize


class SummaryContracts(unittest.TestCase):
    def setUp(self):
        self.data = json.loads((ROOT / "benchmarks/survey-2026-11-17-summary.json").read_text())

    def test_saved_survey_preserves_counts_report_and_card(self):
        path = ROOT / "benchmarks/survey-2026-11-17-summary.json"
        self.assertLess(path.stat().st_size, 250000)
        scan = expand_summary(self.data)
        self.assertEqual(len(scan["domains"]), 60)
        self.assertEqual(sum(len(d["probes"]) for d in scan["domains"]), 420)
        self.assertEqual(statistics(scan)["by_path"]["/llms.txt"]["verified"], 33)
        self.assertEqual(statistics(scan)["by_path"]["/agents.md"]["verified"], 9)
        self.assertEqual((statistics(scan)["focus_false"], statistics(scan)["focus_naive"]), (20, 66))
        self.assertTrue((ROOT / "SURVEY_2026_11_17.md").read_text().startswith(report(scan)))
        self.assertEqual(card(scan), json.loads((ROOT / "benchmarks/home-agent-ready-radar-widget.json").read_text()))
        for domain in scan["domains"]:
            for probe in domain["probes"]:
                self.assertNotIn("body", probe)
                self.assertNotIn("body_base64", probe)
                self.assertRegex(probe["body_sha256"], r"^[0-9a-f]{64}$")

    def test_roundtrip_is_deterministic_and_does_not_mutate(self):
        scan = expand_summary(self.data)
        original = copy.deepcopy(scan)
        for domain in scan["domains"]:
            for probe in domain["probes"]:
                probe.update(body="not published", body_base64="bm90IHB1Ymxpc2hlZA==")
        compact = summarize(scan)
        self.assertEqual(expand_summary(compact), original)
        self.assertEqual(compact, self.data)
        self.assertIn("body", scan["domains"][0]["probes"][0])

    def test_bad_columns_rows_and_statistics_are_refused(self):
        for kind in ("column", "probe", "redirect", "statistics"):
            data = copy.deepcopy(self.data)
            if kind == "column":
                data["probe_columns"][0] = "body"
            elif kind == "probe":
                data["domains"][0]["probes"][0].pop()
            elif kind == "redirect":
                index = data["probe_columns"].index("redirect_chain")
                data["domains"][0]["probes"][0][index][0].pop()
            else:
                data["statistics"]["focus_false"] = 0
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                expand_summary(data)

    def test_empty_and_failed_scans_keep_unknown_availability(self):
        scan = expand_summary(self.data)
        scan["domains"] = []
        self.assertEqual(expand_summary(summarize(scan)), scan)
        probe = {"path": "/llms.txt", "requested_url": "https://example.org/llms.txt",
                 "final_url": "https://example.org/llms.txt", "status": None,
                 "classification": "timeout", "verified": False, "latency_ms": 8000,
                 "error_type": "TimeoutError", "error": "timed out", "redirect_chain": []}
        scan["domains"] = [{"origin": "https://example.org", "cohort": "fixture", "probes": [probe]}]
        self.assertEqual(expand_summary(summarize(scan)), scan)
        self.assertEqual(statistics(scan)["focus_false"], 0)

    def test_cli_summary_report_and_card(self):
        with tempfile.TemporaryDirectory() as tmp:
            raw, compact = Path(tmp) / "raw.json", Path(tmp) / "summary.json"
            raw.write_text(json.dumps(expand_summary(self.data)))
            def run(*args):
                result = subprocess.run([sys.executable, "-m", "agent_ready_radar", *map(str, args)],
                                        env={**os.environ, "PYTHONPATH": SOURCE},
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                return result.stdout
            run("summarize", raw, "-o", compact)
            self.assertEqual(compact.read_bytes(), (ROOT / "benchmarks/survey-2026-11-17-summary.json").read_bytes())
            self.assertEqual(run("report", compact), report(expand_summary(self.data)))
            self.assertEqual(json.loads(run("octopus-card", compact)), card(expand_summary(self.data)))
