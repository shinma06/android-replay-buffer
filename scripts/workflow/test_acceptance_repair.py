"""Original-NG repair provenance and integration gates, using disposable Git history."""
import copy
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import agent_loop as al
import test_agent_loop as tal
from agent_policy import next_action, validate_review
from test_agent_loop import pr_data, report
from test_verification import CASE, change
from verification import REPAIR_MARKER, canonical_hash, verify_pr


class RepairTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.case = copy.deepcopy(CASE)
        self.case['gpt'] = {'status': 'fail', 'reason': 'native saved metadata left gap open'}
        self.case['fix_issue'] = 35
        source = change(36)
        source['cases'] = [self.case]
        self.write('docs/verification/changes/issue-36.json', source)
        self.write('replay_buffer/fixture.py', 'original failure')
        self.source = self.commit('original observed candidate')
        self.write('replay_buffer/fixture.py', 'corrected production path')
        self.candidate = self.commit('fix')
        self.path = 'docs/verification/changes/issue-35.json'
        self.issue = {'number': 35, 'url': 'https://api.github.com/repos/example/repo/issues/35',
                      'body': 'Acceptance repair: 123', 'labels': [{'name': 'type:bug'}]}
        at = self.git('show', '-s', '--format=%cI', self.candidate)
        self.old = {'status': 'fail', 'head': self.source, 'artifact_sha256': '1' * 64,
                    'actor': 'gpt', 'observer': 'native-observer', 'at': at,
                    'evidence': 'fixed original failure report', 'loaded_identity': 'original ZIP/JAR',
                    'reason': 'restored rows exist but metadata gap remains open'}
        self.record = {'schema': 1, 'fix_issue': 35, 'failures': {'GAP-1': {
            'case': '36:QA-1', 'source_candidate': self.source,
            'source_case_sha256': canonical_hash(self.case), 'original_result': self.old,
            'guarantees': {'gap': 'target restored closes gap', 'metadata': 'saved rows and available duration agree'}}}}
        observed = dict(self.old, status='pass', head=self.candidate, artifact_sha256='2' * 64,
                        observer='repair-executor', evidence='fixed production-path recheck and artifact',
                        loaded_identity='corrected ZIP/JAR', reason='all original failed guarantees reproduced and corrected')
        self.data = change(gui=False)
        self.data['repair'] = {'candidate': self.candidate, 'artifacts': {'app': '2' * 64},
            'failure_record_sha256': canonical_hash(self.record), 'writer_session': 'writer-35',
            'results': {'GAP-1': {'observation': observed, 'coverage': {
                'gap': 'target restoration production path execution',
                'metadata': 'save metadata and duration execution'},
                'method': 'equivalent production failure path, full saved metadata; native shell limits reviewed',
                'contract_revision': canonical_hash(None)}}}
        self.pr = pr_data()
        self.pr['base'].update(ref='develop', sha=self.source)
        self.pr['body'] = self.pr['body'].replace('tooling', 'develop')
        self.freeze()

    def git(self, *args):
        if args[0] == 'fetch':
            return ''  # All fixture objects already exist; no network or worker is used.
        return subprocess.check_output(['git', *args], cwd=self.root, text=True, stderr=subprocess.DEVNULL).strip()

    def write(self, path, data):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(json.dumps(data, ensure_ascii=False) if isinstance(data, dict) else data)

    def commit(self, message):
        self.git('add', '.')
        self.git('commit', '-qm', message)
        return self.git('rev-parse', 'HEAD')

    def freeze(self):
        self.write(self.path, self.data)
        self.git('add', '.')
        self.git('commit', '-qm', 'evidence', '--allow-empty')
        self.pr['head']['sha'] = self.git('rev-parse', 'HEAD')

    def api(self, path):
        if path == 'git/ref/heads/main':
            return {'object': {'sha': self.source}}
        if path == 'issues/35':
            return self.issue
        if path == 'issues/comments/123':
            return {'issue_url': self.issue['url'], 'body': REPAIR_MARKER + '\n' + json.dumps(self.record)}
        raise AssertionError(path)

    def verify(self):
        return verify_pr(self.pr, self.api, self.git)

    def test_equivalent_method_all_failures_and_independent_assessment(self):
        accepted = dict(self.verify(), allowed=True)
        self.assertEqual(accepted['mode'], 'develop')
        self.assertEqual(accepted['cases'], 0)  # Unrelated pending Cases are not promoted into this recheck.
        bound = {'head': self.pr['head']['sha'], 'base': self.source}
        review = dict(report(), **bound, repair_revision=accepted['repair_revision'],
                      repair_assessment='Read original NG, full production recheck and method limits')
        validate_review(review, bound, accepted)
        self.assertEqual(next_action({'binding': bound, 'review': review}, bound, 'success', False, accepted), 'merge')
        for mutation in ({'repair_revision': ''}, {'repair_revision': '0' * 64}, {'repair_assessment': ''},
                         {'session': 'repair-executor'}, {'session': 'writer-35'}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_review(dict(review, **mutation), bound, accepted)
        with self.assertRaises(ValueError):
            validate_review(dict(report(), **bound), bound, accepted)

    def test_missing_unknown_failed_stale_and_partially_covered_recheck(self):
        original = copy.deepcopy(self.data)
        for mutation in ({'status': 'pending'}, {'status': 'blocked'}, {'status': 'fail'}, {'status': 'unknown'},
                         {'head': self.source}, {'artifact_sha256': '1' * 64}, {'evidence': ''}, {'loaded_identity': ''}):
            self.data = copy.deepcopy(original)
            self.data['repair']['results']['GAP-1']['observation'].update(mutation)
            self.freeze()
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.verify()
        for key, value in (('coverage', {'gap': 'unit only'}), ('method', ''), ('contract_revision', '0' * 64)):
            self.data = copy.deepcopy(original)
            self.data['repair']['results']['GAP-1'][key] = value
            self.freeze()
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify()
        self.data = copy.deepcopy(original)
        self.data['repair']['results'] = {}
        self.freeze()
        with self.assertRaises(ValueError):
            self.verify()

    def test_preserved_registry_must_cover_all_ngs(self):
        self.record['failures']['GAP-2'] = copy.deepcopy(self.record['failures']['GAP-1'])
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'every original failure'):
            self.verify()
        self.data['repair']['results']['GAP-2'] = copy.deepcopy(self.data['repair']['results']['GAP-1'])
        self.freeze()
        self.assertTrue(self.verify()['repair_revision'])
        self.record['failures']['GAP-2']['guarantees']['third'] = 'separate failed guarantee'
        # Even an updated writer hash cannot conceal partial coverage.
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'coverage'):
            self.verify()

    def test_original_failure_identity_and_registry_edits_rejected(self):
        original = copy.deepcopy(self.record)
        for key, value in (('source_case_sha256', '0' * 64), ('case', '36:UNKNOWN')):
            self.record = copy.deepcopy(original)
            self.record['failures']['GAP-1'][key] = value
            self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
            self.freeze()
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify()
        self.record = copy.deepcopy(original)
        self.record['failures']['GAP-1']['original_result']['status'] = 'pass'
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'original failed'):
            self.verify()
        self.record = copy.deepcopy(original)
        self.record['failures']['GAP-1']['guarantees']['new'] = 'edited record'
        with self.assertRaisesRegex(ValueError, 'every original failure'):
            self.verify()

    def test_bug_classification_and_trusted_tracking_cannot_omit_repair(self):
        for body in ('', 'Acceptance repair: unknown', 'Acceptance repair: none'):
            self.issue['body'] = body
            with self.subTest(body=body), self.assertRaises(ValueError):
                self.verify()
        self.issue['labels'] = []
        self.issue['body'] = ''
        del self.data['repair']
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'preserved failure record'):
            self.verify()
        # An ordinary new feature with no linked original failure retains pending/blocked handling.
        self.case['gpt']['status'] = 'pending'
        self.case['fix_issue'] = None
        old = change(36); old['cases'] = [self.case]
        self.write('docs/verification/changes/issue-36.json', old)
        self.pr['base']['sha'] = self.commit('ordinary feature baseline')
        self.freeze()
        self.assertNotIn('repair_revision', self.verify())
        self.issue['labels'] = [{'name': 'type:bug'}]
        self.issue['body'] = 'Acceptance repair: none'
        self.assertEqual(self.verify()['mode'], 'develop')

    def test_metadata_only_history_rejects_later_product_edit_even_if_reverted(self):
        self.write('replay_buffer/fixture.py', 'untested change')
        self.commit('product edit after test')
        self.write('replay_buffer/fixture.py', 'corrected production path')
        self.commit('revert net diff')
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'trusted scope'):
            self.verify()

    def test_current_main_method_used_instead_of_stale_develop(self):
        from verification import INITIAL_PLAN
        trusted = 'f' * 40
        policy = {'responsibility_revision': {'purpose': 'current trusted method'}}
        self.data['repair']['results']['GAP-1']['contract_revision'] = canonical_hash(policy['responsibility_revision'])
        self.freeze()
        original_api, original_git = self.api, self.git
        def api(path):
            return {'object': {'sha': trusted}} if path == 'git/ref/heads/main' else original_api(path)
        def git(*args):
            if args == ('ls-tree', trusted, '--', INITIAL_PLAN):
                return f'100644 blob {trusted}\t{INITIAL_PLAN}'
            return original_git(*args)
        with patch('verification.initial_stage_plan', return_value=policy) as resolve:
            accepted = verify_pr(self.pr, api, git)
            resolve.assert_called_once_with(trusted, self.candidate, git)
            self.assertEqual(accepted['repair_contract_base'], trusted)
        self.data['repair']['results']['GAP-1']['contract_revision'] = canonical_hash({'purpose': 'stale develop method'})
        self.freeze()
        with patch('verification.initial_stage_plan', return_value=policy), self.assertRaisesRegex(ValueError, 'trusted contract'):
            verify_pr(self.pr, api, git)

    def test_real_comment_newline_is_required(self):
        from verification import repair_record
        comment = self.api('issues/comments/123')
        self.assertEqual(comment['body'][len(REPAIR_MARKER)], chr(10))
        self.assertEqual(repair_record(self.issue, self.api), self.record)
        def escaped(path):
            value = self.api(path)
            return dict(value, body=comment['body'].replace(chr(10), r'\n')) if path.endswith('/123') else value
        with self.assertRaises(ValueError):
            repair_record(self.issue, escaped)

    def test_wrong_issue_comment_duplicate_json_and_symlink_record(self):
        original_api = self.api
        for body in (REPAIR_MARKER + '\n{"schema":1,"schema":1}', 'not a record'):
            def api(path):
                if path == 'issues/comments/123':
                    return {'issue_url': self.issue['url'], 'body': body}
                return original_api(path)
            with self.subTest(body=body), self.assertRaises(ValueError):
                verify_pr(self.pr, api, self.git)
        def foreign(path):
            value = original_api(path)
            return dict(value, issue_url='foreign Issue') if path.endswith('/123') else value
        with self.assertRaises(ValueError):
            verify_pr(self.pr, foreign, self.git)
        (self.root / self.path).unlink()
        (self.root / self.path).symlink_to('issue-36.json')
        self.pr['head']['sha'] = self.commit('symlink metadata')
        with self.assertRaisesRegex(ValueError, 'regular JSON'):
            self.verify()

    def test_observation_time_and_frozen_candidate(self):
        self.data['repair']['results']['GAP-1']['observation']['at'] = (
            datetime.now(timezone.utc) - timedelta(days=2)).isoformat(timespec='seconds')
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'predates'):
            self.verify()
        self.data['repair']['candidate'] = self.pr['head']['sha']
        self.write(self.path, self.data)
        # Test the data-only gate against a commit whose record claims itself as the candidate.
        head = self.pr['head']['sha']
        original_git = self.git
        def git(*args):
            if args == ('show', f'{head}:{self.path}'):
                return json.dumps(self.data)
            return original_git(*args)
        with self.assertRaisesRegex(ValueError, 'Freeze'):
            verify_pr(self.pr, self.api, git)



class RepairLoopTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def prepare(self):
        self.gh.issues[35].update(title='[追跡] fixture', labels=['type:tracking', 'priority:P2', 'status:review'])
        self.gh.pull['base']['ref'] = 'develop'
        self.gh.pull['body'] = self.gh.pull['body'].replace('tooling', 'develop')
        handoff, _, _, _ = self.loop.load(36)
        handoff['target'] = 'develop'
        self.gh.messages[36][0]['body'] = al.pack(al.HANDOFF, handoff)
        self.accepted = {'allowed': True, 'mode': 'develop', 'cases': 0, 'repair_revision': 'e' * 64,
                         'repair_observers': ['writer-35', 'repair-executor']}
        self.loop.acceptance.return_value = self.accepted

    def test_coordinator_cannot_status_or_merge_without_actual_repair_assessment(self):
        self.prepare()
        self.loop.tick(36)
        self.assertEqual(self.gh.merges, 0)
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)
        _, state, _, _ = self.loop.load(36)
        self.assertNotIn('review', state)

    def test_coordinator_validated_repair_can_merge(self):
        self.prepare()
        self.worker.side_effect = lambda *args, **kw: dict(report(), repair_revision='e' * 64,
                                                          repair_assessment='all original guarantees independently verified')
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        self.assertEqual(self.loop.tick(36)['phase'], 'done')
        self.assertEqual(self.gh.merges, 1)
        self.assertEqual(self.worker.call_args.args[2]['acceptance'], self.accepted)

    def test_final_registry_or_contract_change_invalidates_prior_assessment(self):
        self.prepare()
        self.worker.side_effect = lambda *args, **kw: dict(report(), repair_revision='e' * 64,
                                                          repair_assessment='all original guarantees independently verified')
        self.loop.tick(36)
        self.loop.acceptance.side_effect = [self.accepted, dict(self.accepted, repair_contract_base=tal.NEW)]
        self.assertEqual(self.loop.tick(36)['phase'], 'changed-before-merge')
        self.assertEqual(self.gh.merges, 0)
        self.assertNotIn((tal.HEAD, 'success'), self.gh.statuses)


if __name__ == '__main__':
    unittest.main()
