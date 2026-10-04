"""Synthetic gate results exercise scope boundaries; none are product QA evidence."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

import test_verification as tv
from test_verification import CASE, change, observation
from test_agent_loop import BASE, HEAD, NEW
from verification import (INITIAL_PLAN, INITIAL_STAGE, bind_initial_sources, canonical_hash,
                          render_queue, validate_initial_result)


class InitialAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.fixture = tv.AcceptanceTests()
        self.fixture.setUp()
        self.fixture.pr['body'] = self.fixture.pr['body'].replace('GUI: not-required', 'GUI: required')
        self.fixture.documents[f'{HEAD}:docs/verification/changes/issue-35.json'] = change()
        self.manifest = self.fixture.manifest
        self.manifest['stage'] = INITIAL_STAGE
        agent = dict(copy.deepcopy(CASE), id='QA-1-EMU', required_execution='computer_use')
        real = dict(copy.deepcopy(CASE), id='QA-1-REAL')
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'] = [agent, real]
        self.policy = {'schema': 1, 'issue': 64, 'stage': INITIAL_STAGE, 'required_ancestor': NEW,
                       'followup_issue': 65, 'owner': 'QA', 'resume_condition': '後続受入で再開',
                       'human_scope': '明示された人間工程だけ延期', 'cases': {}}
        for case in (agent, real):
            self.policy['cases']['36:' + case['id']] = {
                'sources': [{'source_pr': 99, 'source_merge': NEW,
                             'source_path': 'docs/verification/changes/issue-36.json',
                             'original_case_sha256': canonical_hash(case)}],
                'current_case_sha256': canonical_hash(case),
                'agent_requirements': ['残る条件をすべて実施'],
                'initial_scope': 'deferred' if case is real else 'agent',
                'deferred_scope': ['実機部分' if case is real else '明記された人間操作の部分']}
        self.raw = {}
        self.modes = {}
        original_git = self.fixture.git

        def git(*args):
            if args[0] == 'ls-tree':
                ref, path = args[1], args[-1]
                if f'{ref}:{path}' not in self.fixture.documents:
                    return ''
                return f'{self.modes.get((ref, path), "100644")} blob {"0" * 40}\t{path}'
            if args[0] == 'show' and args[1] in self.raw:
                return self.raw[args[1]]
            return original_git(*args)
        self.fixture.git = git
        self.record_policy()
        self.record_results()

    def record_policy(self):
        for ref in (BASE, NEW):
            self.fixture.documents[f'{ref}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)

    def record_results(self):
        results = {}
        for key, entry in self.policy['cases'].items():
            real = entry['initial_scope'] == 'deferred'
            results[key] = {'status': 'deferred' if real else 'initial-pass',
                            'stage_revision': canonical_hash(self.policy), 'head': NEW,
                            'artifact_sha256': 'e' * 64, 'followup_issue': 65}
            if not real:
                results[key]['initial_observation'] = dict(observation(), actor='gpt', execution='computer_use')
        self.manifest['results'] = results

    def verify(self):
        return self.fixture.verify()

    def test_only_initial_scope_completes_and_full_acceptance_stays_false(self):
        result = self.verify()
        self.assertTrue(result['stage_complete'])
        self.assertFalse(result['gui_complete'])
        self.assertFalse(result['full_acceptance_complete'])
        self.assertEqual((result['initial_cases'], result['deferred_cases']), (1, 1))
        self.assertNotIn('initial_observation', self.manifest['results']['36:QA-1-REAL'])

    def test_fail_pending_wrong_actor_old_build_or_missing_execution_cannot_be_deferred(self):
        saved = copy.deepcopy(self.manifest['results'])
        for patch in ({'status': 'fail'}, {'status': 'pending'}, {'status': 'blocked'},
                      {'actor': 'human'}, {'head': HEAD}, {'artifact_sha256': 'f' * 64},
                      {'execution': 'shell'}, {'evidence': ''}):
            self.manifest['results'] = copy.deepcopy(saved)
            self.manifest['results']['36:QA-1-EMU']['initial_observation'].update(patch)
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                self.verify()
        self.manifest['results'] = copy.deepcopy(saved)
        self.manifest['results']['36:QA-1-EMU'] = dict(saved['36:QA-1-REAL'])
        with self.assertRaises(ValueError):
            self.verify()

    def test_plan_binding_revision_followup_and_exact_result_keys_are_required(self):
        saved = copy.deepcopy(self.manifest['results'])
        for patch in ({'stage_revision': 'f' * 64}, {'followup_issue': 999}, {'head': HEAD},
                      {'status': 'pass'}, {'artifact_sha256': 'f' * 64}, {'extra': True}):
            self.manifest['results'] = copy.deepcopy(saved)
            self.manifest['results']['36:QA-1-REAL'].update(patch)
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                self.verify()
        for key in ('36:QA-1-EMU', '36:QA-1-REAL'):
            self.manifest['results'] = copy.deepcopy(saved)
            del self.manifest['results'][key]
            with self.assertRaisesRegex(ValueError, 'ALL'):
                self.verify()

    def test_candidate_only_changed_symlink_duplicate_key_or_missing_policy_rejected(self):
        for ref in (BASE, NEW):
            path = f'{ref}:{INITIAL_PLAN}'
            saved = self.fixture.documents.pop(path)
            with self.assertRaisesRegex(ValueError, 'regular JSON'):
                self.verify()
            self.fixture.documents[path] = saved
            self.modes[ref, INITIAL_PLAN] = '120000'
            with self.assertRaisesRegex(ValueError, 'regular JSON'):
                self.verify()
            self.modes.clear()
        self.fixture.documents[f'{NEW}:{INITIAL_PLAN}']['owner'] = 'candidate changed plan'
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            self.verify()
        self.record_policy()
        self.raw[f'{BASE}:{INITIAL_PLAN}'] = json.dumps(self.policy)[:-1] + ', "schema": 1}'
        with self.assertRaisesRegex(ValueError, 'Duplicate key'):
            self.verify()

    def test_all_source_occurrences_and_original_contract_are_bound(self):
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']['cases'][0]
        source = {k: v for k, v in self.policy['cases']['36:QA-1-EMU']['sources'][0].items()
                  if k != 'original_case_sha256'}
        original = case['expected']
        case['expected'] = 'remove a requirement'
        with self.assertRaisesRegex(ValueError, 'current Case identity'):
            self.verify()
        case['expected'] = original
        self.policy['cases']['36:QA-1-EMU']['sources'].append(dict(source, source_pr=100,
                                                            original_case_sha256=canonical_hash(case)))
        self.record_policy()
        self.record_results()
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.verify()
        with self.assertRaises(ValueError):
            bind_initial_sources(self.policy, [('36:QA-1-EMU', case, source)] * 2)

    def test_real_name_does_not_authorize_agent_case_skip(self):
        self.policy['cases']['36:QA-1-EMU']['initial_scope'] = 'deferred'
        self.record_policy()
        self.record_results()
        with self.assertRaisesRegex(ValueError, 'scope'):
            self.verify()

    def test_ordinary_results_remain_strict_and_unknown_stage_rejected(self):
        del self.manifest['stage']
        with self.assertRaisesRegex(ValueError, 'must pass'):
            self.verify()
        self.manifest['stage'] = 'any-waiver'
        with self.assertRaisesRegex(ValueError, 'Initial stage'):
            self.verify()

    def test_initial_promotion_keeps_gui_required(self):
        self.fixture.pr['body'] = self.fixture.pr['body'].replace('GUI: required', 'GUI: not-required')
        self.fixture.documents[f'{HEAD}:docs/verification/changes/issue-35.json'] = change(gui=False)
        with self.assertRaisesRegex(ValueError, 'GUI declaration'):
            self.verify()
        self.manifest['stage'] = INITIAL_STAGE
        self.manifest['scope'] = 'main'
        with self.assertRaisesRegex(ValueError, 'Initial stage'):
            self.verify()

    def test_later_full_promotion_cannot_drop_deferred_cases_already_in_main(self):
        self.policy['cases']['36:QA-1-EMU']['full_acceptance_actor'] = 'human'
        self.record_policy()
        self.manifest.pop('stage')
        later = 'd' * 40
        self.manifest['candidate'] = later
        self.fixture.range = [later]
        self.manifest['changes'] = [{'commit': later, 'pr': 100}]
        self.fixture.source['merge_commit_sha'] = later
        self.fixture.source['body'] = ('Issue: #37\nGUI: required\nIntegration: develop\n'
                                       'Verification: docs/verification/changes/issue-37.json')
        self.fixture.documents[f'{later}:docs/verification/changes/issue-37.json'] = change(37)
        self.fixture.documents[f'{later}:{INITIAL_PLAN}'] = copy.deepcopy(self.policy)
        self.manifest['results'] = {'37:QA-1': dict(observation(), head=later)}
        with self.assertRaisesRegex(ValueError, 'missing=.*36:QA-1-EMU'):
            self.verify()
        for key in self.policy['cases']:
            self.manifest['results'][key] = dict(observation(), head=later, execution='computer_use')
        with self.assertRaisesRegex(ValueError, 'human observation'):
            self.verify()
        self.manifest['results']['36:QA-1-EMU']['human_evidence'] = 'Human receipt on later candidate'
        self.assertTrue(self.verify()['gui_complete'])

    def test_renderer_distinguishes_initial_pass_and_unobserved_deferred_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json']))
            output = render_queue([path], self.manifest, self.fixture.git)
            self.assertIn('初期版範囲合格', output)
            self.assertIn('延期・未実施', output)
            self.assertNotIn('Case合格（', output)
            self.assertIn('後続Issue: #65', output)
            self.manifest['results']['36:QA-1-EMU']['initial_observation']['head'] = HEAD
            self.assertNotIn('初期版範囲合格', render_queue([path], self.manifest, self.fixture.git))

    def test_initial_observation_still_needs_gop_revision_and_all_six_proofs(self):
        key = '36:QA-1-EMU'
        contract = {'revision': 'f' * 64}
        result = self.manifest['results'][key]
        with self.assertRaisesRegex(ValueError, 'GOP'):
            validate_initial_result(result, key, self.policy, NEW, 'e' * 64, 'computer_use', contract)

    def test_full_acceptance_requires_explicit_human_evidence_but_initial_scope_remains_agent(self):
        key = '36:QA-1-EMU'
        self.policy['cases'][key]['full_acceptance_actor'] = 'human'
        self.record_policy()
        self.record_results()
        self.assertTrue(self.verify()['stage_complete'])
        self.manifest.pop('stage')
        self.manifest['results'] = {
            key: dict(observation(), actor='gpt', execution='computer_use'),
            '36:QA-1-REAL': dict(observation(), actor='gpt')}
        for patch in ({'actor': 'gpt', 'human_evidence': 'human check reference'},
                      {'actor': 'human'}, {'actor': 'human', 'human_evidence': ''}):
            self.manifest['results'][key] = dict(observation(), execution='computer_use', **patch)
            with self.subTest(patch=patch), self.assertRaisesRegex(ValueError, 'human observation'):
                self.verify()
        self.manifest['results'][key]['human_evidence'] = 'Same candidate image-number check receipt'
        self.assertTrue(self.verify()['gui_complete'])

    def test_full_renderer_restores_missing_or_changed_deferred_contracts_and_human_gate(self):
        key = '36:QA-1-EMU'
        self.policy['cases'][key]['full_acceptance_actor'] = 'human'
        older = copy.deepcopy(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
        older['cases'][0]['expected'] = 'Older original contract remains visible'
        self.fixture.documents[f'{HEAD}:docs/verification/changes/issue-36.json'] = older
        self.policy['cases'][key]['sources'].append({
            'source_pr': 100, 'source_merge': HEAD, 'source_path': 'docs/verification/changes/issue-36.json',
            'original_case_sha256': canonical_hash(older['cases'][0])})
        self.record_policy()
        self.manifest.pop('stage')
        self.manifest['results'] = {key: dict(observation(), actor='gpt', execution='computer_use')}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            current = copy.deepcopy(self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
            current['cases'][0]['expected'] = 'Caller changed the historical expectation'
            path.write_text(json.dumps(current))
            for paths in ([], [path]):
                output = render_queue(paths, self.manifest, self.fixture.git)
                self.assertIn('QA-1-EMU / #36', output)
                self.assertIn('QA-1-REAL / #36', output)
                self.assertIn('後続Issue: #65', output)
                self.assertIn('再開条件: 後続受入で再開', output)
                self.assertIn('原期待結果: ' + CASE['expected'], output)
                self.assertIn('Older original contract remains visible', output)
                self.assertNotIn('Caller changed', output)
                self.assertNotIn('Case合格（', output)
                self.assertNotIn('初期版範囲合格', output)
                self.assertNotIn('原Case全体: 未完了', output)
            self.manifest['results'][key].update(actor='human', human_evidence='Same candidate human receipt')
            self.assertIn('Case合格（', render_queue([], self.manifest, self.fixture.git))

    def test_full_actor_policy_rejects_unrecognized_actor_or_non_deferred_scope(self):
        entry = self.policy['cases']['36:QA-1-EMU']
        for actor in (None, 'gpt', 'either', ''):
            entry['full_acceptance_actor'] = actor
            self.record_policy()
            self.record_results()
            with self.subTest(actor=actor), self.assertRaisesRegex(ValueError, 'scope'):
                self.verify()
        entry.update(full_acceptance_actor='human', deferred_scope=[])
        self.record_policy()
        self.record_results()
        with self.assertRaisesRegex(ValueError, 'scope'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
