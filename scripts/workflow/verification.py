"""Acceptance data and fixed-candidate gates. Never execute code from a PR."""
import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess

SHA = re.compile(r'[0-9a-f]{40}')
HASH = re.compile(r'[0-9a-f]{64}')
PROMOTION = 'docs/verification/promotion.json'
BASELINE = 'docs/verification/legacy-baseline.json'
STATUSES = {'pending', 'blocked', 'fail', 'pass'}
TOOLING = ('docs/', 'scripts/', '.github/', '.githooks/', '.agents/', '.claude/skills/', '.cursor/rules/')
GOP_AMENDMENT = 'docs/verification/amendments/gop-boundaries.json'
INITIAL_STAGE = 'initial-agent'
INITIAL_PLAN = 'docs/verification/amendments/initial-agent.json'
GOP_KEYS = {f'{issue}:SYNC-WINDOW' for issue in (13, 31, 41)} | {
    f'27:{case}-{device}' for case in ('BUFFER-180', 'BUFFER-SHORT', 'SAVE-CONTINUE')
    for device in ('REAL', 'EMU')}
GOP_EVIDENCE = {'one_second_regression', 'after_idr', 'mid_gop', 'before_next_idr',
                'vfr', 'limits_and_quality'}


def field(body, name):
    values = re.findall(r'^' + re.escape(name) + r': (.+)$', body or '', re.M)
    if len(values) != 1:
        raise ValueError('Exactly one ' + name + ' is required')
    return values[0].strip()


def metadata(pr):
    issue = int(field(pr['body'], 'Issue').removeprefix('#'))
    path = field(pr['body'], 'Verification')
    expected = f'docs/verification/changes/issue-{issue}.json'
    if path != expected:
        raise ValueError('Verification must name the Issue acceptance JSON: ' + expected)
    mode = field(pr['body'], 'Integration')
    if mode not in ('develop', 'promotion', 'tooling'):
        raise ValueError('Integration must be develop, promotion or tooling')
    if pr['base']['ref'] != ('develop' if mode == 'develop' else 'main'):
        raise ValueError('Integration does not match PR target')
    return issue, path, mode


def nonempty(value):
    return isinstance(value, str) and bool(value.strip())


def validate_change(data, issue, gui):
    if data.get('schema') != 1 or data.get('issue') != issue or type(data.get('gui_required')) is not bool or data['gui_required'] != gui:
        raise ValueError('Acceptance Issue/GUI declaration mismatch')
    if not nonempty(data.get('reason')) or not data.get('cli_checks') or not all(nonempty(x) for x in data['cli_checks']):
        raise ValueError('Concrete reason and CLI checks are required')
    cases = data.get('cases')
    if not isinstance(cases, list) or (gui and not cases) or (not gui and cases):
        raise ValueError('Required GUI Cases missing or not-required contradicts Cases')
    ids = set()
    for case in cases:
        if not isinstance(case, dict) or not re.fullmatch(r'[A-Z][A-Z0-9-]+', case.get('id', '')) or case['id'] in ids:
            raise ValueError('Invalid or duplicate Case ID')
        ids.add(case['id'])
        if case.get('required_execution') not in (None, 'computer_use'):
            raise ValueError('Unknown required execution method')
        if not re.fullmatch(r'[a-z][a-z0-9_]*', case.get('artifact', 'app')):
            raise ValueError('Invalid Case artifact name')
        for key in ('change', 'preconditions', 'expected', 'next_action'):
            if not nonempty(case.get(key)):
                raise ValueError('Missing Case ' + key)
        if not case.get('steps') or not all(nonempty(x) for x in case['steps']):
            raise ValueError('Reproducible steps are required')
        if not case.get('provenance') or not all(nonempty(x) for x in case['provenance']):
            raise ValueError('Case provenance is required')
        for actor in ('gpt', 'human'):
            observation = case.get(actor)
            if not isinstance(observation, dict) or observation.get('status') not in STATUSES or not nonempty(observation.get('reason')):
                raise ValueError('Explicit agent (gpt key) and human status/reason required')
            if observation['status'] == 'pass':
                validate_observation(observation, observation.get('head'))
            if observation['status'] == 'fail' and (type(case.get('fix_issue')) is not int or case['fix_issue'] <= 0 or case['fix_issue'] == issue):
                raise ValueError('Product fail requires a dedicated fix Issue')
        if 'fix_issue' not in case or 'fix_pr' not in case or not nonempty(case.get('recheck')):
            raise ValueError('Fix/recheck tracking fields are required')
    return data


def validate_observation(result, candidate, artifact=None):
    if (result.get('status') != 'pass' or not SHA.fullmatch(candidate or '') or result.get('head') != candidate or
            not HASH.fullmatch(result.get('artifact_sha256', '')) or
            (artifact is not None and result['artifact_sha256'] != artifact)):
        raise ValueError('Case must pass on the exact candidate and build')
    if result.get('actor') not in ('gpt', 'human'):
        raise ValueError('Observer actor must use the legacy gpt or human value')
    for key in ('observer', 'at', 'evidence', 'loaded_identity', 'reason'):
        if not nonempty(result.get(key)):
            raise ValueError('Missing observed evidence: ' + key)
    if not re.fullmatch(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:Z|[+-]\d\d:\d\d)', result['at']):
        raise ValueError('Use an ISO8601 observation timestamp')


def validate_full_result(result, candidate, artifact, execution, gop, initial_entry=None):
    validate_observation(result, candidate, artifact)
    validate_gop_observation(result, gop)
    if execution and result.get('execution') != execution:
        raise ValueError('Case requires its specified execution method: ' + execution)
    actor = (initial_entry or {}).get('full_acceptance_actor')
    if actor and (result.get('actor') != actor or not nonempty(result.get('human_evidence'))):
        raise ValueError('Full acceptance requires the deferred human observation and evidence')


