#!/usr/bin/env python3
"""Check all CLI modules and existing tests without starting ADB or the daemon."""
import importlib
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
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
