"""Amended methods preserve observations; synthetic results are never product passes."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from test_agent_loop import BASE, NEW
import test_initial_acceptance as ti
from test_verification import observation
from verification import (INITIAL_PLAN, canonical_hash, effective_initial_entry,
                          effective_responsibility_case, initial_stage_plan, render_queue,
                          validate_candidate_result, validate_responsibility_policy)


class ResponsibilityRevisionTests(unittest.TestCase):
    def setUp(self):
        self.initial = ti.InitialAcceptanceTests()
        self.initial.setUp()
        self.fixture = self.initial.fixture
        self.original = copy.deepcopy(self.initial.policy)
        self.old_result = copy.deepcopy(self.initial.manifest['results']['36:QA-1-EMU'])
        self.policy = copy.deepcopy(self.original)
        self.policy['responsibility_revision'] = {
            'issue': 182, 'previous_plan_sha256': canonical_hash(self.original),
            'common_requirements': ['Keep all owned data and failures.'],
            'groups': {'fixture': {'cases': list(self.original['cases']),
                'disposition': '置換', 'purpose': 'Validate our writer.',
                'methods': 'Full file validation plus representative integration.',
                'excluded': 'Every external player seek implementation.',
                'evidence_keys': ['file_integrity']}},
            'overrides': {'36:QA-1-EMU': {'case': {'expected': 'All our samples remain correct.'},
                                        'agent_requirements': ['Validate every owned sample.']}}}
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.initial.policy = self.policy
        self.initial.record_results()
        self.observed = self.initial.manifest['results']['36:QA-1-EMU']['initial_observation']
        self.proof(self.observed)

    def proof(self, observed):
        observed['responsibility_revision'] = canonical_hash(self.policy['responsibility_revision'])
        observed['responsibility_evidence'] = {name: 'Executed evidence / fixed artifact / reviewed coverage'
            for name in ('contract_coverage', 'artifact_binding', 'failure_attribution', 'file_integrity')}

    def verify(self):
        return self.initial.verify()

    def test_trusted_amendment_applies_to_unchanged_candidate_without_rebuild(self):
        self.assertTrue(self.verify()['stage_complete'])
        self.assertEqual(self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'], self.original)
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'][0]
        effective = effective_responsibility_case(self.policy, '36:QA-1-EMU', case)
        self.assertIn('All our samples', effective['expected'])
        self.assertNotEqual(effective['expected'], case['expected'])
        self.assertEqual(effective['gpt'], case['gpt'])
        self.assertEqual(effective['required_execution'], 'computer_use')

    def test_missing_stale_or_wrong_evidence_and_old_stage_revision_rejected(self):
        saved = copy.deepcopy(self.observed)
        for patch in ({'responsibility_revision': 'f' * 64}, {'responsibility_evidence': {}},
                      {'responsibility_evidence': dict(saved['responsibility_evidence'], extra='not needed')},
                      {'execution': 'shell'}):
            self.observed.clear()
            self.observed.update(copy.deepcopy(saved))
            self.observed.update(patch)
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                self.verify()
        self.initial.manifest['results']['36:QA-1-EMU'] = self.old_result
        with self.assertRaises(ValueError):
            self.verify()

    def test_case_omission_duplicate_original_edit_and_untrusted_candidate_rejected(self):
        for edit in ('missing', 'duplicate', 'original', 'override', 'real'):
            p = copy.deepcopy(self.policy)
            if edit == 'missing':
                p['responsibility_revision']['groups']['fixture']['cases'].pop()
            elif edit == 'duplicate':
                p['responsibility_revision']['groups']['fixture']['cases'].append('36:QA-1-EMU')
            elif edit == 'original':
                p['cases']['36:QA-1-EMU']['sources'].pop()
            elif edit == 'override':
                p['responsibility_revision']['overrides']['36:QA-1-EMU']['case']['required_execution'] = None
            else:
                p['responsibility_revision']['overrides']['36:QA-1-REAL'] = {'deferred_scope': []}
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                validate_responsibility_policy(p)
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = self.original
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            self.verify()

    def test_reassessment_preserves_old_result_and_rejects_fail_or_missing_coverage(self):
        confirmation = dict(observation(), actor='gpt', execution='computer_use',
                            equivalence_evidence='Same exact product/build/fixture inputs.',
                            environment_evidence='All source conditions and loaded identity match.')
        self.proof(confirmation)
        reassessed = {'status': 'reassessed', 'head': NEW, 'artifact_sha256': 'e' * 64,
                      'source_candidate': NEW, 'source_result': copy.deepcopy(self.old_result),
                      'confirmation': confirmation}
        self.initial.manifest['results']['36:QA-1-EMU'] = reassessed
        self.assertTrue(self.verify()['stage_complete'])
        self.assertEqual(reassessed['source_result'], self.old_result)
        for status in ('pending', 'blocked', 'fail', 'deferred', 'reused', 'reassessed'):
            reassessed['source_result']['status'] = status
            with self.subTest(status=status), self.assertRaises(ValueError):
                self.verify()
        reassessed['source_result'] = copy.deepcopy(self.old_result)
        del confirmation['responsibility_evidence']['contract_coverage']
        with self.assertRaisesRegex(ValueError, 'coverage'):
            self.verify()

    def test_listing_uses_effective_and_historical_contracts_and_same_gate(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'cases.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
            view = render_queue([path], self.initial.manifest, self.fixture.git)
            self.assertIn('有効な実施分担', view)
            self.assertIn('固定原契約', view)
            self.assertIn('All our samples remain correct.', view)
            self.assertIn('初期版範囲合格', view)
            del self.observed['responsibility_evidence']['file_integrity']
            self.assertNotIn('初期版範囲合格', render_queue([path], self.initial.manifest, self.fixture.git))

    def test_full_acceptance_also_requires_effective_proofs_and_keeps_real_observation(self):
        self.initial.manifest.pop('stage')
        results = {}
        for key in self.policy['cases']:
            results[key] = dict(observation(), actor='gpt')
            self.proof(results[key])
        results['36:QA-1-EMU']['execution'] = 'computer_use'
        self.initial.manifest['results'] = results
        self.assertTrue(self.verify()['gui_complete'])
        results['36:QA-1-REAL']['status'] = 'deferred'
        with self.assertRaises(ValueError):
            self.verify()
        results['36:QA-1-REAL']['status'] = 'pass'
        del results['36:QA-1-EMU']['responsibility_revision']
        with self.assertRaisesRegex(ValueError, 'coverage'):
            self.verify()

    def test_listing_binds_current_contract_even_when_only_older_sources_exist(self):
        current = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']
        old = copy.deepcopy(current)
        old['cases'][0]['expected'] = 'Older contract before a later product change.'
        old_ref = 'a' * 40
        self.fixture.documents[f'{old_ref}:docs/verification/changes/issue-36.json'] = old
        original = copy.deepcopy(self.original)
        source = original['cases']['36:QA-1-EMU']['sources'][0]
        source.update(source_merge=old_ref, original_case_sha256=canonical_hash(old['cases'][0]))
        self.policy['cases'] = original['cases']
        self.policy['responsibility_revision']['previous_plan_sha256'] = canonical_hash(original)
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = original
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = self.policy
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'cases.json'
            for data in (current, old):
                path.write_text(json.dumps(data))
                view = render_queue([path], self.initial.manifest, self.fixture.git)
                self.assertIn('All our samples remain correct.', view)
                self.assertIn('原期待結果: Older contract', view)

    def test_reassessment_cannot_bypass_changed_product_or_fixture_history(self):
        old = 'a' * 40
        key = '36:QA-1-EMU'
        path = 'docs/verification/changes/issue-36.json'
        self.fixture.documents[f'{old}:{path}'] = copy.deepcopy(self.fixture.documents[f'{NEW}:{path}'])
        self.fixture.documents[f'{old}:{INITIAL_PLAN}'] = copy.deepcopy(self.original)
        original = copy.deepcopy(self.old_result)
        original['head'] = original['initial_observation']['head'] = old
        confirmation = dict(observation(), equivalence_evidence='Unchanged', environment_evidence='Same conditions')
        self.proof(confirmation)
        self.initial.manifest['results'][key] = {
            'status': 'reassessed', 'head': NEW, 'artifact_sha256': 'e' * 64,
            'source_candidate': old, 'source_result': original, 'confirmation': confirmation}
        previous = self.fixture.git
        changed = 'docs/note.md'

        def git(*args):
            if args == ('rev-list', f'{old}..{NEW}'):
                return NEW
            if args[:2] == ('rev-list', '--parents') and args[-1] == NEW:
                return NEW + ' ' + old
            if args[0] == 'diff' and args[-2:] == (old, NEW):
                return changed
            return previous(*args)
        self.fixture.git = git
        self.assertTrue(self.verify()['stage_complete'])
        for changed in ('plugin/build.gradle.kts', 'tests/fixture.json', 'replay_buffer/app.py', path):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                self.verify()

    def test_real_repository_mapping_covers_135_and_preserves_all_204_sources(self):
        path = Path(__file__).resolve().parents[2] / INITIAL_PLAN
        p = json.loads(path.read_text())
        validate_responsibility_policy(p)
        self.assertEqual(135, len(p['cases']))
        self.assertEqual(204, sum(len(c['sources']) for c in p['cases'].values()))
        self.assertEqual(34, sum(c['initial_scope'] == 'deferred' for c in p['cases'].values()))
        self.assertTrue(all('full_acceptance_actor' not in effective_initial_entry(p, k) for k in p['cases']))

    def test_new_assessment_can_use_passing_facts_from_old_pending_verdict(self):
        aggregate = {'status': 'pending', 'head': NEW, 'artifact_sha256': 'e' * 64,
                     'reason': 'Owned sample proof exists; former external whole-Play is pending.'}
        fact = dict(observation(), actor='gpt', reason='Actual retained file/sample observation.')
        assessment = {'source_candidate': NEW, 'source_result': copy.deepcopy(aggregate),
                      'source_result_sha256': canonical_hash(aggregate),
                      'source_record_evidence': 'Immutable original report and verdict.',
                      'facts': {'saved-output': fact},
                      'coverage': {name: ['saved-output'] for name in self.observed['responsibility_evidence']},
                      'missing_requirements': [], 'unresolved_owned_failures': []}
        self.observed['responsibility_assessment'] = assessment
        self.assertTrue(self.verify()['stage_complete'])
        self.assertEqual(assessment['source_result'], aggregate)
        saved = copy.deepcopy(assessment)
        for mutation in ('missing-fact', 'missing-condition', 'owned-failure', 'unknown-identity',
                         'unknown-fact', 'failed-fact', 'time', 'aggregate-hash'):
            assessment.clear()
            assessment.update(copy.deepcopy(saved))
            if mutation == 'missing-fact':
                assessment['facts'] = {}
            elif mutation == 'missing-condition':
                assessment['missing_requirements'] = ['A retained data condition.']
            elif mutation == 'owned-failure':
                assessment['unresolved_owned_failures'] = ['Writer index drops a sample.']
            elif mutation == 'unknown-identity':
                assessment['facts']['saved-output']['loaded_identity'] = ''
            elif mutation == 'unknown-fact':
                assessment['coverage']['contract_coverage'] = ['not-observed']
            elif mutation == 'failed-fact':
                assessment['facts']['saved-output']['status'] = 'fail'
            elif mutation == 'time':
                assessment['facts']['saved-output']['at'] = '2099-01-01T00:00:00Z'
            else:
                assessment['source_result_sha256'] = 'f' * 64
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.verify()
