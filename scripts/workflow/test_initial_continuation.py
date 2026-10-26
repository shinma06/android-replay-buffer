"""Synthetic results test gate behavior; none are product acceptance evidence."""
import copy
from functools import cache
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from test_agent_loop import BASE, HEAD, NEW
from initial_continuation_cases import CASES, QUALITY_CASES, QUALITY_UPDATES
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
        self.fixture.documents[f'{HEAD}:{v.INITIAL_CONTINUATION}'] = self.registration

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

    def test_full_history_lookup_requires_ancestry_without_extending_initial_admission(self):
        later = 'd' * 40
        self.assertIsNone(v.initial_continuation(self.initial.policy, BASE, later, self.fixture.git))
        self.assertEqual(self.registration, v.initial_continuation(
            self.initial.policy, BASE, later, self.fixture.git, historical=True))
        original = self.fixture.git
        def unrelated(*args):
            if args[:3] == ('merge-base', '--is-ancestor', NEW) and args[-1] in (later, BASE):
                raise subprocess.CalledProcessError(1, ['git', *args])
            return original(*args)
        self.assertIsNone(v.initial_continuation(self.initial.policy, BASE, later, unrelated, historical=True))
        def adopted(*args):
            if args == ('merge-base', '--is-ancestor', NEW, later):
                raise subprocess.CalledProcessError(1, ['git', *args])
            return original(*args)
        with self.assertRaisesRegex(ValueError, 'already-adopted'):
            v.initial_continuation(self.initial.policy, BASE, later, adopted, historical=True)
        for target in (later, BASE):
            def unavailable(*args):
                if args == ('merge-base', '--is-ancestor', NEW, target):
                    raise subprocess.CalledProcessError(128, ['git', *args])
                return unrelated(*args)
            with self.subTest(unavailable=target), self.assertRaises(ValueError):
                v.initial_continuation(self.initial.policy, BASE, later, unavailable, historical=True)

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

    def test_later_full_gate_and_queue_retain_added_contracts_after_the_batch_is_in_main(self):
        later = 'd' * 40
        self.manifest.pop('stage')
        self.manifest['candidate'] = later
        self.manifest['changes'] = [{'commit': later, 'pr': 101}]
        self.fixture.range = [later]
        self.fixture.source['merge_commit_sha'] = later
        self.fixture.source['head']['ref'] = 'codex/38-source'
        self.fixture.source['body'] = ('Issue: #38\nGUI: required\nIntegration: develop\n'
                                       'Verification: docs/verification/changes/issue-38.json')
        self.fixture.documents[f'{later}:docs/verification/changes/issue-38.json'] = change(38)
        self.fixture.documents[f'{later}:{v.INITIAL_PLAN}'] = copy.deepcopy(self.initial.policy)
        case = self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json']['cases'][0]
        case['required_execution'] = 'computer_use'
        self.registration['cases']['37:QA-1']['current_case_sha256'] = v.canonical_hash(case)
        self.registration['cases']['37:QA-1']['sources'][0]['original_case_sha256'] = v.canonical_hash(case)
        self.fixture.documents[f'{later}:docs/verification/changes/issue-37.json'] = copy.deepcopy(
            self.fixture.documents[f'{NEW}:docs/verification/changes/issue-37.json'])
        self.manifest['results'] = {key: dict(observation(), head=later, execution='computer_use')
                                   for key in (*self.initial.policy['cases'], '38:QA-1')}
        with self.subTest(missing='37:QA-1'), self.assertRaisesRegex(ValueError, 'missing=.*37:QA-1'):
            self.verify()
        with self.subTest(queue='retained'):
            output = self.render()
            self.assertIn('QA-1 / #37', output)
            self.assertIn('固定原契約 / PR #100', output)
            self.assertNotIn('Case合格（', output.split('## #37', 1)[-1])
        self.manifest['results']['37:QA-1'] = dict(observation(), head=later)
        with self.subTest(execution='missing'), self.assertRaisesRegex(ValueError, 'execution method'):
            self.verify()
        self.manifest['results']['37:QA-1']['execution'] = 'computer_use'
        self.assertEqual(4, self.verify()['cases'])
        for status in ('initial-pass', 'deferred', 'pending', 'blocked', 'fail'):
            self.manifest['results']['37:QA-1']['status'] = status
            with self.subTest(status=status), self.assertRaisesRegex(ValueError, 'must pass'):
                self.verify()
        self.manifest['results']['37:QA-1']['status'] = 'pass'
        current = self.fixture.documents[f'{later}:docs/verification/changes/issue-37.json']['cases'][0]
        current['expected'] += ' Current contract also applies.'
        with self.subTest(current_hash='missing'), self.assertRaisesRegex(ValueError, 'exact current'):
            self.verify()
        self.manifest['results']['37:QA-1']['case_sha256'] = v.canonical_hash(current)
        self.assertTrue(self.verify()['gui_complete'])
        output = self.render()
        self.assertIn('Current contract also applies.', output)
        self.assertIn('原期待結果: ' + case['expected'], output)
        self.assertIn('Case合格（', output)
        current.pop('required_execution')
        self.manifest['results']['37:QA-1']['case_sha256'] = v.canonical_hash(current)
        self.manifest['results']['37:QA-1'].pop('execution')
        with self.subTest(original_execution='preserved'), self.assertRaisesRegex(ValueError, 'execution method'):
            self.verify()
        row = next(line for line in self.render().splitlines() if line.startswith('| QA-1 / #37'))
        self.assertNotIn('Case合格（', row)
        self.manifest['results']['37:QA-1']['execution'] = 'computer_use'
        self.assertTrue(self.verify()['gui_complete'])
        current['artifact'] = 'plugin'
        with self.subTest(original_artifact='preserved'), self.assertRaisesRegex(ValueError, 'artifact contract'):
            self.verify()
        current.pop('artifact')
        del self.fixture.documents[f'{later}:docs/verification/changes/issue-37.json']
        self.assertIn('QA-1 / #37', self.render())
        self.assertEqual(4, self.verify()['cases'])

    def test_metadata_carry_keeps_only_exact_main_policy_and_case_free_records(self):
        record_path = 'docs/verification/changes/issue-356.json'
        product_path = 'docs/verification/changes/issue-37.json'
        script = 'scripts/workflow/verification.py'
        fixture = 'scripts/workflow/initial_continuation_cases.py'
        unknown = 'docs/verification/unknown.json'
        required = {v.INITIAL_CONTINUATION, record_path, 'docs/verification/changes/issue-364.json', script, fixture,
                    'scripts/workflow/test_initial_continuation.py',
                    'scripts/workflow/test_impact_evidence_reuse.py', 'docs/verification/README.md',
                    'scripts/workflow/test_agent_loop.py',
                    *('scripts/workflow/' + name + '.py' for name in
                      ('git_guard', 'agent_policy', 'issue_schema', 'handoff_registry', 'agent_worker'))}
        unrelated = ['AGENTS.md', 'README.md', 'docs/android-studio.md', 'docs/design/timeline.md',
                     'docs/project.md', 'docs/requirements.md', 'docs/setup/ci.md',
                     'scripts/workflow/test_change_impact.py', 'scripts/workflow/test_product_check.py',
                     'CONTRIBUTING.md', 'docs/verification/changes/issue-999.json']
        paths = [*required, product_path, unknown, *unrelated]
        records = {record_path: change(356, gui=False),
                   'docs/verification/changes/issue-364.json': change(364, gui=False), product_path: change(37),
                   unrelated[-1]: change(999, gui=False), v.INITIAL_CONTINUATION: self.registration}
        classifier = {BASE: '364b42de6cffde531ad631ae88e20a4271b76d7f',
                      NEW: '8beb4c9aa39196f10a76b6cea2ec757ffa52569c'}
        modules = {script: 'from agent_policy import api\nfrom git_guard import api\nfrom change_impact import api',
                   'scripts/workflow/agent_policy.py': 'from issue_schema import api\nfrom handoff_registry import api',
                   'scripts/workflow/handoff_registry.py': 'from agent_worker import api',
                   'scripts/workflow/agent_worker.py': 'from verification import api',
                   'scripts/workflow/issue_schema.py': '', 'scripts/workflow/git_guard.py': '',
                   'scripts/workflow/change_impact.py': '',
                   'scripts/workflow/test_initial_continuation.py': 'from test_agent_loop import api',
                   'scripts/workflow/test_impact_evidence_reuse.py': '',
                   'scripts/workflow/test_agent_loop.py': ''}

        def git(*args):
            if args[0] == 'merge-base':
                return BASE
            if args[0] == 'diff':
                return '\n'.join(paths)
            if args[0] == 'ls-tree':
                if args[-1] == 'scripts/workflow/change_impact.py':
                    return f'100644 blob {classifier[args[1]]}\t{args[-1]}'
                if args[-1] not in paths and args[-1] not in modules:
                    return ''
                if args[1] == NEW:
                    return ''
                return f'100644 blob {"0" * 40}\t{args[-1]}'
            if args[0] == 'show':
                path = args[1].split(':', 1)[1]
                return modules[path] if path in modules else json.dumps(records[path])
            raise AssertionError(args)

        for continuation in (False, True):
            with self.subTest(continuation=continuation), patch.object(
                    v, 'initial_stage_plan', return_value=self.initial.policy):
                carried = v.promotion_policy_blobs(BASE, NEW, git, continuation=continuation)
                self.assertEqual(required, set(carried))
        classifier[BASE] = 'f' * 40
        for continuation in (False, True):
            with self.subTest(changed_classifier=continuation), patch.object(
                    v, 'initial_stage_plan', return_value=self.initial.policy), self.assertRaisesRegex(
                        ValueError, 'classifier'):
                v.promotion_policy_blobs(BASE, NEW, git, continuation=continuation)
        classifier[BASE] = classifier[NEW]
        records[record_path] = change(356)
        with patch.object(v, 'initial_stage_plan', return_value=self.initial.policy), self.assertRaises(ValueError):
            v.promotion_policy_blobs(BASE, NEW, git, continuation=True)


