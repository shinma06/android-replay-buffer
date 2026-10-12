#!/usr/bin/env python3
"""Execute only from the trusted base checkout. PR content is git-show JSON data."""
import json
import os
import subprocess
from urllib.request import Request, urlopen
from pr_policy import validate
from verification import verify_pr
from agent_policy import CONTEXT, eligible, issue_number


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
    else:
        result = check_pr(event['pull_request'], api)
    print(json.dumps(result, ensure_ascii=False))


def recheck_main_repairs(api, repo):
    """A bounded main-push event invalidates review and retries existing PR checks."""
    requested, unavailable, page = [], [], 1
    while True:
        prs = api(f'pulls?state=open&base=develop&per_page=100&page={page}')
        for pr in prs:
            if not eligible(pr, repo.split('/')[0], repo):
                continue
            issue = api(f'issues/{issue_number(pr)}')
            declaration = [line.removeprefix('Acceptance repair: ') for line in
                           (issue.get('body') or '').splitlines() if line.startswith('Acceptance repair: ')]
            if declaration == ['none'] or (not declaration and not any(
                    label.get('name') == 'type:bug' for label in issue.get('labels', []))):
                continue
            api('statuses/' + pr['head']['sha'], 'POST', {'context': CONTEXT, 'state': 'pending',
                'description': 'Trusted main advanced; recheck original-NG acceptance and independent assessment'})
            runs = api('actions/workflows/acceptance.yml/runs?event=pull_request&head_sha=' +
                       pr['head']['sha'] + '&per_page=1')['workflow_runs']
            if (not runs or runs[0]['head_sha'] != pr['head']['sha'] or runs[0]['event'] != 'pull_request' or
                    not any(p['number'] == pr['number'] for p in runs[0]['pull_requests']) or
                    runs[0]['status'] != 'completed'):
                unavailable.append(pr['number'])
                continue
            api(f'actions/runs/{runs[0]["id"]}/rerun', 'POST')
            requested.append(pr['number'])
        if len(prs) < 100:
            if unavailable:
                raise ValueError(f'Current Acceptance run unavailable or still running for PRs {unavailable}; preserve pending review and retry after completion')
            return requested
        page += 1


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
    if (latest['head']['sha'] != current['head']['sha'] or latest['base']['ref'] != current['base']['ref'] or latest_base != trusted_head or latest.get('body') != current.get('body')):
        raise ValueError('PR/base changed during acceptance validation; rerun')
    if result.get('repair_contract_base') and api('git/ref/heads/main')['object']['sha'] != result['repair_contract_base']:
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
