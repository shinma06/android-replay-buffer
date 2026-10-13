"""Issue naming and the three exclusive label axes."""
import re

TYPES = {'feature': 'feat', 'bug': 'fix', 'research': 'research', 'qa': 'test', 'maintenance': 'chore', 'tracking': 'track'}
LEGACY_TYPES = {'feature': '機能', 'bug': '修正', 'research': '調査', 'qa': '試験', 'maintenance': '運用', 'tracking': '追跡'}
AXES = {'type': set(TYPES), 'priority': {'P0', 'P1', 'P2'},
        'status': {'ready', 'in-progress', 'review', 'blocked', 'deferred', 'done'}}


def labels(issue):
    return [x['name'] if isinstance(x, dict) else x for x in issue.get('labels', [])]


def validate_issue(issue, *, allow_legacy=True):
    if 'pull_request' in issue or issue.get('state') not in ('open', 'closed'):
        raise ValueError('Expected an Issue with a valid state')
    selected = {}
    for axis, allowed in AXES.items():
        values = [x.split(':', 1)[1] for x in labels(issue) if x.startswith(axis + ':')]
        if len(values) != 1 or values[0] not in allowed:
            raise ValueError('Exactly one supported ' + axis + ' label is required')
        selected[axis] = values[0]
    kind = selected['type']
    origin = r'#[1-9][0-9]* ' if kind == 'qa' else ''
    title = issue.get('title', '')
    if not isinstance(title, str) or re.search(r'\bP[012]\b', title):
        raise ValueError('Issue title must omit priority')
    canonical = re.fullmatch(TYPES[kind] + ': ' + origin + r'(\S(?:[^\r\n]*\S)?)', title)
    if canonical:
        if (not re.search(r'[ぁ-んァ-ヶ一-龯]', canonical[1]) or
                re.search(r'\(#[1-9][0-9]*\)', canonical[1]) or
                str(issue.get('number')) in re.findall(r'#([1-9][0-9]*)', title)):
            raise ValueError('Issue needs a Japanese summary without its own number or PR suffix')
    elif not allow_legacy or not re.fullmatch(r'\[' + LEGACY_TYPES[kind] + r'\] ' + origin + r'\S.*', title):
        raise ValueError('Issue title must match its type; QA needs #origin. Legacy titles are read-only compatibility')
    if (issue['state'] == 'closed') != (selected['status'] == 'done'):
        raise ValueError('Closed Issues require status:done; open Issues must not be done')
    return selected


def done_labels(issue):
    return [x for x in labels(issue) if not x.startswith('status:')] + ['status:done']