class RegisteredContinuationTests(unittest.TestCase):
    def test_historical_818b_registration_keeps_167_contracts_and_236_sources(self):
        root = Path(__file__).resolve().parents[2]
        registration = json.loads(subprocess.check_output(['git', 'show',
            '1e49596da78b0e0529dd336b73d18c73a2fd12de:' + v.INITIAL_CONTINUATION], cwd=root, text=True))
        self.check_registration(registration, '818b7e1adbcd0af79e146d1795f833b75a1b1302',
                                (167, 236, 28, 23))

    def test_quality_registration_keeps_169_contracts_and_238_sources(self):
        root = Path(__file__).resolve().parents[2]
        registration = json.loads((root / v.INITIAL_CONTINUATION).read_text())
        self.check_registration(registration, '210ed0473c3f87e02f3ea065506d4616b6fdadd8',
                                (169, 238, 30, 24))

    def check_registration(self, registration, expected_candidate, counts):
        root = Path(__file__).resolve().parents[2]
        policy = json.loads((root / v.INITIAL_PLAN).read_text())
        self.assertEqual(expected_candidate, registration['candidate'])
        total, source_count, extra_count, commit_count = counts
        quality = expected_candidate == '210ed0473c3f87e02f3ea065506d4616b6fdadd8'
        warning = registration['cases']['347:SAVE-WARNING-WINDOW']
        self.assertEqual('aa9b28b5d6eec07756c29ffce51dda1192926004e8c4ac99aac5d3c37f216a4b',
                         warning['current_case_sha256'])
        self.assertEqual([warning['current_case_sha256']],
                         [source['original_case_sha256'] for source in warning['sources']])
        base, candidate = 'c66363ea0b27add98de9853ccf87728235662b17', registration['candidate']
        read = cache(lambda *args: subprocess.check_output(['git', *args], cwd=root, text=True).strip())

        # Exact original contracts and fixed quality changes; the DAG/PR metadata remain synthetic.
        # Historical 139-Case documents are read only from main's reachable history.
        cases = copy.deepcopy(CASES)
        if quality:
            cases.update(copy.deepcopy(QUALITY_CASES))
        self.assertEqual({item['source_path'] for item in registration['changes']}, set(cases))
        documents = {}
        parents = {}
        previous = base
        for item in registration['changes']:
            path = item['source_path']
            data = change(int(Path(path).stem.removeprefix('issue-')), gui=bool(cases[path]))
            data['cases'] = cases[path]
            documents[f'{item["commit"]}:{path}'] = data
            documents[f'{candidate}:{path}'] = data
            parents[item['commit']] = previous
            previous = item['commit']
        self.assertEqual(candidate, previous)
        if quality:
            for path, updates in QUALITY_UPDATES.items():
                data = copy.deepcopy(documents[f'{candidate}:{path}'])
                for case in data['cases']:
                    case.update(updates.get(case['id'], {}))
                documents[f'{candidate}:{path}'] = data
            old = json.loads(read('show',
                '1e49596da78b0e0529dd336b73d18c73a2fd12de:' + v.INITIAL_CONTINUATION))
            for key, entry in old['cases'].items():
                self.assertEqual(entry['sources'], registration['cases'][key]['sources'])
            self.assertNotEqual(old['cases']['274:LOG-ONLY-LOAD']['current_case_sha256'],
                                registration['cases']['274:LOG-ONLY-LOAD']['current_case_sha256'])

        def git(*args):
            if args == ('ls-tree', base, '--', v.INITIAL_CONTINUATION):
                return f'100644 blob {"0" * 40}\t{v.INITIAL_CONTINUATION}'
            if args == ('show', base + ':' + v.INITIAL_CONTINUATION):
                return json.dumps(registration)
            if args[0] == 'ls-tree' and f'{args[1]}:{args[-1]}' in documents:
                return f'100644 blob {"0" * 40}\t{args[-1]}'
            if args[0] == 'show' and args[1] in documents:
                return json.dumps(documents[args[1]])
            if args[:2] == ('ls-tree', candidate):
                return read('ls-tree', base, '--', args[-1])
            if args[0] == 'show' and args[1].startswith(candidate + ':'):
                return read('show', base + ':' + args[1].split(':', 1)[1])
            if args[:2] == ('merge-base', '--is-ancestor') and args[-1] == candidate:
                self.assertIn(args[2], {policy['required_ancestor'], *parents,
                    *(source['source_merge'] for entry in policy['cases'].values() for source in entry['sources'])})
                return ''
            if args == ('merge-base', candidate, base):
                return base
            if args[:3] == ('diff', '--no-renames', '--name-only') and args[3:] in (
                    (base, base), (candidate, base)):
                return ''
            if args == ('rev-list', f'{base}..{candidate}'):
                return '\n'.join(parents)
            if args[:4] == ('rev-list', '--parents', '-n', '1') and args[-1] in parents:
                return args[-1] + ' ' + parents[args[-1]]
            self.assertFalse(any(ref in arg for ref in parents for arg in args), args)
            self.assertNotEqual('fetch', args[0])
            return read(*args)

        self.assertEqual('0b6d9c6d2d5054ed642565afdbaeb5a88ee198a4ea5398ff96ff36168ce0eda5',
                         v.canonical_hash(policy))
        self.assertEqual(policy, v.initial_stage_plan(base, candidate, git))
        extra = v.historical_initial_sources(registration, candidate, git)
        changes = [{k: item[k] for k in ('commit', 'pr')} for item in registration['changes']]
        sources = v.initial_stage_sources(policy, v.initial_continuation(policy, base, candidate, git),
                                         None, changes, candidate, git)
        self.assertEqual((total, source_count, extra_count, commit_count), (len({k for k, _, _ in sources}), len(sources),
                                            len(extra), len(registration['changes'])))
        later = 'd' * 40
        def later_git(*args):
            if args == ('merge-base', '--is-ancestor', candidate, later):
                return ''
            return git(*args)
        self.assertIsNone(v.initial_continuation(policy, base, later, later_git))
        historical_registration = v.initial_continuation(policy, base, later, later_git, historical=True)
        full_sources = v.historical_initial_sources(policy, later, later_git, historical_registration)
        self.assertEqual((total, source_count), (len({key for key, _, _ in full_sources}), len(full_sources)))
        self.assertIn('347:SAVE-WARNING-WINDOW', {key for key, _, _ in full_sources})
        manifest = {'stage': v.INITIAL_STAGE, 'base': base, 'candidate': candidate,
                    'changes': changes, 'results': {}}
        output = v.render_queue([], manifest, git)
        self.assertEqual(total, sum(line.startswith('| ') and ' / #' in line for line in output.splitlines()))
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
            data = json.loads(git('show', item['commit'] + ':' + item['source_path']))
            gui = 'required' if data['gui_required'] else 'not-required'
            prs[f'pulls/{item["pr"]}'] = {'merged': True, 'merge_commit_sha': item['commit'],
                'base': {'ref': 'develop', 'repo': repo},
                'head': {'ref': f'codex/{data["issue"]}-source', 'repo': repo},
                'body': f'Issue: #{data["issue"]}\nGUI: {gui}\nIntegration: develop\nVerification: {item["source_path"]}'}

        def api(path):
            return {'object': {'sha': candidate}} if path == 'git/ref/heads/develop' else prs[path]

        result = v.verify_pr(pr, api, virtual_git)
        self.assertEqual((total, total - 34, 34), (result['cases'], result['initial_cases'], result['deferred_cases']))
        self.assertTrue(result['stage_complete'])
        self.assertFalse(result['gui_complete'])
        self.assertFalse(result['full_acceptance_complete'])
        keys = ['104:GUI-FLOW', '347:SAVE-WARNING-WINDOW']
        if quality:
            keys += ['282:VIDEO-QUALITY-SETTINGS', '282:VIDEO-QUALITY-CAPTURE']
        for key in keys:
            for status in ('pending', 'blocked', 'fail', 'deferred', 'initial-pass'):
                manifest['results'][key]['status'] = status
                with self.subTest(key=key, status=status), self.assertRaisesRegex(ValueError, 'must pass'):
                    v.verify_pr(pr, api, virtual_git)
            manifest['results'][key]['status'] = 'pass'


if __name__ == '__main__':
    unittest.main()
