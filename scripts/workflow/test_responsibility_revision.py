"""Amended methods preserve observations; synthetic results are never product passes."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from test_agent_loop import BASE, NEW
import test_initial_acceptance as ti
from test_verification import observation
from test_observation_reuse import OLD, RECORD, reused, record_original
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
        observed['responsibility_evidence'] = {name: 'Executed proof: ' + name
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
                      'source_candidate': NEW, 'source_record': RECORD, 'source_result': copy.deepcopy(self.old_result),
                      'confirmation': confirmation}
        record_original(self.fixture, '36:QA-1-EMU', reassessed)
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
            emu, real = view.split('## #36 / QA-1-REAL:')
            for reference in self.observed['responsibility_evidence'].values():
                self.assertIn(reference, emu)
                self.assertNotIn(reference, real)
            self.assertIn('このREAL専用Caseは全工程を延期', real)
            self.assertIn('有効条件の証拠: 未登録（未実施又は延期）', real)
            del self.observed['responsibility_evidence']['file_integrity']
            self.assertNotIn('初期版範囲合格', render_queue([path], self.initial.manifest, self.fixture.git))

    def test_reassessed_listing_separates_current_proof_from_original_observation(self):
        confirmation = dict(observation(), actor='gpt', execution='computer_use',
                            equivalence_evidence='Unchanged inputs.', environment_evidence='Same conditions.')
        self.proof(confirmation)
        original = copy.deepcopy(self.old_result)
        original['initial_observation']['reason'] = 'Preserved original observation reason.'
        self.initial.manifest['results']['36:QA-1-EMU'] = {
            'status': 'reassessed', 'head': NEW, 'artifact_sha256': 'e' * 64,
            'source_candidate': NEW, 'source_record': RECORD, 'source_result': original, 'confirmation': confirmation}
        record_original(self.fixture, '36:QA-1-EMU', self.initial.manifest['results']['36:QA-1-EMU'])
        self.assertTrue(self.verify()['stage_complete'])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'cases.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
            view = render_queue([path], self.initial.manifest, self.fixture.git)
        proof_line = next(line for line in view.splitlines() if line.startswith('有効条件の証拠'))
        self.assertIn('現在候補の適合確認', proof_line)
        for reference in confirmation['responsibility_evidence'].values():
            self.assertIn(reference, proof_line)
        self.assertIn('元の観察（履歴', view)
        self.assertIn('Preserved original observation reason.', view)
        self.assertEqual(original['initial_observation']['reason'], 'Preserved original observation reason.')

    def test_ordinary_reuse_lists_source_only_proofs_in_initial_and_full_scope(self):
        for initial in (True, False):
            with self.subTest(initial=initial):
                self.setUp()
                if not initial:
                    self.initial.manifest.pop('stage')
                    self.initial.manifest['results'] = {
                        key: dict(observation(), actor='gpt') for key in self.policy['cases']}
                    for result in self.initial.manifest['results'].values():
                        self.proof(result)
                    self.initial.manifest['results']['36:QA-1-EMU']['execution'] = 'computer_use'
                self.fixture.documents[f'{OLD}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
                self.fixture.documents[f'{OLD}:docs/verification/changes/issue-36.json'] = copy.deepcopy(
                    self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
                original = copy.deepcopy(self.initial.manifest['results']['36:QA-1-EMU'])
                original['head'] = OLD
                observed = original['initial_observation'] if initial else original
                observed['head'] = OLD
                result = reused(original)
                self.initial.manifest['results']['36:QA-1-EMU'] = result
                record_original(self.fixture, '36:QA-1-EMU', result)
                before = copy.deepcopy(result)
                self.assertNotIn('responsibility_evidence', result['confirmation'])
                self.assertTrue(self.verify()['stage_complete' if initial else 'gui_complete'])
                with tempfile.TemporaryDirectory() as tmp:
                    path = Path(tmp) / 'cases.json'
                    path.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
                    view = render_queue([path], self.initial.manifest, self.fixture.git)
                proof_line = next(line for line in view.splitlines() if line.startswith('有効条件の証拠'))
                self.assertIn('保持した元観察', proof_line)
                for reference in observed['responsibility_evidence'].values():
                    self.assertIn(reference, proof_line)
                self.assertIn(result['confirmation']['equivalence_evidence'], view)
                self.assertIn(result['confirmation']['environment_evidence'], view)
                self.assertEqual(result, before)

    def test_full_acceptance_also_requires_effective_proofs_and_keeps_real_observation(self):
        self.initial.manifest.pop('stage')
        results = {}
        for key in self.policy['cases']:
            results[key] = dict(observation(), actor='gpt')
            self.proof(results[key])
        results['36:QA-1-EMU']['execution'] = 'computer_use'
        self.initial.manifest['results'] = results
        self.assertTrue(self.verify()['gui_complete'])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'cases.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
            direct = render_queue([path], self.initial.manifest, self.fixture.git)
            proof_line = next(line for line in direct.splitlines() if line.startswith('有効条件の証拠'))
            self.assertTrue(proof_line.startswith('有効条件の証拠: '))
            for reference in results['36:QA-1-EMU']['responsibility_evidence'].values():
                self.assertIn(reference, proof_line)
            confirmation = results['36:QA-1-EMU']
            results['36:QA-1-EMU'] = {
                'status': 'reassessed', 'head': NEW, 'artifact_sha256': 'e' * 64,
                'source_candidate': NEW, 'source_record': RECORD,
                'source_result': dict(observation(), actor='gpt', execution='computer_use',
                                      reason='Preserved full observation.'),
                'confirmation': dict(confirmation, equivalence_evidence='Same inputs.',
                                     environment_evidence='Same environment.')}
            record_original(self.fixture, '36:QA-1-EMU', results['36:QA-1-EMU'])
            self.assertTrue(self.verify()['gui_complete'])
            assessed = render_queue([path], self.initial.manifest, self.fixture.git)
            proof_line = next(line for line in assessed.splitlines() if line.startswith('有効条件の証拠'))
            self.assertIn('現在候補の適合確認', proof_line)
            for reference in confirmation['responsibility_evidence'].values():
                self.assertIn(reference, proof_line)
            self.assertIn('Preserved full observation.', assessed)
            results['36:QA-1-EMU'] = confirmation
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
            'source_candidate': old, 'source_record': RECORD, 'source_result': original, 'confirmation': confirmation}
        record_original(self.fixture, key, self.initial.manifest['results'][key])
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

    def test_pre_revision_ordinary_reuse_cannot_attach_current_proofs(self):
        self.initial.manifest.pop('stage')
        self.initial.manifest['results'] = {key: dict(observation(), actor='gpt') for key in self.policy['cases']}
        for result in self.initial.manifest['results'].values():
            self.proof(result)
        self.initial.manifest['results']['36:QA-1-EMU']['execution'] = 'computer_use'
        path = 'docs/verification/changes/issue-36.json'
        self.fixture.documents[f'{OLD}:{path}'] = copy.deepcopy(self.fixture.documents[f'{NEW}:{path}'])
        self.fixture.documents[f'{OLD}:{INITIAL_PLAN}'] = copy.deepcopy(self.original)
        original = copy.deepcopy(self.initial.manifest['results']['36:QA-1-EMU'])
        original['head'] = OLD
        result = reused(original)
        record_original(self.fixture, '36:QA-1-EMU', result)
        self.initial.manifest['results']['36:QA-1-EMU'] = result
        with self.assertRaisesRegex(ValueError, 'scope changed'):
            self.verify()

    def test_latest_case_is_retained_and_old_override_cannot_erase_it(self):
        path = 'docs/verification/changes/issue-36.json'
        historical = 'a' * 40
        self.fixture.documents[f'{historical}:{path}'] = copy.deepcopy(self.fixture.documents[f'{NEW}:{path}'])
        self.original['required_ancestor'] = historical
        self.policy['required_ancestor'] = historical
        self.policy['responsibility_revision']['previous_plan_sha256'] = canonical_hash(self.original)
        for policy in (self.original, self.policy):
            for entry in policy['cases'].values():
                entry['sources'][0]['source_merge'] = historical
        self.policy['responsibility_revision']['previous_plan_sha256'] = canonical_hash(self.original)
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.original)
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        current = self.fixture.documents[f'{NEW}:{path}']['cases'][0]
        current['steps'].append('New candidate must preserve the additional output.')
        current['expected'] += ' New mandatory condition.'
        current['artifact'] = 'plugin'
        self.initial.manifest.pop('stage')
        self.initial.manifest['artifacts'] = {'plugin': 'e' * 64}
        self.initial.manifest['results'] = {k: dict(observation(), actor='gpt') for k in self.policy['cases']}
        for result in self.initial.manifest['results'].values():
            self.proof(result)
        observed = self.initial.manifest['results']['36:QA-1-EMU']
        observed['execution'] = 'computer_use'
        with self.assertRaisesRegex(ValueError, 'exact current effective contract'):
            self.verify()
        observed['case_sha256'] = canonical_hash(effective_responsibility_case(self.policy, '36:QA-1-EMU', current))
        self.assertTrue(self.verify()['gui_complete'])
        with tempfile.TemporaryDirectory() as tmp:
            file = Path(tmp) / 'case.json'
            file.write_text(json.dumps(self.fixture.documents[f'{NEW}:{path}']))
            view = render_queue([file], self.initial.manifest, self.fixture.git)
        self.assertIn('New candidate must preserve', view)
        self.assertIn('New mandatory condition.', view)
        self.assertNotIn('期待結果: All our samples remain correct.', view)

    def test_retained_fact_listing_preserves_verdict_facts_and_coverage(self):
        aggregate = {'status': 'pending', 'head': NEW, 'artifact_sha256': 'e' * 64, 'reason': 'Old pending verdict.'}
        self.observed['responsibility_assessment'] = {
            'source_candidate': NEW, 'source_result': aggregate,
            'source_result_sha256': canonical_hash(aggregate), 'source_record_evidence': 'Fixed original private record.',
            'facts': {'samples': dict(observation(), actor='gpt', reason='Actual old sample observation.')},
            'coverage': {name: ['samples'] for name in self.observed['responsibility_evidence']},
            'missing_requirements': [], 'unresolved_owned_failures': []}
        with tempfile.TemporaryDirectory() as tmp:
            file = Path(tmp) / 'case.json'
            file.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
            view = render_queue([file], self.initial.manifest, self.fixture.git)
        self.assertIn('保持事実を有効条件へ再評価／新しい試験実行ではない', view)
        self.assertIn('保持事実の現在評価（再試験ではない）', view)
        self.assertIn('Old pending verdict.', view)
        for key, value in self.observed['responsibility_assessment'].items():
            self.assertIn('"' + key + '"', view)
        self.assertIn('Actual old sample observation.', view)

    def test_real_repository_extension_preserves_original_plan_and_responsibility(self):
        path = Path(__file__).resolve().parents[2] / INITIAL_PLAN
        p = json.loads(path.read_text())
        validate_responsibility_policy(p)
        self.assertEqual(136, len(p['cases']))
        self.assertEqual(205, sum(len(c['sources']) for c in p['cases'].values()))
        self.assertEqual(34, sum(c['initial_scope'] == 'deferred' for c in p['cases'].values()))
        self.assertTrue(all('full_acceptance_actor' not in effective_initial_entry(p, k) for k in p['cases']))
        revision = copy.deepcopy(p['responsibility_revision'])
        extension = revision.pop('plan_extension')
        original = {k: v for k, v in p.items() if k != 'responsibility_revision'}
        original = dict(original, required_ancestor=extension['previous_required_ancestor'],
                        cases={k: v for k, v in p['cases'].items() if k not in extension['added_cases']})
        self.assertEqual('a5c470bf6bdbbbe77d2001bcb287667e4a6d399ee935ebd3fc23388be2f37441',
                         canonical_hash(original))
        revision['groups']['recovery']['cases'].remove('186:CLOCK-BATCH-SAVE')
        self.assertEqual('9d36b54dfb7261927a5608a41114446affad711a79c7af21b3514f9c56b7493a',
                         canonical_hash(revision))
        new = p['cases']['186:CLOCK-BATCH-SAVE']
        self.assertEqual([{'source_pr': 189,
                          'source_merge': '542575c0a23a257be6bb37b028171426ea171efd',
                          'source_path': 'docs/verification/changes/issue-186.json',
                          'original_case_sha256': 'dfa598a37e3c082521dc4ae1332d13d225723482b9acbf82a11fa3edff686962'}],
                         new['sources'])
        self.assertEqual(new['sources'][0]['source_merge'], p['required_ancestor'])
        self.assertEqual(new['sources'][0]['original_case_sha256'], new['current_case_sha256'])
        self.assertEqual(('agent', []), (new['initial_scope'], new['deferred_scope']))
        for mutation in ('old-entry', 'old-ancestor', 'missing-addition', 'unknown-addition', 'null'):
            bad = copy.deepcopy(p)
            if mutation == 'old-entry':
                bad['cases']['3:PLUGIN-LOAD']['agent_requirements'].append('Alter old scope')
            elif mutation == 'old-ancestor':
                bad['responsibility_revision']['plan_extension']['previous_required_ancestor'] = 'f' * 40
            elif mutation == 'missing-addition':
                del bad['cases']['186:CLOCK-BATCH-SAVE']
            elif mutation == 'unknown-addition':
                bad['responsibility_revision']['plan_extension']['added_cases'].append('185:EXTRA')
            else:
                bad['responsibility_revision']['plan_extension'] = None
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_responsibility_policy(bad)

    def test_extension_gate_and_listing_require_new_case_and_keep_all_five_steps(self):
        key = '186:CLOCK-BATCH-SAVE'
        case = dict(copy.deepcopy(ti.CASE), id='CLOCK-BATCH-SAVE',
                    steps=['ZIP identity', 'Clock regression', 'Store/Wire regression',
                           'Native save/continue', 'Native fixed retry'])
        data = {'schema': 1, 'issue': 186, 'gui_required': True,
                'reason': 'Changed clock classes', 'cli_checks': ['Clock regression'], 'cases': [case]}
        entry = copy.deepcopy(self.policy['cases']['36:QA-1-EMU'])
        entry.update(current_case_sha256=canonical_hash(case), deferred_scope=[])
        entry['sources'] = [{'source_pr': 100, 'source_merge': NEW,
                             'source_path': 'docs/verification/changes/issue-186.json',
                             'original_case_sha256': canonical_hash(case)}]
        self.policy['cases'][key] = entry
        revision = self.policy['responsibility_revision']
        revision['plan_extension'] = {'issue': 190, 'previous_required_ancestor': NEW,
                                     'added_cases': [key]}
        revision['groups']['fixture']['cases'].append(key)
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-186.json'] = data
        # A candidate carrying the old 135-style plan is no longer the current plan.
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            initial_stage_plan(BASE, NEW, self.fixture.git)
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.assertEqual(self.policy, initial_stage_plan(BASE, NEW, self.fixture.git))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'new-case.json'
            path.write_text(json.dumps(data))
            view = render_queue([path], self.initial.manifest, self.fixture.git)
        for step in case['steps']:
            self.assertIn(step, view)
        self.assertIn('CLOCK-BATCH-SAVE', view)
        self.assertNotIn('初期版範囲合格', view)
        self.assertIn('有効条件の証拠: 未登録', view)
        self.assertEqual(case, data['cases'][0])
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-186.json']['cases'][0]['steps'].pop()
        with self.assertRaisesRegex(ValueError, 'current Case identity'):
            initial_stage_plan(BASE, NEW, self.fixture.git)

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
        for source_policy in (self.original, self.policy):
            self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(source_policy)
            with self.subTest(amended='responsibility_revision' in source_policy):
                self.assertTrue(self.verify()['stage_complete'])
                validate_candidate_result(self.observed, '36:QA-1-EMU', self.policy, None,
                                          NEW, 'e' * 64, 'computer_use', None, self.fixture.git)
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
        assessment.clear()
        assessment.update(copy.deepcopy(saved))
        source = 'a' * 40
        assessment['source_candidate'] = source
        assessment['source_result']['head'] = source
        assessment['source_result_sha256'] = canonical_hash(assessment['source_result'])
        assessment['facts']['saved-output']['head'] = source
        self.fixture.documents[f'{source}:docs/verification/changes/issue-36.json'] = copy.deepcopy(
            self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
        different = copy.deepcopy(self.policy)
        different['responsibility_revision']['groups']['fixture']['methods'] = 'An unauthorized different method.'
        self.fixture.documents[f'{source}:{INITIAL_PLAN}'] = different
        with self.assertRaisesRegex(ValueError, 'exact .* responsibility plan'):
            self.verify()
