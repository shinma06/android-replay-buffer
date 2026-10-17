#!/usr/bin/env python3
"""One bounded PR transition per tick. Run from trusted main, never from PR code."""
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import html
import json
import os
import posixpath
from pathlib import Path
import re
import shutil
import socket
import string
import subprocess
import sys
import time
import uuid
from urllib.parse import quote, unquote, urlsplit

from agent_policy import CONTEXT, SESSION_HASH, binding, eligible, gui_pass, in_scope, issue_number, next_action, session_fingerprint, update_parent, validate_review
from agent_worker import (REPORT_SCHEMA, private_repair_evidence, read_repair_bundle, recover_originals,
                          require_retained_evidence, retained_originals, run_worker, worker_environment)
from handoff_registry import digest, register, resolve, restore_review, review_privacy, store_review, verify_review_origin
from verification import verify_pr, metadata
from issue_schema import validate_issue, done_labels, labels
from pr_policy import validate_title
from qa_handoff import handoff
from change_impact import git_impact, test_commands, report as impact_report

REPO = 'shinma06/android-replay-buffer'
OWNER = 'shinma06'
ROOT = Path(__file__).resolve().parents[2]
HANDOFF = '<!-- agent-loop-handoff:v1 -->'
HANDOFF_V2 = '<!-- agent-loop-handoff:v2 -->'
STATE = '<!-- agent-loop-state:v1 -->'
HOST = socket.gethostname()
CI_CHECKS = ('test', 'PR policy', 'Acceptance gate')
PRIVATE_REVIEW_FORMAT = 'private-review-v1'
PUBLIC_REVIEW_KEYS = set(REPORT_SCHEMA['properties']) | {
    'session', 'origin_receipt', 'repair_evidence_sha256', 'certificate_evidence_sha256', 'feedback_hash',
}


def command(args, cwd=None, timeout=120, check=True):
    cwd = ROOT if cwd is None else cwd
    result = subprocess.run(args, cwd=cwd, env=worker_environment(), text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stderr[-1800:]}')
    return result.stdout.strip()


def git(*args, cwd=None):
    return command(['git', *args], cwd)


class GitHub:
    def api(self, path, method='GET', data=None):
        args = ['gh', 'api', '--method', method, path]
        if data is None:
            out = command(args)
        else:
            result = subprocess.run(args + ['--input', '-'], input=json.dumps(data), text=True, cwd=ROOT,
                                    capture_output=True, timeout=120)
            if result.returncode:
                raise RuntimeError(result.stderr[-1800:])
            out = result.stdout
        return json.loads(out) if out.strip() else None

    def pages(self, path):
        out = command(['gh', 'api', '--paginate', '--slurp', path])
        return [item for page in json.loads(out) for item in page]

    def review_threads(self, number):
        # REST already supplies every reply; GraphQL adds resolution/outdated state.
        query = '''query($endCursor:String){repository(owner:"shinma06",name:"android-replay-buffer"){
          pullRequest(number:NUMBER){reviewThreads(first:100,after:$endCursor){
            nodes{id isResolved isOutdated comments(first:1){nodes{databaseId}}}
            pageInfo{hasNextPage endCursor}}}}}'''.replace('NUMBER', str(number))
        pages = json.loads(command(['gh', 'api', 'graphql', '--paginate', '--slurp', '-f', 'query=' + query]))
        if any(page.get('errors') for page in pages):
            raise ValueError('Review thread acquisition failed; feedback is incomplete')
        return [node for page in pages for node in page['data']['repository']['pullRequest']['reviewThreads']['nodes']]

    def pr(self, number):
        pr = self.api(f'repos/{REPO}/pulls/{number}')
        if pr['state'] == 'open' and eligible(pr, OWNER, REPO):
            # pulls.base.sha can lag behind the branch after another PR merges.
            # Use the actual branch ref for review/GUI bindings and final merge checks.
            ref = self.api(f'repos/{REPO}/git/ref/heads/{quote(pr["base"]["ref"], safe="")}')
            if (ref.get('ref') != 'refs/heads/' + pr['base']['ref'] or
                    ref.get('object', {}).get('type') != 'commit' or
                    not re.fullmatch(r'[0-9a-f]{40}', ref['object'].get('sha', ''))):
                raise ValueError('Invalid base branch reference; cannot reconcile acceptance')
            pr['base']['sha'] = ref['object']['sha']
        return pr

    def issue(self, number):
        return self.api(f'repos/{REPO}/issues/{number}')

    def comments(self, number):
        return self.pages(f'repos/{REPO}/issues/{number}/comments?per_page=100')

    def comment(self, number, body, comment_id=None):
        path = f'repos/{REPO}/issues/comments/{comment_id}' if comment_id else f'repos/{REPO}/issues/{number}/comments'
        return self.api(path, 'PATCH' if comment_id else 'POST', {'body': body})

    def status(self, sha, state, description, url):
        existing = self.api(f'repos/{REPO}/commits/{sha}/status')['statuses']
        latest = next((x for x in existing if x['context'] == CONTEXT), None)
        if latest and latest['state'] == state and latest['description'] == description:
            return
        self.api(f'repos/{REPO}/statuses/{sha}', 'POST', {'context': CONTEXT, 'state': state,
                 'description': description[:140], 'target_url': url})

    def ci(self, pr):
        checks = json.loads(command(['gh', 'pr', 'checks', str(pr['number']), '--repo', REPO,
                                    '--json', 'name,state'], check=False) or '[]')
        states = {x['name']: x['state'] for x in checks}
        needed = [states.get(x) for x in CI_CHECKS]
        if any(x in ('FAILURE', 'ERROR', 'CANCELLED', 'TIMED_OUT', 'ACTION_REQUIRED') for x in needed):
            return 'failure'
        return 'success' if all(x == 'SUCCESS' for x in needed) else 'pending'

    def development(self, pr):
        query = '''query($pr:Int!,$issue:Int!){repository(owner:"shinma06",name:"android-replay-buffer"){
          defaultBranchRef{name} pullRequest(number:$pr){id closingIssuesReferences(first:100){
            nodes{number state repository{nameWithOwner}} pageInfo{hasNextPage}}}
          issue(number:$issue){id state closedByPullRequestsReferences(first:100,includeClosedPrs:true){
            nodes{number repository{nameWithOwner}} pageInfo{hasNextPage}}}}}'''
        result = self.api('graphql', 'POST', {'query': query,
                          'variables': {'pr': pr['number'], 'issue': issue_number(pr)}})
        try:
            repo = result['data']['repository']
            links = repo['pullRequest']['closingIssuesReferences']
            reverse = repo['issue']['closedByPullRequestsReferences']
            if (result.get('errors') or not isinstance(repo['defaultBranchRef']['name'], str)
                    or not repo['defaultBranchRef']['name'] or not isinstance(links['nodes'], list)
                    or links['pageInfo']['hasNextPage'] is not False
                    or not isinstance(reverse['nodes'], list) or reverse['pageInfo']['hasNextPage'] is not False
                    or any(not isinstance(x['number'], int) or not isinstance(x['repository']['nameWithOwner'], str)
                           or not x['repository']['nameWithOwner']
                           for connection in (links, reverse) for x in connection['nodes'])
                    or any(x['state'] not in ('OPEN', 'CLOSED') for x in links['nodes'])):
                raise ValueError('Development closing references are incomplete; owner must reconcile before merge')
            return repo
        except (KeyError, TypeError) as error:
            raise ValueError('Development closing references unavailable; owner must read back before merge') from error

    def link_completed_issue(self, pr, issue):
        repo = self.development(pr)
        if (not pr.get('merged') or issue['state'] != 'closed' or repo['issue']['state'] != 'CLOSED'
                or 'status:done' not in labels(issue)):
            raise ValueError('Development linking requires actual scoped completion and Issue closure')
        query = '''mutation($issue:ID!,$prs:[ID!]!){addCloseIssueReferences(
          input:{issueId:$issue,pullRequestIds:$prs}){issue{number state}}}'''
        linked = repo
        if (not any(x['number'] == issue_number(pr) and x['repository']['nameWithOwner'] == REPO
                    for x in repo['pullRequest']['closingIssuesReferences']['nodes'])
                or not any(x['number'] == pr['number'] and x['repository']['nameWithOwner'] == REPO
                           for x in repo['issue']['closedByPullRequestsReferences']['nodes'])):
            result = self.api('graphql', 'POST', {'query': query,
                              'variables': {'issue': repo['issue']['id'], 'prs': [repo['pullRequest']['id']]}})
            if not isinstance(result, dict) or result.get('errors'):
                raise ValueError('Development linking failed; preserve resources for owner readback')
            linked = self.development(pr)
        current = self.issue(issue_number(pr))
        if (not any(x['number'] == issue_number(pr) and x['repository']['nameWithOwner'] == REPO
                    for x in linked['pullRequest']['closingIssuesReferences']['nodes'])
                or not any(x['number'] == pr['number'] and x['repository']['nameWithOwner'] == REPO
                           for x in linked['issue']['closedByPullRequestsReferences']['nodes'])
                or current['state'] != 'closed' or 'status:done' not in labels(current)
                or current.get('closed_at') != issue.get('closed_at')):
            raise ValueError('Development/Issue closure readback failed; owner must reconcile')

    def merge(self, pr):
        rules = self.api(f'repos/{REPO}/rules/branches/{pr["base"]["ref"]}')
        by_type = {rule['type']: rule for rule in rules}
        required = {*CI_CHECKS, CONTEXT}
        checks = by_type.get('required_status_checks', {}).get('parameters', {})
        policy = by_type.get('pull_request', {}).get('parameters', {})
        if (not {'deletion', 'non_fast_forward', 'pull_request', 'required_status_checks'} <= by_type.keys()
                or not checks.get('strict_required_status_checks_policy')
                or not required <= {x['context'] for x in checks.get('required_status_checks', [])}
                or not policy.get('required_review_thread_resolution')):
            raise ValueError('Required branch protection is not active; configure and read back rules first')
        for rule_id in {rule['ruleset_id'] for rule in rules}:
            if self.api(f'repos/{REPO}/rulesets/{rule_id}').get('bypass_actors'):
                raise ValueError('Automatic integration requires rulesets without bypass actors')
        # Both the head guard and active strict branch rules are enforced by GitHub.
        if pr['draft']:
            raise ValueError('Draft must be ready and its remote review completed before merge')
        repo = self.development(pr)
        if repo['issue']['state'] != 'OPEN':
            raise ValueError('Issue closed before merge; owner must reconcile actual scope')
        if (pr['base']['ref'] == repo['defaultBranchRef']['name']
                and any(x['state'] == 'OPEN' for x in repo['pullRequest']['closingIssuesReferences']['nodes'])):
            raise ValueError('Default-branch closing links would close Issues before feedback readback; '
                             'owner must unlink manual/branch links or remove closing keywords, then re-read')
        if (pr['base']['ref'] == 'develop'
                and (not any(x['number'] == issue_number(pr) and x['repository']['nameWithOwner'] == REPO
                             for x in repo['pullRequest']['closingIssuesReferences']['nodes'])
                     or not any(x['number'] == pr['number'] and x['repository']['nameWithOwner'] == REPO
                                for x in repo['issue']['closedByPullRequestsReferences']['nodes']))):
            raise ValueError('Develop Development pair is missing; owner must link and read back before merge')
        validate_title(pr, issue_number(pr))
        payload = {'sha': pr['head']['sha'], 'merge_method': 'merge' if metadata(pr)[2] == 'promotion' else 'squash'}
        if pr['base']['ref'] == repo['defaultBranchRef']['name']:
            # Pin the new commit text; repository squash defaults can include discarded commit messages.
            payload.update(commit_title=f'{pr["title"]} (#{pr["number"]})', commit_message=pr['body'])
            messages = [payload['commit_title'], payload['commit_message']]
            if payload['merge_method'] == 'merge':
                # shortcut: PR REST caps at 250 commits; add a complete history query when larger promotions need automation.
                commits = self.pages(f'repos/{REPO}/pulls/{pr["number"]}/commits?per_page=100')
                if len(commits) != pr['commits'] or not commits:
                    raise ValueError('Integrated commit list is incomplete; owner must reconcile before merge')
                for commit in commits:
                    message = commit.get('commit', {}).get('message')
                    if not isinstance(message, str):
                        raise ValueError('Integrated commit message unavailable; owner must reconcile before merge')
                    messages.append(message)
            keywords = r'\b(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?):?\s+'
            text = '\n'.join(messages)
            references = re.findall(keywords + r'(?:([\w.-]+/[\w.-]+))?#([0-9]+)\b', text, re.IGNORECASE)
            references += re.findall(keywords + r'<?https?://github\.com/([\w.-]+/[\w.-]+)/(?:issues|pull)/([0-9]+)\b',
                                     text, re.IGNORECASE)
            for repository, number in set(references):
                issue = self.api(f'repos/{repository or REPO}/issues/{number}')
                if issue.get('state') != 'closed':
                    raise ValueError('Integrated commit closing keyword could close unfinished scope; '
                                     'owner must correct the message and re-read before merge')
        result = self.api(f'repos/{REPO}/pulls/{pr["number"]}/merge', 'PUT', payload)
        if not result.get('merged'):
            raise RuntimeError('Merge was not confirmed: ' + str(result))
        return result

    def ready(self, pr):
        command(['gh', 'pr', 'ready', str(pr['number']), '--repo', REPO])
        current = self.pr(pr['number'])
        if (current['draft'] or not eligible(current, OWNER, REPO) or
                current['head']['sha'] != pr['head']['sha'] or current['state'] != 'open' or
                current['head']['ref'] != pr['head']['ref'] or current['base']['ref'] != pr['base']['ref']):
            raise ValueError('Ready transition changed PR identity; preserve ownership and re-enroll')
        return current


