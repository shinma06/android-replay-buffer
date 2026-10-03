"""Preservation failures must block checks, even in a shallow checkout."""
import contextlib
import io
from pathlib import Path
import shutil
import tempfile
import unittest

import cli_origin
import change_impact as ci


class OriginTests(unittest.TestCase):
    def test_original_and_mutation_detection(self):
        with contextlib.redirect_stdout(io.StringIO()):
            cli_origin.verify()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            shutil.copytree(cli_origin.ROOT / 'docs/cli-origin', root / 'docs/cli-origin')
            # The remaining original files are deliberately absent.
            with self.assertRaisesRegex(ValueError, 'missing'):
                cli_origin.verify(root)
            import json
            manifest = json.loads((root / 'docs/cli-origin/manifest.json').read_text())
            for entry in manifest['files'].values():
                path = root / entry['path']
                path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(cli_origin.ROOT / entry['path'], path)
            for mutation in ('bytes', 'mode', 'symlink'):
                path = root / 'bin/replay'
                if path.exists() or path.is_symlink():
                    path.unlink()
                shutil.copy2(cli_origin.ROOT / 'bin/replay', path)
                if mutation == 'bytes':
                    path.write_text('changed')
                elif mutation == 'mode':
                    path.chmod(0o644)
                else:
                    path.unlink()
                    path.symlink_to(cli_origin.ROOT / 'bin/replay')
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    cli_origin.verify(root)

    def test_plugin_and_origin_inputs_cannot_skip_checks(self):
        for path in ('plugin/build.gradle.kts', 'plugin/src/main/resources/help.md',
                     'plugin/gradle/wrapper/gradle-wrapper.jar', 'plugin/src/main/kotlin/Action.kt'):
            result = ci.classify([(path, ('100644', '000000'))])
            self.assertTrue(result['plugin_check'])
        for path in ('docs/cli-origin/manifest.json', 'docs/cli-origin/README.md',
                     'scripts/workflow/cli_origin.py', '.github/workflows/ci.yml'):
            result = ci.classify([(path, ('100644',))])
            self.assertTrue(result['plugin_check'])
            self.assertTrue(result['product_check'])
