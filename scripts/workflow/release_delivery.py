#!/usr/bin/env python3
"""Deliver identified ZIP bytes; never build, execute, or retag an uploaded product."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote
import xml.etree.ElementTree as ET
import zipfile

from agent_loop import REPO, CI_CHECKS, git
from agent_policy import CONTEXT
from verification import PROMOTION, verify_pr

MARKER = '<!-- arb-delivery:v1\n'
MAX_ARCHIVE = 256 * 1024 * 1024
SHA = re.compile(r'[0-9a-f]{40}')
HASH = re.compile(r'[0-9a-f]{64}')
VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(args, input=None):
    # Reviewed publisher code needs the workflow token; agent workers must still strip it.
    result = subprocess.run(args, input=input, text=True, capture_output=True, timeout=180)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stderr[-1800:]}')
    return result.stdout.strip()


def api(path, method='GET', data=None):
    args = ['gh', 'api', '--method', method, f'repos/{REPO}' + ('/' + path if path else '')]
    out = command(args + (['--input', '-'] if data is not None else []),
                  json.dumps(data) if data is not None else None)
    return json.loads(out) if out else None


def pages(path):
    return [item for page in json.loads(command(['gh', 'api', '--paginate', '--slurp',
                                                 f'repos/{REPO}/{path}'])) for item in page]


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def download(path, destination, accept='application/octet-stream'):
    with destination.open('xb') as output:
        subprocess.run(['gh', 'api', f'repos/{REPO}/{path}', '-H', 'Accept: ' + accept],
                       stdout=output, check=True, timeout=180)
    require(destination.stat().st_size <= MAX_ARCHIVE, 'Downloaded archive exceeds the delivery limit')


def members(archive):
    entries = archive.infolist()
    require(len({e.filename for e in entries}) == len(entries), 'Duplicate ZIP entries')
    require(sum(e.file_size for e in entries) <= MAX_ARCHIVE, 'Expanded archive exceeds the delivery limit')
    for entry in entries:
        name = entry.filename
        require(name and not name.startswith(('/', '\\')) and '\\' not in name
                and '..' not in name.split('/') and ':' not in name
                and (entry.external_attr >> 16) & 0o170000 != 0o120000,
                'Unsafe ZIP entry')
    require(archive.testzip() is None, 'Corrupt ZIP')
    return entries


def identity(path, source, expected_hash):
    require(SHA.fullmatch(source or '') and HASH.fullmatch(expected_hash or ''), 'Fixed source and ZIP hash required')
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= MAX_ARCHIVE,
            'A bounded regular plugin ZIP is required')
    require(digest(path) == expected_hash, 'ZIP hash differs from the expected artifact')
    found = []
    with zipfile.ZipFile(path) as archive:
        for entry in members(archive):
            if not entry.filename.endswith('.jar'):
                continue
            with zipfile.ZipFile(io.BytesIO(archive.read(entry))) as jar:
                names = {e.filename for e in members(jar)}
                if 'replay/deps/identity.properties' not in names:
                    continue
                require('META-INF/plugin.xml' in names, 'Identity must belong to the plugin JAR')
                xml = jar.read('META-INF/plugin.xml')
                require(b'<!DOCTYPE' not in xml and b'<!ENTITY' not in xml, 'Unexpected plugin XML declarations')
                descriptor = ET.fromstring(xml)
                properties = {}
                for line in jar.read('replay/deps/identity.properties').decode('utf-8').splitlines():
                    key, separator, value = line.partition('=')
                    require(separator and key not in properties, 'Malformed build identity')
                    properties[key] = value
                version = descriptor.findtext('version')
                require(descriptor.findtext('id') == 'io.github.shinma06.android-replay-buffer'
                        and properties.get('source') == source and properties.get('dirty') == 'false'
                        and properties.get('version') == version and version
                        and re.fullmatch(r'[0-9A-Za-z.+-]{1,120}', version), 'Plugin source/version/clean identity differs')
                found.append(version)
    require(len(found) == 1, 'Exactly one identified plugin JAR is required')
    return {'source': source, 'version': found[0], 'sha256': expected_hash, 'size': path.stat().st_size}


def producer_run(run_id, source, workflow='.github/workflows/ci.yml'):
    require(type(run_id) is int and run_id > 0 and SHA.fullmatch(source or ''), 'Invalid run or source')
    run = api(f'actions/runs/{run_id}')
    require(run.get('repository', {}).get('full_name') == REPO
            and run.get('head_repository', {}).get('full_name') == REPO
            and run.get('head_sha') == source and run.get('status') == 'completed'
            and run.get('conclusion') == 'success' and run.get('path') == workflow,
            'Artifact must come from the identified successful repository workflow')
    require(run.get('event') in ('push', 'workflow_dispatch'), 'PR artifacts cannot directly become release assets')
    return run


def artifact(run_id, source, directory, workflow='.github/workflows/ci.yml'):
    run = producer_run(run_id, source, workflow)
    collection = api(f'actions/runs/{run_id}/artifacts?per_page=100')
    items = collection.get('artifacts', [])
    require(collection.get('total_count') == len(items), 'Incomplete artifact listing')
    selected = [a for a in items if a.get('name') == f'plugin-{source}']
    if not items:
        jobs = api(f'actions/runs/{run_id}/jobs?filter=latest&per_page=100')
        require(jobs.get('total_count') == len(jobs.get('jobs', [])), 'Incomplete job listing')
        tests = [job for job in jobs['jobs'] if job.get('name') == 'test']
        require(len(tests) == 1 and tests[0].get('conclusion') == 'success', 'Successful producer job required')
        uploads = [step for step in tests[0].get('steps', [])
                   if re.fullmatch(r'Run actions/upload-artifact@v[0-9.]+', step.get('name', ''))]
        require(len(uploads) == 1 and uploads[0].get('conclusion') == 'skipped',
                'Missing artifact without an explicitly skipped producer')
        return None, run
    require(len(selected) == 1 and selected[0].get('expired') is False, 'One live source-bound artifact required')
    return download_artifact(selected[0], directory), run


def download_artifact(item, directory):
    require(type(item.get('id')) is int and item['id'] > 0
            and type(item.get('size_in_bytes')) is int and 0 < item['size_in_bytes'] <= MAX_ARCHIVE
            and re.fullmatch(r'sha256:[0-9a-f]{64}', item.get('digest', '')),
            'Missing artifact transport identity')
    transport = directory / 'transport.zip'
    # Artifact API redirects use JSON negotiation; release assets request binary content.
    download(f'actions/artifacts/{item["id"]}/zip', transport, accept='application/vnd.github+json')
    require('sha256:' + digest(transport) == item['digest'], 'Artifact transport digest mismatch')
    product = directory / 'plugin.zip'
    with zipfile.ZipFile(transport) as archive:
        files = [e for e in members(archive) if not e.is_dir()]
        require(len(files) == 1 and files[0].filename.endswith('.zip'), 'Expected one plugin distribution')
        with product.open('xb') as output:
            output.write(archive.read(files[0]))
    return product


def candidate_archive(expected_hash, directory):
    require(HASH.fullmatch(expected_hash or ''), 'Fixed accepted ZIP hash required')
    collection = api(f'actions/artifacts?name=candidate-{expected_hash}&per_page=100')
    items = collection.get('artifacts', [])
    require(collection.get('total_count') == len(items), 'Incomplete candidate artifact listing')
    matches = [a for a in items if a.get('name') == 'candidate-' + expected_hash and a.get('expired') is False]
    require(matches, 'Accepted candidate artifact unavailable; use the retained identical ZIP with --archive')
    item = max(matches, key=lambda a: a['id'])
    run_id = item.get('workflow_run', {}).get('id')
    require(type(run_id) is int and run_id > 0, 'Candidate workflow identity missing')
    run = api(f'actions/runs/{run_id}')
    require(run.get('repository', {}).get('full_name') == REPO
            and run.get('head_repository', {}).get('full_name') == REPO
            and run.get('head_branch') == 'main' and run.get('event') == 'workflow_dispatch'
            and run.get('path') == '.github/workflows/release-candidate.yml'
            and run.get('status') == 'completed' and run.get('conclusion') == 'success'
            and SHA.fullmatch(run.get('head_sha', '')), 'Candidate did not come from the trusted successful workflow')
    git('merge-base', '--is-ancestor', run['head_sha'], 'origin/main')
    path = download_artifact(item, directory)
    require(digest(path) == expected_hash, 'Transported candidate differs from the accepted ZIP')
    return path


def checks(pr):
    data = json.loads(command(['gh', 'pr', 'checks', str(pr['number']), '--repo', REPO,
                               '--json', 'name,state']))
    for name in (*CI_CHECKS, CONTEXT):
        states = [item['state'] for item in data if item['name'] == name]
        require(states and all(state == 'SUCCESS' for state in states), 'Required PR check incomplete: ' + name)


def current_develop(source):
    require(api('git/ref/heads/develop').get('object', {}).get('sha') == source,
            'Develop advanced; a stale run must not replace early')


def early_gate(run, head):
    source = run['head_sha']
    require(run.get('event') == 'push' and run.get('head_branch') == 'develop', 'Early requires a develop push')
    current_develop(head)
    matches = [p for p in pages(f'commits/{source}/pulls?per_page=100')
               if p.get('merge_commit_sha') == source and p.get('base', {}).get('ref') == 'develop']
    require(len(matches) == 1, 'Early source needs one merged develop PR')
    pr = api(f'pulls/{matches[0]["number"]}')
    require(pr.get('merged') and pr['base']['repo']['full_name'] == REPO
            and (pr.get('head', {}).get('repo') or {}).get('full_name') == REPO,
            'Early source needs a merged same-repository PR')
    checks(pr)
    return pr['number']


def early_product(run_id, source, directory):
    trigger = producer_run(run_id, source)
    require(trigger.get('event') == 'push' and trigger.get('head_branch') == 'develop',
            'Early requires a develop push')
    head = api('git/ref/heads/develop').get('object', {}).get('sha')
    require(SHA.fullmatch(head or ''), 'Current develop identity missing')
    git('fetch', '--no-tags', 'origin', head)
    git('merge-base', '--is-ancestor', source, head)
    for candidate in git('rev-list', '--first-parent', head).splitlines():
        current_develop(head)
        runs = api('actions/workflows/ci.yml/runs?branch=develop&event=push&head_sha=' + candidate + '&per_page=100')
        items = runs.get('workflow_runs', [])
        require(runs.get('total_count') == len(items) and items
                and all(type(r.get('id')) is int and r['id'] > 0 for r in items),
                'Current product history needs complete CI records; do not fall back past unknown history')
        selected = max(items, key=lambda run: run['id'])
        archive, run = artifact(selected['id'], candidate, directory)
        number = early_gate(run, head)
        if archive is not None:
            return archive, run, number, head
        # Only the explicit producer skip above permits looking at the previous product.
    raise ValueError('No identified product artifact in the verified develop history')


def promotion_gate(number):
    require(type(number) is int and number > 0, 'Promotion PR number required')
    pr = api(f'pulls/{number}')
    require(pr.get('merged') and pr['base']['ref'] == 'main' and pr['base']['repo']['full_name'] == REPO
            and (pr['head'].get('repo') or {}).get('full_name') == REPO, 'Merged repository main promotion required')
    merge = pr.get('merge_commit_sha')
    require(SHA.fullmatch(merge or ''), 'Promotion merge identity missing')
    git('fetch', '--no-tags', 'origin', merge, 'main')
    parents = git('rev-list', '--parents', '-n', '1', merge).split()[1:]
    require(len(parents) == 2 and parents[1] == pr['head']['sha'], 'Promotion merge parent mismatch')
    pr['base']['sha'] = parents[0]
    git('merge-base', '--is-ancestor', merge, 'origin/main')
    result = verify_pr(pr, api, git)
    require(result['mode'] == 'promotion' and (result.get('gui_complete') or
            result.get('stage') == 'initial-agent' and result.get('stage_complete')),
            'Promotion acceptance is incomplete')
    record = json.loads(git('show', f'{pr["head"]["sha"]}:{PROMOTION}'))
    require(record['candidate'] == result['candidate'], 'Promotion source mismatch')
    expected_hash = record.get('artifacts', {}).get('plugin')
    require(HASH.fullmatch(expected_hash or ''), 'Promotion plugin artifact missing')
    checks(pr)
    return record['candidate'], expected_hash, merge, result


def read_record(body):
    require(isinstance(body, str) and body.count(MARKER) == 1, 'Release is not owned by this publisher')
    match = re.fullmatch(r'.*\n<!-- arb-delivery:v1\n([^\n]+)\n-->\n?', body, re.S)
    require(match is not None, 'Malformed release identity')
    record = json.loads(match[1])
    require(record.get('schema') == 1 and record.get('repository') == REPO
            and record.get('channel') in ('early', 'stable') and SHA.fullmatch(record.get('source', ''))
            and HASH.fullmatch(record.get('sha256', '')) and type(record.get('size')) is int
            and 0 < record['size'] <= MAX_ARCHIVE, 'Invalid release identity')
    return record


def asset_name(record):
    return 'android-replay-buffer-' + record['sha256'] + '.zip'


def release_body(record):
    early = record['channel'] == 'early'
    acceptance = ('開発版です。必要CIは成功していますが、全GUI・実機受入の完了を示しません。' if early else
                  '固定候補のmain昇格時の受入を確認した、同一ZIPです。')
    if record.get('stage') == 'initial-agent':
        acceptance += ' 初期Agent段階の受入であり、人間・REALの延期はQA #65に残っています。'
    tag = 'early' if early else 'v' + record['version']
    text = (f'# Android Replay Buffer {"early" if early else record["version"]}\n\n{acceptance}\n\n'
            f'[インストール用ZIP](https://github.com/{REPO}/releases/download/{tag}/{asset_name(record)})\n\n'
            f'- Plugin version: `{record["version"]}`\n- Source: `{record["source"]}`\n'
            f'- SHA-256: `{record["sha256"]}`\n'
            f'- [変更・受入](https://github.com/{REPO}/pull/{record["pr"]})\n\n'
            'GitHub自動生成のSource code ZIPはインストール用ではありません。'
            + (' early tagは保管場所の固定識別子です。製品sourceは上記のSHAです。' if early else '') + '\n')
    if early and 'develop_head' in record:
        text += f'\n追従を確認したdevelop: `{record["develop_head"]}`\n'
    return text + '\n' + MARKER + json.dumps(record, sort_keys=True, ensure_ascii=False) + '\n-->\n'


def find_release(tag):
    matches = [r for r in pages('releases?per_page=100') if r['tag_name'] == tag]
    require(len(matches) <= 1, 'Ambiguous release')
    return matches[0] if matches else None


def verify_asset(asset, path):
    require(asset.get('state') == 'uploaded' and asset.get('size') == path.stat().st_size
            and asset.get('digest') == 'sha256:' + digest(path), 'Published asset metadata differs')
    with tempfile.TemporaryDirectory() as temporary:
        fetched = Path(temporary) / 'asset.zip'
        download(f'releases/assets/{asset["id"]}', fetched)
        require(digest(fetched) == digest(path), 'Published asset bytes differ')


def latest_setting(record):
    if record['channel'] == 'early':
        return 'false'
    parsed_version = VERSION.fullmatch(record['version'])
    require(parsed_version, 'Formal release version required')
    version = tuple(map(int, parsed_version.groups()))
    for release in pages('releases?per_page=100'):
        if release['draft'] or release['prerelease']:
            continue
        tag = release['tag_name']
        parsed = VERSION.fullmatch(tag[1:]) if tag.startswith('v') else None
        if not parsed or tuple(map(int, parsed.groups())) > version:
            return 'false'
    return 'true'


def automatic_release_target(target):
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        return
    default = api('').get('default_branch')
    require(isinstance(default, str) and default, 'Default branch identity missing')
    head = api('git/ref/heads/' + quote(default, safe='')).get('object', {}).get('sha')
    require(SHA.fullmatch(head or ''), 'Default branch commit missing')
    git('fetch', '--no-tags', 'origin', head, target)
    require(not git('diff', '--name-only', head, target, '--', '.github/workflows'),
            'Automatic delivery needs Workflows write permission for fixed target ' + target +
            '; GITHUB_TOKEN cannot provide it. Preserve the target/ZIP and use the #271 owner local recovery '
            'in docs/release-delivery.md; retrying this workflow cannot grant permission.')


def publish(path, record, target):
    early = record['channel'] == 'early'
    tag = 'early' if early else 'v' + record['version']
    body, name = release_body(record), asset_name(record)
    require(SHA.fullmatch(target or ''), 'Fixed tag target required')
    release = find_release(tag)
    previous = None
    if release:
        previous = read_record(release['body'])
        require(release['body'] == release_body(previous), 'Release notes changed outside this publisher')
        require(previous['channel'] == record['channel'] and (not early or not release.get('immutable'))
                and release['prerelease'] == early, 'Release ownership/channel/immutability mismatch')
        require(early or previous == record, 'Stable release identity cannot be replaced')
    refs = [r for r in api('git/matching-refs/tags/' + tag) if r['ref'] == 'refs/tags/' + tag]
    require(len(refs) <= 1, 'Ambiguous tag')
    if refs:
        require(refs[0]['object']['type'] == 'commit' and (early and release is not None or
                refs[0]['object']['sha'] == target), 'Existing tag is not the owned fixed target')
    fixed_target = refs[0]['object']['sha'] if refs else target
    if release and release['draft'] and not refs:
        fixed_target = release.get('target_commitish')
        require(SHA.fullmatch(fixed_target or '') and (early or fixed_target == target),
                'Draft target is not the owned fixed commit')
        if early:
            git('fetch', '--no-tags', 'origin', fixed_target, 'main')
            git('merge-base', '--is-ancestor', fixed_target, 'origin/main')
    require(not release or release['draft'] or refs, 'Published release tag missing')
    if early:
        current_develop(record.get('develop_head', record['source']))
    if not release:
        automatic_release_target(fixed_target)
        release = api('releases', 'POST', {'tag_name': tag, 'target_commitish': target,
                      'name': 'Android Replay Buffer ' + ('early' if early else record['version']),
                      'body': body, 'draft': True, 'prerelease': early, 'make_latest': 'false'})
    assets = pages(f'releases/{release["id"]}/assets?per_page=100')
    require(len({a['name'] for a in assets}) == len(assets), 'Duplicate release assets')
    require(early or {a['name'] for a in assets} <= {name}, 'Unexpected stable assets')
    existing = next((a for a in assets if a['name'] == name), None)
    if existing:
        verify_asset(existing, path)
    else:
        require(early or release['draft'], 'Published stable release is incomplete; preserve it for recovery')
        uploaded = json.loads(command(['gh', 'api', '--method', 'POST',
            f'https://uploads.github.com/repos/{REPO}/releases/{release["id"]}/assets?name={quote(name)}',
            '-H', 'Content-Type: application/zip', '--input', str(path)]))
        verify_asset(uploaded, path)
    current = find_release(tag)
    require(current and all(current[k] == release[k] for k in ('id', 'body', 'draft', 'prerelease', 'target_commitish')),
            'Release changed during preparation; preserve both assets')
    if early:
        current_develop(record.get('develop_head', record['source']))
    if current['body'] != body or current['draft']:
        automatic_release_target(fixed_target)
        api(f'releases/{release["id"]}', 'PATCH', {'body': body, 'draft': False,
            'make_latest': latest_setting(record)})
    final = find_release(tag)
    require(final and final['id'] == release['id'] and not final['draft'] and final['body'] == body
            and final['prerelease'] == early, 'Publication readback differs')
    final_ref = api('git/ref/tags/' + tag)
    require(final_ref['object']['type'] == 'commit' and final_ref['object']['sha'] == fixed_target,
            'Published tag moved')
    final_assets = pages(f'releases/{release["id"]}/assets?per_page=100')
    published = [a for a in final_assets if a['name'] == name]
    require(len(published) == 1, 'Published asset missing or ambiguous')
    verify_asset(published[0], path)
    # Keep the old download until the new body and bytes are confirmed.
    if early and previous and asset_name(previous) != name:
        old = [a for a in final_assets if a['name'] == asset_name(previous)]
        for asset in old:
            require(asset.get('digest') == 'sha256:' + previous['sha256'], 'Old asset identity changed')
            api(f'releases/assets/{asset["id"]}', 'DELETE')
    return f'https://github.com/{REPO}/releases/tag/{tag}'


def stable_release(number, directory, archive=None, version=None):
    source, expected, merge, result = promotion_gate(number)
    archive = archive or candidate_archive(expected, directory)
    record = dict(identity(archive, source, expected), schema=1, repository=REPO,
                  channel='stable', pr=number, main_merge=merge, stage=result.get('stage', 'full'))
    require(VERSION.fullmatch(record['version']) and (version is None or record['version'] == version),
            'Select the formal version before building/testing; never rewrite the ZIP')
    return publish(archive, record, merge)


def promoted(source, directory):
    require(SHA.fullmatch(source or ''), 'Fixed main source required')
    matches = [p for p in pages(f'commits/{source}/pulls?per_page=100')
               if p.get('merge_commit_sha') == source and p.get('base', {}).get('ref') == 'main'
               and re.search(r'^Integration: promotion$', p.get('body') or '', re.M)]
    require(len(matches) <= 1, 'Ambiguous main promotion')
    if not matches:
        return 'No promotion in this main commit; no release changed.'
    return stable_release(matches[0]['number'], directory)


def main():
    parser = argparse.ArgumentParser(__doc__)
    sub = parser.add_subparsers(dest='channel', required=True)
    early = sub.add_parser('early')
    early.add_argument('--run', type=int, required=True)
    early.add_argument('--source', required=True)
    stable = sub.add_parser('stable')
    stable.add_argument('--archive', type=Path)
    stable.add_argument('--promotion-pr', type=int, required=True)
    stable.add_argument('--version')
    automatic = sub.add_parser('promoted')
    automatic.add_argument('--source', required=True)
    inspect = sub.add_parser('inspect')
    inspect.add_argument('--archive', type=Path, required=True)
    inspect.add_argument('--source', required=True)
    inspect.add_argument('--version', required=True)
    args = parser.parse_args()
    if args.channel == 'early':
        with tempfile.TemporaryDirectory() as temporary:
            archive, run, number, head = early_product(args.run, args.source, Path(temporary))
            record = dict(identity(archive, run['head_sha'], digest(archive)), schema=1,
                          repository=REPO, channel='early', pr=number, run=run['id'], develop_head=head)
            target = api('git/ref/heads/main')['object']['sha']
            print(publish(archive, record, target))
    elif args.channel == 'inspect':
        record = identity(args.archive, args.source, digest(args.archive))
        require(VERSION.fullmatch(args.version) and record['version'] == args.version,
                'Formal version was not applied before the build')
        print(record['sha256'])
    else:
        with tempfile.TemporaryDirectory() as temporary:
            if args.channel == 'promoted':
                print(promoted(args.source, Path(temporary)))
            else:
                print(stable_release(args.promotion_pr, Path(temporary), args.archive, args.version))


if __name__ == '__main__':
    main()