def git_read(*args, cwd=None):
    return subprocess.check_output(['git', *args], cwd=cwd, text=True).strip()


def tooling_path(path):
    return path.startswith(TOOLING + ('tests/', 'examples/')) or path in ('CLAUDE.md', 'AGENTS.md', 'README.md', 'CONTRIBUTING.md')


def baseline_cases(base, entries, git):
    """Legacy develop history needs a reviewed main-side plan and full real acceptance."""
    selected = [item for item in entries if item.get('baseline') is True]
    if not selected:
        return {}
    data = regular_json(base, BASELINE, git)
    commits = data.get('commits')
    issue = data.get('issue')
    if (data.get('schema') != 1 or type(issue) is not int or issue <= 0
            or not isinstance(commits, list) or not commits
            or not all(isinstance(commit, str) and SHA.fullmatch(commit) for commit in commits)
            or len(set(commits)) != len(commits)
            or {item['commit'] for item in selected} != set(commits)
            or any(set(item) != {'commit', 'baseline'} for item in selected)):
        raise ValueError('Legacy baseline must cover exactly the reviewed commit set and Issue')
    change = validate_change(data['acceptance'], issue, True)
    return {f'{issue}:{case["id"]}': (case.get('required_execution'), case.get('artifact', 'app'))
            for case in change['cases']}


def source_commits(source, base, git):
    """Bind squash results or a tooling-only main sync to their actual merged DAG."""
    merge = source['merge_commit_sha']
    parents = git('rev-list', '--parents', '-n', '1', merge).split()[1:]
    if len(parents) == 1:
        return {merge}
    if (len(parents) != 2 or parents != [source['base']['sha'], source['head']['sha']] or
            field(source['body'], 'GUI') != 'not-required'):
        raise ValueError('Sync merge must match the merged develop PR base/head and be tooling-only')
    develop, head = parents
    chain = git('rev-list', '--first-parent', f'{develop}..{head}').splitlines()
    synced_main = False
    for index, commit in enumerate(chain):
        inputs = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
        previous = chain[index + 1] if index + 1 < len(chain) else develop
        if not inputs or len(inputs) > 2 or inputs[0] != previous:
            raise ValueError('Sync history must return to the merged develop parent along first parents')
        if len(inputs) == 2:
            git('merge-base', '--is-ancestor', inputs[1], base)
            synced_main = True
        # Check each edge, so a product edit followed by a revert cannot disappear.
        if any(not tooling_path(p) for p in git('diff', '--no-renames', '--name-only', inputs[0], commit).splitlines()):
            raise ValueError('Product change in sync history, even if later reverted')
    if not synced_main:
        raise ValueError('Sync PR must actually merge an ancestor of the fixed main base')
    for parent in parents:
        if any(not tooling_path(p) for p in git('diff', '--no-renames', '--name-only', parent, merge).splitlines()):
            raise ValueError('Product change in sync merge result')
    covered = {merge, *chain}
    actual = {merge, *git('rev-list', head, '--not', develop, base).splitlines()}
    if covered != actual:
        raise ValueError('Sync PR contains unexplained or already-main first-parent history')
    return covered


def regular_json(ref, path, git):
    entry = git('ls-tree', ref, '--', path).split()
    if len(entry) != 4 or entry[0] != '100644' or entry[1] != 'blob' or entry[3] != path:
        raise ValueError('Scope plan must be a regular JSON file in trusted main')
    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('Duplicate key in trusted scope plan')
            result[key] = value
        return result
    return json.loads(git('show', f'{ref}:{path}'), object_pairs_hook=unique_pairs)


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(',', ':')).encode()).hexdigest()


def gop_amendment(base, candidate, git):
    """One reviewed premise amendment, read from trusted main, never promotion HEAD."""
    if not SHA.fullmatch(base or '') or not SHA.fullmatch(candidate or ''):
        raise ValueError('GOP amendment requires fixed base and candidate')
    if not git('ls-tree', base, '--', GOP_AMENDMENT):
        return None
    policy = regular_json(base, GOP_AMENDMENT, git)
    if (not isinstance(policy, dict) or set(policy) != {'schema', 'issue', 'required_ancestor', 'original_premise',
                       'runtime_premise', 'additional_steps', 'cases'} or
            type(policy['schema']) is not int or policy['schema'] != 1 or policy['issue'] != 56 or
            not isinstance(policy['required_ancestor'], str) or
            not SHA.fullmatch(policy['required_ancestor']) or
            policy['original_premise'] != '1秒程度の実測GOP' or
            not nonempty(policy['runtime_premise']) or
            not isinstance(policy['additional_steps'], list) or
            not policy['additional_steps'] or not all(nonempty(x) for x in policy['additional_steps']) or
            not isinstance(policy['cases'], dict) or set(policy['cases']) != GOP_KEYS):
        raise ValueError('Invalid limited GOP amendment')
    for key, sources in policy['cases'].items():
        if not isinstance(sources, list) or len(sources) != (2 if key.startswith('27:') else 1):
            raise ValueError('GOP amendment must retain all fifteen source occurrences')
        seen = set()
        for source in sources:
            if (not isinstance(source, dict) or
                    set(source) != {'source_pr', 'source_merge', 'source_path', 'original_case_sha256'} or
                    type(source['source_pr']) is not int or source['source_pr'] <= 0 or
                    not isinstance(source['source_merge'], str) or
                    not SHA.fullmatch(source['source_merge']) or
                    source['source_path'] != f'docs/verification/changes/issue-{key.split(":")[0]}.json' or
                    not isinstance(source['original_case_sha256'], str) or
                    not HASH.fullmatch(source['original_case_sha256']) or
                    source['source_pr'] in seen):
                raise ValueError('Invalid GOP source binding')
            seen.add(source['source_pr'])
    git('merge-base', '--is-ancestor', policy['required_ancestor'], candidate)
    if regular_json(candidate, GOP_AMENDMENT, git) != policy:
        raise ValueError('Candidate lacks the exact trusted GOP amendment; synchronize main first')
    return policy


