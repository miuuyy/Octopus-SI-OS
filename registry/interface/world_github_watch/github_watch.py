"""Durable resource watermarks and outbox; only ledger admission commits a poll."""
import datetime as dt
import json
import os
import sqlite3
import time
from pathlib import Path

from github_api import COMMENT_KINDS, LABEL, PollError, canonical, inventory

MAX_STATE_BYTES = 32 * 1024 * 1024


def now_iso():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


class Watcher:
    def __init__(self, config, boundary):
        self.c, self.b = config, boundary
        boundary.private_directory(boundary.VAR_DIR)
        boundary.private_directory(boundary.PAYLOADS_DIR)
        self.path = Path(boundary.VAR_DIR) / "watch.sqlite3"
        if os.path.lexists(self.path):
            boundary.stable_read(self.path, 128 * 1024 * 1024, "watch database", private=True)
        self.db = sqlite3.connect(str(self.path))
        os.chmod(self.path, 0o600)
        self.db.execute("PRAGMA journal_mode=DELETE")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("CREATE TABLE IF NOT EXISTS records (name TEXT PRIMARY KEY, value TEXT NOT NULL)")
        self.db.commit()
        self.identity = {key: config[key] for key in ("repository", "branch", "ignore_comment_authors")}
        state = self.read("state")
        if state is not None and (state.get("schema_version") != 1 or state.get("identity") != self.identity):
            self.db.close()
            raise ValueError("watch state identity mismatch; use a separate interface for another watch")
        self.health = self.read("health") or {"last_success_at": None, "failures": 0}

    def close(self):
        self.db.close()

    def read(self, name):
        row = self.db.execute("SELECT value FROM records WHERE name=?", (name,)).fetchone()
        return json.loads(row[0]) if row else None

    def write(self, name, value):
        raw = canonical(value)
        if len(raw) > MAX_STATE_BYTES:
            raise PollError("state_byte_limit")
        self.db.execute("INSERT OR REPLACE INTO records VALUES (?,?)", (name, raw.decode()))

    def status(self, state, error=None, **extra):
        self.health.update(state=state, error=error, updated_at=now_iso(), **extra)
        with self.db:
            self.write("health", self.health)
        self.b.atomic_json(str(Path(self.b.VAR_DIR) / "health.json"), self.health)

    def prepare(self, events, observed_at):
        previous = self.read("state")
        candidate = previous or {"schema_version": 1, "identity": self.identity,
                                 "seeded_at": observed_at, "resources": {}, "watermarks": {}}
        candidate = json.loads(json.dumps(candidate))
        notices, suppressed = [], 0
        ignored = {a.casefold() for a in self.c["ignore_comment_authors"]}
        for key, event in sorted(events.items()):
            old = candidate["resources"].get(key)
            stamp = event["updated_at"]
            watermark = candidate["watermarks"].get(event["kind"])
            if watermark is None or stamp > watermark["updated_at"]:
                candidate["watermarks"][event["kind"]] = {"updated_at": stamp, "ids": [event["id"]]}
            elif stamp == watermark["updated_at"]:
                watermark["ids"] = sorted(set(watermark["ids"]) | {event["id"]})
            candidate["resources"][key] = {"digest": event["digest"], "updated_at": stamp}
            if previous is None or (old and old["digest"] == event["digest"]):
                continue
            if old is None and stamp < candidate["seeded_at"]:
                continue
            if event["kind"] in COMMENT_KINDS and (event["actor"] or "").casefold() in ignored:
                suppressed += 1
                continue
            notices.append(dict(event, change="changed" if old else "observed"))
        if len(candidate["resources"]) > self.c["max_resources"]:
            raise PollError("retained_resource_limit")
        candidate["last_poll_at"] = observed_at
        candidate["generation"] = candidate.get("generation", 0) + 1
        if not notices:
            with self.db:
                self.write("state", candidate)
            return 0, suppressed
        prepared = []
        for offset in range(0, len(notices), 100):
            payload = canonical({"schema_version": 1, "repository": self.c["repository"],
                                 "branch": self.c["branch"], "observed_at": observed_at,
                                 "generation": candidate["generation"],
                                 "untrusted_external_data": True, "events": notices[offset:offset + 100]})
            digest = self.b.octopus_content_hash(payload)
            payload_path = str(Path(self.b.PAYLOADS_DIR) / (digest + ".payload"))
            self.b.private_file(payload_path, payload, self.b.IO_LIMITS["max_payload_bytes"], "github notice")
            manifest = canonical({"label": LABEL, "payload_path": payload_path, "payload_content_hash": digest})
            manifest_hash = self.b.octopus_content_hash(manifest)
            path = str(Path(self.b.PAYLOADS_DIR) / (manifest_hash + ".artifact.json"))
            self.b.private_file(path, manifest, self.b.IO_LIMITS["max_manifest_bytes"], "github manifest")
            prepared.append({"label": LABEL, "manifest_path": path, "manifest_hash": manifest_hash,
                             "admitted_seq": None})
        with self.db:
            self.write("pending", {"candidate": candidate, "notices": prepared})
        return len(notices), suppressed

    def deliver(self):
        pending = self.read("pending")
        if pending is None:
            return True
        for notice in pending["notices"]:
            self.b.verify_prepared(notice)
            artifact_id = LABEL + ":" + notice["manifest_hash"]
            admitted = self.b.find_admitted_arrival_seq(artifact_id)
            if notice["admitted_seq"] is not None and admitted != notice["admitted_seq"]:
                raise RuntimeError("recorded admission missing from ledger")
            if admitted is None:
                evidence = [{"interface_id": self.b.INTERFACE_ID, "artifact_id": artifact_id,
                             "label": "github.watch.idempotency",
                             "bytes": list(bytes.fromhex(self.b.octopus_content_hash(artifact_id.encode())))}]
                try:
                    admitted = self.b.hand(notice["manifest_path"], evidence)
                except self.b.Refused as refusal:
                    if refusal.duplicate_of(artifact_id):
                        admitted = self.b.find_admitted_arrival_seq(artifact_id)
                        if admitted is None:
                            raise RuntimeError("duplicate verdict without ledger admission") from None
                    else:
                        self.status("delivery_refused", error="runtime_refused",
                                    refusal_reason=refusal.reason, pending=True)
                        return False
                except Exception:
                    self.status("delivery_uncertain", error="runtime_verdict_uncertain", pending=True)
                    raise
            notice["admitted_seq"] = admitted
            with self.db:
                self.write("pending", pending)
        with self.db:
            self.write("state", pending["candidate"])
            self.db.execute("DELETE FROM records WHERE name='pending'")
        self.reclaim_completed(pending["notices"])
        self.status("watching", pending=False, last_admitted_seq=admitted, refusal_reason=None)
        return True

    def reclaim_completed(self, notices):
        if self.db.in_transaction:
            raise RuntimeError("staging cleanup requires a committed database")
        pending = self.read("pending")
        protected = [n["manifest_hash"] for n in pending["notices"]] if pending else []
        completed = set()
        for notice in notices:
            sequence = notice["admitted_seq"]
            artifact_id = notice["label"] + ":" + notice["manifest_hash"]
            if type(sequence) is not int or self.b.find_admitted_arrival_seq(artifact_id, sequence) != sequence:
                raise RuntimeError("staging cleanup requires confirmed admission")
            completed.add(notice["manifest_hash"])
        self.b.reclaim_staging_pairs(completed, protected)

    def cycle(self, api):
        if not self.deliver():
            return self.c["poll_seconds"]
        observed_at = now_iso()
        try:
            events = inventory(api)
            count, suppressed = self.prepare(events, observed_at)
        except PollError as error:
            retry_at = max(time.time() + self.c["poll_seconds"], error.retry_at or 0)
            self.status("poll_failed", error=error.code, failures=self.health["failures"] + 1,
                        next_poll_at=retry_at, pending=False, api_requests=api.requests)
            return retry_at - time.time()
        self.status("watching", failures=0, last_success_at=observed_at,
                    resources=len(events), pending=count > 0, new_events=count,
                    suppressed_self_comments=suppressed, api_requests=api.requests,
                    next_poll_at=time.time() + self.c["poll_seconds"])
        self.deliver()
        return self.c["poll_seconds"]
