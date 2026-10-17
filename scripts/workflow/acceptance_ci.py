#!/usr/bin/env python3
"""Execute only from the trusted base checkout. PR content is git-show JSON data."""
from http.client import IncompleteRead
import json
import os
import re
import subprocess
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
from pr_policy import validate
from verification import SHA, certificate_changes, git_read, verify_pr
from agent_policy import CONTEXT


def main():
    with open(os.environ['GITHUB_EVENT_PATH']) as stream:
        event = json.load(stream)
    repo = os.environ['GITHUB_REPOSITORY']
    def api(path, method='GET', data=None):
        request = Request(f'https://api.github.com/repos/{repo}/{path}', headers={
            'Authorization': 'Bearer ' + os.environ['GITHUB_TOKEN'],
            'Accept': 'application/vnd.github+json', 'Content-Type': 'application/json'},
            method=method, data=json.dumps(data).encode() if data is not None else None)
        with urlopen(request, timeout=30) as response:
            body = response.read()
            return json.loads(body) if body else None
    if os.environ.get('GITHUB_EVENT_NAME') == 'push':
        if event.get('ref') != 'refs/heads/main' or event.get('deleted'):
            raise ValueError('Contract reconciliation requires a main push')
        result = {'recheck_requested': recheck_main_repairs(api, repo)}
    elif os.environ.get('GITHUB_EVENT_NAME') in ('issues', 'issue_comment'):
        source = repair_source_issue(event, os.environ['GITHUB_EVENT_NAME'])
        result = {'recheck_requested': recheck_main_repairs(api, repo, source_issue=source) if source else []}
    else:
        result = check_pr(event['pull_request'], api)
    print(json.dumps(result, ensure_ascii=False))


def repair_source_issue(event, name):
    """Only repair declaration/record or bug classification changes invalidate the Issue."""
    issue = event['issue']
    if 'pull_request' in issue:
        return None
    if name == 'issues' and event['action'] in ('labeled', 'unlabeled'):
        return issue['number'] if event['label']['name'] == 'type:bug' else None
    if event['action'] not in ('edited', 'deleted'):
        return None
    body = issue.get('body') or ''
    old = event.get('changes', {}).get('body', {}).get('from') or ''
    if name == 'issues':
        if (event['action'] == 'deleted' or re.findall(r'^Acceptance repair: (.*)$', body, re.M) !=
                re.findall(r'^Acceptance repair: (.*)$', old, re.M)):
            return issue['number']
    else:
        comment = event['comment']
        # Prior declarations are invalidated by Issue edits; changes.body here is the comment.
        if str(comment['id']) in re.findall(r'^Acceptance repair: (.*)$', body, re.M):
            return issue['number']
    return None


