"""Synthetic observations only; exercise reuse without running product or GUI tests."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import test_initial_acceptance as tia
import test_verification as tv
from test_verification import change, observation
from test_agent_loop import NEW
from verification import (INITIAL_PLAN, render_queue,
                          validate_candidate_result)

OLD = 'a' * 40
ARTIFACT = 'e' * 64
CASE_PATH = 'docs/verification/changes/issue-36.json'


def reused(original, candidate=NEW, source=OLD):
    return {'status': 'reused', 'head': candidate, 'artifact_sha256': ARTIFACT,
            'source_candidate': source, 'source_result': copy.deepcopy(original),
            'confirmation': dict(observation(), head=candidate, at='2026-09-08T10:00:00+09:00',
                equivalence_evidence='synthetic unchanged artifact and runtime inputs',
                environment_evidence='synthetic current loaded identity and all prerequisites')}


class ReuseIntegrationTests(unittest.TestCase):
    def prepare(self, initial=False):
        if initial:
            self.initial = tia.InitialAcceptanceTests()
            self.initial.setUp()
            self.fixture = self.initial.fixture
            self.key = '36:QA-1-EMU'
            self.fixture.documents[f'{OLD}:{INITIAL_PLAN}'] = copy.deepcopy(self.initial.policy)
        else:
            self.fixture = tv.AcceptanceTests()
            self.fixture.setUp()
            self.key = '36:QA-1'
        self.fixture.documents[f'{OLD}:{CASE_PATH}'] = copy.deepcopy(self.fixture.documents[f'{NEW}:{CASE_PATH}'])
        original = copy.deepcopy(self.fixture.manifest['results'][self.key])
        original['head'] = OLD
        if initial:
            original['initial_observation']['head'] = OLD
        self.fixture.manifest['results'][self.key] = reused(original)
        old_git = self.fixture.git
        self.diff = 'docs/usage.md'

        def git(*args):
            if args[0] == 'ls-tree' and f'{args[1]}:{args[-1]}' in self.fixture.documents:
                return f'100644 blob {"0" * 40}\t{args[-1]}'
            if args == ('rev-list', f'{OLD}..{NEW}'):
                return NEW
            if args[:2] == ('rev-list', '--parents') and args[-1] == NEW:
                return NEW + ' ' + OLD
            if args[0] == 'diff' and args[-2:] == (OLD, NEW):
                return self.diff
            return old_git(*args)
        self.fixture.git = git
        return self.fixture.manifest['results'][self.key]

    def render(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'case.json'
            path.write_text(json.dumps(self.fixture.documents[f'{NEW}:{CASE_PATH}']))
            return render_queue([path], self.fixture.manifest, self.fixture.git)

    def test_full_and_initial_reuse_keep_original_observation_and_display_identity(self):
        for initial in (False, True):
            with self.subTest(initial=initial):
                result = self.prepare(initial)
                before = copy.deepcopy(result)
                outcome = self.fixture.verify()
                self.assertEqual(outcome['gui_complete'], not initial)
                if initial:
                    self.assertFalse(outcome['full_acceptance_complete'])
                    self.assertEqual(outcome['deferred_cases'], 1)
                text = self.render()
                self.assertIn('同一成果物の旧観察を再利用', text)
                self.assertIn(OLD, text)
                self.assertIn('現在候補の適合確認', text)
                self.assertEqual(result, before)

    def test_invalid_reuse_rejected_by_gate_and_view(self):
        patches = [({'head': OLD}, None), ({'artifact_sha256': 'f' * 64}, None),
                   ({'source_candidate': NEW}, None), ({'source_result': None}, None),
                   ({'confirmation': None}, None), ({'extra': 1}, None),
                   ({'status': 'fail'}, 'source_result'), ({'status': 'deferred'}, 'source_result'),
                   ({'status': 'reused'}, 'source_result'), ({'head': NEW}, 'source_result'),
                   ({'artifact_sha256': 'f' * 64}, 'source_result'),
                   ({'equivalence_evidence': ''}, 'confirmation'),
                   ({'environment_evidence': ''}, 'confirmation'), ({'loaded_identity': ''}, 'confirmation'),
                   ({'at': '2026-09-06T00:00:00Z'}, 'confirmation'),
                   ({'at': '2026-99-99T00:00:00Z'}, 'confirmation')]
        for patch, field in patches:
            with self.subTest(patch=patch, field=field):
                result = self.prepare()
                (result[field] if field else result).update(patch)
                with self.assertRaises(ValueError):
                    self.fixture.verify()
                self.assertIn('不可・再利用条件不成立', self.render())

    def test_changed_inputs_contracts_and_initial_scope_are_rejected(self):
        for path in ('replay_buffer/app.py', 'plugin/build.gradle.kts', 'config.json.example',
                     'tests/fixture.json', 'scripts/fixture.py', '.github/workflows/build.yml', 'unknown'):
            self.prepare()
            self.diff = path
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'retesting'):
                self.fixture.verify()
            self.assertIn('不可・再利用条件不成立', self.render())
        self.prepare()
        self.diff = CASE_PATH
        with self.assertRaisesRegex(ValueError, 'contract history'):
            self.fixture.verify()
        self.prepare()
        self.fixture.documents[f'{OLD}:{CASE_PATH}']['cases'][0]['expected'] = 'old contract'
        with self.assertRaisesRegex(ValueError, 'same existing'):
            self.fixture.verify()
        for key in ('human_scope', 'followup_issue', 'cases'):
            self.prepare(True)
            old_policy = self.fixture.documents[f'{OLD}:{INITIAL_PLAN}']
            if key == 'cases':
                old_policy['cases'][self.key]['agent_requirements'] = ['different condition']
            else:
                old_policy[key] = 'different'
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.fixture.verify()

    def test_original_execution_human_and_gop_requirements_remain_required(self):
        result = self.prepare(True)
        result['source_result']['initial_observation']['execution'] = 'shell'
        with self.assertRaisesRegex(ValueError, 'execution'):
            self.fixture.verify()
        result = self.prepare()
        policy = {'cases': {self.key: {'full_acceptance_actor': 'human'}}}
        result['source_result']['actor'] = 'gpt'
        # The ordinary observation validator remains responsible for these requirements.
        original = result['source_result']
        for execution, gop, entry in [('computer_use', None, None),
                                      (None, {'revision': 'f' * 64}, None),
                                      (None, None, policy)]:
            with self.subTest(execution=execution, gop=gop, policy=entry), self.assertRaises(ValueError):
                validate_candidate_result(original, self.key, entry, None, OLD, ARTIFACT,
                                          execution, gop, self.fixture.git)

    def test_removing_old_human_entry_or_plan_rejects_reuse_in_gate_and_view(self):
        self.prepare(True)
        old_policy = self.fixture.documents[f'{OLD}:{INITIAL_PLAN}']
        old_policy['cases'][self.key]['full_acceptance_actor'] = 'human'
        self.fixture.manifest.pop('stage')
        self.fixture.manifest['results'] = {self.key: reused(dict(observation(), head=OLD, actor='gpt')),
            '36:QA-1-REAL': observation()}
        # With no current plan, the source-side human requirement must still be inspected.
        from test_agent_loop import BASE
        self.fixture.documents.pop(f'{BASE}:{INITIAL_PLAN}')
        with self.assertRaisesRegex(ValueError, 'scope changed'):
            self.fixture.verify()
        self.assertIn('不可・再利用条件不成立', self.render())
        # An existing plan without this entry must not bypass that comparison either.
        current_policy = dict(old_policy, cases={})
        with self.assertRaisesRegex(ValueError, 'scope changed'):
            validate_candidate_result(self.fixture.manifest['results'][self.key], self.key,
                current_policy, None, NEW, ARTIFACT, None, None, self.fixture.git)


    def test_gop_policy_removal_is_a_condition_change(self):
        result = self.prepare()
        with patch('verification.GOP_KEYS', {self.key}), patch('verification.gop_amendment', return_value={'revision': 'old'}):
            with self.assertRaisesRegex(ValueError, 'GOP conditions changed'):
                validate_candidate_result(result, self.key, None, None, NEW, ARTIFACT,
                                          None, None, self.fixture.git)



class ReuseHistoryTests(unittest.TestCase):
    def test_real_git_allows_docs_but_rejects_reverted_product_and_nonancestor(self):
        with tempfile.TemporaryDirectory() as folder:
            def git(*args):
                return subprocess.check_output(['git', '-C', folder, *args], text=True, stderr=subprocess.DEVNULL).strip()
            git('init', '-q')
            git('config', 'user.name', 'Test')
            git('config', 'user.email', 'test@example.invalid')
            path = Path(folder) / CASE_PATH
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(change(36)))
            git('add', '.')
            git('commit', '-qm', 'source')
            source = git('rev-parse', 'HEAD')
            (Path(folder) / 'README.md').write_text('docs')
            git('add', '.')
            git('commit', '-qm', 'docs')
            candidate = git('rev-parse', 'HEAD')
            original = dict(observation(), head=source)
            def validate(head, old=source):
                return validate_candidate_result(reused(original, head, old), '36:QA-1', None,
                            None, head, ARTIFACT, None, None, git)
            self.assertEqual(validate(candidate), original)
            with self.assertRaises(ValueError):
                validate(source, candidate)
            product = Path(folder) / 'product.py'
            product.write_text('changed')
            git('add', '.')
            git('commit', '-qm', 'product')
            git('revert', '--no-edit', 'HEAD')
            self.assertEqual(git('diff', '--name-only', candidate, 'HEAD'), '')
            with self.assertRaisesRegex(ValueError, 'retesting'):
                validate(git('rev-parse', 'HEAD'))

    def test_merge_sync_allows_observed_product_but_rejects_new_resolution_and_side_history(self):
        with tempfile.TemporaryDirectory() as folder:
            def git(*args):
                return subprocess.check_output(['git', '-C', folder, *args], text=True, stderr=subprocess.DEVNULL).strip()
            git('init', '-q')
            git('config', 'user.name', 'Test')
            git('config', 'user.email', 'test@example.invalid')
            (Path(folder) / 'README.md').write_text('base')
            git('add', '.')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            git('checkout', '-qb', 'source')
            case = Path(folder) / CASE_PATH
            case.parent.mkdir(parents=True)
            case.write_text(json.dumps(change(36)))
            product = Path(folder) / 'product.py'
            product.write_text('observed product')
            git('add', '.')
            git('commit', '-qm', 'observed product')
            source = git('rev-parse', 'HEAD')
            git('checkout', '-qb', 'docs', base)
            (Path(folder) / 'README.md').write_text('docs')
            git('add', '.')
            git('commit', '-qm', 'docs')
            git('checkout', '-q', 'source')
            git('merge', '--no-ff', '-m', 'sync', 'docs')
            candidate = git('rev-parse', 'HEAD')
            original = dict(observation(), head=source)
            def validate(head):
                return validate_candidate_result(reused(original, head, source), '36:QA-1', None,
                            None, head, ARTIFACT, None, None, git)
            self.assertEqual(git('diff', '--name-only', source, candidate), 'README.md')
            self.assertEqual(validate(candidate), original)
            product.write_text('unobserved merge resolution')
            git('add', '.')
            git('commit', '--amend', '--no-edit', '-q')
            with self.assertRaisesRegex(ValueError, 'retesting'):
                validate(git('rev-parse', 'HEAD'))
            git('checkout', '-qb', 'side', source)
            product.write_text('unobserved side change')
            git('add', '.')
            git('commit', '-qm', 'side product')
            git('revert', '--no-edit', 'HEAD')
            git('checkout', '--detach', '-q', candidate)
            git('merge', '--no-ff', '-m', 'sync side', 'side')
            self.assertEqual(git('diff', '--name-only', candidate, 'HEAD'), '')
            with self.assertRaisesRegex(ValueError, 'retesting'):
                validate(git('rev-parse', 'HEAD'))

            git('checkout', '-qb', 'contract', candidate)
            contract = change(36)
            contract['cases'][0]['expected'] = 'unobserved requirement'
            case.write_text(json.dumps(contract))
            git('add', '.')
            git('commit', '-qm', 'contract change')
            git('revert', '--no-edit', 'HEAD')
            self.assertEqual(git('diff', '--name-only', candidate, 'HEAD'), '')
            with self.assertRaisesRegex(ValueError, 'contract history'):
                validate(git('rev-parse', 'HEAD'))
