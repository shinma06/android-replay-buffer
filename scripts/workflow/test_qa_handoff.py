import copy
import unittest
from unittest.mock import patch
import qa_handoff as q
from issue_schema import validate_issue
from test_agent_loop import FakeGitHub, report, HEAD, BASE
import test_agent_loop as tal
import agent_loop as al
from test_verification import change

NO_CASES = {'schema': 1, 'issue': 35, 'gui_required': False, 'reason': 'CLI tooling', 'cli_checks': ['unit tests'], 'cases': []}

CHANGE = change()


class GH(FakeGitHub):
    def __init__(self):
        super().__init__()
        self.issues[35]['title'] = '[運用] 試験対象'
        self.issues[10] = {'state': 'open', 'title': '[追跡] release', 'labels': ['type:tracking', 'priority:P1', 'status:deferred'], 'body': 'PM release plan'}
        self.created = 0
        self.fail_create = False
        self.fail_link = False
        self.children = []
        self.fail_relationship = False
    def pages(self, path):
        if '/pulls/' in path:
            return []
        if '/sub_issues?' in path:
            return copy.deepcopy(self.children)
        return [dict(copy.deepcopy(x), number=n) for n, x in self.issues.items()]
    def api(self, path, method='GET', data=None):
        if path.endswith('/sub_issues') and method == 'POST':
            if not self.fail_relationship:
                assert data['sub_issue_id'] == 10000
                self.children.append(self.issue(100))
            return self.issue(100)
        if path.endswith('/issues') and method == 'POST':
            self.created += 1
            self.issues[100] = dict(data, id=10000, number=100, state='open')
            if 'milestone' in data:
                self.issues[100]['milestone'] = {'number': data['milestone']}
            if self.fail_create:
                self.fail_create = False
                raise RuntimeError('lost create response')
            return self.issue(100)
        return super().api(path, method, data)
    def comment(self, n, body, comment_id=None):
        if n == 35 and self.fail_link:
            raise RuntimeError('link failed')
        return super().comment(n, body, comment_id)


