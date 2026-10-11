"""Amended methods preserve observations; synthetic results are never product passes."""
import copy
from functools import cache
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from test_agent_loop import BASE, NEW
import test_initial_acceptance as ti
from test_verification import observation
from test_observation_reuse import OLD, RECORD, reused, record_original
from verification import (INITIAL_PLAN, canonical_hash, effective_initial_entry,
                          effective_responsibility_case, initial_stage_plan, render_queue,
                          previous_installation_plan, previous_settings_plan, previous_method_plan, historical_initial_sources,
                          bind_initial_sources, validate_fact_assessment,
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
        self.assertEqual(139, len(p['cases']))
        self.assertEqual(208, sum(len(c['sources']) for c in p['cases'].values()))
        p = previous_settings_plan(p)
        self.assertEqual('6d11e3cdee0b6189c743fb1a4c8ee3a4e582d72af7e9643bb001536dd4e0c79f',
                         canonical_hash(p))
        validate_responsibility_policy(p)
        self.assertEqual(138, len(p['cases']))
        self.assertEqual(207, sum(len(c['sources']) for c in p['cases'].values()))
        self.assertEqual(34, sum(c['initial_scope'] == 'deferred' for c in p['cases'].values()))
        self.assertTrue(all('full_acceptance_actor' not in effective_initial_entry(p, k) for k in p['cases']))
        revision = copy.deepcopy(p['responsibility_revision'])
        previous = previous_installation_plan(p)
        self.assertEqual('13bd1e294519915d8c8a66be2770811a4d6b28465432061e7c2eae91a87fd8a3',
                         canonical_hash(previous))
        self.assertEqual(previous['cases'], p['cases'])
        revision.pop('installation_revision')
        revision['overrides'].pop('3:PLUGIN-LOAD')
        extension = revision.pop('plan_extension')
        original = {k: v for k, v in p.items() if k != 'responsibility_revision'}
        original = dict(original, required_ancestor=extension['previous_required_ancestor'],
                        cases={k: v for k, v in p['cases'].items() if k not in extension['added_cases']})
        self.assertEqual('a5c470bf6bdbbbe77d2001bcb287667e4a6d399ee935ebd3fc23388be2f37441',
                         canonical_hash(original))
        for key in extension['added_cases']:
            revision['groups']['recovery']['cases'].remove(key)
        for key in ('27:SAVE-FAIL-02-EMU', '27:SAVE-FAIL-02-REAL'):
            revision['overrides'].pop(key)
        revision['common_requirements'][0] = revision['common_requirements'][0].replace(
            'と失敗・取消し保存attemptの削除義務', '')
        revision['groups']['recovery']['excluded'] = (
            '合成で検証したfault全種を端末/OSで再注入する義務を除外。自製品glueの失敗表示/cleanup/retryは省略しない。')
        self.assertEqual('9d36b54dfb7261927a5608a41114446affad711a79c7af21b3514f9c56b7493a',
                         canonical_hash(revision))
        new = p['cases']['186:CLOCK-BATCH-SAVE']
        self.assertEqual([{'source_pr': 189,
                          'source_merge': '542575c0a23a257be6bb37b028171426ea171efd',
                          'source_path': 'docs/verification/changes/issue-186.json',
                          'original_case_sha256': 'dfa598a37e3c082521dc4ae1332d13d225723482b9acbf82a11fa3edff686962'}],
                         new['sources'])
        previous = dict(original, required_ancestor=new['sources'][0]['source_merge'],
                        cases={**original['cases'], '186:CLOCK-BATCH-SAVE': new})
        revision['groups']['recovery']['cases'].append('186:CLOCK-BATCH-SAVE')
        revision['plan_extension'] = dict(extension, issue=190, added_cases=['186:CLOCK-BATCH-SAVE'])
        previous['responsibility_revision'] = revision
        self.assertEqual('1fd0f0104e89c9931e59cb56d94a3f027d52f60cfb173663407ad52473f30dde',
                         canonical_hash(previous))
        for key, pr, merge, digest in (
                ('197:SAVE-FAILURE-OWNERSHIP', 199, 'ef36082ec4145bae481cd4ea883fcc26fced2482',
                 'dbde8e191ff0a01174b9573314634bdd6ce91c280c2cf6a73ac9f112bb83843c'),
                ('198:LOCAL-OWNERSHIP-RECOVERY', 200, '4d278e65dec0c55e71d584ff24c7f6fa407586cb',
                 'a7de6997b9c6850c6cd25de4b203a13731a9175676a46e99a79319b96b7e860b')):
            entry = p['cases'][key]
            self.assertEqual([{'source_pr': pr, 'source_merge': merge,
                              'source_path': 'docs/verification/changes/issue-' + key.split(':')[0] + '.json',
                              'original_case_sha256': digest}], entry['sources'])
            self.assertEqual(digest, entry['current_case_sha256'])
            self.assertEqual(('agent', []), (entry['initial_scope'], entry['deferred_scope']))
        self.assertEqual('4d278e65dec0c55e71d584ff24c7f6fa407586cb', p['required_ancestor'])
        self.assertEqual(new['sources'][0]['original_case_sha256'], new['current_case_sha256'])
        self.assertEqual(('agent', []), (new['initial_scope'], new['deferred_scope']))
        for mutation in ('old-entry', 'old-ancestor', 'missing-addition', 'missing-197', 'missing-198',
                         'wrong-issue', 'unknown-addition', 'null'):
            bad = copy.deepcopy(p)
            if mutation == 'old-entry':
                bad['cases']['3:PLUGIN-LOAD']['agent_requirements'].append('Alter old scope')
            elif mutation == 'old-ancestor':
                bad['responsibility_revision']['plan_extension']['previous_required_ancestor'] = 'f' * 40
            elif mutation == 'missing-addition':
                del bad['cases']['186:CLOCK-BATCH-SAVE']
            elif mutation in ('missing-197', 'missing-198'):
                del bad['cases'][next(k for k in bad['cases'] if k.startswith(mutation[-3:] + ':'))]
            elif mutation == 'wrong-issue':
                bad['responsibility_revision']['plan_extension']['issue'] = 190
            elif mutation == 'unknown-addition':
                bad['responsibility_revision']['plan_extension']['added_cases'].append('185:EXTRA')
            else:
                bad['responsibility_revision']['plan_extension'] = None
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_responsibility_policy(bad)

    def installation_policy(self):
        case = copy.deepcopy(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'][0])
        case['id'] = 'PLUGIN-LOAD'
        data = {'schema': 1, 'issue': 3, 'gui_required': True, 'reason': 'Plugin load',
                'cli_checks': ['ZIP identity'], 'cases': [case]}
        entry = copy.deepcopy(self.original['cases']['36:QA-1-EMU'])
        entry.update(current_case_sha256=canonical_hash(case), sources=[{
            'source_pr': 4, 'source_merge': NEW, 'source_path': 'docs/verification/changes/issue-3.json',
            'original_case_sha256': canonical_hash(case)}])
        self.original['cases']['3:PLUGIN-LOAD'] = entry
        self.policy['cases']['3:PLUGIN-LOAD'] = copy.deepcopy(entry)
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-3.json'] = data
        revision = self.policy['responsibility_revision']
        revision['previous_plan_sha256'] = canonical_hash(self.original)
        revision['groups']['fixture']['cases'].append('3:PLUGIN-LOAD')
        previous = copy.deepcopy(self.policy)
        self.enterContext(patch('verification.INSTALLATION_PREVIOUS_PLAN_SHA256', canonical_hash(previous)))
        revision['installation_revision'] = {'issue': 208, 'previous_plan_sha256': canonical_hash(previous)}
        revision['overrides']['3:PLUGIN-LOAD'] = {'case': {'steps': ['Terminal deploy; actual IDE load.']}}
        self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = previous
        self.proof(self.observed)
        return previous

    def test_installation_revision_accepts_exact_registered_plan_and_retained_facts_only(self):
        previous = self.installation_policy()
        self.assertEqual(self.policy, initial_stage_plan(BASE, NEW, self.fixture.git))
        aggregate = {'status': 'pending', 'head': NEW, 'artifact_sha256': 'e' * 64,
                     'reason': 'Original verdict remains pending.'}
        assessment = {'source_candidate': NEW, 'source_result': copy.deepcopy(aggregate),
                      'source_result_sha256': canonical_hash(aggregate), 'source_record_evidence': 'Fixed original report.',
                      'facts': {'output': dict(observation(), actor='gpt')},
                      'coverage': {name: ['output'] for name in self.observed['responsibility_evidence']},
                      'missing_requirements': [], 'unresolved_owned_failures': []}
        self.observed['responsibility_assessment'] = assessment
        validate_candidate_result(self.observed, '36:QA-1-EMU', self.policy, None,
                                  NEW, 'e' * 64, 'computer_use', None, self.fixture.git)
        self.assertEqual(aggregate, assessment['source_result'])
        self.assertEqual(previous, self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'])
        # A real prior PASS still needs its original trusted record and current equivalence proof.
        original = dict(observation(), actor='gpt', execution='computer_use',
                        responsibility_revision=canonical_hash(previous['responsibility_revision']),
                        responsibility_evidence=copy.deepcopy(self.observed['responsibility_evidence']))
        confirmation = dict(self.observed, equivalence_evidence='Same product/build/fixture.',
                            environment_evidence='Same exact loaded environment.')
        confirmation.pop('responsibility_assessment')
        reassessed = {'status': 'reassessed', 'head': NEW, 'artifact_sha256': 'e' * 64,
                      'source_candidate': NEW, 'source_record': RECORD, 'source_result': original,
                      'confirmation': confirmation}
        record_original(self.fixture, '36:QA-1-EMU', reassessed)
        validate_candidate_result(reassessed, '36:QA-1-EMU', self.policy, None,
                                  NEW, 'e' * 64, 'computer_use', None, self.fixture.git, BASE)
        self.assertEqual(previous, self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'])
        for mutation in ('hash', 'other-method', 'case', 'null', 'incomplete', 'owned-failure', 'ordinary-reuse'):
            documents = copy.deepcopy(self.fixture.documents)
            observed = copy.deepcopy(self.observed)
            policy = copy.deepcopy(self.policy)
            if mutation == 'hash':
                policy['responsibility_revision']['installation_revision']['previous_plan_sha256'] = 'f' * 64
            elif mutation == 'other-method':
                self.fixture.documents[f'{NEW}:{INITIAL_PLAN}']['responsibility_revision']['groups']['fixture']['methods'] += ' changed'
            elif mutation == 'case':
                self.fixture.documents[f'{NEW}:docs/verification/changes/issue-3.json']['cases'][0]['steps'].append('Changed contract')
            elif mutation == 'null':
                self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = None
            elif mutation == 'incomplete':
                observed['responsibility_assessment']['missing_requirements'] = ['Unobserved native integration']
            elif mutation == 'owned-failure':
                observed['responsibility_assessment']['unresolved_owned_failures'] = ['Owned failure']
            else:
                original = copy.deepcopy(self.old_result)
                original['head'] = original['initial_observation']['head'] = OLD
                self.fixture.documents[f'{OLD}:{INITIAL_PLAN}'] = previous
                self.fixture.documents[f'{OLD}:docs/verification/changes/issue-3.json'] = documents[f'{NEW}:docs/verification/changes/issue-3.json']
                self.fixture.documents[f'{OLD}:docs/verification/changes/issue-36.json'] = documents[f'{NEW}:docs/verification/changes/issue-36.json']
                observed = reused(original)
                record_original(self.fixture, '36:QA-1-EMU', observed)
            self.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = policy
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                current = initial_stage_plan(BASE, NEW, self.fixture.git)
                validate_candidate_result(observed, '36:QA-1-EMU', current, None,
                                          NEW, 'e' * 64, 'computer_use', None, self.fixture.git, BASE)
            self.fixture.documents = documents

    def test_installation_revision_does_not_authorize_other_policy_or_scope_changes(self):
        path = Path(__file__).resolve().parents[2] / INITIAL_PLAN
        policy = json.loads(path.read_text())
        for mutation in ('old-entry', 'other-group', 'other-override', 'issue', 'extra', 'missing-override'):
            bad = copy.deepcopy(policy)
            revision = bad['responsibility_revision']
            if mutation == 'old-entry':
                bad['cases']['3:PLUGIN-LOAD']['deferred_scope'] = []
            elif mutation == 'other-group':
                revision['groups']['environment']['methods'] += ' changed'
            elif mutation == 'other-override':
                revision['overrides']['27:SAVE-FAIL-02-EMU']['case']['expected'] += ' changed'
            elif mutation == 'issue':
                revision['installation_revision']['issue'] = 201
            elif mutation == 'extra':
                revision['installation_revision']['other_plans'] = ['f' * 64]
            else:
                revision['overrides'].pop('3:PLUGIN-LOAD')
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_responsibility_policy(bad)

    def test_real_installation_plan_rejects_rehashed_prior_contract_changes(self):
        root = Path(__file__).resolve().parents[2]
        base = 'f6352e39e45ee33913c0ee79ca482d61531dffdc'
        candidate = '707d3b0d26f749ef7537ef83a25a38081af35d62'
        prior_ref = '280c96cb2d93002152e57de7803b1eb03c8bc26b'
        policy = previous_settings_plan(json.loads((root / INITIAL_PLAN).read_text()))

        @cache
        def git(*args):
            return subprocess.check_output(['git', *args], cwd=root, text=True).strip()

        prior = json.loads(git('show', prior_ref + ':' + INITIAL_PLAN))
        self.assertEqual(prior, previous_installation_plan(policy))
        self.assertEqual('13bd1e294519915d8c8a66be2770811a4d6b28465432061e7c2eae91a87fd8a3',
                         canonical_hash(prior))
        validate_responsibility_policy(policy)
        validate_responsibility_policy(prior)
        self.assertEqual(policy, initial_stage_plan(base, candidate, git))
        self.assertEqual(prior, initial_stage_plan(candidate, candidate, git))

        # Only plan copies are overlaid; ancestor, regular-file and Case checks use real Git.
        for mutation in ('current', 'group', 'override', 'common', 'hash'):
            bad = copy.deepcopy(policy)
            revision = bad['responsibility_revision']
            if mutation == 'group':
                revision['groups']['environment']['methods'] += ' changed'
            elif mutation == 'override':
                revision['overrides']['27:SAVE-FAIL-02-EMU']['case']['expected'] += ' changed'
            elif mutation == 'common':
                revision['common_requirements'][0] += ' changed'
            elif mutation == 'hash':
                revision['installation_revision']['previous_plan_sha256'] = 'f' * 64
            self.assertEqual(policy['cases'], bad['cases'])
            previous = previous_installation_plan(bad)

            def overlay(*args):
                if args == ('show', base + ':' + INITIAL_PLAN):
                    return json.dumps(bad)
                if args == ('show', candidate + ':' + INITIAL_PLAN):
                    return json.dumps(bad if mutation == 'current' else previous)
                return git(*args)

            if mutation == 'current':
                self.assertEqual(policy, initial_stage_plan(base, candidate, overlay))
                continue
            for rehash in ((False,) if mutation == 'hash' else (False, True)):
                if rehash:
                    revision['installation_revision']['previous_plan_sha256'] = canonical_hash(previous)
                with self.subTest(mutation=mutation, rehash=rehash):
                    with self.assertRaisesRegex(ValueError, 'exact prior plan'):
                        validate_responsibility_policy(bad)
                    with self.assertRaisesRegex(ValueError, 'exact prior plan'):
                        initial_stage_plan(base, candidate, overlay)

    def test_settings_extension_binds_real_history_sources_and_listing(self):
        root = Path(__file__).resolve().parents[2]
        base = '6d976b6e2883e88ca15307e68956eaf649f38a9f'
        candidate = '06419bd17dddc00a1b57c086be4e86d6184e945f'
        key = '216:SETTINGS-WIDTH-RECOVERY'
        policy = json.loads((root / INITIAL_PLAN).read_text())

        @cache
        def git(*args):
            return subprocess.check_output(['git', *args], cwd=root, text=True).strip()

        prior = json.loads(git('show', base + ':' + INITIAL_PLAN))
        self.assertEqual(prior, previous_settings_plan(policy))
        self.assertEqual(prior['cases'], {k: v for k, v in policy['cases'].items() if k != key})
        self.assertEqual(previous_installation_plan(prior), previous_installation_plan(policy))
        self.assertEqual(policy['responsibility_revision']['installation_revision'],
                         prior['responsibility_revision']['installation_revision'])
        self.assertEqual((105, 34), tuple(sum(c['initial_scope'] == scope for c in policy['cases'].values())
                                         for scope in ('agent', 'deferred')))
        self.assertEqual(policy['responsibility_revision']['groups']['ui-settings']['cases'],
                         prior['responsibility_revision']['groups']['ui-settings']['cases'] + [key])
        self.assertEqual(prior, initial_stage_plan(base, candidate, git))
        data = json.loads(git('show', candidate + ':docs/verification/changes/issue-216.json'))
        case = data['cases'][0]
        self.assertEqual('e53106b34733bd505bdd9740892674cbc2acf703ba204b06708d5768d9e9d8f3',
                         canonical_hash(case))
        self.assertEqual([{'source_pr': 218, 'source_merge': candidate,
                          'source_path': 'docs/verification/changes/issue-216.json',
                          'original_case_sha256': canonical_hash(case)}], policy['cases'][key]['sources'])
        self.assertEqual(('agent', []), (policy['cases'][key]['initial_scope'],
                                       policy['cases'][key]['deferred_scope']))

        def synchronized(*args):
            if args in (('show', base + ':' + INITIAL_PLAN), ('show', candidate + ':' + INITIAL_PLAN)):
                return json.dumps(policy)
            return git(*args)

        self.assertEqual(policy, initial_stage_plan(base, candidate, synchronized))
        rows = historical_initial_sources(policy, candidate, synchronized)
        self.assertEqual(208, len(rows))
        self.assertEqual(case, next(c for k, c, _ in rows if k == key))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            path.write_text(json.dumps(data))
            manifest = {'base': base, 'candidate': candidate, 'stage': 'initial-agent', 'results': {}}
            view = render_queue([path], manifest, synchronized)
            for text in [case['preconditions'], *case['steps'], case['expected']]:
                self.assertIn(text, view)
            self.assertIn('有効条件の証拠: 未登録', view)
            self.assertNotIn('初期版範囲合格', view)

        # New candidates must synchronize 139 Cases; the prior plans are history only.
        for candidate_policy in (prior, previous_installation_plan(policy)):
            def unsynchronized(*args):
                if args == ('show', base + ':' + INITIAL_PLAN):
                    return json.dumps(policy)
                if args == ('show', candidate + ':' + INITIAL_PLAN):
                    return json.dumps(candidate_policy)
                return git(*args)
            with self.assertRaisesRegex(ValueError, 'exact trusted'):
                initial_stage_plan(base, candidate, unsynchronized)

        # A candidate-only registration does not change the trusted main plan.
        def candidate_only(*args):
            return json.dumps(policy) if args == ('show', candidate + ':' + INITIAL_PLAN) else git(*args)
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            initial_stage_plan(base, candidate, candidate_only)

        for mutation in ('group', 'override', 'common', 'old-entry', 'hash', 'missing',
                         'extra-key', 'duplicate-group', 'source-pr', 'source-merge', 'source-path',
                         'source-hash', 'current-hash'):
            bad = copy.deepcopy(policy)
            revision = bad['responsibility_revision']
            if mutation == 'group':
                revision['groups']['ui-settings']['methods'] += ' changed'
            elif mutation == 'override':
                revision['overrides']['3:PLUGIN-LOAD']['case']['steps'][0] += ' changed'
            elif mutation == 'common':
                revision['common_requirements'][0] += ' changed'
            elif mutation == 'old-entry':
                bad['cases']['173:PACKAGE-INPUT-GUIDANCE']['agent_requirements'].append('Changed old scope')
            elif mutation == 'hash':
                revision['plan_extension']['previous_plan_sha256'] = 'f' * 64
            elif mutation == 'missing':
                del bad['cases'][key]
            elif mutation == 'extra-key':
                bad['cases']['216:EXTRA'] = copy.deepcopy(bad['cases'][key])
            elif mutation == 'duplicate-group':
                revision['groups']['ui-settings']['cases'].append(key)
            elif mutation == 'source-pr':
                bad['cases'][key]['sources'][0]['source_pr'] = 216
            elif mutation == 'source-merge':
                bad['cases'][key]['sources'][0]['source_merge'] = base
            elif mutation == 'source-path':
                bad['cases'][key]['sources'][0]['source_path'] = 'docs/verification/changes/issue-173.json'
            elif mutation == 'source-hash':
                bad['cases'][key]['sources'][0]['original_case_sha256'] = 'f' * 64
            else:
                bad['cases'][key]['current_case_sha256'] = 'f' * 64

            def changed(*args):
                if args in (('show', base + ':' + INITIAL_PLAN), ('show', candidate + ':' + INITIAL_PLAN)):
                    return json.dumps(bad)
                return git(*args)

            for rehash in (False, True):
                if rehash and mutation in ('group', 'override', 'common', 'old-entry'):
                    revision['plan_extension']['previous_plan_sha256'] = canonical_hash(previous_settings_plan(bad))
                    revision['installation_revision']['previous_plan_sha256'] = canonical_hash(previous_installation_plan(bad))
                with self.subTest(mutation=mutation, rehash=rehash), self.assertRaises(ValueError):
                    admitted = initial_stage_plan(base, candidate, changed)
                    bind_initial_sources(admitted, rows)

        # Retained successful facts from the registered 138 plan are admissible only
        # with unchanged product history; this synthetic observation is not a GUI PASS.
        observed = dict(observation(), actor='gpt', head=candidate,
                        responsibility_revision=canonical_hash(policy['responsibility_revision']))
        observed['responsibility_evidence'] = {name: 'Synthetic coverage fixture' for name in
            ('contract_coverage', 'artifact_binding', 'failure_attribution', 'regression', 'integration')}
        aggregate = {'status': 'pending', 'head': candidate, 'reason': 'Original pending stays pending'}
        observed['responsibility_assessment'] = {
            'source_candidate': candidate, 'source_result': aggregate,
            'source_result_sha256': canonical_hash(aggregate), 'source_record_evidence': 'Synthetic fixture',
            'facts': {'old': dict(observation(), head=candidate, actor='gpt')},
            'coverage': {name: ['old'] for name in observed['responsibility_evidence']},
            'missing_requirements': [], 'unresolved_owned_failures': []}
        validate_fact_assessment(observed, '173:PACKAGE-INPUT-GUIDANCE', policy,
                                 candidate, 'e' * 64, git, base)
        self.assertEqual('pending', aggregate['status'])
        bad_prior = copy.deepcopy(prior)
        bad_prior['responsibility_revision']['overrides']['3:PLUGIN-LOAD']['case']['steps'][0] += ' changed'

        def mutated_source(*args):
            return json.dumps(bad_prior) if args == ('show', candidate + ':' + INITIAL_PLAN) else git(*args)
        with self.assertRaisesRegex(ValueError, 'exact original or trusted current'):
            validate_fact_assessment(observed, '173:PACKAGE-INPUT-GUIDANCE', policy,
                                     candidate, 'e' * 64, mutated_source, base)

        original = dict(observation(), head=candidate, actor='gpt',
                        responsibility_revision=canonical_hash(prior['responsibility_revision']),
                        responsibility_evidence=copy.deepcopy(observed['responsibility_evidence']))
        confirmation = dict(observed, at='2026-10-12T12:00:00+09:00',
                            equivalence_evidence='Synthetic same artifact fixture',
                            environment_evidence='Synthetic same conditions fixture')
        confirmation.pop('responsibility_assessment')
        reassessed = {'status': 'reassessed', 'head': candidate, 'artifact_sha256': 'e' * 64,
                      'source_candidate': candidate, 'source_record': base,
                      'source_result': original, 'confirmation': confirmation}

        def recorded(*args):
            if args == ('ls-tree', base, '--', 'docs/verification/promotion.json'):
                return '100644 blob ' + '0' * 40 + '\tdocs/verification/promotion.json'
            if args == ('show', base + ':docs/verification/promotion.json'):
                return json.dumps({'schema': 1, 'candidate': candidate,
                                   'results': {'173:PACKAGE-INPUT-GUIDANCE': original}})
            return git(*args)
        validate_candidate_result(reassessed, '173:PACKAGE-INPUT-GUIDANCE', policy, None,
                                  candidate, 'e' * 64, None, None, recorded, base)
        assessment = observed['responsibility_assessment']
        old_candidate = 'f3fdec1196ae8a4e1b27c0a57f514f23ec072c7c'
        assessment['source_candidate'] = aggregate['head'] = old_candidate
        assessment['source_result_sha256'] = canonical_hash(aggregate)
        with self.assertRaisesRegex(ValueError, 'Product, build'):
            validate_fact_assessment(observed, '173:PACKAGE-INPUT-GUIDANCE', policy,
                                     candidate, 'e' * 64, git, base)

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
        stripped = {k: v for k, v in self.policy.items() if k != 'responsibility_revision'}
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(stripped)
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            initial_stage_plan(BASE, NEW, self.fixture.git)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'new-case.json'
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'exact trusted'):
                render_queue([path], self.initial.manifest, self.fixture.git)
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

    def test_save_retention_extension_gate_and_listing_share_fixed_sources(self):
        additions = ['186:CLOCK-BATCH-SAVE', '197:SAVE-FAILURE-OWNERSHIP',
                     '198:LOCAL-OWNERSHIP-RECOVERY']
        revision = self.policy['responsibility_revision']
        revision['plan_extension'] = {'issue': 201, 'previous_required_ancestor': NEW,
                                     'added_cases': additions}
        cases = []
        for key in additions:
            number, name = key.split(':')
            case = dict(copy.deepcopy(ti.CASE), id=name, steps=['Same ZIP regression', 'Native shared glue'])
            path = 'docs/verification/changes/issue-' + number + '.json'
            self.fixture.documents[f'{NEW}:{path}'] = {
                'schema': 1, 'issue': int(number), 'gui_required': True,
                'reason': 'Fixed product change', 'cli_checks': ['Regression'], 'cases': [case]}
            entry = copy.deepcopy(self.policy['cases']['36:QA-1-EMU'])
            entry.update(current_case_sha256=canonical_hash(case), deferred_scope=[], sources=[{
                'source_pr': int(number), 'source_merge': NEW, 'source_path': path,
                'original_case_sha256': canonical_hash(case)}])
            self.policy['cases'][key] = entry
            revision['groups']['fixture']['cases'].append(key)
            cases.append((key, case, {k: v for k, v in entry['sources'][0].items()
                                      if k != 'original_case_sha256'}))
        # SAVE-FAIL-02's retained original body is amended by the same resolver as the gate.
        key = '36:QA-1-EMU'
        revised_expected = 'Retain failed/cancelled attempt; best effort location; fixed retry.'
        revision['overrides'][key]['case']['expected'] = revised_expected
        original_case = copy.deepcopy(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'][0])
        # Supply all added merged source Cases through the existing gate fixture.
        original_api = self.fixture.api
        original_git = self.fixture.git
        merges = {number: str(number % 10) * 40 for number in (186, 197, 198)}
        for key, case, source in cases:
            number = int(key.split(':')[0])
            merge = merges[number]
            self.policy['cases'][key]['sources'][0]['source_merge'] = merge
            self.fixture.documents[f"{merge}:{source['source_path']}"] = copy.deepcopy(
                self.fixture.documents[f"{NEW}:{source['source_path']}"])
            self.fixture.range.append(merge)
            self.initial.manifest['changes'].append({'commit': merge, 'pr': number})
        def api(path):
            if path.startswith('pulls/') and int(path.split('/')[1]) in merges:
                number = int(path.split('/')[1])
                source = copy.deepcopy(self.fixture.source)
                source['merge_commit_sha'] = merges[number]
                source['head']['ref'] = f'codex/{number}-change'
                source['body'] = (f'Issue: #{number}\nGUI: required\nIntegration: develop\n'
                                  f'Verification: docs/verification/changes/issue-{number}.json')
                return source
            return original_api(path)
        def git(*args):
            if args[0] == 'rev-list' and '--first-parent' in args:
                return args[-1].split('..')[-1]
            return original_git(*args)
        self.fixture.api, self.fixture.git = api, git
        self.initial.record_policy()
        self.initial.record_results()
        for result in self.initial.manifest['results'].values():
            if 'initial_observation' in result:
                self.proof(result['initial_observation'])
        self.assertEqual(5, self.verify()['cases'])
        missing = self.initial.manifest['results'].pop(additions[1])
        with self.assertRaisesRegex(ValueError, 'ALL required'):
            self.verify()
        self.initial.manifest['results'][additions[1]] = missing
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'original.json'
            data = copy.deepcopy(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
            path.write_text(json.dumps(data))
            paths = [path]
            for new_key in additions:
                source_path = self.policy['cases'][new_key]['sources'][0]['source_path']
                new_path = Path(tmp) / Path(source_path).name
                new_path.write_text(json.dumps(self.fixture.documents[f'{NEW}:{source_path}']))
                paths.append(new_path)
            view = render_queue(paths, self.initial.manifest, self.fixture.git)
            self.assertIn(revised_expected, view)
            for new_key in additions:
                self.assertIn(new_key.split(':')[1], view)
            self.assertEqual(original_case, data['cases'][0])
            for mutation in ('candidate-only', 'missing-source', 'wrong-source-hash', 'current-case'):
                documents = copy.deepcopy(self.fixture.documents)
                if mutation == 'candidate-only':
                    self.fixture.documents[f'{NEW}:{INITIAL_PLAN}']['responsibility_revision']['overrides']['36:QA-1-EMU']['case']['expected'] = 'Candidate amendment'
                elif mutation == 'missing-source':
                    del self.fixture.documents[f'{merges[197]}:docs/verification/changes/issue-197.json']
                elif mutation == 'wrong-source-hash':
                    for ref in (BASE, NEW):
                        self.fixture.documents[f'{ref}:{INITIAL_PLAN}']['cases'][additions[1]]['sources'][0]['original_case_sha256'] = 'f' * 64
                else:
                    self.fixture.documents[f'{NEW}:docs/verification/changes/issue-198.json']['cases'][0]['expected'] = 'Changed contract'
                with self.subTest(mutation=mutation):
                    with self.assertRaises((ValueError, KeyError)):
                        self.verify()
                    with self.assertRaises((ValueError, KeyError)):
                        render_queue(paths, self.initial.manifest, self.fixture.git)
                self.fixture.documents = documents

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


class PurposeMethodRevisionTests(unittest.TestCase):
    def setUp(self):
        self.prior = ResponsibilityRevisionTests()
        self.prior.setUp()
        self.policy = copy.deepcopy(self.prior.policy)
        self.method = {
            'issue': 223, 'previous_plan_sha256': canonical_hash(self.prior.policy),
            'common_requirements': ['Keep real windows, bytes, losses and owned failures.'],
            'groups': copy.deepcopy(self.prior.policy['responsibility_revision']['groups']),
            'overrides': {'36:QA-1-EMU': {'case': {
                'expected': 'Save all acquired data when acquisition is shorter than N.'}}}}
        group = self.method['groups']['fixture']
        group['methods'] = 'Native short capture plus exact production boundary regression.'
        group['preconditions'] = 'Fixed artifact, owned fixture and actual target; no unrelated device requirement.'
        group['evidence_keys'] += ['window_and_gop']
        self.policy['responsibility_revision']['method_revision'] = self.method
        self.prior.initial.policy = self.policy
        self.prior.fixture.documents[f'{BASE}:{INITIAL_PLAN}'] = self.policy
        # The product candidate still contains the exact registered previous plan.
        self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.prior.policy)
        self.prior.initial.record_results()
        self.observed = self.prior.initial.manifest['results']['36:QA-1-EMU']['initial_observation']
        self.observed['responsibility_revision'] = canonical_hash(self.policy['responsibility_revision'])
        self.observed['responsibility_evidence'] = {k: 'Identified actual proof: ' + k for k in
            ('contract_coverage', 'artifact_binding', 'failure_attribution', 'file_integrity', 'window_and_gop')}
        self.binding = patch('verification.PURPOSE_PREVIOUS_PLAN_SHA256', canonical_hash(self.prior.policy))
        self.binding.start()
        self.addCleanup(self.binding.stop)

    def test_method_only_revision_accepts_unchanged_candidate_and_replaces_recipe(self):
        self.assertTrue(self.prior.verify()['stage_complete'])
        case = self.prior.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'][0]
        effective = effective_responsibility_case(self.policy, '36:QA-1-EMU', case)
        self.assertEqual('Save all acquired data when acquisition is shorter than N.',
                         effective['expected'].split(' Keep real windows')[0])
        self.assertNotEqual(case['steps'], effective['steps'])
        self.assertEqual(case['gpt'], effective['gpt'])
        self.assertEqual('computer_use', effective['required_execution'])
        self.assertNotEqual(self.prior.policy['cases']['36:QA-1-EMU']['agent_requirements'],
                            effective_initial_entry(self.policy, '36:QA-1-EMU')['agent_requirements'])
        self.assertEqual(self.prior.policy,
                         self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}'])

    def test_new_method_rejects_missing_window_proof_stale_stage_unknown_plan_and_real_waiver(self):
        for mutation in ('window', 'stage', 'candidate', 'old_hash', 'missing', 'duplicate', 'real'):
            saved = copy.deepcopy(self.policy)
            if mutation == 'window':
                del self.observed['responsibility_evidence']['window_and_gop']
            elif mutation == 'stage':
                self.prior.initial.manifest['results']['36:QA-1-EMU']['stage_revision'] = canonical_hash(self.prior.policy)
            elif mutation == 'candidate':
                self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}']['owner'] += ' changed'
            elif mutation == 'old_hash':
                self.policy['cases']['36:QA-1-EMU']['sources'][0]['original_case_sha256'] = 'f' * 64
            elif mutation == 'missing':
                self.method['groups']['fixture']['cases'].pop()
            elif mutation == 'duplicate':
                self.method['groups']['fixture']['cases'].append('36:QA-1-EMU')
            else:
                self.method['overrides']['36:QA-1-REAL'] = {'deferred_scope': []}
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.prior.verify()
            self.policy.clear()
            self.policy.update(saved)
            self.method = self.policy['responsibility_revision']['method_revision']
            self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.prior.policy)
            self.prior.initial.manifest['results']['36:QA-1-EMU']['stage_revision'] = canonical_hash(self.policy)
            self.observed['responsibility_evidence']['window_and_gop'] = 'Identified actual boundary proof.'

    def test_real_all_case_coverage_and_observed_stalls_use_purpose_methods(self):
        self.binding.stop()
        root = Path(__file__).resolve().parents[2]
        policy = json.loads((root / INITIAL_PLAN).read_text())
        candidate = '830b58294bec3f2fa4751d831a5b95f6b0658de6'
        previous_ref = 'f95e6267cc5c94dfacc1148f6c3afe345fedd2bb'

        @cache
        def git(*args):
            return subprocess.check_output(['git', *args], cwd=root, text=True).strip()

        previous = json.loads(git('show', previous_ref + ':' + INITIAL_PLAN))
        self.assertEqual(previous, previous_method_plan(policy))

        def trusted(*args):
            if args == ('show', previous_ref + ':' + INITIAL_PLAN):
                return json.dumps(policy)
            return git(*args)

        self.assertEqual(policy, initial_stage_plan(previous_ref, candidate, trusted))
        sources = historical_initial_sources(policy, candidate, trusted)
        self.assertEqual((139, 208), (len(policy['cases']), len(sources)))
        self.assertEqual(previous['cases'], policy['cases'])
        method = policy['responsibility_revision']['method_revision']
        self.assertEqual(139, sum(len(g['cases']) for g in method['groups'].values()))
        data = {}
        for key, entry in policy['cases'].items():
            path = entry['sources'][0]['source_path']
            data.setdefault(path, json.loads(git('show', policy['required_ancestor'] + ':' + path)))
            case = next(c for c in data[path]['cases'] if c['id'] == key.split(':')[1])
            effective = effective_responsibility_case(policy, key, case)
            self.assertEqual(case['gpt'], effective['gpt'])
            self.assertEqual(case['human'], effective['human'])
            self.assertEqual(case.get('required_execution'), effective.get('required_execution'))
            requirements = effective_initial_entry(policy, key)['agent_requirements']
            self.assertNotIn('全300event', ' '.join(requirements))
            self.assertNotIn('各100', ' '.join(requirements))
            if key == '27:BUFFER-SHORT-EMU':
                self.assertNotIn('約20秒', effective['expected'])
                self.assertIn('N未満', effective['expected'])
                self.assertIn('実34秒', ' '.join(effective['steps']))
                self.assertNotIn('取得時間>N', ' '.join(requirements))
                self.assertNotIn('2 APK', effective['preconditions'])
                self.assertIn('本Caseに無関係', ' '.join(requirements))
            if key in ('41:CORE-CLEANUP', '198:LOCAL-OWNERSHIP-RECOVERY'):
                self.assertIn('全file即時消失は要求しない', effective['expected'])
            if key == '27:STATE-RESTORE-EMU':
                self.assertIn('通常close後のXML', ' '.join(effective['steps']))
            if key == '85:CLOCK-UNCERTAIN-VIDEO-PLAYBACK':
                self.assertNotIn('全300event/30分', effective['expected'])
                self.assertIn('失敗pin', ' '.join(effective['steps']))
        # Full source history and fixed accuracy/byte safety survive the method replacement.
        self.assertIn('p95≤100ms', method['groups']['precision']['purpose'])
        self.assertIn('window_and_gop', method['groups']['buffer']['evidence_keys'])
        self.assertIn('全対象packet/frame', method['groups']['video']['methods'])
        for edit in ('original', 'missing', 'duplicate', 'real'):
            bad = copy.deepcopy(policy)
            if edit == 'original':
                bad['responsibility_revision']['groups']['buffer']['methods'] += ' changed'
            elif edit == 'missing':
                bad['responsibility_revision']['method_revision']['groups']['buffer']['cases'].pop()
            elif edit == 'duplicate':
                bad['responsibility_revision']['method_revision']['groups']['buffer']['cases'].append('13:SYNC-WINDOW')
            else:
                bad['responsibility_revision']['method_revision']['overrides']['27:BUFFER-SHORT-REAL'] = {'deferred_scope': []}
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                validate_responsibility_policy(bad)

    def test_old_failed_deadline_facts_need_complete_current_proof_and_no_owned_failure(self):
        aggregate = {'status': 'fail', 'head': NEW, 'artifact_sha256': 'e' * 64,
                     'reason': 'Acquired 34 seconds; former chosen 20-second deadline missed.'}
        fact = dict(observation(), actor='gpt', reason='Actual complete N-unfilled save and continuation.')
        self.observed['responsibility_assessment'] = {
            'source_candidate': NEW, 'source_result': copy.deepcopy(aggregate),
            'source_result_sha256': canonical_hash(aggregate),
            'source_record_evidence': 'Immutable original timed attempt and complete FILE report.',
            'facts': {'short-save': fact},
            'coverage': {name: ['short-save'] for name in self.observed['responsibility_evidence']},
            'missing_requirements': [], 'unresolved_owned_failures': []}
        self.assertTrue(self.prior.verify()['stage_complete'])
        self.assertEqual(aggregate, self.observed['responsibility_assessment']['source_result'])
        assessment = self.observed['responsibility_assessment']
        for edit in ('loss', 'missing-boundary', 'failed-fact', 'unknown-plan'):
            saved = copy.deepcopy(assessment)
            if edit == 'loss':
                assessment['unresolved_owned_failures'] = ['An acquired source packet was lost.']
            elif edit == 'missing-boundary':
                del assessment['coverage']['window_and_gop']
            elif edit == 'failed-fact':
                assessment['facts']['short-save']['status'] = 'fail'
            else:
                self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}']['responsibility_revision']['groups']['fixture']['methods'] += ' changed'
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                self.prior.verify()
            assessment.clear()
            assessment.update(saved)
            self.prior.fixture.documents[f'{NEW}:{INITIAL_PLAN}'] = copy.deepcopy(self.prior.policy)
