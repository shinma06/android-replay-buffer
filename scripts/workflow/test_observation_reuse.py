"""Synthetic observations only; exercise reuse without running product or GUI tests."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

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
        self.contract_log = ''

        def git(*args):
            if args[0] == 'ls-tree' and f'{args[1]}:{args[-1]}' in self.fixture.documents:
                return f'100644 blob {"0" * 40}\t{args[-1]}'
            if args == ('rev-list', f'{OLD}..{NEW}'):
                return NEW
            if args[:2] == ('rev-list', '--parents') and args[-1] == NEW:
                return NEW + ' ' + OLD
            if args[0] == 'diff' and args[-2:] == (OLD, NEW):
                return self.diff
            if args[0] == 'log':
                return self.contract_log
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
        self.contract_log = 'b' * 40
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
