"""Impact safety and real Git boundaries; no remote service or recording devices required."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import change_impact as ci


class ClassificationTest(unittest.TestCase):
    def test_checks_keep_selected_interpreter_when_path_points_elsewhere(self):
        with patch.object(ci.sys, 'executable', '/selected/python3'), patch.dict(os.environ, PATH='/other'):
            result = ci.classify([], reason='missing history')
            expected = [
                ['/selected/python3', 'scripts/check.py'],
                ['/selected/python3', 'scripts/workflow/product_check.py', '--plugin'],
            ]
            self.assertEqual(ci.test_commands(result), expected)

    def test_required_eight_cases_and_semantic_exceptions(self):
        cases = [
            (['AGENTS.md'], {ci.KNOWLEDGE}, False),
            (['docs/design.md'], {ci.KNOWLEDGE}, False),
            (['replay_buffer/kotlin/Service.kt'], {ci.RUNTIME}, True),
            (['replay_buffer/res/layout/main.xml'], {ci.RUNTIME}, True),
            (['pyproject.toml'], {ci.BUILD}, True),
            (['.github/workflows/pr-policy.yml'], {ci.TOOLING}, False),
            (['CLAUDE.md', 'replay_buffer/Service.kt'], {ci.KNOWLEDGE, ci.RUNTIME}, True),
            (['new/input.unknown'], {ci.UNKNOWN}, True),
            (['replay_buffer/resources/README.md'], {ci.RUNTIME}, True),
            (['launchd/com.selfregi.replayd.plist'], {ci.BUILD}, True),
            (['tests/resources/input.json'], {ci.TEST}, True),
            (['docs/verification/changes/issue-198.json'], {ci.TOOLING}, False),
            (['.github/ISSUE_TEMPLATE/task.yml'], {ci.METADATA}, False),
            (['scripts/workflow/change_impact.py'], {ci.BUILD, ci.TOOLING}, True),
            (['scripts/workflow/product_check.py'], {ci.BUILD, ci.TOOLING}, True),
            (['docs/codegen.yaml'], {ci.UNKNOWN}, True),
            (['scripts/new/code.kt'], {ci.UNKNOWN}, True),
            (['.github/new/input.bin'], {ci.UNKNOWN}, True),
        ]
        for paths, impacts, build in cases:
            with self.subTest(paths=paths):
                result = ci.classify([(p, ('100644',)) for p in paths])
                self.assertEqual(set(result['impacts']), impacts)
                self.assertEqual(result['product_check'], build)
                self.assertTrue(result['tooling_test'])

    def test_executable_symlink_unknown_and_force_full_never_skip(self):
        for path, modes in [('docs/run.md', ('100644', '100755')), ('docs/link.md', ('120000',)),
                            ('../docs/note.md', ('100644',)), ('/docs/note.md', ('100644',))]:
            self.assertEqual(ci.path_impacts(path, modes), {ci.UNKNOWN})
        self.assertEqual(ci.path_impacts('AGENTS.md', ('120000',)), {ci.KNOWLEDGE})
        for result in (ci.classify([]), ci.classify([], reason='API/history failure'),
                       ci.classify([('README.md', ('100644',))], force_full=True)):
            self.assertTrue(all(result[x] for x in ('product_check', 'tooling_test')))
            expected = [[sys.executable, 'scripts/check.py'],
                        [sys.executable, 'scripts/workflow/product_check.py', '--plugin']]
            self.assertEqual(ci.test_commands(result), expected)

    def test_product_entry_keeps_cli_only_plugin_only_and_mixed_obligations(self):
        for path, arguments in (
                ('docs/design.md', None), ('replay_buffer/save.py', ['--plugin']),
                ('plugin/src/main/kotlin/Action.kt', ['--plugin']),
                ('scripts/workflow/product_check.py', ['--plugin', '--plugin-tests-only'])):
            result = ci.classify([(path, ('100644',))])
            expected = [[sys.executable, 'scripts/check.py']]
            if arguments is not None:
                expected.append([sys.executable, 'scripts/workflow/product_check.py', *arguments])
            with self.subTest(path=path):
                self.assertEqual(ci.test_commands(result), expected)

    def test_distribution_inputs_are_separate_from_required_checks(self):
        positive = ('plugin/src/main/cli/README.txt', 'plugin/src/main/cli/replay',
                    'plugin/src/main/resources/META-INF/plugin.xml', 'plugin/src/clock/java/Clock.java',
                    'plugin/gradle/libs.versions.toml', 'plugin/build.gradle.kts',
                    'plugin/settings.gradle.kts', 'plugin/gradle.properties', 'plugin/gradlew',
                    'scripts/workflow/plugin_check.py', '.github/workflows/ci.yml',
                    '.github/workflows/release-candidate.yml', 'unknown-generator.data')
        negative = ('docs/release-delivery.md', 'README.md', 'AGENTS.md',
                    'plugin/src/test/kotlin/ArchiveTest.kt', 'plugin/src/test/resources/fixture.zip',
                    'tests/test_save.py', 'scripts/workflow/test_release_delivery.py',
                    'scripts/workflow/release_delivery.py', 'docs/verification/changes/issue-320.json',
                    'docs/cli-origin/README.md', 'scripts/workflow/product_check.py')
        for expected, paths in ((True, positive), (False, negative)):
            for path in paths:
                with self.subTest(path=path):
                    result = ci.classify([(path, ('100644',))])
                    self.assertEqual(result['distribution'], expected)
                    self.assertTrue(result['tooling_test'])
                    if expected:
                        self.assertTrue(result['plugin_check'])
        tests = ci.classify([('plugin/src/test/java/ArchiveTest.java', ('100644',))])
        self.assertTrue(tests['plugin_check'])
        self.assertEqual(ci.test_commands(tests)[-1][-2:], ['--plugin', '--plugin-tests-only'])
        self.assertTrue(ci.classify([('plugin/src/test/link', ('120000',))])['distribution'])
        self.assertTrue(ci.classify([('docs/release-delivery.md', ('100755',))])['distribution'])
        self.assertTrue(ci.classify([])['distribution'])
        self.assertTrue(ci.classify([('README.md', ('100644',))], force_full=True)['distribution'])


class GitImpactTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = os.environ.copy()
        for key in subprocess.check_output(['git', 'rev-parse', '--local-env-vars'], text=True).splitlines():
            self.env.pop(key, None)
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Fixture'); self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('replay_buffer/resources/help.md', 'bundled runtime\n')
        self.write('docs/design.md', 'knowledge\n')
        self.base = self.commit()
        self.git('update-ref', 'refs/remotes/origin/develop', self.base)
        env_patch = patch.dict(os.environ, self.env, clear=True); env_patch.start(); self.addCleanup(env_patch.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, env=self.env, stderr=subprocess.PIPE).decode().strip()

    def write(self, name, content):
        file = self.root / name; file.parent.mkdir(parents=True, exist_ok=True); file.write_text(content)

    def commit(self):
        self.git('add', '.'); self.git('commit', '-m', 'fixture'); return self.git('rev-parse', 'HEAD')

    def test_doc_change_and_full_pr_range_after_later_doc_commit(self):
        self.write('docs/design.md', 'changed\n'); docs = self.commit()
        self.assertFalse(ci.git_impact(self.base, docs, cwd=self.root)['product_check'])
        self.write('replay_buffer/Service.kt', 'class Service\n'); self.commit()
        self.write('docs/design.md', 'later knowledge\n'); head = self.commit()
        self.assertTrue(ci.git_impact(None, head, cwd=self.root)['product_check'])

    def test_rename_runtime_to_docs_and_deletion_keep_runtime_impact(self):
        self.git('mv', 'replay_buffer/resources/help.md', 'docs/help.md'); head = self.commit()
        result = ci.git_impact(self.base, head, cwd=self.root)
        self.assertIn(ci.RUNTIME, result['impacts']); self.assertIn(ci.KNOWLEDGE, result['impacts'])
        self.assertEqual(len(result['files']), 2)
        self.assertTrue(result['distribution'])
        self.git('reset', '--hard', self.base)  # Disposable fixture only.
        self.git('rm', 'replay_buffer/resources/help.md'); head = self.commit()
        self.assertTrue(ci.git_impact(self.base, head, cwd=self.root)['product_check'])

    def test_test_only_then_bundled_readme_changes_in_real_git(self):
        self.write('plugin/src/test/resources/input.json', '{}\n')
        tests = self.commit()
        result = ci.git_impact(self.base, tests, cwd=self.root)
        self.assertTrue(result['plugin_check'])
        self.assertFalse(result['distribution'])
        self.write('plugin/src/main/cli/README.txt', 'bundled instructions\n')
        bundled = self.commit()
        self.assertTrue(ci.git_impact(tests, bundled, cwd=self.root)['distribution'])
        self.git('mv', 'plugin/src/main/cli/README.txt', 'docs/cli-instructions.md')
        self.assertTrue(ci.git_impact(bundled, self.commit(), cwd=self.root)['distribution'])

    def test_chmod_unicode_and_newline_paths_are_not_lost(self):
        self.write('docs/日本語\nnotes.md', 'text\n')
        (self.root / 'docs/design.md').chmod(0o755); head = self.commit()
        result = ci.git_impact(self.base, head, cwd=self.root)
        self.assertIn(ci.UNKNOWN, result['impacts']); self.assertTrue(result['product_check'])
        self.assertEqual({x['path'] for x in result['files']}, {'docs/design.md', 'docs/日本語\nnotes.md'})

    def test_missing_history_nonancestor_and_git_failure_are_full(self):
        self.assertTrue(ci.git_impact('0' * 40, self.base, cwd=self.root)['product_check'])
        self.write('docs/design.md', 'one\n'); first = self.commit()
        self.git('checkout', '-b', 'other', self.base)
        self.write('docs/design.md', 'two\n'); second = self.commit()
        result = ci.git_impact(first, second, cwd=self.root, merge_base=False)
        self.assertTrue(result['product_check']); self.assertIn(ci.UNKNOWN, result['impacts'])
        with patch.object(ci, 'git', side_effect=OSError('unavailable')):
            self.assertTrue(ci.git_impact(self.base, second, cwd=self.root)['product_check'])

    def test_gitlinks_are_visible_even_when_git_config_ignores_submodules(self):
        self.git('config', 'diff.ignoreSubmodules', 'all')
        previous = self.base
        for operation in ('add', 'update', 'delete'):
            if operation == 'delete':
                self.git('update-index', '--force-remove', 'vendor/module')
            else:
                self.git('update-index', '--add', '--cacheinfo', '160000,' + previous + ',vendor/module')
            self.write('docs/design.md', operation + '\n'); self.git('add', 'docs/design.md')
            self.git('commit', '-m', operation); head = self.git('rev-parse', 'HEAD')
            result = ci.git_impact(previous, head, cwd=self.root)
            with self.subTest(operation=operation):
                self.assertEqual({f['path'] for f in result['files']}, {'vendor/module', 'docs/design.md'})
                self.assertIn(ci.UNKNOWN, result['impacts'])
                self.assertTrue(result['product_check']); self.assertTrue(result['product_check'])
            previous = head


    def test_test_runner_rejects_dirty_or_different_checkout(self):
        self.write('docs/design.md', 'dirty\n')
        command = [sys.executable, str(Path(ci.__file__).resolve()), '--base', self.base, '--run-tests']
        dirty = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True)
        self.assertNotEqual(dirty.returncode, 0)
        self.commit()
        other = subprocess.run(command + ['--head', self.base], cwd=self.root, env=self.env, capture_output=True)
        self.assertNotEqual(other.returncode, 0)

    def test_push_and_pr_events_emit_successful_skip_and_manual_full(self):
        self.write('docs/design.md', 'updated\n'); head = self.commit()
        script = str(Path(ci.__file__).resolve())
        for event_name, data, expected in [('pull_request', {'pull_request': {'base': {'sha': self.base}}}, False),
                                           ('push', {'before': self.base}, False), ('workflow_dispatch', {}, True)]:
            event = self.root / '.git/event.json'; event.write_text(json.dumps(data))
            output = self.root / '.git/output'; output.write_text('')
            summary = self.root / '.git/summary'; summary.write_text('')
            env = dict(self.env, GITHUB_EVENT_NAME=event_name, GITHUB_EVENT_PATH=str(event),
                       GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(summary))
            subprocess.run([sys.executable, script, '--event', '--head', head], cwd=self.root,
                           env=env, check=True, capture_output=True)
            self.assertIn('product_check=' + str(expected).lower(), output.read_text())
            self.assertIn('Change Impact', summary.read_text())


if __name__ == '__main__':
    unittest.main()
