"""Provider completion and feedback regressions; no live reviews or agent launches."""
import copy
import json
import io
import sys
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
    def native_source(self):
        pr = dict(pr_data(), requested_reviewers=[], requested_teams=[], created_at='2026-10-11T04:00:00Z')
        review = {'id': 51, 'user': {'login': 'native-reviewer'}, 'state': 'APPROVED',
                  'commit_id': HEAD, 'submitted_at': '2026-10-11T04:46:46Z',
                  'pull_request_url': f'https://api.github.com/repos/{al.REPO}/pulls/36'}
        return pr, review

    def test_native_completion_uses_actual_issuer_target_head_and_terminal_record(self):
        pr, review = self.native_source()
        self.assertTrue(al.github_review_complete(pr, [review], 'native-reviewer'))
        for field, bad in [('user', {'login': 'imposter'}), ('state', 'COMMENTED'), ('state', 'PENDING'),
                           ('state', 'DISMISSED'), ('commit_id', NEW), ('submitted_at', None),
                           ('submitted_at', '2026-10-11T03:00:00Z'), ('submitted_at', '2026-10-11T05:00:00'),
                           ('pull_request_url', f'https://api.github.com/repos/{al.REPO}/pulls/37')]:
            with self.subTest(field=field, value=bad):
                self.assertFalse(al.github_review_complete(pr, [dict(review, **{field: bad})], 'native-reviewer'))
        for pending in ('requested_reviewers', 'requested_teams'):
            self.assertFalse(al.github_review_complete(dict(pr, **{pending: [{}]}), [review], 'native-reviewer'))
            missing = dict(pr); missing.pop(pending)
            self.assertFalse(al.github_review_complete(missing, [review], 'native-reviewer'))
        self.assertFalse(al.github_review_complete(pr, [review, dict(review, state='DISMISSED')], 'native-reviewer'))
        self.assertFalse(al.github_review_complete(pr, [review], 'native-reviewer', '2026-10-11T04:47:00Z'))

    def test_trusted_selection_binds_evidence_and_retains_pending_codex_request(self):
        pr, review = self.native_source()
        gh = tal.FakeGitHub()
        loop = object.__new__(al.Loop); loop.gh = gh
        gh.pages = Mock(side_effect=lambda path: [review] if '/reviews?' in path else [])
        self.assertFalse(loop.bound(pr, gh.issue(35), [])[0]['remote_review_complete'])
        loop.github_reviewer = 'Native-Reviewer'
        first, records = loop.bound(pr, gh.issue(35), [])
        self.assertTrue(first['remote_review_complete'])
        self.assertIn(review, records)
        review['body'] = 'New finding'
        second, _ = loop.bound(pr, gh.issue(35), [])
        self.assertNotEqual(first['feedback_hash'], second['feedback_hash'])
        for pending in ([summary(status='⏳ **Running**')],
                        [{'user': {'login': al.OWNER}, 'body': '@codex review', 'created_at': '2026-10-11T04:00:00Z'}]):
            self.assertFalse(loop.bound(pr, gh.issue(35), pending)[0]['remote_review_complete'])
        previous = {'binding': {'remote_review_complete': False}, 'remote_review_after': '2026-10-11T04:47:00Z'}
        comments = [{'id': 12, 'user': {'login': al.OWNER}, 'body': al.pack(al.STATE, previous)}]
        rebound, _ = loop.bound(pr, gh.issue(35), comments)
        self.assertFalse(rebound['remote_review_complete'])
        self.assertEqual(rebound['review_completion']['not_before'], previous['remote_review_after'])
        for invalid in ('', 'unknown:reader', 1):
            with self.assertRaises(ValueError): al.review_selection(invalid)
        loop.github_reviewer = None
        for invalid in (None, 1, 'true'):
            with patch.object(al, 'remote_review_complete', return_value=invalid), self.assertRaises(ValueError):
                loop.bound(pr, gh.issue(35), [])

    def test_native_fractional_request_boundary_keeps_ambiguous_second_pending(self):
        pr, review = self.native_source()
        gh = tal.FakeGitHub()
        loop = object.__new__(al.Loop); loop.gh = gh; loop.github_reviewer = 'native-reviewer'
        gh.pages = Mock(side_effect=lambda path: [review] if '/reviews?' in path else [])
        boundary = '2026-10-11T04:46:46.500000Z'
        requests = [{'method': 'github-review-v1', 'reviewer': 'native-reviewer', 'not_before': boundary}]
        previous = {'remote_review_after': boundary, 'review_requests': requests}
        comments = [{'id': 12, 'user': {'login': al.OWNER}, 'body': al.pack(al.STATE, previous)}]
        hashes = set()
        for submitted, complete in [('2026-10-11T04:46:45Z', False),
                                    ('2026-10-11T04:46:46Z', False),
                                    ('2026-10-11T04:46:46.200000Z', False),
                                    ('2026-10-11T04:46:46.800000Z', True),
                                    ('2026-10-11T04:46:47Z', True)]:
            with self.subTest(submitted=submitted):
                review['submitted_at'] = submitted
                bound, _ = loop.bound(pr, gh.issue(35), comments)
                self.assertEqual(bound['remote_review_complete'], complete)
                self.assertEqual(bound['review_completion']['not_before'], boundary)
                self.assertEqual(bound['review_requests'], requests)
                hashes.add(bound['feedback_hash'])
                if not complete:
                    self.assertEqual(next_action({}, bound, 'success', False), 'remote-review-wait')
                    self.assertEqual(loop.bound(pr, gh.issue(35), comments)[0], bound)
        self.assertEqual(len(hashes), 5)
        # A later timestamp cannot override a latest pending/dismissed record or an outstanding request.
        for state in ('PENDING', 'DISMISSED'):
            review['state'] = state
            self.assertFalse(loop.bound(pr, gh.issue(35), comments)[0]['remote_review_complete'])
        review['state'] = 'APPROVED'
        for key in ('requested_reviewers', 'requested_teams'):
            self.assertFalse(loop.bound(dict(pr, **{key: [{}]}), gh.issue(35), comments)[0]['remote_review_complete'])

    def test_feedback_cli_uses_explicit_source_without_registry_or_worker(self):
        pr, review = self.native_source()
        gh = tal.FakeGitHub(); gh.pull = pr
        gh.pages = Mock(side_effect=lambda path: [review] if '/reviews?' in path else [])
        with patch.object(sys, 'argv', ['agent_loop.py', '--github-reviewer', 'native-reviewer', 'feedback', '--pr', '36']), \
                patch.object(al, 'GitHub', return_value=gh), patch.object(al.Loop, '__init__', side_effect=AssertionError), \
                patch.object(sys, 'stdout', new_callable=io.StringIO) as output:
            self.assertEqual(al.main(), 0)
        self.assertTrue(json.loads(output.getvalue())['binding']['remote_review_complete'])
        self.assertFalse(gh.messages.get(36))

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

    def test_ready_request_rejects_older_completed_review_on_same_head(self):
        self.assertFalse(al.remote_review_complete(HEAD, [summary()], '2026-10-11T04:47:00Z'))
        self.assertTrue(al.remote_review_complete(HEAD, [summary()], '2026-10-11T04:45:00Z'))

    def test_ready_readback_rejects_changed_head_or_target(self):
        gh = al.GitHub()
        for field in ('head', 'base'):
            current = pr_data()
            if field == 'head':
                current['head']['sha'] = NEW
            else:
                current['base']['ref'] = 'develop'
            gh.pr = Mock(return_value=current)
            with patch.object(al, 'command'), self.assertRaisesRegex(ValueError, 'identity'):
                gh.ready(dict(pr_data(), draft=True))

    def test_python39_failure_missing_check_and_missing_protection_block_merge(self):
        gh = al.GitHub()
        checks = [{'name': name, 'state': 'SUCCESS'} for name in al.CI_CHECKS]
        with patch.object(al, 'command', return_value=json.dumps(checks)):
            self.assertEqual(gh.ci(pr_data()), 'success')
        checks[-1]['state'] = 'FAILURE'
        with patch.object(al, 'command', return_value=json.dumps(checks)):
            self.assertEqual(gh.ci(pr_data()), 'failure')
        with patch.object(al, 'command', return_value=json.dumps(checks[:-1])):
            self.assertEqual(gh.ci(pr_data()), 'pending')
        rules = json.loads((al.ROOT / '.github/main-ruleset.json').read_text())['rules']
        for rule in rules:
            rule['ruleset_id'] = 1
            if rule['type'] == 'required_status_checks':
                rule['parameters']['required_status_checks'] = [
                    x for x in rule['parameters']['required_status_checks'] if x['context'] != 'CLI Python 3.9']
        gh.api = Mock(return_value=rules)
        with self.assertRaisesRegex(ValueError, 'protection'):
            gh.merge(dict(pr_data(), draft=False))
        self.assertEqual(gh.api.call_count, 1)

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

    def test_enrolled_draft_becomes_ready_before_wait_without_worker_or_merge(self):
        self.gh.pull['draft'] = True
        with patch.object(al, 'remote_review_complete', return_value=False):
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
            self.assertFalse(self.gh.pull['draft'])
            self.assertEqual(self.gh.ready_calls, 1)
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
        self.assertEqual(self.gh.ready_calls, 1)
        self.worker.assert_not_called()
        self.assertEqual(self.gh.merges, 0)
        _, state, _, _ = self.loop.load(36)
        self.assertIn('remote_review_after', state)
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')

    def test_ready_without_binding_retains_each_source_when_selection_changes(self):
        self.gh.pull.update(draft=True, requested_reviewers=[], requested_teams=[],
                            created_at='2026-10-11T04:00:00Z')
        reviews = []
        self.gh.pages = Mock(side_effect=lambda path: list(reviews) if '/reviews?' in path else [])
        with patch.object(al, 'remote_review_complete', return_value=False):
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
            _, state, _, _ = self.loop.load(36)
            self.assertNotIn('binding', state)
            self.assertEqual({r['method'] for r in state['review_requests']}, {'codex-summary-v1'})
            after = state['remote_review_after']
            self.loop.github_reviewer = 'reviewer-a'
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
            self.loop.github_reviewer = 'reviewer-b'
            reviews.append({'user': {'login': 'reviewer-b'}, 'state': 'APPROVED', 'commit_id': HEAD,
                            'submitted_at': after,
                            'pull_request_url': f'https://api.github.com/repos/{al.REPO}/pulls/36'})
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
        # Codex completes, but the first native request still has no terminal record.
        self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
        self.worker.assert_not_called()
        _, state, _, _ = self.loop.load(36)
        self.assertIn('reviewer-a', state['next'])
        self.assertIn('reviewer-b', state['next'])
        reviews.append(dict(reviews[0], user={'login': 'reviewer-a'}))
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        self.assertEqual(self.worker.call_count, 1)

    def test_native_ready_records_its_source_without_inventing_codex_obligation(self):
        self.gh.pull.update(draft=True, requested_reviewers=[], requested_teams=[],
                            created_at='2026-10-11T04:00:00Z')
        self.loop.github_reviewer = 'reviewer-a'
        with patch.object(al, 'remote_review_complete', return_value=False) as codex:
            self.assertEqual(self.loop.tick(36)['phase'], 'remote-review-wait')
            _, state, _, _ = self.loop.load(36)
            self.assertEqual({r['reviewer'] for r in state['review_requests']}, {'reviewer-a'})
            review = {'user': {'login': 'reviewer-a'}, 'state': 'APPROVED', 'commit_id': HEAD,
                      'submitted_at': state['remote_review_after'],
                      'pull_request_url': f'https://api.github.com/repos/{al.REPO}/pulls/36'}
            self.gh.pages = Mock(side_effect=lambda path: [review] if '/reviews?' in path else [])
            self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
            codex.assert_not_called()

    def test_legacy_first_wait_without_binding_still_requires_codex(self):
        self.gh.comment(36, al.pack(al.STATE, {'phase': 'remote-review-wait',
                        'remote_review_after': '2026-10-11T04:00:00Z'}))
        pr, review = FeedbackTests().native_source()
        self.loop.github_reviewer = 'native-reviewer'
        self.gh.pages = Mock(side_effect=lambda path: [review] if '/reviews?' in path else [])
        with patch.object(al, 'remote_review_complete', return_value=False):
            bound, _ = self.loop.bound(pr, self.gh.issue(35), self.gh.comments(36))
        self.assertFalse(bound['remote_review_complete'])
        self.assertEqual({r['method'] for r in bound['review_requests']},
                         {'codex-summary-v1', 'github-review-v1'})

    def test_draft_ready_does_not_skip_writer_ownership(self):
        self.gh.pull['draft'] = True
        self.loop.source_safe.side_effect = ValueError('writer resumed')
        self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
        self.assertEqual(self.gh.ready_calls, 0)

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

    def test_merged_feedback_recovery_requires_fresh_independent_bound_disposition(self):
        self.loop.tick(36)
        original_merge = self.gh.merge
        def merge(pr):
            result = original_merge(pr)
            self.gh.comment(36, 'Late feedback requiring disposition')
            return result
        self.gh.merge = merge
        self.assertEqual(self.loop.tick(36)['phase'], 'blocked')
        _, state, _, _ = self.loop.load(36)
        stale = {'binding': state['binding'], 'review': tal.report()}
        with self.assertRaisesRegex(ValueError, 'snapshot'):
            self.loop.reconcile_feedback(self.gh.pr(36), stale)
        bound, _ = self.loop.bound(self.gh.pr(36), self.gh.issue(35), self.gh.comments(36))
        with self.assertRaisesRegex(ValueError, 'actually reviewed'):
            self.loop.reconcile_feedback(self.gh.pr(36), {'binding': bound, 'review': tal.report()})
        blocked = {'binding': bound, 'review': dict(tal.report('changes_requested'), feedback_hash=bound['feedback_hash'])}
        with self.assertRaisesRegex(ValueError, 'Unresolved'):
            self.loop.reconcile_feedback(self.gh.pr(36), blocked)
        with patch.object(al, 'remote_review_complete', return_value=False):
            with self.assertRaisesRegex(ValueError, 'Pending'):
                self.loop.reconcile_feedback(self.gh.pr(36), {'binding': bound, 'review': tal.report()})
        evidence = {'binding': bound, 'review': dict(tal.report(), feedback_hash=bound['feedback_hash'],
                    evidence='All late finding IDs have a fixed-source disposition')}
        self.loop.reconcile_feedback(self.gh.pr(36), evidence)
        self.assertEqual(self.loop.tick(36)['phase'], 'done')
        self.assertEqual(self.gh.merges, 1)
        self.assertEqual(self.worker.call_count, 1)
        self.loop.cleanup.assert_called_once()

    def test_feedback_recovery_cannot_publish_unverified_session_metadata(self):
        from agent_policy import native_session, session_fingerprint
        from handoff_registry import store_review
        for field in (None, 'private_sessions', 'private_identifiers', 'private_paths'):
            self.doCleanups(); self.setUp()
            with self.subTest(field=field):
                self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
                self.gh.pull.update(merged=True, state='closed')
                bound, _ = self.loop.bound(self.gh.pr(36), self.gh.issue(35), self.gh.comments(36))
                identity = 'a' * 64
                raw = dict(tal.report(), session=native_session('example.invalid', identity),
                           evidence='Origin: native-session-sha256:' + identity, feedback_hash=bound['feedback_hash'])
                if field:
                    raw[field] = ['native-session-sha256:' + identity] if field == 'private_sessions' else ['Origin']
                evidence = {'binding': bound, 'review': raw}
                source = self.checkout.parent / 'feedback.json'
                source.write_text(json.dumps(evidence)); source.chmod(0o600)
                original = source.read_bytes()
                comments = copy.deepcopy(self.gh.messages)
                if field:
                    with self.assertRaisesRegex(ValueError, 'private review reference|contributor'):
                        self.loop.reconcile_feedback(self.gh.pr(36), json.loads(original))
                    self.assertEqual(self.gh.messages, comments)
                    # The same shared publication boundary covers direct saves and old caches.
                    for wrapper in ('unbound', 'cache', 'old-adapter'):
                        report = copy.deepcopy(raw)
                        if wrapper != 'unbound':
                            stored = report if wrapper == 'cache' else {
                                'kind': 'adapter-review-origin-v1', 'adapter': 'manual',
                                'session': report['session'], 'report': copy.deepcopy(report)}
                            ref = store_review(self.loop.storage, {'binding': bound, 'review': report}, stored)
                            report['origin_receipt'] = ref
                            receipt = self.loop.storage / 'registry' / (ref + '.json')
                            prior = receipt.read_bytes()
                        with self.subTest(wrapper=wrapper), self.assertRaises(ValueError):
                            self.loop.save(self.gh.pr(36), {'phase': 'reviewed', 'binding': bound, 'review': report}, None)
                        self.assertEqual(self.gh.messages, comments)
                        if wrapper != 'unbound':
                            self.assertEqual(receipt.read_bytes(), prior)
                    public = self.loop.public_state({'binding': bound, 'review': raw})
                    cache = store_review(self.loop.storage, public, raw)
                    self.gh.comment(36, al.pack(al.STATE, dict(public, phase='reviewed', review_ref=cache)))
                    cached_path = self.loop.storage / 'registry' / (cache + '.json')
                    cached_bytes = cached_path.read_bytes()
                    with self.assertRaises(ValueError):
                        self.loop.load(36)
                    self.assertEqual(cached_path.read_bytes(), cached_bytes)
                else:
                    self.loop.reconcile_feedback(self.gh.pr(36), json.loads(original))
                    restored = self.loop.load(36)[1]
                    self.assertEqual(restored['binding'], bound)
                    self.assertEqual(restored['review'], raw)
                    public = al.unpack(self.gh.comments(36), al.STATE)[0]
                    self.assertNotIn(identity, public['review']['evidence'])
                    self.assertEqual(public['review']['session'], session_fingerprint(raw['session']))
                self.assertEqual(source.read_bytes(), original)
                self.assertEqual(json.loads(original), evidence)

    def test_merged_finish_refetches_entire_binding_before_entry(self):
        self.loop.tick(36)
        self.gh.pull.update(merged=True, state='closed')
        stale = self.gh.pr(36)
        issue = self.gh.issue(35)
        h, state, sid, _ = self.loop.load(36)
        for change in ('pr-body', 'pr-title', 'issue-body', 'head', 'base', 'target', 'merged'):
            with self.subTest(change=change):
                self.gh.pull = copy.deepcopy(stale)
                self.gh.issues[35] = copy.deepcopy(issue)
                if change == 'pr-body':
                    self.gh.pull['body'] = stale['body'].replace('GUI: not-required', 'GUI: required')
                elif change == 'pr-title':
                    self.gh.pull['title'] = 'fix: ログ欠落を直す (#35)'
                elif change == 'issue-body':
                    self.gh.issues[35]['body'] += '\nNew acceptance condition'
                elif change in ('head', 'base'):
                    self.gh.pull[change]['sha'] = NEW
                elif change == 'target':
                    self.gh.pull['base']['ref'] = 'develop'
                else:
                    self.gh.pull['merged'] = False
                with self.assertRaises(ValueError):
                    self.loop.finish(stale, h, state, sid)
                self.assertEqual(self.gh.issues[35]['state'], 'open')
                self.loop.cleanup.assert_not_called()
        self.assertEqual(self.worker.call_count, 1)

    def test_merged_finish_refetches_contract_during_handoff_even_without_closure(self):
        self.loop.tick(36)
        self.gh.pull.update(merged=True, state='closed')
        original = self.gh.pr(36)
        issue = self.gh.issue(35)
        h, state, sid, _ = self.loop.load(36)
        save = self.loop.save
        for complete in (True, False):
            for change in ('pr-body', 'issue-body'):
                with self.subTest(complete=complete, change=change):
                    self.gh.pull = copy.deepcopy(original)
                    self.gh.issues[35] = copy.deepcopy(issue)
                    state['review']['issue_complete'] = complete
                    def during_handoff(pr, value, comment_id):
                        result = save(pr, value, comment_id)
                        if change == 'pr-body':
                            self.gh.pull['body'] = original['body'].replace('GUI: not-required', 'GUI: required')
                        else:
                            self.gh.issues[35]['body'] += '\nNew acceptance condition'
                        return result
                    with patch.object(self.loop, 'save', side_effect=during_handoff), self.assertRaises(ValueError):
                        self.loop.finish(original, h, state, sid)
                    self.assertEqual(self.gh.issues[35]['state'], 'open')
                    self.loop.cleanup.assert_not_called()
        self.assertEqual(self.worker.call_count, 1)

    def test_unchanged_develop_handoff_bookkeeping_keeps_binding_and_finishes(self):
        h, _, _, _ = self.loop.load(36)
        h['target'] = 'develop'
        self.gh.messages[36][0]['body'] = al.pack(al.HANDOFF, h)
        self.gh.pull['base']['ref'] = 'develop'
        self.gh.pull['body'] = self.gh.pull['body'].replace('Integration: tooling', 'Integration: develop')
        self.assertEqual(self.loop.tick(36)['phase'], 'reviewed')
        self.gh.pull.update(merged=True, state='closed')
        def handoff(gh, repo, pr, issue, change):
            gh.comment(35, '<!-- issue-qa-link:v1 origin=35 qa=74 -->\nQA receipt')
            gh.comment(35, '<!-- issue-qa-record:v1 origin=35 pr=36 -->\nRelease receipt')
            return 74
        with patch.object(al, 'handoff', side_effect=handoff), patch.object(al, 'git', return_value='{}'):
            result = self.loop.tick(36)
        self.assertEqual(result['phase'], 'done')
        self.assertTrue(result['issue_closed'])
        self.loop.cleanup.assert_called_once()
        self.assertEqual(self.worker.call_count, 1)

    def test_recovery_new_feedback_still_blocks_and_changed_contract_drops_old_acceptance(self):
        self.loop.tick(36)
        self.gh.pull.update(merged=True, state='closed')
        self.gh.issues[35]['body'] += '\nNew acceptance condition'
        self.gh.comment(36, 'Late finding disposition is recorded')
        _, state, sid, _ = self.loop.load(36)
        state.update(acceptance={'allowed': True}, gui={'old': 'proof'})
        self.loop.save(self.gh.pr(36), state, sid)
        bound, _ = self.loop.bound(self.gh.pr(36), self.gh.issue(35), self.gh.comments(36))
        evidence = {'binding': bound, 'review': dict(tal.report(), issue_complete=False, feedback_hash=bound['feedback_hash'])}
        fresh = self.loop.reconcile_feedback(self.gh.pr(36), evidence)
        self.assertNotIn('acceptance', fresh)
        self.assertNotIn('gui', fresh)
        self.gh.comment(36, 'Another finding arrives after the independent disposition')
        self.assertEqual(self.loop.tick(36)['phase'], 'cleanup')
        self.loop.cleanup.assert_not_called()
        self.assertEqual(self.gh.issues[35]['state'], 'open')

    def test_unlinked_source_rejected_before_any_enrollment_write(self):
        self.gh.messages.clear()
        args = Mock(pr=36, source=self.checkout)
        with self.assertRaisesRegex(ValueError, 'linked worktree'):
            al.enroll(self.loop, args)
        self.assertEqual(self.gh.messages, {})
        self.assertEqual(self.gh.statuses, [])
