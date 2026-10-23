"""Synthetic results test gate behavior; none are product acceptance evidence."""
import copy
from functools import cache
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from test_agent_loop import BASE, HEAD, NEW
import test_initial_acceptance as initial_tests
from test_verification import change, observation
import verification as v


class InitialContinuationTests(unittest.TestCase):
    def setUp(self):
        self.initial = initial_tests.InitialAcceptanceTests()
        self.initial.setUp()
        self.fixture = self.initial.fixture
        self.manifest = self.initial.manifest
        old = '1' * 40
        self.initial.policy['required_ancestor'] = old
        self.fixture.documents[f'{old}:docs/verification/changes/issue-36.json'] = copy.deepcopy(
            self.fixture.documents[f'{NEW}:docs/verification/changes/issue-36.json'])
        for entry in self.initial.policy['cases'].values():
            entry['sources'][0]['source_merge'] = old
        self.initial.record_policy()
        self.initial.record_results()
        self.fixture.source['body'] = ('Issue: #37\nGUI: required\nIntegration: develop\n'
                                      'Verification: docs/verification/changes/issue-37.json')
        self.fixture.source['head']['ref'] = 'codex/37-source'
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json'] = change(37)
        self.manifest['changes'] = [{'commit': NEW, 'pr': 100}]
        self.manifest['results']['37:QA-1'] = observation()
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json']['cases'][0]
        self.registration = {'schema': 1, 'issue': 344, 'candidate': NEW,
            'plan_sha256': v.canonical_hash(self.initial.policy),
            'changes': [dict(self.manifest['changes'][0], source_path='docs/verification/changes/issue-37.json')],
            'cases': {'37:QA-1': {'current_case_sha256': v.canonical_hash(case), 'sources': [{
                'source_pr': 100, 'source_merge': NEW,
                'source_path': 'docs/verification/changes/issue-37.json',
                'original_case_sha256': v.canonical_hash(case)}]}}}
        self.fixture.documents[f'{BASE}:{v.INITIAL_CONTINUATION}'] = self.registration

    def verify(self):
        return self.initial.verify()

    def render(self):
        return v.render_queue([], self.manifest, self.fixture.git)

    def test_carried_and_additional_cases_share_stage_but_keep_distinct_contracts(self):
        result = self.verify()
        self.assertEqual((3, 2, 1), (result['cases'], result['initial_cases'], result['deferred_cases']))
        self.assertTrue(result['stage_complete'])
        self.assertFalse(result['full_acceptance_complete'])
        self.assertFalse(result['gui_complete'])
        output = self.render()
        self.assertEqual(3, sum(line.startswith('| QA-') for line in output.splitlines()))
        self.assertIn('初期版範囲合格', output)
        self.assertIn('延期・未実施', output)
        self.assertIn('Case合格（', output)
        self.assertIn('今回の候補結果: pass', output)

    def test_new_case_requires_ordinary_full_pass_not_initial_or_deferred(self):
        for status in ('pending', 'blocked', 'fail', 'deferred', 'initial-pass'):
            self.manifest['results']['37:QA-1']['status'] = status
            with self.subTest(status=status), self.assertRaises(ValueError):
                self.verify()
            self.assertNotIn('Case合格（', self.render())

    def test_all_results_current_build_execution_and_evidence_remain_required(self):
        original = copy.deepcopy(self.manifest['results'])
        for key in original:
            self.manifest['results'] = copy.deepcopy(original)
            del self.manifest['results'][key]
            with self.subTest(missing=key), self.assertRaisesRegex(ValueError, 'ALL'):
                self.verify()
        for patch in ({'head': HEAD}, {'artifact_sha256': 'f' * 64}, {'evidence': ''}):
            self.manifest['results'] = copy.deepcopy(original)
            self.manifest['results']['37:QA-1'].update(patch)
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                self.verify()
        self.manifest['results'] = original
        self.manifest['results']['38:UNKNOWN'] = observation()
        with self.assertRaisesRegex(ValueError, 'extra=38:UNKNOWN'):
            self.verify()

    def test_candidate_only_registration_and_other_candidate_do_not_extend_stage(self):
        self.fixture.documents[f'{NEW}:{v.INITIAL_CONTINUATION}'] = self.registration
        saved = self.fixture.documents.pop(f'{BASE}:{v.INITIAL_CONTINUATION}')
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.verify()
        self.assertNotIn('Case合格（', self.render())
        self.fixture.documents[f'{BASE}:{v.INITIAL_CONTINUATION}'] = saved
        self.registration['candidate'] = HEAD
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.verify()
        self.registration['candidate'] = NEW
        self.initial.modes[BASE, v.INITIAL_CONTINUATION] = '120000'
        with self.assertRaisesRegex(ValueError, 'regular JSON'):
            self.verify()

    def test_exact_source_commit_mapping_hash_and_scope_cannot_change(self):
        original = copy.deepcopy(self.registration)
        edits = [lambda r: r.update(plan_sha256='f' * 64),
                 lambda r: r['changes'][0].update(pr=101),
                 lambda r: r['changes'].append(copy.deepcopy(r['changes'][0])),
                 lambda r: r['cases']['37:QA-1'].update(initial_scope='deferred'),
                 lambda r: r['cases']['37:QA-1'].update(current_case_sha256='f' * 64),
                 lambda r: r['cases']['37:QA-1']['sources'][0].update(original_case_sha256='f' * 64),
                 lambda r: r['cases']['37:QA-1']['sources'].append(copy.deepcopy(r['cases']['37:QA-1']['sources'][0]))]
        for edit in edits:
            self.registration.clear()
            self.registration.update(copy.deepcopy(original))
            edit(self.registration)
            with self.subTest(edit=edits.index(edit)), self.assertRaises(ValueError):
                self.verify()
            with self.assertRaises(ValueError):
                self.render()

    def test_unknown_case_or_changed_current_contract_is_not_admitted(self):
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json']['cases'][0]
        original = copy.deepcopy(case)
        case['expected'] += ' changed'
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.verify()
        case.clear()
        case.update(original)
        self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json']['cases'].append(
            dict(case, id='UNKNOWN'))
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.verify()
        with self.assertRaisesRegex(ValueError, 'ALL original'):
            self.render()

    def test_full_acceptance_does_not_inherit_any_initial_deferral(self):
        self.manifest.pop('stage')
        with self.assertRaisesRegex(ValueError, 'must pass'):
            self.verify()
        self.manifest['results'] = {key: dict(observation(), execution='computer_use')
                                   for key in self.manifest['results']}
        self.assertTrue(self.verify()['gui_complete'])

    def test_registered_new_execution_requirement_is_preserved(self):
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json']['cases'][0]
        case['required_execution'] = 'computer_use'
        entry = self.registration['cases']['37:QA-1']
        entry['current_case_sha256'] = v.canonical_hash(case)
        entry['sources'][0]['original_case_sha256'] = v.canonical_hash(case)
        with self.assertRaisesRegex(ValueError, 'execution method'):
            self.verify()
        self.manifest['results']['37:QA-1']['execution'] = 'computer_use'
        self.assertTrue(self.verify()['stage_complete'])

    def test_metadata_carry_keeps_only_exact_main_policy_and_case_free_records(self):
        record_path = 'docs/verification/changes/issue-344.json'
        product_path = 'docs/verification/changes/issue-37.json'
        script = 'scripts/workflow/verification.py'
        unknown = 'docs/verification/unknown.json'
        paths = [v.INITIAL_CONTINUATION, record_path, product_path, script, unknown]
        records = {record_path: change(344, gui=False), product_path: change(37)}

        def git(*args):
            if args[0] == 'merge-base':
                return BASE
            if args[0] == 'diff':
                return '\n'.join(paths)
            if args[0] == 'ls-tree':
                if args[1] == NEW:
                    return ''
                return f'100644 blob {"0" * 40}\t{args[-1]}'
            if args[0] == 'show':
                return json.dumps(records[args[1].split(':', 1)[1]])
            raise AssertionError(args)

        with patch.object(v, 'initial_stage_plan', return_value=self.initial.policy):
            carried = v.promotion_policy_blobs(BASE, NEW, git, continuation=True)
        self.assertEqual({v.INITIAL_CONTINUATION, record_path, script}, set(carried))