def effective_gop_cases(policy, source_cases):
    """Preserve historical Cases; replace only the named premise and append measurements."""
    if policy is None:
        return {}
    actual = {}
    originals = {}
    for key, case, source in source_cases:
        if key not in GOP_KEYS:
            continue
        binding = dict(source, original_case_sha256=canonical_hash(case))
        actual.setdefault(key, []).append(binding)
        if key in originals and originals[key] != case:
            raise ValueError('Contradictory duplicate GOP Case contracts: ' + key)
        originals[key] = case
    if (set(actual) != GOP_KEYS or any(sorted(actual[key], key=lambda x: x['source_pr']) !=
            sorted(policy['cases'][key], key=lambda x: x['source_pr']) for key in GOP_KEYS)):
        raise ValueError('GOP amendment does not match every original source occurrence')
    result = {}
    for key, case in originals.items():
        if case['preconditions'].count(policy['original_premise']) != 1:
            raise ValueError('Original GOP premise missing or ambiguous: ' + key)
        result[key] = {
            'case': dict(case, preconditions=case['preconditions'].replace(
                policy['original_premise'], policy['runtime_premise']),
                steps=case['steps'] + policy['additional_steps']),
            'original_preconditions': case['preconditions'],
            'revision': canonical_hash(policy), 'sources': policy['cases'][key]}
    return result


def validate_gop_observation(result, contract):
    if contract is None:
        if 'gop_revision' in result or 'gop_evidence' in result:
            raise ValueError('GOP revision is not applicable to this fixed Case')
        return
    evidence = result.get('gop_evidence')
    if (result.get('gop_revision') != contract['revision'] or not isinstance(evidence, dict) or
            set(evidence) != GOP_EVIDENCE or not all(nonempty(x) for x in evidence.values())):
        raise ValueError('Fresh GOP revision and all boundary/regression evidence are required')


def initial_stage_plan(base, candidate, git):
    """The one initial-release plan is authorized in main, not in the candidate."""
    policy = regular_json(base, INITIAL_PLAN, git)
    if (not isinstance(policy, dict) or set(policy) != {
            'schema', 'issue', 'stage', 'required_ancestor', 'followup_issue',
            'owner', 'resume_condition', 'human_scope', 'cases'} or
            type(policy['schema']) is not int or policy['schema'] != 1 or
            policy['issue'] != 64 or policy['stage'] != INITIAL_STAGE or
            policy['followup_issue'] != 65 or not isinstance(policy['required_ancestor'], str) or
            not SHA.fullmatch(policy['required_ancestor']) or
            any(not nonempty(policy[k]) for k in ('owner', 'resume_condition', 'human_scope')) or
            not isinstance(policy['cases'], dict) or not policy['cases']):
        raise ValueError('Invalid initial-agent acceptance plan')
    git('merge-base', '--is-ancestor', policy['required_ancestor'], candidate)
    if regular_json(candidate, INITIAL_PLAN, git) != policy:
        raise ValueError('Candidate lacks the exact trusted initial plan; synchronize main first')
    for key, entry in policy['cases'].items():
        if (not re.fullmatch(r'[1-9][0-9]*:[A-Z][A-Z0-9-]+', key) or
                not isinstance(entry, dict) or set(entry) - {'full_acceptance_actor'} != {
                    'sources', 'current_case_sha256', 'initial_scope', 'deferred_scope', 'agent_requirements'} or
                ('full_acceptance_actor' in entry and entry['full_acceptance_actor'] != 'human') or
                not isinstance(entry['current_case_sha256'], str) or not HASH.fullmatch(entry['current_case_sha256']) or
                entry['initial_scope'] not in ('agent', 'deferred') or
                (entry['initial_scope'] == 'deferred') != key.endswith('-REAL') or
                not isinstance(entry['deferred_scope'], list) or
                (entry['initial_scope'] == 'deferred' and not entry['deferred_scope']) or
                not all(nonempty(x) for x in entry['deferred_scope']) or
                ('full_acceptance_actor' in entry and not entry['deferred_scope']) or
                not isinstance(entry['agent_requirements'], list) or
                not all(nonempty(x) for x in entry['agent_requirements']) or
                not isinstance(entry['sources'], list) or not entry['sources']):
            raise ValueError('Invalid initial Case scope: ' + key)
        seen = set()
        for source in entry['sources']:
            if (not isinstance(source, dict) or set(source) != {
                    'source_pr', 'source_merge', 'source_path', 'original_case_sha256'} or
                    type(source['source_pr']) is not int or source['source_pr'] <= 0 or
                    not isinstance(source['source_merge'], str) or not SHA.fullmatch(source['source_merge']) or
                    source['source_path'] != f'docs/verification/changes/issue-{key.split(":")[0]}.json' or
                    not isinstance(source['original_case_sha256'], str) or
                    not HASH.fullmatch(source['original_case_sha256']) or source['source_pr'] in seen):
                raise ValueError('Invalid initial Case source: ' + key)
            seen.add(source['source_pr'])
        source_path = f'docs/verification/changes/issue-{key.split(":")[0]}.json'
        current = regular_json(policy['required_ancestor'], source_path, git)
        cases = [c for c in current['cases'] if c['id'] == key.split(':')[1]]
        if len(cases) != 1 or canonical_hash(cases[0]) != entry['current_case_sha256']:
            raise ValueError('Initial plan current Case identity differs: ' + key)
        if entry['initial_scope'] == 'deferred':
            paired = policy['cases'].get(key.removesuffix('-REAL') + '-EMU')
            if paired is None or paired['initial_scope'] != 'agent':
                raise ValueError('REAL deferral must retain its paired EMU scope: ' + key)
    return policy


