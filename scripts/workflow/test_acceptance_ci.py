"""Delayed events must not turn live closed PRs into fresh acceptance decisions."""
import copy
import json
import os
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import acceptance_ci as ci
from verification import verify_pr

HEAD = 'a' * 40
BASE = 'b' * 40
MOVED = 'c' * 40


def pr_data():
    return {
        'number': 90, 'title': 'chore: 受入チェックを整える (#90)', 'state': 'open', 'merged': False,
        'body': 'Issue: #90\nIntegration: tooling\n'
                'Verification: docs/verification/changes/issue-90.json\n'
                'GUI: not-required\nGUI reason: CI state handling only',
        'head': {'sha': HEAD, 'ref': 'codex/90-acceptance-closed-pr'},
        'base': {'sha': BASE, 'ref': 'main'},
    }


class DelayedAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.event = pr_data()
        self.check_output = self.enterContext(patch.object(ci.subprocess, 'check_output', return_value=BASE))
        self.run = self.enterContext(patch.object(ci.subprocess, 'run'))
        self.verify = self.enterContext(patch.object(ci, 'verify_pr', return_value={'mode': 'tooling', 'cases': 0}))

    def closed(self, merged=True):
        return dict(pr_data(), state='closed', merged=merged)

    def test_delayed_merged_or_unmerged_closed_pr_skips_before_refs_and_validation(self):
        for merged in (True, False):
            with self.subTest(merged=merged):
                current = self.closed(merged)
                # A closed PR does not need a valid *current* body/base to be ignored.
                current['body'] = ''
                current['base'] = {'ref': 'main', 'sha': MOVED}
                api = Mock(return_value=current)
                result = ci.check_pr(self.event, api)
                self.assertEqual(result, {'status': 'skipped', 'reason': 'live-pr-closed', 'pr': 90})
                self.assertNotIn('gui_complete', result)
                api.assert_called_once_with('pulls/90')
        self.check_output.assert_not_called()
        self.run.assert_not_called()
        self.verify.assert_not_called()

    def test_closed_event_with_live_reopened_pr_runs_normal_gate(self):
        self.event.update(state='closed', merged=True)
        current = pr_data()
        api = Mock(side_effect=[current, {'object': {'sha': BASE}}, pr_data(), {'object': {'sha': BASE}}])
        self.assertEqual(ci.check_pr(self.event, api), {'mode': 'tooling', 'cases': 0})
        self.verify.assert_called_once()

    def test_unknown_or_inconsistent_live_state_is_not_skipped(self):
        for state, merged in ((None, False), ('unknown', False), ('open', True)):
            with self.subTest(state=state, merged=merged):
                current = dict(pr_data(), state=state, merged=merged)
                with self.assertRaisesRegex(ValueError, 'state is missing or inconsistent'):
                    ci.check_pr(self.event, Mock(return_value=current))
        self.verify.assert_not_called()

    def test_live_lookup_error_fails_closed(self):
        with self.assertRaisesRegex(OSError, 'API unavailable'):
            ci.check_pr(self.event, Mock(side_effect=OSError('API unavailable')))
        self.verify.assert_not_called()

    def test_open_event_head_or_target_mismatch_still_fails(self):
        for field, key, value in (('head', 'sha', MOVED), ('base', 'ref', 'develop')):
            with self.subTest(field=field):
                current = pr_data()
                current[field][key] = value
                with self.assertRaisesRegex(ValueError, 'changed after this workflow was queued'):
                    ci.check_pr(self.event, Mock(side_effect=[current, copy.deepcopy(current)]))
        self.verify.assert_not_called()

    def test_open_metadata_failure_is_preserved(self):
        current = pr_data()
        current['body'] = ''
        with self.assertRaises(ValueError):
            ci.check_pr(self.event, Mock(side_effect=[current, copy.deepcopy(current)]))
        self.run.assert_not_called()
        self.verify.assert_not_called()

    def test_open_validation_or_git_failure_is_preserved(self):
        for error in (ValueError('coverage incomplete'), subprocess.CalledProcessError(128, 'git')):
            with self.subTest(error=error):
                self.verify.side_effect = error
                api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, pr_data()])
                with self.assertRaises(type(error)) as raised:
                    ci.check_pr(self.event, api)
                self.assertIs(raised.exception, error)

    def test_closed_during_failed_validation_is_no_decision(self):
        for error in (ValueError('empty diff after merge'), subprocess.CalledProcessError(128, 'git')):
            with self.subTest(error=error):
                self.verify.side_effect = error
                api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, self.closed()])
                self.assertEqual(ci.check_pr(self.event, api)['status'], 'skipped')

    def test_lookup_failure_after_validation_error_does_not_report_skip(self):
        self.verify.side_effect = ValueError('coverage incomplete')
        api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, OSError('API unavailable')])
        with self.assertRaisesRegex(OSError, 'API unavailable'):
            ci.check_pr(self.event, api)

    def test_closed_during_successful_validation_discards_acceptance_result(self):
        api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, self.closed()])
        result = ci.check_pr(self.event, api)
        self.assertEqual(result['status'], 'skipped')
        self.assertNotIn('mode', result)
        self.assertEqual(api.call_count, 3)  # No new-base lookup for a closed PR.

    def test_open_final_head_target_body_or_base_race_still_fails(self):
        for change in ('head', 'target', 'body', 'title', 'base'):
            with self.subTest(change=change):
                latest = pr_data()
                latest_base = BASE
                if change == 'head': latest['head']['sha'] = MOVED
                if change == 'target': latest['base']['ref'] = 'develop'
                if change == 'body': latest['body'] += '\nchanged scope'
                if change == 'title': latest['title'] = 'fix: 受入チェックを直す (#90)'
                if change == 'base': latest_base = MOVED
                api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, latest, {'object': {'sha': latest_base}}])
                with self.assertRaisesRegex(ValueError, 'changed during acceptance validation'):
                    ci.check_pr(self.event, api)

    def test_repair_main_dependency_is_read_back_after_validation(self):
        self.verify.return_value = {'mode': 'develop', 'repair_contract_base': BASE}
        current = pr_data()
        current['base']['ref'] = 'develop'
        current['body'] = current['body'].replace('tooling', 'develop')
        event = copy.deepcopy(current)
        for main in (BASE, MOVED):
            with self.subTest(main=main):
                def api(path):
                    if path == 'pulls/90': return copy.deepcopy(current)
                    return {'object': {'sha': main if path.endswith('/main') else BASE}}
                if main == BASE:
                    self.assertEqual(ci.check_pr(event, api), self.verify.return_value)
                else:
                    with self.assertRaisesRegex(ValueError, 'main contract changed'):
                        ci.check_pr(event, api)

    def test_open_empty_tooling_diff_remains_invalid_with_real_validator(self):
        data = {'schema': 1, 'issue': 90, 'gui_required': False, 'reason': 'CI only',
                'cli_checks': ['python3 -m unittest'], 'cases': []}
        git = Mock(side_effect=[f'100644 blob {HEAD}\tdocs/verification/changes/issue-90.json', json.dumps(data), ''])
        self.verify.side_effect = lambda pr, api: verify_pr(pr, api, git)
        api = Mock(side_effect=[pr_data(), {'object': {'sha': BASE}}, pr_data()])
        with self.assertRaisesRegex(ValueError, 'Direct main tooling route'):
            ci.check_pr(self.event, api)


    def test_open_gui_case_coverage_remains_required_with_real_validator(self):
        current = pr_data()
        current['base']['ref'] = 'develop'
        current['body'] = current['body'].replace('Integration: tooling', 'Integration: develop').replace('GUI: not-required', 'GUI: required')
        event = copy.deepcopy(current)
        data = {'schema': 1, 'issue': 90, 'gui_required': True, 'reason': 'GUI changed',
                'cli_checks': ['python3 -m unittest'], 'cases': []}
        git = Mock(side_effect=lambda *args: f'100644 blob {HEAD}\t{args[-1]}' if args[0] == 'ls-tree' else json.dumps(data))
        self.verify.side_effect = lambda pr, api: verify_pr(pr, api, git)
        api = Mock(side_effect=[current, {'object': {'sha': BASE}}, copy.deepcopy(current)])
        with self.assertRaisesRegex(ValueError, 'Required GUI Cases missing'):
            ci.check_pr(event, api)