def recheck_main_repairs(api, repo, source_issue=None):
    """Invalidate all affected PRs before optional reruns can fail or be denied."""
    pending, requested, unavailable, errors, page = [], [], [], [], 1
    while True:
        enumeration_failed = False
        for attempt in range(3):
            try:
                prs = api(f'pulls?state=open&base=develop&per_page=100&page={page}')
            except (OSError, IncompleteRead, json.JSONDecodeError, UnicodeDecodeError) as failure:
                transient = (500 <= failure.code < 600 if isinstance(failure, HTTPError) else
                             isinstance(failure, (URLError, TimeoutError, ConnectionError, IncompleteRead)))
                if transient and attempt < 2:
                    continue
                error = failure
            else:
                if isinstance(prs, list):
                    break
                error = ValueError('PR enumeration response must be a list')
            error.add_note(f'PR enumeration incomplete at page {page} after {attempt + 1} attempts; '
                           'recover known pending targets, then retry enumeration and live gates before integration')
            errors.append(error)
            enumeration_failed = True
            break
        if enumeration_failed:
            break
        for pr in prs:
            try:
                # A title-policy failure must not hide stale original-NG approval.
                issue_id = validate(pr, check_title=False)
                if (pr['state'] != 'open' or pr.get('merged') is True or pr['base']['ref'] != 'develop' or
                        pr['base']['repo']['full_name'] != repo or pr['head']['repo'] is None or
                        pr['head']['repo']['full_name'] != repo):
                    continue
            except (ValueError, KeyError, TypeError):
                continue
            if source_issue is not None and issue_id != source_issue:
                continue
            try:
                if source_issue is None:
                    try:
                        issue = api(f'issues/{issue_id}')
                        if (not isinstance(issue, dict) or 'body' not in issue or
                                issue['body'] is not None and not isinstance(issue['body'], str)):
                            raise ValueError('Issue response requires a body string or null')
                        declaration = re.findall(r'^Acceptance repair: (.*)$', issue['body'] or '', re.M)
                        labels = issue.get('labels')
                        if not declaration and (not isinstance(labels, list) or any(
                                not isinstance(label, dict) or not isinstance(label.get('name'), str)
                                for label in labels)):
                            raise ValueError('Issue response requires labels to establish repair classification')
                        if declaration == ['none'] or (not declaration and not any(
                                label['name'] == 'type:bug' for label in labels)):
                            ref = api('git/ref/heads/develop')
                            obj = ref.get('object') if isinstance(ref, dict) else None
                            base = obj.get('sha') if isinstance(obj, dict) else None
                            if not isinstance(base, str) or not SHA.fullmatch(base):
                                raise ValueError('Develop ref response requires a full commit SHA')
                            head = pr['head']['sha']
                            subprocess.run(['git', 'fetch', '--no-tags', 'origin', base, head], check=True)
                            files = git_read('diff', '--no-renames', '--name-only', base, head).splitlines()
                            if not any(certificate_changes(base, head, files, git_read)):
                                continue
                    except (OSError, IncompleteRead, ValueError, subprocess.CalledProcessError) as error:
                        error.add_note(f'PR {pr["number"]}: acceptance dependency unknown; preserve pending review')
                        errors.append(error)
                api('statuses/' + pr['head']['sha'], 'POST', {'context': CONTEXT, 'state': 'pending',
                    'description': 'Acceptance source or trusted main changed; recheck acceptance and independent assessment'})
                pending.append(pr)
            except (OSError, IncompleteRead, json.JSONDecodeError, UnicodeDecodeError) as error:
                error.add_note(f'PR {pr["number"]}: source lookup or review invalidation failed')
                errors.append(error)
        if len(prs) < 100:
            break
        page += 1
    for pr in pending:
        try:
            runs = api('actions/workflows/acceptance.yml/runs?event=pull_request&head_sha=' +
                       pr['head']['sha'] + '&per_page=1')['workflow_runs']
            if (not runs or runs[0]['head_sha'] != pr['head']['sha'] or runs[0]['event'] != 'pull_request' or
                    not any(p['number'] == pr['number'] for p in runs[0]['pull_requests']) or
                    runs[0]['status'] != 'completed'):
                unavailable.append(pr['number'])
                continue
            api(f'actions/runs/{runs[0]["id"]}/rerun', 'POST')
            requested.append(pr['number'])
        except (OSError, IncompleteRead, json.JSONDecodeError, UnicodeDecodeError) as error:
            error.add_note(f'PR {pr["number"]}: Acceptance lookup or rerun failed; review remains pending')
            errors.append(error)
    if errors:
        for error in errors[1:]:
            errors[0].add_note(str(error) + ': ' + '; '.join(error.__notes__))
        if unavailable:
            errors[0].add_note(f'Acceptance run unavailable or still running for PRs {unavailable}')
        raise errors[0]
    if unavailable:
        raise ValueError(f'Current Acceptance run unavailable or still running for PRs {unavailable}; preserve pending review and retry after completion')
    return requested


def closed_result(pr):
    """A live closed PR needs no decision; never label this as acceptance."""
    state = pr.get('state')
    if state not in ('open', 'closed') or (state == 'open' and pr.get('merged') is True):
        raise ValueError('Live PR state is missing or inconsistent')
    if state == 'closed':
        return {'status': 'skipped', 'reason': 'live-pr-closed', 'pr': pr['number']}
    return None


def check_pr(pr, api):
    endpoint = f'pulls/{pr["number"]}'
    current = api(endpoint)
    skipped = closed_result(current)
    if skipped:
        return skipped
    try:
        result, trusted_head = check_open_pr(pr, current, api)
    except (ValueError, subprocess.CalledProcessError):
        # The PR may have merged while refs/diffs were being validated. A failure
        # remains a failure for an open/reopened PR, including API lookup errors.
        skipped = closed_result(api(endpoint))
        if skipped:
            return skipped
        raise
    latest = api(endpoint)
    skipped = closed_result(latest)
    if skipped:
        return skipped
    latest_base = api('git/ref/heads/' + current['base']['ref'])['object']['sha']
    if (latest['head']['sha'] != current['head']['sha'] or latest['base']['ref'] != current['base']['ref']
            or latest_base != trusted_head or latest.get('body') != current.get('body')
            or latest.get('title') != current.get('title')):
        raise ValueError('PR/base changed during acceptance validation; rerun')
    for field in ('repair_contract_base', 'certificate_sync_main'):
        if result.get(field) and api('git/ref/heads/main')['object']['sha'] != result[field]:
            raise ValueError('Trusted main contract changed during acceptance validation; rerun')
    return result


def check_open_pr(pr, current, api):
    if current['head']['sha'] != pr['head']['sha'] or current['base']['ref'] != pr['base']['ref']:
        raise ValueError('PR changed after this workflow was queued')
    validate(current)
    current['base']['sha'] = api('git/ref/heads/' + current['base']['ref'])['object']['sha']
    trusted_head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if trusted_head != current['base']['sha']:
        raise ValueError('Trusted validator checkout is stale; rerun with the current base branch')
    subprocess.run(['git', 'fetch', '--no-tags', 'origin', current['base']['sha'], current['head']['sha']], check=True)
    if current['base']['ref'] == 'main':
        subprocess.run(['git', 'fetch', '--no-tags', 'origin', 'main'], check=True)
        if 'Integration: promotion' in current['body']:
            subprocess.run(['git', 'fetch', '--no-tags', 'origin', 'develop'], check=True)
    result = verify_pr(current, api)
    return result, trusted_head


if __name__ == '__main__':
    main()