def bind_initial_sources(policy, source_cases):
    """Bind every source occurrence, including different historical versions of a Case."""
    actual = {}
    for key, case, source in source_cases:
        actual.setdefault(key, []).append(dict(source, original_case_sha256=canonical_hash(case)))
    expected = {key: entry['sources'] for key, entry in policy['cases'].items()}
    if (set(actual) != set(expected) or any(
            sorted(actual[key], key=lambda x: x['source_pr']) !=
            sorted(expected[key], key=lambda x: x['source_pr']) for key in expected)):
        raise ValueError('Initial plan must bind ALL original Case source occurrences')


def historical_initial_sources(policy, candidate, git):
    """Keep deferred contracts readable after their commits are already in main."""
    rows = []
    for key, entry in policy['cases'].items():
        for source in entry['sources']:
            git('merge-base', '--is-ancestor', source['source_merge'], candidate)
            data = regular_json(source['source_merge'], source['source_path'], git)
            validate_change(data, int(key.split(':')[0]), True)
            cases = [c for c in data['cases'] if c['id'] == key.split(':')[1]]
            if len(cases) != 1:
                raise ValueError('Original initial Case is missing: ' + key)
            rows.append((key, cases[0], {k: source[k] for k in
                         ('source_pr', 'source_merge', 'source_path')}))
    bind_initial_sources(policy, rows)
    return rows


def validate_initial_result(result, key, policy, candidate, artifact, execution, gop):
    entry = policy['cases'][key]
    deferred = entry['initial_scope'] == 'deferred'
    expected_keys = {'status', 'stage_revision', 'head', 'artifact_sha256', 'followup_issue'}
    if not deferred:
        expected_keys.add('initial_observation')
    if (not isinstance(result, dict) or set(result) != expected_keys or
            result.get('status') != ('deferred' if deferred else 'initial-pass') or
            result.get('stage_revision') != canonical_hash(policy) or
            not SHA.fullmatch(candidate or '') or result.get('head') != candidate or
            not HASH.fullmatch(artifact or '') or result.get('artifact_sha256') != artifact or
            result.get('followup_issue') != policy['followup_issue']):
        raise ValueError('Initial result must retain its exact scope, build and deferred follow-up: ' + key)
    if not deferred:
        observed = result['initial_observation']
        if not isinstance(observed, dict) or observed.get('actor') != 'gpt':
            raise ValueError('Initial scope requires an actual agent observation: ' + key)
        validate_observation(observed, candidate, artifact)
        validate_gop_observation(observed, gop)
        if execution and observed.get('execution') != execution:
            raise ValueError('Case requires its specified execution method: ' + key)


def validate_candidate_result(result, key, policy, stage, candidate, artifact, execution, gop, git):
    """Keep old observations intact; accept reuse only with fresh equivalence evidence."""
    if not isinstance(result, dict):
        raise ValueError('Case result must be an object')
    source = candidate
    source_policy = policy
    if result.get('status') == 'reused':
        if (set(result) != {'status', 'head', 'artifact_sha256', 'source_candidate',
                           'source_result', 'confirmation'} or
                result['head'] != candidate or result['artifact_sha256'] != artifact or
                not isinstance(result['source_candidate'], str) or
                not SHA.fullmatch(result['source_candidate']) or result['source_candidate'] == candidate):
            raise ValueError('Reuse requires distinct fixed source and current candidate/build')
        source = result['source_candidate']
        original, confirmation = result['source_result'], result['confirmation']
        if (not isinstance(original, dict) or original.get('status') != ('initial-pass' if stage else 'pass') or
                not isinstance(confirmation, dict)):
            raise ValueError('Only original successful observations can be reused; no chains or deferrals')
        validate_observation(confirmation, candidate, artifact)
        for name in ('equivalence_evidence', 'environment_evidence'):
            if not nonempty(confirmation.get(name)):
                raise ValueError('Reuse needs current loaded/environment/condition evidence: ' + name)
        # A reverted product or contract edit also invalidates reuse.
        try:
            git('merge-base', '--is-ancestor', source, candidate)
            allowed = ('docs/', 'scripts/workflow/', '.agents/skills/', '.claude/skills/', '.cursor/rules/')
            for commit in git('rev-list', f'{source}..{candidate}').splitlines():
                parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
                if not parents:
                    raise ValueError('Reuse history lacks a parent')
                for parent in parents:
                    paths = git('diff', '--no-renames', '--name-only', parent, commit).splitlines()
                    if any(not (p.startswith(allowed) or p in
                            ('AGENTS.md', 'CLAUDE.md', 'README.md', 'CONTRIBUTING.md')) for p in paths):
                        raise ValueError('Product, build, configuration, fixture or unknown change requires retesting')
            path = f'docs/verification/changes/issue-{key.split(":")[0]}.json'
            if git('log', '--full-history', '--format=%H', f'{source}..{candidate}', '--', path):
                raise ValueError('Case contract history changed; retest required')
            old_change = regular_json(source, path, git)
            new_change = regular_json(candidate, path, git)
            for change in (old_change, new_change):
                validate_change(change, int(key.split(':')[0]), True)
            old_cases = [c for c in old_change['cases'] if c['id'] == key.split(':')[1]]
            new_cases = [c for c in new_change['cases'] if c['id'] == key.split(':')[1]]
            if len(old_cases) != 1 or old_cases != new_cases:
                raise ValueError('Reuse requires the same existing Case contract')
            if policy and key in policy['cases']:
                source_policy = initial_stage_plan(source, source, git)
                if (source_policy['cases'].get(key) != policy['cases'][key] or
                        any(source_policy[k] != policy[k] for k in ('human_scope', 'followup_issue'))):
                    raise ValueError('Initial/deferred/human scope changed; retest required')
        except subprocess.CalledProcessError as exc:
            raise ValueError('Reuse source or equivalence could not be verified') from exc
        observed = original.get('initial_observation', {}) if stage else original
        if not isinstance(observed, dict):
            raise ValueError('Original observation missing')
        validate_observation(observed, source, artifact)
        if datetime.fromisoformat(confirmation['at'].replace('Z', '+00:00')) < datetime.fromisoformat(observed['at'].replace('Z', '+00:00')):
            raise ValueError('Reuse confirmation predates the original observation')
        result = original
    if stage:
        validate_initial_result(result, key, source_policy, source, artifact, execution, gop)
    else:
        validate_full_result(result, source, artifact, execution, gop,
                             policy['cases'].get(key) if policy else None)
    return result


