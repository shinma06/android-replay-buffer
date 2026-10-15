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

from agent_policy import canonical_session, native_session, session_fingerprint, validate_review
from agent_worker import bind_native_review, main as bind_manual_report, private_repair_evidence, run_worker
from handoff_registry import digest, issue_review_origin, register, restore_review, verify_review_origin
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

    def test_bundle_hash_and_parse_use_checked_descriptor_after_replacement(self):
        original_read, original_fstat = Path.read_bytes, os.fstat
        with tempfile.TemporaryDirectory(dir='/tmp') as folder:
            parent = Path(folder); parent.chmod(0o777)
            self.path = parent / 'bundle.json'
            for symlink in (False, True):
                if self.path.is_symlink(): self.path.unlink()
                self.seal()
                body = self.path.read_bytes()
                replacement = parent / 'replacement.json'
                replacement.write_text(json.dumps(self.bundle, indent=2)); replacement.chmod(0o644)
                changed = False
                def replace():
                    nonlocal changed
                    if changed: return
                    changed = True
                    if symlink:
                        self.path.unlink(); self.path.symlink_to(replacement)
                    else:
                        replacement.replace(self.path)
                def read(path):
                    if path == self.path: replace()
                    return original_read(path)
                def fstat(fd):
                    info = original_fstat(fd)
                    replace()
                    return info
                with self.subTest(symlink=symlink), patch.object(Path, 'read_bytes', read), patch.object(os, 'fstat', fstat):
                    packet = self.load()
                    self.assertEqual(packet['bundle_sha256'], hashlib.sha256(body).hexdigest())
                    self.assertEqual(packet['origins'], self.bundle['origins'])
                self.assertTrue(changed)
                with self.assertRaises(ValueError): self.load()
            parent.chmod(0o700)

    def test_bundle_keeps_strict_leaf_privacy_under_private_ancestor(self):
        from agent_worker import read_repair_bundle
        self.root.chmod(0o700)
        for mode in (0o600, 0o601, 0o610, 0o602, 0o620, 0o644):
            self.path.chmod(mode)
            with self.subTest(mode=oct(mode)):
                if mode == 0o600:
                    self.assertEqual(read_repair_bundle(self.path)[1], self.bundle)
                else:
                    with self.assertRaisesRegex(ValueError, 'private and locally owned'):
                        read_repair_bundle(self.path)
        self.path.chmod(0o600)

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

    def test_native_origin_is_private_in_saved_state_text_and_encoded_links(self):
        from urllib.parse import quote
        identity = 'Case-Sensitive/Session-99'
        source = 'native://example.invalid/operation-19'
        raw = tal.report(); raw.pop('session')
        bound = {'head': tal.HEAD, 'base': tal.BASE}
        observed = {'session_id': identity, 'source': source, 'operation_id': 'operation-19',
                    'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
        issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid/host-1',
                                    observe_origin=Mock(return_value=observed))
        # Preserve the original sealed report; exercise text projection separately.
        for text in (identity, issued['session'], source, '[origin](https://example.invalid/' + quote(identity, safe='') + ')',
                     '[origin](https://example.invalid/Case&#45;Sensitive/Session-99)',
                     '[origin](https://example.invalid/Case\\-Sensitive/Session-99)'):
            public = self.loop.public_state(dict(issued, evidence=text), private_identifiers=['operation-19'], private_locators=[source])
            serialized = json.dumps(public)
            self.assertNotIn(identity, serialized)
            self.assertNotIn(issued['session'], serialized)
            self.assertNotIn(source, serialized)
            self.assertNotIn('Session-99', serialized)
        external = 'https://example.invalid/ordinary-public-record'
        self.assertEqual(self.loop.public_state(dict(issued, evidence=external), private_identifiers=['operation-19'], private_locators=[source])['evidence'], external)
        state = {'phase': 'reviewed', 'binding': bound, 'review': issued}
        self.loop.save(self.gh.pull, state, None)
        restored = self.loop.load(36)[1]['review']
        self.assertEqual(restored, issued)
        verify_review_origin(self.loop.storage, restored, bound)
        self.gh.comment(36, tal.al.pack(tal.al.STATE, {'review': {'session': session_fingerprint(issued['session'])}}))
        with self.assertRaisesRegex(ValueError, 'private review'): self.loop.load(36)

    def test_short_native_ids_preserve_binding_and_machine_metadata_through_save_load(self):
        self.gh.pull['head']['sha'] = '19' * 20
        self.loop.github_reviewer = 'reviewer-1'
        for identity, operation in [('Session-9', '19'), ('1', 'operation-1'), ('e', 'e'),
                                    ('session', 'evidence'), ('review', 'binding'), ('head', 'base'),
                                    (tal.report()['session'], tal.report()['session'])]:
            with self.subTest(identity=identity, operation=operation):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = tal.report(head=bound['head']); raw.pop('session')
                if identity in ('session', 'review', 'head'):
                    raw['evidence'] = {identity: 'first diagnostic', operation: 'second diagnostic'}
                observed = {'session_id': identity, 'source': 'native://host/operation', 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                state = {'phase': 'reviewed', 'binding': bound, 'review': issued, 'expected_head': bound['head']}
                sid = self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                self.assertEqual(public['binding'], bound)
                self.assertEqual(public['review']['head'], bound['head'])
                self.assertEqual(public['review']['origin_receipt'], issued['origin_receipt'])
                self.assertEqual(public['review']['repair_evidence_sha256'], raw['repair_evidence_sha256'])
                self.assertEqual(public['expected_head'], bound['head'])
                self.assertEqual(set(public['review']), set(issued) - {'private_identifiers'})
                self.assertEqual(public['review']['session'], session_fingerprint(issued['session']))
                if isinstance(raw['evidence'], dict):
                    self.assertEqual(public['review']['evidence'], {
                        session_fingerprint(issued['session']): 'first diagnostic',
                        '[private origin]': 'second diagnostic'})
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['binding'], bound)
                self.assertEqual(restored['review'], issued)
                validate_review(restored['review'], bound)
                self.assertEqual(self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0], bound)
                self.loop.save(self.gh.pull, restored, sid)
                verify_review_origin(self.loop.storage, self.loop.load(36)[1]['review'], bound)

    def test_extra_and_nested_report_fields_never_inherit_machine_metadata_exemption(self):
        identity, source, operation = 'Case-Sensitive/Session-99', 'native://example.invalid/operation-19', 'operation-19'
        for extra in ({'diagnostic_revision': identity}, {'status': source}, {'evidence': {'status': operation}},
                      {'binding': {'head': identity, 'review_completion': {'method': source}}},
                      {'evidence': {'session': 'native-session-v1:00:00', 'origin_receipt': source,
                                    'review': {'session': 'native-session-v1:00:00'},
                                    'private_sessions': ['native-session-v1:00:00']}},
                      {'verdict_sha256': operation}):
            with self.subTest(extra=extra):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = dict(tal.report(), **extra); raw.pop('session')
                observed = {'session_id': identity, 'source': source, 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                state = {'phase': 'reviewed', 'binding': bound, 'review': issued}
                self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                serialized = json.dumps(public)
                for private in (identity, source, operation, issued['session']):
                    self.assertNotIn(private, serialized)
                self.assertEqual(public['binding'], bound)
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                verify_review_origin(self.loop.storage, restored['review'], bound)
        for field in ('head', 'base', 'repair_revision', 'repair_evidence_sha256', 'origin_receipt', 'verdict'):
            public = self.loop.public_state(dict(issued, **{field: identity}))
            self.assertNotIn(identity, json.dumps(public))

    def test_native_diagnostic_keys_are_private_through_issuance_save_and_load(self):
        from urllib.parse import quote
        identity, source, operation = 'Case-Sensitive/Session-99', 'native://example.invalid/operation-19', 'operation-19'
        for extra in ({'evidence': {operation: 'checked', 'ordinary': 'also checked'}}, {'evidence': [{source: 'checked'}]},
                      {identity: 'checked'}, {'evidence': {'[origin](https://example.invalid/' +
                          quote(identity, safe='') + ')': 'checked'}},
                      {'evidence': {'[origin](https://example.invalid/Case&#45;Sensitive/Session-99)': 'checked'}}):
            with self.subTest(extra=extra):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = dict(tal.report(), **extra); raw.pop('session')
                original = copy.deepcopy(raw)
                observed = {'session_id': identity, 'source': source, 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                validate_review(issued, bound)
                receipt = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                receipt_bytes = receipt.read_bytes()
                self.assertEqual(json.loads(receipt_bytes)['review']['native_origin']['submitted_report'], original)
                state = {'phase': 'reviewed', 'binding': bound, 'review': issued}
                self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                serialized = json.dumps(public)
                for private in (identity, source, operation, issued['session'], quote(identity, safe=''), 'Session-99'):
                    self.assertNotIn(private, serialized)
                if isinstance(extra.get('evidence'), dict):
                    self.assertEqual(len(public['review']['evidence']), len(extra['evidence']))
                    self.assertEqual(list(public['review']['evidence'].values()), list(extra['evidence'].values()))
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                self.assertEqual(raw, original)
                self.assertEqual(receipt.read_bytes(), receipt_bytes)
                validate_review(restored['review'], restored['binding'])
                verify_review_origin(self.loop.storage, restored['review'], bound)

    def test_submitted_private_sessions_cannot_exempt_private_operation_on_roundtrip(self):
        from agent_worker import bind_report_session
        operation = 'a' * 64
        claimed = 'native-session-sha256:' + operation
        for metadata in (None, [claimed], [native_session('example.invalid', 'unverified contributor')]):
            with self.subTest(metadata=metadata):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = dict(tal.report(), evidence='Operation: ' + claimed); raw.pop('session')
                if metadata is not None:
                    raw['private_sessions'] = metadata
                original = copy.deepcopy(raw)
                observed = {'session_id': 'Actual-Session-99', 'source': 'native://host/private-record',
                            'operation_id': operation, 'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                actual = session_fingerprint(issued['session'])
                self.assertNotEqual(actual, claimed)
                self.assertNotIn('private_sessions', issued)
                validate_review(issued, bound)
                receipt = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                receipt_bytes = receipt.read_bytes()
                self.assertEqual(json.loads(receipt_bytes)['review']['native_origin']['submitted_report'], original)
                state = {'phase': 'reviewed', 'binding': bound, 'review': issued}
                sid = self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                self.assertNotIn(operation, public['review']['evidence'])
                self.assertEqual(public['review']['session'], actual)
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                validate_review(restored['review'], bound)
                verify_review_origin(self.loop.storage, restored['review'], bound)
                self.loop.save(self.gh.pull, restored, sid)
                self.assertEqual(self.loop.load(36)[1]['review'], issued)
                self.assertEqual(receipt.read_bytes(), receipt_bytes)
                self.assertEqual(raw, original)
                # Legacy file/CLI intake also cannot turn extra fields into provenance.
                legacy = bind_report_session(raw, tal.report()['session'])
                self.assertNotIn('private_sessions', legacy)
                if metadata is not None:
                    submitted = dict(raw, session=tal.report()['session'])
                    manual = issue_review_origin(self.loop.storage, submitted, bound, submitted['session'], 'manual')
                    self.assertNotIn('private_sessions', manual)
                    record = self.loop.storage / 'registry' / (manual['origin_receipt'] + '.json')
                    self.assertEqual(json.loads(record.read_bytes())['review']['submitted_report'], submitted)
                    verify_review_origin(self.loop.storage, manual, bound)

    def test_verified_contributors_and_legacy_bundle_survive_public_resume(self):
        from handoff_registry import store_review
        fixture = EvidenceTests(); fixture.setUp(); self.addCleanup(fixture.doCleanups)
        contributor = native_session('example.invalid', 'session')
        fixture.sessions[1] = contributor
        fixture.bundle['origins'][1]['session'] = contributor
        fixture.items[1]['session'] = session_fingerprint(contributor)
        fixture.seal()
        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
        raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='Synthetic original evidence assessment',
                   evidence=' '.join(fixture.sessions)); raw.pop('session')
        def observe(submitted, binding):
            return {'session_id': 'Actual-Reviewer', 'source': 'native://host/review', 'operation_id': '1',
                    'report_sha256': digest(submitted), 'binding_sha256': digest(binding)}
        issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid', observe_origin=observe,
                                    acceptance=fixture.accepted, repair_evidence=fixture.path)
        accepted = dict(fixture.accepted, repair_observers=list(map(session_fingerprint, fixture.sessions)),
                        repair_evidence_sha256=issued['repair_evidence_sha256'])
        original_path = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
        original_bytes = original_path.read_bytes()
        # Construct a separate old-format fixture; never rewrite the sealed current receipt.
        legacy = json.loads(original_bytes)['review']; legacy.pop('contributor_sessions', None)
        unbound = {k: v for k, v in issued.items() if k != 'origin_receipt'}
        legacy_ref = store_review(self.loop.storage, {'binding': bound, 'review': unbound}, legacy,
                                  verify_review_origin(self.loop.storage, issued, bound))
        for report in (issued, dict(unbound, origin_receipt=legacy_ref)):
            with self.subTest(receipt=report['origin_receipt']):
                path = self.loop.storage / 'registry' / (report['origin_receipt'] + '.json')
                before = path.read_bytes()
                state = {'phase': 'reviewed', 'binding': bound, 'review': report, 'acceptance': accepted}
                self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                for session in fixture.sessions:
                    self.assertIn(session_fingerprint(session), public['review']['evidence'])
                self.assertEqual(public['review']['session'], session_fingerprint(issued['session']))
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], report)
                validate_review(restored['review'], bound, restored['acceptance'], self.loop.storage)
                self.assertEqual(path.read_bytes(), before)
        self.assertEqual(original_path.read_bytes(), original_bytes)
        claimed = dict(unbound, private_sessions=['native-session-sha256:' + 'a' * 64])
        unsupported = dict(legacy, report=claimed)
        ref = store_review(self.loop.storage, {'binding': bound, 'review': claimed}, unsupported,
                           verify_review_origin(self.loop.storage, issued, bound))
        before = copy.deepcopy(self.gh.messages)
        with self.assertRaisesRegex(ValueError, 'adapter-verified identities'):
            self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound,
                           'review': dict(claimed, origin_receipt=ref)}, None)
        self.assertEqual(self.gh.messages, before)

    def test_colliding_private_diagnostic_keys_fail_before_public_write(self):
        identity, source, operation = 'NativeReviewSession', 'native://host/review', 'operation-19'
        session = native_session('example.invalid', identity)
        for extra in ({'evidence': {source: 'first', source + '/child': 'second'}},
                      {operation: 'first', '[private origin]': 'second'},
                      {'evidence': {identity: 'first', session_fingerprint(session): 'second'}}):
            with self.subTest(extra=extra):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = dict(tal.report(), **extra); raw.pop('session')
                observed = {'session_id': identity, 'source': source, 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                validate_review(issued, bound)
                receipt = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                receipt_bytes = receipt.read_bytes()
                comments = copy.deepcopy(self.gh.messages)
                with self.assertRaisesRegex(ValueError, 'diagnostic key collision'):
                    self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
                self.assertEqual(self.gh.messages, comments)
                self.assertEqual(receipt.read_bytes(), receipt_bytes)
                verify_review_origin(self.loop.storage, issued, bound)

    def test_non_string_diagnostic_keys_fail_before_public_write(self):
        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
        raw = dict(tal.report(), evidence={19: 'unsupported Python mapping key'})
        comments = copy.deepcopy(self.gh.messages)
        with self.assertRaisesRegex(ValueError, 'diagnostic keys must be strings'):
            self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': raw}, None)
        self.assertEqual(self.gh.messages, comments)

    def test_opaque_native_ids_preserve_unrelated_numbers_and_keys_on_roundtrip(self):
        for operation in ('1', 'e', 'review', session_fingerprint(native_session('example.invalid', 'Actual-Reviewer'))):
            with self.subTest(operation=operation):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                text = 'Checked line 123, version 1.23, issue-123 and reviewer entries.'
                raw = dict(tal.report('changes_requested'), evidence=text, findings=[text], origin_reference=operation,
                           diagnostics={'line 123': 'checked', 'line [private origin]23': 'checked',
                                        'entry': 'checked', '[private origin]ntry': 'checked'})
                raw.pop('session')
                observed = {'session_id': 'Actual-Reviewer', 'source': 'native://host/receipt', 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                original = record.read_bytes()
                state = {'phase': 'reviewed', 'binding': bound, 'review': issued}
                sid = self.loop.save(self.gh.pull, state, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                self.assertEqual(public['review']['evidence'], text)
                self.assertEqual(public['review']['findings'], [text])
                self.assertEqual(public['review']['origin_reference'], '[private origin]')
                self.assertEqual(public['review']['diagnostics'], raw['diagnostics'])
                self.assertEqual(public['review']['session'], session_fingerprint(issued['session']))
                self.assertEqual(public['binding'], bound)
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                validate_review(restored['review'], bound)
                self.loop.save(self.gh.pull, restored, sid)
                self.assertEqual(record.read_bytes(), original)

    def test_opaque_id_boundaries_keep_complete_private_locators_secret(self):
        from urllib.parse import quote
        for identity, operation in (('Actual-Reviewer', '1'), ('e', 'operation-19')):
            with self.subTest(identity=identity, operation=operation):
                source = 'https://host.invalid/private/receipt-1'
                public_url = 'https://public.invalid/123/checked'
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                raw = dict(tal.report(), evidence={
                    'numbers': 'line 123; version 1.23; issue-123; ' + public_url,
                    'identifiers': f'Operation: ({operation}), {operation}. Native: [{identity}].',
                    'links': f'[operation](https://host.invalid/{quote(operation, safe="")}) '
                             f'[encoded](https://host.invalid/%{ord(operation[0]):02X}{operation[1:]}) '
                             f'[session](https://host.invalid/{quote(identity, safe="")}) '
                             f'[native](native://host/%{ord(operation[0]):02X}{operation[1:]})',
                    'locators': f'prefix{source}suffix {source}/child [source]({source.replace("private", "%70rivate")})',
                    'keys': {operation: 'checked', 'ordinary': 'retained'}})
                raw.pop('session')
                observed = {'session_id': identity, 'source': source, 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']['evidence']
                self.assertEqual(public['numbers'], raw['evidence']['numbers'])
                self.assertNotIn(f'({operation})', public['identifiers'])
                self.assertNotIn(f'{operation}.', public['identifiers'])
                self.assertNotIn(f'[{identity}]', public['identifiers'])
                self.assertNotIn(source, json.dumps(public))
                self.assertNotIn('%70rivate', json.dumps(public))
                self.assertNotIn('https://host.invalid/', public['links'])
                self.assertNotIn('native://host/', public['links'])
                self.assertEqual(public['keys'], {'[private origin]': 'checked', 'ordinary': 'retained'})
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                verify_review_origin(self.loop.storage, restored['review'], bound)

    def test_native_operation_roles_preserve_public_uris_and_secret_locators(self):
        from urllib.parse import quote
        for operation in ('/', 'operations/review-1', 'native://operation/1', 'native', 'https', '443', '1', 'e'):
            with self.subTest(operation=operation):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                source = 'https://host.invalid/private/receipt'
                text = 'critical finding remains: https://public.invalid/a/b '
                text += 'https://public.invalid:443/ xoperations/review-1suffix native://operation/123 and line 123 '
                text += 'reference=https://public.invalid/guide ref=native://public/guide'
                text += ' https://user:password@public.invalid:443/guide native://user:password@public.invalid:443/guide'
                raw = dict(tal.report(), evidence={
                    'ordinary': text, 'private': f'Operation [{operation}]',
                    'nested': {'private_paths': ['critical'], 'private_identifiers': ['finding'], 'text': text},
                    'link': 'https://ref.invalid/id/' + quote(operation, safe=''),
                    'locator': f'prefix{source}suffix {source}/child'},
                    private_identifiers=['finding'], private_paths=['critical'])
                raw.pop('session')
                observed = {'session_id': 'Actual-Reviewer', 'source': source, 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                original = record.read_bytes()
                self.assertEqual(json.loads(original)['review']['native_origin']['submitted_report'], raw)
                self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']['evidence']
                self.assertEqual(public['ordinary'], text)
                self.assertEqual(public['nested']['text'], text)
                self.assertNotIn(f'[{operation}]', public['private'])
                self.assertNotIn('https://ref.invalid/', public['link'])
                self.assertNotIn(source, public['locator'])
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                validate_review(restored['review'], bound)
                self.assertEqual(record.read_bytes(), original)

    def test_native_uri_authority_components_hide_exact_private_ids(self):
        encode = lambda value: ''.join(f'%{byte:02X}' for byte in value.encode())
        for operation in ('operation-19', encode('operation-19'), 'Case-Sensitive-ID', '2001:DB8::1'):
            identity = 'Case-Sensitive/Session-99'
            secret = {}
            for form, value in (('raw', operation), ('encoded', encode(operation))):
                if form == 'raw' and '%' in operation:
                    continue
                host = '[' + value + ']' if operation == '2001:DB8::1' and form == 'raw' else value
                secret.update({form + '-username': f'https://{value}@public.example/guide',
                               form + '-password': f'https://user:{value}@public.example/guide',
                               form + '-host': f'https://{host}/guide',
                               form + '-host-port': f'https://{host}:443/guide'})
            if operation == '2001:DB8::1':
                del secret['raw-username']
            secret['session-userinfo'] = f'https://{encode(identity)}@public.example/guide'
            controls = {'public': 'https://user:password@public.example:443/guide',
                        'native': 'native://user:password@public.example:443/guide',
                        'longer-user': f'https://{encode(operation + "suffix")}@public.example/guide'}
            if operation == 'Case-Sensitive-ID':
                controls['different-case'] = 'https://case-sensitive-id:443/guide'
            evidence = {key: f'[record]({target})' for key, target in dict(secret, **controls).items()}
            bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
            raw = dict(tal.report(), evidence=evidence); raw.pop('session')
            observed = {'session_id': identity, 'source': 'native://private.invalid/unique-record',
                        'operation_id': operation, 'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
            issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                        observe_origin=Mock(return_value=observed))
            record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
            original = record.read_bytes()
            sid = self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
            public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']['evidence']
            for key in secret:
                with self.subTest(operation=operation, component=key):
                    self.assertEqual(public[key], '[record]([private URI])')
            for key in controls:
                with self.subTest(operation=operation, control=key):
                    self.assertEqual(public[key], evidence[key])
            restored = self.loop.load(36)[1]
            self.assertEqual(restored['review'], issued)
            verify_review_origin(self.loop.storage, restored['review'], bound)
            self.loop.save(self.gh.pull, restored, sid)
            self.assertEqual(record.read_bytes(), original)

    def test_native_bare_ipv6_uri_keeps_authority_port_and_path_together(self):
        secret = 'https://op@[2001:db8::1]:443/vault/receipt'
        ordinary = 'https://user@[2001:db8::1]:443/public/guide'
        forms = ('Evidence: {} tail', 'target={}', '<{}>', '[record]({})',
                 '[record](<{}> "title")', '[record]: {} "title"\nnext',
                 '> [record]: <{}> "title"\nnext')
        secrets = {'raw': secret, 'private-prefix': secret.replace('/vault/', '/private/'),
                   'encoded': secret.replace('://op@', '://%6F%70@'),
                   'native': secret.replace('https://', 'native://')}
        evidence = {f'{kind}-{index}': form.format(target)
                    for kind, target in dict(secrets, public=ordinary).items()
                    for index, form in enumerate(forms)}
        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
        raw = dict(tal.report(), evidence=evidence); raw.pop('session')
        observed = {'session_id': 'Actual-Reviewer', 'source': secret,
                    'operation_id': 'op', 'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
        issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                    observe_origin=Mock(return_value=observed))
        record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
        original = record.read_bytes()
        sid = self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
        public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']['evidence']
        for index, form in enumerate(forms):
            for kind in secrets:
                with self.subTest(kind=kind, form=form):
                    self.assertEqual(public[f'{kind}-{index}'], form.format('[private URI]'))
            self.assertEqual(public[f'public-{index}'], form.format(ordinary))
        restored = self.loop.load(36)[1]
        self.assertEqual(restored['review'], issued)
        verify_review_origin(self.loop.storage, restored['review'], bound)
        self.loop.save(self.gh.pull, restored, sid)
        self.assertEqual(record.read_bytes(), original)

    def test_punctuation_origin_ids_are_private_at_sentence_end(self):
        for kind in ('operation', 'session'):
            for identity, terminator in ((identity, end) for identity in ('/', ':', '@', '/@', '/:@')
                                        for end in ('.', '。', '、', '！', '？', '—', '“', '”', '（', '）')):
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                text = f'Operation {identity}{terminator} next'
                ordinary = 'https://user:password@public.example:443/a/b and path/part name@example.com'
                ordinary += ' ' + ' '.join(f'Operation {identity}{suffix} next'
                                           for suffix in ('€', '∑', '🙂', '´', '@', '＿suffix', '‿suffix', '-suffix', '_suffix'))
                raw = dict(tal.report('changes_requested'), findings=[text],
                           evidence={'sentence': text, 'ordinary': ordinary, 'keys': {text: 'retained diagnostic'},
                                     'encoded': f'Operation {identity}&#x3001; next'})
                raw.pop('session')
                observed = {'session_id': identity if kind == 'session' else 'Actual-Reviewer',
                            'source': 'native://private.invalid/unique-record',
                            'operation_id': identity if kind == 'operation' else 'operation-punctuation',
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
                original = record.read_bytes()
                sid = self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']
                replacement = session_fingerprint(issued['session']) if kind == 'session' else '[private origin]'
                expected = f'Operation {replacement}{terminator} next'
                with self.subTest(kind=kind, identity=identity, terminator=terminator):
                    self.assertEqual(public['findings'], [expected])
                    self.assertEqual(public['evidence']['sentence'], expected)
                    self.assertEqual(public['evidence']['keys'], {expected: 'retained diagnostic'})
                    self.assertEqual(public['evidence']['ordinary'], ordinary)
                    self.assertEqual(public['evidence']['encoded'], f'Operation {replacement}&#x3001; next')
                restored = self.loop.load(36)[1]
                self.assertEqual(restored['review'], issued)
                verify_review_origin(self.loop.storage, restored['review'], bound)
                self.loop.save(self.gh.pull, restored, sid)
                self.assertEqual(record.read_bytes(), original)

    def test_private_uri_locator_normalizes_only_scheme_and_host(self):
        from urllib.parse import quote
        cases = (
            ('https://host.invalid/vault/receipt',
             ('HTTPS://host.invalid/vault/receipt', 'https://HOST.INVALID/vault/receipt',
              'HTTPS://%48%4F%53%54.INVALID/vault/receipt', 'HTTPS://HOST.INVALID/%76ault/receipt'),
             ('https://host.invalid/Vault/receipt', 'https://host.invalid.evil/vault/receipt')),
            ('https://User:Pass@host.invalid:443/Vault/Receipt?Key=Value#Frag',
             ('HTTPS://User:Pass@HOST.INVALID:443/Vault/Receipt?Key=Value#Frag',),
             ('https://user:Pass@host.invalid:443/Vault/Receipt?Key=Value#Frag',
              'https://User:pass@host.invalid:443/Vault/Receipt?Key=Value#Frag',
              'https://User:Pass@host.invalid:443/vault/Receipt?Key=Value#Frag',
              'https://User:Pass@host.invalid:443/Vault/Receipt?key=Value#Frag',
              'https://User:Pass@host.invalid:443/Vault/Receipt?Key=value#Frag',
              'https://User:Pass@host.invalid:443/Vault/Receipt?Key=Value#frag',
              'https://User%3APass@host.invalid:443/Vault/Receipt?Key=Value#Frag',
              'https://User:Pass@host.invalid:443/Vault/Receipt?Key=Value%23Frag',
              'https://User:Pass@host.invalid:443/Vault/Receipt%3FKey=Value%23Frag')),
            ('native://User:Pass@[2001:DB8::1]:443/Vault/Receipt',
             ('NATIVE://User:Pass@[2001:db8::1]:443/Vault/Receipt',),
             ('native://user:Pass@[2001:DB8::1]:443/Vault/Receipt',
              'native://User:Pass@[2001:DB8::1]:443/vault/Receipt')))
        for source, aliases, controls in cases:
            evidence = {'public-' + str(i): target for i, target in enumerate(controls)}
            evidence['opaque-case'] = 'https://case-operation:443/public/session-case'
            evidence['opaque-longer'] = 'https://public.invalid/id/Case-Operation-extra/Session-Case-extra'
            for i, alias in enumerate((source, *aliases)):
                encoded_key = f'public-encoded-{i}' if '%' in alias else f'encoded-{i}'
                evidence.update({f'raw-{i}': alias, f'link-{i}': f'[record]({alias})',
                                 f'child-{i}': alias + '/child', f'embedded-{i}': f'prefix{alias}suffix',
                                 encoded_key: 'https://public.invalid/lookup?source=' + quote(alias, safe=''),
                                 f'public-double-{i}': 'https://public.invalid/lookup?source=' + quote(quote(alias, safe=''), safe='')})
            bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
            raw = dict(tal.report(), evidence=evidence); raw.pop('session')
            observed = {'session_id': 'Session-Case', 'source': source, 'operation_id': 'Case-Operation',
                        'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
            issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                        observe_origin=Mock(return_value=observed))
            record = self.loop.storage / 'registry' / (issued['origin_receipt'] + '.json')
            original = record.read_bytes()
            sid = self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
            public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]['review']['evidence']
            for key, target in evidence.items():
                with self.subTest(source=source, field=key):
                    expected = target if key.startswith(('public-', 'opaque-')) else (
                        '[record]([private URI])' if key.startswith('link-') else '[private URI]')
                    self.assertEqual(public[key], expected)
            restored = self.loop.load(36)[1]
            self.assertEqual(restored['review'], issued)
            verify_review_origin(self.loop.storage, restored['review'], bound)
            self.loop.save(self.gh.pull, restored, sid)
            self.assertEqual(record.read_bytes(), original)

    def test_unverified_privacy_controls_cannot_hide_diagnostics(self):
        from agent_worker import bind_report_session
        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
        for field in ('private_identifiers', 'private_paths'):
            with self.subTest(field=field):
                raw = dict(tal.report('changes_requested'), findings=['critical finding remains'],
                           **{field: ['finding']})
                before = copy.deepcopy(raw)
                comments = copy.deepcopy(self.gh.messages)
                with self.assertRaises(ValueError):
                    self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': raw}, None)
                self.assertEqual(self.gh.messages, comments)
                self.assertEqual(raw, before)
                normalized = bind_report_session(raw, raw['session'])
                self.assertNotIn(field, normalized)
                self.assertEqual(normalized['findings'], raw['findings'])
                issued = issue_review_origin(self.loop.storage, raw, bound, raw['session'], 'manual')
                self.loop.save(self.gh.pull, {'phase': 'reviewed', 'binding': bound, 'review': issued}, None)
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                self.assertEqual(public['review']['findings'], ['critical finding remains'])
                self.assertEqual(self.loop.load(36)[1]['review'], issued)
        public = self.loop.public_state({'private_identifiers': ['critical'], 'private_paths': ['finding'],
                                         'next': 'critical finding remains'})
        self.assertEqual(public['next'], 'critical finding remains')

    def test_native_machine_bindings_survive_close_interruption_and_resume(self):
        for field in ('head', 'base', 'issue_hash', 'feedback_hash', 'merge_sha'):
            self.doCleanups(); self.setUp()
            with self.subTest(field=field):
                self.gh.pull.update(merged=True, state='closed')
                h, _, _, _ = self.loop.load(36)
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                operation = self.gh.pull['merge_commit_sha'] if field == 'merge_sha' else bound[field]
                raw = dict(tal.report(), feedback_hash=bound['feedback_hash']); raw.pop('session')
                observed = {'session_id': 'Actual-Reviewer', 'source': 'native://host/receipt', 'operation_id': operation,
                            'report_sha256': digest(raw), 'binding_sha256': digest(bound)}
                issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                            observe_origin=Mock(return_value=observed))
                state = {'phase': 'cleanup', 'binding': bound, 'review': issued, 'expected_head': tal.HEAD,
                         'acceptance': {'allowed': True, 'mode': 'tooling'}}
                sid = self.loop.save(self.gh.pull, state, None)
                api = self.gh.api
                def interrupted(path, method='GET', data=None):
                    result = api(path, method, data)
                    if method == 'PATCH' and path.endswith('/issues/35') and (data or {}).get('state') == 'closed':
                        raise RuntimeError('fixture interruption after actual fake Issue close')
                    return result
                with patch.object(self.gh, 'api', side_effect=interrupted), self.assertRaisesRegex(RuntimeError, 'fixture interruption'):
                    self.loop.finish(self.gh.pull, h, state, sid)
                self.loop.cleanup.assert_not_called()
                _, restored, sid, _ = self.loop.load(36)
                self.assertEqual(restored['issue_closure'], {k: bound[k] for k in ('head', 'base', 'issue_hash')})
                public = tal.al.unpack(self.gh.comments(36), tal.al.STATE)[0]
                self.assertEqual(public['review']['feedback_hash'], bound['feedback_hash'])
                self.assertEqual(self.loop.finish(self.gh.pull, h, restored, sid)['phase'], 'done')
                final = self.loop.load(36)[1]
                self.assertEqual(final['merge_sha'], self.gh.pull['merge_commit_sha'])
                self.assertEqual(final['review'], issued)
                self.loop.cleanup.assert_called_once()

    def test_short_native_operation_preserves_certificate_reviewer_on_resume(self):
        fixture = EvidenceTests(); fixture.setUp(); self.addCleanup(fixture.doCleanups)
        fixture.bundle['certificate_revision'] = fixture.bundle.pop('repair_revision')
        fixture.seal()
        accepted = {'allowed': True, 'mode': 'tooling', 'head': tal.HEAD, 'base': tal.BASE,
                    'certificate_revision': 'e' * 64,
                    'certificate_provenance': {'sample': fixture.observed},
                    'certificate_observers': list(map(session_fingerprint, fixture.sessions))}
        raw = dict(tal.report(), certificate_revision='e' * 64,
                   certificate_assessment='Synthetic independent original evidence assessment')
        raw.pop('session')
        def observe(submitted, bound):
            return {'session_id': 'NativeReviewSession', 'source': 'native://host/review', 'operation_id': '1',
                    'report_sha256': digest(submitted), 'binding_sha256': digest(bound)}
        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
        issued = bind_native_review(raw, bound, self.loop.storage, issuer='example.invalid',
                                    observe_origin=observe, acceptance=accepted, repair_evidence=fixture.path)
        accepted.update(certificate_reviewers=[session_fingerprint(issued['session'])],
                        certificate_evidence_sha256=issued['certificate_evidence_sha256'])
        self.assertIn('1', accepted['certificate_reviewers'][0])
        validate_review(issued, bound, accepted, self.loop.storage)
        state = {'phase': 'reviewed', 'binding': bound, 'review': issued, 'acceptance': accepted}
        sid = self.loop.save(self.gh.pull, state, None)
        restored = self.loop.load(36)[1]
        self.assertEqual(restored['review'], issued)
        for key in ('mode', 'head', 'base', 'certificate_revision', 'certificate_evidence_sha256',
                    'certificate_observers', 'certificate_reviewers'):
            self.assertEqual(restored['acceptance'][key], accepted[key], key)
        validate_review(restored['review'], restored['binding'], restored['acceptance'], self.loop.storage)
        self.loop.save(self.gh.pull, restored, sid)
        restored = self.loop.load(36)[1]
        validate_review(restored['review'], restored['binding'], restored['acceptance'], self.loop.storage)
        invalid = copy.deepcopy(state)
        invalid['review']['evidence'] = {'certificate_reviewers': ['1', 'native://host/review']}
        public = self.loop.public_state(invalid, private_identifiers=['1'], private_locators=['native://host/review'])
        self.assertEqual(public['review']['evidence']['certificate_reviewers'], ['[private origin]', '[private URI]'])

    def test_malformed_native_text_is_redacted_but_actual_session_stays_strict(self):
        for malformed in ('native-session-v1:00:00', 'native-session-v1:61:1'):
            with self.subTest(malformed=malformed):
                raw = dict(tal.report(), evidence='Rejected ' + malformed, findings=[malformed])
                bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                state = {'phase': 'reviewed', 'binding': bound, 'review': raw}
                self.loop.save(self.gh.pull, state, None)
                self.assertEqual(self.loop.load(36)[1]['review'], raw)
                public = self.loop.public_state(raw)
                self.assertEqual(public['evidence'], 'Rejected [private origin]')
                self.assertEqual(public['findings'], ['[private origin]'])
                with self.assertRaises(ValueError):
                    self.loop.save(self.gh.pull, dict(state, review=dict(raw, session=malformed)), None)

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
        packet = fixture.load()
        public = register(self.loop.storage, public, h['source'], tal.al.HOST, fixture.path,
                          packet['private_paths'], packet['evidence_records'])
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
            raw = dict(self.worker(*args, **kw), evidence_records=args[2]['private_repair_evidence']['evidence_records'])
            return issue_review_origin(self.loop.storage, raw, kw['origin_binding'], raw['session'], 'cli',
                                       args[2]['private_repair_evidence']['private_paths'],
                                       [origin['session'] for origin in args[2]['private_repair_evidence']['origins']])
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

    def test_native_host_acquisition_binds_report_and_mixed_legacy_contributors(self):
        self.sessions[1] = native_session('example.invalid/host-1', 'Actual-Executor/Session-2')
        self.bundle['origins'][1]['session'] = self.sessions[1]
        self.items[1]['session'] = session_fingerprint(self.sessions[1])
        self.seal()
        raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic native source')
        raw.pop('session')
        packet = self.load()
        submitted = dict(raw, repair_evidence_sha256=packet['bundle_sha256'], evidence_records=packet['evidence_records'])
        source = {'session_id': 'Actual-Host/Session-99', 'source': 'native://host/execution/operation-19',
                  'operation_id': 'operation-19', 'report_sha256': digest(submitted), 'binding_sha256': digest(self.bound)}
        host = Mock(return_value=copy.deepcopy(source))
        issued = bind_native_review(raw, self.bound, self.root, issuer='example.invalid/host-1',
                                    observe_origin=host, acceptance=self.accepted, repair_evidence=self.path)
        host.assert_called_once_with(submitted, self.bound)
        accepted = dict(self.accepted, repair_observers=list(map(session_fingerprint, self.sessions)),
                        repair_evidence_sha256=packet['bundle_sha256'])
        validate_review(issued, self.bound, accepted, self.root)
        self.assertEqual(issued['session'], native_session('example.invalid/host-1', source['session_id']))
        receipt = json.loads((self.root / 'registry' / (issued['origin_receipt'] + '.json')).read_text())
        self.assertEqual(receipt['review']['native_origin']['observation'], source)
        with self.assertRaisesRegex(ValueError, 'self-reviewed'):
            validate_review(issued, self.bound, dict(accepted, repair_observers=[session_fingerprint(issued['session'])]), self.root)
        for mutated in (dict(issued, evidence='rewritten'), dict(issued, origin_receipt='0' * 32)):
            with self.assertRaises((ValueError, OSError)): validate_review(mutated, self.bound, accepted, self.root)
        with self.assertRaises(ValueError): validate_review(issued, dict(self.bound, feedback_hash='changed'), accepted, self.root)
        for invalid in (None, {}, dict(source, report_sha256='0' * 64), dict(source, binding_sha256='0' * 64)):
            with self.assertRaises(ValueError):
                bind_native_review(raw, self.bound, self.root, issuer='example.invalid/host-1',
                                   observe_origin=Mock(return_value=invalid), acceptance=self.accepted, repair_evidence=self.path)
        claimed = dict(submitted, session=native_session('example.invalid/host-1', 'Different-Session'))
        with self.assertRaisesRegex(ValueError, 'differs'):
            bind_native_review(claimed, self.bound, self.root, issuer='example.invalid/host-1',
                               observe_origin=Mock(return_value=dict(source, report_sha256=digest(claimed))),
                               acceptance=self.accepted, repair_evidence=self.path)
        self.receipt.write_text('changed original')
        with self.assertRaises(ValueError):
            bind_native_review(raw, self.bound, self.root, issuer='example.invalid/host-1',
                               observe_origin=host, acceptance=self.accepted, repair_evidence=self.path)

    def test_native_identity_preserves_case_and_legacy_uuid_without_aliases(self):
        legacy = self.sessions[0]
        expected = 'codex-session-sha256:' + hashlib.sha256(('codex-session-v1:' + legacy).encode()).hexdigest()
        self.assertEqual(session_fingerprint(legacy), expected)
        self.assertEqual(native_session('example.invalid', legacy), legacy)
        self.assertEqual(session_fingerprint(native_session('example.invalid', legacy)), expected)
        identity = native_session('example.invalid', 'Case-Sensitive-ID')
        self.assertEqual(canonical_session(identity), identity)
        self.assertNotEqual(session_fingerprint(identity), session_fingerprint(native_session('example.invalid', 'case-sensitive-id')))
        alias = f'native-session-v1:{"example.invalid".encode().hex()}:{legacy.encode().hex()}'
        for invalid in (alias, identity.upper(), 'native-session-v1:zz:00', session_fingerprint(identity)):
            with self.assertRaises(ValueError): canonical_session(invalid)
        for issuer, raw in [('codex', legacy), ('role label', 'some-id'), ('example.invalid', ''),
                            ('example.invalid', legacy.replace('-', '')), ('example.invalid', 'bad\nID')]:
            with self.assertRaises(ValueError): native_session(issuer, raw)
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

    def test_encoded_file_uris_with_space_unicode_and_escape_case_stay_private(self):
        session = '00000000-0000-4000-8000-000000000099'
        self.check_paths(f'/mnt/private receipt/{session}/記録.json', f'/opt/private receipt/{session}/対応.json')

    def test_parenthesized_known_uri_through_save_cache_and_public_control(self):
        self.check_paths('/mnt/private receipt/event(1).json', '/opt/private receipt/mapping(1).json')

    def test_encoded_open_filename_normalized_markdown_through_save_and_cache(self):
        self.check_paths('/mnt/private receipt/event(1.json', '/opt/private receipt/mapping(1.json')

    def test_markdown_destination_escapes_references_and_decode_order(self):
        known = '/mnt/receipts/event(1).json'
        targets = (r'file:///mnt/receipts/event\(1\).json',
                   r'file:///mnt/receipts/event(1)\.json',
                   'file:///mnt/receipts/event&#40;1&#41;.json',
                   'file:///mnt/receipts/event&#x28;1&#x29;.json',
                   'file:///mnt/receipts/event&lpar;1&rpar;.json',
                   'file:///mnt/receipts/event&#37;281&#37;29.json')
        for target in targets:
            with self.subTest(target=target):
                self.assertEqual(self.loop.public_state(f'[receipt]({target})', [known]), '[receipt]([private URI])')
        bracket = r'[receipt](file:///mnt/receipts/event\].json)'
        self.assertEqual(self.loop.public_state(bracket, ['/mnt/receipts/event].json']), '[receipt]([private URI])')
        bracket_dot = r'[receipt](file:///mnt/receipts/event]\.json)'
        self.assertEqual(self.loop.public_state(bracket_dot, ['/mnt/receipts/event].json']), '[receipt]([private URI])')
        for target in (r'file:///mnt/receipts/event\&lpar;1\&rpar;.json',
                       'file:///mnt/receipts/event&amp;#40;1&amp;#41;.json',
                       'file:///mnt/receipts/event%5C(1).json',
                       'file:///mnt/receipts/event&#92;(1).json',
                       'file:///mnt/receipts/event%25281%2529.json'):
            source = f'[receipt]({target})'
            self.assertEqual(self.loop.public_state(source, [known]), source)
        for source, other in ((r'[receipt](file:///mnt/receipts/event\z.json)', '/mnt/receipts/eventz.json'),
                              ('[receipt](file:///mnt/receipts/event&notit;.json)', '/mnt/receipts/event¬it;.json')):
            self.assertEqual(self.loop.public_state(source, [other]), source)
        public = r'[public](HTTPS://public.example/event\(1\).json?x=%41&amp;y=2)'
        self.assertEqual(self.loop.public_state(public, [known]), public)
        label = f'[{targets[0]}](HTTPS://public.example/target?x=%41&amp;y=2)'
        self.assertEqual(self.loop.public_state(label, [known]), '[[private URI]](HTTPS://public.example/target?x=%41&amp;y=2)')
        session = tal.report()['session']
        target = 'https://public.example/session/' + session.replace('-', r'\-')
        projected = self.loop.public_state({'review': {'session': session, 'evidence': f'[origin]({target})'}})
        self.assertNotIn(target, projected['review']['evidence'])

    def test_complete_markdown_destination_before_uri_prefix_recognition(self):
        known = '/mnt/receipts/event.json'
        targets = ('&#47;mnt&#47;receipts&#47;event.json',
                   'file:&#47;&#47;&#47;mnt&#47;receipts&#47;event.json',
                   r'file:\/\/\/mnt\/receipts\/event.json',
                   '&#102;ile&#58;&#47;&#47;&#47;mnt&#47;receipts&#47;event.json',
                   '&#102;ile&colon;&sol;&sol;&sol;mnt&sol;receipts&sol;event.json',
                   '&sol;mnt&sol;receipts&sol;event.json',
                   '&#x2f;mnt&#x2f;receipts&#x2f;event.json')
        for target in targets:
            for source, expected in ((f'[receipt]({target})', '[receipt]([private URI])'),
                                     (f'[receipt](<{target}> "title")', '[receipt](<[private URI]> "title")'),
                                     (f'[receipt]: {target} "title"', '[receipt]: [private URI] "title"')):
                with self.subTest(source=source):
                    self.assertEqual(self.loop.public_state(source, [known]), expected)
        for target in ('&amp;#47;mnt&amp;#47;receipts&amp;#47;event.json',
                       r'\&sol;mnt\&sol;receipts\&sol;event.json',
                       '&notit;mnt&sol;receipts&sol;event.json',
                       'file&#92;:&#47;&#47;&#47;mnt&#47;receipts&#47;event.json',
                       r'file\z:&#47;&#47;&#47;mnt&#47;receipts&#47;event.json',
                       '%66ile:%2F%2F%2Fmnt%2Freceipts%2Fevent.json'):
            source = f'[literal]({target})'
            self.assertEqual(self.loop.public_state(source, [known]), source)
        public = r'[public](&#72;TTPS\:\/\/public.example/event.json?x=%41&amp;y=2)'
        self.assertEqual(self.loop.public_state(public, [known]), public)
        session = tal.report()['session']
        target = '&#104;ttps&#58;&#47;&#47;public.example/session/' + session.replace('-', r'\-')
        projected = self.loop.public_state({'review': {'session': session, 'evidence': f'[origin]({target})'}})
        self.assertEqual(projected['review']['evidence'], '[origin]([private URI])')
        target = '&#104;ttps&colon;&sol;&sol;public.example&sol;session&sol;' + session.replace('-', '&#45;')
        projected = self.loop.public_state({'private_sessions': [session_fingerprint(session)],
                                           'review': {'session': session, 'evidence': f'[origin]({target})'}},
                                           private_sessions=[session_fingerprint(session)])
        self.assertEqual(projected['review']['evidence'], '[origin]([private URI])')
        self.assertEqual(self.loop.public_state('prefix:file:///mnt/receipts/event.json:suffix', [known]),
                         'prefix:file://[private path]:suffix')
        self.assertEqual(self.loop.public_state('[receipt](&sol;mnt&sol;receipts&sol;event.json)tail', [known]),
                         '[receipt]([private URI])tail')
        self.assertEqual(self.loop.public_state('[receipt](&sol;mnt&sol;receipts&sol;event.json "title")[public](https://public.example/next)', [known]),
                         '[receipt]([private URI] "title")[public](https://public.example/next)')
        self.assertEqual(self.loop.public_state('[receipt](<file:&sol;&sol;&sol;mnt&sol;private receipt&sol;event.json>)tail',
                                               ['/mnt/private receipt/event.json']),
                         '[receipt](<[private URI]>)tail')
        self.assertEqual(self.loop.public_state('[receipt](&sol;mnt&sol;receipts&sol;event(1).json)tail',
                                               ['/mnt/receipts/event(1).json']),
                         '[receipt]([private URI])tail')
        for target, path in (('&sol;mnt&sol;receipts&sol;event&rpar;.json', '/mnt/receipts/event).json'),
                             (r'&sol;mnt&sol;receipts&sol;event\).json', '/mnt/receipts/event).json'),
                             ('&sol;mnt&sol;receipts&sol;event>.json', '/mnt/receipts/event>.json'),
                             ('&sol;mnt&sol;receipts&sol;event\u00a0name.json', '/mnt/receipts/event\u00a0name.json'),
                             ('&sol;mnt&sol;receipts&sol;event(outer(inner)).json', '/mnt/receipts/event(outer(inner)).json')):
            self.assertEqual(self.loop.public_state(f'[receipt]({target})tail', [path]),
                             '[receipt]([private URI])tail')
        self.assertEqual(self.loop.public_state('[receipt](<&sol;mnt&sol;receipts&sol;event&gt;.json>)tail',
                                               ['/mnt/receipts/event>.json']),
                         '[receipt](<[private URI]>)tail')
        self.assertEqual(self.loop.public_state('[receipt]: &sol;mnt&sol;receipts&sol;event.json "title"\nnext', [known]),
                         '[receipt]: [private URI] "title"\nnext')
        self.assertEqual(self.loop.public_state('> [receipt]: &sol;mnt&sol;receipts&sol;event.json "title"', [known]),
                         '> [receipt]: [private URI] "title"')

    def test_markdown_destination_forms_through_save_cache_and_resave(self):
        self.check_paths('/mnt/receipts/event(1).json', '/opt/receipts/mapping(1).json')
        self.doCleanups(); self.setUp()
        self.check_paths('/mnt/receipts/event].json', '/opt/receipts/mapping].json')

    def test_referenced_public_http_uuid_keeps_original_bytes(self):
        session = '00000000-0000-4000-8000-000000000097'
        targets = ('&#104;ttps&colon;&sol;&sol;public.example&sol;runs&sol;' + session,
                   r'HTTPS\:\/\/public.example\/runs\/' + session,
                   'https&colon;&sol;&sol;public.example/runs/' + session)
        for target in targets:
            for source in (f'[public]({target})', f'[public](<{target}> "title")tail',
                           f'[public]: {target} "title"\nnext'):
                with self.subTest(source=source):
                    self.assertEqual(self.loop.public_state(source), source)
            public = self.loop.public_state({'private_sessions': [session_fingerprint(session)],
                                            'evidence': f'[private]({target})'}, private_sessions=[session_fingerprint(session)])
            self.assertEqual(public['evidence'], '[private]([private URI])')
        self.assertEqual(self.loop.public_state('standalone:' + session), 'standalone:' + session_fingerprint(session))
        literal = f'[literal](&#104;ttps&COLON;&sol;&sol;public.example&sol;runs&sol;{session})'
        self.assertEqual(self.loop.public_state(literal), literal.replace(session, session_fingerprint(session)))
        literal = f'[literal](&amp;#104;ttps&amp;colon;&amp;sol;&amp;sol;public.example&amp;sol;runs&amp;sol;{session})'
        self.assertEqual(self.loop.public_state(literal), literal.replace(session, session_fingerprint(session)))

    def test_bare_references_use_shared_private_comparison_and_keep_tail(self):
        known = '/mnt/receipts/event.json'
        for target in ('&sol;mnt&sol;receipts&sol;event.json', '&#47;mnt&#47;receipts&#47;event.json',
                       'file&colon;&sol;&sol;&sol;mnt&sol;receipts&sol;event.json'):
            self.assertEqual(self.loop.public_state(f'Evidence: {target} tail', [known]),
                             'Evidence: [private URI] tail')
        for target in ('&amp;sol;mnt&amp;sol;receipts&amp;sol;event.json',
                       r'\&sol;mnt\&sol;receipts\&sol;event.json',
                       '&#104;ttps&colon;&sol;&sol;public.example/runs/00000000-0000-4000-8000-000000000097'):
            source = f'Evidence: {target} tail'
            self.assertEqual(self.loop.public_state(source, [known]), source)

    def test_known_bare_path_span_inside_prose_keeps_prefix_and_tail(self):
        known = '/mnt/receipts/event.json'
        for target in ('&sol;mnt&sol;receipts&sol;event.json', '&#47;mnt&#47;receipts&#47;event.json'):
            for prefix in ('receipt=', 'path='):
                self.assertEqual(self.loop.public_state(prefix + target, [known]), prefix + '[private URI]')
            self.assertEqual(self.loop.public_state(f'prefix:{target},tail', [known]), 'prefix:[private URI],tail')
        for target in ('&amp;sol;mnt&amp;sol;receipts&amp;sol;event.json', r'\&sol;mnt\&sol;receipts\&sol;event.json'):
            source = f'receipt={target} tail'
            self.assertEqual(self.loop.public_state(source, [known]), source)

    def test_http_classification_is_per_occurrence_not_token_value(self):
        session = '00000000-0000-4000-8000-000000000097'
        token = 'https&colon;&sol;&sol;x/r/' + session
        url = f'[public]({token})'
        prose = f'prefix{token}suffix'
        expected = prose.replace(session, session_fingerprint(session))
        self.assertEqual(self.loop.public_state(url + ' ' + prose), url + ' ' + expected)
        self.assertEqual(self.loop.public_state(prose + ' ' + url), expected + ' ' + url)
        self.assertEqual(self.loop.public_state(prose), expected)

    def test_escaped_closing_bracket_does_not_open_http_destination(self):
        session = '00000000-0000-4000-8000-000000000097'
        token = 'https&colon;&sol;&sol;x/r/' + session
        for delimiter in ('(', ':'):
            for count in (1, 2, 3, 4):
                source = ('[label' if delimiter == ':' else '') + '\\' * count + ']' + delimiter + token + ')'
                expected = source.replace(session, session_fingerprint(session)) if count % 2 else source
                self.assertEqual(self.loop.public_state(source), expected)

    def test_known_prose_identity_single_pass_keeps_surrounding_bytes(self):
        session = tal.report()['session']
        for target in (session.replace('-', '&#45;'), session.replace('-', r'\-')):
            state = {'private_sessions': [session_fingerprint(session)], 'evidence': 'Observer: prefix' + target + 'suffix tail'}
            self.assertEqual(self.loop.public_state(state, private_sessions=[session_fingerprint(session)])['evidence'], 'Observer: prefix' + session_fingerprint(session) + 'suffix tail')
        for target in (session.replace('-', '&amp;#45;'), session.replace('-', r'\&num;45;')):
            state = {'private_sessions': [session_fingerprint(session)], 'evidence': 'Observer: ' + target + ' tail'}
            self.assertEqual(self.loop.public_state(state, private_sessions=[session_fingerprint(session)])['evidence'], state['evidence'])

    def test_isolated_known_session_escaped_close_does_not_add_suffix(self):
        session = tal.report()['session']
        for target in ('https://example/session/' + session + r'\)',
                       'https://example/session/' + session.replace('-', '%2D') + r'\)',
                       'https://example/session/' + session + '&rpar;'):
            source = {'private_sessions': [session_fingerprint(session)], 'evidence': f'[origin]({target})tail'}
            self.assertEqual(self.loop.public_state(source, private_sessions=[session_fingerprint(session)])['evidence'], '[origin]([private URI])tail')
    def test_known_uri_filename_closings_are_distinct_from_markdown_enclosure(self):
        from urllib.parse import quote
        known = '/mnt/private receipt/event(1.json'
        source = '[receipt](file:///mnt/./%70rivate%20receipt/event%281.json)'
        self.assertEqual(self.loop.public_state(source, [known]), '[receipt]([private URI])')
        for known in ('/mnt/private receipt/event(1).json', '/mnt/private receipt/event)',
                      '/mnt/private receipt/event(1.json'):
            for safe in ('/()', '/'):
                encoded = 'file://' + quote(known, safe=safe).replace('private', '%70rivate')
                for target in (encoded, encoded.replace('/mnt/', '/mnt/./')):
                    self.assertEqual(self.loop.public_state(target, [known]), '[private URI]')
                    self.assertEqual(self.loop.public_state(f'[receipt]({target})', [known]), '[receipt]([private URI])')
                    self.assertEqual(self.loop.public_state(f'<{target}>', [known]), '<[private URI]>')
                    self.assertEqual(self.loop.public_state(f'[receipt](<{target}>)', [known]), '[receipt](<[private URI]>)')
        public = '[public](HTTPS://public.example/event%281.json?x=%41%2f)'
        self.assertEqual(self.loop.public_state(public, [known]), public)

    def check_paths(self, virtual, support):
        self.prepare()
        first = self.fixture.receipt.read_bytes(); second = b'{"support":true}'
        self.fixture.bundle['receipts'][0]['path'] = virtual
        self.fixture.bundle['receipts'].append({'path': support, 'sha256': hashlib.sha256(second).hexdigest()})
        self.fixture.seal()
        originals = {virtual: first, support: second}
        original_stat, original_open, original_file = Path.stat, os.open, Path.is_file
        backing = {}
        for i, (name, body) in enumerate(originals.items()):
            path = self.fixture.root / f'virtual-original-{i}.json'
            path.write_bytes(body); path.chmod(0o600)
            backing[name] = path
        info = __import__('types').SimpleNamespace(st_uid=os.getuid(), st_mode=0o100600)
        def stat(path, *args, **kw):
            return info if str(path) in originals else original_stat(path, *args, **kw)
        def open_original(path, flags, *args, **kwargs):
            return original_open(backing.get(str(path), path), flags, *args, **kwargs)
        urls = ('https://public.example/tmp/guide https://public.example/opt/docs '
                'https://public.example/runs/00000000-0000-4000-8000-000000000097/result '
                'HTTPS://public.example/runs/00000000-0000-4000-8000-000000000097/result')
        encoded = Path(support).as_uri()
        import re
        lower = re.sub(r'%[0-9A-F]{2}', lambda m: m[0].lower(), encoded)
        local = encoded.replace('file:///', 'file://localhost/')
        private_url = 'https://public.example/session/' + tal.report()['session']
        contributor_url = 'https://public.example/origin/' + self.fixture.sessions[0]
        alternate = encoded.replace('private', '%70rivate').replace('.json', '%2Ej%73on')
        dot = encoded.replace('/private', '/./private')
        escaped_dot = encoded.replace('/private', '/%2e/private')
        encoded_identity = private_url.replace('-', '%2D')
        encoded_contributor = contributor_url.replace('00000000-', '%300000000%2d', 1)
        semantic = encoded.replace('%28', '(').replace('%29', ')').replace('%5D', ']')
        markdown_targets = (semantic.replace('(', r'\(').replace(')', r'\)'), semantic.replace('.', r'\.'),
                            semantic.replace('(', '&#40;').replace(')', '&#41;'),
                            semantic.replace('(', '&#x28;').replace(')', '&#x29;'),
                            semantic.replace('(', '&lpar;').replace(')', '&rpar;'), semantic.replace(']', r'\]'),
                            encoded.replace('/', '&#47;'), encoded[7:].replace('/', '&#47;'),
                            encoded.replace('file:', '&#102;ile&#58;').replace('/', '&sol;'),
                            encoded.replace(':', r'\:').replace('/', r'\/'))
        markdown_forms = ' '.join(f'[form]({target})' for target in markdown_targets)
        bare_forms = ' '.join(encoded.replace('/', ref) for ref in ('&sol;', '&#47;'))
        bare_forms += ' receipt=' + support.replace('/', '&sol;').replace(' ', '&#32;')
        markdown_forms += ' ' + bare_forms + ' [origin](' + private_url + r'\))tail'
        unrelated = 'https://public.example/runs/00000000%2d0000%2D4000%2d8000%2D000000000097/result'
        urls += ' ' + unrelated + ' https://public.example/event(1).json [public](https://public.example/event(1).json)'
        referenced_public = '[public](&#104;ttps&colon;&sol;&sol;public.example&sol;runs&sol;00000000-0000-4000-8000-000000000097)'
        urls += ' ' + referenced_public
        twin = 'https&colon;&sol;&sol;x/r/00000000-0000-4000-8000-000000000097'
        mixed = f'[public]({twin}) prefix{twin}suffix'
        mixed_expected = mixed.replace('prefix' + twin, 'prefix' + twin.replace('00000000-0000-4000-8000-000000000097',
                                    session_fingerprint('00000000-0000-4000-8000-000000000097')))
        encoded_prose = tal.report()['session'].replace('-', '&#45;')
        escaped = r'\](' + twin + ')'
        markdown_forms += f' {mixed} {escaped} Observer:{encoded_prose}:tail'
        session = '00000000-0000-4000-8000-000000000098'
        self.worker.side_effect = lambda *args, **kw: dict(tal.report('changes_requested'),
                evidence=f'prefix:{virtual}:suffix file://{support} {urls} standalone:{session} {encoded} {lower} {local} {private_url} {contributor_url} {alternate} {dot} {escaped_dot} [dot]({dot}) [alternate]({alternate[7:]}) {encoded_identity} {encoded_contributor} <{alternate}> [angle](<{alternate}>) [normal]({alternate}) [encoded]({encoded[7:]}) [lower]({lower[7:]}) {markdown_forms}',
                findings=[f'Check [{virtual}]({support}) [{encoded}]({local}) [target]({encoded[7:]}) [lower]({lower[7:]}) [alternate]({alternate[7:]}) {encoded_identity} {encoded_contributor} {markdown_forms} {referenced_public}'], repair_revision='e' * 64,
                repair_evidence_sha256=args[2]['private_repair_evidence']['bundle_sha256'])
        def is_file(path):
            return True if str(path) in originals else original_file(path)
        with patch.object(Path, 'stat', stat), patch.object(os, 'open', open_original), patch.object(Path, 'is_file', is_file):
            h = self.loop.load(36)[0]
            public = {k: v for k, v in h.items() if k not in ('source', 'host', 'repair_evidence', 'private_paths', 'evidence_records')}
            packet = self.fixture.load()
            public = register(self.loop.storage, public, h['source'], tal.al.HOST, self.fixture.path,
                              packet['private_paths'], packet['evidence_records'])
            self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF_V2, public)
            self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        _, state, sid, _ = self.loop.load(36)
        self.assertIn(virtual, state['review']['evidence'])
        self.assertIn(virtual, state['private_paths']); self.assertIn(support, state['private_paths'])
        self.loop.save(self.gh.pull, state, sid)
        public = json.dumps(self.gh.messages)
        self.assertNotIn(virtual, public); self.assertNotIn(support, public)
        self.assertNotIn('private_paths', public)
        self.assertNotIn('private_sessions', public)
        for uri in (encoded, lower, local, encoded[7:], lower[7:], private_url, contributor_url, alternate, alternate[7:], dot, escaped_dot, encoded_identity, encoded_contributor): self.assertNotIn(uri, public)
        for target in markdown_targets: self.assertNotIn(target, json.dumps(self.gh.messages))
        self.assertIn('https://public.example/origin/' + session_fingerprint(self.fixture.sessions[0]), public)
        self.assertIn('https://public.example/session/' + session_fingerprint(tal.report()['session']), public)
        self.assertNotIn('/mnt/receipts/', public); self.assertNotIn('/opt/receipts/', public)
        self.assertNotIn(session, public)
        self.assertIn('standalone:' + session_fingerprint(session), public)
        self.assertIn(urls, public)
        self.assertIn(mixed_expected, public)
        self.assertIn(escaped.replace('00000000-0000-4000-8000-000000000097',
                                     session_fingerprint('00000000-0000-4000-8000-000000000097')), public)
        self.assertNotIn(encoded_prose, public)
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


    def test_legacy_writer_precedes_first_observation_but_later_composition_is_valid(self):
        self.legacy()
        later = '2026-01-02T00:00:00Z'
        self.accepted['repair']['results']['LATER'] = {'observation': dict(self.observed, at=later)}
        self.receipt.write_text(json.dumps({'events': [{} for _ in range(5)]}))
        sealed = hashlib.sha256(self.receipt.read_bytes()).hexdigest()
        self.bundle['receipts'][0]['sha256'] = self.bundle['mapping_receipt_sha256'] = sealed
        for origin in self.bundle['origins']: origin['receipt_sha256'] = sealed
        for i, activity in ((3, 'execution'), (4, 'composition')):
            self.bundle['origins'].append(dict(self.bundle['origins'][0], receipt_locator=f'/events/{i}',
                                              activity=activity, at=later))
        self.seal(); self.load()
        self.bundle['origins'][0]['at'] = later; self.seal()
        with self.assertRaisesRegex(ValueError, 'Legacy'): self.load()


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

    def test_rebind_write_failures_never_combine_new_handoff_with_old_approval(self):
        from types import SimpleNamespace
        for failure in ('state', 'handoff', 'progress'):
            for paused in (False, True):
                self.doCleanups(); self.setUp()
                with self.subTest(failure=failure, paused=paused):
                    ConsumerTests.prepare(self)
                    self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
                    before, state, sid, _ = self.loop.load(36)
                    state['paused'] = paused
                    self.loop.save(self.gh.pull, state, sid)
                    self.loop.validate_review(state['review'], state['binding'], self.accepted)
                    replacement = self.fixture.root / 'replacement.json'
                    replacement.write_bytes(self.fixture.path.read_bytes()); replacement.chmod(0o600)
                    args = SimpleNamespace(pr=36, source=None, migration_record=None, repair_evidence=replacement)
                    original_comment = self.gh.comment
                    calls = []
                    def write(number, body, comment_id=None):
                        calls.append((number, body))
                        if ((failure == 'state' and body.startswith(tal.al.STATE)) or
                            (failure == 'handoff' and body.startswith(tal.al.HANDOFF_V2)) or
                            (failure == 'progress' and number == 35)):
                            raise OSError('synthetic publication failure')
                        return original_comment(number, body, comment_id)
                    with patch.object(self.gh, 'comment', side_effect=write), self.assertRaises(OSError):
                        tal.al.rebind_target(self.loop, args)
                    current, saved, _, _ = self.loop.load(36)
                    self.assertEqual(current['registry_id'], before['registry_id'])
                    self.assertEqual(saved['paused'], paused)
                    if failure == 'state':
                        self.assertIn('review', saved)
                        self.assertFalse(any(body.startswith(tal.al.HANDOFF_V2) for _, body in calls))
                    else:
                        for key in ('review', 'binding', 'gui', 'acceptance'): self.assertNotIn(key, saved)
                        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
                    self.assertEqual(self.gh.merges, 0)
                    self.assertEqual(self.gh.issues[35]['state'], 'open')
                    tal.al.rebind_target(self.loop, args)
                    recovered, saved, _, _ = self.loop.load(36)
                    self.assertEqual(recovered['repair_evidence'], str(replacement.resolve()))
                    self.assertEqual(saved['paused'], paused)
                    for key in ('review', 'binding', 'gui', 'acceptance'): self.assertNotIn(key, saved)


class WorkerDirectoryTests(unittest.TestCase):
    setUp = EvidenceTests.setUp
    seal = EvidenceTests.seal

    def test_existing_public_directory_is_private_before_mock_launch(self):
        output = self.root / 'workers'; output.mkdir(mode=0o755); output.chmod(0o755)
        process = Mock(pid=123, returncode=0)
        def start(command, **kw):
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            raw = tal.report(); raw.pop('session')
            target = Path(command[command.index('-o') + 1]); target.write_text(json.dumps(raw)); target.chmod(0o644)
            kw['stdout'].write(json.dumps({'type': 'thread.started', 'thread_id': tal.report()['session']}) + '\n')
            kw['stdout'].write(json.dumps({'type': 'turn.completed'}) + '\n'); kw['stdout'].flush()
            return process
        with patch('agent_worker.subprocess.Popen', side_effect=start), patch('agent_worker.os.killpg'):
            result = run_worker('review', self.root, {}, output)
        self.assertEqual(result['session'], tal.report()['session'])

    def test_out_of_contract_public_storage_is_rejected_after_mock_launch(self):
        tal.LoopTests.setUp(self)
        self.loop.storage.chmod(0o755)
        output = self.checkout.parent / 'workers'; output.mkdir(); output.chmod(0o755)
        self.loop.worker = run_worker
        process = Mock(pid=123, returncode=0)
        def start(command, **kw):
            self.assertEqual(self.loop.storage.stat().st_mode & 0o777, 0o755)
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            raw = tal.report(); raw.pop('session')
            target = Path(command[command.index('-o') + 1]); target.write_text(json.dumps(raw)); target.chmod(0o644)
            kw['stdout'].write(json.dumps({'type': 'thread.started', 'thread_id': tal.report()['session']}) + '\n')
            kw['stdout'].write(json.dumps({'type': 'turn.completed'}) + '\n'); kw['stdout'].flush()
            return process
        with patch('agent_worker.subprocess.Popen', side_effect=start) as launch, patch('agent_worker.os.killpg'):
            with self.assertRaisesRegex(ValueError, 'Storage must be private'):
                self.loop.tick(36)
        launch.assert_called_once()
        self.assertEqual(output.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.gh.merges, 0)

    def test_symlink_or_foreign_output_directory_never_launches(self):
        from types import SimpleNamespace
        target = self.root / 'target'; target.mkdir()
        link = self.root / 'link'; link.symlink_to(target, target_is_directory=True)
        with patch('agent_worker.subprocess.Popen') as launch, self.assertRaises((OSError, ValueError)):
            run_worker('review', self.root, {}, link)
        launch.assert_not_called()
        with patch('agent_worker.os.fstat', return_value=SimpleNamespace(st_uid=os.getuid() + 1)), \
                patch('agent_worker.subprocess.Popen') as launch, self.assertRaises((OSError, ValueError)):
            run_worker('review', self.root, {}, target)
        launch.assert_not_called()


class EvidenceRetentionTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def move_evidence(self, mode, root=None):
        ignored = (root or self.checkout) / '.harness-local'; ignored.mkdir(exist_ok=True)
        if mode == 'bundle':
            target = ignored / 'bundle.json'; target.write_bytes(self.fixture.path.read_bytes()); target.chmod(0o600)
            return target
        target = ignored / 'receipt.json'; target.write_bytes(self.fixture.receipt.read_bytes())
        if mode == 'extra':
            target.write_text('{"context":true}')
            self.fixture.bundle['receipts'].append({'sha256': hashlib.sha256(target.read_bytes()).hexdigest(), 'path': str(target)})
        else:
            if mode == 'alias':
                alias = self.fixture.root / 'alias'; alias.symlink_to(ignored, target_is_directory=True)
                target = alias / target.name
            self.fixture.bundle['receipts'][0]['path'] = str(target)
        self.fixture.seal()
        return self.fixture.path

    def test_enroll_and_rebind_reject_all_cleanup_evidence_without_mutation(self):
        for operation in ('enroll', 'rebind'):
            for mode in ('bundle', 'mapping', 'extra', 'alias'):
                self.doCleanups(); self.setUp()
                with self.subTest(operation=operation, mode=mode):
                    args = (EnrollmentTests.prepare(self) if operation == 'enroll' else
                            RepairRecoveryTests.prepare(self, 2, True))
                    args.repair_evidence = self.move_evidence(mode)
                    messages = copy.deepcopy(self.gh.messages)
                    registry = self.loop.storage / 'registry'
                    records = {p.name: p.read_bytes() for p in registry.glob('*')}
                    action = tal.al.enroll if operation == 'enroll' else tal.al.rebind_target
                    with self.assertRaisesRegex(ValueError, 'outside cleanup'): action(self.loop, args)
                    self.assertEqual(self.gh.messages, messages)
                    self.assertEqual({p.name: p.read_bytes() for p in registry.glob('*')}, records)
                    self.assertFalse(self.gh.statuses)

    def test_existing_unsafe_bundle_blocks_review_postmerge_and_cleanup_before_effects(self):
        for location in ('source', 'managed'):
            for phase in ('review', 'postmerge', 'cleanup', 'retained-old-path'):
                self.doCleanups(); self.setUp()
                with self.subTest(phase=phase):
                    ConsumerTests.prepare(self)
                    original = self.loop.load(36)[0]
                    source = self.fixture.root / 'owned-source'; source.mkdir()
                    public = {k: v for k, v in original.items() if k not in ('source', 'host', 'repair_evidence', 'private_paths')}
                    public = register(self.loop.storage, public, source, tal.al.HOST, self.fixture.path)
                    self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF_V2, public)
                    h, state, sid, _ = self.loop.load(36)
                    cleanup_root = source if location == 'source' else self.checkout
                    if phase == 'retained-old-path':
                        h['private_paths'] = [str(cleanup_root / '.harness-local/old-original.json')]
                    else:
                        self.move_evidence('mapping', cleanup_root)
                        packet = self.fixture.load()
                        self.accepted['repair_evidence_sha256'] = packet['bundle_sha256']
                        bound = self.loop.bound(self.gh.pull, self.gh.issues[35], self.gh.comments(36))[0]
                        raw = dict(tal.report(), repair_revision='e' * 64, repair_assessment='synthetic cached evidence',
                                   repair_evidence_sha256=packet['bundle_sha256'])
                        state.update(phase='reviewed', binding=bound, review=issue_review_origin(
                            self.loop.storage, raw, bound, raw['session'], 'manual', packet['private_paths'],
                            [origin['session'] for origin in packet['origins']]))
                        self.loop.save(self.gh.pull, state, sid)
                    before = self.fixture.receipt.read_bytes()
                    if phase == 'review':
                        self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
                        self.worker.assert_not_called()
                    elif phase == 'postmerge':
                        self.gh.pull.update(merged=True, state='closed')
                        with self.assertRaisesRegex(ValueError, 'outside cleanup'):
                            self.loop.merged_acceptance(self.gh.pull, h, state)
                    else:
                        with patch.object(tal.al, 'command') as command, patch.object(tal.al, 'git') as git, \
                                patch.object(tal.al.shutil, 'rmtree') as remove:
                            with self.assertRaisesRegex(ValueError, 'outside cleanup'):
                                tal.al.Loop._cleanup_resources(self.loop, self.gh.pull, h)
                            command.assert_not_called(); git.assert_not_called(); remove.assert_not_called()
                    self.assertEqual(self.fixture.receipt.read_bytes(), before)
                    self.assertEqual(self.gh.issues[35]['state'], 'open'); self.assertEqual(self.gh.merges, 0)
                    self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)


    def test_external_sibling_evidence_is_allowed_without_copying(self):
        args = EnrollmentTests.prepare(self)
        sibling = self.checkout.with_name('checkout-retained'); sibling.mkdir()
        receipt = sibling / 'receipt.json'; receipt.write_bytes(self.fixture.receipt.read_bytes())
        self.fixture.bundle['receipts'][0]['path'] = str(receipt); self.fixture.seal()
        args.repair_evidence = self.fixture.path
        expected = receipt.read_bytes()
        tal.al.enroll(self.loop, args)
        self.assertEqual(receipt.read_bytes(), expected)


class HistoricalEvidenceTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp
    prepare = ConsumerTests.prepare

    def replace(self):
        from types import SimpleNamespace
        self.prepare()
        replacement = EvidenceTests(); replacement.setUp(); self.addCleanup(replacement.doCleanups)
        args = SimpleNamespace(pr=36, source=None, migration_record=None, repair_evidence=replacement.path)
        tal.al.rebind_target(self.loop, args)
        return args, replacement

    def test_missing_changed_history_blocks_rebind_and_all_consumers_with_current_good_bundle(self):
        for original in ('bundle', 'receipt'):
            for fault in ('missing', 'changed'):
                self.doCleanups(); self.setUp()
                with self.subTest(original=original, fault=fault):
                    args, current = self.replace()
                    self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
                    h, state, _, _ = self.loop.load(36)
                    target = self.fixture.path if original == 'bundle' else self.fixture.receipt
                    if fault == 'missing': target.unlink()
                    else: target.write_bytes(b'changed historical bytes')
                    current.load()  # The replacement originals remain intact.
                    before = copy.deepcopy(self.gh.messages)
                    registry = {p.name: p.read_bytes() for p in (self.loop.storage / 'registry').iterdir()}
                    with self.assertRaises((ValueError, OSError)): tal.al.rebind_target(self.loop, args)
                    self.assertEqual(self.gh.messages, before)
                    self.assertEqual({p.name: p.read_bytes() for p in (self.loop.storage / 'registry').iterdir()}, registry)
                    self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
                    with self.assertRaises((ValueError, OSError)): self.loop.merged_acceptance(self.gh.pull, h, state)
                    with self.assertRaises((ValueError, OSError)): self.loop._cleanup_resources(self.gh.pull, h)
                    with self.assertRaises((ValueError, OSError)):
                        self.loop.repair_evidence({'allowed': True, 'mode': 'tooling'}, h, state['binding'])
                    self.gh.pull.update(merged=True, state='closed')
                    self.loop.bound = Mock(return_value=(state['binding'], []))
                    before = copy.deepcopy(self.gh.messages)
                    proof = {'binding': state['binding'], 'review': dict(state['review'],
                             feedback_hash=state['binding']['feedback_hash'])}
                    with self.assertRaises((ValueError, OSError)): self.loop.reconcile_feedback(self.gh.pull, proof)
                    self.assertEqual(self.gh.messages, before)
                    self.gh.pull['base']['ref'] = 'main'
                    with self.assertRaises((ValueError, OSError)): self.loop.finish(self.gh.pull, h, state, None)
                    self.assertEqual(self.gh.messages, before)
                    self.assertEqual(self.gh.merges, 0)
                    self.assertEqual(self.gh.issues[35]['state'], 'open')

    def test_unchanged_history_and_canonical_aliases_have_one_identity(self):
        args, current = self.replace()
        first = self.loop.load(36)[0]['evidence_records']
        args.repair_evidence = current.path.parent / '..' / current.path.parent.name / current.path.name
        tal.al.rebind_target(self.loop, args)
        records = self.loop.load(36)[0]['evidence_records']
        self.assertEqual(records, first)
        self.assertEqual(len({item['path'] for item in records}), len(records))
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        self.assertNotIn('evidence_records', json.dumps(self.gh.messages))

    def test_omitted_bundle_is_preflighted_before_any_mutation(self):
        from types import SimpleNamespace
        for location in ('source', 'managed'):
            self.doCleanups(); self.setUp()
            with self.subTest(location=location):
                self.prepare()
                h = self.loop.load(36)[0]
                source = Path(self.tmp.name) / 'separate-source'; source.mkdir()
                root = source if location == 'source' else self.checkout
                receipt = root / '.harness-local' / 'mapping.json'; receipt.parent.mkdir(exist_ok=True)
                receipt.write_bytes(self.fixture.receipt.read_bytes())
                self.fixture.bundle['receipts'][0]['path'] = str(receipt); self.fixture.seal()
                public = {k: v for k, v in h.items() if k not in ('source', 'host', 'repair_evidence', 'private_paths', 'evidence_records')}
                public = register(self.loop.storage, public, source, tal.al.HOST, self.fixture.path)
                self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF_V2, public)
                before = copy.deepcopy(self.gh.messages)
                registry = {p.name: p.read_bytes() for p in (self.loop.storage / 'registry').iterdir()}
                args = SimpleNamespace(pr=36, source=None, migration_record=None, repair_evidence=None)
                with self.assertRaisesRegex(ValueError, 'outside cleanup'): tal.al.rebind_target(self.loop, args)
                self.assertEqual(self.gh.messages, before)
                self.assertEqual({p.name: p.read_bytes() for p in (self.loop.storage / 'registry').iterdir()}, registry)

    def test_legacy_history_needs_explicit_issued_hash_proof_not_current_bytes(self):
        for version in (1, 2):
            for paused in (False, True):
                self.doCleanups(); self.setUp()
                with self.subTest(version=version, paused=paused):
                    self.recover_legacy(version, paused)

    def recover_legacy(self, version, paused):
        from types import SimpleNamespace
        from agent_worker import recover_originals
        self.prepare()
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        h, state, sid, _ = self.loop.load(36)
        state['paused'] = paused
        self.loop.save(self.gh.pull, state, sid)
        # Simulate an older issued report and registry with no recorded identities.
        raw = {k: v for k, v in state['review'].items() if k not in ('origin_receipt', 'evidence_records')}
        issued = issue_review_origin(self.loop.storage, raw, state['binding'], raw['session'], 'manual', h['private_paths'])
        proof = {'review': issued, 'binding': state['binding']}
        path = self.fixture.root / 'issued-proof.json'; path.write_text(json.dumps(proof)); path.chmod(0o600)
        public = {k: v for k, v in h.items() if k not in ('source', 'host', 'registry_id', 'version',
                  'repair_evidence', 'private_paths', 'evidence_records')}
        if version == 1:
            public.update(source=h['source'], host=h['host'], repair_evidence=str(self.fixture.path),
                          private_paths=h['private_paths'])
            self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF, public)
        else:
            public = register(self.loop.storage, public, h['source'], tal.al.HOST, self.fixture.path, h['private_paths'])
            self.gh.messages[36][0]['body'] = tal.al.pack(tal.al.HANDOFF_V2, public)
        args = SimpleNamespace(pr=36, source=None, migration_record=None, repair_evidence=None)
        with self.assertRaisesRegex(ValueError, 'explicit trusted'): tal.al.rebind_target(self.loop, args)
        altered = copy.deepcopy(proof); altered['review']['repair_evidence_sha256'] = '0' * 64
        with self.assertRaises(ValueError): recover_originals(self.loop.storage, altered)
        args.retained_review = [path]
        tal.al.rebind_target(self.loop, args)
        recovered, saved, _, _ = self.loop.load(36)
        self.assertEqual(recovered['evidence_records'], self.fixture.load()['evidence_records'])
        self.assertEqual(saved['paused'], paused)
        for key in ('branch', 'issue', 'head', 'scope', 'source', 'previous_owner'):
            self.assertEqual(recovered.get(key), h.get(key))
        self.assertNotIn('review', saved)
        self.fixture.receipt.write_bytes(b'changed after trusted proof')
        with self.assertRaisesRegex(ValueError, 'content changed'): recover_originals(self.loop.storage, proof)
        self.fixture.path.write_bytes(b'changed bundle after trusted proof')
        with self.assertRaises(ValueError): recover_originals(self.loop.storage, proof)