class MainContractEventTests(unittest.TestCase):
    def setUp(self):
        self.pr = pr_data()
        self.pr.update(user={'login': 'shinma06'})
        self.pr['base'].update(ref='develop', repo={'full_name': 'shinma06/android-replay-buffer'})
        self.pr['head']['repo'] = {'full_name': 'shinma06/android-replay-buffer'}
        self.pr['body'] = self.pr['body'].replace('tooling', 'develop')
        self.issue = {'body': 'Acceptance repair: 123', 'labels': [{'name': 'type:bug'}]}
        self.run = {'id': 42, 'head_sha': HEAD, 'event': 'pull_request', 'status': 'completed',
                    'pull_requests': [{'number': 90}]}
        self.calls = []

    def api(self, path, method='GET', data=None):
        self.calls.append((path, method, data))
        if method == 'POST': return None
        if path.startswith('pulls?'): return [copy.deepcopy(self.pr)]
        if path == 'issues/90': return copy.deepcopy(self.issue)
        if path.startswith('actions/workflows/'): return {'workflow_runs': [copy.deepcopy(self.run)]}
        raise AssertionError(path)

    def test_main_event_invalidates_independent_status_before_rerunning_acceptance(self):
        self.assertEqual(ci.recheck_main_repairs(self.api, 'shinma06/android-replay-buffer'), [90])
        writes = [c for c in self.calls if c[1] == 'POST']
        self.assertEqual(writes[0][0], 'statuses/' + HEAD)
        self.assertEqual(writes[0][2]['context'], 'Agent review')
        self.assertEqual(writes[0][2]['state'], 'pending')
        self.assertEqual(writes[1][:2], ('actions/runs/42/rerun', 'POST'))

    def test_invalid_title_cannot_hide_original_ng_approval_invalidation(self):
        for title in ('[運用] #90 旧形式', 'fix: 別Issueの題名 (#91)', None):
            for source in (None, 90):
                with self.subTest(title=title, source=source):
                    self.pr['title'] = title
                    with self.assertRaises(ValueError): ci.validate(self.pr)
                    self.calls.clear()
                    self.assertEqual(ci.recheck_main_repairs(
                        self.api, 'shinma06/android-replay-buffer', source_issue=source), [90])
                    writes = [c for c in self.calls if c[1] == 'POST']
                    self.assertEqual(writes[0][2]['state'], 'pending')
                    self.assertEqual(writes[0][2]['context'], 'Agent review')

    def test_collaborator_repairs_are_invalidated_for_main_and_source_events(self):
        from agent_policy import eligible
        for author in ('shinma06', 'collaborator'):
            self.pr['user']['login'] = author
            self.assertEqual(ci.validate(self.pr), 90)
            # Worker authorization still belongs to the coordinator's owner policy.
            self.assertEqual(eligible(self.pr, 'shinma06', 'shinma06/android-replay-buffer'),
                             author == 'shinma06')
            for source in (None, 90):
                with self.subTest(author=author, source=source):
                    self.calls.clear()
                    self.assertEqual(ci.recheck_main_repairs(
                        self.api, 'shinma06/android-replay-buffer', source_issue=source), [90])
                    writes = [c for c in self.calls if c[1] == 'POST']
                    self.assertEqual([c[0] for c in writes], ['statuses/' + HEAD, 'actions/runs/42/rerun'])
                    self.assertEqual(writes[0][2]['state'], 'pending')
                    self.assertEqual(writes[0][2]['context'], 'Agent review')
                    self.assertEqual(any(c[0] == 'issues/90' for c in self.calls), source is None)

    def test_invalidation_rejects_untrusted_or_invalid_pr_scope(self):
        invalid = []
        for ref in ('main', 'other'):
            pr = copy.deepcopy(self.pr)
            pr['base']['ref'] = ref
            invalid.append(pr)
        for side, repository in (('head', None), ('head', {'full_name': 'someone/fork'}),
                                 ('base', {'full_name': 'someone/other'})):
            pr = copy.deepcopy(self.pr)
            pr[side]['repo'] = repository
            invalid.append(pr)
        for state in ('closed', 'unknown'):
            invalid.append(dict(self.pr, state=state))
        invalid.append(dict(self.pr, merged=True))
        for ref in ('feature/unscoped', 'codex/91-other-issue'):
            pr = copy.deepcopy(self.pr)
            pr['head']['ref'] = ref
            invalid.append(pr)
        for old, new in (('Issue: #90', 'Issue: #91'), ('Integration: develop', 'Integration: tooling'),
                         ('issue-90.json', 'issue-91.json'), ('GUI: not-required', 'GUI: unknown')):
            invalid.append(dict(self.pr, body=self.pr['body'].replace(old, new)))
        for pr in invalid:
            for source in (None, 90):
                with self.subTest(pr=pr, source=source):
                    self.pr = pr
                    self.calls.clear()
                    self.assertEqual(ci.recheck_main_repairs(
                        self.api, 'shinma06/android-replay-buffer', source_issue=source), [])
                    self.assertEqual(self.calls, [
                        ('pulls?state=open&base=develop&per_page=100&page=1', 'GET', None)])

    def test_unrelated_pr_and_fork_do_not_get_invalidated(self):
        for issue, repo in (({'body': 'Acceptance repair: none', 'labels': [{'name': 'type:bug'}]},
                             'shinma06/android-replay-buffer'),
                            ({'body': '', 'labels': []}, 'shinma06/android-replay-buffer'),
                            (self.issue, 'someone/fork')):
            with self.subTest(issue=issue, repo=repo):
                self.issue = issue
                self.pr['head']['repo']['full_name'] = repo
                self.calls.clear()
                self.assertEqual(ci.recheck_main_repairs(self.api, 'shinma06/android-replay-buffer'), [])
                self.assertFalse(any(c[1] == 'POST' for c in self.calls))

    def test_all_open_pr_pages_are_checked(self):
        original = self.api
        def api(path, method='GET', data=None):
            if path.endswith('page=1') and path.startswith('pulls?'):
                return [dict(self.pr, head=dict(self.pr['head'], ref='codex/91-unrelated')) for _ in range(100)]
            return original(path, method, data)
        self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [90])
        self.assertTrue(any(c[0].endswith('page=2') for c in self.calls))

    def test_missing_stale_or_running_acceptance_never_restores_success(self):
        for run in (None, dict(self.run, head_sha=MOVED), dict(self.run, status='in_progress'),
                    dict(self.run, pull_requests=[{'number': 91}])):
            self.calls.clear()
            original = self.api
            def api(path, method='GET', data=None):
                if path.startswith('actions/workflows/'):
                    return {'workflow_runs': [run] if run else []}
                return original(path, method, data)
            with self.subTest(run=run), self.assertRaises(ValueError):
                ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
            writes = [c for c in self.calls if c[1] == 'POST']
            self.assertEqual(len(writes), 1)
            self.assertEqual(writes[0][2]['state'], 'pending')

    def test_rerun_failure_leaves_independent_status_pending(self):
        original = self.api
        def api(path, method='GET', data=None):
            if path.endswith('/rerun'): raise OSError('service denied rerun')
            return original(path, method, data)
        with self.assertRaisesRegex(OSError, 'service denied'):
            ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
        self.assertEqual([c[2]['state'] for c in self.calls if c[1] == 'POST'], ['pending'])

    def test_unavailable_run_does_not_leave_later_pr_review_successful(self):
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        original = self.api
        def api(path, method='GET', data=None):
            if path.startswith('pulls?'): return [self.pr, second]
            if path == 'issues/91': return self.issue
            if path.startswith('actions/workflows/'):
                return {'workflow_runs': [] if HEAD in path else [dict(
                    self.run, id=43, head_sha=MOVED, pull_requests=[{'number': 91}])]}
            return original(path, method, data)
        with self.assertRaisesRegex(ValueError, '90'):
            ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
        writes = [c for c in self.calls if c[1] == 'POST']
        self.assertEqual([c[0] for c in writes], ['statuses/' + HEAD, 'statuses/' + MOVED, 'actions/runs/43/rerun'])

    def test_main_event_dispatch_rejects_other_refs_without_api_writes(self):
        with tempfile.NamedTemporaryFile(mode='w+') as event, patch.dict(os.environ, {
                'GITHUB_EVENT_PATH': event.name, 'GITHUB_EVENT_NAME': 'push',
                'GITHUB_REPOSITORY': 'shinma06/android-replay-buffer'}), patch.object(ci, 'recheck_main_repairs') as recheck:
            for ref in ('refs/heads/develop', 'refs/heads/main'):
                event.seek(0); event.truncate()
                json.dump({'ref': ref}, event); event.flush()
                if ref.endswith('/develop'):
                    with self.assertRaisesRegex(ValueError, 'main push'):
                        ci.main()
                    recheck.assert_not_called()
                else:
                    recheck.return_value = [90]
                    ci.main()
                    recheck.assert_called_once()

    def test_rerun_denial_does_not_leave_later_pr_review_successful(self):
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        original = self.api
        def api(path, method='GET', data=None):
            if path.startswith('pulls?'): return [self.pr, second]
            if path == 'issues/91': return self.issue
            if path == 'actions/runs/42/rerun': raise OSError('service denied rerun')
            if path.startswith('actions/workflows/') and MOVED in path:
                return {'workflow_runs': [dict(self.run, id=43, head_sha=MOVED, pull_requests=[{'number': 91}])]}
            return original(path, method, data)
        with self.assertRaisesRegex(OSError, 'service denied'):
            ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
        writes = [c[0] for c in self.calls if c[1] == 'POST']
        self.assertEqual(writes[:2], ['statuses/' + HEAD, 'statuses/' + MOVED])
        self.assertIn('actions/runs/43/rerun', writes)

    def test_failed_status_write_is_not_pending_success_and_later_pr_is_invalidated(self):
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        original = self.api
        def api(path, method='GET', data=None):
            if path.startswith('pulls?'): return [self.pr, second]
            if path == 'issues/91': return self.issue
            if path == 'statuses/' + HEAD: raise OSError('status denied')
            if path.startswith('actions/workflows/'):
                return {'workflow_runs': [dict(self.run, id=43, head_sha=MOVED, pull_requests=[{'number': 91}])]}
            return original(path, method, data)
        with self.assertRaisesRegex(OSError, 'status denied') as caught:
            ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
        self.assertIn('invalidation failed', caught.exception.__notes__[0])
        self.assertEqual([c[0] for c in self.calls if c[1] == 'POST'],
                         ['statuses/' + MOVED, 'actions/runs/43/rerun'])


