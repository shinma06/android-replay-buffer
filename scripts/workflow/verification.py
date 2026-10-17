"""Acceptance data and fixed-candidate gates. Never execute code from a PR."""
import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess
from git_guard import BRANCH
from agent_policy import SESSION_HASH, session_fingerprint

SHA = re.compile(r'[0-9a-f]{40}')
HASH = re.compile(r'[0-9a-f]{64}')
PROMOTION = 'docs/verification/promotion.json'
BASELINE = 'docs/verification/legacy-baseline.json'
STATUSES = {'pending', 'blocked', 'fail', 'pass'}
TOOLING = ('docs/', 'scripts/', '.github/', '.githooks/', '.agents/', '.claude/skills/', '.cursor/rules/')
GOP_AMENDMENT = 'docs/verification/amendments/gop-boundaries.json'
INITIAL_STAGE = 'initial-agent'
INITIAL_PLAN = 'docs/verification/amendments/initial-agent.json'
INSTALLATION_PREVIOUS_PLAN_SHA256 = '13bd1e294519915d8c8a66be2770811a4d6b28465432061e7c2eae91a87fd8a3'
SETTINGS_PREVIOUS_PLAN_SHA256 = '6d11e3cdee0b6189c743fb1a4c8ee3a4e582d72af7e9643bb001536dd4e0c79f'
PURPOSE_PREVIOUS_PLAN_SHA256 = '993e76bb7a0bd6f9f864c2974b3ef4d4392665de51a4791c9cc0c094c394b50c'
CONSUMER_PREVIOUS_PLAN_SHA256 = '3ee1d401997722423b4db5f52c07d8edb4b6e377bc1f6f5456c66fab8460af57'
CI_PREDECESSOR = '836da534938597bd8674a63adac0925139ba3fc6'
CI_MERGE = '280c96cb2d93002152e57de7803b1eb03c8bc26b'
CI_OBSERVED_SOURCE = '707d3b0d26f749ef7537ef83a25a38081af35d62'
CI_OBSERVED_BEFORE = '101ec75e81776f0ef074c42174ce2ae27c33aa71'
CI_OBSERVED_UPDATE = '5d1d9c810b567a9dc5eb5b48f7a07a6c60ac3f6e'
CI_PATHS = {'.github/workflows/' + name for name in ('acceptance.yml', 'ci.yml', 'pr-policy.yml')}
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
    branch = BRANCH.fullmatch(pr.get('head', {}).get('ref', ''))
    if not branch or int(branch.group(2)) != issue:
        raise ValueError('Source branch and Issue metadata must match')
    if field(pr['body'], 'GUI') not in ('required', 'not-required'):
        raise ValueError('GUI must be an exact required or not-required declaration')
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
    if (not nonempty(data.get('reason')) or not isinstance(data.get('cli_checks'), list)
            or not data['cli_checks'] or not all(nonempty(x) for x in data['cli_checks'])):
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
        if not isinstance(case.get('steps'), list) or not case['steps'] or not all(nonempty(x) for x in case['steps']):
            raise ValueError('Reproducible steps are required')
        if not isinstance(case.get('provenance'), list) or not case['provenance'] or not all(nonempty(x) for x in case['provenance']):
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