def unpack(comments, marker):
    selected = [c for c in comments if c['user']['login'] == OWNER and c['body'].startswith(marker + '\n')]
    if not selected:
        return None, None
    latest = max(selected, key=lambda c: c['id'])
    payload = latest['body'].split('```json\n', 1)[1].split('\n```', 1)[0]
    record = json.loads(payload)
    if not isinstance(record, dict) or not record:
        raise ValueError('Agent loop record must be a nonempty JSON object')
    return record, latest['id']


def pack(marker, state):
    return marker + '\n' + f'Agent loop: **{state.get("phase", "handoff")}**\n\n```json\n' + json.dumps(state, ensure_ascii=False, indent=2) + '\n```'


@contextmanager
def coordinator_lock():
    # One active coordinator per OS user, including separate clones of this repo.
    path = Path('/tmp') / f'android-replay-buffer-coordinator-{os.getuid()}'
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


class Loop:
    github_reviewer = None

    def __init__(self, gh=None, storage=None, worker=run_worker, github_reviewer=None):
        self.gh = gh or GitHub()
        self.github_reviewer = review_selection(github_reviewer)
        # Stable across linked worktrees, persists through app restarts. Not checked in.
        common = Path(git('rev-parse', '--git-common-dir'))
        if not common.is_absolute():
            common = ROOT / common
        self.storage = Path(storage) if storage else common.resolve() / 'agent-loop'
        self.storage.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.worker = worker

    def load(self, number):
        comments = self.gh.comments(number)
        legacy, legacy_id = unpack(comments, HANDOFF)
        public, public_id = unpack(comments, HANDOFF_V2)
        handoff = resolve(self.storage, public, HOST) if public and (not legacy_id or public_id > legacy_id) else legacy
        state, comment_id = unpack(comments, STATE)
        if (state and SESSION_HASH.fullmatch(str(state.get('review', {}).get('session', '')))
                and not state.get('review_ref')):
            raise ValueError('Public session fingerprint requires its bound private review')
        if (state and not state.get('review_ref') and
                re.fullmatch(r'review-[0-9a-f-]{36}', str(state.get('review', {}).get('session', '')))):
            self.invalidate_acceptance(state)
            if not state.get('paused'):
                state.update(phase='queued', next='Legacy wrapper approval invalidated; fresh independent review required')
        if state and state.get('review_ref'):
            if state.get('review', {}).get('publication_format') == PRIVATE_REVIEW_FORMAT and 'findings' not in state['review']:
                state = self.restore_private_state(state)
            else:
                public = {'binding': state.get('binding'), 'review': state.get('review')}
                state['review'], paths = restore_review(self.storage, state['review_ref'], public, True)
                state['private_paths'] = paths
                _, sessions, _, _, limited = review_privacy(self.storage, state['review'], state.get('binding'), True)
                if limited:
                    raise ValueError('Unsupported old native publication; retain original/PAUSED/owner and use explicit owner recovery')
                if self._legacy_public_state(state['review'], paths, sessions) != public['review']:
                    raise ValueError('Private review public projection mismatch')
        elif state:
            _, _, limited = self.publication_privacy(state)
            if limited or state.get('review', {}).get('publication_format') == PRIVATE_REVIEW_FORMAT:
                raise ValueError('Native public state requires its bound private review; explicit owner recovery required')
        if state is not None and handoff:
            state['private_paths'] = sorted(set(state.get('private_paths', [])) | set(handoff.get('private_paths', [])))
        return handoff, state or {'phase': 'queued', 'fixes': 0, 'reviews': 0, 'errors': 0}, comment_id, comments

    def save(self, pr, state, comment_id):
        state['updated_at'] = int(time.time())
        state['review_requests'] = review_requests(state, self.github_reviewer, state.get('remote_review_after'))
        # Raw private logs stay local. Public state may contain reviewer text, so remove local paths.
        paths, _, limited = self.publication_privacy(state)
        *_, current_limited = review_privacy(self.storage, state.get('review'), state.get('binding'), True)
        if current_limited:
            state['private_publication'] = {'binding': state['binding'], 'review': state['review']}
        state['private_paths'] = sorted(set(state.get('private_paths', [])) | set(paths))
        paths = state.get('private_paths', ())
        if any(not isinstance(p, str) or not Path(p).is_absolute() for p in paths):
            raise ValueError('Coordinator private paths must be absolute verified paths')
        retained = {k: v for k, v in state.items() if k != 'review_ref'}
        public = self.public_state(retained, paths)
        if state.get('review') or limited:
            bound_review = {'binding': public.get('binding'), 'review': public['review']}
            # Never accept a public fingerprint as an originating session on resume.
            if state.get('review'):
                session_fingerprint(state['review']['session'])
            public['review_ref'] = (store_review(self.storage, public, {'kind': PRIVATE_REVIEW_FORMAT, 'state': retained}, paths)
                                    if limited else store_review(self.storage, bound_review, state['review'], paths))
            state['review_ref'] = public['review_ref']
        else:
            public.pop('review_ref', None)
            state.pop('review_ref', None)
        state = public
        body = pack(STATE, state)
        result = self.gh.comment(pr['number'], body, comment_id)
        # A dedicated Issue comment is the recoverable dashboard; don't overwrite Issue criteria.
        issue = issue_number(pr)
        marker = f'<!-- agent-loop-progress:pr-{pr["number"]} -->'
        comments = self.gh.comments(issue)
        prior = next((c for c in reversed(comments) if c['user']['login'] == OWNER and c['body'].startswith(marker)), None)
        progress = (marker + f'\nPR #{pr["number"]}: **{state["phase"]}**\n'
                    f'HEAD: {pr["head"]["sha"]}\nOwner: agent-loop\n'
                    f'Next: {state.get("next", state["phase"])}\nDetails: {result["html_url"]}')
        if not prior or prior['body'] != progress:
            self.gh.comment(issue, progress, prior['id'] if prior else None)
        return result['id']

    def private_error(self, error):
        identifier = uuid.uuid4().hex
        directory = self.storage / 'diagnostics'
        directory.mkdir(mode=0o700, exist_ok=True)
        path = directory / (identifier + '.txt')
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as stream:
            stream.write(str(error))
        return f'Operation failed ({type(error).__name__}); coordinator private diagnostic {identifier}'

    def restore_private_state(self, public):
        projection = {k: v for k, v in public.items() if k != 'review_ref'}
        retained = restore_review(self.storage, public.get('review_ref'), projection)
        if retained.get('kind') != PRIVATE_REVIEW_FORMAT or not isinstance(retained.get('state'), dict):
            raise ValueError('Unsupported private review state; explicit owner recovery required')
        state = retained['state']
        _, _, limited = self.publication_privacy(state)
        if not limited:
            raise ValueError('Private publication format requires verified native provenance')
        if self.public_state(state) != projection:
            raise ValueError('Private review public projection mismatch; retain originals for owner recovery')
        return dict(state, review_ref=public['review_ref'])

    def publication_privacy(self, state):
        paths, sessions, _, _, limited = review_privacy(self.storage, state.get('review'), state.get('binding'), True)
        retained = state.get('private_publication')
        if retained is not None:
            if not isinstance(retained, dict):
                raise ValueError('Invalid retained private publication provenance')
            old_paths, old_sessions, _, _, old_limited = review_privacy(self.storage, retained.get('review'), retained.get('binding'), True)
            if not old_limited:
                raise ValueError('Retained private publication requires verified native provenance')
            paths, sessions, limited = sorted(set(paths) | set(old_paths)), sorted(set(sessions) | set(old_sessions)), True
        return paths, sessions, limited

    def public_result(self, value):
        if isinstance(value, list):
            return [self.public_result(item) for item in value]
        if isinstance(value, dict) and (isinstance(value.get('review'), dict) or 'private_publication' in value):
            report = value.get('review', {})
            if report.get('publication_format') == PRIVATE_REVIEW_FORMAT and 'findings' not in report:
                self.restore_private_state(value)
                return value
            _, _, limited = self.publication_privacy(value)
            if limited:
                return self.public_state(value)
        return value

    def public_state(self, value, private_paths=(), private_sessions=()):
        if isinstance(value, dict) and (isinstance(value.get('review'), dict) or 'private_publication' in value and 'session' not in value):
            report, bound = value.get('review'), value.get('binding')
            paths, sessions, limited = self.publication_privacy(value)
            private_paths = set(private_paths) | set(paths)
            private_sessions = set(private_sessions) | set(sessions)
            if limited:
                if report is not None:
                    validate_review(report, bound, storage=self.storage)
                return self._private_public_state(value)
        elif isinstance(value, dict) and (value.get('origin_receipt') or str(value.get('session', '')).startswith('native-session-v1:')):
            raise ValueError('Publication requires the complete review binding and verified origin')
        return self._legacy_public_state(value, private_paths, private_sessions)

    def _private_public_state(self, state):
        report, bound = state.get('review'), state.get('binding', {})
        if report is None:
            envelope = {'publication_format': PRIVATE_REVIEW_FORMAT, 'status': 'pending',
                        'finding_count': None, 'summary': 'Fresh independent review required; prior private details retained'}
        else:
            envelope = {key: report[key] for key in ('head', 'base', 'verdict', 'scope_complete', 'issue_complete', 'gui_required')}
            envelope.update(publication_format=PRIVATE_REVIEW_FORMAT, session=session_fingerprint(report['session']),
                            finding_count=len(report['findings']), report_sha256=digest({'binding': bound, 'review': report}))
            for key in ('repair_revision', 'repair_evidence_sha256', 'certificate_revision', 'certificate_evidence_sha256', 'feedback_hash', 'origin_receipt'):
                if key in report and isinstance(report[key], str) and public_machine_value(('review', key), report[key]):
                    envelope[key] = report[key]
            envelope['summary'] = f"{report['verdict']}; {len(report['findings'])} findings; details in private review reference"
        phase = state.get('phase', 'queued')
        if not public_machine_value(('phase',), phase):
            raise ValueError('Invalid coordinator phase')
        phase_next = {
            'syncing': 'merge-current-base-then-publish',
            'publishing': 'validate-and-publish-issue-branch',
            'remote-review-wait': 'wait-for-current-head-review-completion',
            'reviewing': 'complete-independent-read-only-review',
            'fixing': 'apply-bounded-private-review-findings',
            'gui-queued': 'designated-operator-observe-required-gui-evidence',
            'acceptance-wait': 'complete-required-acceptance',
            'ci-wait': 'wait-for-required-ci-checks',
            'ci-failed': 'inspect-ci-failure-and-recover',
            'blocked': 'owner-recovery',
            'merging': 'confirm-github-merge-result',
            'cleanup': 'verify-post-merge-review-and-recovery-before-issue-or-resource-changes',
            'done': 'pm-confirm-issue-qa-project-and-roadmap-status',
        }
        if state.get('paused'):
            selected_next = 'owner-resume-required'
        elif phase == 'cleanup' or (report is not None and phase in ('merging', 'done')):
            selected_next = phase_next[phase]
        elif state.get('errors') and state.get('retry_at'):
            selected_next = 'check-scheduled-retry-and-private-diagnostic'
        elif phase in ('syncing', 'publishing', 'remote-review-wait', 'reviewing'):
            selected_next = phase_next[phase]
        elif report is None:
            selected_next = 'review'
        elif phase in ('queued', 'reviewed'):
            selected_next = ('wait-for-current-head-review-completion' if bound.get('remote_review_complete') is False else
                             {'approved': 'reconcile-acceptance-and-review-obligations',
                              'changes_requested': 'fix' if state.get('fixes', 0) < 3 else 'owner-recovery',
                              'blocked': 'owner-recovery'}[report['verdict']])
        else:
            selected_next = phase_next[phase]
        public = {'phase': phase, 'review': envelope, 'binding': {},
                  'state_sha256': digest({k: v for k, v in state.items() if k != 'review_ref'}),
                  'next': selected_next}
        for section, keys in (
                ('binding', ('head', 'base', 'target', 'title_hash', 'body_hash', 'issue_hash', 'feedback_hash')),
                ('gui', ('head', 'base', 'artifact_sha256')),
                ('acceptance', ('head', 'base', 'mode', 'repair_revision', 'repair_evidence_sha256', 'certificate_revision', 'certificate_evidence_sha256')),
                ('issue_closure', ('head', 'base', 'issue_hash'))):
            source = state.get(section, {})
            if not isinstance(source, dict):
                raise ValueError('Invalid coordinator machine record')
            if section in state:
                public[section] = {}
            for key in keys:
                if key in source:
                    if not isinstance(source[key], str) or not public_machine_value((section, key), source[key]):
                        raise ValueError('Invalid coordinator machine fact')
                    public[section][key] = source[key]
        for section, keys in (('', ('paused',)), ('binding', ('issue_bug', 'remote_review_complete')),
                              ('gui', ('processes_stopped',)), ('acceptance', ('allowed', 'gui_complete'))):
            source, target = (state, public) if not section else (state.get(section, {}), public.get(section, {}))
            for key in keys:
                if key in source:
                    if type(source[key]) is not bool:
                        raise ValueError('Invalid coordinator boolean')
                    target[key] = source[key]
        for key in ('updated_at', 'errors', 'fixes', 'reviews', 'qa_issue', 'main_tracking_issue'):
            if key in state:
                if type(state[key]) is not int or state[key] < 0:
                    raise ValueError('Invalid coordinator counter')
                public[key] = state[key]
        for key in ('expected_head', 'publish_head', 'merge_sha', 'interrupted_phase', 'remote_review_after', 'review_ref'):
            if key in state:
                if not isinstance(state[key], str) or not public_machine_value((key,), state[key]):
                    raise ValueError('Invalid coordinator identity')
                public[key] = state[key]
        for section in ('binding', ''):
            source, target = (bound, public['binding']) if section else (state, public)
            for key in ('review_completion', 'review_requests'):
                if key not in source:
                    continue
                records = source[key] if key == 'review_requests' else [source[key]]
                if not isinstance(records, list):
                    raise ValueError('Invalid retained review policy')
                projected = []
                for record in records:
                    if not isinstance(record, dict):
                        raise ValueError('Invalid retained review policy')
                    normalized = review_requests({'review_requests': [record]}, record.get('reviewer'), record.get('not_before'))
                    if len(normalized) != 1:
                        raise ValueError('Invalid retained review policy')
                    item = normalized[0]
                    for field, content in item.items():
                        if content is not None and not public_machine_value(('binding', 'review_completion', field), content):
                            raise ValueError('Invalid retained review policy fact')
                    if 'requests_hash' in record:
                        if not public_machine_value(('binding', 'review_completion', 'requests_hash'), record['requests_hash']):
                            raise ValueError('Invalid retained review policy hash')
                        item['requests_hash'] = record['requests_hash']
                    projected.append(item)
                target[key] = projected if key == 'review_requests' else projected[0]
        return public

    def _legacy_public_state(self, value, private_paths=(), private_sessions=(), location=()):
        if isinstance(value, dict):
            if not location and 'session' in value:
                location = ('review',)
            review = value.get('review') if isinstance(value.get('review'), dict) else {}
            private_sessions = set(private_sessions) | {str(value.get('session', '')).lower(), str(review.get('session', '')).lower()}
            public = {}
            for key, item in value.items():
                if key == 'private_publication' and not location:
                    continue
                if key in ('host', 'source', 'repair_evidence', 'evidence_kind', 'private_repair_evidence',
                           'private_paths', 'private_sessions', 'private_identifiers', 'evidence_records'):
                    continue
                projected = key
                if location[:1] == ('review',) and not (location == ('review',) and key in PUBLIC_REVIEW_KEYS):
                    if not isinstance(key, str):
                        raise ValueError('Review diagnostic keys must be strings')
                    projected = self._legacy_public_state(key, private_paths, private_sessions, ('diagnostic_key',))
                if projected in public:
                    raise ValueError('Public review diagnostic key collision; retain the private report')
                public[projected] = self._legacy_public_state(item, private_paths, private_sessions, location + (key,))
            return public
        if isinstance(value, list):
            return [self._legacy_public_state(v, private_paths, private_sessions, location + ('*',)) for v in value]
        if isinstance(value, str):
            # Full raw paths can contain spaces across the later lexical spans.
            for path in sorted(private_paths, key=len, reverse=True):
                value = value.replace(path, '[private path]')
            for private in (str(self.storage), str(ROOT), str(Path.home()), HOST):
                value = value.replace(private, '[local]')
            markdown_atom = re.compile(r'\\([' + re.escape(string.punctuation) +
                                       r'])|(&(?:#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[A-Za-z][A-Za-z0-9]*);)')
            def private_uri(token, bounded=False):
                positions, offset = [], 0
                def markdown_char(escape):
                    nonlocal offset
                    positions.extend((i, i + 1) for i in range(offset, escape.start()))
                    if escape[1] is not None:
                        replacement = escape[1]
                    else:
                        reference = escape[2]
                        replacement = (html.unescape(reference) if reference.startswith('&#') or
                                       reference[1:] in html.entities.html5 else reference)
                    positions.extend((escape.start(), escape.end()) for _ in replacement)
                    offset = escape.end()
                    return replacement
                # One pass: escaped ampersands and decoded references are not reinterpreted.
                destination = markdown_atom.sub(markdown_char, token)
                positions.extend((i, i + 1) for i in range(offset, len(token)))
                if not re.match(r'(?i)(?:https?://|file://|/)', destination):
                    redacted, cursor = [], 0
                    patterns = [] if bounded else [re.escape(path) for path in sorted(private_paths, key=len, reverse=True) if path]
                    patterns.append(r'(?i:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12})')
                    for match in re.finditer('|'.join(patterns), destination):
                        identity = match[0].lower()
                        if not bounded and match[0] in private_paths:
                            replacement = '[private URI]'
                        elif identity in private_sessions or session_fingerprint(identity) in private_sessions:
                            replacement = session_fingerprint(identity)
                        else:
                            continue
                        start, end = positions[match.start()][0], positions[match.end() - 1][1]
                        redacted.extend((token[cursor:start], replacement))
                        cursor = end
                    return ''.join(redacted) + token[cursor:], False
                decoded = unquote(destination)
                try:
                    path = posixpath.normpath(unquote(urlsplit(destination).path))
                except ValueError:
                    path = ''
                # Keep enclosing Markdown punctuation beyond the complete known filename.
                closing = 0 if bounded else len(token) - len(token.rstrip(')'))
                matched = []
                for known in private_paths:
                    own_closing = known.count(')') - known.count('(')
                    enclosing = min(closing, max(0, decoded.count(')') - decoded.count('(') - own_closing))
                    target = path[:-enclosing] if enclosing else path
                    if known in decoded or posixpath.normpath(known) == posixpath.normpath(target):
                        matched.append(known)
                protected = max((known.count(')') - known.count('(') for known in matched), default=0)
                suffix = ')' * min(closing, max(0, decoded.count(')') - decoded.count('(') - protected))
                if (matched and
                        (destination != token or '%' in token or not any(known in token for known in private_paths))):
                    return '[private URI]' + suffix, False
                identities = re.findall(r'(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}', decoded)
                if (destination != token or '%' in token) and any(identity.lower() in private_sessions or
                                        session_fingerprint(identity.lower()) in private_sessions for identity in identities):
                    return '[private URI]' + suffix, False
                return token, bool(re.match(r'(?i)https?://', destination))
            span = r'(?:\\[' + re.escape(string.punctuation) + r']|\](?=[^()\s<>"`])|[^\s<>"`\]])*'
            def private_text(text):
                cursor, parts = 0, []
                for match in re.finditer(r'(?:\\[' + re.escape(string.punctuation) + r']|[^\s<>"`\[\]()])' + span, text):
                    parts.extend(((text[cursor:match.start()], False), private_uri(match[0])))
                    cursor = match.end()
                return parts + [(text[cursor:], False)]
            # Raw syntax bounds the destination; interpreted references cannot add delimiters.
            pieces, cursor = [], 0
            for opening in re.finditer(r'\](?:\(|:)\s*', value):
                prefix = value[:opening.start()]
                if (len(prefix) - len(prefix.rstrip('\\'))) % 2:
                    continue
                start = opening.end()
                if start < cursor or start >= len(value):
                    continue
                angle = value[start] == '<'
                start += angle
                end, depth = start, 0
                while end < len(value):
                    atom = markdown_atom.match(value, end)
                    if atom:
                        end = atom.end()
                        continue
                    char = value[end]
                    if angle:
                        if char in '<>\r\n':
                            break
                    else:
                        if char == ' ' or ord(char) < 32 or ord(char) == 127:
                            break
                        if char == '(':
                            depth += 1
                        elif char == ')':
                            if not depth:
                                break
                            depth -= 1
                    end += 1
                if (end == start or depth or
                        (not angle and end == len(value) and value[opening.start() + 1] == '(') or
                        (angle and (end == len(value) or value[end] != '>'))):
                    continue
                pieces.extend(private_text(value[cursor:start]))
                pieces.append(private_uri(value[start:end], True))
                cursor = end
            pieces.extend(private_text(value[cursor:]))
            # Preserve bare URIs embedded in surrounding prose or punctuation.
            def bare_parts(text):
                spans = re.split(r'(?i)((?:https?://|file://|/)' + span + ')', text)
                return [private_uri(part) if i % 2 else (part, False) for i, part in enumerate(spans)]
            parts, prose = [], []
            for text, is_url in pieces:
                if not is_url:
                    prose.append(text)
                    continue
                parts.extend(bare_parts(''.join(prose)))
                prose.clear()
                parts.append((text, True))
            parts.extend(bare_parts(''.join(prose)))
            public = []
            for part, is_url in parts:
                part = re.sub(r'(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}',
                              lambda match: session_fingerprint(match[0].lower())
                              if not is_url or match[0].lower() in private_sessions or
                              session_fingerprint(match[0].lower()) in private_sessions else match[0], part)
                public.append(part if is_url else re.sub(r'/(?:Users|home|private|tmp|var)/[^\n`]+', '[local path]', part))
            return ''.join(public)
        return value

    def acceptance(self, pr, path):
        def read_git(*args):
            return subprocess.check_output(['git', *args], cwd=path, env=worker_environment(),
                                           text=True, stderr=subprocess.PIPE, timeout=120).strip()
        try:
            result = verify_pr(pr, lambda p: self.gh.api(f'repos/{REPO}/' + p),
                               read_git)
            return dict(result, allowed=True)
        except (ValueError, KeyError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
            return {'allowed': False, 'reason': self.private_error(error)}

    def validate_review(self, report, bound, accepted):
        validate_review(report, bound, accepted, self.storage)

    def retained_history(self, h):
        paths = list(h.get('private_paths', []))
        if h.get('repair_evidence'):
            paths.append(h['repair_evidence'])
        records = h.get('evidence_records', [])
        if not paths and not records:
            return retained_originals(records)
        roots = (h['source'], self.storage / f'pr-{h["pr"]}' / 'checkout')
        return retained_originals(records, paths, roots)

    def repair_evidence(self, accepted, h, bound):
        revision_field = 'certificate_revision' if accepted.get('certificate_revision') else 'repair_revision'
        if not accepted.get(revision_field):
            self.retained_history(h)
            return None
        roots = (h['source'], self.storage / f'pr-{h["pr"]}' / 'checkout')
        packet = private_repair_evidence(h.get('repair_evidence'), accepted, bound, roots, revision_field=revision_field)
        records = self.retained_history(h)
        packet['evidence_records'] = retained_originals(
            [*records, *packet['evidence_records']], packet['private_paths'], roots)
        accepted[revision_field.replace('_revision', '_evidence_sha256')] = packet['bundle_sha256']
        return packet

    def merged_acceptance(self, pr, h, state):
        # Closed PR refs retain the reviewed base; current failure records/main stay live.
        state.pop('acceptance', None)
        accepted = self.acceptance(pr, self.checkout(pr, h))
        if not accepted.get('allowed'):
            raise ValueError(accepted.get('reason', 'Current merged acceptance is incomplete'))
        packet = self.repair_evidence(accepted, h, {'head': pr['head']['sha'], 'base': pr['base']['sha']})
        if packet:
            state['private_paths'] = sorted(set(state.get('private_paths', [])) | set(packet['private_paths']))
        return dict(accepted, head=pr['head']['sha'], base=pr['base']['sha'])

    def bound(self, pr, issue, comments, remote_after=None):
        prior, _ = unpack(comments, STATE)
        if remote_after is None and prior:
            remote_after = prior.get('remote_review_after')
        value = binding(pr, issue)
        # Only our own changing dashboards are excluded, never other reviewers/bots.
        conversation = [c for c in comments + self.gh.comments(issue_number(pr))
                        if not (c['user']['login'] == OWNER and
                        c['body'].startswith((HANDOFF, HANDOFF_V2, STATE, '<!-- agent-loop-progress:',
                                              '<!-- issue-qa-link:v1 ', '<!-- issue-qa-record:v1 ')))]
        inline = self.gh.pages(f'repos/{REPO}/pulls/{pr["number"]}/comments?per_page=100')
        reviews = self.gh.pages(f'repos/{REPO}/pulls/{pr["number"]}/reviews?per_page=100')
        threads = self.gh.review_threads(pr['number'])
        feedback = [*conversation, *inline, *reviews, *threads]
        value['feedback_hash'] = hashlib.sha256(json.dumps(feedback, sort_keys=True).encode()).hexdigest()
        reviewer = review_selection(self.github_reviewer)
        value['review_completion'] = {'method': 'github-review-v1' if reviewer else 'codex-summary-v1',
                                      'reviewer': reviewer, 'not_before': remote_after}
        if reviewer:
            value['review_completion']['requests_hash'] = hashlib.sha256(json.dumps(
                {k: pr.get(k) for k in ('requested_reviewers', 'requested_teams')}, sort_keys=True).encode()).hexdigest()
        requests = review_requests(prior or {}, reviewer, remote_after)
        # A service record/request is also an obligation even before a coordinator save.
        if reviewer and any(c['user']['login'] == 'chatgpt-codex-connector[bot]' or
                re.search(r'^\s*@codex\s+review\b', c['body'], re.M) for c in comments):
            requests = review_requests({'review_requests': requests}, None, remote_after)
        value['review_requests'] = requests
        complete = True
        for request in requests:
            observed = (remote_review_complete(pr['head']['sha'], comments, request['not_before'])
                        if request['method'] == 'codex-summary-v1' else
                        github_review_complete(pr, reviews, request['reviewer'], request['not_before']))
            if type(observed) is not bool:
                raise ValueError('Review completion must be an observed boolean')
            complete = complete and observed
        value['remote_review_complete'] = complete
        return value, feedback

    def source_safe(self, h):
        source = Path(h['source'])
        if not source.exists():
            raise ValueError('Original writer worktree is missing; explicitly recover ownership before continuing')
        if source.exists():
            if git('branch', '--show-current', cwd=source) != h['branch'] or git('rev-parse', 'HEAD', cwd=source) != h['head']:
                raise ValueError('Original writer resumed or changed branch; handoff must be renewed')
            if git('status', '--porcelain', cwd=source):
                raise ValueError('Original handoff worktree has edits; refusing concurrent writer')

    def checkout(self, pr, h):
        path = self.storage / f'pr-{pr["number"]}' / 'checkout'
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            git('clone', '--no-local', str(ROOT), str(path))
            git('remote', 'set-url', 'origin', f'https://github.com/{REPO}.git', cwd=path)
            git('fetch', 'origin', h['branch'], pr['base']['ref'], cwd=path)
            git('checkout', '-b', h['branch'], pr['head']['sha'], cwd=path)
            # Do not run hook implementations supplied by the PR; tests are run by coordinator.
            git('config', 'core.hooksPath', str(ROOT / '.githooks'), cwd=path)
        return path

    @staticmethod
    def invalidate_acceptance(state):
        for key in ('review', 'review_ref', 'binding', 'gui', 'acceptance'):
            state.pop(key, None)

    def test_and_push(self, pr, path, state):
        self.invalidate_acceptance(state)
        expected = pr['head']['sha']
        actual = git('rev-parse', 'HEAD', cwd=path)
        remote = self.gh.pr(pr['number'])
        if (remote['head']['sha'] != expected or remote.get('title') != pr.get('title')):
            raise ValueError('Remote HEAD or title changed; do not publish recovered work')
        # These repositories are maintainer-owned, enrolled work only. No forks are executed.
        impact = git_impact(pr['base']['sha'], actual, cwd=path)
        print(impact_report(impact))
        for test in test_commands(impact):
            command(test, path, 600)
        if impact.get('base'):
            command(['git', 'diff', '--check', impact['base'], impact['head'], '--'], path)
        if git('status', '--porcelain', cwd=path):
            raise ValueError('Tests left dirty files; cannot publish')
        remote = self.gh.pr(pr['number'])
        if (not eligible(remote, OWNER, REPO) or remote['state'] != 'open' or
                remote['head']['sha'] != expected or remote.get('title') != pr.get('title')):
            raise ValueError('Remote PR changed during tests; preserve unpublished work')
        git('push', 'origin', f'HEAD:refs/heads/{pr["head"]["ref"]}', cwd=path)
        state['expected_head'] = actual
        state.pop('review', None)
        state.pop('gui', None)
        state['phase'] = 'queued'
        state['next'] = 'Review the newly published HEAD'

    def tick(self, number):
        pr = self.gh.pr(number)
        if not eligible(pr, OWNER, REPO):
            return {'pr': number, 'phase': 'unmanaged'}
        h, state, comment_id, comments = self.load(number)
        if (not h or h.get('host') != HOST or h.get('pr') != number or h.get('issue') != issue_number(pr)
                or h.get('branch') != pr['head']['ref'] or h.get('target', 'main') != pr['base']['ref'] or h.get('writer_stopped') is not True):
            return {'pr': number, 'phase': 'unmanaged'}
        if not state.get('expected_head'):
            state['expected_head'] = h['head']
        if pr['state'] == 'closed' and not pr.get('merged'):
            return {'pr': number, 'phase': 'retained-closed-unmerged',
                    'next': 'Owner/PM: preserve and restore-check under branch lifecycle; claim remains retained'}
        if state.get('paused'):
            return {'pr': number, 'phase': state['phase']}
        if pr.get('merged'):
            if state.get('phase') == 'done':
                return {'pr': number, 'phase': 'done'}
            try:
                return self.finish(pr, h, state, comment_id)
            except Exception as error:
                state.update(phase='cleanup', next=self.private_error(error))
                self.save(pr, state, comment_id)
                return {'pr': number, 'phase': 'cleanup', 'error': state['next']}
        if pr['state'] != 'open':
            return {'pr': number, 'phase': state['phase']}
        if state.get('retry_at', 0) > time.time():
            return {'pr': number, 'phase': 'backoff'}
        try:
            issue = self.gh.issue(h['issue'])
            if issue['state'] != 'open' or 'pull_request' in issue:
                raise ValueError('Issue must remain open until scoped acceptance is evaluated')
            validate_issue(issue)
            self.source_safe(h)
            path = self.checkout(pr, h)
            running = self.storage / f'pr-{number}' / 'worker.json'
            if running.exists():
                pid = json.loads(running.read_text())['pid']
                if pid is None:
                    raise ValueError('Worker launch was interrupted before PID acknowledgement; verify processes before resume')
                try:
                    os.killpg(pid, 0)
                    return {'pr': number, 'phase': 'worker-still-running'}
                except ProcessLookupError:
                    running.unlink()
            if pr['head']['sha'] != state['expected_head']:
                # A controller push can finish immediately before a crash. Recognize exactly its local tip.
                tip = git('rev-parse', 'HEAD', cwd=path)
                if (pr['head']['sha'] == tip == state.get('publish_head') and
                        not git('status', '--porcelain', cwd=path)):
                    state['expected_head'] = tip
                    self.invalidate_acceptance(state)
                else:
                    raise ValueError('External push after handoff; explicit re-enrollment required')
            bound, feedback = self.bound(pr, issue, comments, state.get('remote_review_after'))
            state['review_requests'] = bound['review_requests']
            if any(record and any(record.get(k) != bound[k] for k in ('head', 'base'))
                   for record in (state.get('binding'), state.get('gui'))):
                self.invalidate_acceptance(state)
                comment_id = self.save(pr, state, comment_id)
            self.gh.status(pr['head']['sha'], 'pending', 'Review/acceptance reconciliation in progress', pr['html_url'])
            current = git('rev-parse', 'HEAD', cwd=path)
            dirty = git('status', '--porcelain', cwd=path)
            if current != pr['head']['sha'] or dirty:
                if state['phase'] not in ('fixing', 'publishing', 'syncing'):
                    raise ValueError('Unexpected local changes; preserve and pause')
                if current != pr['head']['sha'] and current != state.get('publish_head'):
                    raise ValueError('Unrecognized local commit; coordinator must inspect history before publication')
                if dirty:
                    self.commit_fix(path, h, pr)
                    state['publish_head'] = git('rev-parse', 'HEAD', cwd=path)
                if git('merge-base', '--is-ancestor', pr['head']['sha'], 'HEAD', cwd=path) != '':
                    raise ValueError('Unexpected local ancestry')
                state['phase'] = 'publishing'
                comment_id = self.save(pr, state, comment_id)
                self.test_and_push(pr, path, state)
                self.save(pr, state, comment_id)
                return {'pr': number, 'phase': 'published-recovery'}
            # Strict base update is a normal merge, never a rebase/reset/force push.
            git('fetch', 'origin', pr['base']['ref'], h['branch'], cwd=path)
            latest = self.gh.pr(number)
            if (not eligible(latest, OWNER, REPO) or latest['state'] != 'open' or
                    latest['head']['sha'] != pr['head']['sha'] or latest['base']['ref'] != pr['base']['ref'] or
                    git('rev-parse', f'origin/{h["branch"]}', cwd=path) != pr['head']['sha']):
                self.invalidate_acceptance(state)
                raise ValueError('Remote PR changed during base fetch; preserve checkout and re-enroll')
            if (git('rev-parse', 'origin/' + pr['base']['ref'], cwd=path) != pr['base']['sha'] or
                    latest['base']['sha'] != pr['base']['sha']):
                self.invalidate_acceptance(state)
                state.update(phase='queued', next='Base moved during fetch; retry with the current branch ref')
                self.save(pr, state, comment_id)
                return {'pr': number, 'phase': 'base-race-retry'}
            check = subprocess.run(['git', 'merge-base', '--is-ancestor', pr['base']['sha'], 'HEAD'], cwd=path,
                                   env=worker_environment(), capture_output=True)
            if check.returncode not in (0, 1):
                raise ValueError('Cannot determine base ancestry; preserve checkout')
            if check.returncode:
                self.invalidate_acceptance(state)
                state['phase'] = 'syncing'
                comment_id = self.save(pr, state, comment_id)
                git('merge', '--no-edit', pr['base']['sha'], cwd=path)
                state['publish_head'] = git('rev-parse', 'HEAD', cwd=path)
                state['phase'] = 'publishing'
                self.save(pr, state, comment_id)
                self.test_and_push(pr, path, state)
                self.save(pr, state, comment_id)
                return {'pr': number, 'phase': 'base-synced'}
            if pr['draft']:
                self.invalidate_acceptance(state)
                state['review_requests'] = bound['review_requests']
                state['remote_review_after'] = datetime.now(timezone.utc).isoformat()
                state.update(phase='remote-review-wait', next='Mark enrolled stopped-writer Draft ready, then read its new terminal review')
                comment_id = self.save(pr, state, comment_id)
                pr = self.gh.ready(pr)
                self.save(pr, state, comment_id)
                return {'pr': number, 'phase': 'remote-review-wait'}
            gui_needed = h['gui_required'] or bool(state.get('review', {}).get('gui_required'))
            acceptance = self.acceptance(pr, path)
            private_evidence = self.repair_evidence(acceptance, h, bound)
            if private_evidence:
                state['private_paths'] = sorted(set(state.get('private_paths', [])) | set(private_evidence['private_paths']))
            if gui_needed and acceptance.get('mode') in ('develop', 'tooling') and not acceptance.get('cases'):
                acceptance = {'allowed': False, 'reason': 'Reviewer requires GUI Cases; declare them in the matrix'}
            if state.get('binding') == bound and state.get('review'):
                self.validate_review(state['review'], bound, acceptance)
            action = next_action(state, bound, self.gh.ci(pr), gui_needed, acceptance, self.storage)
            if action == 'review':
                if state.get('reviews', 0) >= 8:
                    raise ValueError('Review limit reached; explicit resume required')
                state.pop('review', None)
                state.pop('binding', None)
                state.update(phase='reviewing', next='Independent read-only review',
                             reviews=state.get('reviews', 0) + 1)
                comment_id = self.save(pr, state, comment_id)
                running.write_text(json.dumps({'pid': None}))
                report = self.worker('review', path, {'pr': pr['number'], **bound, 'title': pr['title'], 'issue': issue['body'],
                                      'scope': h['scope'], 'close_issue_requested': h['close_issue'],
                                      'feedback': feedback, 'gui': state.get('gui'), 'target': pr['base']['ref'], 'acceptance': acceptance,
                                      'private_repair_evidence': private_evidence}, path.parent / 'workers',
                                      on_start=lambda pid: running.write_text(json.dumps({'pid': pid})),
                                      origin_storage=self.storage, origin_binding=bound)
                running.unlink(missing_ok=True)
                self.validate_review(report, bound, acceptance)
                if git('rev-parse', 'HEAD', cwd=path) != pr['head']['sha'] or git('status', '--porcelain', cwd=path):
                    raise ValueError('Reviewer changed checkout; approval rejected')
                state.update(binding=bound, review=report, phase='reviewed', errors=0, next='Reconcile verdict on the next tick')
            elif action == 'fix':
                state.pop('publish_head', None)
                state.update(phase='fixing', fixes=state.get('fixes', 0) + 1, next='Apply bounded review findings')
                comment_id = self.save(pr, state, comment_id)
                running.write_text(json.dumps({'pid': None}))
                self.worker('fix', path, {**bound, 'issue': issue['body'], 'scope': h['scope'],
                            'findings': state['review']['findings']}, path.parent / 'workers',
                            on_start=lambda pid: running.write_text(json.dumps({'pid': pid})))
                running.unlink(missing_ok=True)
                if git('rev-parse', 'HEAD', cwd=path) != pr['head']['sha']:
                    raise ValueError('Fixer changed git history; preserve and pause')
                self.commit_fix(path, h, pr)
                state['publish_head'] = git('rev-parse', 'HEAD', cwd=path)
                state['phase'] = 'publishing'
                comment_id = self.save(pr, state, comment_id)
                self.test_and_push(pr, path, state)
            elif action == 'merge':
                latest = self.gh.pr(number)
                latest_issue = self.gh.issue(h['issue'])
                if latest_issue.get('state') != 'open' or 'pull_request' in latest_issue:
                    raise ValueError('Issue must remain open until scoped acceptance is evaluated')
                validate_issue(latest_issue)
                now, _ = self.bound(latest, latest_issue, self.gh.comments(number), state.get('remote_review_after'))
                latest_acceptance = self.acceptance(latest, path)
                self.repair_evidence(latest_acceptance, h, now)
                if (now != bound or self.gh.ci(latest) != 'success' or not latest_acceptance['allowed'] or
                        latest_acceptance != acceptance):
                    self.invalidate_acceptance(state)
                    state.update(phase='queued', next='PR/base/acceptance changed before merge; reconcile again')
                    self.save(latest, state, comment_id)
                    return {'pr': number, 'phase': 'changed-before-merge'}
                self.validate_review(state['review'], bound, latest_acceptance)
                state['acceptance'] = dict(acceptance, head=bound['head'], base=bound['base'])
                self.gh.status(bound['head'], 'success', 'Independent review and target branch acceptance gate passed', pr['html_url'])
                state.update(phase='merging', next='Read back GitHub merge result')
                comment_id = self.save(pr, state, comment_id)
                self.gh.merge(latest)
                return self.finish(self.gh.pr(number), h, state, comment_id)
            else:
                state.update(phase=action, next={'gui-queued': '能力・許可を確認した指定GUI担当: leaseを取得し、実画面を観察して証拠を記録',
                             'remote-review-wait': 'Await current-HEAD completion for: ' + ', '.join(
                                 r['reviewer'] or 'Codex' for r in bound['review_requests']) + '; retain every outstanding request',
                             'acceptance-wait': acceptance.get('reason', 'Complete the required acceptance matrix'),
                             'ci-wait': 'Wait for latest required CI checks', 'ci-failed': 'Inspect CI failure; resume after repair',
                             'blocked': 'Inspect review findings or retry budget; resume explicitly'}[action])
                if action in ('blocked', 'ci-failed'):
                    state['paused'] = True
                if action == 'ci-wait' and acceptance['allowed']:
                    self.gh.status(bound['head'], 'success', 'Independent review and target branch acceptance gate passed', pr['html_url'])
            self.save(pr, state, comment_id)
            return {'pr': number, 'phase': state['phase'], 'next': self.public_state(state).get('next')}
        except Exception as error:
            state['errors'] = state.get('errors', 0) + 1
            state['next'] = self.private_error(error)
            # Preserve fixing/publishing phase so an interrupted worker can be recovered without discarding files.
            state['interrupted_phase'] = state['phase']
            if isinstance(error, ValueError) or state['errors'] >= 3:
                state['paused'] = True
                state['phase'] = 'blocked'
            else:
                state['retry_at'] = time.time() + 300 * state['errors']
            self.save(pr, state, comment_id)
            return {'pr': number, 'phase': state['phase'], 'error': state['next']}

    def commit_fix(self, path, h, pr):
        changed = set(git('diff', '--no-renames', '--name-only', 'HEAD', cwd=path).splitlines())
        changed.update(git('ls-files', '--others', '--exclude-standard', cwd=path).splitlines())
        if not changed or not in_scope(changed, h['scope']):
            raise ValueError('No fix or out-of-scope changes; preserve files for coordinator review')
        if git('ls-files', '-u', cwd=path):
            raise ValueError('Merge conflict requires explicit coordinator repair')
        latest = self.gh.pr(pr['number'])
        if (not eligible(latest, OWNER, REPO) or latest['state'] != 'open' or
                any(latest.get(key) != pr.get(key) for key in ('head', 'base', 'body', 'title'))):
            raise ValueError('Live PR changed before commit; preserve unstaged work')
        validate_title(latest, h['issue'])
        git('add', '--', *sorted(changed), cwd=path)
        git('commit', '-m', latest['title'], cwd=path)

    def finish(self, pr, h, state, comment_id):
        pr = self.gh.pr(pr['number'])
        if not pr.get('merged'):
            raise ValueError('Cleanup requires confirmed GitHub merge')
        bound = state.get('binding', {})
        report = state.get('review', {})
        current_issue = self.gh.issue(h['issue'])
        now, _ = self.bound(pr, current_issue, self.gh.comments(pr['number']), state.get('remote_review_after'))
        if now != bound or now.get('remote_review_complete') is not True:
            raise ValueError('PR/Issue/feedback changed or remote review is pending after merge; preserve scope/resources for disposition')
        self.retained_history(h)
        accepted = state.get('acceptance', {})
        if pr['base']['ref'] == 'develop':
            accepted = self.merged_acceptance(pr, h, state)
        self.validate_review(report, bound, accepted)
        state['acceptance'] = accepted
        promotion_pass = (accepted.get('allowed') is True and accepted.get('mode') == 'promotion' and
                          accepted.get('gui_complete') is True and accepted.get('head') == bound.get('head') and
                          accepted.get('base') == bound.get('base'))
        complete = (pr['base']['ref'] == 'main' and h['close_issue'] and report.get('verdict') == 'approved' and report.get('scope_complete') and
                    report.get('issue_complete') and bound.get('head') == pr['head']['sha'] and
                    bound.get('issue_hash') == binding(pr, current_issue)['issue_hash'] and
                    (not (h['gui_required'] or report.get('gui_required')) or promotion_pass or gui_pass(state.get('gui'), bound)))
        closure = {key: bound[key] for key in ('head', 'base', 'issue_hash')}
        if (pr['base']['ref'] == 'main' and current_issue['state'] != 'open'
                and not (complete and state.get('issue_closure') == closure and 'status:done' in labels(current_issue))):
            raise ValueError('Issue closed before coordinator completion; owner must reconcile closure and remaining scope')
        transferred = False
        if pr['base']['ref'] == 'develop':
            axes = validate_issue(current_issue)
            # issue_complete means no implementation remains; GUI is tracked separately.
            implementation_complete = (axes['type'] in ('feature', 'bug', 'maintenance') and
                report.get('verdict') == 'approved' and report.get('scope_complete') is True and
                report.get('issue_complete') is True and bound.get('head') == pr['head']['sha'] and
                bound.get('issue_hash') == binding(pr, current_issue)['issue_hash'])
            if implementation_complete:
                _, case_path, _ = metadata(pr)
                git('fetch', '--no-tags', 'origin', pr['merge_commit_sha'])
                change = json.loads(git('show', f'{pr["merge_commit_sha"]}:{case_path}'))
                qa = handoff(self.gh, REPO, pr, current_issue, change)
                state['qa_issue'] = qa
                state['main_tracking_issue'] = 10
                complete = transferred = True
        state.update(phase='cleanup', next='Update Issue and remove only verified finished resources')
        comment_id = self.save(pr, state, comment_id)
        pr = self.gh.pr(pr['number'])
        if not pr.get('merged'):
            raise ValueError('Cleanup requires confirmed GitHub merge')
        issue = self.gh.issue(h['issue'])
        fresh, _ = self.bound(pr, issue, self.gh.comments(pr['number']), state.get('remote_review_after'))
        if fresh != bound or fresh.get('remote_review_complete') is not True:
            raise ValueError('PR/Issue/feedback changed during handoff; preserve scope/resources before closure')
        self.retained_history(h)
        if pr['base']['ref'] == 'develop':
            accepted = self.merged_acceptance(pr, h, state)
            self.validate_review(report, bound, accepted)
            state['acceptance'] = accepted
        if (pr['base']['ref'] == 'main' and issue['state'] != 'open'
                and not (complete and state.get('issue_closure') == closure and 'status:done' in labels(issue))):
            raise ValueError('Issue closed during handoff before owned completion; owner must reconcile')
        if complete and issue['state'] == 'open':
            if not transferred:
                state['issue_closure'] = closure
                comment_id = self.save(pr, state, comment_id)
            self.gh.api(f'repos/{REPO}/issues/{h["issue"]}', 'PATCH', {'state': 'closed', 'state_reason': 'completed', 'labels': done_labels(issue)})
            closed = self.gh.issue(h['issue'])
            if closed['state'] != 'closed' or 'status:done' not in labels(closed):
                raise ValueError('Issue closure readback failed')
        if complete and not transferred:
            self.gh.link_completed_issue(pr, self.gh.issue(h['issue']))
        if complete and not transferred and h.get('parent') is not None:
            parent = self.gh.issue(h['parent'])
            body = update_parent(parent['body'], h['issue'])
            if body != parent['body']:
                self.gh.api(f'repos/{REPO}/issues/{h["parent"]}', 'PATCH', {'body': body})
        self.cleanup(pr, h)
        state.update(phase='done',
                     next=('Claim released; Issue closed' if complete else 'Claim released; Issue remains open for remaining acceptance') +
                          '; PM: reconcile parent roadmap and Project registration/status for origin and QA, Milestone, Relationship Status and Development/Issue closure; record readback or retry owner; use scripts/workflow/project_item.py with own-repository Issue/PR URLs for Project item mutations',
                     merge_sha=pr['merge_commit_sha'])
        self.save(pr, state, comment_id)
        return {'pr': pr['number'], 'phase': 'done', 'issue_closed': bool(complete)}

    def reconcile_feedback(self, pr, evidence):
        h, state, sid, comments = self.load(pr['number'])
        if (not pr.get('merged') or not eligible(pr, OWNER, REPO) or not h or
                h.get('host') != HOST or h.get('writer_stopped') is not True or
                h.get('pr') != pr['number'] or h.get('issue') != issue_number(pr) or
                h.get('branch') != pr['head']['ref'] or h.get('target', 'main') != pr['base']['ref'] or
                state.get('expected_head', h['head']) != pr['head']['sha']):
            raise ValueError('Feedback recovery requires the unchanged local merged handoff')
        if (self.storage / f'pr-{pr["number"]}' / 'worker.json').exists():
            raise ValueError('Worker must be stopped before feedback recovery')
        self.source_safe(h)
        bound, _ = self.bound(pr, self.gh.issue(h['issue']), comments, state.get('remote_review_after'))
        if not bound['remote_review_complete'] or evidence.get('binding') != bound:
            raise ValueError('Pending or changed feedback; independent disposition must bind the exact snapshot')
        report = evidence['review']
        if report.get('feedback_hash') != bound['feedback_hash']:
            raise ValueError('Independent report must identify the feedback it actually reviewed')
        review_privacy(self.storage, report, bound)
        self.retained_history(h)
        previous = state.get('binding', {})
        if previous != bound:
            for key in ('gui', 'acceptance'):
                state.pop(key, None)
        if pr['base']['ref'] == 'develop':
            self.invalidate_acceptance(state)
            self.gh.status(pr['head']['sha'], 'pending', 'Current original-NG acceptance requires reconciliation', pr['html_url'])
            sid = self.save(pr, state, sid)
            accepted = self.merged_acceptance(pr, h, state)
        else:
            accepted = state.get('acceptance')
        self.validate_review(report, bound, accepted)
        if report['verdict'] != 'approved':
            raise ValueError('Unresolved feedback cannot resume closure or cleanup')
        if accepted is not None:
            state['acceptance'] = accepted
        state.update(binding=bound, review=report, phase='cleanup', paused=False, errors=0, retry_at=0,
                     next='Independent feedback disposition fixed; re-read before closure/cleanup')
        self.save(pr, state, sid)
        return self.public_result(state)

    def cleanup(self, pr, h):
        from gui_lease import locked, transition
        lease_dir = Path('/tmp') / f'cursor-in-android-studio-gui-{os.getuid()}'
        with locked(lease_dir) as lease:
            if transition(lease, 'status').get('state') != 'free':
                raise ValueError('GUI is occupied; defer resource cleanup until operator release')
            return self._cleanup_resources(pr, h)

    def _cleanup_resources(self, pr, h):
        branch = h['branch']
        if branch in ('main', 'master', 'develop'):
            raise ValueError('Persistent branches must never be cleaned up')
        roots = (h['source'], self.storage / f'pr-{pr["number"]}' / 'checkout')
        paths = list(h.get('private_paths', []))
        if h.get('repair_evidence'):
            _, bundle = read_repair_bundle(h['repair_evidence'], h.get('evidence_kind', 'repair_revision'))
            paths.extend([h['repair_evidence'], *[item['path'] for item in bundle['receipts']]])
        require_retained_evidence(paths, roots)
        retained_originals(h.get('evidence_records', []), paths, roots)
        path = self.storage / f'pr-{pr["number"]}' / 'checkout'
        if (path.parent / 'worker.json').exists():
            raise ValueError('Worker state remains; do not remove resources')
        if path.exists() and (git('rev-parse', 'HEAD', cwd=path) != pr['head']['sha'] or git('status', '--porcelain', cwd=path)):
            raise ValueError('Managed checkout changed; keep all resources')
        source = Path(h['source'])
        worktrees = git('worktree', 'list', '--porcelain')
        if source.exists():
            self.source_safe(h)
            if f'worktree {source}\n' not in worktrees or source.resolve() == ROOT.resolve():
                raise ValueError('Source is not an owned linked worktree; keep it')
        for entry in worktrees.split('\n\n'):
            if 'branch refs/heads/' + branch in entry and f'worktree {source}\n' not in entry:
                raise ValueError('Branch is active in another worktree; keep it')
        ref = 'refs/heads/' + branch
        old = command(['git', 'rev-parse', '--verify', ref], check=False)
        if old and old != h['head']:
            raise ValueError('Local branch changed; keep all resources')
        tip = git('ls-remote', f'https://github.com/{REPO}.git', ref)
        if tip:
            sha = tip.split()[0]
            if sha != pr['head']['sha']:
                raise ValueError('Remote branch was reused after merge; do not delete')
            # Compare-and-delete only. Never force-update a branch.
            if not path.exists():
                raise ValueError('No managed checkout for safe remote deletion; preserve ref')
            command(['git', 'push', f'--force-with-lease={ref}:{sha}', f'https://github.com/{REPO}.git',
                     ':' + ref], path)
        if path.exists():
            shutil.rmtree(path)
        if source.exists():
            git('worktree', 'remove', str(source))
        if old:
            git('update-ref', '-d', ref, old)



def public_machine_value(location, value):
    """Validate the fixed machine facts allowed in the private publication envelope."""
    if not location:
        return False
    scope, key = location[:-1], location[-1]
    pattern = None
    if scope in ((), ('binding',), ('review',), ('gui',), ('acceptance',), ('issue_closure',)):
        if key in ('head', 'base') or (not scope and key in ('expected_head', 'publish_head', 'merge_sha')):
            pattern = r'[0-9a-f]{40}'
        hashes = {
            ('binding',): ('title_hash', 'body_hash', 'issue_hash', 'feedback_hash'),
            ('issue_closure',): ('issue_hash',),
            ('review',): ('repair_revision', 'repair_evidence_sha256', 'certificate_revision', 'certificate_evidence_sha256', 'feedback_hash'),
            ('gui',): ('artifact_sha256',),
            ('acceptance',): ('repair_revision', 'repair_evidence_sha256', 'certificate_revision', 'certificate_evidence_sha256'),
        }
        if key in hashes.get(scope, ()):
            pattern = r'[0-9a-f]{64}'
    if (scope, key) in (((), 'review_ref'), (('review',), 'origin_receipt')):
        pattern = r'[0-9a-f]{32}'
    if scope == ('review',) and key == 'verdict':
        pattern = r'approved|changes_requested|blocked'
    if scope == ('binding',) and key == 'target':
        pattern = r'main|develop'
    if scope == ('acceptance',) and key == 'mode':
        pattern = r'develop|promotion|tooling'
    if not scope and key in ('phase', 'interrupted_phase'):
        pattern = (r'queued|syncing|publishing|remote-review-wait|reviewing|reviewed|fixing|merging|cleanup|done|'
                   r'blocked|gui-queued|acceptance-wait|ci-wait|ci-failed')
    if scope in (('binding', 'review_completion'), ('binding', 'review_requests', '*'), ('review_requests', '*')):
        if key == 'method':
            pattern = r'codex-summary-v1|github-review-v1'
        elif key == 'reviewer':
            pattern = r'[a-z0-9][a-z0-9-]*(?:\[bot\])?'
        elif key == 'requests_hash' and scope == ('binding', 'review_completion'):
            pattern = r'[0-9a-f]{64}'
        elif key == 'not_before':
            pattern = r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})'
    if not scope and key == 'remote_review_after':
        pattern = r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})'
    return pattern is not None and re.fullmatch(pattern, value) is not None


