"""Public opaque handoffs resolve only through the enrolling repository's private registry."""
import hashlib
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import uuid

REVIEW_PRIVACY_FIELDS = ('private_sessions', 'private_identifiers', 'private_paths')


@contextmanager
def registry_directory(storage, create=False):
    """Open private directories without following links or changing existing modes."""
    storage = Path(storage)
    if create:
        storage.mkdir(mode=0o700, parents=True, exist_ok=True)
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    parent = os.open(storage, flags)
    try:
        info = os.fstat(parent)
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError('Storage must be private and locally owned')
        if create:
            try:
                os.mkdir('registry', mode=0o700, dir_fd=parent)
            except FileExistsError:
                pass
        directory = os.open('registry', flags, dir_fd=parent)
        try:
            info = os.fstat(directory)
            if info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise ValueError('Registry directory must be private and locally owned')
            yield directory
        finally:
            os.close(directory)
    finally:
        os.close(parent)


def digest(public):
    return hashlib.sha256(json.dumps(public, sort_keys=True).encode()).hexdigest()


def register(storage, public, source, host, repair_evidence=None, private_paths=(), evidence_records=(), evidence_kind=None):
    if evidence_kind not in (None, 'repair_revision', 'certificate_revision'):
        raise ValueError('Unknown private evidence bundle kind')
    public = dict(public, registry_id=uuid.uuid4().hex, version=2)
    with registry_directory(storage, create=True) as directory:
        fd = os.open(public['registry_id'] + '.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory)
        with os.fdopen(fd, 'w') as stream:
            json.dump({'digest': digest(public), 'source': str(source), 'host': host,
                       'repair_evidence': str(repair_evidence) if repair_evidence else None,
                       'evidence_kind': evidence_kind or 'repair_revision',
                       'private_paths': list(private_paths), 'evidence_records': list(evidence_records)}, stream)
    return public


def resolve(storage, public, host):
    if public.get('version') != 2 or not re.fullmatch(r'[0-9a-f]{32}', public.get('registry_id', '')):
        raise ValueError('Invalid opaque enrollment')
    try:
        with registry_directory(storage) as directory:
            fd = os.open(public['registry_id'] + '.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
            with os.fdopen(fd) as stream:
                stat = os.fstat(stream.fileno())
                if stat.st_uid != os.getuid() or stat.st_mode & 0o077:
                    raise ValueError('Registry must be private and locally owned')
                local = json.load(stream)
    except (OSError, ValueError) as error:
        raise ValueError('Local enrollment registry unavailable or invalid; explicit owner recovery required') from error
    if (not isinstance(local, dict) or local.get('digest') != digest(public) or local.get('host') != host or
            not isinstance(local.get('source'), str) or not Path(local['source']).is_absolute()):
        raise ValueError('Enrollment binding/local ownership mismatch; preserve existing owner')
    return dict(public, source=local['source'], host=local['host'], repair_evidence=local.get('repair_evidence'),
                evidence_kind=local.get('evidence_kind', 'repair_revision'), private_paths=local.get('private_paths', []), evidence_records=local.get('evidence_records', []))


def store_review(storage, public, report, private_paths=()):
    identifier = uuid.uuid4().hex
    with registry_directory(storage, create=True) as directory:
        fd = os.open(identifier + '.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory)
        with os.fdopen(fd, 'w') as stream:
            json.dump({'digest': digest(public), 'review': report, 'private_paths': list(private_paths)}, stream)
    return identifier


def restore_review(storage, identifier, public, with_paths=False):
    if not isinstance(identifier, str) or not re.fullmatch(r'[0-9a-f]{32}', identifier):
        raise ValueError('Invalid private review reference')
    with registry_directory(storage) as directory:
        fd = os.open(identifier + '.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        with os.fdopen(fd) as stream:
            info = os.fstat(stream.fileno())
            if info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise ValueError('Review must remain private and locally owned')
            local = json.load(stream)
    if local.get('digest') != digest(public) or not isinstance(local.get('review'), dict):
        raise ValueError('Private review binding mismatch')
    paths = local.get('private_paths', [])
    if not isinstance(paths, list) or not all(isinstance(p, str) and Path(p).is_absolute() for p in paths):
        raise ValueError('Invalid private path projection metadata')
    return (local['review'], paths) if with_paths else local['review']


def issue_review_origin(storage, report, bound, session, kind, private_paths=(), private_sessions=(), native_origin=None):
    """Trusted adapter boundary, not authentication against a writer of local files/environment."""
    from agent_policy import canonical_session, session_fingerprint
    session = canonical_session(session)
    if session.startswith('native-session-v1:') and kind != 'native':
        raise ValueError('Native session requires its actual native adapter origin')
    submitted_report = report
    report = {k: v for k, v in report.items() if k not in REVIEW_PRIVACY_FIELDS}
    if (report.get('session') != session or 'origin_receipt' in report or kind not in ('cli', 'manual', 'native') or
            not isinstance(bound, dict) or any(bound.get(k) != report.get(k) for k in ('head', 'base'))):
        raise ValueError('Adapter origin differs from report or is already bound')
    if kind == 'native':
        from agent_policy import native_session
        observed = native_origin.get('observation', {}) if isinstance(native_origin, dict) else {}
        submitted = native_origin.get('submitted_report') if isinstance(native_origin, dict) else None
        if (not isinstance(submitted, dict) or
                native_session(native_origin.get('issuer'), observed.get('session_id')) != session or
                observed.get('binding_sha256') != digest(bound) or
                observed.get('report_sha256') != digest(submitted) or
                dict({k: v for k, v in submitted.items() if k not in REVIEW_PRIVACY_FIELDS}, session=session) != report):
            raise ValueError('Native source differs from the observed report/binding')
        report = dict(report, private_identifiers=[observed['source'], observed['operation_id']])
    elif native_origin is not None:
        raise ValueError('Unexpected native origin for legacy adapter')
    if private_sessions:
        report = dict(report, private_sessions=sorted(set(map(session_fingerprint, private_sessions)) |
                      {s for s in private_sessions if s.startswith('native-session-v1:')}))
    public = {'binding': bound, 'review': report}
    identifier = store_review(storage, public, {'kind': 'adapter-review-origin-v1', 'adapter': kind,
                                              'session': session, 'report': report,
                                              'contributor_sessions': list(private_sessions),
                                              **({'submitted_report': submitted_report} if any(k in submitted_report for k in REVIEW_PRIVACY_FIELDS) else {}),
                                              **({'native_origin': native_origin} if kind == 'native' else {})}, private_paths)
    return dict(report, origin_receipt=identifier)


def verify_review_origin(storage, report, bound, with_sessions=False):
    if storage is None:
        raise ValueError('Review requires its trusted adapter private registry')
    original = {k: v for k, v in report.items() if k != 'origin_receipt'}
    receipt, paths = restore_review(storage, report.get('origin_receipt'), {'binding': bound, 'review': original}, True)
    if (receipt.get('kind') != 'adapter-review-origin-v1' or receipt.get('adapter') not in ('cli', 'manual', 'native') or
            receipt.get('session') != report.get('session') or receipt.get('report') != original):
        raise ValueError('Report was not issued by the actual-session adapter')
    if str(report.get('session', '')).startswith('native-session-v1:') and receipt['adapter'] != 'native':
        raise ValueError('Native session requires its actual native adapter origin')
    if receipt['adapter'] == 'native':
        from agent_policy import native_session
        origin = receipt.get('native_origin')
        observed = origin.get('observation') if isinstance(origin, dict) else None
        submitted = origin.get('submitted_report') if isinstance(origin, dict) else None
        if (not isinstance(observed, dict) or not isinstance(submitted, dict) or
                observed.get('binding_sha256') != digest(bound) or observed.get('report_sha256') != digest(submitted) or
                native_session(origin.get('issuer'), observed.get('session_id')) != report['session'] or
                any(not isinstance(observed.get(k), str) or not observed[k].strip() for k in ('source', 'operation_id')) or
                dict({k: v for k, v in submitted.items() if k not in REVIEW_PRIVACY_FIELDS}, session=report['session']) !=
                {k: v for k, v in original.items() if k not in REVIEW_PRIVACY_FIELDS}):
            raise ValueError('Native origin lacks its actual observation/report/binding')
    if not with_sessions or 'private_sessions' not in report:
        return (paths, []) if with_sessions else paths
    from agent_policy import session_fingerprint
    contributors = receipt.get('contributor_sessions')
    if contributors is None:
        # Old receipts need their retained canonical mapping, never the report's claim.
        from agent_worker import read_repair_bundle, recover_originals
        kind = 'certificate_revision' if report.get('certificate_revision') else 'repair_revision'
        expected_hash = report.get(kind.replace('_revision', '_evidence_sha256'))
        for record in recover_originals(storage, {'review': report, 'binding': bound}):
            if record['sha256'] != expected_hash:
                continue
            body, bundle = read_repair_bundle(record['path'], kind)
            if (hashlib.sha256(body).hexdigest() != expected_hash or
                    any(bundle[k] != report.get(k) for k in ('head', 'base', kind))):
                raise ValueError('Retained contributor bundle differs from the issued report')
            contributors = [origin['session'] for origin in bundle['origins']]
            break
    if not isinstance(contributors, list):
        raise ValueError('Reserved contributor metadata lacks independent retained provenance')
    sessions = sorted(set(map(session_fingerprint, contributors)) |
                      {s for s in contributors if s.startswith('native-session-v1:')})
    if report['private_sessions'] != sessions:
        raise ValueError('Reserved contributor metadata differs from adapter-verified identities')
    return paths, sessions


def review_privacy(storage, report, bound, with_publication=False):
    """Return projection inputs from adapter evidence, never inline report controls."""
    if not report or not (str(report.get('session', '')).startswith('native-session-v1:') or
                          report.get('origin_receipt') or any(k in report for k in REVIEW_PRIVACY_FIELDS)):
        return ([], [], [], [], False) if with_publication else ([], [], [], [])
    paths, sessions = verify_review_origin(storage, report, bound, with_sessions=True)
    original = {k: v for k, v in report.items() if k != 'origin_receipt'}
    receipt = restore_review(storage, report['origin_receipt'], {'binding': bound, 'review': original})
    identifiers, locators = [], []
    if receipt['adapter'] == 'native':
        origin = receipt.get('native_origin', {})
        observed = origin['observation']
        identifiers, locators = [observed['operation_id']], [observed['source']]
        if observed['source'] in paths:
            from agent_worker import recover_originals
            records = (recover_originals(storage, {'review': report, 'binding': bound})
                       if report.get('repair_revision') or report.get('certificate_revision') else [])
            verified = {str(path) for item in records for path in (Path(item['path']), Path(item['path']).resolve())}
            if str(Path(observed['source']).resolve()) not in verified:
                raise ValueError('Native locator lacks filesystem evidence provenance; retain receipt for owner reconciliation')
    if ('private_paths' in report or
            ('private_identifiers' in report and report['private_identifiers'] != locators + identifiers)):
        raise ValueError('Reserved privacy metadata differs from adapter-verified inputs')
    contributors = receipt.get('contributor_sessions')
    if with_publication and ((contributors is not None and not isinstance(contributors, list)) or contributors and not sessions):
        raise ValueError('Contributors lack adapter-verified publication provenance')
    result = (paths, sessions, identifiers, locators)
    limited = receipt['adapter'] == 'native' or any(s.startswith('native-session-v1:') for s in sessions)
    return (*result, limited) if with_publication else result
