"""Provider completion and feedback regressions; no live reviews or agent launches."""
import copy
import json
import unittest
from unittest.mock import Mock, patch

import agent_loop as al
from agent_policy import next_action
from test_agent_loop import HEAD, NEW, pr_data
import test_agent_loop as tal


def summary(head=HEAD, status='✅ **Completed**'):
    return {'id': 99, 'user': {'login': 'chatgpt-codex-connector[bot]'},
            'body': '<!-- codex-pull-request-review-summary -->\n'
                    '| 📝 **Code Review** | ' + status + ' '
                    '<relative-time datetime="2026-10-11T04:46:46Z">2026-10-11T04:46:46Z</relative-time> '
                    '| `' + head[:7] + '` | Draft marked ready |',
            'created_at': '2026-10-11T04:46:48Z'}


class FeedbackTests(unittest.TestCase):
    def test_empty_old_running_failed_or_untrusted_summary_is_pending(self):
        self.assertTrue(al.remote_review_complete(HEAD, [summary()]))
        for rows in ([], [summary(NEW)], [summary(status='⏳ **Running**')],
                     [summary(status='❌ **Failed**')], [summary(), summary()],
                     [dict(summary(), user={'login': al.OWNER})],
                     [dict(summary(), body='Unknown new format')]):
            with self.subTest(rows=rows):
                self.assertFalse(al.remote_review_complete(HEAD, rows))
                self.assertEqual(next_action({}, {'remote_review_complete': False}, 'success', False),
                                 'remote-review-wait')

    def test_new_request_before_provider_acknowledgement_keeps_pending(self):
        request = {'user': {'login': al.OWNER}, 'body': '@codex review', 'created_at': '2026-10-11T04:47:00Z'}
        self.assertFalse(al.remote_review_complete(HEAD, [summary(), request]))
        request['created_at'] = '2026-10-11T04:45:00Z'
        self.assertTrue(al.remote_review_complete(HEAD, [summary(), request]))
        request['updated_at'] = '2026-10-11T04:47:00Z'
        self.assertFalse(al.remote_review_complete(HEAD, [summary(), request]))
        request.pop('updated_at')
        request['created_at'] = 'invalid'
        self.assertFalse(al.remote_review_complete(HEAD, [summary(), request]))
        receipt = {'user': {'login': al.OWNER}, 'body': 'Disposition: the earlier `@codex review` completed.',
                   'created_at': '2026-10-11T04:50:00Z', 'updated_at': '2026-10-11T04:51:00Z'}
        self.assertTrue(al.remote_review_complete(HEAD, [summary(), receipt]))

    def test_pages_and_threads_read_all_pages_and_fail_closed(self):
        gh = al.GitHub()
        with patch.object(al, 'command', return_value=json.dumps([[{'id': 1}], [{'id': 2}]])) as command:
            self.assertEqual([r['id'] for r in gh.pages('test')], [1, 2])
            self.assertIn('--paginate', command.call_args.args[0])
        def page(i):
            return {'data': {'repository': {'pullRequest': {'reviewThreads': {'nodes': [{'id': i}]}}}}}
        with patch.object(al, 'command', return_value=json.dumps([page(1), page(2)])) as command:
            self.assertEqual([r['id'] for r in gh.review_threads(36)], [1, 2])
            self.assertIn('--paginate', command.call_args.args[0])
            self.assertIn('comments(first:1)', command.call_args.args[0][-1])
        with patch.object(al, 'command', return_value=json.dumps([page(1), {'errors': ['unavailable']}])):
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                gh.review_threads(36)

    def test_bot_external_author_edit_reply_and_resolution_reach_review(self):
        gh = tal.FakeGitHub()
        loop = object.__new__(al.Loop); loop.gh = gh
        records = [{'id': 1, 'user': {'login': 'other-reviewer'}, 'body': 'Concrete external feedback'},
                   {'id': 2, 'user': {'login': 'another-bot[bot]'}, 'body': 'Concrete bot feedback'}, summary()]
        gh.messages[35] = [dict(records[0], id=3, body='Issue feedback')]
        inline = [{'id': 4, 'body': 'Resolved original finding', 'original_commit_id': HEAD},
                  {'id': 5, 'body': 'Reply evidence', 'in_reply_to_id': 4}]
        gh.pages = Mock(side_effect=lambda path: inline if '/comments?' in path else [{'id': 6, 'body': 'Review body'}])
        threads = [{'id': 'thread1', 'isResolved': True, 'isOutdated': True, 'comments': {'nodes': [{'databaseId': 4}]}}]
        gh.review_threads = Mock(side_effect=lambda n: copy.deepcopy(threads))
        bound, feedback = loop.bound(pr_data(), gh.issue(35), records)
        self.assertTrue(bound['remote_review_complete'])
        self.assertTrue({'Concrete external feedback', 'Concrete bot feedback', 'Issue feedback',
                         'Resolved original finding', 'Reply evidence', 'Review body'} <=
                        {r.get('body') for r in feedback})
        records[0]['body'] += ' edited'
        edited, _ = loop.bound(pr_data(), gh.issue(35), records)
        self.assertNotEqual(bound['feedback_hash'], edited['feedback_hash'])
        threads[0]['isResolved'] = False
        changed, _ = loop.bound(pr_data(), gh.issue(35), records)
        self.assertNotEqual(edited['feedback_hash'], changed['feedback_hash'])
        self.assertIn(HEAD, next(r for r in feedback if r.get('id') == 4)['original_commit_id'])


class FeedbackLoopTests(unittest.TestCase):
    setUp = tal.LoopTests.setUp

    def test_pending_completion_does_not_run_or_merge_then_completion_reviews(self):
        with patch.object(al, 'remote_review_complete', return_value=False):
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
        self.worker.assert_not_called()
        self.assertEqual(self.gh.merges, 0)
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        self.assertEqual(self.worker.call_count, 1)

    def test_after_merge_late_feedback_blocks_closure_and_cleanup(self):
        self.loop.tick(36)
        original_merge = self.gh.merge
        def merge(pr):
            result = original_merge(pr)
            self.gh.comment(36, 'Late actionable feedback after the last pre-merge read.')
            return result
        self.gh.merge = merge
        self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
        self.assertEqual(self.gh.merges, 1)
        self.assertEqual(self.gh.issues[35]['state'], 'open')
        self.loop.cleanup.assert_not_called()

    def test_unlinked_source_rejected_before_any_enrollment_write(self):
        self.gh.messages.clear()
        args = Mock(pr=36, source=self.checkout)
        with self.assertRaisesRegex(ValueError, 'linked worktree'):
            al.enroll(self.loop, args)
        self.assertEqual(self.gh.messages, {})
        self.assertEqual(self.gh.statuses, [])