def review_selection(github_reviewer):
    if github_reviewer is not None and (not isinstance(github_reviewer, str) or
            not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]*(?:\[bot\])?', github_reviewer)):
        raise ValueError('Expected GitHub reviewer login is missing or malformed')
    return github_reviewer.lower() if github_reviewer else None



def review_requests(state, reviewer, not_before):
    """Retain source obligations independently of disposable approval bindings."""
    reviewer = review_selection(reviewer)
    requests = state.get('review_requests', [])
    if not isinstance(requests, list):
        raise ValueError('Invalid retained review requests')
    requests = list(requests)
    previous = state.get('binding')
    if previous:
        requests.extend(previous.get('review_requests', []))
        requests.append(previous.get('review_completion',
                        {'method': 'codex-summary-v1', 'reviewer': None,
                         'not_before': state.get('remote_review_after')}))
    elif 'review_requests' not in state and state.get('remote_review_after'):
        requests.append({'method': 'codex-summary-v1', 'reviewer': None,
                         'not_before': state['remote_review_after']})
    requests.append({'method': 'github-review-v1' if reviewer else 'codex-summary-v1',
                     'reviewer': reviewer, 'not_before': not_before})
    retained = []
    for request in requests:
        if not isinstance(request, dict):
            raise ValueError('Invalid retained review request')
        method, selected, after = request.get('method'), review_selection(request.get('reviewer')), request.get('not_before')
        if method != ('github-review-v1' if selected else 'codex-summary-v1'):
            raise ValueError('Unknown prior completion policy; preserve its review obligation')
        if after is not None:
            try:
                if datetime.fromisoformat(after.replace('Z', '+00:00')).tzinfo is None:
                    raise ValueError('Missing timezone')
            except (AttributeError, ValueError) as error:
                raise ValueError('Invalid retained review request boundary') from error
        normalized = {'method': method, 'reviewer': selected, 'not_before': after}
        if normalized not in retained:
            retained.append(normalized)
    return retained


