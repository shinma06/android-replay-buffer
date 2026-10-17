"""Delayed events must not turn live closed PRs into fresh acceptance decisions."""
import copy
from http.client import IncompleteRead
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import json
import os
import subprocess
import tempfile
import threading
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

    def test_main_dependencies_are_read_back_after_validation(self):
        current = pr_data()
        current['base']['ref'] = 'develop'
        current['body'] = current['body'].replace('tooling', 'develop')
        event = copy.deepcopy(current)
        for field, main in ((field, main) for field in ('repair_contract_base', 'certificate_sync_main')
                            for main in (BASE, MOVED)):
            with self.subTest(field=field, main=main):
                self.verify.return_value = {'mode': 'develop', field: BASE}
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
        git = Mock(side_effect=[f'100644 blob {HEAD}\tdocs/verification/changes/issue-90.json', json.dumps(data), '', ''])
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
        self.fetch = self.enterContext(patch.object(ci.subprocess, 'run'))
        self.git = self.enterContext(patch.object(ci, 'git_read', return_value=''))
        self.certificates = self.enterContext(patch.object(ci, 'certificate_changes', return_value=[]))

    def api(self, path, method='GET', data=None):
        self.calls.append((path, method, data))
        if method == 'POST': return None
        if path.startswith('pulls?'): return [copy.deepcopy(self.pr)]
        if path == 'issues/90': return copy.deepcopy(self.issue)
        if path == 'git/ref/heads/develop': return {'object': {'sha': BASE}}
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

    def test_certificate_dependency_lookup_failure_preserves_pending_and_reports_error(self):
        self.issue['body'] = 'Acceptance repair: none'
        for lookup, failure in ((self.fetch, subprocess.CalledProcessError(128, ['git', 'fetch'])),
                                (self.git, OSError('git unavailable')),
                                (self.certificates, ValueError('malformed archive'))):
            with self.subTest(lookup=lookup):
                self.calls.clear()
                lookup.side_effect = failure
                with self.assertRaises(type(failure)) as raised:
                    ci.recheck_main_repairs(self.api, 'shinma06/android-replay-buffer')
                self.assertIs(raised.exception, failure)
                writes = [c for c in self.calls if c[1] == 'POST']
                self.assertEqual(writes[0][0], 'statuses/' + HEAD)
                self.assertEqual(writes[0][2]['state'], 'pending')
                self.assertEqual(writes[1][:2], ('actions/runs/42/rerun', 'POST'))
                lookup.side_effect = None

    def test_all_open_pr_pages_are_checked(self):
        original = self.api
        def api(path, method='GET', data=None):
            if path.endswith('page=1') and path.startswith('pulls?'):
                return [dict(self.pr, head=dict(self.pr['head'], ref='codex/91-unrelated')) for _ in range(100)]
            return original(path, method, data)
        self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [90])
        self.assertTrue(any(c[0].endswith('page=2') for c in self.calls))

    def test_develop_ref_shape_failure_invalidates_known_prs_before_reporting(self):
        self.issue['body'] = 'Acceptance repair: none'
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        invalid = [None, [], {}, {'object': None}, {'object': []}, {'object': {}}]
        invalid += [{'object': {'sha': sha}} for sha in (None, 1, True, [], {}, '', BASE[:7], '--all', 'g' * 40)]
        for response in [*invalid, {'object': {'sha': BASE}}]:
            with self.subTest(response=response):
                self.calls.clear()
                self.fetch.reset_mock()
                self.certificates.return_value = ['changed-certificate']
                def api(path, method='GET', data=None):
                    if path.startswith('pulls?'): return [self.pr, second]
                    if path == 'git/ref/heads/develop': return response
                    if path == 'issues/91': return {'body': 'Acceptance repair: 123'}
                    if path.startswith('actions/workflows/') and MOVED in path:
                        return {'workflow_runs': [dict(self.run, id=43, head_sha=MOVED,
                                                      pull_requests=[{'number': 91}])]}
                    return self.api(path, method, data)
                if response in invalid:
                    with self.assertRaisesRegex(ValueError, 'Develop ref response') as raised:
                        ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                    self.assertIn('PR 90: certificate dependency unknown', raised.exception.__notes__[0])
                    self.fetch.assert_not_called()
                else:
                    self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [90, 91])
                    self.fetch.assert_called_once_with(
                        ['git', 'fetch', '--no-tags', 'origin', BASE, HEAD], check=True)
                writes = [c for c in self.calls if c[1] == 'POST']
                self.assertEqual([c[0] for c in writes], ['statuses/' + HEAD, 'statuses/' + MOVED,
                                                        'actions/runs/42/rerun', 'actions/runs/43/rerun'])
                self.assertTrue(all(c[2]['state'] == 'pending' for c in writes[:2]))

    def test_transient_page_failure_retries_same_page_before_all_reruns(self):
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        filler = dict(self.pr, head=dict(self.pr['head'], ref='unscoped'))
        original = self.api
        for page in (1, 2):
            for failure in (HTTPError('fixture', 502, 'Bad Gateway', {}, None),
                            URLError('connection reset'), TimeoutError('timed out')):
                with self.subTest(page=page, failure=type(failure).__name__):
                    self.calls.clear()
                    attempts = []
                    def api(path, method='GET', data=None):
                        if path.startswith('pulls?'):
                            attempts.append(path)
                            if path.endswith(f'page={page}') and attempts.count(path) == 1:
                                raise failure
                            return [self.pr, *[filler] * 99] if path.endswith('page=1') else [second]
                        if path == 'issues/91': return self.issue
                        if path.startswith('actions/workflows/') and MOVED in path:
                            return {'workflow_runs': [dict(self.run, id=43, head_sha=MOVED,
                                                          pull_requests=[{'number': 91}])]}
                        return original(path, method, data)
                    self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [90, 91])
                    self.assertEqual(sum(p.endswith(f'page={page}') for p in attempts), 2)
                    writes = [c for c in self.calls if c[1] == 'POST']
                    self.assertEqual([c[0] for c in writes], ['statuses/' + HEAD, 'statuses/' + MOVED,
                                                           'actions/runs/42/rerun', 'actions/runs/43/rerun'])
                    self.assertEqual([c[2]['state'] for c in writes[:2]], ['pending', 'pending'])

    def test_incomplete_enumeration_recovers_known_targets_and_preserves_errors(self):
        original = self.api
        filler = dict(self.pr, head=dict(self.pr['head'], ref='unscoped'))
        for rerun_denied in (False, True):
            with self.subTest(rerun_denied=rerun_denied):
                self.calls.clear()
                attempts = []
                def api(path, method='GET', data=None):
                    if path.startswith('pulls?'):
                        attempts.append(path)
                        if path.endswith('page=2'):
                            raise HTTPError('fixture', 502, 'Bad Gateway', {}, None)
                        return [self.pr, *[filler] * 99]
                    if path.endswith('/rerun') and rerun_denied:
                        self.calls.append((path, method, data))
                        raise HTTPError('fixture', 403, 'Forbidden', {}, None)
                    return original(path, method, data)
                with self.assertRaises(HTTPError) as caught:
                    ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                self.assertEqual(caught.exception.code, 502)
                self.assertEqual(sum(p.endswith('page=2') for p in attempts), 3)
                notes = '; '.join(caught.exception.__notes__)
                self.assertIn('page 2', notes)
                self.assertIn('incomplete', notes)
                if rerun_denied: self.assertIn('rerun failed', notes)
                self.assertEqual([c[0] for c in self.calls if c[1] == 'POST'],
                                 ['statuses/' + HEAD, 'actions/runs/42/rerun'])

    def test_page_permission_denial_is_not_retried_or_reported_successful(self):
        for code in (401, 403, 422):
            with self.subTest(code=code):
                attempts = []
                def api(path, method='GET', data=None):
                    attempts.append(path)
                    raise HTTPError('fixture', code, 'Denied', {}, None)
                with self.assertRaises(HTTPError) as caught:
                    ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                self.assertEqual(caught.exception.code, code)
                self.assertEqual(len(attempts), 1)
                self.assertIn('incomplete', caught.exception.__notes__[0])

    def test_truncated_http_page_retries_or_recovers_known_targets(self):
        original = self.api
        filler = dict(self.pr, head=dict(self.pr['head'], ref='unscoped'))
        for page in (1, 2):
            for failures in (1, 3):
                with self.subTest(page=page, failures=failures):
                    self.calls.clear()
                    requests = []
                    pages = {1: [self.pr]} if page == 1 else {1: [self.pr, *[filler] * 99], 2: []}
                    class Handler(BaseHTTPRequestHandler):
                        def do_GET(handler):
                            current = int(handler.path.rsplit('page=', 1)[1])
                            requests.append(current)
                            body = json.dumps(pages[current]).encode()
                            cut = current == page and requests.count(current) <= failures
                            handler.send_response(200)
                            handler.send_header('Content-Length', str(len(body)))
                            handler.end_headers()
                            handler.wfile.write(body[:len(body) // 2] if cut else body)
                            handler.close_connection = True

                        def log_message(handler, *args): pass

                    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
                    thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.01})
                    thread.start()
                    def api(path, method='GET', data=None):
                        if path.startswith('pulls?'):
                            with urlopen(f'http://127.0.0.1:{server.server_port}/{path}', timeout=2) as response:
                                return json.loads(response.read())
                        return original(path, method, data)
                    try:
                        if failures == 1:
                            self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [90])
                        else:
                            with self.assertRaises(IncompleteRead) as caught:
                                ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                            self.assertIn(f'page {page} after 3 attempts', caught.exception.__notes__[0])
                        self.assertEqual(requests, [1] * (2 if failures == 1 else 3) if page == 1 else
                                         [1, *[2] * (2 if failures == 1 else 3)])
                        writes = [c for c in self.calls if c[1] == 'POST']
                        self.assertEqual([c[0] for c in writes], [] if page == 1 and failures == 3 else
                                         ['statuses/' + HEAD, 'actions/runs/42/rerun'])
                        if writes: self.assertEqual(writes[0][2]['state'], 'pending')
                    finally:
                        server.shutdown()
                        server.server_close()
                        thread.join(timeout=2)
                    self.assertFalse(thread.is_alive())

    def test_malformed_pr_pages_fail_without_retry_after_known_recovery(self):
        original = self.api
        filler = dict(self.pr, head=dict(self.pr['head'], ref='unscoped'))
        # Decode the actual external bytes; malformed JSON and encoding keep their exception types.
        for page in (1, 2):
            for body in (b'{}', b'{"message":"malformed response"}', b'""', b'"text"', b'null',
                         b'1', b'1.5', b'true', b'{', b'\xff'):
                with self.subTest(page=page, body=body):
                    self.calls.clear()
                    requests = []
                    def api(path, method='GET', data=None):
                        if path.startswith('pulls?'):
                            current = int(path.rsplit('page=', 1)[1])
                            requests.append(current)
                            return json.loads(body) if current == page else [self.pr, *[filler] * 99]
                        return original(path, method, data)
                    expected = UnicodeDecodeError if body == b'\xff' else json.JSONDecodeError if body == b'{' else ValueError
                    with self.assertRaises(expected) as caught:
                        ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                    self.assertEqual(requests, [1] if page == 1 else [1, 2])
                    self.assertIn(f'page {page} after 1 attempts', caught.exception.__notes__[0])
                    self.assertIn('incomplete', caught.exception.__notes__[0])
                    self.assertEqual([c[0] for c in self.calls if c[1] == 'POST'], [] if page == 1 else
                                     ['statuses/' + HEAD, 'actions/runs/42/rerun'])

    def test_empty_list_is_complete_enumeration(self):
        api = Mock(return_value=[])
        self.assertEqual(ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer'), [])
        api.assert_called_once_with('pulls?state=open&base=develop&per_page=100&page=1')

    def test_enumeration_does_not_catch_unrelated_programming_errors(self):
        for failure in (TypeError('reader programming error'), ValueError('unrelated programming error')):
            with self.subTest(failure=type(failure).__name__):
                api = Mock(side_effect=failure)
                with self.assertRaises(type(failure)) as caught:
                    ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                self.assertIs(caught.exception, failure)
                self.assertEqual(api.call_count, 1)
                self.assertFalse(hasattr(failure, '__notes__'))

    def test_per_pr_response_failures_preserve_errors_and_continue_known_targets(self):
        second = copy.deepcopy(self.pr)
        second['number'] = 91
        second['head'].update(sha=MOVED, ref='codex/91-repair')
        second['body'] = second['body'].replace('#90', '#91').replace('issue-90', 'issue-91')
        filler = dict(self.pr, head=dict(self.pr['head'], ref='unscoped'))
        phases = {'issue': 'issues/90', 'status': 'statuses/' + HEAD,
                  'develop': 'git/ref/heads/develop',
                  'run': 'actions/workflows/acceptance.yml/runs?event=pull_request&head_sha=' + HEAD + '&per_page=1',
                  'rerun': 'actions/runs/42/rerun'}
        cases = [(phase, failure, False) for phase in phases
                 for failure in ('truncated', 'json', 'encoding', 'denied')]
        cases += [('run', failure, True) for failure in ('truncated', 'json', 'encoding')]
        for phase, failure, incomplete in cases:
            with self.subTest(phase=phase, failure=failure, incomplete=incomplete):
                self.issue['body'] = 'Acceptance repair: none' if phase == 'develop' else 'Acceptance repair: 123'
                self.certificates.return_value = ['changed-certificate']
                requests, failures = [], []
                target = phases[phase]
                class Handler(BaseHTTPRequestHandler):
                    def do_GET(handler): handler.respond()
                    def do_POST(handler):
                        handler.rfile.read(int(handler.headers.get('Content-Length', 0)))
                        handler.respond()
                    def respond(handler):
                        path = handler.path[1:]
                        requests.append((handler.command, path))
                        page_failure = incomplete and path.endswith('page=2')
                        if path.startswith('pulls?'):
                            value = [self.pr, second, *[filler] * 98] if incomplete else [self.pr, second]
                        elif path.startswith('issues/'): value = self.issue
                        elif path == 'git/ref/heads/develop': value = {'object': {'sha': BASE}}
                        elif path.startswith('actions/workflows/'):
                            value = {'workflow_runs': [self.run if HEAD in path else dict(
                                self.run, id=43, head_sha=MOVED, pull_requests=[{'number': 91}])]}
                        else: value = {}
                        body = json.dumps(value).encode()
                        status, cut = 200, page_failure
                        if path == target:
                            if failure == 'denied': status, body = 403, b'{"message":"Denied"}'
                            elif failure == 'json': body = b'{'
                            elif failure == 'encoding': body = b'\xff'
                            else: cut = True
                        handler.send_response(status)
                        handler.send_header('Content-Length', str(len(body)))
                        handler.end_headers()
                        handler.wfile.write(body[:len(body) // 2] if cut else body)
                        handler.close_connection = True
                    def log_message(handler, *args): pass

                server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
                thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.01})
                thread.start()
                def local_urlopen(request, timeout):
                    local = Request(f'http://127.0.0.1:{server.server_port}/' + request.full_url.split('/repos/shinma06/android-replay-buffer/', 1)[1],
                                    method=request.get_method(), data=request.data)
                    return urlopen(local, timeout=2)
                original = ci.recheck_main_repairs
                def recheck(api, repo):
                    def observed_api(*args):
                        try: return api(*args)
                        except (OSError, IncompleteRead, json.JSONDecodeError, UnicodeDecodeError) as error:
                            failures.append(error)
                            raise
                    return original(observed_api, repo)
                expected = IncompleteRead if incomplete or failure == 'truncated' else {
                    'json': json.JSONDecodeError, 'encoding': UnicodeDecodeError, 'denied': HTTPError}[failure]
                try:
                    with tempfile.NamedTemporaryFile(mode='w+') as event:
                        json.dump({'ref': 'refs/heads/main'}, event); event.flush()
                        with patch.dict(os.environ, {'GITHUB_EVENT_PATH': event.name, 'GITHUB_EVENT_NAME': 'push',
                                'GITHUB_REPOSITORY': 'shinma06/android-replay-buffer', 'GITHUB_TOKEN': 'disposable'}), \
                                patch.object(ci, 'urlopen', side_effect=local_urlopen), \
                                patch.object(ci, 'recheck_main_repairs', side_effect=recheck), \
                                self.assertRaises(expected) as caught:
                            ci.main()
                    self.assertIs(caught.exception, failures[2] if incomplete else failures[0])
                    notes = '; '.join(caught.exception.__notes__)
                    self.assertIn('PR 90', notes)
                    self.assertIn('certificate dependency unknown' if phase == 'develop' else
                                  'source lookup or review invalidation' if phase in ('issue', 'status') else
                                  'Acceptance lookup or rerun', notes)
                    self.assertEqual(requests.count(('POST' if phase in ('status', 'rerun') else 'GET', target)),
                                     2 if phase == 'develop' else 1)
                    self.assertIn(('POST', 'statuses/' + MOVED), requests)
                    self.assertIn(('POST', 'actions/runs/43/rerun'), requests)
                    self.assertEqual(('POST', 'actions/runs/42/rerun') in requests, phase in ('rerun', 'develop'))
                    writes = [path for method, path in requests if method == 'POST']
                    self.assertEqual(writes[:1 if phase == 'issue' else 2],
                                     ['statuses/' + MOVED] if phase == 'issue' else ['statuses/' + HEAD, 'statuses/' + MOVED])
                    if incomplete:
                        self.assertIn('page 2 after 3 attempts', notes)
                        self.assertEqual(sum(path.endswith('page=2') for _, path in requests), 3)
                finally:
                    for error in failures:
                        if isinstance(error, HTTPError): error.close()
                    server.shutdown()
                    server.server_close()
                    thread.join(timeout=2)
                self.assertFalse(thread.is_alive())

    def test_per_pr_boundary_does_not_catch_programming_errors(self):
        original = self.api
        for phase in ('issues/90', 'statuses/' + HEAD, 'actions/workflows/', 'actions/runs/42/rerun'):
            for failure in (TypeError('programming error'), ValueError('programming error')):
                with self.subTest(phase=phase, failure=type(failure).__name__):
                    def api(path, method='GET', data=None):
                        if path.startswith(phase): raise failure
                        return original(path, method, data)
                    with self.assertRaises(type(failure)) as caught:
                        ci.recheck_main_repairs(api, 'shinma06/android-replay-buffer')
                    self.assertIs(caught.exception, failure)
                    self.assertFalse(hasattr(failure, '__notes__'))

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