class RegisteredContinuationTests(unittest.TestCase):
    def test_registered_166_contracts_and_235_sources_render_without_inventing_results(self):
        root = Path(__file__).resolve().parents[2]
        registration = json.loads((root / v.INITIAL_CONTINUATION).read_text())
        policy = json.loads((root / v.INITIAL_PLAN).read_text())
        base, candidate = '8e6a1d626e103d5e0c67850bc53445d18ab5791e', registration['candidate']
        read = cache(lambda *args: subprocess.check_output(['git', *args], cwd=root, text=True).strip())

        def git(*args):
            if args == ('ls-tree', base, '--', v.INITIAL_CONTINUATION):
                return f'100644 blob {"0" * 40}\t{v.INITIAL_CONTINUATION}'
            if args == ('show', base + ':' + v.INITIAL_CONTINUATION):
                return json.dumps(registration)
            return read(*args)

        self.assertEqual('0b6d9c6d2d5054ed642565afdbaeb5a88ee198a4ea5398ff96ff36168ce0eda5',
                         v.canonical_hash(policy))
        self.assertEqual(policy, v.initial_stage_plan(base, candidate, git))
        extra = v.historical_initial_sources(registration, candidate, git)
        changes = [{k: item[k] for k in ('commit', 'pr')} for item in registration['changes']]
        sources = v.initial_stage_sources(policy, v.initial_continuation(policy, base, candidate, git),
                                         None, changes, candidate, git)
        self.assertEqual((166, 235, 27, 20), (len({k for k, _, _ in sources}), len(sources),
                                            len(extra), len(registration['changes'])))
        manifest = {'stage': v.INITIAL_STAGE, 'base': base, 'candidate': candidate,
                    'changes': changes, 'results': {}}
        output = v.render_queue([], manifest, git)
        self.assertEqual(166, sum(line.startswith('| ') and ' / #' in line for line in output.splitlines()))
        self.assertNotIn('Case合格（', output)
        self.assertNotIn('初期版範囲合格／', output)
        self.assertEqual(139, len(policy['cases']))
        self.assertEqual(34, sum(c['initial_scope'] == 'deferred' for c in policy['cases'].values()))

        # The virtual promotion branch/API and all observations below are synthetic.
        head = 'f' * 40
        path = 'docs/verification/changes/issue-999.json'
        manifest['schema'] = 1
        manifest['artifacts'] = {'plugin': 'e' * 64, 'app': 'e' * 64}
        contracts = {key: case for key, case, _ in sources}
        for key, case in contracts.items():
            observed = dict(observation(), head=candidate, actor='gpt',
                            at='2026-10-23T16:00:00+09:00')
            if case.get('required_execution'):
                observed['execution'] = case['required_execution']
            group = v.responsibility_group(policy, key)
            if group:
                observed['responsibility_revision'] = v.canonical_hash(policy['responsibility_revision'])
                observed['responsibility_evidence'] = {name: 'Synthetic unit coverage only' for name in
                    {'contract_coverage', 'artifact_binding', 'failure_attribution', *group['evidence_keys']}}
            if key in policy['cases']:
                deferred = v.effective_initial_entry(policy, key)['initial_scope'] == 'deferred'
                result = {'status': 'deferred' if deferred else 'initial-pass',
                          'head': candidate, 'artifact_sha256': 'e' * 64,
                          'stage_revision': v.canonical_hash(policy), 'followup_issue': 65}
                if not deferred:
                    result['initial_observation'] = observed
            else:
                result = observed
            manifest['results'][key] = result

        def virtual_git(*args):
            if args[0] == 'fetch':
                return ''
            if args[0] == 'ls-tree' and args[1] == head:
                if args[-1] in (path, v.PROMOTION):
                    return f'100644 blob {"0" * 40}\t{args[-1]}'
                return git('ls-tree', base, '--', args[-1])
            if args == ('show', head + ':' + path):
                return json.dumps(change(999))
            if args == ('show', head + ':' + v.PROMOTION):
                return json.dumps(manifest)
            if args[0] == 'rev-list' and head in args:
                return ''
            if args[0] == 'merge-base' and head in args:
                return ''
            if args[0] == 'diff' and head in args:
                return '\n'.join((path, v.PROMOTION))
            return git(*args)

        repo = {'full_name': 'synthetic/repository'}
        pr = {'base': {'sha': base, 'ref': 'main', 'repo': repo},
              'head': {'sha': head, 'ref': 'codex/999-promotion', 'repo': repo},
              'body': f'Issue: #999\nGUI: required\nIntegration: promotion\nVerification: {path}'}
        prs = {}
        for item in registration['changes']:
            data = json.loads(read('show', item['commit'] + ':' + item['source_path']))
            gui = 'required' if data['gui_required'] else 'not-required'
            prs[f'pulls/{item["pr"]}'] = {'merged': True, 'merge_commit_sha': item['commit'],
                'base': {'ref': 'develop', 'repo': repo},
                'head': {'ref': f'codex/{data["issue"]}-source', 'repo': repo},
                'body': f'Issue: #{data["issue"]}\nGUI: {gui}\nIntegration: develop\nVerification: {item["source_path"]}'}

        def api(path):
            return {'object': {'sha': candidate}} if path == 'git/ref/heads/develop' else prs[path]

        result = v.verify_pr(pr, api, virtual_git)
        self.assertEqual((166, 132, 34), (result['cases'], result['initial_cases'], result['deferred_cases']))
        self.assertTrue(result['stage_complete'])
        self.assertFalse(result['gui_complete'])
        self.assertFalse(result['full_acceptance_complete'])
        manifest['results']['104:GUI-FLOW']['status'] = 'deferred'
        with self.assertRaisesRegex(ValueError, 'must pass'):
            v.verify_pr(pr, api, virtual_git)


if __name__ == '__main__':
    unittest.main()