def validate_timestamp(at):
    if not isinstance(at, str) or not re.fullmatch(
            r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,6})?(?:Z|[+-]\d\d:[0-5]\d)', at):
        raise ValueError('Use an ISO8601 observation timestamp')
    return datetime.fromisoformat(at.replace('Z', '+00:00'))


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
    validate_timestamp(result['at'])


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
    if (not isinstance(policy, dict) or set(policy) - {'responsibility_revision'} != {
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
    candidate_plan = regular_json(candidate, INITIAL_PLAN, git)
    original = {k: v for k, v in policy.items() if k != 'responsibility_revision'}
    revision = policy.get('responsibility_revision')
    if revision is not None:
        validate_responsibility_policy(policy)
    if candidate_plan != policy and not (revision and (
            'method_revision' in revision and candidate_plan in [p for p in
                (previous_consumer_plan(policy), previous_method_plan(policy)) if p is not None] or
            'installation_revision' in revision and
            revision.get('plan_extension', {}).get('issue') != 219 and
            candidate_plan == previous_installation_plan(policy) or
            'plan_extension' not in revision and candidate_plan == original)):
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


def previous_consumer_plan(policy):
    """Undo only Issue 230's two consumer groups to recover the registered 223 plan."""
    revision = policy.get('responsibility_revision', {})
    method = revision.get('method_revision', {})
    if not isinstance(method, dict) or method.get('issue') != 230:
        return None
    fields = {'recipient': {'purpose', 'methods', 'excluded', 'preconditions'},
              'video': {'methods', 'excluded'}}
    previous = method.get('previous_methods')
    if (not isinstance(previous, dict) or set(previous) != set(fields) or
            any(not isinstance(previous[k], dict) or set(previous[k]) != fields[k] or
                not all(nonempty(v) for v in previous[k].values()) for k in fields) or
            not isinstance(method.get('groups'), dict) or
            any(not isinstance(method['groups'].get(k), dict) for k in fields)):
        raise ValueError('Consumer revision requires the exact previous recipient/video methods')
    restored = {k: v for k, v in method.items() if k != 'previous_methods'}
    restored.update(issue=223, previous_plan_sha256=PURPOSE_PREVIOUS_PLAN_SHA256,
                    groups={k: dict(g, **previous[k]) if k in previous else g
                            for k, g in method['groups'].items()})
    return dict(policy, responsibility_revision=dict(revision, method_revision=restored))


def previous_method_plan(policy):
    policy = previous_consumer_plan(policy) or policy
    revision = policy.get('responsibility_revision', {})
    if 'method_revision' not in revision:
        return None
    return dict(policy, responsibility_revision={k: v for k, v in revision.items() if k != 'method_revision'})


def previous_settings_plan(policy):
    """Undo only Issue 219's addition to reconstruct the registered 138-Case plan."""
    policy = previous_method_plan(policy) or policy
    revision = policy.get('responsibility_revision', {})
    extension = revision.get('plan_extension', {})
    if extension.get('issue') != 219:
        return None
    key = '216:SETTINGS-WIDTH-RECOVERY'
    group = revision['groups'].get('ui-settings')
    if not isinstance(group, dict) or not isinstance(group.get('cases'), list):
        raise ValueError('Settings extension requires the existing ui-settings group')
    groups = dict(revision['groups'])
    groups['ui-settings'] = dict(group, cases=[k for k in group['cases'] if k != key])
    previous = dict(revision, plan_extension={
        'issue': 201, 'previous_required_ancestor': extension['previous_required_ancestor'],
        'added_cases': [k for k in extension['added_cases'] if k != key]},
        groups=groups)
    return dict(policy, required_ancestor='4d278e65dec0c55e71d584ff24c7f6fa407586cb',
                cases={k: v for k, v in policy['cases'].items() if k != key},
                responsibility_revision=previous)


def previous_installation_plan(policy):
    """Reconstruct only Issue 208's registered prior plan, keeping all original scopes."""
    policy = previous_settings_plan(policy) or policy
    revision = policy.get('responsibility_revision', {})
    if 'installation_revision' not in revision:
        return None
    previous = {k: v for k, v in revision.items() if k != 'installation_revision'}
    previous['overrides'] = {k: v for k, v in revision['overrides'].items() if k != '3:PLUGIN-LOAD'}
    return dict(policy, responsibility_revision=previous)


def validate_responsibility_policy(policy):
    """Issue 182 amends methods without erasing the fixed original plan or Cases."""
    revision = policy['responsibility_revision']
    original = {k: v for k, v in policy.items() if k != 'responsibility_revision'}
    if not isinstance(revision, dict):
        raise ValueError('Invalid responsibility revision')
    if 'method_revision' in revision:
        consumer_previous = previous_consumer_plan(policy)
        previous = consumer_previous or previous_method_plan(policy)
        method = revision['method_revision']
        expected_hash = CONSUMER_PREVIOUS_PLAN_SHA256 if consumer_previous else PURPOSE_PREVIOUS_PLAN_SHA256
        if (not isinstance(method, dict) or set(method) != {
                'issue', 'previous_plan_sha256', 'common_requirements', 'groups', 'overrides'} | (
                    {'previous_methods'} if consumer_previous else set()) or
                method['issue'] != (230 if consumer_previous else 223) or
                method['previous_plan_sha256'] != expected_hash or canonical_hash(previous) != expected_hash):
            raise ValueError('Purpose method revision must retain the exact prior plan')
        if (not isinstance(method['groups'], dict) or
                any(not nonempty(g.get('preconditions')) for g in method['groups'].values()
                    if isinstance(g, dict))):
            raise ValueError('Purpose methods need the actual group preconditions')
        validate_responsibility_policy(previous)
        # Reuse the existing complete-group and bounded-override validation.
        effective = dict(previous['responsibility_revision'], **{k: method[k] for k in
                         ('common_requirements', 'groups', 'overrides')})
        effective['groups'] = {k: {name: value for name, value in group.items() if name != 'preconditions'}
                               if isinstance(group, dict) else group for k, group in method['groups'].items()}
        effective.pop('method_revision', None)
        effective.pop('installation_revision', None)
        effective.pop('plan_extension', None)
        original = {k: v for k, v in previous.items() if k != 'responsibility_revision'}
        effective['previous_plan_sha256'] = canonical_hash(original)
        validate_responsibility_policy(dict(previous, responsibility_revision=effective))
        return
    extension = revision.get('plan_extension')
    if 'plan_extension' in revision:
        if (not isinstance(extension, dict) or set(extension) != {
                'issue', 'previous_required_ancestor', 'added_cases'} | (
                    {'previous_plan_sha256'} if extension.get('issue') == 219 else set()) or
                extension['issue'] not in (190, 201, 219) or extension['added_cases'] != {
                    190: ['186:CLOCK-BATCH-SAVE'],
                    201: ['186:CLOCK-BATCH-SAVE', '197:SAVE-FAILURE-OWNERSHIP',
                          '198:LOCAL-OWNERSHIP-RECOVERY'],
                    219: ['186:CLOCK-BATCH-SAVE', '197:SAVE-FAILURE-OWNERSHIP',
                          '198:LOCAL-OWNERSHIP-RECOVERY', '216:SETTINGS-WIDTH-RECOVERY']}[extension['issue']] or
                not isinstance(extension['previous_required_ancestor'], str) or
                not SHA.fullmatch(extension['previous_required_ancestor']) or
                any(key not in policy['cases'] for key in extension['added_cases'])):
            raise ValueError('Invalid responsibility plan extension')
        # Bind the unchanged original plan; do not relabel its historical hash.
        original = dict(original, required_ancestor=extension['previous_required_ancestor'],
                        cases={k: v for k, v in original['cases'].items()
                               if k not in extension['added_cases']})
    if (set(revision) - {'plan_extension', 'installation_revision'} != {
            'issue', 'previous_plan_sha256', 'common_requirements', 'groups', 'overrides'} or
            revision['issue'] != 182 or revision['previous_plan_sha256'] != canonical_hash(original) or
            not isinstance(revision['common_requirements'], list) or not revision['common_requirements'] or
            not all(nonempty(x) for x in revision['common_requirements']) or
            not isinstance(revision['groups'], dict) or not revision['groups'] or
            not isinstance(revision['overrides'], dict)):
        raise ValueError('Invalid responsibility revision or original plan binding')
    if 'installation_revision' in revision:
        installation = revision['installation_revision']
        if (not isinstance(installation, dict) or set(installation) != {'issue', 'previous_plan_sha256'} or
                installation['issue'] != 208 or '3:PLUGIN-LOAD' not in revision['overrides'] or
                installation['previous_plan_sha256'] != INSTALLATION_PREVIOUS_PLAN_SHA256 or
                canonical_hash(previous_installation_plan(policy)) != INSTALLATION_PREVIOUS_PLAN_SHA256):
            raise ValueError('Installation method revision must retain the exact prior plan')
    if extension and extension['issue'] == 219:
        if (extension['previous_plan_sha256'] != SETTINGS_PREVIOUS_PLAN_SHA256 or
                canonical_hash(previous_settings_plan(policy)) != SETTINGS_PREVIOUS_PLAN_SHA256):
            raise ValueError('Settings extension must retain the exact registered 138-Case plan')
    keys = []
    for group in revision['groups'].values():
        if (not isinstance(group, dict) or set(group) != {
                'cases', 'disposition', 'purpose', 'methods', 'excluded', 'evidence_keys'} or
                group['disposition'] not in ('維持', '統合', '置換') or
                any(not nonempty(group[k]) for k in ('purpose', 'methods', 'excluded')) or
                any(not isinstance(group[k], list) or not group[k] or
                    not all(nonempty(x) for x in group[k]) for k in ('cases', 'evidence_keys')) or
                len(set(group['evidence_keys'])) != len(group['evidence_keys'])):
            raise ValueError('Invalid responsibility group')
        keys.extend(group['cases'])
    if len(keys) != len(set(keys)) or set(keys) != set(policy['cases']):
        raise ValueError('Responsibility groups must cover every original Case exactly once')
    for key, override in revision['overrides'].items():
        if (key not in policy['cases'] or not isinstance(override, dict) or not override or
                set(override) - {'case', 'agent_requirements', 'deferred_scope', 'full_acceptance_actor'}):
            raise ValueError('Invalid responsibility override')
        for name in ('agent_requirements', 'deferred_scope'):
            if name in override and (not isinstance(override[name], list) or
                                     not all(nonempty(x) for x in override[name])):
                raise ValueError('Invalid revised Case scope')
        if ('full_acceptance_actor' in override and
                (override['full_acceptance_actor'] is not None or
                 policy['cases'][key].get('full_acceptance_actor') != 'human')):
            raise ValueError('Only the original blanket human requirement can be superseded')
        case = override.get('case', {})
        if (not isinstance(case, dict) or set(case) - {'preconditions', 'steps', 'expected'} or
                any(not nonempty(v) for k, v in case.items() if k != 'steps') or
                ('steps' in case and (not isinstance(case['steps'], list) or not case['steps'] or
                                      not all(nonempty(x) for x in case['steps'])))):
            raise ValueError('Invalid effective responsibility Case')
        if key.endswith('-REAL') and override.get('deferred_scope', policy['cases'][key]['deferred_scope']) != policy['cases'][key]['deferred_scope']:
            raise ValueError('REAL deferrals must remain intact')


def responsibility_group(policy, key):
    revision = (policy or {}).get('responsibility_revision')
    if revision and key in policy['cases']:
        revision = revision.get('method_revision', revision)
        return next(g for g in revision['groups'].values() if key in g['cases'])
    return None


def effective_initial_entry(policy, key):
    entry = dict(policy['cases'][key])
    override = policy.get('responsibility_revision', {}).get('overrides', {}).get(key, {})
    entry.update({k: v for k, v in override.items() if k != 'case'})
    method = policy.get('responsibility_revision', {}).get('method_revision')
    if method:
        entry['agent_requirements'] = [
            '有効なCase固有expectedの保証を満たす証拠を対応付ける。groupの方法は分担の集合であり、本Caseに無関係な保証/工程を新しい依存にしない。']
        entry.update({k: v for k, v in method['overrides'].get(key, {}).items() if k != 'case'})
    if entry.get('full_acceptance_actor') is None:
        entry.pop('full_acceptance_actor', None)
    return entry


def effective_responsibility_case(policy, key, case):
    revised = dict(case)
    group = responsibility_group(policy, key)
    if group:
        revision = policy['responsibility_revision']
        # An old method amendment cannot erase conditions added by a later Case.
        if canonical_hash(case) == policy['cases'][key]['current_case_sha256']:
            revised.update(revision['overrides'].get(key, {}).get('case', {}))
            if 'method_revision' in revision:
                revision = revision['method_revision']
                revised['preconditions'] = group['preconditions']
                revised.update(revision['overrides'].get(key, {}).get('case', {}))
                revised['steps'] = ['目的群（本Case固有expectedに該当する保証を確認）: ' + group['purpose'],
                    '分担の集合（本Caseの保証に必要な方法だけを適用）: ' + group['methods'],
                    'Case固有の期待結果を全て照合し、同条件の既存証拠と不足を分けて記録する。']
        elif 'method_revision' in revision:
            # A new product contract must not inherit this fixed method replacement.
            return revised
        revised['steps'] = [*revised['steps'], '有効な実施分担: ' + group['methods']]
        revised['expected'] += ' ' + ' '.join(revision['common_requirements'])
    return revised


def validate_responsibility_observation(observed, key, policy):
    group = responsibility_group(policy, key)
    if group:
        evidence = observed.get('responsibility_evidence')
        required = {'contract_coverage', 'artifact_binding', 'failure_attribution', *group['evidence_keys']}
        if (observed.get('responsibility_revision') != canonical_hash(policy['responsibility_revision']) or
                not isinstance(evidence, dict) or set(evidence) != required or
                not all(nonempty(x) for x in evidence.values())):
            raise ValueError('Revised responsibility coverage and exact evidence are required: ' + key)


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
    entry = effective_initial_entry(policy, key)
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
        validate_responsibility_observation(observed, key, policy)
        if not policy.get('responsibility_revision', {}).get('method_revision'):
            validate_gop_observation(observed, gop)
        if execution and observed.get('execution') != execution:
            raise ValueError('Case requires its specified execution method: ' + key)


def validate_management_ci_transition(source, commit, parents, candidate, trusted_base, git):
    """Match PR207's exact inputs on the single squash candidate following observed 707."""
    if not trusted_base or not SHA.fullmatch(trusted_base):
        raise ValueError('Management CI transition requires a fixed trusted main base')
    git('merge-base', '--is-ancestor', CI_MERGE, trusted_base)
    for incoming in git('rev-list', f'{CI_MERGE}..{trusted_base}').splitlines():
        for parent in git('rev-list', '--parents', '-n', '1', incoming).split()[1:]:
            if any(path.startswith('.github/workflows/') for path in
                   git('diff', '--no-renames', '--name-only', parent, incoming).splitlines()):
                raise ValueError('Trusted main has another CI change, even if reverted; retesting required')
    if git('rev-list', '--parents', '-n', '1', CI_MERGE).split() != [CI_MERGE, CI_PREDECESSOR]:
        raise ValueError('Management CI transition has a different fixed source history')
    if source != CI_OBSERVED_SOURCE or parents != [source] or commit != candidate:
        raise ValueError('Management CI change is not the registered source-to-squash transition')
    for path in CI_PATHS:
        before = git('ls-tree', CI_PREDECESSOR, '--', path).split()
        after = git('ls-tree', CI_MERGE, '--', path).split()
        if any(len(entry) != 4 or entry[:2] != ['100644', 'blob'] or entry[3] != path
               for entry in (before, after)) or before == after:
            raise ValueError('Management CI transition lacks complete regular-file proof')
        if path == '.github/workflows/ci.yml':
            # Keep 707's Java/Gradle/Plugin CI; this blob changes only its Python selection.
            before = ['100644', 'blob', CI_OBSERVED_BEFORE, path]
            after = ['100644', 'blob', CI_OBSERVED_UPDATE, path]
        if (git('ls-tree', source, '--', path).split() != before or
                git('ls-tree', candidate, '--', path).split() != after):
            raise ValueError('Management CI content or mode differs; retesting required')


def unchanged_artifact_history(source, candidate, key, git, trusted_base=None):
    """Share the strict history/Case check across reuse and retained-fact assessment."""
    git('merge-base', '--is-ancestor', source, candidate)
    path = f'docs/verification/changes/issue-{key.split(":")[0]}.json'
    allowed = ('docs/', 'scripts/workflow/', '.agents/skills/', '.claude/skills/', '.cursor/rules/')
    for commit in git('rev-list', f'{source}..{candidate}').splitlines():
        parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
        if not parents:
            raise ValueError('Reuse history lacks a parent')
        source_changes = (set(git('diff', '--no-renames', '--name-only', source, commit).splitlines())
                          if len(parents) > 1 else None)
        for parent in parents:
            paths = git('diff', '--no-renames', '--name-only', parent, commit).splitlines()
            # A sync merge may carry already-observed product files absent from its other parent.
            if source_changes is not None:
                paths = [p for p in paths if p in source_changes]
            if path in paths:
                raise ValueError('Case contract history changed; retest required')
            ci_paths = set(paths) & CI_PATHS
            if ci_paths:
                validate_management_ci_transition(source, commit, parents, candidate, trusted_base, git)
            if any(not (p.startswith(allowed) or p in
                    ('AGENTS.md', 'CLAUDE.md', 'README.md', 'CONTRIBUTING.md') or p in ci_paths) for p in paths):
                raise ValueError('Product, build, configuration, fixture or unknown change requires retesting')
    old_change = regular_json(source, path, git)
    new_change = regular_json(candidate, path, git)
    for change in (old_change, new_change):
        validate_change(change, int(key.split(':')[0]), True)
    old_cases = [c for c in old_change['cases'] if c['id'] == key.split(':')[1]]
    new_cases = [c for c in new_change['cases'] if c['id'] == key.split(':')[1]]
    if len(old_cases) != 1 or old_cases != new_cases:
        raise ValueError('Reuse requires the same existing Case contract')


IMPACT_DIMENSIONS = {'source', 'callers', 'callees', 'dataflow', 'build', 'configuration',
                     'dependencies', 'fixtures', 'runtime', 'display', 'failure_attribution'}


def load_impact_source(source, git):
    """A public pre-squash commit may be absent from a fresh acceptance checkout."""
    if not isinstance(source, str) or not SHA.fullmatch(source):
        raise ValueError('Impact source must be a full immutable commit SHA')
    try:
        kind = git('cat-file', '-t', source)
    except subprocess.CalledProcessError:
        origin = git('remote', 'get-url', 'origin')
        if not re.fullmatch(r'(https://github\.com/|git@github\.com:)shinma06/android-replay-buffer(?:\.git)?', origin):
            raise ValueError('Missing impact source requires this repository trusted origin')
        git('fetch', '--no-tags', 'origin', source)
        kind = git('cat-file', '-t', source)
    if kind != 'commit':
        raise ValueError('Impact source object must be the exact requested commit')


def impact_history(source, candidate, key, git):
    """Bind all relevant history edges, including edits later reverted."""
    load_impact_source(source, git)
    try:
        git('merge-base', '--is-ancestor', source, candidate)
        commits = git('rev-list', f'{source}..{candidate}').splitlines()
    except subprocess.CalledProcessError:
        # A pre-squash observation can be a direct sibling with identical product inputs.
        parents = [git('rev-list', '--parents', '-n', '1', ref).split()[1:] for ref in (source, candidate)]
        if len(parents[0]) != 1 or parents[0] != parents[1]:
            raise ValueError('Nonancestor reuse requires direct single-parent sibling lineage')
        for path in git('diff', '--no-renames', '--name-only', source, candidate).splitlines():
            if not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', path):
                raise ValueError('Sibling reuse requires identical product/build/fixture inputs')
            for ref in (source, candidate):
                if git('ls-tree', ref, '--', path):
                    record = regular_json(ref, path, git)
                    validate_change(record, int(Path(path).stem.removeprefix('issue-')), False)
        commits = [source, candidate]
    case_path = f'docs/verification/changes/issue-{key.split(":")[0]}.json'
    edges = []
    for commit in commits:
        parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
        if not parents:
            raise ValueError('Impact reuse history lacks a parent')
        source_changes = (set(git('diff', '--no-renames', '--name-only', source, commit).splitlines())
                          if len(parents) > 1 else None)
        for parent in parents:
            paths = git('diff', '--no-renames', '--name-only', parent, commit).splitlines()
            if source_changes is not None:
                paths = [p for p in paths if p in source_changes]
            if case_path in paths:
                raise ValueError('Impact reuse cannot replace a changed or reverted Case contract')
            if paths:
                edges.append({'commit': commit, 'parent': parent, 'paths': sorted(paths)})
    old = regular_json(source, case_path, git)
    current = regular_json(candidate, case_path, git)
    for change in (old, current):
        validate_change(change, int(key.split(':')[0]), True)
    cases = [[c for c in change['cases'] if c['id'] == key.split(':')[1]] for change in (old, current)]
    if len(cases[0]) != 1 or cases[0] != cases[1]:
        raise ValueError('Impact reuse requires the same existing Case contract')
    return sorted(edges, key=lambda e: (e['commit'], e['parent'])), canonical_hash(cases[0][0])


def certificate_review(certificate, receipts=None):
    """Public fingerprint/receipt structure; actual origins are checked by the approval adapter."""
    review = certificate.get('review', {})
    provenance = certificate.get('provenance')
    if (not isinstance(review, dict) or review.get('status') != 'approved' or
            not all(isinstance(review.get(k), str) and SESSION_HASH.fullmatch(review[k])
                    for k in ('reviewer_session', 'writer_session')) or
            not all(nonempty(review.get(k)) for k in ('at', 'evidence')) or
            not isinstance(review.get('executor_sessions'), list) or not review['executor_sessions'] or
            not all(isinstance(s, str) and SESSION_HASH.fullmatch(s) for s in review['executor_sessions']) or
            len(review['executor_sessions']) != len(set(review['executor_sessions'])) or
            not isinstance(provenance, dict) or not nonempty(provenance.get('at')) or
            not isinstance(provenance.get('attribution'), dict)):
        raise ValueError('Impact certificate requires canonical session fingerprints and sealed originating receipts')
    reviewed_at = validate_timestamp(review['at'])
    assessed_at = validate_timestamp(provenance['at'])
    # Use the same complete contributor/receipt claim checks as original-NG evidence.
    contributors = repair_contributors(provenance, '', datetime.min.replace(tzinfo=reviewed_at.tzinfo), receipts)
    execution = {c['session'] for c in provenance['attribution']['contributors'] if c['activity'] == 'execution'}
    if (review['writer_session'] != provenance['attribution']['writer'] or
            set(review['executor_sessions']) != execution or review['reviewer_session'] in contributors or
            assessed_at > reviewed_at):
        raise ValueError('Impact certificate reviewer/writer/executors conflict with complete originating attribution')
    return contributors


def validate_certificate_guarantees(certificate, coverage, git, facts=None):
    candidate, artifact, history = certificate['candidate'], certificate['artifact_sha256'], certificate['history']
    guarantees = certificate.get('guarantees')
    if not isinstance(guarantees, dict) or set(guarantees) != set(coverage):
        raise ValueError('Impact certificate needs every effective guarantee')
    for name, proof in guarantees.items():
        if (not isinstance(proof, dict) or proof.get('facts') != coverage[name] or
                proof.get('disposition') not in ('unaffected', 'equivalent', 'fresh') or
                not isinstance(proof.get('impact'), dict) or set(proof['impact']) != IMPACT_DIMENSIONS or
                not all(nonempty(v) for v in proof['impact'].values())):
            raise ValueError('Unknown or unexplained guarantee impact: ' + name)
        selected = [facts.get(i, {}) for i in proof['facts']] if facts is not None else []
        if facts is not None and not all(isinstance(f, dict) for f in selected):
            raise ValueError('Impact coverage must reference actual successful facts')
        if name == 'artifact_binding' or proof['disposition'] == 'fresh':
            if any(f.get('head') != candidate or f.get('artifact_sha256') != artifact for f in selected):
                raise ValueError('Current identity or affected guarantee requires fresh current-build facts')
        else:
            components = proof.get('components')
            if not isinstance(components, dict) or not components:
                raise ValueError('Retained guarantees require concrete relevant components')
            for path, binding in components.items():
                if (not isinstance(path, str) or path.startswith('/') or '..' in path.split('/') or
                        not isinstance(binding, dict)):
                    raise ValueError('Invalid relevant component binding')
                old = git('ls-tree', certificate['source_candidate'], '--', path).split()
                current = git('ls-tree', candidate, '--', path).split()
                if (any(len(tree) != 4 or tree[0] not in ('100644', '100755') or tree[1] != 'blob' or tree[3] != path
                        for tree in (old, current)) or binding.get('source_blob') != old[2] or
                        binding.get('candidate_blob') != current[2]):
                    raise ValueError('Relevant component proof differs from fixed source/build inputs')
                touched = any(path in e['paths'] for e in history)
                if old != current or touched:
                    if proof['disposition'] != 'equivalent' or not nonempty(binding.get('equivalence_evidence')):
                        raise ValueError('Changed or reverted relevant component needs reviewed semantic equivalence')



def validate_certificate(certificate, git, trusted_base=None):
    if (not isinstance(certificate, dict) or certificate.get('schema') != 1 or
            not isinstance(certificate.get('key'), str) or
            not re.fullmatch(r'[1-9][0-9]*:[A-Z][A-Z0-9-]+', certificate['key']) or
            any(not isinstance(certificate.get(k), str) or not SHA.fullmatch(certificate[k])
                for k in ('candidate', 'source_candidate')) or
            certificate['candidate'] == certificate['source_candidate'] or
            any(not isinstance(certificate.get(k), str) or not HASH.fullmatch(certificate[k])
                for k in ('artifact_sha256', 'source_artifact_sha256', 'case_sha256', 'plan_sha256',
                          'original_file_sha256', 'original_result_sha256', 'assessment_sha256', 'observation_sha256')) or
            not nonempty(certificate.get('original_record_evidence')) or
            not isinstance(certificate.get('original_fact_sha256'), dict) or not certificate['original_fact_sha256'] or
            any(not nonempty(k) or not isinstance(h, str) or not HASH.fullmatch(h)
                for k, h in certificate['original_fact_sha256'].items()) or
            not isinstance(certificate.get('guarantees'), dict)):
        raise ValueError('Certificate registration requires the complete original/build/contract/hash structure')
    key, source, candidate = certificate['key'], certificate['source_candidate'], certificate['candidate']
    try:
        history, case_hash = impact_history(source, candidate, key, git)
        policy = initial_stage_plan(trusted_base or candidate, candidate, git)
        if (history != certificate.get('history') or case_hash != certificate['case_sha256'] or
                initial_stage_plan(source, source, git) != policy or canonical_hash(policy) != certificate['plan_sha256']):
            raise ValueError('Certificate registration differs from fixed Git history/Case/plan')
    except subprocess.CalledProcessError as error:
        raise ValueError('Certificate registration Git identity/history is unavailable') from error
    group = responsibility_group(policy, key)
    required = {'contract_coverage', 'artifact_binding', 'failure_attribution', *group['evidence_keys']} if group else set()
    if set(certificate['guarantees']) != required or not required:
        raise ValueError('Certificate registration needs all effective guarantees')
    coverage = {}
    for name, proof in certificate['guarantees'].items():
        ids = proof.get('facts') if isinstance(proof, dict) else None
        if (not isinstance(ids, list) or not ids or not all(nonempty(i) for i in ids) or
                len(ids) != len(set(ids)) or name == 'artifact_binding' and proof.get('disposition') != 'fresh'):
            raise ValueError('Certificate registration needs explicit unique fact coverage and fresh identity')
        if (name == 'artifact_binding' or proof.get('disposition') == 'fresh') and set(ids) & set(certificate['original_fact_sha256']):
            raise ValueError('Fresh certificate guarantees cannot use retained old fact identities')
        coverage[name] = ids
    if not set(certificate['original_fact_sha256']) <= set().union(*map(set, coverage.values())):
        raise ValueError('Certificate registration contains unbound original facts')
    validate_certificate_guarantees(certificate, coverage, git)

def certificate_acceptance(certificates, git=git_read, trusted_base=None):
    if not isinstance(certificates, dict) or not certificates or not all(nonempty(k) for k in certificates):
        raise ValueError('Malformed Issue evidence certificates')
    contributors, reviewers, receipts = set(), set(), {}
    for certificate in certificates.values():
        validate_certificate(certificate, git, trusted_base)
        contributors.update(certificate_review(certificate, receipts))
        fixed_at = datetime.fromisoformat(git('show', '-s', '--format=%cI', certificate['candidate']))
        assessed_at = datetime.fromisoformat(certificate['provenance']['at'].replace('Z', '+00:00'))
        reviewed_at = datetime.fromisoformat(certificate['review']['at'].replace('Z', '+00:00'))
        if fixed_at.tzinfo is None or assessed_at < fixed_at or reviewed_at < assessed_at:
            raise ValueError('Certificate assessment/review predates the fixed candidate or assessment')
        reviewers.add(certificate['review']['reviewer_session'])
    if len(reviewers) != 1 or reviewers & contributors:
        raise ValueError('Certificate registration needs one actual independent reviewer for its complete scope')
    return {'certificate_revision': canonical_hash(certificates), 'certificate_observers': sorted(contributors),
            'certificate_reviewers': sorted(reviewers),
            'certificate_provenance': {k: c['provenance'] for k, c in certificates.items()}}


def validate_impact_reuse(observed, assessment, key, policy, candidate, artifact, git, trusted_base):
    """Trust an independently reviewed, public-safe certificate, never a writer's assertion."""
    ref = assessment['impact_reuse']
    if (not isinstance(ref, dict) or set(ref) != {'commit', 'path', 'id', 'sha256'} or
            not isinstance(ref.get('commit'), str) or not SHA.fullmatch(ref['commit']) or
            not isinstance(ref.get('sha256'), str) or not HASH.fullmatch(ref['sha256']) or
            not nonempty(ref.get('id')) or
            not isinstance(ref.get('path'), str) or
            not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', ref['path']) or
            not trusted_base):
        raise ValueError('Impact reuse requires a fixed trusted-main Issue certificate')
    try:
        git('merge-base', '--is-ancestor', ref['commit'], trusted_base)
        record = regular_json(ref['commit'], ref['path'], git)
        validate_change(record, int(Path(ref['path']).stem.removeprefix('issue-')), record.get('gui_required'))
        certificates = record.get('evidence_certificates')
        if not isinstance(certificates, dict):
            raise ValueError('Trusted Issue has no evidence certificates')
        certificate = certificates.get(ref['id'])
        history, case_hash = impact_history(assessment['source_candidate'], candidate, key, git)
        source_plan = initial_stage_plan(assessment['source_candidate'], assessment['source_candidate'], git)
    except subprocess.CalledProcessError as exc:
        raise ValueError('Impact certificate provenance or source history could not be verified') from exc
    payload = {k: v for k, v in assessment.items() if k != 'impact_reuse'}
    source_artifact = assessment['source_result'].get('artifact_sha256')
    if (not isinstance(certificate, dict) or canonical_hash(certificate) != ref['sha256'] or
            certificate.get('schema') != 1 or certificate.get('key') != key or
            certificate.get('candidate') != candidate or certificate.get('artifact_sha256') != artifact or
            certificate.get('source_candidate') != assessment['source_candidate'] or
            certificate.get('source_artifact_sha256') != source_artifact or not HASH.fullmatch(source_artifact or '') or
            certificate.get('assessment_sha256') != canonical_hash(payload) or
            certificate.get('observation_sha256') != canonical_hash({k: v for k, v in observed.items()
                                                                    if k != 'responsibility_assessment'}) or
            assessment['source_candidate'] == candidate or
            certificate.get('case_sha256') != case_hash or
            certificate.get('plan_sha256') != canonical_hash(policy) or source_plan != policy or
            certificate.get('history') != history or
            not isinstance(certificate.get('original_file_sha256'), str) or
            not HASH.fullmatch(certificate['original_file_sha256']) or
            not isinstance(certificate.get('original_result_sha256'), str) or
            not HASH.fullmatch(certificate['original_result_sha256']) or
            not nonempty(certificate.get('original_record_evidence'))):
        raise ValueError('Impact certificate must bind exact original proof, history, contract, plan and both builds')
    old_facts = {name for name, fact in assessment['facts'].items()
                 if isinstance(fact, dict) and fact.get('head') == assessment['source_candidate']}
    originals = certificate.get('original_fact_sha256')
    if (not old_facts or not isinstance(originals, dict) or set(originals) != old_facts or
            not all(isinstance(h, str) and HASH.fullmatch(h) for h in originals.values())):
        raise ValueError('Every retained fact needs independently verified immutable original provenance')
    certificate_review(certificate)
    review = certificate['review']
    validate_observation(dict(observed, at=review['at']), candidate, artifact)
    fixed_at = datetime.fromisoformat(git('show', '-s', '--format=%cI', candidate))
    assessed_at = datetime.fromisoformat(observed['at'].replace('Z', '+00:00'))
    reviewed_at = datetime.fromisoformat(review['at'].replace('Z', '+00:00'))
    if fixed_at.tzinfo is None or assessed_at < fixed_at or reviewed_at < assessed_at:
        raise ValueError('Impact assessment/review predates the fixed candidate or assessment')
    validate_certificate_guarantees(certificate, assessment['coverage'], git, assessment['facts'])
    source_result = assessment['source_result']
    original_observation = (source_result.get('initial_observation', {})
                            if source_result['status'] == 'initial-pass' else source_result)
    if not isinstance(original_observation, dict):
        raise ValueError('Original successful observation missing')
    original_assessment = original_observation.get('responsibility_assessment') or {}
    if not isinstance(original_assessment, dict) or original_assessment.get('impact_reuse'):
        raise ValueError('Impact reuse chains cannot create an original successful execution')
    if source_result['status'] in ('pass', 'initial-pass'):
        execution = regular_json(assessment['source_candidate'],
            f'docs/verification/changes/issue-{key.split(":")[0]}.json', git)
        execution = next(c.get('required_execution') for c in execution['cases'] if c['id'] == key.split(':')[1])
        source_gop = None
        if key in GOP_KEYS and not policy['responsibility_revision'].get('method_revision'):
            amendment = gop_amendment(assessment['source_candidate'], assessment['source_candidate'], git)
            source_gop = {'revision': canonical_hash(amendment)} if amendment else None
        if source_result['status'] == 'initial-pass':
            validate_initial_result(source_result, key, source_plan, assessment['source_candidate'], source_artifact, execution, source_gop)
        else:
            validate_full_result(source_result, assessment['source_candidate'], source_artifact, execution, source_gop,
                                 effective_initial_entry(source_plan, key))
            validate_responsibility_observation(source_result, key, source_plan)
        validate_fact_assessment(original_observation, key, source_plan, assessment['source_candidate'],
                                 source_artifact, git, trusted_base, allow_impact=False)
    return source_artifact


def validate_fact_assessment(observed, key, policy, candidate, artifact, git, trusted_base=None, allow_impact=True):
    assessment = observed.get('responsibility_assessment')
    if assessment is None:
        return
    if isinstance(assessment, dict) and 'impact_reuse' in assessment and not allow_impact:
        raise ValueError('Nested impact reuse is not supported; retain original successful facts directly')
    group = responsibility_group(policy, key)
    if (not group or not isinstance(assessment, dict) or set(assessment) != {
            'source_candidate', 'source_result', 'source_result_sha256', 'source_record_evidence',
            'facts', 'coverage', 'missing_requirements', 'unresolved_owned_failures'} |
            ({'impact_reuse'} if 'impact_reuse' in assessment else set()) or
            not isinstance(assessment['source_candidate'], str) or
            not SHA.fullmatch(assessment['source_candidate']) or
            not isinstance(assessment['source_result'], dict) or
            assessment['source_result'].get('status') not in ('pending', 'blocked', 'fail', 'pass', 'initial-pass') or
            assessment['source_result_sha256'] != canonical_hash(assessment['source_result']) or
            not nonempty(assessment['source_record_evidence']) or
            not isinstance(assessment['facts'], dict) or not assessment['facts'] or
            not all(nonempty(x) for x in assessment['facts']) or
            not isinstance(assessment['coverage'], dict) or
            set(assessment['coverage']) != set(observed['responsibility_evidence']) or
            assessment['missing_requirements'] != [] or assessment['unresolved_owned_failures'] != []):
        raise ValueError('Retained-fact assessment needs original record, complete coverage and no unresolved owned failure')
    for facts in assessment['coverage'].values():
        if (not isinstance(facts, list) or not facts or not all(isinstance(x, str) for x in facts) or
                len(facts) != len(set(facts)) or set(facts) - set(assessment['facts'])):
            raise ValueError('Each effective condition needs identified passing facts')
    source = assessment['source_candidate']
    impact = 'impact_reuse' in assessment
    source_artifact = (validate_impact_reuse(observed, assessment, key, policy, candidate, artifact, git, trusted_base)
                       if impact else artifact)
    if (assessment['source_result'].get('head', source) != source or
            assessment['source_result'].get('artifact_sha256', source_artifact) != source_artifact):
        raise ValueError('Original aggregate record has a different identity')
    try:
        if not impact:
            unchanged_artifact_history(source, candidate, key, git, trusted_base)
        original_policy = initial_stage_plan(source, source, git)
    except subprocess.CalledProcessError as exc:
        raise ValueError('Retained-fact source or history could not be verified') from exc
    if canonical_hash(original_policy) not in {
            policy['responsibility_revision']['previous_plan_sha256'], canonical_hash(policy),
            policy['responsibility_revision'].get('installation_revision', {}).get('previous_plan_sha256'),
            policy['responsibility_revision'].get('method_revision', {}).get('previous_plan_sha256'),
            PURPOSE_PREVIOUS_PLAN_SHA256 if previous_consumer_plan(policy) else None,
            SETTINGS_PREVIOUS_PLAN_SHA256 if previous_settings_plan(policy) else None}:
        raise ValueError('Retained facts must bind the exact original or trusted current responsibility plan')
    for fact in assessment['facts'].values():
        if not isinstance(fact, dict):
            raise ValueError('Retained fact must be an actual successful observation')
        fact_head = fact.get('head') if impact else source
        if impact and fact_head not in (source, candidate):
            raise ValueError('Impact facts must bind the original or current fixed candidate')
        validate_observation(fact, fact_head, artifact if fact_head == candidate else source_artifact)
        if impact and fact_head == candidate:
            fixed_at = datetime.fromisoformat(git('show', '-s', '--format=%cI', candidate))
            if datetime.fromisoformat(fact['at'].replace('Z', '+00:00')) < fixed_at:
                raise ValueError('Current-build fact predates the fixed candidate')
        if datetime.fromisoformat(fact['at'].replace('Z', '+00:00')) > datetime.fromisoformat(observed['at'].replace('Z', '+00:00')):
            raise ValueError('Assessment predates its retained facts')
        validate_fact_assessment(fact, key, policy if fact_head == candidate else original_policy, fact_head,
                                 artifact if fact_head == candidate else source_artifact, git, trusted_base, allow_impact=False)


def validate_candidate_result(result, key, policy, stage, candidate, artifact, execution, gop, git,
                              trusted_base=None, case=None):
    """Keep old observations intact; accept reuse only with fresh equivalence evidence."""
    if not isinstance(result, dict):
        raise ValueError('Case result must be an object')
    source = candidate
    source_policy = policy
    reassessed = result.get('status') == 'reassessed'
    if result.get('status') in ('reused', 'reassessed'):
        if (set(result) != {'status', 'head', 'artifact_sha256', 'source_candidate', 'source_record',
                           'source_result', 'confirmation'} or
                result['head'] != candidate or result['artifact_sha256'] != artifact or
                not isinstance(result['source_candidate'], str) or
                not SHA.fullmatch(result['source_candidate']) or
                (result['source_candidate'] == candidate and not reassessed)):
            raise ValueError('Reuse requires distinct fixed source and current candidate/build')
        source = result['source_candidate']
        original, confirmation = result['source_result'], result['confirmation']
        if (not isinstance(original, dict) or original.get('status') != ('initial-pass' if stage else 'pass') or
                not isinstance(confirmation, dict)):
            raise ValueError('Only original successful observations can be reused; no chains or deferrals')
        record = result['source_record']
        if not isinstance(record, str) or not SHA.fullmatch(record) or not trusted_base:
            raise ValueError('Reuse requires a fixed original promotion record in trusted main')
        try:
            git('merge-base', '--is-ancestor', record, trusted_base)
            stored = regular_json(record, PROMOTION, git)
        except subprocess.CalledProcessError as exc:
            raise ValueError('Original promotion record is not in trusted main') from exc
        if (not isinstance(stored, dict) or stored.get('schema') != 1 or stored.get('candidate') != source or
                not isinstance(stored.get('results'), dict) or stored['results'].get(key) != original):
            raise ValueError('Embedded result differs from the fixed original promotion record')
        validate_observation(confirmation, candidate, artifact)
        fixed_at = datetime.fromisoformat(git('show', '-s', '--format=%cI', candidate))
        if fixed_at.tzinfo is None or datetime.fromisoformat(confirmation['at'].replace('Z', '+00:00')) < fixed_at:
            raise ValueError('Reuse confirmation predates the fixed candidate commit')
        for name in ('equivalence_evidence', 'environment_evidence'):
            if not nonempty(confirmation.get(name)):
                raise ValueError('Reuse needs current loaded/environment/condition evidence: ' + name)
        if reassessed:
            if not responsibility_group(policy, key):
                raise ValueError('Reassessment requires a trusted responsibility revision')
            validate_responsibility_observation(confirmation, key, policy)
        # A reverted product or contract edit also invalidates reuse.
        try:
            unchanged_artifact_history(source, candidate, key, git, trusted_base)
            entry = policy['cases'].get(key) if policy else None
            if entry or git('ls-tree', source, '--', INITIAL_PLAN):
                source_policy = initial_stage_plan(source, source, git)
                if reassessed:
                    if canonical_hash(source_policy) not in {
                            policy['responsibility_revision']['previous_plan_sha256'],
                            policy['responsibility_revision'].get('installation_revision', {}).get('previous_plan_sha256'),
                            policy['responsibility_revision'].get('method_revision', {}).get('previous_plan_sha256'),
                            PURPOSE_PREVIOUS_PLAN_SHA256 if previous_consumer_plan(policy) else None,
                            SETTINGS_PREVIOUS_PLAN_SHA256 if previous_settings_plan(policy) else None}:
                        raise ValueError('Reassessment must retain the exact original plan and observation')
                elif (source_policy.get('responsibility_revision') != (policy or {}).get('responsibility_revision') or
                        source_policy['cases'].get(key) != entry or
                        (entry and any(source_policy[k] != policy[k] for k in ('human_scope', 'followup_issue')))):
                    raise ValueError('Initial/deferred/human scope changed; retest required')
            if key in GOP_KEYS:
                source_gop = gop_amendment(source, source, git)
                if (canonical_hash(source_gop) if source_gop else None) != (gop or {}).get('revision'):
                    raise ValueError('GOP conditions changed; retest required')
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
        method = (source_policy or {}).get('responsibility_revision', {}).get('method_revision')
        validate_full_result(result, source, artifact, execution, None if method else gop,
                             effective_initial_entry(source_policy, key) if source_policy and key in source_policy['cases'] else None)
        validate_responsibility_observation(result, key, source_policy)
    if not reassessed and source != candidate:
        validate_responsibility_observation(result.get('initial_observation', {}) if stage else result, key, policy)
    if source == candidate and not reassessed:
        validate_fact_assessment(result.get('initial_observation', {}) if stage else result,
                                 key, policy, candidate, artifact, git, trusted_base)
    if case and policy and key in policy['cases'] and canonical_hash(case) != policy['cases'][key]['current_case_sha256']:
        observed = result.get('initial_observation', {}) if stage else result
        if observed.get('case_sha256') != canonical_hash(effective_responsibility_case(policy, key, case)):
            raise ValueError('Updated Case requires observation of the exact current effective contract: ' + key)
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


REPAIR_MARKER = '<!-- acceptance-repair:v1 -->'


def repair_record(issue, api):
    """Remote scope is reviewed with the Issue; the PR cannot shrink its failure set."""
    declaration = re.findall(r'^Acceptance repair: (.+)$', issue.get('body') or '', re.M)
    bug = any(label.get('name') == 'type:bug' for label in issue.get('labels', []))
    if not declaration and not bug:
        return None
    if len(declaration) != 1:
        raise ValueError('Bug Issue requires an explicit Acceptance repair declaration')
    if declaration == ['none']:
        return None
    if not re.fullmatch(r'[1-9][0-9]*', declaration[0]):
        raise ValueError('Acceptance repair must be none or its preserved Issue comment ID')
    comment = api('issues/comments/' + declaration[0])
    if comment.get('issue_url') != issue.get('url') or not comment.get('body', '').startswith(REPAIR_MARKER + '\n'):
        raise ValueError('Repair failure record must belong to the correction Issue')
    created = comment.get('created_at')
    if (not nonempty(created) or created != comment.get('updated_at') or
            datetime.fromisoformat(created.replace('Z', '+00:00')).tzinfo is None):
        raise ValueError('Original failure record must be an identified unedited comment; preserve history and use a new ID')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('Duplicate repair record key')
            result[key] = value
        return result
    record = json.loads(comment['body'][len(REPAIR_MARKER):], object_pairs_hook=unique)
    if (not isinstance(record, dict) or set(record) != {'schema', 'fix_issue', 'failures'} or
            type(record['schema']) is not int or record['schema'] != 1 or
            type(record['fix_issue']) is not int or record['fix_issue'] != issue.get('number') or
            not isinstance(record['failures'], dict) or not record['failures']):
        raise ValueError('Invalid original acceptance failure record')
    return record


def bind_receipt_claim(receipts, receipt, claim):
    locator = receipt[1]
    if not isinstance(locator, str) or not re.fullmatch(r'(?:/(?:[^~/\r\n]|~[01])*)*', locator):
        raise ValueError('Receipt locator must be an exact JSON Pointer, or empty for the whole receipt')
    if any(sealed == receipt[0] and previous != claim and
           (location == locator or location.startswith(locator + '/') or locator.startswith(location + '/'))
           for (sealed, location), previous in receipts.items()):
        raise ValueError('Conflicting origin/session/activity/time for the same sealed receipt observation')
    receipts[receipt] = claim


def repair_contributors(observed, writer, earliest, receipts=None):
    """Public roles are display text; only originating-session fingerprints exclude reviewers."""
    attribution = observed.get('attribution')
    if 'attribution' not in observed:
        # Only the unambiguous one-session legacy form can retain scalar attribution.
        if writer != observed['observer']:
            raise ValueError('Legacy repair requires single-session attribution; composed records need contributors')
        return {session_fingerprint(writer)}
    if (not isinstance(attribution, dict) or set(attribution) != {'writer', 'observer', 'contributors'} or
            not all(isinstance(attribution[k], str) and SESSION_HASH.fullmatch(attribution[k])
                    for k in ('writer', 'observer')) or
            not isinstance(attribution['contributors'], list) or not attribution['contributors']):
        raise ValueError('Repair attribution requires canonical writer, observer and all contributors')
    identities, activities, entries = set(), set(), set()
    receipts = {} if receipts is None else receipts
    observed_at = datetime.fromisoformat(observed['at'].replace('Z', '+00:00'))
    observer_found, writer_found = False, False
    for item in attribution['contributors']:
        if (not isinstance(item, dict) or set(item) not in (
                {'role', 'session', 'activity', 'at', 'receipt_sha256'},
                {'role', 'session', 'activity', 'at', 'receipt_sha256', 'receipt_locator'}) or
                not nonempty(item['role']) or not isinstance(item['session'], str) or
                not SESSION_HASH.fullmatch(item['session']) or
                item['activity'] not in ('writer', 'execution', 'assessment', 'composition') or
                not isinstance(item['receipt_sha256'], str) or not HASH.fullmatch(item['receipt_sha256']) or
                not isinstance(item['at'], str) or not re.fullmatch(
                    r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:[0-5]\d)', item['at'])):
            raise ValueError('Malformed repair contributor identity, activity, time or sealed receipt')
        when = datetime.fromisoformat(item['at'].replace('Z', '+00:00'))
        locator = item.get('receipt_locator', '')
        receipt = (item['receipt_sha256'], locator)
        claim = (item['session'], item['activity'], when)
        bind_receipt_claim(receipts, receipt, claim)
        entry = (receipt, claim)
        if entry in entries or when > observed_at or (item['activity'] != 'writer' and when < earliest):
            raise ValueError('Duplicate or out-of-order repair contributor observation')
        entries.add(entry)
        identities.add(item['session'])
        activities.add(item['activity'])
        writer_found |= (item['session'] == attribution['writer'] and item['activity'] in ('writer', 'composition'))
        observer_found |= (item['session'] == attribution['observer'] and when == observed_at and
                           item['activity'] in ('execution', 'assessment', 'composition'))
    if (not writer_found or not observer_found or
            not {'writer', 'execution'} <= activities):
        raise ValueError('Repair attribution omits its writer, actual observation or execution')
    return identities


def verify_repair(pr, issue, data, path, api, git):
    """Structure/provenance only. An independent reviewer must assess the actual evidence."""
    record = repair_record(issue, api)
    tracked, seen = set(), set()
    base, head = pr['base']['sha'], pr['head']['sha']
    # Follow every parent and changed blob, including failures later deleted or reverted.
    history = git('log', '--full-history', '-m', '--format=%H', '--name-only',
                  '--diff-filter=AMT', '--no-renames', base, '--', 'docs/verification/changes/')
    for line in history.splitlines():
        if SHA.fullmatch(line):
            source = line
            continue
        if not line:
            continue
        match = re.fullmatch(r'docs/verification/changes/issue-([1-9][0-9]*)\.json', line)
        if not match:
            raise ValueError('Unknown acceptance source in trusted history')
        entry = git('ls-tree', source, '--', line).split()
        if len(entry) != 4 or entry[0] != '100644' or entry[1] != 'blob' or entry[3] != line:
            raise ValueError('Acceptance history requires regular JSON source files')
        identity = (line, entry[2])
        if identity in seen:
            continue
        seen.add(identity)
        original = regular_json(source, line, git)
        for case in original['cases']:
            if case.get('fix_issue') == data['issue']:
                tracked.update((f'{match[1]}:{case["id"]}', actor, canonical_hash(case))
                               for actor in ('gpt', 'human')
                               if case[actor]['status'] == 'fail')
    repair = data.get('repair')
    if record is None:
        if tracked or repair is not None:
            raise ValueError('Acceptance NG correction requires its preserved failure record')
        return {}
    if (not isinstance(repair, dict) or set(repair) != {'candidate', 'artifacts', 'failure_record_sha256',
                                                      'writer_session', 'results'} or
            repair['failure_record_sha256'] != canonical_hash(record) or
            not nonempty(repair['writer_session']) or not isinstance(repair['artifacts'], dict) or
            not isinstance(repair['results'], dict) or set(repair['results']) != set(record['failures'])):
        raise ValueError('Repair must cover exactly every original failure on a fixed candidate')
    candidate = repair['candidate']
    if not SHA.fullmatch(candidate or '') or candidate == head:
        raise ValueError('Freeze the tested repair candidate before committing evidence metadata')
    git('merge-base', '--is-ancestor', base, candidate)
    git('merge-base', '--is-ancestor', candidate, head)
    correction = git('diff', '--no-renames', '--name-only', base, candidate).splitlines()
    if candidate == base or not any(not p.startswith('docs/verification/changes/') and
                                    p != PROMOTION for p in correction):
        raise ValueError('Repair candidate requires a substantive correction beyond acceptance metadata')
    scoped_history(candidate, head, {path: '100644'}, git)
    trusted = api('git/ref/heads/main').get('object', {}).get('sha')
    if not SHA.fullmatch(trusted or ''):
        raise ValueError('Repair requires an identified current trusted main contract')
    git('fetch', '--no-tags', 'origin', trusted)
    policy = initial_stage_plan(trusted, candidate, git) if git('ls-tree', trusted, '--', INITIAL_PLAN) else None
    contract = canonical_hash(policy['responsibility_revision']) if policy else canonical_hash(None)
    cases, observers, receipts = set(), set(), {}
    for identifier, failure in record['failures'].items():
        if (not re.fullmatch(r'[A-Z][A-Z0-9-]+', identifier) or not isinstance(failure, dict) or
                set(failure) != {'case', 'source_candidate', 'source_case_sha256', 'original_result', 'guarantees'} or
                not re.fullmatch(r'[1-9][0-9]*:[A-Z][A-Z0-9-]+', failure['case']) or
                not isinstance(failure['guarantees'], dict) or not failure['guarantees'] or
                not all(nonempty(k) and nonempty(v) for k, v in failure['guarantees'].items())):
            raise ValueError('Original failed guarantees and fixed Case provenance are required')
        source = failure['source_candidate']
        old = failure['original_result']
        if not isinstance(old, dict) or old.get('status') != 'fail':
            raise ValueError('Preserve the original failed observation')
        validate_observation(dict(old, status='pass'), source)
        git('merge-base', '--is-ancestor', source, base)
        number, case_id = failure['case'].split(':')
        original = regular_json(source, f'docs/verification/changes/issue-{number}.json', git)
        validate_change(original, int(number), True)
        matches = [case for case in original['cases'] if case['id'] == case_id]
        if len(matches) != 1 or canonical_hash(matches[0]) != failure['source_case_sha256']:
            raise ValueError('Original repair Case hash mismatch')
        if matches[0].get('fix_issue') == data['issue'] and matches[0][old['actor']]['status'] == 'fail':
            cases.add((failure['case'], old['actor'], failure['source_case_sha256']))
        result = repair['results'][identifier]
        if (not isinstance(result, dict) or set(result) != {'observation', 'coverage', 'method', 'contract_revision'} or
                result['contract_revision'] != contract or not nonempty(result['method']) or
                not isinstance(result['coverage'], dict) or set(result['coverage']) != set(failure['guarantees']) or
                not all(nonempty(v) for v in result['coverage'].values())):
            raise ValueError('Repair requires complete failed-guarantee coverage and trusted contract/method')
        artifact = repair['artifacts'].get(matches[0].get('artifact', 'app'))
        if not HASH.fullmatch(artifact or '') or artifact == old['artifact_sha256']:
            raise ValueError('Repair requires the corrected fixed artifact, not the failed build')
        observed = result['observation']
        validate_observation(observed, candidate, artifact)
        when = datetime.fromisoformat(observed['at'].replace('Z', '+00:00'))
        earliest = max(datetime.fromisoformat(t.replace('Z', '+00:00')) for t in
                       (old['at'], git('show', '-s', '--format=%cI', candidate)))
        if when < earliest:
            raise ValueError('Repair observation predates the failure or fixed candidate')
        observers.update(repair_contributors(observed, repair['writer_session'], earliest, receipts))
    if not tracked <= cases:
        raise ValueError('Repair omits a trusted tracked original failure Case')
    if api('git/ref/heads/main').get('object', {}).get('sha') != trusted:
        raise ValueError('Trusted main contract changed during repair validation; rerun')
    return {'repair_revision': canonical_hash([record, repair]), 'repair_contract_base': trusted, 'repair_record': record,
            'repair': repair, 'repair_observers': sorted(observers)}


def promotion_certificates(promotion, base, candidate, git):
    """Keep exact main certificate archives; consumed evidence must bind the candidate."""
    paths = {}
    for path in git('diff', '--no-renames', '--name-only', candidate, base).splitlines():
        if (not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', path) or
                not git('ls-tree', base, '--', path)):
            continue
        adopted = regular_json(base, path, git)
        if 'evidence_certificates' not in adopted:
            continue
        validate_change(adopted, int(Path(path).stem.removeprefix('issue-')), False)
        if not isinstance(adopted['evidence_certificates'], dict) or not adopted['evidence_certificates']:
            raise ValueError('Historical certificate archive must be a nonempty main tooling registration')
        paths[path] = git('ls-tree', base, '--', path)
    results = promotion.get('results', {})
    if not isinstance(results, dict):
        raise ValueError('Candidate results must be an object matching ALL required Cases')
    for key, result in results.items():
        if not isinstance(result, dict):
            continue
        observed = result.get('initial_observation', result)
        assessment = observed.get('responsibility_assessment', {}) if isinstance(observed, dict) else {}
        ref = assessment.get('impact_reuse') if isinstance(assessment, dict) else None
        if ref is None:
            continue
        if (not isinstance(ref, dict) or not isinstance(ref.get('path'), str) or
                not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', ref['path']) or
                not isinstance(ref.get('commit'), str) or not SHA.fullmatch(ref['commit'])):
            raise ValueError('Promotion certificate requires its fixed main Issue record')
        try:
            git('merge-base', '--is-ancestor', ref['commit'], base)
        except subprocess.CalledProcessError as error:
            raise ValueError('Certificate was not adopted in trusted main') from error
        path = ref['path']
        adopted = regular_json(base, path, git)
        validate_change(adopted, int(Path(path).stem.removeprefix('issue-')), False)
        original = regular_json(ref['commit'], path, git)
        certificate = original.get('evidence_certificates', {}).get(ref.get('id'))
        if (not isinstance(certificate, dict) or certificate != adopted.get('evidence_certificates', {}).get(ref.get('id')) or
                canonical_hash(certificate) != ref.get('sha256') or certificate.get('key') != key or
                certificate.get('candidate') != candidate or result.get('head') != candidate or
                certificate.get('artifact_sha256') != result.get('artifact_sha256')):
            raise ValueError('Promotion metadata must carry the exact main-adopted candidate/build certificate')
        paths[path] = git('ls-tree', base, '--', path)
    return paths



def promotion_policy_blobs(base, candidate, git):
    """Keep reviewed main policy without changing the candidate's effective contract."""
    from change_impact import path_impacts, TOOLING, KNOWLEDGE, METADATA
    if initial_stage_plan(base, candidate, git) != initial_stage_plan(candidate, candidate, git):
        raise ValueError('Policy carry requires the unchanged effective candidate contract')
    ancestor = git('merge-base', candidate, base)
    paths = {}
    for path in git('diff', '--no-renames', '--name-only', ancestor, base).splitlines():
        if path.startswith('docs/verification/') and not path.endswith('.md'):
            continue  # Certificate archives and consumed evidence have their own adoption gate.
        blob = git('ls-tree', base, '--', path)
        old_blob = git('ls-tree', candidate, '--', path)
        modes = tuple(x.split()[0] if x else '000000' for x in (old_blob, blob))
        if path_impacts(path, modes) <= {TOOLING, KNOWLEDGE, METADATA} and blob != old_blob:
            paths[path] = blob
    return paths


def promotion_metadata_history(base, candidate, head, allowed, carried, git):
    if any(git('ls-tree', head, '--', p) != blob for p, blob in carried.items()):
        raise ValueError('Promotion policy/certificate metadata must equal the trusted main blob')
    if any(p not in allowed for p in git('diff', '--no-renames', '--name-only', candidate, head).splitlines()):
        raise ValueError('Promotion tree differs from tested candidate outside acceptance metadata')
    for commit in git('rev-list', head, '--not', candidate, base).splitlines():
        parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
        if not parents or len(parents) > 2 or (len(parents) == 2 and parents[1] != base):
            raise ValueError('Unexpected promotion ancestry; only the current main merge is allowed')
        changed = git('diff', '--no-renames', '--name-only', parents[0], commit).splitlines()
        if any(p not in allowed for p in changed):
            raise ValueError('Untested commit in promotion history, even if later reverted')
        if any(p in carried and git('ls-tree', commit, '--', p) != carried[p] for p in changed):
            raise ValueError('Changed or reverted policy/certificate metadata differs from trusted main')


def certificate_sync_main(pr, path, files, api, git):
    """Retain adopted archives through actual main merges, never candidate registration."""
    from change_impact import path_impacts, TOOLING, KNOWLEDGE, METADATA
    base, head = pr['base']['sha'], pr['head']['sha']
    chain = git('rev-list', '--first-parent', f'{base}..{head}').splitlines()
    edges, previous = [], base
    for commit in reversed(chain):
        parents = git('rev-list', '--parents', '-n', '1', commit).split()[1:]
        if not parents or len(parents) > 2 or parents[0] != previous:
            return None
        edges.append((commit, parents))
        previous = commit
    if previous != head or not any(len(parents) == 2 for _, parents in edges):
        return None
    trusted = api('git/ref/heads/main').get('object', {}).get('sha')
    load_impact_source(trusted, git)
    for commit, parents in edges:
        source = parents[1] if len(parents) == 2 else None
        if source:
            git('merge-base', '--is-ancestor', source, trusted)
        for changed in git('diff', '--no-renames', '--name-only', parents[0], commit).splitlines():
            if changed == path:
                record = regular_json(commit, path, git)
                validate_change(record, int(Path(path).stem.removeprefix('issue-')), False)
                if 'evidence_certificates' in record or 'repair' in record:
                    raise ValueError('Certificate sync cannot register certificates or repair results')
                continue
            before, after = (git('ls-tree', ref, '--', changed) for ref in (parents[0], commit))
            modes = tuple(entry.split()[0] if entry else '000000' for entry in (before, after))
            if (not source or path_impacts(changed, modes) - {TOOLING, KNOWLEDGE, METADATA} or
                    after != git('ls-tree', source, '--', changed)):
                raise ValueError('Certificate sync requires exact main blobs and only its own acceptance edits')
    # All non-first-parent history must already belong to the fixed trusted main.
    if set(chain) != set(git('rev-list', head, '--not', base, trusted).splitlines()):
        raise ValueError('Certificate sync contains unexplained or already-main first-parent history')
    for changed in sorted(set(files) | set(git('diff', '--no-renames', '--name-only', base, trusted).splitlines())):
        if not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', changed):
            continue
        records = [regular_json(ref, changed, git) for ref in (base, head, trusted)
                   if git('ls-tree', ref, '--', changed)]
        if not any('evidence_certificates' in record for record in records):
            continue
        adopted = regular_json(trusted, changed, git)
        validate_change(adopted, int(Path(changed).stem.removeprefix('issue-')), False)
        if (not isinstance(adopted.get('evidence_certificates'), dict) or not adopted['evidence_certificates'] or
                git('ls-tree', head, '--', changed) != git('ls-tree', trusted, '--', changed)):
            raise ValueError('Certificate sync archive differs from current trusted main')
    return trusted


def verify_pr(pr, api, git=git_read):
    """api(path) uses the same repo; git sees fetched PR/candidate objects only as data."""
    issue, path, mode = metadata(pr)
    head, base = pr['head']['sha'], pr['base']['sha']
    data = regular_json(head, path, git)
    gui = field(pr['body'], 'GUI') == 'required'
    validate_change(data, issue, gui)
    files = git('diff', '--no-renames', '--name-only', base, head).splitlines()
    pairs = [(base, head)]
    sync_main = None
    for commit in git('rev-list', head, '--not', base).splitlines():
        for parent in git('rev-list', '--parents', '-n', '1', commit).split()[1:]:
            pairs.append((parent, commit))
    for before_ref, after_ref in pairs:
        changed_paths = files if (before_ref, after_ref) == (base, head) else git(
            'diff', '--no-renames', '--name-only', before_ref, after_ref).splitlines()
        for changed in changed_paths:
            if not re.fullmatch(r'docs/verification/changes/issue-[1-9][0-9]*\.json', changed):
                continue
            before = regular_json(before_ref, changed, git).get('evidence_certificates') if git('ls-tree', before_ref, '--', changed) else None
            after = regular_json(after_ref, changed, git).get('evidence_certificates') if git('ls-tree', after_ref, '--', changed) else None
            adopted = after and git('ls-tree', after_ref, '--', changed) == git('ls-tree', base, '--', changed)
            if before != after and not adopted and (mode != 'tooling' or changed != path or not after):
                if mode == 'develop' and not gui and sync_main is None:
                    sync_main = certificate_sync_main(pr, path, files, api, git)
                if not sync_main:
                    raise ValueError('Changed certificates require their dedicated current-Issue main tooling registration')
    if 'evidence_certificates' in data and mode != 'tooling':
        raise ValueError('Evidence certificate registration requires its independent main tooling route')
    if mode == 'develop':
        # Ordinary pending/blocked/fail Cases remain tracked; original-NG corrections must pass.
        for case in data['cases']:
            if any(case[actor]['status'] == 'fail' for actor in ('gpt', 'human')):
                fix = api(f'issues/{case["fix_issue"]}')
                if fix.get('state') != 'open' or 'pull_request' in fix:
                    raise ValueError('Product failure needs an open dedicated fix Issue')
        repair = verify_repair(pr, api(f'issues/{issue}'), data, path, api, git)
        if sync_main and api('git/ref/heads/main').get('object', {}).get('sha') != sync_main:
            raise ValueError('Trusted main changed during certificate sync validation; rerun')
        return {'mode': mode, 'gui_complete': not gui, 'cases': len(data['cases']), **repair,
                **({'certificate_sync_main': sync_main} if sync_main else {})}
    if mode == 'tooling':
        if gui or not files or any(not tooling_path(f) for f in files):
            raise ValueError('Direct main tooling route is restricted to documented non-product paths')
        certificates = certificate_acceptance(data['evidence_certificates'], git, base) if 'evidence_certificates' in data else {}
        return {'mode': mode, 'gui_complete': True, 'cases': 0, **certificates}
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
        certificates = promotion_certificates(promotion, base, candidate, git)
        carried = dict(certificates)
        if certificates:
            carried.update(promotion_policy_blobs(base, candidate, git))
        allowed = {PROMOTION, path} | set(carried)
        promotion_metadata_history(base, candidate, head, allowed, carried, git)
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
            if carried.get('responsibility_revision') or effective_initial_entry(carried, key)['deferred_scope']:
                requirement = (case.get('required_execution'), case.get('artifact', 'app'))
                if key not in required:
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
        case = None
        if full_policy and key in full_policy['cases']:
            path = f'docs/verification/changes/issue-{key.split(":")[0]}.json'
            if git('ls-tree', candidate, '--', path):
                current = regular_json(candidate, path, git)
                validate_change(current, int(key.split(':')[0]), True)
                case = next((c for c in current['cases'] if c['id'] == key.split(':')[1]), None)
            if case:
                required[key] = (case.get('required_execution'), case.get('artifact', 'app'))
        execution, artifact_name = required[key]
        artifact = artifacts.get(artifact_name)
        if not HASH.fullmatch(artifact or ''):
            raise ValueError('Fixed candidate artifact hash is required: ' + artifact_name)
        validate_candidate_result(result, key, initial_policy or full_policy, stage, candidate,
                                  artifact, execution, gop_cases.get(key), git, base, case)
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
                        entry['current_case_sha256'], *(s['original_case_sha256'] for s in entry['sources'])}:
                    raise ValueError('Current Case differs from its historical initial contract')
            current_rows = []
            for data, case in rows:
                current = regular_json(initial_policy['required_ancestor'],
                                       f'docs/verification/changes/issue-{data["issue"]}.json', git)
                current_rows.append((data, next(c for c in current['cases'] if c['id'] == case['id'])))
            rows = current_rows
        else:
            current_rows = []
            for data, case in rows:
                if f'{data["issue"]}:{case["id"]}' in initial_policy['cases']:
                    path = f'docs/verification/changes/issue-{data["issue"]}.json'
                    current = regular_json(candidate, path, git) if git('ls-tree', candidate, '--', path) else {'cases': []}
                    if 'issue' in current:
                        validate_change(current, data['issue'], True)
                    case = next((c for c in current['cases'] if c['id'] == case['id']), None)
                if case:
                    current_rows.append((data, case))
            rows = current_rows
            carried = {key for key in initial_policy['cases'] if initial_policy.get('responsibility_revision') or
                       effective_initial_entry(initial_policy, key)['deferred_scope']}
            current_keys = {f'{data["issue"]}:{case["id"]}' for data, case in rows}
            for key in sorted(carried):
                if key in current_keys:
                    continue
                entry = initial_policy['cases'][key]
                data = regular_json(initial_policy['required_ancestor'], entry['sources'][0]['source_path'], git)
                case = next(c for c in data['cases'] if c['id'] == key.split(':')[1])
                rows.append((data, case))
    contracts = {f'{data["issue"]}:{case["id"]}': case for data, case in rows}
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
    if (initial_policy or {}).get('responsibility_revision', {}).get('method_revision'):
        rows = [(data, contracts.get(f'{data["issue"]}:{case["id"]}', case)) for data, case in rows]
    rows = [(data, effective_responsibility_case(initial_policy, f'{data["issue"]}:{case["id"]}', case))
            for data, case in rows]
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
                        artifact, case.get('required_execution'), gop_cases.get(key), git,
                        promotion.get('base'), contracts[key] if not stage else None)
            if stage:
                label = ('延期・未実施／初期版必須外・後続#65' if observed['status'] == 'deferred' else
                         '初期版範囲合格／原Case未完了（全範囲gateは別途必要）')
            else:
                label = 'Case合格（全範囲gateは別途必要）'
            if result.get('status') == 'reused':
                label = '同一成果物の旧観察を再利用／' + label
            elif result.get('status') == 'reassessed':
                label = '旧観察を有効条件へ再評価／新しい試験実行ではない／' + label
            elif (observed.get('initial_observation', {}) if stage else observed).get('responsibility_assessment'):
                assessment = (observed.get('initial_observation', {}) if stage else observed)['responsibility_assessment']
                label = ('修正影響を保証ごとに評価／旧観察と現在事実を区別／' if 'impact_reuse' in assessment else
                         '保持事実を有効条件へ再評価／新しい試験実行ではない／') + label
        except ValueError as exc:
            if result.get('status') in ('reused', 'reassessed'):
                label = '不可・再利用条件不成立: ' + str(exc)
        lines.append(f'| {case["id"]} / #{data["issue"]} / #{data.get("pr", "未作成")} | {case["change"]} | '
                     f'{result.get("status", "pending")} | {label} | '
                     f'{case["fix_issue"] or "—"} / {case["fix_pr"] or "—"} |')
    for data, case in rows:
        lines += ['', f'## #{data["issue"]} / {case["id"]}: {case["change"]}', '',
                  f'PR: [#{data["pr"]}](https://github.com/shinma06/android-replay-buffer/pull/{data["pr"]})' if data.get('pr') else 'PR: 未登録', '', '前提・対象build: ' + case['preconditions'], '']
        amended = gop_cases.get(f'{data["issue"]}:{case["id"]}')
        result = results.get(f'{data["issue"]}:{case["id"]}', {})
        retained = result.get('status') in ('reused', 'reassessed')
        reassessed = result.get('status') == 'reassessed'
        evidence_label = '有効条件の証拠'
        if retained:
            confirmation = result.get('confirmation', {})
            evidence_label += '（現在候補の適合確認）' if reassessed else '（保持した元観察）'
            lines += ['再利用申請（判定は上表）・元候補: ' + str(result.get('source_candidate', '未登録')),
                      '現在候補の適合確認: ' + json.dumps(confirmation, ensure_ascii=False), '']
            result = result.get('source_result', {})
            if not isinstance(result, dict):
                result = {}
        if stage:
            result = result.get('initial_observation', {})
        responsibility_observed = confirmation if reassessed else result
        if not isinstance(responsibility_observed, dict):
            responsibility_observed = {}
        key = f'{data["issue"]}:{case["id"]}'
        assessment = responsibility_observed.get('responsibility_assessment')
        if assessment is not None:
            retained = True
            lines += ['保持事実の再評価（新しい試験実行ではない）: ' + json.dumps(assessment, ensure_ascii=False), '']
        entry = effective_initial_entry(initial_policy, key) if initial_policy and key in initial_policy['cases'] else None
        if entry:
            lines += ['初期版範囲: ' + entry['initial_scope'],
                      '初期版計画revision: ' + canonical_hash(initial_policy),
                      ('原Case全体: 未完了。初期版の合格・延期を全体passに転記しない。' if stage else
                       '完全受入: 初期版で延期した原契約も含め、同一候補の全工程を確認する。'),
                      '初期版で実施: 以下の全出典の条件から、明記した延期部分だけを除く。',
                      ('旧人間範囲（履歴）: ' if initial_policy.get('responsibility_revision') else '人間確認の扱い: ') + initial_policy['human_scope'],
                      '延期部分: ' + ' / '.join(entry['deferred_scope']),
                      '維持するAgent条件: ' + ' / '.join(entry['agent_requirements']),
                      '完全受入の確認者: ' + entry.get('full_acceptance_actor', '原契約の実施者（gpt / human）'),
                      f'後続Issue: #{initial_policy["followup_issue"]} / 担当: ' + initial_policy['owner'],
                      '再開条件: ' + initial_policy['resume_condition'],
                      '全固定出典: ' + ', '.join(f'PR #{s["source_pr"]} / {s["source_merge"]} / '
                                               f'{s["source_path"]} / {s["original_case_sha256"]}'
                                               for s in entry['sources']), '']
            group = responsibility_group(initial_policy, key)
            if group:
                evidence = responsibility_observed.get('responsibility_evidence')
                method = initial_policy['responsibility_revision'].get('method_revision')
                lines += [(f'目的による方法改訂 #{method["issue"]}: ' if method else '責任改訂 #182: ') + group['disposition'] + ' / ' + group['purpose'],
                          '改訂revision: ' + canonical_hash(initial_policy['responsibility_revision']),
                          '除外/置換する義務（PASSではない）: ' + group['excluded'],
                          '有効な実施分担: ' + group['methods'],
                          evidence_label + ': ' +
                          (json.dumps(evidence, ensure_ascii=False) if evidence else '未登録（未実施又は延期）'),
                          '以下の固定原契約は履歴。有効条件は明示改訂と上の分担を適用する。', '']
            if stage and entry['initial_scope'] == 'deferred':
                lines += ['このREAL専用Caseは全工程を延期。以下は再開用の原契約。', '']
            versions = {}
            for source_key, original, source in historical:
                if source_key == f'{data["issue"]}:{case["id"]}':
                    versions.setdefault(canonical_hash(original), (original, []))[1].append(source['source_pr'])
            for original, prs in versions.values():
                lines += ['固定原契約 / PR ' + ', '.join('#' + str(pr) for pr in prs),
                          '原前提: ' + original['preconditions']]
                lines += [f'原手順 {n}: {step}' for n, step in enumerate(original['steps'], 1)]
                lines += ['原期待結果: ' + original['expected'], '']
        if amended and not (initial_policy or {}).get('responsibility_revision', {}).get('method_revision'):
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
            lines += [('保持事実の現在評価（再試験ではない）: ' if assessment is not None else
                       '元の観察（履歴、新候補の再試験ではない）: ' if retained else '今回の候補結果: ') +
                      result.get('status', 'pending'),
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
