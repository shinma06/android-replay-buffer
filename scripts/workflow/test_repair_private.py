"""Private originating receipts and public resume state; no real worker or approval."""
import copy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from agent_policy import session_fingerprint, validate_review
from agent_worker import main as bind_manual_report, private_repair_evidence
from handoff_registry import register, restore_review
import test_agent_loop as tal
from verification import repair_contributors


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sessions = [f'00000000-0000-4000-8000-{n:012d}' for n in (1, 2, 3)]
        self.at = '2026-01-01T00:00:00Z'
        self.receipt = self.root / 'receipt.json'
        self.receipt.write_text(json.dumps({'events': [{'kind': k} for k in ('writer', 'execution', 'composition')]}))
        self.sha = hashlib.sha256(self.receipt.read_bytes()).hexdigest()
        self.items = [{'role': activity, 'session': session_fingerprint(session), 'activity': activity,
                       'at': self.at, 'receipt_sha256': self.sha, 'receipt_locator': f'/events/{i}'}
                      for i, (session, activity) in enumerate(zip(self.sessions, ('writer', 'execution', 'composition')))]
        self.observed = {'at': self.at, 'attribution': {'writer': self.items[0]['session'],
                         'observer': self.items[2]['session'], 'contributors': self.items}}
        self.bound = {'head': tal.HEAD, 'base': tal.BASE}
        self.accepted = {'repair_revision': 'e' * 64, 'repair': {'results': {'GAP': {'observation': self.observed}}}}
        self.bundle = dict(self.bound, repair_revision='e' * 64, mapping_receipt_sha256=self.sha,
                           receipts=[{'sha256': self.sha, 'path': str(self.receipt)}],
                           origins=[dict(receipt_sha256=self.sha, receipt_locator=item['receipt_locator'],
                                         session=session, activity=item['activity'], at=self.at)
                                    for session, item in zip(self.sessions, self.items)])
        self.path = self.root / 'bundle.json'
        self.seal()

    def seal(self):
        self.path.write_text(json.dumps(self.bundle))
        self.path.chmod(0o600)

    def load(self):
        return private_repair_evidence(self.path, self.accepted, self.bound)

    def test_multi_event_receipt_and_repeated_consistent_observation(self):
        claims = {}
        earliest = datetime(2026, 1, 1, tzinfo=timezone.utc)
        for _ in range(2):
            self.assertEqual(repair_contributors(self.observed, '', earliest, claims),
                             set(map(session_fingerprint, self.sessions)))
        packet = self.load()
        self.assertEqual(packet['origins'], self.bundle['origins'])
        self.assertEqual(packet['bundle_sha256'], hashlib.sha256(self.path.read_bytes()).hexdigest())

    def test_conflicting_same_observation_is_rejected_across_results(self):
        earliest = datetime(2026, 1, 1, tzinfo=timezone.utc)
        for field, value in (('session', session_fingerprint(tal.report()['session'])),
                             ('activity', 'assessment'), ('at', '2025-12-31T23:59:59Z')):
            claims = {}
            repair_contributors(self.observed, '', earliest, claims)
            other = copy.deepcopy(self.observed)
            other['attribution']['contributors'][1][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'Conflicting'):
                repair_contributors(other, '', earliest, claims)
        same_file = copy.deepcopy(self.observed)
        for item in same_file['attribution']['contributors']: item.pop('receipt_locator')
        with self.assertRaisesRegex(ValueError, 'Conflicting'):
            repair_contributors(same_file, '', earliest)
        same_file = copy.deepcopy(self.observed)
        same_file['attribution']['contributors'][0]['receipt_locator'] = ''
        with self.assertRaisesRegex(ValueError, 'Conflicting'):
            repair_contributors(same_file, '', earliest)

    def test_missing_stale_tampered_incomplete_and_false_locator_fail_closed(self):
        original = copy.deepcopy(self.bundle)
        for mutation in ({'head': tal.NEW}, {'base': tal.NEW}, {'repair_revision': 'f' * 64},
                         {'origins': self.bundle['origins'][:-1]}, {'receipts': []}):
            self.bundle = dict(original, **mutation); self.seal()
            with self.subTest(mutation=mutation), self.assertRaises(ValueError): self.load()
        self.bundle = copy.deepcopy(original)
        self.bundle['origins'][1]['session'] = tal.report()['session']; self.seal()
        with self.assertRaises(ValueError): self.load()
        self.bundle = copy.deepcopy(original)
        self.bundle['origins'][1]['receipt_locator'] = '/events/99'; self.seal()
        with self.assertRaises((ValueError, IndexError)): self.load()
        self.bundle = original; self.seal()
        self.receipt.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'changed'): self.load()
        with self.assertRaises(ValueError): private_repair_evidence(None, self.accepted, self.bound)

    def test_permissions_and_legacy_ambiguous_mapping_are_blocked(self):
        self.path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'private'): self.load()
        self.path.chmod(0o600)
        self.observed.pop('attribution')
        with self.assertRaisesRegex(ValueError, 'explicit'): self.load()

    def test_manual_adapter_binds_rehashed_bundle_and_refuses_missing_input(self):
        source, target, accepted = (self.root / name for name in ('report.json', 'bound.json', 'accepted.json'))
        report = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic evidence, not approval')
        source.write_text(json.dumps(report))
        original = source.read_bytes()
        accepted.write_text(json.dumps(self.accepted))
        args = ['agent_worker.py', '--report', str(source), '--output', str(target)]
        with patch.dict(os.environ, {'CODEX_THREAD_ID': report['session']}):
            with patch.object(sys, 'argv', args), self.assertRaises(ValueError): bind_manual_report()
            self.assertFalse(target.exists())
            with patch.object(sys, 'argv', args + ['--acceptance', str(accepted), '--repair-evidence', str(self.path)]):
                bind_manual_report()
        self.assertEqual(json.loads(target.read_text())['repair_evidence_sha256'], self.load()['bundle_sha256'])
        self.assertEqual(source.read_bytes(), original)
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)


class PrivacyTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def test_private_session_restores_only_from_bound_private_record(self):
        raw = tal.report()['session']
        state = {'phase': 'reviewed', 'review': dict(tal.report(), evidence='origin prefix' + raw + 'suffix'),
                 'binding': self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]}
        sid = self.loop.save(self.gh.pull, state, None)
        public = self.loop.public_state(state)
        self.assertNotIn(raw, json.dumps(self.gh.comments(36)))
        self.assertNotIn(raw, json.dumps(self.gh.comments(35)))
        self.assertEqual(public['review']['session'], session_fingerprint(raw))
        restored = self.loop.load(36)[1]
        self.assertEqual(restored['review']['session'], raw)
        self.assertEqual(restored['review']['evidence'], 'origin prefix' + raw + 'suffix')
        ref = state['review_ref']
        path = self.loop.storage / 'registry' / (ref + '.json')
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        altered = {'binding': dict(public['binding'], head=tal.NEW), 'review': public['review']}
        with self.assertRaisesRegex(ValueError, 'binding'): restore_review(self.loop.storage, ref, altered)
        path.unlink()
        with self.assertRaises(OSError): self.loop.load(36)
        with self.assertRaises(ValueError):
            validate_review(public['review'], public['binding'], {'repair_revision': 'e' * 64})

    def test_public_fingerprint_cannot_be_sealed_as_a_new_origin(self):
        state = {'phase': 'reviewed', 'binding': {}, 'review': self.loop.public_state(tal.report())}
        with self.assertRaises(ValueError): self.loop.save(self.gh.pull, state, None)
        self.assertFalse(any(comment['body'].startswith(tal.al.STATE) for comment in self.gh.comments(36)))


class ConsumerTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def prepare(self):
        fixture = EvidenceTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.fixture = fixture
        self.gh.issues[35].update(title='[追跡] fixture', labels=['type:tracking', 'priority:P2', 'status:review'])
        self.gh.pull['base']['ref'] = 'develop'
        self.gh.pull['body'] = self.gh.pull['body'].replace('tooling', 'develop')
        h = self.loop.load(36)[0]
        public = {k: v for k, v in h.items() if k not in ('source', 'host')}
        public['target'] = 'develop'
        public = register(self.loop.storage, public, h['source'], tal.al.HOST, fixture.path)
        self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF_V2, public)
        self.assertNotIn(str(fixture.path), self.gh.messages[36][0]['body'])
        self.assertEqual(self.loop.load(36)[0]['repair_evidence'], str(fixture.path))
        self.accepted = dict(fixture.accepted, allowed=True, mode='develop', cases=0,
                             repair_observers=list(map(session_fingerprint, fixture.sessions)))
        self.loop.acceptance.return_value = self.accepted
        self.worker.side_effect = lambda role, path, packet, output, **kw: dict(
            tal.report(), repair_revision='e' * 64, repair_assessment='synthetic original bodies checked',
            repair_evidence_sha256=packet['private_repair_evidence']['bundle_sha256'])

    def test_exact_private_packet_then_tampered_receipt_blocks_cached_merge(self):
        self.prepare()
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        packet = self.worker.call_args.args[2]['private_repair_evidence']
        self.assertEqual(packet['receipts'], self.fixture.bundle['receipts'])
        self.assertEqual(packet['origins'], self.fixture.bundle['origins'])
        self.fixture.receipt.write_text('{}')
        self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)
        self.assertEqual(self.gh.merges, 0)
        self.loop.cleanup.assert_not_called()

    def test_missing_bundle_never_starts_reviewer(self):
        self.prepare()
        self.fixture.path.unlink()
        self.loop.tick(36)
        self.worker.assert_not_called()
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)

    def test_postmerge_rehash_blocks_closure_cleanup(self):
        self.prepare()
        self.loop.tick(36)
        h, state, sid, _ = self.loop.load(36)
        self.gh.pull.update(merged=True, state='closed')
        self.fixture.receipt.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'changed'):
            self.loop.finish(self.gh.pull, h, state, sid)
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.loop.cleanup.assert_not_called()