class RepairSourceEventTests(unittest.TestCase):
    setUp = MainContractEventTests.setUp
    api = MainContractEventTests.api
    def event(self, body='Acceptance repair: 123', action='edited'):
        return {'action': action, 'issue': {'number': 90, 'body': body},
                'changes': {'body': {'from': 'Acceptance repair: 122'}}}

    def test_source_declaration_change_removal_and_issue_deletion(self):
        for event in (self.event(), self.event(''), self.event('Acceptance repair: none'),
                      self.event(action='deleted')):
            with self.subTest(event=event):
                self.assertEqual(ci.repair_source_issue(event, 'issues'), 90)

    def test_bug_label_addition_and_removal_invalidate_only_source_issue(self):
        for action in ('labeled', 'unlabeled'):
            event = self.event(body='', action=action)
            event['label'] = {'name': 'type:bug'}
            # Removal payload contains the new labels, which no longer include type:bug.
            event['issue']['labels'] = [] if action == 'unlabeled' else [{'name': 'type:bug'}]
            with self.subTest(action=action):
                self.assertEqual(ci.repair_source_issue(event, 'issues'), 90)
        for label in ('status:review', 'priority:P1', 'type:feature'):
            event['label']['name'] = label
            self.assertIsNone(ci.repair_source_issue(event, 'issues'))
        event['label']['name'] = 'type:bug'
        event['issue']['pull_request'] = {}
        self.assertIsNone(ci.repair_source_issue(event, 'issues'))

    def test_source_comment_edit_delete_and_marker_removal(self):
        for action in ('edited', 'deleted'):
            event = self.event(action=action)
            event['comment'] = {'id': 123, 'body': 'removed marker'}
            self.assertEqual(ci.repair_source_issue(event, 'issue_comment'), 90)
            event['issue']['body'] = 'Acceptance repair: none'
            event['changes']['body']['from'] = '<!-- acceptance-repair:v1 -->\noriginal'
            self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))
            declaration_event = self.event('Acceptance repair: none', action='edited')
            declaration_event['changes']['body']['from'] = 'Acceptance repair: 123'
            self.assertEqual(ci.repair_source_issue(declaration_event, 'issues'), 90)

    def test_unreferenced_marker_comment_edits_and_deletions_are_irrelevant(self):
        for action in ('edited', 'deleted'):
            event = self.event(action=action)
            event['comment'] = {'id': 456, 'body': '<!-- acceptance-repair:v1 -->\nnot referenced'}
            event['changes']['body']['from'] = '<!-- acceptance-repair:v1 -->\nold comment body'
            with self.subTest(action=action):
                self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))
        event['comment']['body'] = 'marker removed'
        self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))

    def test_comment_history_is_not_previous_issue_declaration(self):
        event = self.event('Acceptance repair: none')
        event['comment'] = {'id': 456, 'body': 'ordinary unrelated edited comment'}
        event['changes']['body']['from'] = 'Acceptance repair: 456'
        self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))
        # The actual Issue edit invalidates removal of the prior reference separately.
        self.assertEqual(ci.repair_source_issue(event, 'issues'), 90)

    def test_receipts_unrelated_body_edits_and_pr_comments_are_ignored(self):
        event = self.event()
        event['changes']['body']['from'] = event['issue']['body'] + '\nold receipt'
        self.assertIsNone(ci.repair_source_issue(event, 'issues'))
        event['comment'] = {'id': 456, 'body': 'review receipt'}
        self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))
        event['issue']['pull_request'] = {}
        event['comment']['body'] = '<!-- acceptance-repair:v1 -->\nrecord'
        self.assertIsNone(ci.repair_source_issue(event, 'issue_comment'))

    def test_source_event_pages_only_invalidate_matching_issue_without_fetching_deleted_issue(self):
        unrelated = copy.deepcopy(self.pr)
        unrelated['number'] = 91
        unrelated['head'].update(sha=MOVED, ref='codex/91-repair')
        unrelated['body'] = unrelated['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        original = self.api
        def api(path, method='GET', data=None):
            if path.startswith('pulls?') and path.endswith('page=1'): return [unrelated] * 100
            if path == 'issues/90': raise OSError('deleted source issue')
            return original(path, method, data)
        self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer', source_issue=90), [90])
        self.assertEqual([c[0] for c in self.calls if c[1] == 'POST'],
                         ['statuses/' + HEAD, 'actions/runs/42/rerun'])

    def test_source_event_main_dispatch_uses_scoped_route(self):
        for action in ('edited', 'labeled', 'unlabeled'):
            event = self.event(action=action)
            event['label'] = {'name': 'type:bug'}
            if action != 'edited': event['issue']['body'] = ''
            with self.subTest(action=action), tempfile.NamedTemporaryFile(mode='w+') as event_file, patch.dict(os.environ, {
                    'GITHUB_EVENT_PATH': event_file.name, 'GITHUB_EVENT_NAME': 'issues',
                    'GITHUB_REPOSITORY': 'shinma06/android-replay-buffer'}), patch.object(ci, 'recheck_main_repairs') as recheck:
                json.dump(event, event_file); event_file.flush()
                recheck.return_value = [90]
                ci.main()
                recheck.assert_called_once_with(unittest.mock.ANY, 'shinma06/android-replay-buffer', source_issue=90)

    def test_ignored_comment_dispatch_does_not_scan_or_write(self):
        event = self.event()
        event['comment'] = {'id': 456, 'body': '<!-- acceptance-repair:v1 -->\nunreferenced'}
        with tempfile.NamedTemporaryFile(mode='w+') as event_file, patch.dict(os.environ, {
                'GITHUB_EVENT_PATH': event_file.name, 'GITHUB_EVENT_NAME': 'issue_comment',
                'GITHUB_REPOSITORY': 'shinma06/android-replay-buffer'}), patch.object(ci, 'recheck_main_repairs') as recheck:
            json.dump(event, event_file); event_file.flush()
            recheck.return_value = []
            ci.main()
            recheck.assert_not_called()

    def test_workflow_source_events_use_trusted_main_and_do_not_cancel_invalidation(self):
        from pathlib import Path
        workflow = (Path(__file__).resolve().parents[2] / '.github/workflows/acceptance.yml').read_text()
        self.assertIn('  issues:\n    types: [edited, deleted, labeled, unlabeled]', workflow)
        self.assertIn('  issue_comment:\n    types: [edited, deleted]', workflow)
        self.assertIn("ref: refs/heads/${{ github.event.pull_request.base.ref || 'main' }}", workflow)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}", workflow)
        self.assertIn('github.event.pull_request.number || github.run_id', workflow)
        self.assertNotIn('github.event.pull_request.head.sha', workflow)


if __name__ == '__main__':
    unittest.main()