def github_review_complete(pr, reviews, reviewer, not_before=None):
    """Read an explicitly selected native review; completion is not independent approval."""
    if pr.get('requested_reviewers') != [] or pr.get('requested_teams') != []:
        return False
    selected = [r for r in reviews if r.get('user', {}).get('login', '').lower() == reviewer]
    if not selected:
        return False
    # GitHub's endpoint returns chronological order, including dismissed/pending reviews.
    record = selected[-1]
    if (record.get('state') not in ('APPROVED', 'CHANGES_REQUESTED') or
            record.get('commit_id') != pr['head']['sha'] or
            record.get('pull_request_url') != f'https://api.github.com/repos/{REPO}/pulls/{pr["number"]}'):
        return False
    try:
        completed = datetime.fromisoformat(record['submitted_at'].replace('Z', '+00:00'))
        requested = datetime.fromisoformat((not_before or pr['created_at']).replace('Z', '+00:00'))
        return completed.tzinfo is not None and requested.tzinfo is not None and completed >= requested
    except (KeyError, TypeError, ValueError, AttributeError):
        return False


def remote_review_complete(head, comments, not_before=None):
    summaries = [c for c in comments if c['user']['login'] == 'chatgpt-codex-connector[bot]' and
                 c['body'].startswith('<!-- codex-pull-request-review-summary -->')]
    if len(summaries) != 1:
        return False
    # Fail closed if the provider changes its visible summary format/status.
    rows = [line for line in summaries[0]['body'].splitlines() if '**Code Review**' in line]
    if len(rows) != 1:
        return False
    match = re.fullmatch(r'\|[^|]*\*\*Code Review\*\*[^|]*\| ✅ \*\*Completed\*\* '
                         r'<relative-time datetime="([^"]+)">[^<]+</relative-time> '
                         r'\| `([0-9a-f]{7,40})` \|[^|]*\|', rows[0])
    if not match or not head.startswith(match[2]):
        return False
    try:
        completed = datetime.fromisoformat(match[1].replace('Z', '+00:00'))
        requests = [datetime.fromisoformat(c.get('updated_at', c['created_at']).replace('Z', '+00:00'))
                    for c in comments if c['user']['login'] != 'chatgpt-codex-connector[bot]' and
                    re.search(r'^\s*@codex\s+review\b', c['body'], re.M)]
        if not_before:
            requests.append(datetime.fromisoformat(not_before.replace('Z', '+00:00')))
        if completed.tzinfo is None or any(at.tzinfo is None for at in requests):
            return False
    except (KeyError, ValueError):
        return False
    # A newer explicit request may not have reached the provider summary yet.
    return not any(at > completed for at in requests)


