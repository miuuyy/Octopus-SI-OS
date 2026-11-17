import json
from pathlib import Path
import unittest
from unittest.mock import patch

import test_contract
from test_contract import event, snapshot, T1


class Reclamation(unittest.TestCase):
    setUp = test_contract.Contract.setUp
    admit = test_contract.Contract.admit
    seed = test_contract.Contract.seed

    def prepare(self):
        self.seed()
        self.w.prepare(snapshot(event(2)), T1)
        return self.w.read('pending')['notices']

    def files(self):
        return list(Path(self.b.PAYLOADS_DIR).glob('*'))

    def test_completion_commits_before_unlink_and_reclaims_pair(self):
        self.prepare()
        reclaim = self.b.reclaim_staging_pairs
        def checked(*args):
            self.assertIsNone(self.w.read('pending'))
            self.assertFalse(self.w.db.in_transaction)
            self.assertEqual(len(self.files()), 2)
            return reclaim(*args)
        with patch.object(self.b, 'hand', side_effect=self.admit), patch.object(self.b, 'reclaim_staging_pairs', side_effect=checked):
            self.assertTrue(self.w.deliver())
        self.assertEqual(self.files(), [])

    def test_no_admission_refusal_and_uncertainty_preserve_pair(self):
        notices = self.prepare()
        with self.assertRaises(RuntimeError):
            self.w.reclaim_completed(notices)
        with patch.object(self.b, 'hand', side_effect=self.b.Refused({'refused': {'reason': 'no_binding', 'detail': {}}})):
            self.assertFalse(self.w.deliver())
        with patch.object(self.b, 'hand', side_effect=OSError('uncertain')):
            with self.assertRaises(OSError):
                self.w.deliver()
        self.assertEqual(len(self.files()), 2)

    def test_failed_terminal_commit_keeps_outbox_and_materialization(self):
        self.prepare()
        write = self.w.write
        def fail(name, value):
            if name == 'state':
                raise OSError('commit failure')
            return write(name, value)
        with patch.object(self.b, 'hand', side_effect=self.admit), patch.object(self.w, 'write', side_effect=fail):
            with self.assertRaises(OSError):
                self.w.deliver()
        self.assertIsNotNone(self.w.read('pending'))
        self.assertEqual(len(self.files()), 2)
        self.assertTrue(self.w.deliver())  # recovers from admission without resending
        self.assertEqual(self.files(), [])

    def test_shared_pending_payload_and_manifest_are_protected(self):
        notices = self.prepare()
        for notice in notices:
            notice['admitted_seq'] = self.admit(notice['manifest_path'], [])
        self.w.reclaim_completed(notices)  # same prepared manifest remains pending
        self.assertEqual(len(self.files()), 2)
        path, payload = self.b.staging_pair(notices[0]['manifest_hash'])
        manifest = json.loads(Path(path).read_text())
        manifest['label'] = 'other.notice'
        raw = self.b.canonical_json(manifest)
        digest = self.b.octopus_content_hash(raw)
        other = str(Path(self.b.PAYLOADS_DIR) / (digest + '.artifact.json'))
        self.b.private_file(other, raw, self.b.IO_LIMITS['max_manifest_bytes'], 'other manifest')
        with self.w.db:
            self.w.write('pending', {'notices': [{'manifest_hash': digest}]})
        self.w.reclaim_completed(notices)
        self.assertEqual(len(self.files()), 3)

    def test_duplicate_completed_hashes_reclaim_once(self):
        notices = self.prepare()
        with self.w.db:
            pending = self.w.read('pending')
            pending['notices'] *= 2
            self.w.write('pending', pending)
        with patch.object(self.b, 'hand', side_effect=self.admit):
            self.assertTrue(self.w.deliver())
        self.assertEqual(self.files(), [])

    def test_symlink_and_path_escape_refused_without_unlink(self):
        notices = self.prepare()
        path, payload = self.b.staging_pair(notices[0]['manifest_hash'])
        outside = self.root / 'outside'
        outside.write_bytes(Path(payload).read_bytes())
        Path(payload).unlink()
        Path(payload).symlink_to(outside)
        with self.assertRaises(PermissionError):
            self.b.reclaim_staging_pairs([notices[0]['manifest_hash']], [])
        self.assertTrue(Path(path).exists())
        self.assertTrue(outside.exists())

    def test_manifest_path_escape_is_rejected(self):
        raw = self.b.canonical_json({'payload_path': str(self.root / 'outside'), 'payload_content_hash': 'a' * 64})
        digest = self.b.octopus_content_hash(raw)
        path = Path(self.b.PAYLOADS_DIR) / (digest + '.artifact.json')
        self.b.atomic_bytes(str(path), raw)
        with self.assertRaisesRegex(ValueError, 'escapes'):
            self.b.reclaim_staging_pairs([digest], [])
        self.assertTrue(path.exists())

    def test_uncommitted_transaction_refuses_cleanup(self):
        notices = self.prepare()
        with self.w.db:
            self.w.write('health', {})
            with self.assertRaisesRegex(RuntimeError, 'committed database'):
                self.w.reclaim_completed(notices)
        self.assertEqual(len(self.files()), 2)
