"""Public opaque handoffs resolve only through the enrolling repository's private registry."""
import hashlib
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import uuid


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


def register(storage, public, source, host, repair_evidence=None, private_paths=()):
    public = dict(public, registry_id=uuid.uuid4().hex, version=2)
    with registry_directory(storage, create=True) as directory:
        fd = os.open(public['registry_id'] + '.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory)
        with os.fdopen(fd, 'w') as stream:
            json.dump({'digest': digest(public), 'source': str(source), 'host': host,
                       'repair_evidence': str(repair_evidence) if repair_evidence else None,
                       'private_paths': list(private_paths)}, stream)
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
                private_paths=local.get('private_paths', []))


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


def issue_review_origin(storage, report, bound, session, kind, private_paths=()):
    """Trusted adapter boundary, not authentication against a writer of local files/environment."""
    from agent_policy import canonical_session
    session = canonical_session(session)
    if (report.get('session') != session or 'origin_receipt' in report or kind not in ('cli', 'manual') or
            not isinstance(bound, dict) or any(bound.get(k) != report.get(k) for k in ('head', 'base'))):
        raise ValueError('Adapter origin differs from report or is already bound')
    public = {'binding': bound, 'review': report}
    identifier = store_review(storage, public, {'kind': 'adapter-review-origin-v1', 'adapter': kind,
                                              'session': session, 'report': report}, private_paths)
    return dict(report, origin_receipt=identifier)


def verify_review_origin(storage, report, bound):
    if storage is None:
        raise ValueError('Repair approval requires its trusted adapter private registry')
    original = {k: v for k, v in report.items() if k != 'origin_receipt'}
    receipt, paths = restore_review(storage, report.get('origin_receipt'), {'binding': bound, 'review': original}, True)
    if (receipt.get('kind') != 'adapter-review-origin-v1' or receipt.get('adapter') not in ('cli', 'manual') or
            receipt.get('session') != report.get('session') or receipt.get('report') != original):
        raise ValueError('Report was not issued by the actual-session adapter')
    return paths