class OriginalPrivacyTests(unittest.TestCase):
    setUp = EvidenceTests.setUp
    load = EvidenceTests.load
    seal = EvidenceTests.seal
    def test_checked_receipt_descriptor_survives_path_replacement(self):
        from agent_worker import private_original
        original_read, original_fstat = Path.read_bytes, os.fstat
        with tempfile.TemporaryDirectory(dir='/tmp') as folder:
            parent = Path(folder); parent.chmod(0o777)
            source = parent / 'receipt.json'
            for symlink in (False, True):
                if source.is_symlink(): source.unlink()
                source.write_bytes(b'checked original'); source.chmod(0o600)
                replacement = parent / 'replacement.json'
                replacement.write_bytes(b'replacement'); replacement.chmod(0o644)
                changed = False
                def replace():
                    nonlocal changed
                    if changed: return
                    changed = True
                    if symlink:
                        source.unlink(); source.symlink_to(replacement)
                    else:
                        replacement.replace(source)
                def read(path):
                    if path == source: replace()
                    return original_read(path)
                def fstat(fd):
                    info = original_fstat(fd)
                    replace()
                    return info
                with self.subTest(symlink=symlink), patch.object(Path, 'read_bytes', read), patch.object(os, 'fstat', fstat):
                    self.assertEqual(private_original(source), b'checked original')
                self.assertTrue(changed)
            parent.chmod(0o700)
    def test_public_ancestors_reject_other_user_write_but_private_ancestor_protects(self):
        from agent_worker import private_original, retained_originals
        with tempfile.TemporaryDirectory(dir='/tmp') as folder:
            parent = Path(folder)
            receipt = parent / 'receipt.json'
            body = b'{"synthetic":true}'
            receipt.write_bytes(body)
            record = [{'path': str(receipt), 'sha256': hashlib.sha256(body).hexdigest()}]
            for parent_mode, mode, accepted in ((0o755, 0o602, False), (0o755, 0o620, False),
                                                (0o700, 0o602, True), (0o755, 0o600, True),
                                                (0o755, 0o644, False)):
                parent.chmod(parent_mode); receipt.chmod(mode)
                with self.subTest(parent=parent_mode, mode=mode):
                    if parent_mode == 0o755:
                        self.assertTrue(all(p.stat().st_mode & 1 for p in receipt.resolve().parents))
                    if accepted:
                        self.assertEqual(private_original(receipt), body)
                        retained_originals(record)
                    else:
                        with self.assertRaisesRegex(ValueError, 'private file or ancestor'): private_original(receipt)
                        with self.assertRaisesRegex(ValueError, 'private file or ancestor'): retained_originals(record)
            parent.chmod(0o700); receipt.chmod(0o600)

    def test_manual_input_uses_private_original_before_parse_or_output(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as folder:
            parent = Path(folder)
            source = parent / 'report.json'
            raw = tal.report()
            source.write_text(json.dumps(raw))
            for i, (parent_mode, mode, accepted) in enumerate(((0o755, 0o644, False), (0o755, 0o602, False),
                                                              (0o700, 0o644, True), (0o755, 0o600, True))):
                parent.chmod(parent_mode); source.chmod(mode)
                output = parent / f'output-{i}.json'
                with patch.dict(os.environ, {'CODEX_THREAD_ID': raw['session']}), patch.object(sys, 'argv',
                        ['manual', '--report', str(source), '--output', str(output)]):
                    if accepted:
                        bind_manual_report()
                        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
                    else:
                        with self.assertRaisesRegex(ValueError, 'private file or ancestor'): bind_manual_report()
                        self.assertFalse(output.exists())
            alias = parent / 'alias.json'; alias.symlink_to(source)
            output = parent / 'symlink-output.json'
            with patch.dict(os.environ, {'CODEX_THREAD_ID': raw['session']}), patch.object(sys, 'argv',
                    ['manual', '--report', str(alias), '--output', str(output)]):
                with self.assertRaisesRegex(ValueError, 'regular file'): bind_manual_report()
            self.assertFalse(output.exists())
            parent.chmod(0o700); source.chmod(0o600)
    def test_accessible_readable_original_rejected_private_file_and_real_parent_accepted(self):
        from types import SimpleNamespace
        from agent_worker import private_original
        original_stat = Path.stat
        parents = set(self.receipt.resolve().parents)
        def public_ancestors(path, *args, **kwargs):
            if path in parents:
                return SimpleNamespace(st_uid=os.getuid(), st_mode=0o40755)
            return original_stat(path, *args, **kwargs)
        expected = self.receipt.read_bytes()
        self.receipt.chmod(0o644)
        self.assertEqual(private_original(self.receipt), expected)
        with patch.object(Path, 'stat', public_ancestors):
            with self.assertRaisesRegex(ValueError, 'private file or ancestor'): self.load()
            self.receipt.chmod(0o600)
            self.assertEqual(private_original(self.receipt), expected)
        self.receipt.chmod(0o644)
        alias = self.root / 'alias'; alias.symlink_to(self.root, target_is_directory=True)
        aliased_receipt = alias / self.receipt.name
        with patch.object(Path, 'stat', public_ancestors):
            with self.assertRaisesRegex(ValueError, 'private file or ancestor'): private_original(aliased_receipt)
