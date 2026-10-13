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
    @unittest.skipUnless(sys.version_info[:2] == (3, 9), 'Run in the CLI Python 3.9 job')
    def test_unimported_module_with_python310_syntax_fails_python39_check(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
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
            env = dict(os.environ, PYTHONDONTWRITEBYTECODE='1')
            valid = subprocess.run([sys.executable, str(checker)], cwd=root, env=env,
                                   text=True, capture_output=True, timeout=30)
            self.assertEqual(valid.returncode, 0, valid.stderr)
            self.assertIn('Ran 2 tests', valid.stderr)
            target = root / 'replay_buffer/save.py'
            target.write_text(target.read_text() + '\nmatch None:\n    case _:\n        pass\n')
            if origin.is_file():
                # Keep fixture identity valid so unsupported syntax reaches compile.
                manifest_path = root / 'docs/cli-origin/manifest.json'
                manifest = json.loads(manifest_path.read_text())
                manifest['files']['replay_buffer/save.py']['sha256'] = hashlib.sha256(target.read_bytes()).hexdigest()
                manifest_path.write_text(json.dumps(manifest))
            invalid = subprocess.run([sys.executable, str(checker)], cwd=root, env=env,
                                     text=True, capture_output=True, timeout=30)
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn('SyntaxError', invalid.stderr)
            self.assertIn('save.py', invalid.stderr)


if __name__ == '__main__':
    unittest.main()
