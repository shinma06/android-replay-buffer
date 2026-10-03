"""Build the plugin ZIP and verify its structure, without launching an IDE."""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]

if __name__ == '__main__':
    command = [str(ROOT / 'plugin/gradlew'), '-p', str(ROOT / 'plugin'), '--no-daemon',
               'check', 'buildPlugin', 'verifyPluginStructure']
    if os.environ.get('REPLAY_PLATFORM_PATH'):
        command += ['-PuseLocalPlatform=true', '-PplatformPath=' + os.environ['REPLAY_PLATFORM_PATH']]
    subprocess.run(command, cwd=ROOT, check=True)
