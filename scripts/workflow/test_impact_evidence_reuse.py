"""Synthetic certificates exercise shared gates; none are real product acceptance."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_agent_loop import BASE, NEW
from test_observation_reuse import OLD, RECORD, CASE_PATH
from test_responsibility_revision import ResponsibilityRevisionTests
from verification import (IMPACT_DIMENSIONS, INITIAL_PLAN, canonical_hash, impact_history,
                          render_queue, unchanged_artifact_history)

CERT_PATH = 'docs/verification/changes/issue-246.json'
COMPONENT = 'plugin/src/main/kotlin/Video.kt'
REPAIR = 'plugin/src/main/kotlin/AppHistory.kt'


class ImpactEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.prior = ResponsibilityRevisionTests()
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
            'review': {'status': 'approved', 'reviewer_session': 'independent-synthetic',
                       'writer_session': 'writer-synthetic', 'executor_sessions': ['executor-synthetic'],
                       'at': '2026-10-13T11:00:00+09:00', 'evidence': 'Fixed independent synthetic report.'},
            'guarantees': {k: {'facts': ids, 'disposition': 'fresh' if k == 'artifact_binding' else 'unaffected',
                              'impact': {d: 'Concrete synthetic comparison: ' + d for d in IMPACT_DIMENSIONS},
                              'components': {COMPONENT: {'source_blob': '0' * 40, 'candidate_blob': '0' * 40}}}
                           for k, ids in self.assessment['coverage'].items()}}
        self.seal()

    def git(self, *args):
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
        self.assessment['impact_reuse'] = {
            'commit': RECORD, 'path': CERT_PATH, 'id': 'sample', 'sha256': canonical_hash(self.certificate)}

    def render(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'case.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:{CASE_PATH}']))
            return render_queue([path], self.prior.initial.manifest, self.git)

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
                 'changed-human-scope', 'source-execution', 'missing-component', 'wrong-history', 'old-load']
        for edit in edits:
            with self.subTest(edit=edit):
                self.setUp()
                if edit == 'missing-original':
                    self.certificate.pop('original_file_sha256')
                elif edit == 'forged-hash':
                    self.assessment['source_result_sha256'] = 'f' * 64
                elif edit in ('writer-review', 'executor-review'):
                    self.certificate['review']['reviewer_session'] = edit.split('-')[0] + '-synthetic'
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
            with self.assertRaises(subprocess.CalledProcessError):
                impact_history(candidate, source, '36:QA-1', git)
            case.write_text(json.dumps(dict(change(36), reason='changed metadata')))
            git('add', '.')
            git('commit', '-qm', 'case changed')
            with self.assertRaisesRegex(ValueError, 'Case contract'):
                impact_history(source, git('rev-parse', 'HEAD'), '36:QA-1', git)

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


if __name__ == '__main__':
    unittest.main()
