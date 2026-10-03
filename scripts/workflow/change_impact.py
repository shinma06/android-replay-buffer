#!/usr/bin/env python3
"""One conservative impact classifier for CI, hooks, and Python CLI checks."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import subprocess

RUNTIME = 'IMPACT_RUNTIME'
BUILD = 'IMPACT_BUILD'
TEST = 'IMPACT_TEST'
PLUGIN = 'IMPACT_PLUGIN'
TOOLING = 'IMPACT_TOOLING'
KNOWLEDGE = 'IMPACT_KNOWLEDGE_ONLY'
METADATA = 'IMPACT_METADATA_ONLY'
UNKNOWN = 'IMPACT_UNKNOWN'
ORDER = (RUNTIME, BUILD, TEST, PLUGIN, TOOLING, KNOWLEDGE, METADATA, UNKNOWN)


def path_impacts(path, modes=('100644',)):
    """Allow only known non-executable knowledge paths; bundled Markdown is runtime."""
    p = PurePosixPath(path)
    if not path or p.is_absolute() or '..' in p.parts or str(p) != path:
        return {UNKNOWN}
    if path.startswith('plugin/'):
        return {PLUGIN}
    if path.startswith('docs/cli-origin/') or path in ('scripts/workflow/cli_origin.py', 'scripts/workflow/plugin_check.py'):
        return {BUILD, TOOLING}
    if path.startswith('tests/'):
        return {TEST}
    if path.startswith(('replay_buffer/', 'bin/')):
        return {RUNTIME}
    if path.startswith('launchd/') or path in ('pyproject.toml', 'config.json.example'):
        return {BUILD}
    if path in ('.gitattributes', '.gitignore', 'scripts/workflow/product_check.py',
                'scripts/workflow/change_impact.py',
                '.github/workflows/ci.yml'):
        # These determine build inputs, classification, or artifact generation itself.
        return {BUILD, TOOLING}
    if (path.startswith(('scripts/', 'tests/')) and p.suffix in ('.py', '.sh')
            or path in ('.githooks/pre-commit', '.githooks/pre-push')):
        return {TOOLING}
    if path.startswith('docs/verification/') and p.suffix != '.md':
        return {TOOLING}  # Acceptance inputs are executable policy, not inert metadata.
    regular = all(mode in ('000000', '100644') for mode in modes)
    if path in ('AGENTS.md', 'CLAUDE.md') and all(mode in ('000000', '100644', '120000') for mode in modes):
        return {KNOWLEDGE}
    if regular and (path in ('README.md', 'CLAUDE.md', 'CONTRIBUTING.md')
                    or path.startswith('docs/') and p.suffix == '.md'
                    or path.startswith(('.agents/skills/', '.claude/skills/')) and p.name == 'SKILL.md'
                    or path.startswith('.cursor/rules/') and p.suffix == '.mdc'):
        return {KNOWLEDGE}
    if regular and (path == '.github/pull_request_template.md' or path == '.github/CODEOWNERS'
                    or path.startswith('.github/ISSUE_TEMPLATE/') and p.suffix in ('.yml', '.yaml', '.md')):
        return {METADATA}
    if (path.startswith('.github/workflows/') and p.suffix in ('.yml', '.yaml')
            or path in ('.github/main-ruleset.json', '.github/develop-ruleset.json', '.claude/settings.json')):
        return {TOOLING}
    return {UNKNOWN}


def classify(changes, *, reason=None, force_full=False):
    files = [{'path': path, 'impacts': sorted(path_impacts(path, modes))} for path, modes in changes]
    impacts = set().union(*(set(f['impacts']) for f in files))
    if reason or not files:
        impacts.add(UNKNOWN)
        reason = reason or 'No changed files established; run full validation'
    full = force_full or UNKNOWN in impacts
    return {'impacts': [x for x in ORDER if x in impacts], 'files': files,
            'product_check': full or bool(impacts & {RUNTIME, BUILD, TEST}),
            'plugin_check': full or bool(impacts & {PLUGIN, BUILD}),
            'tooling_test': True,  # Small shared checks also validate documentation and metadata.
            'force_full': force_full, 'reason': reason}


def git(*args, cwd=None):
    return subprocess.check_output(['git', *args], cwd=cwd, stderr=subprocess.PIPE)


def default_base(head='HEAD', cwd=None):
    # A new Issue branch has no upstream. Use the nearest applicable integration
    # line; absent/stale history fails towards full validation, not a guessed skip.
    for ref in ('origin/develop', 'origin/main'):
        try:
            sha = git('rev-parse', '--verify', ref + '^{commit}', cwd=cwd).decode().strip()
            ancestor = git('merge-base', sha, head, cwd=cwd).decode().strip()
            return ancestor
        except subprocess.CalledProcessError:
            continue
    raise ValueError('No verified integration base; fetch origin or specify --base')


def git_impact(base=None, head='HEAD', *, cwd=None, merge_base=True, force_full=False):
    try:
        base = base or default_base(head, cwd)
        # Verify refs separately; never pass an untrusted filename/ref as a Git option.
        base = git('rev-parse', '--verify', '--end-of-options', base + '^{commit}', cwd=cwd).decode().strip()
        head = git('rev-parse', '--verify', '--end-of-options', head + '^{commit}', cwd=cwd).decode().strip()
        ancestor = git('merge-base', base, head, cwd=cwd).decode().strip()
        if merge_base:
            base = ancestor
        elif ancestor != base:
            raise ValueError('Non-ancestor comparison; full validation required')
        # Disable rename detection: inspect both deleted and added paths/modes.
        raw = git('diff', '--raw', '-z', '--no-renames', '--no-ext-diff', '--ignore-submodules=none', base, head, '--', cwd=cwd)
        entries = raw.split(b'\0')
        changes = []
        for index in range(0, len(entries) - 1, 2):
            old, new, *_ = entries[index].decode('ascii').removeprefix(':').split()
            changes.append((entries[index + 1].decode('utf-8'), (old, new)))
        result = classify(changes, force_full=force_full)
        result.update(base=base, head=head)
        return result
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        return classify([], reason='Cannot establish complete diff: ' + type(error).__name__, force_full=force_full)


def test_commands(result):
    commands = [['python3', 'scripts/check.py']]
    if result['product_check']:
        commands.append(['python3', 'scripts/workflow/product_check.py'])
    if result['plugin_check']:
        commands.append(['python3', 'scripts/workflow/plugin_check.py'])
    return commands


def report(result):
    summary = 'Change Impact: ' + ', '.join(result['impacts']) + '\n'
    summary += '\n'.join(f'{name}: {"required" if result[name] else "skipped (no relevant impact)"}'
                         for name in ('product_check', 'plugin_check', 'tooling_test'))
    if result['reason']:
        summary += '\n' + result['reason']
    return summary


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--base')
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--event', action='store_true', help='Read GitHub event diff boundaries')
    parser.add_argument('--force-full', action='store_true')
    parser.add_argument('--run-tests', action='store_true')
    args = parser.parse_args()
    base, merge_base, full = args.base, True, args.force_full
    if args.event:
        event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
        if os.environ['GITHUB_EVENT_NAME'] == 'pull_request':
            base = event['pull_request']['base']['sha']
        elif os.environ['GITHUB_EVENT_NAME'] == 'push':
            base, merge_base = event['before'], False
        else:
            full = True  # Manual validation is an explicit full-CI override.
    if args.run_tests:
        current = git('rev-parse', 'HEAD').decode().strip()
        requested = git('rev-parse', '--verify', '--end-of-options', args.head + '^{commit}').decode().strip()
        if requested != current or git('status', '--porcelain'):
            parser.error('--run-tests requires the checked-out, clean committed HEAD')
    result = git_impact(base, args.head, merge_base=merge_base, force_full=full)
    print(report(result), flush=True)
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            for key in ('product_check', 'plugin_check', 'tooling_test'):
                output.write(f'{key}={str(result[key]).lower()}\n')
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
            summary.write('### Change Impact\n\n' + report(result) + '\n\n```json\n'
                          + json.dumps(result, ensure_ascii=True, indent=2) + '\n```\n')
    if result.get('base'):
        subprocess.run(['git', 'diff', '--check', result['base'], result['head'], '--'], check=True)
    if args.run_tests:
        for command in test_commands(result):
            subprocess.run(command, check=True)


if __name__ == '__main__':
    main()