def scoped_history(base, head, allowed, git):
    """Inspect every edge, including changes later reverted; never follow symlinks."""
    previous = base
    commits = git('rev-list', '--reverse', f'{base}..{head}').splitlines()
    for commit in commits:
        if git('rev-list', '--parents', '-n', '1', commit).split() != [commit, previous]:
            raise ValueError('Scoped candidate and result history must be linear from the fixed base')
        raw = git('diff', '--raw', '--no-abbrev', '--no-renames', '--ignore-submodules=none', '-z', previous, commit)
        fields = raw.split('\0')
        if fields[-1] != '':
            raise ValueError('Malformed scoped diff')
        for index in range(0, len(fields) - 1, 2):
            header, path = fields[index].split(), fields[index + 1]
            if (len(header) != 5 or path not in allowed or header[0] not in (':000000', ':100644', ':100755')
                    or header[1] != allowed[path] or header[4] not in ('A', 'M')):
                raise ValueError('Path or file mode outside trusted scope: ' + path)
        previous = commit
    if previous != head:
        raise ValueError('Scoped candidate is not descended from the fixed base')
    return commits


def scoped_candidate(base, candidate, issue, git=git_read):
    if not SHA.fullmatch(base or '') or not SHA.fullmatch(candidate or '') or type(issue) is not int or issue <= 0:
        raise ValueError('Invalid scoped candidate identity')
    path = f'docs/verification/scopes/issue-{issue}.json'
    plan = regular_json(base, path, git)
    if plan.get('schema') != 1 or plan.get('issue') != issue:
        raise ValueError('Trusted scope Issue mismatch')
    allowed = plan.get('files')
    if (not isinstance(allowed, dict) or not allowed or
            any(not isinstance(p, str) or p.startswith(('/', 'scripts/workflow/', 'docs/verification/')) or
                any(part in ('', '.', '..') for part in p.split('/')) or '\\' in p or m not in ('100644', '100755') for p, m in allowed.items())):
        raise ValueError('Scope cannot modify its plan, acceptance or release gates')
    protected = {'.github/workflows/acceptance.yml', '.github/workflows/pr-policy.yml', '.github/workflows/agent-review.yml'}
    if set(allowed) & protected:
        raise ValueError('Scope cannot modify trusted check workflows')
    change = validate_change(plan['acceptance'], issue, True)
    if not scoped_history(base, candidate, allowed, git):
        raise ValueError('Scoped candidate has no changes')
    return change


