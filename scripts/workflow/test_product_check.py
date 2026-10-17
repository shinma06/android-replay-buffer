"""Declared-runtime regression using owned CLI copies; never start the daemon."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ProductCheckTests(unittest.TestCase):
    def copy_cli(self, root):
        for name in ('replay_buffer', 'tests'):
            shutil.copytree(ROOT / name, root / name, ignore=shutil.ignore_patterns('__pycache__'))
        checker = root / 'scripts/workflow/product_check.py'
        checker.parent.mkdir(parents=True)
        shutil.copy2(ROOT / 'scripts/workflow/product_check.py', checker)
        origin = ROOT / 'scripts/workflow/cli_origin.py'
        if origin.is_file():
            shutil.copy2(origin, checker.parent / origin.name)
            for name in ('docs/cli-origin', 'bin', 'launchd'):
                shutil.copytree(ROOT / name, root / name)
            for name in ('config.json.example', 'pyproject.toml'):
                shutil.copy2(ROOT / name, root / name)
        return checker

    @unittest.skipUnless(sys.version_info[:2] == (3, 9), 'Run on the declared Python 3.9 runtime')
    def test_unsupported_syntax_is_rejected_and_valid_cli_recovers_on_python39(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checker = self.copy_cli(root)
            origin = checker.parent / 'cli_origin.py'
            env = dict(os.environ, PYTHONDONTWRITEBYTECODE='1')
            valid = subprocess.run([sys.executable, str(checker)], cwd=root, env=env,
                                   text=True, capture_output=True, timeout=30)
            self.assertEqual(valid.returncode, 0, valid.stderr)
            self.assertIn('Ran 2 tests', valid.stderr)
            target = root / 'replay_buffer/save.py'
            original = target.read_bytes()
            target.write_text(target.read_text() + '\nmatch None:\n    case _:\n        pass\n')
            if origin.is_file():
                # Keep fixture identity valid so unsupported syntax reaches compile.
                manifest_path = root / 'docs/cli-origin/manifest.json'
                manifest_bytes = manifest_path.read_bytes()
                manifest = json.loads(manifest_bytes)
                manifest['files']['replay_buffer/save.py']['sha256'] = hashlib.sha256(target.read_bytes()).hexdigest()
                manifest_path.write_text(json.dumps(manifest))
            invalid = subprocess.run([sys.executable, str(checker)], cwd=root, env=env,
                                     text=True, capture_output=True, timeout=30)
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn('SyntaxError', invalid.stderr)
            self.assertIn('save.py', invalid.stderr)
            target.write_bytes(original)
            if origin.is_file():
                manifest_path.write_bytes(manifest_bytes)
            recovered = subprocess.run([sys.executable, str(checker)], cwd=root, env=env,
                                       text=True, capture_output=True, timeout=30)
            self.assertEqual(recovered.returncode, 0, recovered.stderr)
            self.assertIn('Ran 2 tests', recovered.stderr)

    def test_plugin_runs_once_after_cli_and_propagates_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checker = self.copy_cli(root)
            plugin = checker.parent / 'plugin_check.py'
            calls = root / 'plugin-calls'
            source = "with open('plugin-calls', 'a') as output:\n    output.write('run\\n')\n"
            plugin.write_text(source)
            env = dict(os.environ, PYTHONDONTWRITEBYTECODE='1')

            def run(*arguments):
                return subprocess.run([sys.executable, str(checker), *arguments], cwd=root,
                                      env=env, text=True, capture_output=True, timeout=30)

            self.assertEqual(run().returncode, 0)
            self.assertFalse(calls.exists())
            valid = run('--plugin')
            self.assertEqual(valid.returncode, 0, valid.stderr)
            self.assertEqual(calls.read_text(), 'run\n')
            failure = root / 'tests/test_injected_failure.py'
            failure.write_text('import unittest\nclass Failure(unittest.TestCase):\n'
                               '    def test_failure(self): self.fail("owned CLI test failure")\n')
            invalid = run('--plugin')
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn('owned CLI test failure', invalid.stderr)
            self.assertEqual(calls.read_text(), 'run\n')
            failure.unlink()
            plugin.write_text(source + 'raise SystemExit(17)\n')
            self.assertNotEqual(run('--plugin').returncode, 0)
            self.assertEqual(calls.read_text(), 'run\nrun\n')
            plugin.unlink()
            self.assertNotEqual(run('--plugin').returncode, 0)
            self.assertEqual(calls.read_text(), 'run\nrun\n')
            plugin.write_text(source)
            recovered = run('--plugin')
            self.assertEqual(recovered.returncode, 0, recovered.stderr)
            self.assertEqual(calls.read_text(), 'run\nrun\nrun\n')


if __name__ == '__main__':
    unittest.main()
