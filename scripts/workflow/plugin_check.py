"""Run plugin checks locally; the full mode also requests ZIP/structure verification.

Tests-only omits distribution tasks here, but Gradle tests may still build their
archive fixture. Neither mode starts an IDE or publishes the resulting archive.
"""
import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--tests-only', action='store_true')
    args = parser.parse_args()
    command = [str(ROOT / 'plugin/gradlew'), '-p', str(ROOT / 'plugin'), '--no-daemon',
               'check'] + ([] if args.tests_only else ['buildPlugin', 'verifyPluginStructure'])
    if os.environ.get('REPLAY_PLATFORM_PATH'):
        command += ['-PuseLocalPlatform=true', '-PplatformPath=' + os.environ['REPLAY_PLATFORM_PATH']]
    subprocess.run(command, cwd=ROOT, check=True)