def verify_pr(pr, api, git=git_read):
    """api(path) uses the same repo; git sees fetched PR/candidate objects only as data."""
    issue, path, mode = metadata(pr)
    head, base = pr['head']['sha'], pr['base']['sha']
    data = json.loads(git('show', f'{head}:{path}'))
    gui = field(pr['body'], 'GUI') == 'required'
    validate_change(data, issue, gui)
    files = git('diff', '--no-renames', '--name-only', base, head).splitlines()
    if mode == 'develop':
        # Outcome is deliberately unrestricted, but every required Case has steps and tracking.
        for case in data['cases']:
            if any(case[actor]['status'] == 'fail' for actor in ('gpt', 'human')):
                fix = api(f'issues/{case["fix_issue"]}')
                if fix.get('state') != 'open' or 'pull_request' in fix:
                    raise ValueError('Product failure needs an open dedicated fix Issue')
        return {'mode': mode, 'gui_complete': not gui, 'cases': len(data['cases'])}
    if mode == 'tooling':
        if gui or not files or any(not tooling_path(f) for f in files):
            raise ValueError('Direct main tooling route is restricted to documented non-product paths')
        return {'mode': mode, 'gui_complete': True, 'cases': 0}
    promotion = json.loads(git('show', f'{head}:{PROMOTION}'))
    candidate = promotion.get('candidate')
    if promotion.get('schema') != 1 or promotion.get('base') != base or not SHA.fullmatch(candidate or ''):
        raise ValueError('Promotion must bind the current main base and fixed candidate')
    scope = promotion.get('scope', 'develop')
    gop_cases = {}
    initial_policy = None
    full_policy = None
    source_cases = []
    stage = promotion.get('stage')
    if stage is not None and (stage != INITIAL_STAGE or scope != 'develop'):
        raise ValueError('Initial stage is available only for the reviewed develop candidate')
    if stage == INITIAL_STAGE and not gui:
        raise ValueError('Initial promotion still requires its GUI declaration')
    if scope not in ('develop', 'main'):
        raise ValueError('Unknown promotion scope')
    if scope == 'main':
        if not gui or 'changes' in promotion:
            raise ValueError('Scoped promotion requires GUI and uses trusted plan, not develop changes')
        change = scoped_candidate(base, candidate, issue, git)
        if data != change:
            raise ValueError('Scoped acceptance must equal the trusted plan; observations belong in results')
        scoped_history(candidate, head, {PROMOTION: '100644', path: '100644'}, git)
        required = {f'{issue}:{case["id"]}': (case.get('required_execution'), case.get('artifact', 'app'))
                    for case in change['cases']}
    else:
        # Later develop work belongs to the next batch. The fixed candidate must still be its ancestor.
        ref = api('git/ref/heads/develop')
        develop = ref.get('object', {}).get('sha')
        if not SHA.fullmatch(develop or ''):
            raise ValueError('Invalid develop reference')
        git('fetch', '--no-tags', 'origin', develop)
        git('merge-base', '--is-ancestor', candidate, develop)
        git('merge-base', '--is-ancestor', candidate, head)
        git('merge-base', '--is-ancestor', base, head)
        # Metadata-only changes after the tested candidate. No untested product edits or main conflict resolutions.
        allowed = {PROMOTION, path}
        if any(p not in allowed for p in git('diff', '--no-renames', '--name-only', candidate, head).splitlines()):
            raise ValueError('Promotion tree differs from tested candidate outside acceptance metadata')
        # A net-zero revert must not smuggle later/unobserved commits into main ancestry.
        # Every new promotion commit is metadata-only; the only allowed merge parent is current main.
        for commit in git('rev-list', head, '--not', candidate, base).splitlines():
            parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
            if not parents or len(parents) > 2 or (len(parents) == 2 and parents[1] != base):
                raise ValueError('Unexpected promotion ancestry; only the current main merge is allowed')
            changed = git('diff', '--no-renames', '--name-only', parents[0], commit).splitlines()
            if any(p not in allowed for p in changed):
                raise ValueError('Untested commit in promotion history, even if later reverted')
        commits = git('rev-list', f'{base}..{candidate}').splitlines()
        if not commits or len(commits) != len(set(commits)):
            raise ValueError('No candidate changes or duplicate commits')
        changes = promotion.get('changes', [])
        if not isinstance(changes, list) or {x.get('commit') for x in changes} != set(commits) or len(changes) != len(commits):
            raise ValueError('Promotion must cover EVERY candidate commit absent from main exactly once')
        required = baseline_cases(base, changes, git)
        by_pr = {}
        for item in changes:
            if item.get('baseline') is True:
                continue
            number = item.get('pr')
            if type(number) is not int or number <= 0:
                raise ValueError('Each candidate commit needs its merged develop PR')
            by_pr.setdefault(number, set()).add(item['commit'])
        for number, covered in by_pr.items():
            source = api(f'pulls/{number}')
            if (not source.get('merged') or source['base']['ref'] != 'develop' or
                    source['merge_commit_sha'] not in covered or
                    source['base']['repo']['full_name'] != pr['base']['repo']['full_name'] or
                    source['head']['repo'] is None or source['head']['repo']['full_name'] != pr['base']['repo']['full_name']):
                raise ValueError('Commit is not the identified same-repository merged develop PR')
            if covered != source_commits(source, base, git):
                raise ValueError('Candidate commits do not exactly match their merged develop PR provenance')
            source_issue, source_path, source_mode = metadata(source)
            if source_mode != 'develop':
                raise ValueError('Missing develop acceptance provenance')
            source_gui = field(source['body'], 'GUI') == 'required'
            change = validate_change(json.loads(git('show', f'{source["merge_commit_sha"]}:{source_path}')), source_issue, source_gui)
            for case in change['cases']:
                key = f'{source_issue}:{case["id"]}'
                source_cases.append((key, case, {'source_pr': number,
                    'source_merge': source['merge_commit_sha'], 'source_path': source_path}))
                requirement = (case.get('required_execution'), case.get('artifact', 'app'))
                if key in required and required[key] != requirement:
                    raise ValueError('Case execution requirement changed across candidate commits')
                required[key] = requirement
        if set(required) & GOP_KEYS:
            gop_cases = effective_gop_cases(gop_amendment(base, candidate, git), source_cases)
        if stage == INITIAL_STAGE:
            initial_policy = initial_stage_plan(base, candidate, git)
            bind_initial_sources(initial_policy, source_cases)
            if set(initial_policy['cases']) != set(required):
                raise ValueError('Initial stage cannot omit baseline or other required Cases')
    if stage is None and git('ls-tree', base, '--', INITIAL_PLAN):
        # A later promotion does not erase deferred work merely because its
        # original commits are now in main. Full acceptance still needs it.
        carried = full_policy = initial_stage_plan(base, candidate, git)
        historical = historical_initial_sources(carried, candidate, git)
        for key, case, _ in historical:
            if carried['cases'][key]['deferred_scope']:
                requirement = (case.get('required_execution'), case.get('artifact', 'app'))
                if key in required and required[key] != requirement:
                    raise ValueError('Deferred Case execution requirement changed: ' + key)
                required[key] = requirement
        if set(required) & GOP_KEYS:
            sources = {canonical_hash([key, case, source]): (key, case, source)
                       for key, case, source in source_cases + historical}
            gop_cases = effective_gop_cases(gop_amendment(base, candidate, git), list(sources.values()))
    results = promotion.get('results', {})
    if not isinstance(results, dict):
        raise ValueError('Candidate results must be an object matching ALL required Cases')
    if set(results) != set(required):
        raise ValueError('Candidate results must match ALL required Cases; missing=' +
                         ','.join(sorted(set(required) - set(results))) + '; extra=' +
                         ','.join(sorted(set(results) - set(required))))
    artifacts = dict(promotion.get('artifacts', {}))
    if promotion.get('artifact_sha256'):
        artifacts['app'] = promotion['artifact_sha256']
    for key, result in results.items():
        execution, artifact_name = required[key]
        artifact = artifacts.get(artifact_name)
        if not HASH.fullmatch(artifact or ''):
            raise ValueError('Fixed candidate artifact hash is required: ' + artifact_name)
        validate_candidate_result(result, key, initial_policy or full_policy, stage, candidate,
                                  artifact, execution, gop_cases.get(key), git)
    if initial_policy:
        deferred = sum(entry['initial_scope'] == 'deferred' for entry in initial_policy['cases'].values())
        return {'mode': mode, 'gui_complete': False, 'stage': INITIAL_STAGE, 'stage_complete': True,
                'full_acceptance_complete': False,
                'cases': len(required), 'initial_cases': len(required) - deferred,
                'deferred_cases': deferred, 'followup_issue': initial_policy['followup_issue'],
                'candidate': candidate}
    return {'mode': mode, 'gui_complete': True, 'cases': len(required), 'candidate': candidate}


