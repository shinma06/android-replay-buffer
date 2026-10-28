"""Pure decisions shared by the autonomous coordinator and its tests."""
import fnmatch
import hashlib
import json
import re
import uuid
from datetime import datetime, timezone
from git_guard import BRANCH
from issue_schema import labels

CONTEXT = 'Agent review'
SHA = re.compile(r'[0-9a-f]{40}')
SESSION_HASH = re.compile(r'(?:codex|native)-session-sha256:[0-9a-f]{64}')
NATIVE_SESSION = re.compile(r'native-session-v1:([0-9a-f]+):([0-9a-f]+)')
REVIEW_ORIGINAL_REF = re.compile(r'[0-9a-f]{32}:[0-9a-f]{64}')


def native_session(issuer, identity):
    """Encode a session identity supplied by the trusted host.

    Issuer/instance and opaque ID must come from that host, never a role/model label.
    """
    if (not isinstance(issuer, str) or not re.fullmatch(r'[a-z0-9][a-z0-9./_-]*', issuer) or
            'codex' in issuer or not isinstance(identity, str) or not identity or
            any(ord(c) < 32 or ord(c) == 127 for c in identity) or SESSION_HASH.fullmatch(identity) or
            identity.startswith('native-session-')):
        raise ValueError('Invalid native issuer/session identity')
    # UUID identities retain their single historical identity, never a namespace alias.
    try:
        uuid.UUID(identity)
    except ValueError:
        pass
    else:
        return canonical_session(identity)
    return f'native-session-v1:{issuer.encode().hex()}:{identity.encode().hex()}'


def canonical_session(session):
    match = NATIVE_SESSION.fullmatch(session) if isinstance(session, str) else None
    if match:
        try:
            issuer, identity = (bytes.fromhex(part).decode() for part in match.groups())
            if native_session(issuer, identity) == session:
                return session
        except (ValueError, UnicodeError):
            pass
        raise ValueError('Noncanonical native session identity')
    try:
        value = uuid.UUID(session) if isinstance(session, str) else None
    except ValueError:
        value = None
    if value is None or str(value) != session or value.variant != uuid.RFC_4122:
        raise ValueError('Canonical originating session is missing or malformed')
    return session


def codex_session(session):
    session = canonical_session(session)
    if session.startswith('native-session-v1:'):
        raise ValueError('Codex source requires its canonical UUID session')
    return session


def session_fingerprint(session):
    session = canonical_session(session)
    if session.startswith('native-session-v1:'):
        return 'native-session-sha256:' + hashlib.sha256(session.encode()).hexdigest()
    return 'codex-session-sha256:' + hashlib.sha256(('codex-session-v1:' + session).encode()).hexdigest()


def issue_number(pr):
    match = BRANCH.fullmatch(pr['head']['ref'])
    issues = re.findall(r'^Issue: #([1-9][0-9]*)\s*$', pr.get('body') or '', re.M)
    if not match or issues != [match.group(2)]:
        raise ValueError('PR branch and Issue metadata do not match')
    return int(issues[0])


def eligible(pr, owner, repo):
    try:
        issue_number(pr)
        return (pr['user']['login'] == owner and pr['base']['ref'] in ('main', 'develop') and
                pr['base']['repo']['full_name'] == repo and pr['head']['repo'] is not None and
                pr['head']['repo']['full_name'] == repo)
    except (ValueError, KeyError, TypeError):
        return False


def binding(pr, issue):
    return {'head': pr['head']['sha'], 'base': pr['base']['sha'], 'target': pr['base']['ref'],
            'title_hash': hashlib.sha256((pr.get('title') or '').encode()).hexdigest(),
            'body_hash': hashlib.sha256((pr.get('body') or '').encode()).hexdigest(),
            'issue_hash': hashlib.sha256((issue.get('body') or '').encode()).hexdigest(),
            'issue_bug': 'type:bug' in labels(issue)}