def preflight_repair_bundle(loop, pr, source, path, cleanup_roots=None):
    accepted = loop.acceptance(pr, source)
    kind = 'certificate_revision' if accepted.get('certificate_revision') else 'repair_revision'
    if path is None and not accepted.get(kind):
        return None, [], [], None
    if not accepted.get('allowed'):
        raise ValueError('Repair enrollment requires verified current acceptance')
    if path is None:
        raise ValueError('Repair enrollment requires a private regular bundle')
    path = Path(path).absolute()
    if path.is_symlink():
        raise ValueError('Repair enrollment requires a private regular bundle')
    path = path.resolve(strict=True)
    roots = cleanup_roots or (source, loop.storage / f'pr-{pr["number"]}' / 'checkout')
    packet = private_repair_evidence(path, accepted, {'head': pr['head']['sha'], 'base': pr['base']['sha']}, roots, revision_field=kind)
    return path, packet['private_paths'], packet['evidence_records'], kind


def enroll(loop, args):
    pr = loop.gh.pr(args.pr)
    if not eligible(pr, OWNER, REPO) or pr['state'] != 'open':
        raise ValueError('Only maintainer-owned, same-repository open Issue PRs can enroll')
    source = args.source.resolve()
    if f'worktree {source}\n' not in git('worktree', 'list', '--porcelain'):
        raise ValueError('Source must be a linked worktree of the trusted repository')
    if source == ROOT.resolve() or git('branch', '--show-current', cwd=source) != pr['head']['ref']:
        raise ValueError('Handoff requires the dedicated Issue worktree')
    if git('rev-parse', 'HEAD', cwd=source) != pr['head']['sha'] or git('status', '--porcelain', cwd=source):
        raise ValueError('Source must be clean and match remote HEAD')
    issue = loop.gh.issue(issue_number(pr))
    if issue['state'] != 'open' or 'pull_request' in issue:
        raise ValueError('Associated Issue is not open')
    existing, state, state_id, _ = loop.load(args.pr)
    if existing:
        raise ValueError('Already enrolled; use resume for the existing handoff')
    h = {'pr': args.pr, 'issue': issue_number(pr), 'parent': args.parent, 'target': pr['base']['ref'],
         'head': pr['head']['sha'], 'base': pr['base']['sha'], 'branch': pr['head']['ref'],
         'scope': args.scope, 'close_issue': args.close_issue,
         'gui_required': not bool(re.search(r'^GUI: not-required\s*$', pr.get('body') or '', re.M)),
         'previous_owner': args.owner, 'owner': 'agent-loop', 'writer_stopped': True}
    metadata(pr)
    if any(Path(x).is_absolute() or '..' in Path(x).parts for x in args.scope):
        raise ValueError('Use repository-relative enrollment scope')
    repair_path, paths, records, kind = preflight_repair_bundle(loop, pr, source, getattr(args, 'repair_evidence', None))
    h = register(loop.storage, loop.public_state(h, paths), source, HOST, repair_path, paths, records, evidence_kind=kind)
    loop.gh.comment(args.pr, pack(HANDOFF_V2, h))
    loop.gh.status(h['head'], 'pending', 'Enrolled for independent review', pr['html_url'])
    loop.save(pr, {'phase': 'queued', 'expected_head': h['head'], 'fixes': 0, 'reviews': 0, 'errors': 0}, None)
    return h