def render_queue(paths, promotion=None, git=git_read):
    """Human view is generated from JSON; it is never a second editable status source."""
    promotion = promotion or {}
    candidate = promotion.get('candidate')
    results = promotion.get('results', {})
    rows = []
    for path in paths:
        data = json.loads(Path(path).read_text())
        validate_change(data, data['issue'], data['gui_required'])
        rows.extend((data, case) for case in data['cases'])
    initial_policy = None
    historical = []
    stage = promotion.get('stage')
    if stage is not None and (stage != INITIAL_STAGE or promotion.get('scope', 'develop') != 'develop'):
        raise ValueError('Unknown or inapplicable acceptance stage')
    if stage is not None or (candidate and promotion.get('base') and
                             git('ls-tree', promotion['base'], '--', INITIAL_PLAN)):
        initial_policy = initial_stage_plan(promotion.get('base'), candidate, git)
        historical = historical_initial_sources(initial_policy, candidate, git)
        if stage:
            for data, case in rows:
                entry = initial_policy['cases'].get(f'{data["issue"]}:{case["id"]}')
                if entry is None or canonical_hash(case) not in {
                        s['original_case_sha256'] for s in entry['sources']}:
                    raise ValueError('Current Case differs from its historical initial contract')
        else:
            carried = {key for key, entry in initial_policy['cases'].items() if entry['deferred_scope']}
            rows = [(data, case) for data, case in rows if f'{data["issue"]}:{case["id"]}' not in carried]
            for key in sorted(carried):
                entry = initial_policy['cases'][key]
                data = regular_json(initial_policy['required_ancestor'], entry['sources'][0]['source_path'], git)
                case = next(c for c in data['cases'] if c['id'] == key.split(':')[1])
                rows.append((data, case))
    gop_cases = {}
    if candidate and any(f'{data["issue"]}:{case["id"]}' in GOP_KEYS for data, case in rows):
        policy = gop_amendment(promotion.get('base'), candidate, git)
        if policy:
            # The trusted bindings, not current same-named files, select original contracts.
            source_cases = []
            for key, sources in policy['cases'].items():
                for source in sources:
                    git('merge-base', '--is-ancestor', source['source_merge'], candidate)
                    data = regular_json(source['source_merge'], source['source_path'], git)
                    validate_change(data, int(key.split(':')[0]), True)
                    matches = [c for c in data['cases'] if c['id'] == key.split(':')[1]]
                    if len(matches) != 1:
                        raise ValueError('Original GOP Case is missing: ' + key)
                    case = matches[0]
                    source_cases.append((key, case, {k: source[k] for k in
                        ('source_pr', 'source_merge', 'source_path')}))
            gop_cases = effective_gop_cases(policy, source_cases)
            for data, case in rows:
                key = f'{data["issue"]}:{case["id"]}'
                if key in gop_cases and canonical_hash(case) != policy['cases'][key][0]['original_case_sha256']:
                    raise ValueError('Current Case differs from its historical GOP contract: ' + key)
            rows = [(data, gop_cases[f'{data["issue"]}:{case["id"]}']['case'])
                    if f'{data["issue"]}:{case["id"]}' in gop_cases else (data, case) for data, case in rows]
    lines = ['# 今回の動作確認一覧', '',
             '> 自動生成。結果は正本JSONへ入力して再生成してください。過去buildの結果は参考です。', '',
             '固定候補SHA: ' + (candidate or '未固定'),
             '対象アプリ配布物 SHA-256: ' + (promotion.get('artifact_sha256') or '未登録'), '',
             '| Case / Issue / PR | 対象 | 候補結果 | main可否（Case単位） | 修正先 |',
             '|---|---|---|---|---|']
    for data, case in rows:
        key = f'{data["issue"]}:{case["id"]}'
        result = results.get(key, {})
        label = '不可・固定候補のpass未登録'
        try:
            artifacts = dict(promotion.get('artifacts', {}))
            if promotion.get('artifact_sha256'):
                artifacts['app'] = promotion['artifact_sha256']
            artifact = artifacts.get(case.get('artifact', 'app'))
            if not HASH.fullmatch(artifact or ''):
                raise ValueError('Candidate artifact not registered')
            observed = validate_candidate_result(result, key, initial_policy, stage, candidate,
                        artifact, case.get('required_execution'), gop_cases.get(key), git)
            if stage:
                label = ('延期・未実施／初期版必須外・後続#65' if observed['status'] == 'deferred' else
                         '初期版範囲合格／原Case未完了（全範囲gateは別途必要）')
            else:
                label = 'Case合格（全範囲gateは別途必要）'
            if result.get('status') == 'reused':
                label = '同一成果物の旧観察を再利用／' + label
        except ValueError as exc:
            if result.get('status') == 'reused':
                label = '不可・再利用条件不成立: ' + str(exc)
        lines.append(f'| {case["id"]} / #{data["issue"]} / #{data.get("pr", "未作成")} | {case["change"]} | '
                     f'{result.get("status", "pending")} | {label} | '
                     f'{case["fix_issue"] or "—"} / {case["fix_pr"] or "—"} |')
    for data, case in rows:
        lines += ['', f'## #{data["issue"]} / {case["id"]}: {case["change"]}', '',
                  f'PR: [#{data["pr"]}](https://github.com/shinma06/android-replay-buffer/pull/{data["pr"]})' if data.get('pr') else 'PR: 未登録', '', '前提・対象build: ' + case['preconditions'], '']
        amended = gop_cases.get(f'{data["issue"]}:{case["id"]}')
        result = results.get(f'{data["issue"]}:{case["id"]}', {})
        if result.get('status') == 'reused':
            confirmation = result.get('confirmation', {})
            lines += ['再利用申請（判定は上表）・元候補: ' + str(result.get('source_candidate', '未登録')),
                      '現在候補の適合確認: ' + json.dumps(confirmation, ensure_ascii=False),
                      '以下は元の観察です。新候補での再試験を意味しません。', '']
            result = result.get('source_result', {})
            if not isinstance(result, dict):
                result = {}
        entry = initial_policy['cases'].get(f'{data["issue"]}:{case["id"]}') if initial_policy else None
        if entry:
            lines += ['初期版範囲: ' + entry['initial_scope'],
                      '初期版計画revision: ' + canonical_hash(initial_policy),
                      ('原Case全体: 未完了。初期版の合格・延期を全体passに転記しない。' if stage else
                       '完全受入: 初期版で延期した原契約も含め、同一候補の全工程を確認する。'),
                      '初期版で実施: 以下の全出典の条件から、明記した延期部分だけを除く。',
                      '人間確認の扱い: ' + initial_policy['human_scope'],
                      '延期部分: ' + ' / '.join(entry['deferred_scope']),
                      '維持するAgent条件: ' + ' / '.join(entry['agent_requirements']),
                      '完全受入の確認者: ' + entry.get('full_acceptance_actor', '原契約の実施者（gpt / human）'),
                      f'後続Issue: #{initial_policy["followup_issue"]} / 担当: ' + initial_policy['owner'],
                      '再開条件: ' + initial_policy['resume_condition'],
                      '全固定出典: ' + ', '.join(f'PR #{s["source_pr"]} / {s["source_merge"]} / '
                                               f'{s["source_path"]} / {s["original_case_sha256"]}'
                                               for s in entry['sources']), '']
            if stage and entry['initial_scope'] == 'deferred':
                lines += ['このREAL専用Caseは全工程を延期。以下は再開用の原契約。', '']
            if stage:
                result = result.get('initial_observation', {})
            versions = {}
            for source_key, original, source in historical:
                if source_key == f'{data["issue"]}:{case["id"]}':
                    versions.setdefault(canonical_hash(original), (original, []))[1].append(source['source_pr'])
            for original, prs in versions.values():
                lines += ['固定原契約 / PR ' + ', '.join('#' + str(pr) for pr in prs),
                          '原前提: ' + original['preconditions']]
                lines += [f'原手順 {n}: {step}' for n, step in enumerate(original['steps'], 1)]
                lines += ['原期待結果: ' + original['expected'], '']
        if amended:
            proof = result.get('gop_evidence')
            if not isinstance(proof, dict):
                proof = {}
            lines += ['GOP改訂revision: ' + amended['revision'],
                      '元の前提（履歴）: ' + amended['original_preconditions'],
                      '固定出典: ' + ', '.join(f'PR #{s["source_pr"]} / {s["source_merge"]}'
                                               for s in amended['sources']),
                      '観察GOP改訂revision: ' + str(result.get('gop_revision') or '未登録')]
            lines += [f'GOP観察証拠 ({name}): {proof.get(name) or "未登録"}' for name in sorted(GOP_EVIDENCE)]
            lines.append('')
        if data.get('pr_role') == 'related_evidence_only':
            lines += ['このPRは関連証拠です。親Issueの残条件であり、当該PRのmain受入へ追加しません。', '']
        lines += [f'{n}. {step}' for n, step in enumerate(case['steps'], 1)]
        lines += ['', '期待結果: ' + case['expected'], '']
        if result:
            lines += ['今回の候補結果: ' + result.get('status', 'pending'),
                      f'確認者: {result.get("actor", "未登録")} / {result.get("observer", "未登録")}',
                      '確認日時: ' + result.get('at', '未登録'), '実施経路: ' + result.get('execution', '未登録'),
                      '観察/失敗理由: ' + result.get('reason', '未登録'),
                      '証拠: ' + result.get('evidence', '未登録'),
                      '人間工程・依存先の証拠: ' + result.get('human_evidence', '未登録'),
                      'ロード実体: ' + result.get('loaded_identity', '未登録'),
                      '対象artifact SHA-256: ' + result.get('artifact_sha256', '未登録'), '']
        for actor, label in (('gpt', 'Agent（互換キーgpt）'), ('human', '人間')):
            recorded = case[actor]
            lines += [f'初期登録時の{label}: {recorded["status"]} — {recorded["reason"]}']
        lines += ['', f'修正Issue/PR: {case["fix_issue"] or "未登録"} / {case["fix_pr"] or "未登録"}',
                  '再確認: ' + case['recheck'], '次の操作: ' + case['next_action'],
                  '根拠: ' + ', '.join(case['provenance']), '']
        if case.get('history'):
            lines += ['<details><summary>過去の観察（新候補へ転記しない）</summary>', '', '```json',
                      json.dumps(case['history'], ensure_ascii=False, indent=2), '```', '', '</details>', '']
    return '\n'.join(lines)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', nargs='*', help='Select only this batch of change JSON files')
    parser.add_argument('--batch', type=Path, help='JSON with repository-relative files and optional promotion path')
    parser.add_argument('--promotion', type=Path, help='Candidate/results JSON; observations are rendered for this build')
    parser.add_argument('--output', type=Path, help='Generated Markdown view; never edit it directly')
    args = parser.parse_args()
    batch = json.loads(args.batch.read_text()) if args.batch else {}
    files = args.files or batch.get('files', [])
    if not files:
        parser.error('Select a batch or change JSON files')
    result_path = args.promotion or batch.get('promotion')
    promotion = json.loads(Path(result_path).read_text()) if result_path else None
    rendered = render_queue(files, promotion)
    if args.output:
        args.output.write_text(rendered + '\n')
    else:
        print(rendered)
