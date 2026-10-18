#!/usr/bin/env python3
"""Validate the shipped product, optionally including the plugin build."""
import argparse
import importlib
from pathlib import Path
import subprocess
import sys

from cli_origin import verify

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--plugin', action='store_true', help='Also validate the integrated plugin')
    parser.add_argument('--plugin-tests-only', action='store_true',
                        help='Keep plugin checks without requesting a distribution ZIP')
    args = parser.parse_args()
    if args.plugin_tests_only and not args.plugin:
        parser.error('--plugin-tests-only requires --plugin')
    verify()
    modules = sorted((ROOT / 'replay_buffer').rglob('*.py'))
    for path in modules:
        compile(path.read_bytes(), str(path), 'exec')
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(ROOT))
    for path in modules:
        module = path.relative_to(ROOT).with_suffix('')
        if module.name == '__init__':
            module = module.parent
        importlib.import_module('.'.join(module.parts))
    print(f'Compiled and imported {len(modules)} CLI modules on Python {sys.version.split()[0]}', flush=True)
    subprocess.run([sys.executable, '-m', 'unittest', 'discover', '-s', 'tests', '-v'],
                   cwd=ROOT, check=True)
    if args.plugin:
        subprocess.run([sys.executable, str(ROOT / 'scripts/workflow/plugin_check.py')] +
                       (['--tests-only'] if args.plugin_tests_only else []), cwd=ROOT, check=True)
        print('ARB_PRODUCT_PLUGIN_CHECK_COMPLETE', flush=True)
