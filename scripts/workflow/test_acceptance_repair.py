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
        self.case['artifact'] = 'plugin'
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
        self.record_created = '2026-01-01T00:00:00Z'
        self.record_updated = self.record_created
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
        self.data['repair'] = {'candidate': self.candidate, 'artifacts': {'plugin': '2' * 64},
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
            return {'issue_url': self.issue['url'], 'body': REPAIR_MARKER + '\n' + json.dumps(self.record),
                    'created_at': self.record_created, 'updated_at': self.record_updated}
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

    def test_edited_original_cannot_shrink_guarantees_even_with_matching_results(self):
        self.record_updated = '2026-01-01T00:00:01Z'
        del self.record['failures']['GAP-1']['guarantees']['metadata']
        del self.data['repair']['results']['GAP-1']['coverage']['metadata']
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'unedited'):
            self.verify()

    def test_missing_or_invalid_original_comment_metadata_is_rejected(self):
        for timestamp in (None, '', 'unknown', '2026-01-01T00:00:00'):
            self.record_created = self.record_updated = timestamp
            with self.subTest(timestamp=timestamp), self.assertRaises(ValueError):
                self.verify()

    def test_new_unedited_record_id_keeps_schema_and_requires_fresh_assessment(self):
        from verification import repair_record
        original_api = self.api
        original = self.verify()
        preserved = copy.deepcopy(self.record)
        old_comment = original_api('issues/comments/123')
        self.issue['body'] = 'Acceptance repair: 124'
        self.record['failures']['GAP-1']['guarantees']['third'] = 'additional observed failure'
        def api(path):
            if path == 'issues/comments/124': return original_api('issues/comments/123')
            if path == 'issues/comments/123': return old_comment
            return original_api(path)
        self.assertEqual(repair_record(self.issue, api), self.record)
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.data['repair']['results']['GAP-1']['coverage']['third'] = 'fresh fixed evidence'
        self.freeze()
        updated = verify_pr(self.pr, api, self.git)
        self.assertNotEqual(updated['repair_revision'], original['repair_revision'])
        self.assertEqual(set(updated['repair_record']), set(preserved))
        self.assertEqual(json.loads(api('issues/comments/123')['body'][len(REPAIR_MARKER):]), preserved)
        bound = {'head': self.pr['head']['sha'], 'base': self.source}
        review = dict(report(), **bound, repair_revision=original['repair_revision'],
                      repair_assessment='historical assessment')
        with self.assertRaisesRegex(ValueError, 'stale'):
            validate_review(review, bound, updated)

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
        # An unrelated Issue with no linked historical failure remains an ordinary change.
        self.issue.update(number=100, body='')
        self.path = 'docs/verification/changes/issue-100.json'
        self.pr['head']['ref'] = 'codex/100-ordinary-feature'
        self.pr['body'] = self.pr['body'].replace('#35', '#100').replace('issue-35', 'issue-100')
        self.data = change(100, gui=False)
        self.freeze()
        original_api = self.api
        api = lambda path: self.issue if path == 'issues/100' else original_api(path)
        self.assertNotIn('repair_revision', verify_pr(self.pr, api, self.git))
        self.issue['labels'] = [{'name': 'type:bug'}]
        self.issue['body'] = 'Acceptance repair: none'
        self.assertEqual(verify_pr(self.pr, api, self.git)['mode'], 'develop')

    def test_historical_failure_cannot_disappear_at_base_tip(self):
        for mutation in ('pending', 'pass', 'delete', 'unlink'):
            with self.subTest(mutation=mutation):
                self.git('checkout', '-q', self.source)
                source = change(36)
                source['cases'] = [copy.deepcopy(self.case)]
                if mutation == 'delete':
                    (self.root / 'docs/verification/changes/issue-36.json').unlink()
                else:
                    if mutation == 'unlink': source['cases'][0]['fix_issue'] = 100
                    else: source['cases'][0]['gpt']['status'] = mutation
                    self.write('docs/verification/changes/issue-36.json', source)
                self.pr['base']['sha'] = self.commit('later develop Case edit')
                self.issue['body'] = 'Acceptance repair: none'
                self.data.pop('repair', None)
                self.freeze()
                with self.assertRaisesRegex(ValueError, 'preserved failure record'):
                    self.verify()

    def test_merged_and_squashed_failure_then_revert_is_not_lost(self):
        for merged in (False, True):
            with self.subTest(merged=merged):
                self.git('checkout', '--orphan', 'baseline-' + str(merged))
                pending = change(36)
                pending['cases'][0]['artifact'] = 'plugin'
                self.write('docs/verification/changes/issue-36.json', pending)
                # This root has no earlier linked fail; the side-branch is the only source.
                baseline = self.commit('ordinary baseline')
                self.git('checkout', '-b', 'failure-' + str(merged))
                failed = change(36); failed['cases'] = [copy.deepcopy(self.case)]
                self.write('docs/verification/changes/issue-36.json', failed)
                failure_commit = self.commit('observed failure on topic branch')
                if not merged:
                    self.git('checkout', '-q', baseline)
                    self.git('merge', '--squash', failure_commit)
                    self.commit('actual squash into develop')
                self.write('docs/verification/changes/issue-36.json', pending)
                failure_tip = self.commit('revert failure status')
                if merged:
                    self.git('checkout', '-q', baseline)
                    self.write('docs/unrelated.md', 'independent mainline edit')
                    self.commit('mainline change')
                    self.git('merge', '--no-ff', '-m', 'real merge', failure_tip)
                self.pr['base']['sha'] = self.git('rev-parse', 'HEAD')
                self.issue['body'] = 'Acceptance repair: none'
                self.data.pop('repair', None)
                self.freeze()
                with self.assertRaisesRegex(ValueError, 'preserved failure record'):
                    self.verify()

    def test_native_record_for_original_pending_case_remains_valid(self):
        self.git('checkout', '--orphan', 'native-source')
        self.case['gpt']['status'] = 'pending'
        self.case['fix_issue'] = None
        source = change(36); source['cases'] = [self.case]
        self.write('docs/verification/changes/issue-36.json', source)
        self.source = self.commit('original pending Case before native NG')
        self.write('replay_buffer/fixture.py', 'native failure corrected')
        self.candidate = self.commit('native fix')
        self.pr['base']['sha'] = self.source
        failure = self.record['failures']['GAP-1']
        failure.update(source_candidate=self.source, source_case_sha256=canonical_hash(self.case))
        failure['original_result']['head'] = self.source
        self.data['repair']['candidate'] = self.candidate
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.data['repair']['results']['GAP-1']['observation'].update(
            head=self.candidate, at=self.git('show', '-s', '--format=%cI', self.candidate))
        self.freeze()
        self.assertTrue(self.verify()['repair_revision'])

    def test_historical_failed_actor_requires_its_actual_case_and_association(self):
        self.git('checkout', '-q', self.source)
        source = change(36)
        source['cases'] = [copy.deepcopy(self.case)]
        source['cases'][0]['gpt']['status'] = 'pending'
        source['cases'][0]['human'] = {'status': 'fail', 'reason': 'human observed failure'}
        self.write('docs/verification/changes/issue-36.json', source)
        human_source = self.commit('human failure linked to same fix')
        source['cases'][0]['human']['status'] = 'pending'
        self.write('docs/verification/changes/issue-36.json', source)
        self.pr['base']['sha'] = self.commit('later pending status')
        self.write('replay_buffer/fixture.py', 'corrected production path')
        self.candidate = self.commit('new correction')
        self.data['repair']['candidate'] = self.candidate
        self.data['repair']['results']['GAP-1']['observation'].update(
            head=self.candidate, at=self.git('show', '-s', '--format=%cI', self.candidate))
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'tracked original failure'):
            self.verify()
        # A separate preserved human original covers that actor without rewriting the old record.
        self.record['failures']['HUMAN-1'] = dict(copy.deepcopy(self.record['failures']['GAP-1']),
            source_candidate=human_source,
            source_case_sha256=canonical_hash(dict(source['cases'][0], human={'status': 'fail', 'reason': 'human observed failure'})),
            original_result=dict(self.old, head=human_source, actor='human'))
        self.data['repair']['results']['HUMAN-1'] = copy.deepcopy(self.data['repair']['results']['GAP-1'])
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        self.assertTrue(self.verify()['repair_revision'])
        failure = self.record['failures']['HUMAN-1']
        failure.update(source_candidate=self.source, source_case_sha256=canonical_hash(self.case))
        failure['original_result']['head'] = self.source
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'tracked original failure'):
            self.verify()

    def test_each_distinct_failed_case_version_requires_its_original_record(self):
        self.git('checkout', '-q', self.source)
        second = copy.deepcopy(self.case)
        second['expected'] += '; later failure also requires the saved duration boundary'
        second['gpt']['reason'] = 'different failed saved-duration guarantee'
        source = change(36); source['cases'] = [second]
        self.write('docs/verification/changes/issue-36.json', source)
        second_source = self.commit('second failed definition for same Case and actor')
        # Repeat both exact blobs at different commits; do not require a record per commit.
        source['cases'] = [self.case]
        self.write('docs/verification/changes/issue-36.json', source)
        self.commit('restore first failed definition')
        source['cases'] = [second]
        self.write('docs/verification/changes/issue-36.json', source)
        self.commit('restore identical second failed definition')
        pending = copy.deepcopy(second); pending['gpt']['status'] = 'pending'
        source['cases'] = [pending]
        self.write('docs/verification/changes/issue-36.json', source)
        self.pr['base']['sha'] = self.commit('normal later status evolution')
        self.write('replay_buffer/fixture.py', 'both failed definitions corrected')
        self.candidate = self.commit('complete correction')
        result = self.data['repair']['results']['GAP-1']
        result['observation'].update(head=self.candidate, at=self.git('show', '-s', '--format=%cI', self.candidate))
        self.data['repair']['candidate'] = self.candidate
        first = copy.deepcopy(self.record['failures']['GAP-1'])
        later = copy.deepcopy(first)
        later.update(source_candidate=second_source, source_case_sha256=canonical_hash(second),
                     original_result=dict(self.old, head=second_source, reason=second['gpt']['reason']),
                     guarantees={'duration': 'later failed duration boundary'})
        later_result = copy.deepcopy(result)
        later_result['coverage'] = {'duration': 'fixed saved duration boundary recheck'}
        for failures, results in (({'GAP-1': first}, {'GAP-1': result}),
                                  ({'GAP-2': later}, {'GAP-2': later_result})):
            self.record['failures'] = failures
            self.data['repair']['results'] = results
            self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
            self.freeze()
            with self.subTest(included=list(failures)), self.assertRaisesRegex(ValueError, 'tracked original failure'):
                self.verify()
        self.record['failures'] = {'GAP-1': first, 'GAP-2': later}
        self.data['repair']['results'] = {'GAP-1': result, 'GAP-2': later_result}
        self.data['repair']['failure_record_sha256'] = canonical_hash(self.record)
        self.freeze()
        self.assertTrue(self.verify()['repair_revision'])

    def test_metadata_only_history_rejects_later_product_edit_even_if_reverted(self):
        self.write('replay_buffer/fixture.py', 'untested change')
        self.commit('product edit after test')
        self.write('replay_buffer/fixture.py', 'corrected production path')
        self.commit('revert net diff')
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'trusted scope'):
            self.verify()

    def test_base_itself_is_not_a_repaired_candidate(self):
        self.pr['base']['sha'] = self.candidate
        with self.assertRaisesRegex(ValueError, 'substantive correction'):
            self.verify()

    def test_metadata_only_and_reverted_correction_are_not_repairs(self):
        self.git('checkout', '-q', self.source)
        self.write(self.path, {'note': 'metadata only'})
        metadata = self.commit('metadata')
        self.write('replay_buffer/fixture.py', 'temporary fix')
        self.commit('fix then undo')
        self.write('replay_buffer/fixture.py', 'original failure')
        reverted = self.commit('undo fix')
        self.write('docs/repair-guide.md', 'legitimate non-product correction')
        substantive = self.commit('repair the actual procedure')
        for candidate in (metadata, reverted, substantive):
            self.git('checkout', '-q', candidate)
            self.data['repair']['candidate'] = candidate
            self.data['repair']['results']['GAP-1']['observation'].update(
                head=candidate, at=self.git('show', '-s', '--format=%cI', candidate))
            self.freeze()
            with self.subTest(candidate=candidate):
                if candidate == substantive:
                    self.assertTrue(self.verify()['repair_revision'])
                else:
                    with self.assertRaisesRegex(ValueError, 'substantive correction'):
                        self.verify()

    def test_main_changes_during_repair_validation_rejects_stale_result(self):
        original = self.api
        reads = 0
        def api(path):
            nonlocal reads
            if path == 'git/ref/heads/main':
                reads += 1
                return {'object': {'sha': self.source if reads == 1 else self.candidate}}
            return original(path)
        with self.assertRaisesRegex(ValueError, 'main contract changed'):
            verify_pr(self.pr, api, self.git)

    def test_artifact_key_comes_from_original_case(self):
        self.assertTrue(self.verify()['repair_revision'])
        self.data['repair']['artifacts'] = {'app': '2' * 64}
        self.freeze()
        with self.assertRaisesRegex(ValueError, 'corrected fixed artifact'):
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

    def merged_repair(self):
        self.prepare()
        self.worker.side_effect = lambda *args, **kw: dict(report(), repair_revision='e' * 64,
                                                          repair_assessment='all failed guarantees checked')
        self.loop.tick(36)
        self.gh.pull.update(merged=True, state='closed')
        _, state, sid, _ = self.loop.load(36)
        state['acceptance'] = dict(self.accepted, head=tal.HEAD, base=tal.BASE)
        self.loop.save(self.gh.pr(36), state, sid)

    def recovery_evidence(self):
        bound, _ = self.loop.bound(self.gh.pr(36), self.gh.issue(35), self.gh.comments(36))
        return {'binding': bound, 'review': dict(report(), repair_revision='e' * 64,
                repair_assessment='all failed guarantees checked', feedback_hash=bound['feedback_hash'])}

    def test_merged_repair_edit_recomputes_acceptance_before_recovery(self):
        self.merged_repair()
        self.gh.comment(35, 'edited original failure record adds a failed guarantee')
        self.loop.acceptance.return_value = {'allowed': False, 'reason': 'record hash mismatch'}
        with self.assertRaisesRegex(ValueError, 'record hash mismatch'):
            self.loop.reconcile_feedback(self.gh.pr(36), self.recovery_evidence())
        _, state, _, _ = self.loop.load(36)
        self.assertNotIn('acceptance', state)
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_merged_repair_new_revision_needs_new_assessment(self):
        self.merged_repair()
        self.gh.comment(35, 'edited failure record')
        self.loop.acceptance.return_value = dict(self.accepted, repair_revision='f' * 64)
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.loop.reconcile_feedback(self.gh.pr(36), self.recovery_evidence())
        evidence = self.recovery_evidence()
        evidence['review']['repair_revision'] = 'f' * 64
        fresh = self.loop.reconcile_feedback(self.gh.pr(36), evidence)
        self.assertEqual(fresh['acceptance']['repair_revision'], 'f' * 64)

    def test_merged_main_change_blocks_handoff_and_closure(self):
        self.merged_repair()
        self.loop.acceptance.return_value = {'allowed': False, 'reason': 'current main contract mismatch'}
        with patch.object(al, 'handoff') as transfer:
            self.assertEqual(self.loop.tick(36)['phase'], 'cleanup')
            transfer.assert_not_called()
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_contract_change_during_merged_handoff_blocks_closure(self):
        self.merged_repair()
        self.loop.acceptance.side_effect = [self.accepted, {'allowed': False, 'reason': 'main changed'}]
        with patch.object(al, 'handoff', return_value=74), patch.object(al, 'git', return_value='{}'):
            self.assertEqual(self.loop.tick(36)['phase'], 'cleanup')
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.loop.cleanup.assert_not_called()


if __name__ == '__main__':
    unittest.main()
