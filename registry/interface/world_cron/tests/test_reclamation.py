"""Completion ordering and content-addressed staging reachability."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import test_one_shot


class Reclamation(unittest.TestCase):
    setUp = test_one_shot.OneShotContract.setUp
    reload = test_one_shot.OneShotContract.reload
    schedule = test_one_shot.OneShotContract.schedule
    sleep = test_one_shot.OneShotContract.sleep
    admit = test_one_shot.OneShotContract.admit
    execute = test_one_shot.OneShotContract.execute

    def files(self):
        return list(Path(self.module.PAYLOADS_DIR).glob('*'))

    def test_completed_one_shot_reclaims_and_restarts_without_staging(self):
        self.assertEqual(self.execute(), 7)
        self.assertEqual(self.files(), [])
        self.module = self.reload()
        self.assertEqual(self.execute(), 7)
        self.assertEqual(len(self.emissions), 1)

    def test_no_admission_or_uncertain_delivery_preserves_both_files(self):
        for failure in (RuntimeError('uncertain'), self.module.DeliveryRefused({'refused': {}})):
            with patch.object(self.module, 'hand_artifact_with_evidence', side_effect=failure):
                with self.assertRaises(RuntimeError):
                    self.execute(hand=self.module.hand_artifact_with_evidence)
            self.assertEqual(len(self.files()), 2)
            self.assertEqual(self.module.load_one_shot_state(self.schedule())['status'], 'prepared')

    def test_admission_before_terminal_commit_preserves_both_files(self):
        write = self.module.atomic_write_json
        def fail_commit(path, value, *args, **kwargs):
            if value.get('status') == 'admitted':
                raise OSError('disk failure')
            return write(path, value, *args, **kwargs)
        with patch.object(self.module, 'atomic_write_json', side_effect=fail_commit):
            with self.assertRaisesRegex(OSError, 'disk failure'):
                self.execute()
        self.assertEqual(len(self.files()), 2)
        # Even confirmed admission cannot reclaim a prepared durable reference.
        state = self.module.load_one_shot_state(self.schedule())
        self.module.reclaim_completed_artifact(state['artifact_label'], state['manifest_hash'], 7)
        self.assertEqual(len(self.files()), 2)
        self.execute()
        self.assertEqual(self.files(), [])

    def test_shared_payload_with_different_prepared_manifest_is_protected(self):
        def shared(path, evidence):
            manifest = json.loads(Path(path).read_text())
            payload = Path(manifest['payload_path']).read_bytes()
            _, digest = self.module.materialize_artifact('other.notice', payload)
            self.module.atomic_write_json(self.module.INTERVAL_STATE_PATH, {'status': 'prepared', 'manifest_hash': digest})
            return self.admit(path, evidence)
        self.execute(shared)
        self.assertEqual(len(self.files()), 3)

    def test_symlink_or_escaping_manifest_refuses_cleanup(self):
        m = self.module
        m.private_directory(m.PAYLOADS_DIR)
        target = self.root / 'outside'
        target.write_bytes(b'do not delete')
        digest = 'a' * 64
        path = Path(m.PAYLOADS_DIR) / ('artifact-' + digest + '.artifact.json')
        path.symlink_to(target)
        with self.assertRaises(RuntimeError):
            m.reclaim_staging_pair(digest, [])
        self.assertTrue(path.is_symlink())
        self.assertEqual(target.read_bytes(), b'do not delete')

    def test_prepared_same_hash_and_missing_admission_are_never_reclaimed(self):
        def fail_commit(path, evidence):
            self.admit(path, evidence)
            raise RuntimeError('lost acknowledgement')
        with self.assertRaises(RuntimeError):
            self.execute(fail_commit)
        state = self.module.load_one_shot_state(self.schedule())
        with self.assertRaises(RuntimeError):
            self.module.reclaim_completed_artifact(state['artifact_label'], state['manifest_hash'], None)
        self.module.reclaim_completed_artifact(state['artifact_label'], state['manifest_hash'], 7)
        self.assertEqual(len(self.files()), 2)

    def test_manifest_path_escape_is_rejected(self):
        m = self.module
        m.private_directory(m.PAYLOADS_DIR)
        raw = m.canonical_json({'payload_path': str(self.root / 'outside'), 'payload_content_hash': 'a' * 64})
        digest = m.octopus_content_hash(raw)
        path = Path(m.PAYLOADS_DIR) / ('artifact-' + digest + '.artifact.json')
        m.atomic_write_bytes(str(path), raw)
        with self.assertRaisesRegex(ValueError, 'escapes'):
            m.reclaim_staging_pair(digest, [])
        self.assertTrue(path.exists())

    def test_interval_and_start_cleanup_after_durable_completion(self):
        m = self.module
        self.config['schedule'].update(due_at=None, interval_seconds=60)
        schedule = self.schedule()
        with patch.object(m, 'hand_artifact_with_evidence', self.admit):
            m.emit_interval_due(schedule, '', {'status': 'waiting', 'next_due_unix_ns': 1})
            self.assertEqual(m.load_interval_state(schedule)['status'], 'waiting')
            self.assertEqual(self.files(), [])
            m.emit_fire_on_start(m.now(schedule), '', 'interval', schedule)
            self.assertEqual(m.load_fire_on_start_state(schedule)['status'], 'admitted')
            self.assertEqual(self.files(), [])

    def test_interval_commit_failure_keeps_prepared_pair(self):
        m = self.module
        self.config['schedule'].update(due_at=None, interval_seconds=60)
        schedule = self.schedule()
        commit = m.commit_interval_state
        def fail(schedule, status, *args):
            if status == 'waiting':
                raise OSError('commit failure')
            return commit(schedule, status, *args)
        with patch.object(m, 'hand_artifact_with_evidence', self.admit), patch.object(m, 'commit_interval_state', side_effect=fail):
            with self.assertRaises(OSError):
                m.emit_interval_due(schedule, '', {'status': 'waiting', 'next_due_unix_ns': 1})
        self.assertEqual(m.load_interval_state(schedule)['status'], 'prepared')
        self.assertEqual(len(self.files()), 2)
