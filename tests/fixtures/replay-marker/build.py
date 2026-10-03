#!/usr/bin/env python3
"""Build only the disposable APKs; never install or contact ADB."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--offline', action='store_true')
args = parser.parse_args()
java = Path(os.environ['JAVA_HOME'])
source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
dirty_state = subprocess.check_output(['git', 'status', '--porcelain'], cwd=root)
private = root / '.private'
private.mkdir(mode=0o700, exist_ok=True)
key = private / 'qa.keystore'
if not key.exists():
    subprocess.run([str(java / 'bin' / 'keytool'), '-genkeypair', '-keystore', str(key),
                    '-alias', 'androiddebugkey', '-storepass', 'android', '-keypass', 'android',
                    '-keyalg', 'RSA', '-keysize', '2048', '-validity', '3650',
                    '-dname', 'CN=Replay QA Fixture'], check=True)
    key.chmod(0o600)
wrapper = root / ('gradlew.bat' if os.name == 'nt' else 'gradlew')
command = [str(wrapper), '--no-daemon', ':appA:assembleDebug', ':appB:assembleDebug',
           ':appA:lintDebug', ':appB:lintDebug']
if args.offline:
    command.append('--offline')
subprocess.run(command, cwd=root, check=True)
if (source != subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        or dirty_state != subprocess.check_output(['git', 'status', '--porcelain'], cwd=root)):
    raise SystemExit('Source changed during build; discard these APKs and rebuild.')
dirty = bool(dirty_state)
apks = {}
for module in ('appA', 'appB'):
    apk = root / module / 'build' / 'outputs' / 'apk' / 'debug' / f'{module}-debug.apk'
    apks[module] = {'file': str(apk.relative_to(root)), 'sha256': hashlib.sha256(apk.read_bytes()).hexdigest()}
identity = {'schema': 1, 'source': source, 'dirty': dirty, 'apks': apks}
out = root / 'build' / 'fixture-identity.json'
out.parent.mkdir(exist_ok=True)
out.write_text(json.dumps(identity, indent=2) + '\n')
print(json.dumps(identity, indent=2))