def rebind_target(loop, args):
    pr = loop.gh.pr(args.pr)
    h, state, sid, _ = loop.load(args.pr)
    if (not h or h.get('host') != HOST or h.get('writer_stopped') is not True or
            not eligible(pr, OWNER, REPO) or pr['state'] != 'open'):
        raise ValueError('No local open managed PR to rebind')
    if h['branch'] != pr['head']['ref'] or h['issue'] != issue_number(pr):
        raise ValueError('Rebinding cannot change branch/Issue ownership')
    if (loop.storage / f'pr-{args.pr}' / 'worker.json').exists():
        raise ValueError('Worker record remains; confirm stop before rebinding')
    source = Path(h['source'])
    if args.source is not None:
        from source_relocation import relocated_source
        source = relocated_source(ROOT, h, args.source, args.migration_record, git)
    elif args.migration_record is not None or not source.exists():
        raise ValueError('Missing source requires explicit --source and migration evidence')
    loop.source_safe(dict(h, source=str(source)))
    path = loop.storage / f'pr-{args.pr}' / 'checkout'
    if path.exists() and (git('rev-parse', 'HEAD', cwd=path) != pr['head']['sha'] or git('status', '--porcelain', cwd=path)):
        raise ValueError('Managed checkout is not clean/current; preserve it')
    if state.get('expected_head', h['head']) != pr['head']['sha']:
        raise ValueError('External HEAD changed; rebind cannot adopt a new writer')
    metadata(pr)
    repair_path = getattr(args, 'repair_evidence', None) or h.get('repair_evidence')
    paths = set(h.get('private_paths', [])) | set(state.get('private_paths', []))
    repair_path, checked_paths, checked_records, kind = preflight_repair_bundle(
        loop, pr, path if path.exists() else source, repair_path, (source, path))
    historical_paths = paths | ({h['repair_evidence']} if h.get('repair_evidence') else set())
    records = retained_originals(h.get('evidence_records', []), cleanup_roots=(source, path))
    for proof_path in getattr(args, 'retained_review', ()):
        records.extend(recover_originals(loop.storage, json.loads(Path(proof_path).read_text())))
    records = retained_originals(records, historical_paths, (source, path))
    paths.update(checked_paths)
    records = retained_originals([*records, *checked_records], paths, (source, path))
    public = {k: v for k, v in h.items()
              if k not in ('source', 'host', 'registry_id', 'version', 'repair_evidence', 'evidence_kind', 'private_paths', 'evidence_records')}
    public = loop.public_state(public, paths)
    public['owner'] = 'agent-loop'
    public['target'] = pr['base']['ref']
    loop.invalidate_acceptance(state)
    state['private_paths'] = sorted(paths)
    state['next'] = 'Target/evidence explicitly rebound; independent review and acceptance required'
    if not state.get('paused'):
        state['phase'] = 'queued'
    loop.save(pr, state, sid)
    public = register(loop.storage, public, source, HOST, repair_path, sorted(paths), records, evidence_kind=kind)
    loop.gh.comment(args.pr, pack(HANDOFF_V2, public))
    loop.gh.status(pr['head']['sha'], 'pending', 'Target rebound; prior acceptance invalidated', pr['html_url'])
    return public


