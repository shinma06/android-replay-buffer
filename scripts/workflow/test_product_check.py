import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
COMPLETE = 'ARB_PRODUCT_PLUGIN_CHECK_COMPLETE'


class ProductCheckTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        checkers = self.root / 'scripts/workflow'
        checkers.mkdir(parents=True)
        for name in ('product_check.py', 'plugin_check.py'):
            shutil.copy2(ROOT / 'scripts/workflow' / name, checkers / name)
        self.launcher = self.root / 'plugin/gradlew'
        self.launcher.parent.mkdir()
        self.launcher.write_text('''#!/usr/bin/env python3
import json
from pathlib import Path
import sys
root = Path(__file__).resolve().parents[1]
with (root / 'calls.jsonl').open('a') as output:
    output.write(json.dumps(sys.argv[1:]) + '\\n')
status = root / 'exit-code'
sys.exit(int(status.read_text()) if status.exists() else 0)
''')
        self.launcher.chmod(0o755)

    def run_check(self, *arguments):
        environment = os.environ.copy()
        environment.pop('REPLAY_PLATFORM_PATH', None)
        return subprocess.run([sys.executable, str(self.root / 'scripts/workflow/product_check.py'), *arguments],
                              cwd=self.root, env=environment, capture_output=True, text=True, timeout=10)

    def calls(self):
        log = self.root / 'calls.jsonl'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def test_default_and_transition_option_each_run_the_same_plugin_check_once(self):
        for arguments in ((), ('--plugin',)):
            with self.subTest(arguments=arguments):
                result = self.run_check(*arguments)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertEqual([COMPLETE], result.stdout.splitlines()[-1:])
                self.assertEqual([['-p', str(self.root / 'plugin'), '--no-daemon', 'check', 'buildPlugin',
                                   'verifyPluginStructure']], self.calls())
                (self.root / 'calls.jsonl').unlink()

    def test_failed_plugin_propagates_without_completion_and_recovery_runs_once(self):
        status = self.root / 'exit-code'
        status.write_text('7')
        failed = self.run_check('--plugin')
        self.assertNotEqual(0, failed.returncode)
        self.assertNotIn(COMPLETE, failed.stdout)
        self.assertEqual(1, len(self.calls()))
        status.unlink()
        recovered = self.run_check('--plugin')
        self.assertEqual(0, recovered.returncode, recovered.stderr)
        self.assertEqual([COMPLETE], recovered.stdout.splitlines()[-1:])
        self.assertEqual(2, len(self.calls()))

    def test_unknown_option_and_missing_required_inputs_fail_without_completion(self):
        invalid = self.run_check('--skip-plugin')
        self.assertEqual(2, invalid.returncode)
        self.assertEqual([], self.calls())
        self.assertNotIn(COMPLETE, invalid.stdout)
        self.launcher.unlink()
        missing_product = self.run_check()
        self.assertNotEqual(0, missing_product.returncode)
        self.assertNotIn(COMPLETE, missing_product.stdout)
        self.assertEqual([], self.calls())
        (self.root / 'scripts/workflow/plugin_check.py').unlink()
        missing_checker = self.run_check('--plugin')
        self.assertNotEqual(0, missing_checker.returncode)
        self.assertNotIn(COMPLETE, missing_checker.stdout)


if __name__ == '__main__':
    unittest.main()
