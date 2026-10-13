"""Synthetic certificates exercise shared gates; none are real product acceptance."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, call, patch

from test_agent_loop import BASE, NEW
from test_verification import HEAD
from agent_policy import session_fingerprint
from test_observation_reuse import OLD, RECORD, CASE_PATH
import test_responsibility_revision as tr
from verification import (IMPACT_DIMENSIONS, INITIAL_PLAN, canonical_hash, impact_history,
                          load_impact_source, render_queue, unchanged_artifact_history)

CERT_PATH = 'docs/verification/changes/issue-246.json'
COMPONENT = 'plugin/src/main/kotlin/Video.kt'
REPAIR = 'plugin/src/main/kotlin/AppHistory.kt'


class ImpactSourceTests(unittest.TestCase):
    def test_cached_commit_needs_no_remote_or_fetch(self):
        git = Mock(return_value='commit')
        load_impact_source(OLD, git)
        git.assert_called_once_with('cat-file', '-t', OLD)

    def test_missing_commit_fetches_exact_sha_and_checks_retrieved_object(self):
        for origin in ('https://github.com/shinma06/android-replay-buffer',
                       'https://github.com/shinma06/android-replay-buffer.git',
                       'git@github.com:shinma06/android-replay-buffer.git'):
            with self.subTest(origin=origin):
                git = Mock(side_effect=[subprocess.CalledProcessError(128, 'cat-file'), origin, '', 'commit'])
                load_impact_source(OLD, git)
                self.assertEqual(git.call_args_list, [call('cat-file', '-t', OLD),
                    call('remote', 'get-url', 'origin'), call('fetch', '--no-tags', 'origin', OLD),
                    call('cat-file', '-t', OLD)])

    def test_invalid_ref_is_rejected_before_git(self):
        for source in (None, 1, 'main', OLD[:12], OLD.upper(), '--all', OLD + ':file'):
            with self.subTest(source=source):
                git = Mock()
                with self.assertRaises(ValueError):
                    load_impact_source(source, git)
                git.assert_not_called()

    def test_untrusted_origin_cannot_fetch_missing_commit(self):
        for origin in ('/tmp/foreign', 'https://github.com/foreign/android-replay-buffer.git',
                       'https://github.com/shinma06/android-replay-buffer.evil',
                       'https://github.com.evil/shinma06/android-replay-buffer'):
            with self.subTest(origin=origin):
                git = Mock(side_effect=[subprocess.CalledProcessError(128, 'cat-file'), origin])
                with self.assertRaisesRegex(ValueError, 'trusted origin'):
                    load_impact_source(OLD, git)
                self.assertEqual(git.call_count, 2)

    def test_failed_fetch_or_missing_retrieved_object_remains_fatal(self):
        origin = 'https://github.com/shinma06/android-replay-buffer'
        for tail in ([subprocess.CalledProcessError(128, 'fetch')],
                     ['', subprocess.CalledProcessError(128, 'cat-file')]):
            git = Mock(side_effect=[subprocess.CalledProcessError(128, 'cat-file'), origin, *tail])
            with self.assertRaises(subprocess.CalledProcessError):
                load_impact_source(OLD, git)
            self.assertEqual(git.call_count, 2 + len(tail))

    def test_blob_tree_or_tag_cannot_stand_in_for_commit(self):
        for kind in ('blob', 'tree', 'tag'):
            for missing in (False, True):
                with self.subTest(kind=kind, missing=missing):
                    git = Mock(side_effect=[subprocess.CalledProcessError(128, 'cat-file'),
                        'https://github.com/shinma06/android-replay-buffer', '', kind] if missing else [kind])
                    with self.assertRaisesRegex(ValueError, 'exact requested commit'):
                        load_impact_source(OLD, git)


class ImpactEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.prior = tr.ResponsibilityRevisionTests()
        self.prior.setUp()
        self.fixture = self.prior.fixture
        self.key = '36:QA-1-EMU'
        self.policy = self.prior.policy
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.fixture.documents[f'{OLD}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.fixture.documents[f'{OLD}:{CASE_PATH}'] = copy.deepcopy(self.fixture.documents[f'{NEW}:{CASE_PATH}'])
        self.fixture.documents[f'{OLD}:{COMPONENT}'] = 'source component'
        self.fixture.documents[f'{NEW}:{COMPONENT}'] = 'source component'
        self.paths = REPAIR
        self.original_git = self.fixture.git
        self.fixture.git = self.git
        self.observed = self.prior.observed
        self.observed['at'] = '2026-10-13T10:00:00+09:00'
        old = dict(copy.deepcopy(self.observed), head=OLD, artifact_sha256='d' * 64,
                   at='2026-09-08T10:00:00+09:00', reason='Actual prior execution (synthetic).')
        original = copy.deepcopy(self.prior.initial.manifest['results'][self.key])
        original.update(head=OLD, artifact_sha256='d' * 64, initial_observation=old)
        fresh = dict(copy.deepcopy(self.observed), at='2026-10-13T09:00:00+09:00',
                     reason='Actual current load and integration (synthetic).')
        self.assessment = {
            'source_candidate': OLD, 'source_result': original,
            'source_result_sha256': canonical_hash(original),
            'source_record_evidence': 'Public-safe original certificate; private bytes retained.',
            'facts': {'prior': old, 'current': fresh},
            'coverage': {k: ['current' if k == 'artifact_binding' else 'prior']
                         for k in self.observed['responsibility_evidence']},
            'missing_requirements': [], 'unresolved_owned_failures': []}
        self.observed['responsibility_assessment'] = self.assessment
        history, case_hash = impact_history(OLD, NEW, self.key, self.git)
        self.certificate = {
            'schema': 1, 'key': self.key, 'candidate': NEW, 'artifact_sha256': 'e' * 64,
            'source_candidate': OLD, 'source_artifact_sha256': 'd' * 64,
            'case_sha256': case_hash, 'plan_sha256': canonical_hash(self.policy), 'history': history,
            'original_file_sha256': 'a' * 64, 'original_result_sha256': 'b' * 64,
            'original_fact_sha256': {'prior': 'c' * 64},
            'original_record_evidence': 'Reviewer examined immutable private bytes and public summaries.',
            'review': {'status': 'approved',
                       'reviewer_session': session_fingerprint('00000000-0000-4000-8000-000000000004'),
                       'writer_session': session_fingerprint('00000000-0000-4000-8000-000000000001'),
                       'executor_sessions': [session_fingerprint('00000000-0000-4000-8000-000000000002'),
                                             session_fingerprint('00000000-0000-4000-8000-000000000003')],
                       'at': '2026-10-13T11:00:00+09:00', 'evidence': 'Fixed independent synthetic report.'},
            'guarantees': {k: {'facts': ids, 'disposition': 'fresh' if k == 'artifact_binding' else 'unaffected',
                              'impact': {d: 'Concrete synthetic comparison: ' + d for d in IMPACT_DIMENSIONS},
                              'components': {COMPONENT: {'source_blob': '0' * 40, 'candidate_blob': '0' * 40}}}
                           for k, ids in self.assessment['coverage'].items()}}
        review = self.certificate['review']
        activities = [('writer', review['writer_session'], old['at']),
                      ('execution', review['executor_sessions'][0], old['at']),
                      ('execution', review['executor_sessions'][1], fresh['at']),
                      ('composition', review['writer_session'], self.observed['at'])]
        self.certificate['provenance'] = {'at': self.observed['at'], 'attribution': {
            'writer': review['writer_session'], 'observer': review['writer_session'],
            'contributors': [{'role': activity, 'session': session, 'activity': activity,
                              'at': at, 'receipt_sha256': 'a' * 64, 'receipt_locator': f'/events/{i}'}
                             for i, (activity, session, at) in enumerate(activities)]}}
        self.seal()

    def git(self, *args):
        if args[:2] == ('cat-file', '-t'):
            return 'commit'
        if args == ('rev-list', f'{OLD}..{OLD}'):
            return ''
        if args == ('rev-list', f'{OLD}..{NEW}'):
            return NEW
        if args[:2] == ('rev-list', '--parents') and args[-1] == NEW:
            return NEW + ' ' + OLD
        if args[0] == 'diff' and args[-2:] == (OLD, NEW):
            return self.paths
        return self.original_git(*args)

    def seal(self):
        self.certificate['observation_sha256'] = canonical_hash(
            {k: v for k, v in self.observed.items() if k != 'responsibility_assessment'})
        self.certificate['assessment_sha256'] = canonical_hash(
            {k: v for k, v in self.assessment.items() if k != 'impact_reuse'})
        self.fixture.documents[f'{RECORD}:{CERT_PATH}'] = {
            'schema': 1, 'issue': 246, 'gui_required': False, 'reason': 'Certificate only.',
            'cli_checks': ['Synthetic fixture.'], 'cases': [],
            'evidence_certificates': {'sample': copy.deepcopy(self.certificate)}}
        for ref in (BASE, HEAD):
            self.fixture.documents[f'{ref}:{CERT_PATH}'] = copy.deepcopy(self.fixture.documents[f'{RECORD}:{CERT_PATH}'])
        self.fixture.product_diff = [CERT_PATH]
        self.assessment['impact_reuse'] = {
            'commit': RECORD, 'path': CERT_PATH, 'id': 'sample', 'sha256': canonical_hash(self.certificate)}

    def render(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'case.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:{CASE_PATH}']))
            return render_queue([path], self.prior.initial.manifest, self.git)

    def test_registration_rejects_pre_candidate_assessment_or_review_but_retains_old_execution_times(self):
        from verification import certificate_acceptance
        original = copy.deepcopy(self.certificate)
        for assessment_at, review_at in (('2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z'),
                                         ('2026-01-01T00:00:00Z', '2026-10-13T11:00:00+09:00')):
            bad = copy.deepcopy(original)
            bad['provenance']['at'] = assessment_at
            bad['review']['at'] = review_at
            for contributor in bad['provenance']['attribution']['contributors']:
                contributor['at'] = assessment_at
            with self.subTest(assessment_at=assessment_at, review_at=review_at):
                with self.assertRaisesRegex(ValueError, 'predates the fixed candidate'):
                    certificate_acceptance({'sample': bad}, self.git)
        valid = copy.deepcopy(original)
        for contributor in valid['provenance']['attribution']['contributors']:
            if contributor['activity'] == 'execution':
                contributor['at'] = '2026-01-01T00:00:00Z'
        self.assertIn('certificate_revision', certificate_acceptance({'sample': valid}, self.git))

    def test_fractional_review_registers_and_consumes_the_exact_unchanged_certificate(self):
        from verification import certificate_acceptance
        self.certificate['review']['at'] = '2026-10-13T08:45:34.861592+00:00'
        saved = json.dumps(self.certificate)
        accepted = certificate_acceptance({'sample': self.certificate}, self.git)
        self.seal()
        self.assertEqual(self.assessment['impact_reuse']['sha256'], canonical_hash(self.certificate))
        self.assertEqual(accepted['certificate_revision'], canonical_hash({'sample': self.certificate}))
        self.assertTrue(self.prior.verify()['stage_complete'])
        self.assertEqual(json.dumps(self.certificate), saved)
        self.assertEqual(self.fixture.documents[f'{RECORD}:{CERT_PATH}']['evidence_certificates']['sample'],
                         self.certificate)

    def test_review_registration_and_consumption_share_the_bounded_timestamp_language(self):
        from verification import certificate_acceptance
        for suffix in ('Z', '.1Z', '.123+00:00', '.123456-00:59'):
            with self.subTest(valid=suffix):
                self.certificate['review']['at'] = '2026-10-13T08:45:34' + suffix
                certificate_acceptance({'sample': self.certificate}, self.git)
                self.seal()
                self.assertTrue(self.prior.verify()['stage_complete'])
        for at in ('2026-02-30T08:45:34.123Z', '2026-10-13T08:45:34.Z',
                   '2026-10-13T08:45:34.1234567Z', '2026-10-13T08:45:34.1+00:60',
                   '2026-10-13T08:45:34.1+99:00', '2026-10-13T08:45:34.1',
                   '2026-10-13 08:45:34.1Z', '2026-10-13T08:45:34.1+0000'):
            with self.subTest(invalid=at):
                self.certificate['review']['at'] = at
                self.seal()
                with self.assertRaises(ValueError):
                    certificate_acceptance({'sample': self.certificate}, self.git)
                with self.assertRaises(ValueError):
                    self.prior.verify()

    def test_microsecond_ordering_keeps_review_facts_and_contributors_in_sequence(self):
        from verification import certificate_acceptance
        self.certificate['provenance']['at'] = '2026-10-13T01:00:00.123456Z'
        composition = self.certificate['provenance']['attribution']['contributors'][-1]
        composition['at'] = '2026-10-13T10:00:00.123456+09:00'
        self.certificate['review']['at'] = '2026-10-13T01:00:00.123456+00:00'
        certificate_acceptance({'sample': self.certificate}, self.git)
        self.seal()
        self.assertTrue(self.prior.verify()['stage_complete'])
        saved = copy.deepcopy(self.certificate)
        self.certificate['review']['at'] = '2026-10-13T01:00:00.123455Z'
        with self.assertRaises(ValueError):
            certificate_acceptance({'sample': self.certificate}, self.git)
        self.certificate = copy.deepcopy(saved)
        self.certificate['provenance']['attribution']['contributors'][2]['at'] = '2026-10-13T01:00:00.123457Z'
        with self.assertRaisesRegex(ValueError, 'out-of-order'):
            certificate_acceptance({'sample': self.certificate}, self.git)
        self.certificate = saved
        self.observed['at'] = '2026-10-13T01:00:00.123456Z'
        for at in ('2026-10-13T01:00:00.123457Z', '2026-09-06T23:59:59.999999Z'):
            with self.subTest(current_fact=at):
                self.assessment['facts']['current']['at'] = at
                self.seal()
                with self.assertRaises(ValueError):
                    self.prior.verify()

    def test_changed_product_and_zip_combine_old_facts_with_current_identity_without_prior_promotion(self):
        saved = copy.deepcopy(self.assessment)
        with self.assertRaisesRegex(ValueError, 'retesting'):
            unchanged_artifact_history(OLD, NEW, self.key, self.git, BASE)
        result = self.prior.verify()
        self.assertTrue(result['stage_complete'])
        self.assertFalse(result['gui_complete'])
        self.assertEqual(result['deferred_cases'], 1)
        self.assertEqual(self.assessment, saved)
        self.assertIn('修正影響を保証ごとに評価', self.render())
        self.assertNotIn(f'{RECORD}:docs/verification/promotion.json', self.fixture.documents)

    def test_structural_provenance_coverage_identity_and_review_fail_closed_in_gate_and_view(self):
        edits = ['missing-original', 'forged-hash', 'writer-review', 'executor-review', 'missing-review',
                 'wrong-artifact', 'old-as-current', 'future-fact', 'unknown-runtime', 'missing-impact',
                 'missing-coverage', 'failure', 'failed-fact', 'unknown-plan', 'changed-case',
                 'changed-human-scope', 'source-execution', 'missing-component', 'wrong-history', 'old-load', 'alias-reviewer']
        for edit in edits:
            with self.subTest(edit=edit):
                self.setUp()
                if edit == 'missing-original':
                    self.certificate.pop('original_file_sha256')
                elif edit == 'forged-hash':
                    self.assessment['source_result_sha256'] = 'f' * 64
                elif edit in ('writer-review', 'executor-review'):
                    self.certificate['review']['reviewer_session'] = (self.certificate['review']['writer_session']
                        if edit == 'writer-review' else self.certificate['review']['executor_sessions'][0])
                elif edit == 'alias-reviewer':
                    self.certificate['review']['reviewer_session'] = 'reviewer-b'
                elif edit == 'missing-review':
                    self.certificate.pop('review')
                elif edit == 'wrong-artifact':
                    self.assessment['facts']['prior']['artifact_sha256'] = 'e' * 64
                elif edit == 'old-as-current':
                    self.assessment['coverage']['artifact_binding'] = ['prior']
                    self.certificate['guarantees']['artifact_binding']['facts'] = ['prior']
                elif edit == 'future-fact':
                    self.assessment['facts']['prior']['at'] = '2099-01-01T00:00:00Z'
                elif edit == 'unknown-runtime':
                    self.certificate['guarantees']['file_integrity']['disposition'] = 'unknown'
                elif edit == 'missing-impact':
                    self.certificate['guarantees']['file_integrity']['impact']['runtime'] = ''
                elif edit == 'missing-coverage':
                    self.assessment['coverage'].pop('file_integrity')
                elif edit == 'failure':
                    self.assessment['unresolved_owned_failures'] = ['Preserved owned failure.']
                elif edit == 'failed-fact':
                    self.assessment['facts']['prior']['status'] = 'fail'
                elif edit == 'unknown-plan':
                    self.fixture.documents[f'{OLD}:{INITIAL_PLAN}']['owner'] = 'unknown source plan'
                elif edit == 'changed-case':
                    self.paths = CASE_PATH
                elif edit == 'changed-human-scope':
                    self.fixture.documents[f'{OLD}:{INITIAL_PLAN}']['human_scope'] = 'changed deferral'
                elif edit == 'source-execution':
                    self.assessment['source_result']['initial_observation']['execution'] = 'shell'
                    self.assessment['source_result_sha256'] = canonical_hash(self.assessment['source_result'])
                elif edit == 'missing-component':
                    self.certificate['guarantees']['file_integrity']['components'] = {}
                elif edit == 'wrong-history':
                    self.certificate['history'] = []
                elif edit == 'old-load':
                    self.assessment['facts']['current']['artifact_sha256'] = 'd' * 64
                self.seal()
                with self.assertRaises(ValueError):
                    self.prior.verify()
                self.assertNotIn('修正影響を保証ごとに評価／', self.render())

    def test_affected_guarantee_requires_current_fact_and_preserves_original(self):
        proof = self.certificate['guarantees']['file_integrity']
        proof['disposition'] = 'fresh'
        self.seal()
        with self.assertRaisesRegex(ValueError, 'affected guarantee'):
            self.prior.verify()
        self.assessment['coverage']['file_integrity'] = ['current']
        proof['facts'] = ['current']
        original = copy.deepcopy(self.assessment['source_result'])
        self.seal()
        self.assertTrue(self.prior.verify()['stage_complete'])
        self.assertEqual(self.assessment['source_result'], original)

    def test_real_git_reverted_edits_stay_in_history_and_nonancestor_is_rejected(self):
        from test_verification import change
        with tempfile.TemporaryDirectory() as folder:
            def git(*args):
                return subprocess.check_output(['git', '-C', folder, *args], text=True,
                                               stderr=subprocess.DEVNULL).strip()
            git('init', '-q')
            git('config', 'maintenance.auto', 'false')
            git('config', 'user.name', 'Synthetic')
            git('config', 'user.email', 'test@example.invalid')
            case = Path(folder) / CASE_PATH
            case.parent.mkdir(parents=True)
            case.write_text(json.dumps(change(36)))
            product = Path(folder) / 'product.txt'
            product.write_text('original')
            git('add', '.')
            git('commit', '-qm', 'original')
            source = git('rev-parse', 'HEAD')
            product.write_text('changed')
            git('commit', '-qam', 'repair')
            product.write_text('original')
            git('commit', '-qam', 'revert')
            candidate = git('rev-parse', 'HEAD')
            history, _ = impact_history(source, candidate, '36:QA-1', git)
            self.assertEqual(len(history), 2)
            self.assertEqual({tuple(e['paths']) for e in history}, {('product.txt',)})
            with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                impact_history(candidate, source, '36:QA-1', git)
            case.write_text(json.dumps(dict(change(36), reason='changed metadata')))
            git('add', '.')
            git('commit', '-qm', 'case changed')
            with self.assertRaisesRegex(ValueError, 'Case contract'):
                impact_history(source, git('rev-parse', 'HEAD'), '36:QA-1', git)

    def nested(self):
        fact = copy.deepcopy(self.assessment['facts']['prior'])
        aggregate = {'status': 'pending', 'head': OLD, 'artifact_sha256': 'd' * 64,
                     'reason': 'Original missing purpose remains a historical pending verdict.'}
        return {'source_candidate': OLD, 'source_result': aggregate,
                'source_result_sha256': canonical_hash(aggregate), 'source_record_evidence': 'Sealed original.',
                'facts': {'executed': fact}, 'coverage': {k: ['executed'] for k in fact['responsibility_evidence']},
                'missing_requirements': [], 'unresolved_owned_failures': []}

    def test_valid_original_composition_survives_but_nested_missing_or_failure_is_rejected(self):
        self.assessment['source_result']['initial_observation']['responsibility_assessment'] = self.nested()
        self.assessment['source_result_sha256'] = canonical_hash(self.assessment['source_result'])
        self.seal()
        self.assertTrue(self.prior.verify()['stage_complete'])
        for field in ('missing_requirements', 'unresolved_owned_failures'):
            self.assessment['source_result']['initial_observation']['responsibility_assessment'][field] = ['Unmet owned purpose.']
            self.assessment['source_result_sha256'] = canonical_hash(self.assessment['source_result'])
            self.seal()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.prior.verify()
            self.assessment['source_result']['initial_observation']['responsibility_assessment'][field] = []

    def test_previously_pending_case_uses_successful_shared_purpose_and_fresh_missing_fact(self):
        # A fact is qualified by its real provenance/purpose, not a whole-Case PASS label.
        self.assessment['source_result'] = {'status': 'pending', 'head': OLD, 'artifact_sha256': 'd' * 64,
            'reason': 'Target Case never passed; shared consumer purpose succeeded in a separate original Case.'}
        self.assessment['facts']['prior']['evidence'] = 'Successful original consumer purpose shared from another Case.'
        self.assessment['source_result_sha256'] = canonical_hash(self.assessment['source_result'])
        self.assessment['coverage']['file_integrity'] = ['current']
        proof = self.certificate['guarantees']['file_integrity']
        proof.update(facts=['current'], disposition='fresh')
        original = copy.deepcopy(self.assessment['source_result'])
        self.seal()
        self.assertTrue(self.prior.verify()['stage_complete'])
        self.assertEqual(self.assessment['source_result'], original)
        self.assessment['missing_requirements'] = ['Fresh missing fact not obtained.']
        self.seal()
        with self.assertRaises(ValueError):
            self.prior.verify()

    def test_nested_impact_fact_is_rejected_while_original_plain_and_ordinary_facts_remain_supported(self):
        nested = copy.deepcopy(self.assessment)
        self.assessment['facts']['current']['responsibility_assessment'] = nested
        self.seal()
        with self.assertRaisesRegex(ValueError, 'Nested impact reuse'):
            self.prior.verify()

    def test_registration_rejects_incomplete_certificate_or_unavailable_git_identity(self):
        from verification import certificate_acceptance
        self.assertIn('certificate_revision', certificate_acceptance({'sample': self.certificate}, self.git))
        for field in ('key', 'candidate', 'artifact_sha256', 'source_candidate', 'source_artifact_sha256',
                      'history', 'guarantees', 'original_file_sha256', 'original_result_sha256',
                      'original_fact_sha256', 'assessment_sha256', 'observation_sha256'):
            with self.subTest(field=field):
                bad = copy.deepcopy(self.certificate); bad.pop(field)
                with self.assertRaises(ValueError):
                    certificate_acceptance({'sample': bad}, self.git)
        bad = copy.deepcopy(self.certificate); bad['guarantees']['file_integrity']['components'][COMPONENT]['source_blob'] = 'f' * 40
        with self.assertRaises(ValueError):
            certificate_acceptance({'sample': bad}, self.git)
        def unavailable(*args):
            raise subprocess.CalledProcessError(1, ['git'])
        with self.assertRaises(ValueError):
            certificate_acceptance({'sample': self.certificate}, unavailable)

    def test_foreign_issue_certificates_cannot_bypass_tooling_or_develop_registration(self):
        import test_verification as tv
        from verification import verify_pr
        for mode in ('tooling', 'develop'):
            fixture = tv.AcceptanceTests(); fixture.setUp()
            fixture.pr['body'] = fixture.pr['body'].replace('Integration: promotion', 'Integration: ' + mode)
            fixture.pr['base']['ref'] = 'develop' if mode == 'develop' else 'main'
            fixture.documents[f'{HEAD}:{CERT_PATH}'] = {'evidence_certificates': {'forged': self.certificate}}
            original = fixture.git
            def git(*args):
                if args[0] == 'diff':
                    return CERT_PATH
                return original(*args)
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, 'dedicated current-Issue'):
                verify_pr(fixture.pr, fixture.api, git)
        # An exact already-adopted certificate may be retained without becoming a new registration.
        fixture.pr['body'] = fixture.pr['body'].replace('Integration: develop', 'Integration: tooling')
        fixture.pr['base']['ref'] = 'main'
        fixture.documents[f'{BASE}:{CERT_PATH}'] = copy.deepcopy(fixture.documents[f'{HEAD}:{CERT_PATH}'])
        self.assertEqual(verify_pr(fixture.pr, fixture.api, git)['mode'], 'tooling')

    def test_foreign_certificate_in_reverted_history_cannot_hide_in_net_zero_diff(self):
        import test_verification as tv
        from verification import verify_pr
        fixture = tv.AcceptanceTests(); fixture.setUp()
        fixture.pr['body'] = fixture.pr['body'].replace('Integration: promotion', 'Integration: tooling')
        fixture.documents[f'{NEW}:{CERT_PATH}'] = {'evidence_certificates': {'forged': self.certificate}}
        original = fixture.git
        def git(*args):
            if args == ('rev-list', HEAD, '--not', BASE):
                return NEW + '\n' + HEAD
            if args[:2] == ('rev-list', '--parents'):
                return args[-1] + ' ' + (BASE if args[-1] == NEW else NEW)
            if args[0] == 'diff':
                return 'docs/verification/changes/issue-35.json' if args[-2:] == (BASE, HEAD) else CERT_PATH
            return original(*args)
        with self.assertRaisesRegex(ValueError, 'dedicated current-Issue'):
            verify_pr(fixture.pr, fixture.api, git)

    def test_certificate_hash_record_and_trusted_main_cannot_be_self_asserted(self):
        for edit in ('tamper', 'missing', 'untrusted'):
            self.setUp()
            if edit == 'tamper':
                self.fixture.documents[f'{RECORD}:{CERT_PATH}']['evidence_certificates']['sample']['review']['evidence'] = 'tampered'
            elif edit == 'missing':
                self.fixture.documents.pop(f'{RECORD}:{CERT_PATH}')
            else:
                original = self.fixture.git
                def denied(*args):
                    if args == ('merge-base', '--is-ancestor', RECORD, BASE):
                        raise subprocess.CalledProcessError(1, ['git'])
                    return original(*args)
                self.fixture.git = denied
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                self.prior.verify()

    def test_touched_relevant_component_requires_explicit_reviewed_equivalence_even_after_revert(self):
        self.paths += '\n' + COMPONENT
        self.certificate['history'] = impact_history(OLD, NEW, self.key, self.git)[0]
        self.seal()
        with self.assertRaisesRegex(ValueError, 'reverted relevant'):
            self.prior.verify()
        for proof in self.certificate['guarantees'].values():
            if proof['disposition'] != 'fresh':
                proof['disposition'] = 'equivalent'
                proof['components'][COMPONENT]['equivalence_evidence'] = 'Reviewed full intermediate history and equivalent behavior.'
        self.seal()
        self.assertTrue(self.prior.verify()['stage_complete'])


class CertificateOriginTests(unittest.TestCase):
    def accept_certificates(self, certificates):
        from verification import certificate_acceptance
        return certificate_acceptance(certificates, self.validation_git)

    def setUp(self):
        import test_repair_private as tp
        import test_agent_loop as tal
        from verification import certificate_acceptance
        self.fixture = tp.EvidenceTests()
        self.fixture.setUp()
        self.fixture.at = '2026-10-13T00:00:00Z'
        self.fixture.observed['at'] = self.fixture.at
        for contributor in self.fixture.items:
            if contributor['activity'] != 'execution': contributor['at'] = self.fixture.at
        for origin in self.fixture.bundle['origins']:
            if origin['activity'] != 'execution': origin['at'] = self.fixture.at
        self.addCleanup(self.fixture.doCleanups)
        self.bound = self.fixture.bound
        self.report = tal.report()
        self.report.update(certificate_revision='e' * 64, certificate_assessment='Original receipts, actors, purposes and coverage examined.')
        sessions = list(map(session_fingerprint, self.fixture.sessions))
        impact = ImpactEvidenceTests(); impact.setUp()
        self.validation_git = impact.git
        self.certificate_template = copy.deepcopy(impact.certificate)
        certificate = dict(copy.deepcopy(self.certificate_template), provenance=self.fixture.observed, review={
            'status': 'approved', 'writer_session': sessions[0], 'executor_sessions': [sessions[1]],
            'reviewer_session': session_fingerprint(self.report['session']), 'at': self.fixture.at,
            'evidence': 'Synthetic fixed review.'})
        self.accepted = dict(self.accept_certificates({'sample': certificate}), allowed=True)
        self.report['certificate_revision'] = self.accepted['certificate_revision']
        self.fixture.bundle.pop('repair_revision')
        self.fixture.bundle['certificate_revision'] = self.accepted['certificate_revision']
        self.fixture.seal()

    def evidence(self):
        from agent_worker import private_repair_evidence
        packet = private_repair_evidence(self.fixture.path, self.accepted, self.bound,
                                         revision_field='certificate_revision')
        self.accepted['certificate_evidence_sha256'] = packet['bundle_sha256']
        self.report['certificate_evidence_sha256'] = packet['bundle_sha256']
        return packet

    def test_private_origin_adapter_binds_actual_reviewer_and_sealed_contributors(self):
        from handoff_registry import issue_review_origin
        from agent_policy import validate_review, next_action
        packet = self.evidence()
        issued = issue_review_origin(self.fixture.root, self.report, self.bound, self.report['session'], 'manual',
                                     packet['private_paths'], [o['session'] for o in packet['origins']])
        validate_review(issued, self.bound, self.accepted, self.fixture.root)
        self.assertEqual(next_action({'binding': self.bound, 'review': issued}, self.bound, 'success', False,
                                    self.accepted, self.fixture.root), 'merge')
        for edit in ('missing-origin', 'unknown-reviewer', 'self-review', 'stale-proof'):
            bad = dict(issued)
            if edit == 'missing-origin':
                bad.pop('origin_receipt')
            elif edit == 'unknown-reviewer':
                bad['session'] = '00000000-0000-4000-8000-000000000088'
            elif edit == 'self-review':
                bad['session'] = self.fixture.sessions[0]
            else:
                bad['certificate_evidence_sha256'] = 'f' * 64
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                validate_review(bad, self.bound, self.accepted, self.fixture.root)

    def test_forged_or_missing_contributor_origins_are_rejected(self):
        original = copy.deepcopy(self.fixture.bundle)
        for edit in ('alias', 'missing', 'wrong-session', 'changed-receipt'):
            self.fixture.bundle = copy.deepcopy(original)
            if edit == 'alias':
                self.fixture.bundle['origins'][0]['session'] = 'writer-a'
            elif edit == 'missing':
                self.fixture.bundle['origins'].pop()
            elif edit == 'wrong-session':
                self.fixture.bundle['origins'][0]['session'] = self.fixture.sessions[1]
            else:
                self.fixture.bundle['receipts'][0]['sha256'] = 'b' * 64
            self.fixture.seal()
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                self.evidence()

    def test_consistent_cross_certificate_claims_share_one_private_origin(self):
        from verification import certificate_acceptance
        original = copy.deepcopy(self.accepted['certificate_provenance']['sample'])
        review = {'status': 'approved', 'writer_session': original['attribution']['writer'],
                  'executor_sessions': [session_fingerprint(self.fixture.sessions[1])],
                  'reviewer_session': session_fingerprint(self.report['session']), 'at': self.fixture.at,
                  'evidence': 'Synthetic independent review.'}
        certificates = {key: dict(copy.deepcopy(self.certificate_template), provenance=copy.deepcopy(original), review=copy.deepcopy(review))
                        for key in ('first', 'last')}
        self.accepted.update(self.accept_certificates(certificates))
        self.fixture.bundle['certificate_revision'] = self.accepted['certificate_revision']
        self.fixture.seal()
        self.evidence()

    def test_cross_certificate_session_activity_time_and_parent_child_claims_fail_in_both_gates(self):
        from verification import certificate_acceptance
        from agent_worker import private_repair_evidence
        for edit in ('session', 'activity', 'time', 'parent-child'):
            with self.subTest(edit=edit):
                self.setUp()
                original = copy.deepcopy(self.accepted['certificate_provenance']['sample'])
                bad = copy.deepcopy(original)
                item = bad['attribution']['contributors'][1]
                if edit in ('session', 'parent-child'):
                    item['session'] = session_fingerprint('00000000-0000-4000-8000-000000000077')
                if edit == 'activity':
                    extra = copy.deepcopy(item); extra['receipt_locator'] = '/other-execution'
                    bad['attribution']['contributors'].append(extra)
                    item['activity'] = 'assessment'
                elif edit == 'time':
                    item['at'] = '2025-12-31T23:59:59Z'
                elif edit == 'parent-child':
                    item['receipt_locator'] += '/child'
                def certificate(provenance):
                    return dict(copy.deepcopy(self.certificate_template), provenance=provenance, review={
                        'status': 'approved', 'writer_session': provenance['attribution']['writer'],
                        'executor_sessions': sorted({c['session'] for c in provenance['attribution']['contributors']
                                                     if c['activity'] == 'execution'}),
                        'reviewer_session': session_fingerprint(self.report['session']),
                        'at': self.fixture.at, 'evidence': 'Synthetic independent review.'})
                # Each certificate alone is well formed; the complete packet contradicts one observation.
                self.accept_certificates({'first': certificate(bad)})
                with self.assertRaisesRegex(ValueError, 'Conflicting origin'):
                    self.accept_certificates({'first': certificate(bad), 'last': certificate(original)})
                self.accepted['certificate_provenance'] = {'first': bad, 'last': original}
                with self.assertRaisesRegex(ValueError, 'Conflicting origin'):
                    private_repair_evidence(self.fixture.path, self.accepted, self.bound,
                                           revision_field='certificate_revision')

    def test_certificate_bundle_preflight_rebind_registry_and_cleanup_preserve_kind_without_launch(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        import agent_loop as loop_module
        from handoff_registry import register, resolve
        source = self.fixture.root / 'source'
        loop = SimpleNamespace(acceptance=lambda pr, root: self.accepted, storage=self.fixture.root / 'storage')
        pr = {'number': 246, 'head': {'sha': self.bound['head']}, 'base': {'sha': self.bound['base']}}
        with self.assertRaisesRegex(ValueError, 'private regular bundle'):
            loop_module.preflight_repair_bundle(loop, pr, source, None)
        path, paths, records, kind = loop_module.preflight_repair_bundle(loop, pr, source, self.fixture.path)
        self.assertEqual(kind, 'certificate_revision')
        public = register(loop.storage, {'pr': 246}, source, 'synthetic-host', path, paths, records, evidence_kind=kind)
        handoff = resolve(loop.storage, public, 'synthetic-host')
        self.assertEqual(handoff['evidence_kind'], kind)
        # Same actual helper used for rebind; no worker or coordinator dispatch.
        rebound = loop_module.preflight_repair_bundle(loop, pr, source, handoff['repair_evidence'])
        self.assertEqual(rebound[3], kind)
        handoff.update(branch='codex/246-fixture')
        with patch.object(loop_module, 'read_repair_bundle', wraps=loop_module.read_repair_bundle) as reader, \
                patch.object(loop_module, 'require_retained_evidence', side_effect=RuntimeError('stop before resource mutation')):
            with self.assertRaisesRegex(RuntimeError, 'stop before resource mutation'):
                loop_module.Loop._cleanup_resources(loop, pr, handoff)
            reader.assert_called_once_with(str(path), kind)

    def test_manual_binder_uses_actual_environment_session_and_never_model_alias(self):
        import os
        from unittest.mock import patch
        from agent_worker import main
        from agent_policy import validate_review
        root = self.fixture.root
        report = root / 'manual.json'; report.write_text(json.dumps(self.report)); report.chmod(0o600)
        accepted = root / 'accepted.json'; accepted.write_text(json.dumps(self.accepted))
        bound = root / 'binding.json'; bound.write_text(json.dumps(self.bound))
        output = root / 'issued.json'
        args = ['agent_worker.py', '--report', str(report), '--output', str(output),
                '--repair-evidence', str(self.fixture.path), '--acceptance', str(accepted),
                '--storage', str(root), '--binding', str(bound)]
        with patch.dict(os.environ, CODEX_THREAD_ID=self.report['session']), patch('sys.argv', args):
            main()
        issued = json.loads(output.read_text())
        self.evidence()
        validate_review(issued, self.bound, self.accepted, root)
        args[args.index(str(output))] = str(root / 'forged.json')
        with patch.dict(os.environ, CODEX_THREAD_ID=self.fixture.sessions[0]), patch('sys.argv', args), self.assertRaises(ValueError):
            main()



class PromotionTreeTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Synthetic fixture')
        prior = tr.ResponsibilityRevisionTests(); prior.setUp()
        self.write(CASE_PATH, json.dumps(prior.fixture.documents[f'{NEW}:{CASE_PATH}']))
        self.commit('root')
        root = self.git('rev-parse', 'HEAD')
        plan = copy.deepcopy(prior.policy); plan['required_ancestor'] = root
        plan['responsibility_revision']['previous_plan_sha256'] = canonical_hash(
            {k: v for k, v in plan.items() if k != 'responsibility_revision'})
        self.write(INITIAL_PLAN, json.dumps(plan))
        self.write('scripts/workflow/verification.py', 'old policy')
        self.write(COMPONENT, 'old product')
        self.commit('common')
        self.common = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '-b', 'candidate')
        self.write(COMPONENT, 'repaired product')
        self.commit('tested repair')
        self.candidate = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '-b', 'trusted-main', self.common)
        self.write('scripts/workflow/verification.py', 'reviewed policy')
        self.write('AGENTS.md', 'reviewed instructions')
        self.write('.github/workflows/ci.yml', 'mixed build policy')
        self.write(CERT_PATH, json.dumps({'schema': 1, 'issue': 246, 'gui_required': False,
                   'reason': 'Certificate only.', 'cli_checks': ['Synthetic'], 'cases': [],
                   'evidence_certificates': {'sample': {'candidate': self.candidate,
                       'key': '36:QA-1-EMU', 'artifact_sha256': 'e' * 64}}}))
        self.commit('adopted main policy and certificate')
        self.base = self.git('rev-parse', 'HEAD')
        certificate = json.loads(self.git('show', f'{self.base}:{CERT_PATH}'))['evidence_certificates']['sample']
        self.promotion = {'results': {'36:QA-1-EMU': {'head': self.candidate, 'artifact_sha256': 'e' * 64,
            'initial_observation': {'responsibility_assessment': {'impact_reuse': {
                'commit': self.base, 'path': CERT_PATH, 'id': 'sample', 'sha256': canonical_hash(certificate)}}}}}}
        from verification import promotion_certificates, promotion_policy_blobs
        self.carried = promotion_certificates(self.promotion, self.base, self.candidate, self.git)
        self.carried.update(promotion_policy_blobs(self.base, self.candidate, self.git))
        self.allowed = set(self.carried) | {'docs/verification/promotion.json'}
        self.git('checkout', '-q', 'candidate')
        # A real merge retains tested product bytes and copies only the exact permitted main blobs.
        self.git('merge', '--no-commit', '--no-ff', '-s', 'ours', self.base)
        for path in self.carried:
            self.write(path, self.git('show', f'{self.base}:{path}'))
        self.write('docs/verification/promotion.json', json.dumps(self.promotion))
        self.commit('promotion metadata')

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, text=True, stderr=subprocess.PIPE).strip()

    def write(self, path, text):
        target = self.root / path; target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message):
        self.git('add', '.')
        self.git('commit', '-q', '-m', message)

    def verify(self):
        from verification import promotion_metadata_history
        promotion_metadata_history(self.base, self.candidate, self.git('rev-parse', 'HEAD'),
                                   self.allowed, self.carried, self.git)

    def test_real_divergent_tree_retains_reviewed_policy_and_certificate_without_product_rebuild(self):
        self.verify()
        self.assertEqual(set(self.carried), {CERT_PATH, 'scripts/workflow/verification.py', 'AGENTS.md'})
        self.assertEqual(self.git('diff', '--name-only', self.candidate, 'HEAD', '--', COMPONENT), '')
        self.assertNotIn('.github/workflows/ci.yml', self.carried)

    def test_unreviewed_policy_product_unknown_and_reverts_remain_blocked(self):
        clean = self.git('rev-parse', 'HEAD')
        for path in ('scripts/workflow/verification.py', COMPONENT, 'unknown.bin', '.github/workflows/ci.yml', CERT_PATH):
            with self.subTest(path=path):
                self.git('reset', '--hard', clean)
                self.write(path, 'unreviewed alteration')
                self.commit('unreviewed')
                altered = self.git('rev-parse', 'HEAD')
                with self.assertRaises(ValueError):
                    self.verify()
                self.git('revert', '--no-edit', altered)
                with self.assertRaises(ValueError):
                    self.verify()

    def test_pre_squash_direct_sibling_keeps_both_real_histories_and_rejects_changed_product_or_case_metadata(self):
        from verification import impact_history
        source = self.candidate
        self.git('checkout', '-q', '-b', 'squashed', self.common)
        self.write(COMPONENT, 'repaired product')
        record = {'schema': 1, 'issue': 244, 'gui_required': False, 'reason': 'Repair metadata only.',
                  'cli_checks': ['Synthetic'], 'cases': []}
        self.write('docs/verification/changes/issue-244.json', json.dumps(record))
        self.commit('squashed same product with cases-empty metadata')
        candidate = self.git('rev-parse', 'HEAD')
        history, case_hash = impact_history(source, candidate, '36:QA-1-EMU', self.git)
        self.assertEqual({e['commit'] for e in history}, {source, candidate})
        self.assertTrue(all(e['parent'] == self.common for e in history))
        self.assertTrue(all(COMPONENT in e['paths'] for e in history))
        for changed in ('product', 'unknown', 'case-metadata'):
            with self.subTest(changed=changed):
                self.git('checkout', '-q', '-B', 'invalid-squash', self.common)
                self.write(COMPONENT, 'different product' if changed == 'product' else 'repaired product')
                if changed == 'unknown':
                    self.write('unknown.bin', 'unreviewed')
                elif changed == 'case-metadata':
                    bad = dict(record, gui_required=True, cases=[{'id': 'NEW-CASE'}])
                    self.write('docs/verification/changes/issue-244.json', json.dumps(bad))
                self.commit('invalid sibling endpoint')
                with self.assertRaises(ValueError):
                    impact_history(source, self.git('rev-parse', 'HEAD'), '36:QA-1-EMU', self.git)
        with self.assertRaises(ValueError):
            impact_history(source, self.base, '36:QA-1-EMU', self.git)

    def test_loop_acceptance_reader_fetches_missing_real_sibling_then_reuses_cached_history(self):
        import agent_loop as al
        source = self.candidate
        self.git('checkout', '-q', '-b', 'reader-sibling', self.common)
        self.write(COMPONENT, 'repaired product')
        self.write('docs/verification/changes/issue-244.json', json.dumps({
            'schema': 1, 'issue': 244, 'gui_required': False, 'reason': 'Metadata only.',
            'cli_checks': ['Synthetic'], 'cases': []}))
        self.commit('same product sibling')
        candidate = self.git('rev-parse', 'HEAD')
        reader = self.root / 'reader'; reader.mkdir()
        run = subprocess.check_output
        run(['git', 'init', '-q'], cwd=reader)
        run(['git', 'fetch', '-q', '--no-tags', str(self.root), candidate], cwd=reader, stderr=subprocess.PIPE)
        run(['git', 'remote', 'add', 'origin', 'https://github.com/shinma06/android-replay-buffer'], cwd=reader)
        with self.assertRaises(subprocess.CalledProcessError):
            run(['git', 'cat-file', '-t', source], cwd=reader, stderr=subprocess.PIPE)
        loop = al.Loop(storage=self.root / 'storage')
        fetches = []
        def transport(args, **kwargs):
            if args[:4] == ['git', 'fetch', '--no-tags', 'origin']:
                fetches.append(args)
                # Synthetic transport only: preserve real Git exits and the requested immutable SHA.
                args = ['git', 'fetch', '--no-tags', str(self.root), *args[4:]]
            return run(args, **kwargs)
        def validate(pr, api, git):
            history, case_hash = impact_history(source, pr['candidate'], '36:QA-1-EMU', git)
            return {'history': history, 'case_hash': case_hash}
        with patch.object(al, 'verify_pr', side_effect=validate), patch.object(al.subprocess, 'check_output', side_effect=transport):
            accepted = loop.acceptance({'candidate': candidate}, reader)
            self.assertTrue(accepted['allowed'])
            self.assertEqual({e['commit'] for e in accepted['history']}, {source, candidate})
            self.assertEqual(loop.acceptance({'candidate': candidate}, reader), accepted)
            self.assertEqual(fetches, [['git', 'fetch', '--no-tags', 'origin', source]])
            self.assertFalse(loop.acceptance({'candidate': self.base}, reader)['allowed'])

    def test_changed_effective_contract_cannot_carry_main_policy(self):
        from verification import promotion_policy_blobs
        self.git('checkout', '-q', 'trusted-main')
        plan = json.loads(self.git('show', f'{self.base}:{INITIAL_PLAN}'))
        plan['human_scope'] += ' changed'
        self.write(INITIAL_PLAN, json.dumps(plan)); self.commit('changed contract')
        with self.assertRaises(ValueError):
            promotion_policy_blobs(self.git('rev-parse', 'HEAD'), self.candidate, self.git)

class CertificateArchivePromotionTests(unittest.TestCase):
    git = PromotionTreeTests.git
    write = PromotionTreeTests.write
    commit = PromotionTreeTests.commit

    def setUp(self):
        from test_verification import change, observation
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Synthetic fixture')
        self.git('remote', 'add', 'origin', str(self.root))
        self.write(CASE_PATH, json.dumps(change(36)))
        self.write(COMPONENT, 'old product')
        self.commit('common')
        self.old = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '-b', 'candidate')
        self.write(COMPONENT, 'tested new product')
        self.commit('new candidate')
        self.candidate = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '-b', 'trusted-main', self.old)
        self.active_path = 'docs/verification/changes/issue-255.json'
        self.certificates = {}
        for issue, candidate, artifact in ((246, self.old, 'd' * 64), (255, self.candidate, 'e' * 64)):
            certificate = {'candidate': candidate, 'key': '36:QA-1', 'artifact_sha256': artifact}
            self.certificates[issue] = certificate
            self.write(f'docs/verification/changes/issue-{issue}.json', json.dumps(
                dict(change(issue, False), evidence_certificates={'sample': certificate})))
        self.commit('two adopted certificate generations')
        self.base = self.git('rev-parse', 'HEAD')
        result = dict(observation(), head=self.candidate, responsibility_assessment={'impact_reuse': {
            'commit': self.base, 'path': self.active_path, 'id': 'sample',
            'sha256': canonical_hash(self.certificates[255])}})
        self.promotion = {'schema': 1, 'base': self.base, 'candidate': self.candidate,
                          'artifact_sha256': 'e' * 64, 'changes': [{'commit': self.candidate, 'pr': 99}],
                          'results': {'36:QA-1': result}}
        self.git('checkout', '-q', 'candidate')
        self.git('merge', '--no-commit', '--no-ff', '-s', 'ours', self.base)
        for path in (CERT_PATH, self.active_path):
            self.write(path, self.git('show', f'{self.base}:{path}'))
        self.write('docs/verification/changes/issue-87.json', json.dumps(change(87, False)))
        self.write('docs/verification/promotion.json', json.dumps(self.promotion))
        self.commit('exact archives and new active reference')

    def verify(self):
        from test_agent_loop import pr_data
        from verification import verify_pr
        pr = pr_data()
        pr['head'].update(sha=self.git('rev-parse', 'HEAD'), ref='codex/87-promotion')
        pr['base'].update(sha=self.base, ref='main')
        pr['body'] = ('Issue: #87\nGUI: not-required\nIntegration: promotion\n'
                      'Verification: docs/verification/changes/issue-87.json')
        source = copy.deepcopy(pr)
        source.update(merged=True, merge_commit_sha=self.candidate)
        source['base'].update(ref='develop', sha=self.old)
        source['head'].update(ref='codex/36-source', sha=self.candidate)
        source['body'] = ('Issue: #36\nGUI: required\nIntegration: develop\n'
                          'Verification: docs/verification/changes/issue-36.json')
        def api(path):
            if path == 'git/ref/heads/develop':
                return {'object': {'sha': self.candidate}}
            if path == 'pulls/99':
                return source
            raise AssertionError(path)
        # Isolate real carry/history from plan and substantive fact proofs tested above.
        with patch('verification.initial_stage_plan', return_value=None), patch('verification.validate_fact_assessment'):
            return verify_pr(pr, api, self.git)

    def test_full_gate_preserves_two_generations_and_refuses_archive_mutations_and_old_consumption(self):
        self.assertEqual(self.verify()['cases'], 1)
        clean = self.git('rev-parse', 'HEAD')
        for mutation in ('delete', 'certificate', 'record', 'unadopted', 'old-reference'):
            with self.subTest(mutation=mutation):
                self.git('reset', '--hard', clean)
                if mutation == 'delete':
                    (self.root / CERT_PATH).unlink()
                elif mutation in ('certificate', 'record'):
                    record = json.loads((self.root / CERT_PATH).read_text())
                    if mutation == 'certificate':
                        record['evidence_certificates']['sample']['candidate'] = self.candidate
                    else:
                        record['reason'] = 'Archive bytes changed outside certificate dictionary.'
                    self.write(CERT_PATH, json.dumps(record))
                elif mutation == 'unadopted':
                    self.write('docs/verification/changes/issue-256.json', '{}')
                else:
                    manifest = copy.deepcopy(self.promotion)
                    manifest['results']['36:QA-1']['responsibility_assessment']['impact_reuse'].update(
                        path=CERT_PATH, sha256=canonical_hash(self.certificates[246]))
                    self.write('docs/verification/promotion.json', json.dumps(manifest))
                self.commit('invalid promotion')
                invalid = self.git('rev-parse', 'HEAD')
                with self.assertRaises(ValueError):
                    self.verify()
                if mutation != 'old-reference':
                    self.git('revert', '--no-edit', invalid)
                    with self.assertRaises(ValueError):
                        self.verify()

    def test_archive_only_promotion_retains_records_without_claiming_reused_evidence(self):
        del self.promotion['results']['36:QA-1']['responsibility_assessment']
        self.write('docs/verification/promotion.json', json.dumps(self.promotion))
        self.commit('ordinary observation without certificate consumption')
        self.assertEqual(self.verify()['cases'], 1)


if __name__ == '__main__':
    unittest.main()