def validate_review(report, expected, acceptance=None, storage=None):
    for key in ('head', 'base'):
        if report.get(key) != expected[key]:
            raise ValueError('Stale reviewer ' + key)
    if report.get('verdict') not in ('approved', 'changes_requested', 'blocked'):
        raise ValueError('Invalid review verdict')
    for key in ('scope_complete', 'issue_complete', 'gui_required'):
        if type(report.get(key)) is not bool:
            raise ValueError('Missing review decision: ' + key)
    if not isinstance(report.get('findings'), list) or not all(isinstance(x, str) for x in report['findings']):
        raise ValueError('Invalid findings')
    if not report.get('session') or not report.get('evidence'):
        raise ValueError('Review session/evidence missing')
    if report['verdict'] == 'approved' and (report['findings'] or not report['scope_complete']):
        raise ValueError('Approval conflicts with incomplete scope or unresolved findings')
    if (report['verdict'] == 'approved' and expected.get('remote_review_complete') is False and
            'review_retry' in expected):
        if not remote_review_ready(expected) or report.get('feedback_hash') != expected.get('feedback_hash'):
            raise ValueError('Pending external review requires the elapsed retry and exact feedback disposition')
        from handoff_registry import verify_review_origin
        verify_review_origin(storage, report, expected)
    if report['verdict'] == 'approved' and (acceptance or {}).get('repair_revision'):
        reviewer = session_fingerprint(report['session'])
        observers = acceptance.get('repair_observers')
        if (report.get('repair_revision') != acceptance['repair_revision'] or
                not isinstance(acceptance.get('repair_evidence_sha256'), str) or
                not re.fullmatch(r'[0-9a-f]{64}', acceptance.get('repair_evidence_sha256', '')) or
                report.get('repair_evidence_sha256') != acceptance['repair_evidence_sha256'] or
                not isinstance(report.get('repair_assessment'), str) or not report['repair_assessment'].strip() or
                not isinstance(observers, list) or not observers or
                not all(isinstance(x, str) and SESSION_HASH.fullmatch(x) for x in observers) or
                reviewer in observers):
            raise ValueError('Independent original-NG evidence assessment is missing, stale or self-reviewed')
        from handoff_registry import verify_review_origin
        verify_review_origin(storage, report, expected)
    if report['verdict'] == 'approved' and (acceptance or {}).get('certificate_revision'):
        reviewer = session_fingerprint(report['session'])
        observers = acceptance.get('certificate_observers')
        if (report.get('certificate_revision') != acceptance['certificate_revision'] or
                acceptance.get('certificate_reviewers') != [reviewer] or
                not isinstance(observers, list) or not observers or reviewer in observers or
                not all(isinstance(s, str) and SESSION_HASH.fullmatch(s) for s in observers) or
                not isinstance(acceptance.get('certificate_evidence_sha256'), str) or
                not re.fullmatch(r'[0-9a-f]{64}', acceptance['certificate_evidence_sha256']) or
                report.get('certificate_evidence_sha256') != acceptance['certificate_evidence_sha256'] or
                not isinstance(report.get('certificate_assessment'), str) or not report['certificate_assessment'].strip()):
            raise ValueError('Certificate registration lacks its actual independent session/original evidence assessment')
        from handoff_registry import verify_review_origin
        verify_review_origin(storage, report, expected)
    if report['verdict'] == 'changes_requested' and not report['findings']:
        raise ValueError('Changes requested without concrete findings')
    from handoff_registry import review_privacy
    review_privacy(storage, report, expected)


def in_scope(paths, scope):
    return all(any(p == s or (s.endswith('/') and p.startswith(s)) or
                   ('*' in s and fnmatch.fnmatchcase(p, s)) for s in scope) for p in paths)


def gui_pass(evidence, bound):
    return bool(evidence and evidence.get('processes_stopped') is True and evidence.get('head') == bound['head'] and evidence.get('base') == bound['base'] and
                re.fullmatch(r'[0-9a-f]{64}', evidence.get('artifact_sha256', '')) and
                all(evidence.get(k) for k in ('run', 'observer', 'loaded_identity', 'evidence_url')) and
                evidence.get('cases') and all(x.get('status') == 'pass' and x.get('id') and x.get('observation')
                                              for x in evidence['cases']))


def remote_review_ready(bound, now=None):
    """Allow the bounded wait exception without changing provider completion."""
    if bound.get('remote_review_complete') is not False:
        return True
    retry = bound.get('review_retry')
    required = {'head', 'base', 'comment_id', 'created_at'}
    if (not isinstance(retry, dict) or set(retry) not in (required, required | {'original_ref'}) or
            type(retry['comment_id']) is not int or retry['comment_id'] <= 0 or
            ('original_ref' in retry and not REVIEW_ORIGINAL_REF.fullmatch(str(retry['original_ref']))) or
            any(retry.get(k) != bound.get(k) for k in ('head', 'base'))):
        return False
    try:
        started = datetime.fromisoformat(retry['created_at'].replace('Z', '+00:00'))
        return started.tzinfo is not None and ((now or datetime.now(timezone.utc)) - started).total_seconds() >= 1800
    except (KeyError, TypeError, ValueError, AttributeError):
        return False


def next_action(state, bound, ci, gui, acceptance=None, storage=None):
    if not remote_review_ready(bound):
        return 'remote-review-wait'
    if state.get('binding') != bound:
        return 'review'
    report = state.get('review')
    if not report:
        return 'review'
    validate_review(report, bound, acceptance if acceptance and acceptance.get('allowed') else None, storage)
    if report['verdict'] == 'blocked':
        return 'blocked'
    if report['verdict'] == 'changes_requested':
        return 'fix' if state.get('fixes', 0) < 3 else 'blocked'
    if acceptance is not None and not acceptance.get('allowed'):
        return 'acceptance-wait'
    if acceptance is None and gui and not gui_pass(state.get('gui'), bound):
        return 'gui-queued'
    if ci == 'failure':
        return 'ci-failed'
    if ci != 'success':
        return 'ci-wait'
    return 'merge'


def update_parent(body, issue):
    """Only check a line dedicated to this Issue; never check aggregate lines."""
    lines = body.splitlines(keepends=True)
    for i, line in enumerate(lines):
        if re.match(rf'^\s*- \[ \] #{issue}(?!\d)(?:\s|:|：|—|$)', line) and len(re.findall(r'#\d+', line)) == 1:
            lines[i] = line.replace('[ ]', '[x]', 1)
    return ''.join(lines)
