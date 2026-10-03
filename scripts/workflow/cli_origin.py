"""Detect changes to the preserved CLI without Git history or external tools."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def verify(root=ROOT):
    manifest = json.loads((root / 'docs/cli-origin/manifest.json').read_text())
    errors = []
    for original, entry in manifest['files'].items():
        path = root / entry['path']
        if path.is_symlink() or not path.is_file():
            errors.append(original + ': missing or replaced by symlink')
        elif hashlib.sha256(path.read_bytes()).hexdigest() != entry['sha256']:
            errors.append(original + ': original bytes changed')
        elif bool(path.stat().st_mode & 0o111) != (entry['mode'] == '100755'):
            errors.append(original + ': executable mode changed')
    if errors:
        raise ValueError('CLI origin preservation failed:\n' + '\n'.join(errors))
    print(f"CLI origin: {len(manifest['files'])} files match {manifest['source_commit']}")


if __name__ == '__main__':
    verify()
