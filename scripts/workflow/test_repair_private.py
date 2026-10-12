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
from unittest.mock import Mock, patch

from agent_policy import session_fingerprint, validate_review
from agent_worker import main as bind_manual_report, private_repair_evidence, run_worker
from handoff_registry import issue_review_origin, register, restore_review
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
        self.accepted['repair']['writer_session'] = self.sessions[0]
        self.observed['observer'] = self.sessions[1]
        with self.assertRaisesRegex(ValueError, 'unambiguous'): self.load()

    def test_manual_adapter_binds_rehashed_bundle_and_refuses_missing_input(self):
        source, target, accepted = (self.root / name for name in ('report.json', 'bound.json', 'accepted.json'))
        report = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic evidence, not approval')
        source.write_text(json.dumps(report))
        original = source.read_bytes()
        accepted.write_text(json.dumps(self.accepted))
        binding = self.root / 'binding.json'; binding.write_text(json.dumps(self.bound))
        args = ['agent_worker.py', '--report', str(source), '--output', str(target)]
        with patch.dict(os.environ, {'CODEX_THREAD_ID': report['session']}):
            with patch.object(sys, 'argv', args), self.assertRaises(ValueError): bind_manual_report()
            self.assertFalse(target.exists())
            with patch.object(sys, 'argv', args + ['--acceptance', str(accepted), '--repair-evidence', str(self.path),
                                                     '--storage', str(self.root), '--binding', str(binding)]):
                bind_manual_report()
        bound_report = json.loads(target.read_text())
        self.assertEqual(bound_report['repair_evidence_sha256'], self.load()['bundle_sha256'])
        validate_review(bound_report, self.bound, dict(self.accepted,
                        repair_observers=list(map(session_fingerprint, self.sessions)),
                        repair_evidence_sha256=self.load()['bundle_sha256']), self.root)
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

    def test_public_fingerprint_without_private_reference_cannot_resume_ordinary_approval(self):
        state = {'phase': 'reviewed', 'binding': {}, 'review': self.loop.public_state(tal.report())}
        self.gh.comment(36, tal.al.pack(tal.al.STATE, state))
        with self.assertRaisesRegex(ValueError, 'bound private review'): self.loop.load(36)
        with self.assertRaises(ValueError): self.loop.tick(36)
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)
        self.assertEqual(self.gh.merges, 0)
        self.loop.cleanup.assert_not_called()


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
        def fixture_adapter(*args, **kw):
            raw = self.worker(*args, **kw)
            return issue_review_origin(self.loop.storage, raw, kw['origin_binding'], raw['session'], 'cli',
                                       args[2]['private_repair_evidence']['private_paths'])
        self.loop.worker = fixture_adapter

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

    def test_external_recovery_cannot_claim_an_arbitrary_origin(self):
        self.prepare(); self.loop.tick(36)
        self.gh.pull.update(merged=True, state='closed')
        bound, _ = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))
        raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='handwritten assertion',
                   repair_evidence_sha256=self.accepted['repair_evidence_sha256'], feedback_hash=bound['feedback_hash'])
        with self.assertRaisesRegex(ValueError, 'private review reference'):
            self.loop.reconcile_feedback(self.gh.pull, {'binding': bound, 'review': raw})
        self.loop.cleanup.assert_not_called()
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)