def audit_branches(gh, apply=False):
    """Audit without fetch/prune; clean only verified merged local refs."""
    prs = gh.pages(f'repos/{REPO}/pulls?state=closed&per_page=100')
    merged = {pr['head']['ref']: pr for pr in prs if pr.get('merged_at') and eligible(pr, OWNER, REPO)}
    unmerged = {}
    for pr in prs:
        if not pr.get('merged_at') and eligible(pr, OWNER, REPO):
            unmerged.setdefault(pr['head']['ref'], []).append(pr)
    checked = set(re.findall(r'^branch refs/heads/(.+)$', git('worktree', 'list', '--porcelain'), re.M))
    results = []
    for line in git('for-each-ref', '--format=%(refname) %(objectname)', 'refs/remotes/origin/').splitlines():
        ref, sha = line.split()
        branch = ref.removeprefix('refs/remotes/origin/')
        if branch in unmerged:
            for pr in unmerged[branch]:
                results.append({'branch': branch, 'ref': ref, 'head': sha,
                                'state': 'retained-closed-unmerged', 'pr': pr['number'],
                                'pr_head': pr['head']['sha'], 'head_matches': sha == pr['head']['sha']})
    for line in git('for-each-ref', '--format=%(refname:short) %(objectname)', 'refs/heads/').splitlines():
        branch, sha = line.split()
        if branch in ('main', 'master', 'develop'):
            continue
        if branch in unmerged:
            for pr in unmerged[branch]:
                results.append({'branch': branch, 'ref': 'refs/heads/' + branch, 'head': sha,
                                'state': 'retained-closed-unmerged', 'pr': pr['number'],
                                'pr_head': pr['head']['sha'], 'head_matches': sha == pr['head']['sha']})
            continue
        pr = merged.get(branch)
        if not pr or sha != pr['head']['sha']:
            continue
        if branch in checked:
            results.append({'branch': branch, 'state': 'retained-active-worktree'})
            continue
        remote = git('ls-remote', 'origin', 'refs/heads/' + branch)
        if remote:
            results.append({'branch': branch, 'state': 'retained-remote-ref'})
            continue
        if apply:
            git('update-ref', '-d', 'refs/heads/' + branch, sha)
        results.append({'branch': branch, 'state': 'deleted' if apply else 'candidate', 'pr': pr['number']})
    reported = {row.get('pr') for row in results}
    for branch, candidates in unmerged.items():
        for pr in candidates:
            if pr['number'] not in reported:
                results.append({'branch': branch, 'ref': None, 'head': None,
                                'state': 'retained-closed-unmerged', 'pr': pr['number'],
                                'pr_head': pr['head']['sha'], 'head_matches': False})
    return results