class HandoffTests(unittest.TestCase):
    def setUp(self):
        self.gh = GH()
        self.pr = self.gh.pull
        self.pr.update(merged=True, merge_commit_sha='d'*40)
        self.pr['base']['ref'] = 'develop'
        self.pr['body'] = self.pr['body'].replace('tooling', 'develop').replace('GUI: not-required', 'GUI: required')
    def transfer(self):
        return q.handoff(self.gh, al.REPO, self.pr, self.gh.issue(35), CHANGE)
    def test_success_and_retry_reuse_full_snapshot(self):
        self.assertEqual(self.transfer(), 100)
        self.assertEqual(self.transfer(), 100)
        self.assertEqual(self.gh.created, 1)
        self.assertIn('QA-1', self.gh.issue(100)['body'])
        self.assertNotIn('MAIN-REFLECTION', self.gh.issue(100)['body'])
        self.assertIn('/blob/main/docs/verification/human-qa.md', self.gh.issue(100)['body'])
        self.assertEqual(len(self.gh.comments(35)), 1)
    def test_new_qa_title_and_legacy_alias_reuse_preserve_summary(self):
        for origin_title in ('[運用] 試験対象', 'chore: 試験対象'):
            self.gh = GH(); self.pr = self.gh.pull
            self.pr.update(merged=True, merge_commit_sha='d' * 40)
            self.pr['base']['ref'] = 'develop'
            self.pr['body'] = self.pr['body'].replace('tooling', 'develop').replace('GUI: not-required', 'GUI: required')
            self.gh.issues[35]['title'] = origin_title
            self.assertEqual(self.transfer(), 100)
            self.assertEqual(self.gh.issue(100)['title'], 'test: #35 試験対象')
            for qa_title in ('[試験] #35 試験対象', 'test: #35 試験対象'):
                self.gh.issues[100]['title'] = qa_title
                self.assertEqual(self.transfer(), 100)
                self.assertEqual(self.gh.created, 1)

    def test_supported_origin_summaries_generate_canonical_qa_and_keep_full_title(self):
        examples = (
            ('bug', 'fix: #123 の回帰を修正する', 'Issue 123 の回帰を修正する'),
            ('bug', '[修正] #123 の回帰を修正する (#45)', 'Issue 123 の回帰を修正する (Issue 45)'),
            ('maintenance', '[運用] legacy English', '残りの受入試験: legacy English'),
            ('maintenance', '[運用] 試験対象  \r継続\t ', '試験対象 継続'),
            ('qa', 'test: #12 試験対象', '試験対象'),
            ('qa', '[試験] #12 #123 の試験対象', 'Issue 123 の試験対象'),
        )
        for kind, title, summary in examples:
            with self.subTest(title=title):
                self.setUp()
                self.gh.issues[35].update(title=title,
                    labels=['type:' + kind, 'priority:P2', 'status:ready'], milestone={'number': 7})
                origin = self.gh.issue(35)
                self.assertEqual(validate_issue(origin)['type'], kind)
                self.assertEqual(self.transfer(), 100)
                qa = self.gh.issue(100)
                self.assertEqual(qa['title'], 'test: #35 ' + summary)
                self.assertEqual(validate_issue(qa, allow_legacy=False)['type'], 'qa')
                self.assertIn('元Issue題名: ' + title, qa['body'])
                self.assertIn('元Issue: #35', qa['body'])
                self.assertIn('Case: QA-1', qa['body'])
                self.assertEqual(qa['milestone'], origin['milestone'])
                self.assertEqual([item['number'] for item in self.gh.children], [100])
                self.assertEqual(self.transfer(), 100)
                self.assertEqual(self.gh.created, 1)
                self.assertEqual(self.gh.issue(35), origin)

    def test_title_aliases_and_origin_boundaries_do_not_duplicate_or_misidentify_qa(self):
        self.transfer()
        self.gh.issues[101] = dict(self.gh.issue(100), number=101, id=10001,
                                 title='[試験] #35 試験対象', body='Manual QA')
        with self.assertRaisesRegex(ValueError, 'Multiple QA'):
            self.transfer()
        del self.gh.issues[101]
        for title in ('[試験] #3 試験対象', 'test: #3 試験対象'):
            self.gh.issues[100]['title'] = title
            with self.assertRaisesRegex(ValueError, 'identity readback'):
                self.transfer()
            self.assertEqual(self.gh.created, 1)
        self.gh.issues[100]['title'] = 'test: #35 試験対象'
        self.gh.issues[101] = dict(self.gh.issue(100), number=101, id=10001,
                                 title='test: #3 試験対象', body='Other origin QA')
        self.assertEqual(self.transfer(), 100)
        self.assertEqual(self.gh.created, 1)

    def test_invalid_origin_titles_are_rejected_before_transfer_writes(self):
        self.gh.issues[35]['number'] = 35
        for title in ('chore: legacy English', 'chore: #35 試験対象', 'chore: 試験対象 (#99)'):
            self.gh.issues[35]['title'] = title
            with self.assertRaisesRegex(ValueError, 'Japanese summary'):
                self.transfer()
            self.assertEqual(self.gh.created, 0)
            self.assertEqual(self.gh.comments(10), [])
            self.assertEqual(self.gh.comments(35), [])

    def test_milestone_and_parent_are_preserved_on_retry(self):
        self.gh.issues[35]['milestone'] = {'number': 7}
        self.transfer()
        self.transfer()
        self.assertEqual(self.gh.issue(100)['milestone'], {'number': 7})
        self.assertEqual([i['number'] for i in self.gh.children], [100])
        self.gh.issues[100]['milestone'] = {'number': 8}
        with self.assertRaisesRegex(ValueError, 'milestone differs'):
            self.transfer()
        self.assertEqual(self.gh.issue(100)['milestone'], {'number': 8})

    def test_same_number_in_another_repository_is_not_the_qa(self):
        self.gh.children = [{'id': 99999, 'number': 100,
                             'repository_url': 'https://api.github.com/repos/owner/other'}]
        self.transfer()
        self.assertEqual([i['id'] for i in self.gh.children], [99999, 10000])

    def test_failed_relationship_readback_keeps_origin_open(self):
        self.gh.fail_relationship = True
        with self.assertRaisesRegex(ValueError, 'parent relationship readback'):
            self.transfer()
        self.assertEqual(self.gh.issue(35)['state'], 'open')
        self.assertEqual(self.gh.comments(35), [])
        self.gh.fail_relationship = False
        self.transfer()
        self.assertEqual(self.gh.created, 1)

    def test_lost_create_response_reuses_issue(self):
        self.gh.fail_create = True
        with self.assertRaises(RuntimeError): self.transfer()
        self.assertEqual(self.gh.issue(35)['state'], 'open')
        self.transfer()
        self.assertEqual(self.gh.created, 1)
    def test_link_failure_keeps_origin_and_retries(self):
        self.gh.fail_link = True
        with self.assertRaises(RuntimeError): self.transfer()
        self.assertEqual(self.gh.issue(35)['state'], 'open')
        self.gh.fail_link = False
        self.transfer()
        self.assertEqual(self.gh.created, 1)
    def test_duplicate_or_closed_qa_blocks(self):
        self.transfer()
        self.gh.issues[101] = self.gh.issue(100)
        with self.assertRaisesRegex(ValueError, 'Multiple'): self.transfer()
        del self.gh.issues[101]
        self.gh.issues[100].update(state='closed', labels=['type:qa','priority:P2','status:done'])
        with self.assertRaisesRegex(ValueError, 'closed'): self.transfer()
    def test_existing_pm_body_preserved_and_snapshot_appended(self):
        self.transfer()
        self.gh.issues[100]['body'] = '<!-- issue-qa-handoff:v1 origin=35 -->\nPM observations'
        self.transfer()
        self.assertIn('PM observations', self.gh.issue(100)['body'])
        self.assertEqual(len(self.gh.comments(100)), 1)
    def test_invalid_cases_block_before_creating_qa(self):
        invalid = dict(CHANGE, gui_required=True, cases=[])
        self.pr['body'] = self.pr['body'].replace('GUI: not-required', 'GUI: required')
        with self.assertRaises(ValueError):
            q.handoff(self.gh, al.REPO, self.pr, self.gh.issue(35), invalid)
        self.assertEqual(self.gh.created, 0)

    def transfer_without_cases(self):
        self.pr['body'] = self.pr['body'].replace('GUI: required', 'GUI: not-required')
        return q.handoff(self.gh, al.REPO, self.pr, self.gh.issue(35), NO_CASES)

    def test_gui_words_in_explanation_do_not_change_metadata(self):
        self.pr['body'] = self.pr['body'].replace('GUI: required', 'GUI: not-required')
        self.pr['body'] += '\nExplanation: GUI: required is not this PR metadata.'
        self.assertIsNone(q.handoff(self.gh, al.REPO, self.pr, self.gh.issue(35), NO_CASES))
        self.assertEqual(self.gh.created, 0)

    def test_no_cases_reuses_release_without_qa_or_body_replacement(self):
        self.gh.issues[35]['title'] = 'chore: #123 の回帰を確認する'
        for _ in range(2):
            self.assertIsNone(self.transfer_without_cases())
        self.assertEqual(self.gh.created, 0)
        self.assertEqual(self.gh.children, [])
        self.assertEqual(self.gh.issue(10)['body'], 'PM release plan')
        self.assertEqual(len(self.gh.comments(10)), 1)
        self.assertEqual(len(self.gh.comments(35)), 1)
        self.assertIn('merge SHA: ' + 'd'*40, self.gh.comments(10)[0]['body'])

    def test_no_cases_readback_failure_blocks_and_retry_is_idempotent(self):
        original = self.gh.comment
        self.gh.comment = lambda n, body: None
        with self.assertRaisesRegex(ValueError, 'readback'):
            self.transfer_without_cases()
        self.gh.comment = original
        self.gh.fail_link = True
        with self.assertRaisesRegex(RuntimeError, 'link failed'):
            self.transfer_without_cases()
        self.assertEqual(self.gh.issue(35)['state'], 'open')
        self.gh.fail_link = False
        self.transfer_without_cases()
        self.assertEqual(len(self.gh.comments(10)), 1)

    def test_closed_or_wrong_release_blocks(self):
        for changes in ({'state': 'closed', 'labels': ['type:tracking','priority:P1','status:done']},
                        {'title': '[運用] other', 'labels': ['type:maintenance','priority:P1','status:ready']}):
            with self.subTest(changes=changes):
                self.gh = GH()
                self.gh.issues[10].update(changes)
                with self.assertRaisesRegex(ValueError, 'tracker'):
                    self.transfer_without_cases()
                self.assertEqual(self.gh.comments(35), [])

    def test_existing_qa_requires_manual_reconciliation_for_no_cases(self):
        self.transfer()
        for state in ('open', 'closed'):
            self.gh.issues[100]['state'] = state
            with self.assertRaisesRegex(ValueError, 'reconciliation'):
                self.transfer_without_cases()
        self.assertEqual(len(self.gh.comments(10)), 1)

    def test_manual_qa_without_marker_blocks_duplicate_and_no_cases(self):
        self.transfer()
        self.gh.issues[100]['body'] = 'Manual QA with remaining obligations'
        with self.assertRaisesRegex(ValueError, 'identity readback'):
            self.transfer()
        with self.assertRaisesRegex(ValueError, 'reconciliation'):
            self.transfer_without_cases()
        self.assertEqual(self.gh.created, 1)

    def test_gui_handoff_requires_open_release_and_receipt(self):
        self.gh.issues[10].update(state='closed', labels=['type:tracking','priority:P1','status:done'])
        with self.assertRaisesRegex(ValueError, 'tracker'):
            self.transfer()
        self.assertEqual(self.gh.created, 0)
        self.gh = GH()
        original = self.gh.comment
        self.gh.comment = lambda n, body: None if n == 10 else original(n, body)
        with self.assertRaisesRegex(ValueError, 'readback'):
            self.transfer()
        self.assertEqual(self.gh.created, 0)
        self.assertEqual(self.gh.comments(35), [])
        self.gh.comment = original
        self.transfer()
        self.assertEqual(len(self.gh.comments(10)), 1)
        self.assertIn('試験は元IssueのQA', self.gh.comments(10)[0]['body'])

    def test_unconfirmed_merge_blocks(self):
        self.pr['merged'] = False
        with self.assertRaises(ValueError): self.transfer()
        self.assertEqual(self.gh.created, 0)


class ClosureTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp
    def finish(self, complete=True, fail_link=False, change_origin=False, no_cases=False):
        gh = GH(); gh.fail_link = fail_link; self.loop.gh = gh
        if change_origin:
            original_comment = gh.comment
            def comment(n, body, comment_id=None):
                result = original_comment(n, body, comment_id)
                if n == 35: gh.issues[35]["body"] += " new acceptance"
                return result
            gh.comment = comment
        gh.pull.update(merged=True, merge_commit_sha='d'*40)
        gh.pull['base']['ref'] = 'develop'
        gh.pull['body'] = gh.pull['body'].replace('tooling','develop').replace('GUI: not-required', 'GUI: required')
        if no_cases:
            gh.pull['body'] = gh.pull['body'].replace('GUI: required', 'GUI: not-required')
        bound, _ = self.loop.bound(gh.pull, gh.issue(35), gh.comments(36))
        state = {'binding': bound, 'review': dict(report(), issue_complete=complete)}
        h = {'issue':35,'parent':1,'close_issue':False,'gui_required':False}
        with patch.object(al, 'git', return_value=__import__('json').dumps(NO_CASES if no_cases else CHANGE)) as git:
            result = self.loop.finish(gh.pull, h, state, None)
            if complete:
                self.assertEqual([c.args[0] for c in git.call_args_list], ['fetch', 'show'])
        return gh, result
    def test_completed_implementation_closes_after_qa_even_without_flag(self):
        gh, result = self.finish()
        self.assertTrue(result['issue_closed'])
        self.assertEqual(gh.issue(35)['state'], 'closed')
        self.assertIn('status:done', gh.issue(35)['labels'])
        self.assertEqual(gh.issue(1)['body'], '- [ ] #35 task')
        self.assertEqual(gh.created, 1)
    def test_no_cases_closes_only_after_release_handoff(self):
        gh, result = self.finish(no_cases=True)
        self.assertTrue(result['issue_closed'])
        self.assertEqual(gh.created, 0)
        self.assertEqual(len(gh.comments(10)), 1)
        self.assertIn('#10', gh.comments(35)[0]['body'])

    def test_no_cases_failed_link_prevents_closure_and_cleanup(self):
        with self.assertRaisesRegex(RuntimeError, 'link failed'):
            self.finish(fail_link=True, no_cases=True)
        self.assertEqual(self.loop.gh.issue(35)['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_failed_link_never_closes_or_cleans_up(self):
        with self.assertRaisesRegex(RuntimeError, 'link failed'): self.finish(fail_link=True)
        self.assertEqual(self.loop.gh.issue(35)['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_changed_acceptance_during_transfer_never_closes(self):
        with self.assertRaisesRegex(ValueError, 'changed during handoff'): self.finish(change_origin=True)
        self.assertEqual(self.loop.gh.issue(35)['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_unfinished_implementation_stays_open(self):
        gh, result = self.finish(False)
        self.assertFalse(result['issue_closed'])
        self.assertEqual(gh.created, 0)


class SchemaTests(unittest.TestCase):
    def test_canonical_type_mapping_and_read_only_legacy_compatibility(self):
        mapping = {'feature': ('feat', '機能'), 'bug': ('fix', '修正'), 'qa': ('test', '試験'),
                   'maintenance': ('chore', '運用'), 'research': ('research', '調査'), 'tracking': ('track', '追跡')}
        for kind, (prefix, legacy) in mapping.items():
            suffix = '#35 要約' if kind == 'qa' else '要約'
            issue = {'number': 100, 'title': f'{prefix}: {suffix}', 'state': 'open',
                     'labels': [f'type:{kind}', 'priority:P1', 'status:ready']}
            self.assertEqual(validate_issue(issue, allow_legacy=False)['type'], kind)
            old = dict(issue, title=f'[{legacy}] {suffix}')
            self.assertEqual(validate_issue(old)['type'], kind)
            with self.assertRaises(ValueError): validate_issue(old, allow_legacy=False)
            wrong = dict(issue, title=('fix' if prefix != 'fix' else 'chore') + ': ' + suffix)
            with self.assertRaises(ValueError): validate_issue(wrong)

    def test_new_title_rejects_own_number_pr_suffix_missing_origin_and_priority(self):
        issue = {'number': 35, 'title': 'chore: 要約', 'state': 'open',
                 'labels': ['type:maintenance', 'priority:P1', 'status:ready']}
        for title in ('chore: #35 要約', 'chore: 要約 (#35)', 'chore: P1 要約',
                      'chore: English only', 'Chore: 要約', 'chore: 要約\n次行', None):
            with self.subTest(title=title), self.assertRaises(ValueError):
                validate_issue(dict(issue, title=title))
        qa = dict(issue, number=100, labels=['type:qa', 'priority:P1', 'status:ready'])
        for title in ('test: 試験', 'test: #0 試験', 'test: #100 試験', 'test: #35 試験 (#100)', 'test: #35 #99 再試験', 'test: #35 #35 再試験'):
            with self.subTest(title=title), self.assertRaises(ValueError):
                validate_issue(dict(qa, title=title))
        validate_issue(dict(qa, title='test: #35 試験'), allow_legacy=False)

    def test_axes_title_and_closed_status(self):
        valid = {'title':'[試験] #35 試験', 'state':'open', 'labels':['type:qa','priority:P1','status:ready']}
        validate_issue(valid)
        for change in ({'title':'[試験] 試験'}, {'title':'[試験] #35 P1 試験'}, {'state':'closed'},
                       {'labels':['type:qa','priority:P1','status:ready','status:review']},
                       {'labels':['type:qa','priority:P1','status:unknown']}):
            with self.assertRaises(ValueError): validate_issue(dict(valid, **change))
        validate_issue(dict(valid, state='closed', labels=['type:qa','priority:P1','status:done']))

if __name__ == '__main__': unittest.main()
