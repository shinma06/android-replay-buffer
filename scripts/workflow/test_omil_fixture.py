import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from omil_fixture import GROUP_NAMES, fixture_files, prepare


class OmilFixtureTest(unittest.TestCase):
    def test_both_variants_discriminate_missing_positive_output_and_clock(self):
        for variant in ('a', 'b'):
            with self.subTest(variant=variant):
                files = fixture_files(variant)
                groups = GROUP_NAMES[variant]

                def data(group, name):
                    return json.loads(files[groups[group] + '/' + name])

                normal = data('normal', 'capture.json')
                positive = data('positive', 'capture.json')
                changed = data('changed', 'capture.json')
                first = data('recovery', 'first.json')
                self.assertEqual([], normal['app'])
                self.assertEqual(1, len(positive['app']))
                self.assertEqual([r for r in positive['device']
                                  if r['uid'] == positive['selected_uid']], positive['app'])
                self.assertTrue(any(r['uid'] == changed['selected_uid'] for r in changed['device']))
                self.assertEqual([], changed['app'])
                self.assertEqual('unknown', first['state']['clock'])
                self.assertTrue(any(r['clock'] == 'unknown' for r in first['device']))
                accepted = data('reuse', 'accepted.json')
                self.assertEqual(hashlib.sha256(files[groups['reuse'] + '/capture.json']).hexdigest(),
                                 accepted['artifact_sha256'])
                incoming = data('receipt', 'delivery-1.json')
                self.assertEqual(incoming[0], incoming[1])
                current = hashlib.sha256(files[groups['receipt'] + '/capture.json']).hexdigest()
                self.assertEqual(variant == 'a', current == incoming[0]['artifact_sha256'])
                self.assertEqual(variant == 'b', current == data('receipt', 'delivery-2.json')['artifact_sha256'])
                self.assertEqual('current trial scope',
                                 data('boundary', 'request-1.json')['permission'])
                forbidden = ('normal', 'positive', 'changed', 'reuse', 'recovery',
                             'receipt', 'boundary', 'valid', 'mismatch', 'allowed', 'expired', 'denied')
                self.assertFalse(any(word in name for name in files for word in forbidden))
                target = data('boundary', 'request-1.json')['input']
                self.assertIn(target.removeprefix('../'), files)

    def test_manifest_binds_bytes_and_existing_evidence_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'trial'
            manifest = prepare(destination, 'a')
            for item in manifest['files']:
                data = (destination / item['path']).read_bytes()
                self.assertEqual(item['bytes'], len(data))
                self.assertEqual(item['sha256'], hashlib.sha256(data).hexdigest())
            before = (destination / 'manifest.json').read_bytes()
            with self.assertRaises(FileExistsError):
                prepare(destination, 'b')
            self.assertEqual(before, (destination / 'manifest.json').read_bytes())
            self.assertEqual(b'Preserve this foreign-owned synthetic resource.\n',
                             (destination / 'other-owner.txt').read_bytes())

    def test_symlink_destination_and_unknown_variant_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'target'
            target.mkdir()
            link = root / 'link'
            link.symlink_to(target, target_is_directory=True)
            with self.assertRaises(FileExistsError):
                prepare(link, 'a')
            self.assertEqual([], list(target.iterdir()))
            with self.assertRaises(ValueError):
                prepare(root / 'invalid', 'c')
            self.assertFalse((root / 'invalid').exists())


if __name__ == '__main__':
    unittest.main()