def main():
    global ROOT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo-root', type=Path, default=ROOT, help='Trusted main checkout (bootstrap/test override)')
    parser.add_argument('--github-reviewer', help='Explicit authorized native review source; default remains Codex')
    sub = parser.add_subparsers(dest='action', required=True)
    tick = sub.add_parser('tick'); tick.add_argument('--pr', type=int)
    sub.add_parser('scan')
    audit = sub.add_parser('cleanup-branches'); audit.add_argument('--apply', action='store_true')
    feedback = sub.add_parser('feedback', help='Read-only fixed feedback snapshot; no enrollment or worker')
    feedback.add_argument('--pr', type=int, required=True)
    en = sub.add_parser('enroll'); en.add_argument('--pr', type=int, required=True)
    en.add_argument('--source', type=Path, required=True); en.add_argument('--owner', required=True)
    en.add_argument('--scope', nargs='+', required=True); en.add_argument('--parent', type=int, help='Actual parent Issue; omit for Standalone')
    en.add_argument('--repair-evidence', type=Path, help='Private sealed repair origin bundle; never published')
    en.add_argument('--close-issue', action='store_true')
    en.add_argument('--writer-stopped', action='store_true', required=True)
    rebound = sub.add_parser('rebind-target'); rebound.add_argument('--pr', type=int, required=True)
    rebound.add_argument('--writer-stopped', action='store_true', required=True)
    rebound.add_argument('--repair-evidence', type=Path, help='Preflight and replace/add private bundle; preserves pause')
    rebound.add_argument('--retained-review', type=Path, action='append', default=[],
                         help='Explicit legacy identity recovery from private {binding, review} adapter-issued proof')
    rebound.add_argument('--source', type=Path, help='Explicit relocated original linked worktree')
    rebound.add_argument('--migration-record', type=Path, help='Private verified directory migration JSON')
    resume = sub.add_parser('resume'); resume.add_argument('--pr', type=int, required=True)
    resume.add_argument('--reason', required=True)
    recovery = sub.add_parser('reconcile-feedback', help='Bind a fresh independent disposition for an unchanged merged PR')
    recovery.add_argument('--pr', type=int, required=True)
    recovery.add_argument('--review', type=Path, required=True)
    gui = sub.add_parser('gui'); gui.add_argument('--pr', type=int, required=True); gui.add_argument('--evidence', type=Path, required=True); gui.add_argument('--lease-token', required=True)
    args = parser.parse_args()
    ROOT = args.repo_root.resolve()
    try:
        reviewer = review_selection(args.github_reviewer)
        if args.action == 'feedback':
            gh = GitHub()
            pr = gh.pr(args.pr)
            # No registry, status, comment, worker, or coordinator lock mutation.
            loop = object.__new__(Loop)
            loop.gh = gh
            loop.github_reviewer = reviewer
            bound, records = loop.bound(pr, gh.issue(issue_number(pr)), gh.comments(args.pr))
            print(json.dumps({'pr': args.pr, 'binding': bound, 'feedback': records}, ensure_ascii=False))
            return 0
        with coordinator_lock():
            loop = Loop(github_reviewer=reviewer)
            if args.action == 'cleanup-branches':
                result = audit_branches(loop.gh, args.apply)
            elif args.action == 'rebind-target':
                result = rebind_target(loop, args)
            elif args.action == 'enroll':
                result = enroll(loop, args)
            elif args.action == 'reconcile-feedback':
                result = loop.reconcile_feedback(loop.gh.pr(args.pr), json.loads(args.review.read_text()))
            elif args.action in ('resume', 'gui'):
                pr = loop.gh.pr(args.pr); h, state, sid, comments = loop.load(args.pr)
                if not h or h['host'] != HOST:
                    raise ValueError('No local managed handoff')
                if args.action == 'gui':
                    evidence = json.loads(args.evidence.read_text())
                    from gui_lease import locked, transition
                    with locked(Path('/tmp') / f'cursor-in-android-studio-gui-{os.getuid()}') as lease:
                        current = transition(lease, 'status')
                        if (current.get('token') != args.lease_token or current.get('expired') or
                                current.get('issue') != h['issue'] or current.get('head') != pr['head']['sha'] or
                                current.get('owner') != evidence.get('observer')):
                            raise ValueError('GUI evidence requires the current designated operator lease')
                    bound, _ = loop.bound(pr, loop.gh.issue(h['issue']), comments, state.get('remote_review_after'))
                    if not gui_pass(evidence, bound):
                        raise ValueError('GUI evidence is incomplete or belongs to another HEAD/base')
                    state['gui'] = evidence
                    state['phase'] = 'reviewed'
                else:
                    state.update(paused=False, errors=0, retry_at=0, phase=state.get('interrupted_phase', 'queued'),
                                 next=args.reason)
                    # Explicit human/coordinator decision permits another bounded attempt window.
                    state.update(reviews=0, fixes=0)
                    # A blocked verdict may depend on evidence added since the review.
                    # Keep that evidence, but require an independent review on resume.
                    if state.get('review', {}).get('verdict') == 'blocked':
                        state.pop('review', None)
                        state.pop('binding', None)
                loop.save(pr, state, sid); result = loop.public_result(state)
            else:
                prs = loop.gh.pages(f'repos/{REPO}/pulls?state=all&per_page=100')
                targets = []
                attempt_file = loop.storage / 'attempts.json'
                attempts = json.loads(attempt_file.read_text()) if attempt_file.exists() else {}
                skipped = []
                for pr in prs:
                    if eligible(pr, OWNER, REPO):
                        try:
                            h, state, _, _ = loop.load(pr['number'])
                        except (ValueError, KeyError, IndexError) as error:
                            skipped.append({'pr': pr['number'], 'error': str(error)})
                            continue
                        if h and h.get('host') == HOST and state.get('phase') != 'done':
                            targets.append(pr['number'])
                if args.action == 'scan':
                    result = {'managed_prs': targets, 'invalid_records': skipped}
                else:
                    selected = [args.pr] if args.pr else sorted(targets, key=lambda n: attempts.get(str(n), 0))[:3]
                    result = []
                    for n in selected:
                        attempts[str(n)] = time.time()
                        temporary = attempt_file.with_suffix('.tmp')
                        temporary.write_text(json.dumps(attempts)); os.replace(temporary, attempt_file)
                        try:
                            result.append(loop.tick(n))
                        except Exception as error:
                            result.append({'pr': n, 'phase': 'error', 'error': str(error)})
                    result.extend(skipped)
            print(json.dumps(loop.public_result(result), ensure_ascii=False, indent=2))
            return 0
    except BlockingIOError:
        print('{"phase":"coordinator-busy"}'); return 0
    except Exception as error:
        print(str(error), file=sys.stderr); return 1


if __name__ == '__main__':
    sys.exit(main())
