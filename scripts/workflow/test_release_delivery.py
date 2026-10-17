import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import release_delivery as delivery

SOURCE = 'a' * 40
NEXT = 'b' * 40
MAIN = 'c' * 40


def package(source=SOURCE, version='0.2.0', dirty='false', plugin_id=None):
    jar_bytes = io.BytesIO()
    with zipfile.ZipFile(jar_bytes, 'w') as jar:
        jar.writestr('META-INF/plugin.xml', '<idea-plugin><id>' +
                     (plugin_id or 'io.github.shinma06.android-replay-buffer') +
                     '</id><version>' + version + '</version></idea-plugin>')
        jar.writestr('replay/deps/identity.properties', f'source={source}\ndirty={dirty}\nversion={version}\n')
    product = io.BytesIO()
    with zipfile.ZipFile(product, 'w') as archive:
        archive.writestr('Android Replay Buffer/lib/plugin.jar', jar_bytes.getvalue())
    return product.getvalue()


class Repository:
    def __init__(self):
        self.releases = []
        self.refs = {}
        self.assets = {}
        self.files = {}
        self.next_id = 1
        self.head = SOURCE
        self.mutations = []
        self.fail_upload = False
        self.fail_after_patch = False
        self.fail_patch = False
        self.advance_after_upload = False

    def pages(self, path):
        if path.startswith('releases?'):
            return copy.deepcopy(self.releases)
        number = int(path.split('/')[1])
        return [copy.deepcopy(a) for a in self.assets.values() if a['release'] == number]

    def api(self, path, method='GET', data=None):
        if path == 'git/ref/heads/develop':
            return {'object': {'sha': self.head}}
        if path.startswith('git/matching-refs/tags/'):
            tag = path.removeprefix('git/matching-refs/tags/')
            return [{'ref': 'refs/tags/' + tag, 'object': self.refs[tag]}] if tag in self.refs else []
        if path.startswith('git/ref/tags/'):
            return {'object': self.refs[path.removeprefix('git/ref/tags/')]}
        self.mutations.append((path, method, copy.deepcopy(data)))
        if path == 'releases' and method == 'POST':
            result = dict(data, id=self.next_id, immutable=False)
            self.next_id += 1
            self.releases.append(result)
            return copy.deepcopy(result)
        if path.startswith('releases/assets/') and method == 'DELETE':
            self.assets.pop(int(path.split('/')[2]))
            return None
        if path.startswith('releases/') and method == 'PATCH':
            if self.fail_patch:
                raise OSError('connection failed before PATCH')
            result = next(r for r in self.releases if r['id'] == int(path.split('/')[1]))
            result.update(data)
            self.refs.setdefault(result['tag_name'], {'type': 'commit', 'sha': result['target_commitish'], 'url': 'tag-url'})
            if self.fail_after_patch:
                raise OSError('connection lost after PATCH')
            return copy.deepcopy(result)
        raise AssertionError((path, method, data))

    def command(self, args):
        if args[:3] == ['gh', 'pr', 'checks']:
            return json.dumps([{'name': n, 'state': 'SUCCESS'} for n in (*delivery.CI_CHECKS, delivery.CONTEXT)])
        url = next(a for a in args if a.startswith('https://uploads.github.com/'))
        name = url.split('name=')[1]
        data = Path(args[args.index('--input') + 1]).read_bytes()
        release = int(url.split('/releases/')[1].split('/')[0])
        asset = {'id': self.next_id, 'release': release, 'name': name, 'state': 'uploaded',
                 'size': len(data), 'digest': 'sha256:' + hashlib.sha256(data).hexdigest()}
        self.next_id += 1
        self.assets[asset['id']] = asset
        self.files[asset['id']] = data
        self.mutations.append(('upload', 'POST', name))
        if self.advance_after_upload:
            self.head = NEXT
        if self.fail_upload:
            raise OSError('upload response lost')
        return json.dumps(asset)

    def download(self, path, destination):
        destination.write_bytes(self.files[int(path.split('/')[2])])


class ReleaseDeliveryTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.archive = self.directory / 'product.zip'
        self.archive.write_bytes(package())
        self.repo = Repository()
        for name in ('api', 'pages', 'command', 'download'):
            self.addCleanup(patch.stopall)
            patch.object(delivery, name, getattr(self.repo, name)).start()

    def record(self, channel='early', source=SOURCE):
        return dict(delivery.identity(self.archive, source, delivery.digest(self.archive)), schema=1,
                    repository=delivery.REPO, channel=channel, pr=286)

    def test_identity_rejects_wrong_source_hash_dirty_plugin_and_ambiguous_archive(self):
        self.assertEqual(delivery.identity(self.archive, SOURCE, delivery.digest(self.archive))['version'], '0.2.0')
        with self.assertRaises(ValueError):
            delivery.identity(self.archive, NEXT, delivery.digest(self.archive))
        with self.assertRaises(ValueError):
            delivery.identity(self.archive, SOURCE, '0' * 64)
        for data in (package(dirty='true'), package(plugin_id='other.plugin'), package(version='bad\nversion')):
            self.archive.write_bytes(data)
            with self.assertRaises(ValueError):
                delivery.identity(self.archive, SOURCE, delivery.digest(self.archive))
        with zipfile.ZipFile(self.archive, 'w') as archive:
            archive.writestr('../outside.zip', b'unsafe')
        with self.assertRaises(ValueError):
            delivery.identity(self.archive, SOURCE, delivery.digest(self.archive))

    def test_new_early_update_readback_and_unchanged_retry(self):
        first = self.record()
        url = delivery.publish(self.archive, first, MAIN)
        self.assertTrue(url.endswith('/early'))
        first_tag = copy.deepcopy(self.repo.refs['early'])
        before = len(self.repo.mutations)
        delivery.publish(self.archive, first, MAIN)
        self.assertEqual(len(self.repo.mutations), before)
        self.archive.write_bytes(package(NEXT))
        self.repo.head = NEXT
        second = self.record(source=NEXT)
        delivery.publish(self.archive, second, 'd' * 40)
        self.assertEqual(self.repo.refs['early'], first_tag)
        self.assertEqual(len(self.repo.releases), 1)
        self.assertEqual([a['name'] for a in self.repo.assets.values()], [delivery.asset_name(second)])
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), second)

    def test_stale_run_and_foreign_release_never_mutate(self):
        first = self.record()
        self.repo.head = NEXT
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, first, MAIN)
        self.assertEqual(self.repo.mutations, [])
        self.repo.head = SOURCE
        self.repo.releases.append({'tag_name': 'early', 'body': 'human-owned release'})
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, first, MAIN)
        self.assertEqual(self.repo.mutations, [])

    def test_stable_is_immutable_and_existing_tag_cannot_be_moved(self):
        record = self.record('stable')
        delivery.publish(self.archive, record, MAIN)
        before = len(self.repo.mutations)
        delivery.publish(self.archive, record, MAIN)
        self.assertEqual(len(self.repo.mutations), before)
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, record, NEXT)
        self.archive.write_bytes(package(NEXT))
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, self.record('stable', NEXT), MAIN)
        self.assertEqual(len(self.repo.mutations), before)

    def test_unknown_upload_outcome_resumes_without_duplicate(self):
        record = self.record()
        self.repo.fail_upload = True
        with self.assertRaises(OSError):
            delivery.publish(self.archive, record, MAIN)
        self.assertTrue(self.repo.releases[0]['draft'])
        self.repo.fail_upload = False
        delivery.publish(self.archive, record, MAIN)
        self.assertEqual(sum(path == 'upload' for path, _, _ in self.repo.mutations), 1)
        self.assertFalse(self.repo.releases[0]['draft'])

    def test_uncertain_patch_is_read_back_on_retry(self):
        record = self.record()
        self.repo.fail_after_patch = True
        with self.assertRaises(OSError):
            delivery.publish(self.archive, record, MAIN)
        self.repo.fail_after_patch = False
        before = len(self.repo.mutations)
        delivery.publish(self.archive, record, MAIN)
        self.assertEqual(len(self.repo.mutations), before)

    def test_failed_update_keeps_old_working_release_and_new_stage(self):
        old = self.record()
        delivery.publish(self.archive, old, MAIN)
        self.archive.write_bytes(package(NEXT))
        self.repo.head = NEXT
        new = self.record(source=NEXT)
        self.repo.fail_patch = True
        with self.assertRaises(OSError):
            delivery.publish(self.archive, new, MAIN)
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), old)
        self.assertEqual(len(self.repo.assets), 2)
        self.repo.fail_patch = False
        delivery.publish(self.archive, new, MAIN)
        self.assertEqual(len(self.repo.assets), 1)
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), new)

    def test_branch_advance_after_upload_does_not_publish_stale_asset(self):
        self.repo.advance_after_upload = True
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, self.record(), MAIN)
        self.assertTrue(self.repo.releases[0]['draft'])
        self.assertFalse(any(method == 'PATCH' for _, method, _ in self.repo.mutations))

    def test_modified_asset_is_rejected_and_preserved(self):
        record = self.record()
        delivery.publish(self.archive, record, MAIN)
        asset = next(iter(self.repo.assets.values()))
        self.repo.files[asset['id']] = b'changed bytes'
        before = len(self.repo.mutations)
        with self.assertRaises(ValueError):
            delivery.publish(self.archive, record, MAIN)
        self.assertEqual(len(self.repo.mutations), before)

    def test_human_edited_release_notes_are_preserved(self):
        record = self.record()
        delivery.publish(self.archive, record, MAIN)
        self.repo.releases[0]['body'] = 'Human note\n' + self.repo.releases[0]['body']
        original = self.repo.releases[0]['body']
        before = len(self.repo.mutations)
        with self.assertRaisesRegex(ValueError, 'notes changed'):
            delivery.publish(self.archive, record, MAIN)
        self.assertEqual(self.repo.releases[0]['body'], original)
        self.assertEqual(len(self.repo.mutations), before)

    def test_missing_failed_cancelled_duplicate_required_checks_are_not_success(self):
        for state in ('FAILURE', 'CANCELLED', 'PENDING', 'TIMED_OUT', None):
            data = [{'name': n, 'state': 'SUCCESS'} for n in (*delivery.CI_CHECKS, delivery.CONTEXT)]
            data = [item for item in data if item['name'] != 'test']
            if state:
                data.append({'name': 'test', 'state': state})
            with self.subTest(state=state), patch.object(delivery, 'command', return_value=json.dumps(data)):
                with self.assertRaises(ValueError):
                    delivery.checks({'number': 1})
        data += [{'name': 'test', 'state': 'SUCCESS'}, {'name': 'test', 'state': 'FAILURE'}]
        with patch.object(delivery, 'command', return_value=json.dumps(data)), self.assertRaises(ValueError):
            delivery.checks({'number': 1})

    def test_artifact_transport_and_workflow_binding(self):
        transport = io.BytesIO()
        with zipfile.ZipFile(transport, 'w') as archive:
            archive.writestr('plugin.zip', package())
        self.repo.files[12] = transport.getvalue()
        run = {'repository': {'full_name': delivery.REPO}, 'head_repository': {'full_name': delivery.REPO},
               'head_sha': SOURCE, 'status': 'completed', 'conclusion': 'success', 'event': 'push',
               'head_branch': 'develop', 'path': '.github/workflows/ci.yml'}
        item = {'id': 12, 'name': 'plugin-' + SOURCE, 'expired': False,
                'size_in_bytes': len(transport.getvalue()),
                'digest': 'sha256:' + hashlib.sha256(transport.getvalue()).hexdigest()}
        listing = {'total_count': 1, 'artifacts': [item]}
        for field, bad in (('head_sha', NEXT), ('event', 'pull_request'), ('conclusion', 'failure'),
                           ('path', '.github/workflows/other.yml'), ('repository', {'full_name': 'other/repo'})):
            with self.subTest(field=field), patch.object(delivery, 'api', return_value=dict(run, **{field: bad})):
                with self.assertRaises(ValueError):
                    delivery.artifact(42, SOURCE, self.directory)
        with patch.object(delivery, 'api', side_effect=[run, listing]):
            product, returned = delivery.artifact(42, SOURCE, self.directory)
        self.assertEqual(product.read_bytes(), package())
        self.assertEqual(returned, run)

    def test_candidate_is_successful_trusted_main_workflow_and_exact_accepted_bytes(self):
        expected = delivery.digest(self.archive)
        item = {'id': 12, 'name': 'candidate-' + expected, 'expired': False, 'workflow_run': {'id': 42}}
        listing = {'total_count': 1, 'artifacts': [item]}
        run = {'repository': {'full_name': delivery.REPO}, 'head_repository': {'full_name': delivery.REPO},
               'head_sha': MAIN, 'head_branch': 'main', 'event': 'workflow_dispatch',
               'path': '.github/workflows/release-candidate.yml', 'status': 'completed', 'conclusion': 'success'}
        with patch.object(delivery, 'git') as git, patch.object(delivery, 'download_artifact', return_value=self.archive):
            with patch.object(delivery, 'api', side_effect=[listing, run]):
                self.assertEqual(delivery.candidate_archive(expected, self.directory), self.archive)
            git.assert_called_once_with('merge-base', '--is-ancestor', MAIN, 'origin/main')
            for field, bad in (('head_branch', 'develop'), ('event', 'pull_request'), ('conclusion', 'failure'),
                               ('path', '.github/workflows/other.yml')):
                with self.subTest(field=field), patch.object(delivery, 'api', side_effect=[listing, dict(run, **{field: bad})]):
                    with self.assertRaises(ValueError):
                        delivery.candidate_archive(expected, self.directory)
            self.archive.write_bytes(package(NEXT))
            with patch.object(delivery, 'api', side_effect=[listing, run]), self.assertRaises(ValueError):
                delivery.candidate_archive(expected, self.directory)

    def test_only_explicitly_skipped_producer_can_omit_early_artifact(self):
        run = {'repository': {'full_name': delivery.REPO}, 'head_repository': {'full_name': delivery.REPO},
               'head_sha': SOURCE, 'status': 'completed', 'conclusion': 'success', 'event': 'push',
               'head_branch': 'develop', 'path': '.github/workflows/ci.yml'}
        listing = {'total_count': 0, 'artifacts': []}
        for conclusion in ('skipped', 'success', 'failure', None):
            jobs = {'total_count': 1, 'jobs': [{'name': 'test', 'conclusion': 'success',
                    'steps': [{'name': 'Run actions/upload-artifact@v4', 'conclusion': conclusion}]}]}
            with patch.object(delivery, 'api', side_effect=[run, listing, jobs]):
                if conclusion == 'skipped':
                    self.assertEqual(delivery.artifact(42, SOURCE, self.directory), (None, run))
                else:
                    with self.assertRaises(ValueError):
                        delivery.artifact(42, SOURCE, self.directory)
        self.assertEqual(self.repo.mutations, [])
        for data in ({'total_count': 0, 'artifacts': []},
                     {'total_count': 2, 'artifacts': [item]},
                     {'total_count': 1, 'artifacts': [dict(item, expired=True)]}):
            with patch.object(delivery, 'api', return_value=data), self.assertRaises(ValueError):
                delivery.candidate_archive(expected, self.directory)

    def test_promotion_reconstructs_reviewed_base_and_requires_complete_acceptance(self):
        pr = {'number': 286, 'merged': True, 'merge_commit_sha': MAIN,
              'base': {'ref': 'main', 'sha': MAIN, 'repo': {'full_name': delivery.REPO}},
              'head': {'sha': NEXT, 'repo': {'full_name': delivery.REPO}}}
        expected = delivery.digest(self.archive)
        accepted = {'mode': 'promotion', 'candidate': SOURCE, 'gui_complete': True}
        record = {'candidate': SOURCE, 'artifacts': {'plugin': expected}}
        def git(*args):
            if args[0] == 'rev-list':
                return f'{MAIN} {SOURCE} {NEXT}'
            if args[0] == 'show':
                return json.dumps(record)
            return ''
        with patch.object(delivery, 'api', return_value=pr), patch.object(delivery, 'git', side_effect=git), \
             patch.object(delivery, 'verify_pr', return_value=accepted) as verify:
            self.assertEqual(delivery.promotion_gate(286)[:3], (SOURCE, expected, MAIN))
            self.assertEqual(verify.call_args.args[0]['base']['sha'], SOURCE)
            for result in (dict(accepted, gui_complete=False), dict(accepted, mode='tooling'),
                           dict(accepted, candidate=NEXT)):
                verify.return_value = result
                with self.assertRaises(ValueError):
                    delivery.promotion_gate(286)
            verify.return_value = accepted
            with patch.object(delivery, 'git', return_value=f'{MAIN} {SOURCE}'), self.assertRaises(ValueError):
                delivery.promotion_gate(286)
            pr['merged'] = False
            with self.assertRaises(ValueError):
                delivery.promotion_gate(286)
        self.assertEqual(self.repo.mutations, [])

    def test_stable_uses_exact_formal_version_without_rebuilding(self):
        expected = delivery.digest(self.archive)
        with patch.object(delivery, 'promotion_gate', return_value=(SOURCE, expected, MAIN, {})), \
             patch.object(delivery, 'candidate_archive', return_value=self.archive):
            url = delivery.stable_release(286, self.directory, version='0.2.0')
            self.assertTrue(url.endswith('/v0.2.0'))
            with self.assertRaises(ValueError):
                delivery.stable_release(286, self.directory, version='0.3.0')
        self.assertEqual(self.archive.read_bytes(), package())

    def test_main_tooling_commit_does_not_publish_and_only_unique_promotion_dispatches(self):
        promotion = {'number': 286, 'merge_commit_sha': MAIN, 'base': {'ref': 'main'},
                     'body': 'Integration: promotion\n'}
        with patch.object(delivery, 'pages', return_value=[]), patch.object(delivery, 'stable_release') as stable:
            self.assertIn('No promotion', delivery.promoted(MAIN, self.directory))
            stable.assert_not_called()
            with patch.object(delivery, 'pages', return_value=[promotion]):
                delivery.promoted(MAIN, self.directory)
            stable.assert_called_once_with(286, self.directory)
            with patch.object(delivery, 'pages', return_value=[promotion, promotion]), self.assertRaises(ValueError):
                delivery.promoted(MAIN, self.directory)

    def test_artifact_metadata_size_is_checked_before_download(self):
        for size in (None, 0, delivery.MAX_ARCHIVE + 1):
            item = {'id': 12, 'size_in_bytes': size, 'digest': 'sha256:' + 'a' * 64}
            with patch.object(delivery, 'download') as download, self.assertRaises(ValueError):
                delivery.download_artifact(item, self.directory)
            download.assert_not_called()


if __name__ == '__main__':
    unittest.main()
