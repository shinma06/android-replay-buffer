#!/usr/bin/env python3
"""Validate the integrated plugin and its bundled CLI without starting an IDE."""
import argparse
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--plugin', action='store_true', help='Compatibility option; the integrated product always includes the plugin')
    parser.add_argument('--plugin-tests-only', action='store_true',
                        help='Keep plugin checks without requesting a distribution ZIP')
    args = parser.parse_args()
    if args.plugin_tests_only and not args.plugin:
        parser.error('--plugin-tests-only requires --plugin')
    subprocess.run([sys.executable, str(ROOT / 'scripts/workflow/plugin_check.py')] +
                   (['--tests-only'] if args.plugin_tests_only else []), cwd=ROOT, check=True)
    print('ARB_PRODUCT_PLUGIN_CHECK_COMPLETE', flush=True)
