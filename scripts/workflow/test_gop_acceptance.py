"""GOP amendment trust/provenance tests; synthetic observations are not product QA."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from test_agent_loop import pr_data, HEAD, BASE, NEW
from test_verification import CASE, change, observation
from verification import (GOP_AMENDMENT, GOP_EVIDENCE, canonical_hash, effective_gop_cases,
                          gop_amendment, render_queue, verify_pr)


class GopAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads((Path(__file__).resolve().parents[2] / GOP_AMENDMENT).read_text())
        self.policy['required_ancestor'] = BASE
        self.pr = pr_data()
        self.pr['body'] = self.pr['body'].replace('tooling', 'promotion')
        self.sources = {}
        self.documents = {f'{HEAD}:docs/verification/changes/issue-35.json': change(gui=False)}
        for key, bindings in self.policy['cases'].items():
            issue, case_id = key.split(':')
            case = dict(copy.deepcopy(CASE), id=case_id, artifact='plugin',
                        preconditions='専用環境。1秒程度の実測GOP。固定ZIP。')
            if issue == '13':
                case['required_execution'] = 'computer_use'
            for binding in bindings:
                binding['original_case_sha256'] = canonical_hash(case)
                number = binding['source_pr']
                source = self.sources.setdefault(number, copy.deepcopy(self.pr))
                source.update(merged=True, merge_commit_sha=binding['source_merge'])
                source['base']['ref'] = 'develop'
                source['body'] = (f'Issue: #{issue}\nGUI: required\nIntegration: develop\n'
                                  f'Verification: {binding["source_path"]}')
                path = f'{binding["source_merge"]}:{binding["source_path"]}'
                data = self.documents.setdefault(path, dict(change(int(issue)), cases=[]))
                data['cases'].append(copy.deepcopy(case))
        self.manifest = {'schema': 1, 'base': BASE, 'candidate': NEW,
                         'artifacts': {'plugin': 'e' * 64},
                         'changes': [{'commit': s['merge_commit_sha'], 'pr': n}
                                     for n, s in self.sources.items()],
                         'results': {key: dict(observation(), execution='computer_use',
                                      gop_revision=canonical_hash(self.policy),
                                      gop_evidence={k: 'https://example.invalid/' + k for k in GOP_EVIDENCE})
                                     for key in self.policy['cases']}}
        self.documents[f'{BASE}:{GOP_AMENDMENT}'] = copy.deepcopy(self.policy)
        self.documents[f'{NEW}:{GOP_AMENDMENT}'] = copy.deepcopy(self.policy)
        self.modes = {}
        self.raw = {}

    def git(self, *args):
        if args[0] == 'show':
            if args[1].endswith(':docs/verification/promotion.json'):
                return json.dumps(self.manifest)
            return self.raw.get(args[1], json.dumps(self.documents[args[1]]))
        if args[0] == 'ls-tree':
            ref, path = args[1], args[-1]
            if f'{ref}:{path}' not in self.documents:
                return ''
            return f'{self.modes.get((ref, path), "100644")} blob {"0" * 40}\t{path}'
        if args[0] == 'diff':
            return 'docs/verification/promotion.json'
        if args[:2] == ('rev-list', '--parents'):
            return args[-1] + ' ' + BASE
        if args[0] == 'rev-list':
            return '' if '--not' in args else '\n'.join(x['commit'] for x in self.manifest['changes'])
        if args[0] in ('fetch', 'merge-base'):
            return ''
        raise AssertionError(args)

    def api(self, path):
        return ({'object': {'sha': NEW}} if path.startswith('git/ref') else
                self.sources[int(path.split('/')[-1])])

    def verify(self):
        return verify_pr(self.pr, self.api, self.git)

    def rows(self):
        result = []
        for key, sources in self.policy['cases'].items():
            for s in sources:
                data = self.documents[f'{s["source_merge"]}:{s["source_path"]}']
                case = next(c for c in data['cases'] if c['id'] == key.split(':')[1])
                result.append((key, case, {k: s[k] for k in ('source_pr', 'source_merge', 'source_path')}))
        return result

    def test_all_fifteen_sources_and_nine_results_preserve_the_contract(self):
        self.assertEqual(self.verify()['cases'], 9)
        contracts = effective_gop_cases(self.policy, self.rows())
        for key, original, _ in self.rows():
            amended = contracts[key]['case']
            for field in set(original) - {'preconditions', 'steps'}:
                self.assertEqual(amended[field], original[field])
            self.assertEqual(amended['steps'][:len(original['steps'])], original['steps'])
            self.assertIn('専用環境', amended['preconditions'])
            self.assertIn('約1秒GOP', amended['preconditions'])
            self.assertEqual(contracts[key]['revision'], canonical_hash(self.policy))
        self.manifest['changes'].reverse()
        self.assertEqual(self.verify()['cases'], 9)

    def test_missing_or_extra_source_occurrence_cannot_select_a_duplicate_winner(self):
        saved = copy.deepcopy(self.manifest)
        self.manifest['changes'] = [c for c in self.manifest['changes'] if c['pr'] != 33]
        with self.assertRaisesRegex(ValueError, 'every original source'):
            self.verify()
        self.manifest = saved
        rows = self.rows()
        with self.assertRaisesRegex(ValueError, 'every original source'):
            effective_gop_cases(self.policy, rows + [rows[0]])

    def test_original_contract_and_artifact_or_execution_changes_are_rejected(self):
        s = self.policy['cases']['13:SYNC-WINDOW'][0]
        path = f'{s["source_merge"]}:{s["source_path"]}'
        saved = copy.deepcopy(self.documents[path])
        for field, value in (('expected', '無条件で成功'), ('artifact', 'app'),
                             ('required_execution', None), ('steps', ['元操作を省略'])):
            self.documents[path] = copy.deepcopy(saved)
            self.documents[path]['cases'][0][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'every original source'):
                self.verify()

    def test_contradictory_duplicate_contracts_are_rejected(self):
        s = self.policy['cases']['27:BUFFER-SHORT-EMU'][0]
        data = self.documents[f'{s["source_merge"]}:{s["source_path"]}']
        next(c for c in data['cases'] if c['id'] == 'BUFFER-SHORT-EMU')['expected'] = '異なる期待'
        with self.assertRaisesRegex(ValueError, 'Contradictory'):
            self.verify()

    def test_wrong_pr_merge_path_hash_and_unknown_keys_fail_closed(self):
        saved = copy.deepcopy(self.policy)
        for field, value in (('source_pr', 999), ('source_merge', 'f' * 40),
                             ('source_path', 'docs/verification/changes/issue-999.json'),
                             ('original_case_sha256', '0' * 64)):
            policy = copy.deepcopy(saved)
            policy['cases']['13:SYNC-WINDOW'][0][field] = value
            for ref in (BASE, NEW):
                self.documents[f'{ref}:{GOP_AMENDMENT}'] = copy.deepcopy(policy)
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.verify()
        policy = copy.deepcopy(saved)
        policy['cases']['99:UNKNOWN'] = policy['cases'].pop('13:SYNC-WINDOW')
        for ref in (BASE, NEW):
            self.documents[f'{ref}:{GOP_AMENDMENT}'] = copy.deepcopy(policy)
        with self.assertRaisesRegex(ValueError, 'limited GOP'):
            self.verify()

    def test_candidate_missing_changed_or_symlink_policy_cannot_authorize_itself(self):
        path = f'{NEW}:{GOP_AMENDMENT}'
        saved = self.documents.pop(path)
        with self.assertRaisesRegex(ValueError, 'regular JSON'):
            self.verify()
        self.documents[path] = copy.deepcopy(saved)
        self.documents[path]['runtime_premise'] = 'すべて許容'
        with self.assertRaisesRegex(ValueError, 'exact trusted'):
            self.verify()
        self.documents[path] = saved
        self.modes[NEW, GOP_AMENDMENT] = '120000'
        with self.assertRaisesRegex(ValueError, 'regular JSON'):
            self.verify()

    def test_untrusted_head_only_revision_is_not_applied(self):
        self.documents[f'{HEAD}:{GOP_AMENDMENT}'] = self.documents.pop(f'{BASE}:{GOP_AMENDMENT}')
        self.assertIsNone(gop_amendment(BASE, NEW, self.git))
        with self.assertRaisesRegex(ValueError, 'not applicable'):
            self.verify()

    def test_duplicate_json_keys_or_unrelated_candidate_rejected(self):
        path = f'{BASE}:{GOP_AMENDMENT}'
        self.raw[path] = json.dumps(self.policy)[:-1] + ', "schema": 1}'
        with self.assertRaisesRegex(ValueError, 'Duplicate key'):
            self.verify()
        self.raw.clear()
        git = self.git
        self.git = lambda *args: (_ for _ in ()).throw(ValueError('wrong ancestor')) if (
            args == ('merge-base', '--is-ancestor', self.policy['required_ancestor'], NEW)) else git(*args)
        with self.assertRaisesRegex(ValueError, 'wrong ancestor'):
            self.verify()

    def test_missing_revision_boundary_proof_or_old_build_never_passes(self):
        key = '13:SYNC-WINDOW'
        saved = copy.deepcopy(self.manifest['results'][key])
        for values in ({'gop_revision': None}, {'gop_revision': '0' * 64},
                       {'gop_evidence': {}}, {'head': HEAD}, {'artifact_sha256': '0' * 64},
                       {'status': 'pending'}, {'status': 'blocked'}, {'status': 'fail'},
                       {'execution': 'manual'}):
            self.manifest['results'][key] = dict(saved, **values)
            with self.subTest(values=values), self.assertRaises(ValueError):
                self.verify()
        self.manifest['results'][key] = saved
        del self.manifest['results']['27:BUFFER-180-REAL']
        with self.assertRaisesRegex(ValueError, 'ALL required'):
            self.verify()

    def test_queue_uses_same_revision_preserves_history_and_refuses_current_rewrite(self):
        s = self.policy['cases']['13:SYNC-WINDOW'][0]
        data = copy.deepcopy(self.documents[f'{s["source_merge"]}:{s["source_path"]}'])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            path.write_text(json.dumps(data))
            output = render_queue([path], self.manifest, self.git)
            self.assertIn(canonical_hash(self.policy), output)
            self.assertIn('元の前提（履歴）: 専用環境。1秒程度の実測GOP', output)
            self.assertIn(self.policy['additional_steps'][0], output)
            self.assertIn('Case合格（', output)
            self.manifest['results']['13:SYNC-WINDOW'].pop('gop_revision')
            self.assertNotIn('Case合格（', render_queue([path], self.manifest, self.git))
            data['cases'][0]['expected'] = '候補側だけの緩和'
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'historical GOP'):
                render_queue([path], self.manifest, self.git)

    def test_queue_reads_historical_sources_without_requiring_them_in_current_changes(self):
        source = self.policy['cases']['13:SYNC-WINDOW'][0]
        data = self.documents[f'{source["source_merge"]}:{source["source_path"]}']
        self.manifest['changes'] = []
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            path.write_text(json.dumps(data))
            output = render_queue([path], self.manifest, self.git)
            self.assertIn('Case合格（', output)
            self.assertIn(source['source_merge'], output)
            git = self.git
            self.git = lambda *args: (_ for _ in ()).throw(ValueError('unrelated source')) if (
                args == ('merge-base', '--is-ancestor', source['source_merge'], NEW)) else git(*args)
            with self.assertRaisesRegex(ValueError, 'unrelated source'):
                render_queue([path], self.manifest, self.git)

    def test_queue_shows_observed_revision_and_all_boundary_references_including_missing(self):
        source = self.policy['cases']['13:SYNC-WINDOW'][0]
        data = self.documents[f'{source["source_merge"]}:{source["source_path"]}']
        result = self.manifest['results']['13:SYNC-WINDOW']
        result['gop_revision'] = 'f' * 64
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'case.json'
            path.write_text(json.dumps(data))
            output = render_queue([path], self.manifest, self.git)
            self.assertIn('観察GOP改訂revision: ' + 'f' * 64, output)
            for name, reference in result['gop_evidence'].items():
                self.assertIn(f'GOP観察証拠 ({name}): {reference}', output)
            self.assertNotIn('Case合格（', output)
            result['gop_revision'] = canonical_hash(self.policy)
            result['gop_evidence'].pop('mid_gop')
            self.assertIn('GOP観察証拠 (mid_gop): 未登録', render_queue([path], self.manifest, self.git))
            self.assertNotIn('Case合格（', render_queue([path], self.manifest, self.git))
