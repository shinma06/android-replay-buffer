#!/usr/bin/env python3
"""Run the existing CLI tests without starting ADB, recording, or the daemon."""
from pathlib import Path
import subprocess
import sys

from cli_origin import verify

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
    verify()
    subprocess.run([sys.executable, '-m', 'unittest', 'discover', '-s', 'tests', '-v'],
                   cwd=ROOT, check=True)