class OriginTests(unittest.TestCase):
    setUp = EvidenceTests.setUp
    seal = EvidenceTests.seal
    load = EvidenceTests.load
    def test_complete_report_binding_and_adapter_kind_are_required(self):
        from handoff_registry import store_review
        accepted = dict(self.accepted, repair_observers=list(map(session_fingerprint, self.sessions)),
                        repair_evidence_sha256=self.load()['bundle_sha256'])
        raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic only',
                   repair_evidence_sha256=accepted['repair_evidence_sha256'])
        issued = issue_review_origin(self.root, raw, self.bound, raw['session'], 'manual')
        validate_review(issued, self.bound, accepted, self.root)
        for field, value in (('evidence', 'rewritten'), ('repair_assessment', 'different'),
                             ('session', '00000000-0000-4000-8000-000000000088')):
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate_review(dict(issued, **{field: value}), self.bound, accepted, self.root)
        with self.assertRaises(ValueError): validate_review(raw, self.bound, accepted, self.root)
        with self.assertRaises(ValueError): validate_review(issued, dict(self.bound, issue_hash='changed'), accepted, self.root)
        cache_ref = store_review(self.root, {'binding': self.bound, 'review': raw}, raw)
        with self.assertRaisesRegex(ValueError, 'not issued'):
            validate_review(dict(raw, origin_receipt=cache_ref), self.bound, accepted, self.root)
        path = self.root / 'registry' / (issued['origin_receipt'] + '.json')
        original = path.read_bytes()
        path.chmod(0o644)
        with self.assertRaises(ValueError): validate_review(issued, self.bound, accepted, self.root)
        path.chmod(0o600); path.write_text('{}')
        with self.assertRaises(ValueError): validate_review(issued, self.bound, accepted, self.root)
        path.write_bytes(original); path.unlink()
        with self.assertRaises(OSError): validate_review(issued, self.bound, accepted, self.root)

    def test_cli_adapter_issues_receipt_from_actual_event_envelope(self):
        packet = dict(self.bound, private_repair_evidence=self.load())
        process = Mock(pid=123, returncode=0)
        def start(command, **kw):
            raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic fixture')
            raw.pop('session'); raw.pop('repair_evidence_sha256')
            Path(command[command.index('-o') + 1]).write_text(json.dumps(raw))
            kw['stdout'].write(json.dumps({'type': 'thread.started', 'thread_id': tal.report()['session']}) + '\n')
            kw['stdout'].write(json.dumps({'type': 'turn.completed'}) + '\n'); kw['stdout'].flush()
            return process
        with patch('agent_worker.subprocess.Popen', side_effect=start), patch('agent_worker.os.killpg'):
            issued = run_worker('review', self.root, packet, self.root / 'out',
                                origin_storage=self.root, origin_binding=self.bound)
        validate_review(issued, self.bound, dict(self.accepted,
                        repair_observers=list(map(session_fingerprint, self.sessions)),
                        repair_evidence_sha256=self.load()['bundle_sha256']), self.root)
        with patch('agent_worker.subprocess.Popen') as launch, self.assertRaises(ValueError):
            run_worker('review', self.root, packet, self.root / 'unused')
        launch.assert_not_called()

    def test_unambiguous_legacy_mapping_supported_and_other_origin_rejected(self):
        self.observed.pop('attribution')
        self.accepted['repair']['writer_session'] = self.sessions[0]
        self.observed['observer'] = self.sessions[0]
        for origin in self.bundle['origins']: origin['session'] = self.sessions[0]
        self.seal(); self.load()
        structured = copy.deepcopy(self.observed)
        for item in self.items: item['session'] = session_fingerprint(self.sessions[0])
        structured['attribution'] = {'writer': self.items[0]['session'], 'observer': self.items[0]['session'],
                                     'contributors': self.items}
        self.accepted['repair']['results']['STRUCTURED'] = {'observation': structured}
        self.load()
        self.accepted['repair']['results'].pop('STRUCTURED')
        self.bundle['origins'][1]['session'] = self.sessions[1]; self.seal()
        with self.assertRaisesRegex(ValueError, 'Legacy'): self.load()
        self.bundle['origins'][1]['session'] = self.sessions[0]
        self.bundle['origins'][1]['activity'] = 'assessment'; self.seal()
        with self.assertRaisesRegex(ValueError, 'Legacy'): self.load()


class LegacyRecoveryTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def test_merged_wrapper_cache_keeps_pause_before_any_finish_transition(self):
        self.gh.pull.update(merged=True, state='closed')
        state = {'phase': 'blocked', 'paused': True, 'review': dict(tal.report(), session='review-' + tal.report()['session']),
                 'binding': {}, 'acceptance': {'allowed': True}}
        self.gh.comment(36, tal.al.pack(tal.al.STATE, state))
        before = copy.deepcopy(self.gh.messages)
        with patch.object(self.loop, 'finish') as finish:
            self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
        finish.assert_not_called(); self.worker.assert_not_called(); self.loop.cleanup.assert_not_called()
        self.assertEqual(self.gh.messages, before)
        self.assertTrue(self.loop.load(36)[1]['paused'])

    def test_wrapper_invalidates_approval_then_reviews_or_preserves_pause(self):
        for paused in (False, True):
            with self.subTest(paused=paused):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                state = {'phase': 'blocked' if paused else 'reviewed', 'paused': paused,
                         'binding': bound, 'review': dict(tal.report(), session='review-' + tal.report()['session']),
                         'acceptance': {'allowed': True}, 'gui': {'old': True}}
                self.gh.comment(36, tal.al.pack(tal.al.STATE, state))
                h, loaded, sid, _ = self.loop.load(36)
                self.assertNotIn('review', loaded); self.assertNotIn('binding', loaded)
                self.assertNotIn('acceptance', loaded); self.assertNotIn('gui', loaded)
                self.assertEqual(loaded['paused'], paused)
                self.loop.save(self.gh.pull, loaded, sid)
                self.worker.reset_mock()
                result = self.loop.tick(36)
                if paused:
                    self.assertEqual(result['phase'], 'blocked'); self.worker.assert_not_called()
                    self.assertEqual(self.loop.load(36)[1]['phase'], 'blocked')
                else:
                    self.assertEqual(result['phase'], 'reviewed'); self.worker.assert_called_once()
                self.assertEqual(self.loop.load(36)[0], h)
                self.assertEqual(self.gh.merges, 0)
                self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)


class EnrollmentTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def prepare(self):
        from types import SimpleNamespace
        fixture = EvidenceTests(); fixture.setUp(); self.addCleanup(fixture.doCleanups)
        self.fixture = fixture
        self.gh.messages.clear()
        previous = tal.al.git.side_effect
        def git(*args, **kw):
            if args == ('branch', '--show-current'): return self.gh.pull['head']['ref']
            if args == ('worktree', 'list', '--porcelain'): return f'worktree {self.checkout.resolve()}\n'
            return previous(*args, **kw)
        tal.al.git.side_effect = git
        self.loop.acceptance.return_value = dict(fixture.accepted, allowed=True)
        return SimpleNamespace(pr=36, source=self.checkout, owner='fixture', scope=['src/'], parent=None,
                               close_issue=False, writer_stopped=True, repair_evidence=Path('bundle.json'))

    def test_relative_bundle_is_preflighted_and_stored_as_private_absolute_path(self):
        args = self.prepare()
        with patch('os.getcwd', return_value=str(self.fixture.root)):
            public = tal.al.enroll(self.loop, args)
        h = self.loop.load(36)[0]
        self.assertEqual(h['repair_evidence'], str(self.fixture.path.resolve()))
        self.assertNotIn(str(self.fixture.path), json.dumps(self.gh.messages))
        self.assertNotIn('repair_evidence', public)

    def test_invalid_bundle_never_creates_public_handoff_or_registry(self):
        args = self.prepare()
        original = copy.deepcopy(self.fixture.bundle)
        for mode in ('missing', 'permissions', 'candidate', 'origin', 'symlink', 'absent'):
            self.fixture.bundle = copy.deepcopy(original); self.fixture.seal()
            args.repair_evidence = Path('bundle.json')
            if mode == 'missing': args.repair_evidence = Path('missing.json')
            elif mode == 'permissions': self.fixture.path.chmod(0o644)
            elif mode == 'candidate': self.fixture.bundle['head'] = tal.NEW; self.fixture.seal()
            elif mode == 'origin':
                self.fixture.bundle['origins'][0]['session'] = tal.report()['session']; self.fixture.seal()
            elif mode == 'symlink':
                link = self.fixture.root / 'link.json'; link.symlink_to(self.fixture.path); args.repair_evidence = Path('link.json')
            else: args.repair_evidence = None
            with self.subTest(mode=mode), patch('os.getcwd', return_value=str(self.fixture.root)), \
                    self.assertRaises((ValueError, OSError)):
                tal.al.enroll(self.loop, args)
            self.assertFalse(self.gh.messages)
            self.assertFalse(self.gh.statuses)
            self.assertFalse((self.loop.storage / 'registry').exists())


class PrivatePathTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp
    prepare = ConsumerTests.prepare

    def test_bundle_paths_are_private_through_review_save_and_cached_resume(self):
        self.check_paths('/mnt/c3/private-receipt.json', '/opt/c3/private-mapping.json')

    def test_uuid_paths_redact_before_identity_through_save_and_cache(self):
        session = '00000000-0000-4000-8000-000000000099'
        self.check_paths(f'/mnt/receipts/{session}/event.json', f'/opt/receipts/{session}/mapping.json')

    def check_paths(self, virtual, support):
        self.prepare()
        first = self.fixture.receipt.read_bytes(); second = b'{"support":true}'
        self.fixture.bundle['receipts'][0]['path'] = virtual
        self.fixture.bundle['receipts'].append({'path': support, 'sha256': hashlib.sha256(second).hexdigest()})
        self.fixture.seal()
        originals = {virtual: first, support: second}
        original_stat, original_read, original_file = Path.stat, Path.read_bytes, Path.is_file
        info = self.fixture.receipt.stat()
        def stat(path, *args, **kw):
            return info if str(path) in originals else original_stat(path, *args, **kw)
        def read(path):
            return originals[str(path)] if str(path) in originals else original_read(path)
        urls = 'https://public.example/tmp/guide https://public.example/opt/docs'
        session = '00000000-0000-4000-8000-000000000098'
        self.worker.side_effect = lambda *args, **kw: dict(tal.report('changes_requested'),
                evidence=f'prefix:{virtual}:suffix file://{support} {urls} standalone:{session}',
                findings=[f'Check [{virtual}]({support})'], repair_revision='e' * 64,
                repair_evidence_sha256=args[2]['private_repair_evidence']['bundle_sha256'])
        def is_file(path):
            return True if str(path) in originals else original_file(path)
        with patch.object(Path, 'stat', stat), patch.object(Path, 'read_bytes', read), patch.object(Path, 'is_file', is_file):
            self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        _, state, sid, _ = self.loop.load(36)
        self.assertIn(virtual, state['review']['evidence'])
        self.assertIn(virtual, state['private_paths']); self.assertIn(support, state['private_paths'])
        self.loop.save(self.gh.pull, state, sid)
        public = json.dumps(self.gh.messages)
        self.assertNotIn(virtual, public); self.assertNotIn(support, public)
        self.assertNotIn('private_paths', public)
        self.assertNotIn('/mnt/receipts/', public); self.assertNotIn('/opt/receipts/', public)
        self.assertNotIn(session, public)
        self.assertIn('standalone:' + session_fingerprint(session), public)
        self.assertIn(urls, public)
        self.assertIn('[private path]', public)


class LegacyBundleTests(unittest.TestCase):
    setUp = EvidenceTests.setUp
    seal = EvidenceTests.seal
    load = EvidenceTests.load

    def legacy(self):
        self.observed.pop('attribution')
        self.accepted['repair']['writer_session'] = self.sessions[0]
        self.observed['observer'] = self.sessions[0]
        for origin in self.bundle['origins']: origin['session'] = self.sessions[0]
        self.seal(); self.load()

    def test_structured_other_actor_cannot_bypass_bundle_wide_legacy_identity(self):
        self.legacy()
        structured = copy.deepcopy(self.observed)
        for item in self.items: item['session'] = session_fingerprint(self.sessions[0])
        structured['attribution'] = {'writer': self.items[0]['session'], 'observer': self.items[2]['session'],
                                     'contributors': self.items}
        self.accepted['repair']['results']['STRUCTURED'] = {'observation': structured}
        self.seal(); self.load()
        self.items[1]['session'] = session_fingerprint(self.sessions[1])
        self.bundle['origins'][1]['session'] = self.sessions[1]
        self.seal()
        with self.assertRaisesRegex(ValueError, 'Legacy'): self.load()

    def test_legacy_uses_canonical_pointer_and_same_overlap_contract(self):
        self.legacy()
        body = json.dumps({'event': {'child': {}}, 'event/child': {}, 'sibling': {}}).encode()
        self.receipt.write_bytes(body); sealed = hashlib.sha256(body).hexdigest()
        self.bundle['receipts'][0]['sha256'] = self.bundle['mapping_receipt_sha256'] = sealed
        for origin in self.bundle['origins']: origin['receipt_sha256'] = sealed
        for pointers in (('', '/event', '/sibling'), ('/event', '/event/child', '/sibling'),
                         ('/event', '/event', '/sibling'), ('/event', '/sibling', '/bad~2')):
            for origin, pointer in zip(self.bundle['origins'], pointers): origin['receipt_locator'] = pointer
            self.seal()
            with self.subTest(pointers=pointers), self.assertRaises(ValueError): self.load()
        # Escaped slash selects a distinct literal key, not a parent/child alias.
        for origin, pointer in zip(self.bundle['origins'], ('/event', '/event~1child', '/sibling')):
            origin['receipt_locator'] = pointer
        self.seal(); self.load()


class RepairRecoveryTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def prepare(self, version, paused):
        from types import SimpleNamespace
        h = self.loop.load(36)[0]
        fixture = EvidenceTests(); fixture.setUp(); self.addCleanup(fixture.doCleanups)
        self.fixture = fixture
        self.loop.acceptance.return_value = dict(fixture.accepted, allowed=True)
        if version == 2:
            public = {k: v for k, v in h.items() if k not in ('source', 'host', 'repair_evidence', 'private_paths')}
            h2 = register(self.loop.storage, public, h['source'], tal.al.HOST)
            self.gh.comment(36, tal.al.pack(tal.al.HANDOFF_V2, h2))
        state = {'phase': 'blocked' if paused else 'reviewed', 'paused': paused, 'expected_head': tal.HEAD,
                 'binding': {}, 'review': tal.report(), 'acceptance': {'old': True}, 'gui': {'old': True}}
        self.loop.save(self.gh.pull, state, None)
        return SimpleNamespace(pr=36, source=None, migration_record=None, writer_stopped=True,
                               repair_evidence=fixture.path)

    def test_v1_v2_add_replace_bundle_keep_owner_and_pause_invalidate_approval(self):
        for version in (1, 2):
            for paused in (False, True):
                self.doCleanups(); self.setUp()
                with self.subTest(version=version, paused=paused):
                    args = self.prepare(version, paused)
                    before = self.loop.load(36)[0]
                    tal.al.rebind_target(self.loop, args)
                    h, state, _, _ = self.loop.load(36)
                    self.assertEqual(h['repair_evidence'], str(self.fixture.path.resolve()))
                    for key in ('branch', 'issue', 'head', 'scope', 'source', 'previous_owner'):
                        self.assertEqual(h.get(key), before.get(key))
                    self.assertEqual(state['paused'], paused)
                    self.assertEqual(state['phase'], 'blocked' if paused else 'queued')
                    for key in ('review', 'binding', 'gui', 'acceptance'): self.assertNotIn(key, state)
                    self.assertNotIn(str(self.fixture.path), json.dumps(self.gh.messages))
                    replacement = self.fixture.root / 'replacement.json'
                    replacement.write_bytes(self.fixture.path.read_bytes()); replacement.chmod(0o600)
                    args.repair_evidence = replacement
                    tal.al.rebind_target(self.loop, args)
                    self.assertEqual(self.loop.load(36)[0]['repair_evidence'], str(replacement.resolve()))
                    self.assertEqual(self.loop.load(36)[1]['paused'], paused)

    def test_recovery_preflight_failure_preserves_handoff_state_and_private_registry(self):
        args = self.prepare(2, True)
        messages = copy.deepcopy(self.gh.messages)
        registry = self.loop.storage / 'registry'
        original = {p.name: p.read_bytes() for p in registry.iterdir()}
        self.fixture.bundle['head'] = tal.NEW; self.fixture.seal()
        with self.assertRaises(ValueError): tal.al.rebind_target(self.loop, args)
        self.assertEqual(self.gh.messages, messages)
        self.assertEqual({p.name: p.read_bytes() for p in registry.iterdir()}, original)
        self.assertTrue(self.loop.load(36)[1]['paused'])
