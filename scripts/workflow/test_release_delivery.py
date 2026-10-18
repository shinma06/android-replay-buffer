import copy
import hashlib
import io
import json
import os
from pathlib import Path
import sys
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
        self.main = MAIN
        self.workflow_changes = ''
        self.mutations = []
        self.fail_upload = False
        self.fail_after_patch = False
        self.fail_patch = False
        self.advance_after_upload = False
        self.advance_workflows_after_upload = False
        self.latest = None

    def pages(self, path):
        if path.startswith('releases?'):
            return copy.deepcopy(self.releases)
        number = int(path.split('/')[1])
        return [copy.deepcopy(a) for a in self.assets.values() if a['release'] == number]

    def api(self, path, method='GET', data=None):
        if path == '':
            return {'default_branch': 'main'}
        if path == 'git/ref/heads/main':
            return {'object': {'sha': self.main}}
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
            if data.get('make_latest') == 'true':
                self.latest = result['id']
            self.refs.setdefault(result['tag_name'], {'type': 'commit', 'sha': result['target_commitish'], 'url': 'tag-url'})
            if self.fail_after_patch:
                raise OSError('connection lost after PATCH')
            return copy.deepcopy(result)
        raise AssertionError((path, method, data))

    def git(self, *args):
        return self.workflow_changes if args[0] == 'diff' else ''

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
        if self.advance_workflows_after_upload:
            self.main = NEXT
            self.workflow_changes = '.github/workflows/ci.yml'
        if self.fail_upload:
            raise OSError('upload response lost')
        return json.dumps(asset)

    def download(self, path, destination, accept='application/octet-stream'):
        destination.write_bytes(self.files[int(path.split('/')[2])])


class DeliveryCredentialsTest(unittest.TestCase):
    def test_workflow_token_reaches_reads_pages_checks_and_upload_commands(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            executable = root / 'gh'
            executable.write_text(f'''#!{sys.executable}
import json, os, sys
assert os.environ.get('GH_TOKEN') == 'workflow-token-fixture'
assert 'repos/{delivery.REPO}/' not in sys.argv
if sys.argv[1:3] == ['pr', 'checks']:
    print(json.dumps([{{'name': n, 'state': 'SUCCESS'}} for n in {(*delivery.CI_CHECKS, delivery.CONTEXT)!r}]))
elif '--slurp' in sys.argv:
    print('[[], []]')
else:
    print('{{"authenticated": true}}')
''')
            executable.chmod(0o755)
            with patch.dict(os.environ, {'GH_TOKEN': 'workflow-token-fixture', 'GH_CONFIG_DIR': folder,
                                         'PATH': folder + os.pathsep + os.environ['PATH']}):
                self.assertTrue(delivery.api('releases')['authenticated'])
                self.assertTrue(delivery.api('')['authenticated'])
                self.assertEqual(delivery.pages('releases?per_page=100'), [])
                delivery.checks({'number': 1})
                self.assertTrue(json.loads(delivery.command(['gh', 'api', '--method', 'POST',
                    'https://uploads.github.com/repos/' + delivery.REPO + '/releases/1/assets']))['authenticated'])


class DeliveryDownloadTest(unittest.TestCase):
    def test_artifact_and_release_asset_use_their_download_media_types(self):
        product = package()
        transport = io.BytesIO()
        with zipfile.ZipFile(transport, 'w') as archive:
            archive.writestr('plugin.zip', product)
        responses = {
            f'repos/{delivery.REPO}/actions/artifacts/12/zip':
                ('application/vnd.github+json', transport.getvalue()),
            f'repos/{delivery.REPO}/releases/assets/34':
                ('application/octet-stream', product),
        }

        def respond(args, *, stdout, check, timeout):
            accept, data = responses[args[2]]
            self.assertIn('Accept: ' + accept, args)
            self.assertTrue(check)
            self.assertEqual(timeout, 180)
            stdout.write(data)

        item = {'id': 12, 'size_in_bytes': len(transport.getvalue()),
                'digest': 'sha256:' + hashlib.sha256(transport.getvalue()).hexdigest()}
        with (tempfile.TemporaryDirectory() as folder,
              patch.object(delivery.subprocess, 'run', side_effect=respond) as run):
            archive = delivery.download_artifact(item, Path(folder))
            self.assertEqual(archive.read_bytes(), product)
            delivery.verify_asset({'id': 34, 'state': 'uploaded', 'size': len(product),
                                   'digest': 'sha256:' + hashlib.sha256(product).hexdigest()},
                                  {'size': len(product), 'sha256': hashlib.sha256(product).hexdigest()})
            self.assertEqual(run.call_count, 2)


class ReleaseDeliveryTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.archive = self.directory / 'product.zip'
        self.archive.write_bytes(package())
        self.repo = Repository()
        self.enterContext(patch.dict(os.environ, {'GITHUB_ACTIONS': 'false', 'ARB_DELIVERY_APP_TOKEN': 'false'}))
        for name in ('api', 'pages', 'command', 'download', 'git'):
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

    def test_zip_changed_after_identity_is_rejected_before_remote_writes(self):
        accepted = self.record('stable')
        self.archive.write_bytes(package(NEXT))
        with self.assertRaisesRegex(ValueError, 'ZIP changed after identity'):
            delivery.publish(self.archive, accepted, MAIN)
        self.assertEqual(self.repo.mutations, [])

    def test_original_path_replacement_during_upload_cannot_replace_pinned_bytes(self):
        accepted = self.record('stable')
        original = self.archive.read_bytes()
        upload = self.repo.command
        def replaced(args):
            self.archive.write_bytes(package(NEXT))
            return upload(args)
        with patch.object(delivery, 'command', side_effect=replaced):
            delivery.publish(self.archive, accepted, MAIN)
        self.assertNotEqual(self.archive.read_bytes(), original)
        self.assertEqual(list(self.repo.files.values()), [original])
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), accepted)

    def test_identity_swap_between_digest_and_parse_is_rejected_before_remote_writes(self):
        original = self.archive.read_bytes()
        replacement = package(version='0.3.0')
        self.assertEqual(len(original), len(replacement))
        expected = hashlib.sha256(original).hexdigest()
        digest = delivery.digest
        def swapped(path):
            result = digest(path)
            self.archive.write_bytes(replacement)
            return result
        with patch.object(delivery, 'digest', side_effect=swapped):
            accepted = dict(delivery.identity(self.archive, SOURCE, expected), schema=1,
                            repository=delivery.REPO, channel='stable', pr=286)
        self.assertEqual(accepted['version'], '0.3.0')
        self.assertEqual(accepted['sha256'], expected)
        self.archive.write_bytes(original)
        with self.assertRaisesRegex(ValueError, 'Fixed ZIP identity differs'):
            delivery.publish(self.archive, accepted, MAIN)
        self.assertEqual(self.repo.mutations, [])

    def test_asset_verification_uses_accepted_hash_even_if_staged_path_changes(self):
        accepted = self.record('stable')
        upload = self.repo.command
        def replaced(args):
            Path(args[args.index('--input') + 1]).write_bytes(package(NEXT))
            return upload(args)
        with patch.object(delivery, 'command', side_effect=replaced), \
             self.assertRaisesRegex(ValueError, 'asset metadata differs'):
            delivery.publish(self.archive, accepted, MAIN)
        self.assertTrue(self.repo.releases[0]['draft'])
        self.assertFalse(any(method == 'PATCH' for _, method, _ in self.repo.mutations))

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

    def test_draft_recovery_retains_its_target_after_main_advances(self):
        record = self.record()
        self.repo.fail_upload = True
        with self.assertRaises(OSError):
            delivery.publish(self.archive, record, MAIN)
        self.repo.fail_upload = False
        self.repo.main = NEXT
        delivery.publish(self.archive, record, NEXT)
        self.assertFalse(self.repo.releases[0]['draft'])
        self.assertEqual(self.repo.refs['early']['sha'], MAIN)
        self.assertEqual(sum(path == 'upload' for path, _, _ in self.repo.mutations), 1)

    def test_automatic_workflow_permission_boundary_precedes_writes_and_local_recovery_keeps_identity(self):
        record = self.record('stable')
        self.repo.main = NEXT
        self.repo.workflow_changes = '.github/workflows/ci.yml'
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}), self.assertRaisesRegex(ValueError, 'Workflows'):
            delivery.publish(self.archive, record, MAIN)
        self.assertEqual(self.repo.mutations, [])
        # The owner uses the existing separately authorized local gh credential.
        delivery.publish(self.archive, record, MAIN)
        self.assertEqual(self.repo.refs['v0.2.0']['sha'], MAIN)
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), record)
        self.assertEqual(next(iter(self.repo.files.values())), self.archive.read_bytes())

    def test_workflow_change_after_upload_preserves_draft_for_owner_recovery(self):
        record = self.record('stable')
        self.repo.advance_workflows_after_upload = True
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}), self.assertRaisesRegex(ValueError, 'Workflows'):
            delivery.publish(self.archive, record, MAIN)
        self.assertTrue(self.repo.releases[0]['draft'])
        self.assertFalse(any(method == 'PATCH' for _, method, _ in self.repo.mutations))
        self.assertNotIn('v0.2.0', self.repo.refs)
        delivery.publish(self.archive, record, MAIN)
        self.assertEqual(self.repo.refs['v0.2.0']['sha'], MAIN)
        self.assertEqual(sum(path == 'upload' for path, _, _ in self.repo.mutations), 1)

    def test_known_permission_or_lookup_failure_precedes_existing_early_upload(self):
        old = self.record()
        delivery.publish(self.archive, old, MAIN)
        self.archive.write_bytes(package(NEXT))
        self.repo.head = NEXT
        new = self.record(source=NEXT)
        before = copy.deepcopy((self.repo.releases, self.repo.refs, self.repo.assets, self.repo.mutations))
        self.repo.workflow_changes = '.github/workflows/ci.yml'
        with patch.dict(os.environ, GITHUB_ACTIONS='true'), self.assertRaisesRegex(ValueError, 'Workflows'):
            delivery.publish(self.archive, new, MAIN)
        self.assertEqual((self.repo.releases, self.repo.refs, self.repo.assets, self.repo.mutations), before)
        with patch.object(delivery, 'automatic_release_target', side_effect=OSError('lookup failed')):
            with self.assertRaises(OSError):
                delivery.publish(self.archive, new, MAIN)
        self.assertEqual((self.repo.releases, self.repo.refs, self.repo.assets, self.repo.mutations), before)
        # Successful explicitly scoped App issuance is the trusted workflow's capability signal.
        with patch.dict(os.environ, GITHUB_ACTIONS='true', ARB_DELIVERY_APP_TOKEN='true'):
            delivery.publish(self.archive, new, MAIN)
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), new)
        self.assertEqual(self.repo.refs['early']['sha'], MAIN)

    def test_same_product_retry_does_not_rewrite_metadata_after_docs_advance(self):
        first = dict(self.record(), develop_head=SOURCE, run=42)
        delivery.publish(self.archive, first, MAIN)
        self.repo.head = NEXT
        before = copy.deepcopy((self.repo.releases, self.repo.refs, self.repo.assets, self.repo.mutations))
        delivery.publish(self.archive, dict(first, develop_head=NEXT, run=43), MAIN)
        self.assertEqual((self.repo.releases, self.repo.refs, self.repo.assets, self.repo.mutations), before)

    def test_existing_release_race_preserves_old_body_and_both_assets(self):
        old = self.record()
        delivery.publish(self.archive, old, MAIN)
        self.archive.write_bytes(package(NEXT))
        self.repo.head = NEXT
        self.repo.advance_workflows_after_upload = True
        original = copy.deepcopy(self.repo.refs)
        with patch.dict(os.environ, GITHUB_ACTIONS='true'), self.assertRaisesRegex(ValueError, 'Workflows'):
            delivery.publish(self.archive, self.record(source=NEXT), MAIN)
        self.assertEqual(delivery.read_record(self.repo.releases[0]['body']), old)
        self.assertEqual(self.repo.refs, original)
        self.assertEqual(len(self.repo.assets), 2)
        uploaded = sum(path == 'upload' for path, _, _ in self.repo.mutations)
        delivery.publish(self.archive, self.record(source=NEXT), MAIN)
        self.assertEqual(sum(path == 'upload' for path, _, _ in self.repo.mutations), uploaded)

    def test_distribution_lookup_failure_never_becomes_a_skip(self):
        with patch.object(delivery, 'git_impact', return_value={'reason': 'missing history', 'distribution': True}):
            with self.assertRaisesRegex(ValueError, 'complete delivery inputs'):
                delivery.distribution_change(SOURCE)

    def test_invalid_or_changed_draft_target_is_not_published(self):
        record = self.record()
        self.repo.fail_upload = True
        with self.assertRaises(OSError):
            delivery.publish(self.archive, record, MAIN)
        self.repo.fail_upload = False
        self.repo.releases[0]['target_commitish'] = 'main'
        before = len(self.repo.mutations)
        with self.assertRaisesRegex(ValueError, 'Draft target'):
            delivery.publish(self.archive, record, NEXT)
        self.assertEqual(len(self.repo.mutations), before)
        self.repo.releases[0]['target_commitish'] = MAIN
        find = delivery.find_release
        def moved(tag):
            result = find(tag)
            if moved.called:
                result['target_commitish'] = NEXT
            moved.called = True
            return result
        moved.called = False
        with patch.object(delivery, 'find_release', side_effect=moved), self.assertRaisesRegex(ValueError, 'changed during'):
            delivery.publish(self.archive, record, NEXT)
        self.assertEqual(len(self.repo.mutations), before)

    def test_automatic_permission_lookup_failure_does_not_mutate(self):
        record = self.record('stable')
        api = self.repo.api
        def missing(path, method='GET', data=None):
            if path == '': raise OSError('repository lookup unavailable')
            return api(path, method, data)
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}), patch.object(delivery, 'api', side_effect=missing):
            with self.assertRaises(OSError):
                delivery.publish(self.archive, record, MAIN)
        self.assertEqual(self.repo.mutations, [])

    def test_recovering_older_stable_does_not_move_latest_back(self):
        self.archive.write_bytes(package(NEXT, version='0.10.0'))
        delivery.publish(self.archive, self.record('stable', NEXT), 'd' * 40)
        newest = self.repo.latest
        self.archive.write_bytes(package(version='0.9.0'))
        delivery.publish(self.archive, self.record('stable'), MAIN)
        self.assertIsNotNone(newest)
        self.assertEqual(self.repo.latest, newest)

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
               'head_branch': 'develop', 'path': '.github/workflows/ci.yml', 'workflow_id': 7}
        workflow = {'id': 7, 'path': '.github/workflows/ci.yml'}
        item = {'id': 12, 'name': 'plugin-' + SOURCE, 'expired': False,
                'size_in_bytes': len(transport.getvalue()),
                'digest': 'sha256:' + hashlib.sha256(transport.getvalue()).hexdigest()}
        listing = {'total_count': 1, 'artifacts': [item]}
        for field, bad in (('head_sha', NEXT), ('event', 'pull_request'), ('conclusion', 'failure'),
                           ('path', '.github/workflows/other.yml'), ('repository', {'full_name': 'other/repo'})):
            with self.subTest(field=field), patch.object(delivery, 'api', side_effect=[dict(run, **{field: bad}), workflow]):
                with self.assertRaises(ValueError):
                    delivery.artifact(42, SOURCE, self.directory)
        with patch.object(delivery, 'api', side_effect=[run, workflow, listing]):
            product, returned = delivery.artifact(42, SOURCE, self.directory)
        self.assertEqual(product.read_bytes(), package())
        self.assertEqual(returned, run)

    def test_producer_path_forms_require_the_current_workflow_identity(self):
        path = '.github/workflows/ci.yml'
        workflow = {'id': 7, 'path': path}
        run = {'repository': {'full_name': delivery.REPO}, 'head_repository': {'full_name': delivery.REPO},
               'head_sha': SOURCE, 'head_branch': 'develop', 'event': 'push',
               'status': 'completed', 'conclusion': 'success', 'path': path, 'workflow_id': 7}
        for branch in ('develop', 'main'):
            for suffix in ('', '@' + branch):
                observed = dict(run, head_branch=branch, path=path + suffix)
                with self.subTest(branch=branch, suffix=suffix), patch.object(
                        delivery, 'api', side_effect=[observed, workflow]) as api:
                    self.assertEqual(delivery.producer_run(42, SOURCE), observed)
                self.assertEqual(api.call_args.args, ('actions/workflows/ci.yml',))
        for bad_path in (None, '', path + '@', path + '@main', path + '@develop@other',
                         path + '@develop\n', path + '.other', '.github/workflows/other.yml@develop'):
            with self.subTest(path=bad_path), patch.object(delivery, 'api', side_effect=[
                    dict(run, path=bad_path), workflow]), self.assertRaises(ValueError):
                delivery.producer_run(42, SOURCE)
        for bad_id in (None, 0, -1, True, '7', 7.0, 8):
            with self.subTest(run_id=bad_id), patch.object(delivery, 'api', side_effect=[
                    dict(run, workflow_id=bad_id), workflow]), self.assertRaises(ValueError):
                delivery.producer_run(42, SOURCE)
        for current in (None, {}, {'id': 7}, {'path': path}, {'id': True, 'path': path},
                        {'id': '7', 'path': path}, {'id': 8, 'path': path},
                        {'id': 7, 'path': '.github/workflows/other.yml'}):
            with self.subTest(workflow=current), patch.object(delivery, 'api', side_effect=[
                    run, current]), self.assertRaises(ValueError):
                delivery.producer_run(42, SOURCE)
        with patch.object(delivery, 'api', side_effect=[run, RuntimeError('workflow lookup failed')]), \
             self.assertRaises(RuntimeError):
            delivery.producer_run(42, SOURCE)

    def test_candidate_is_successful_trusted_main_workflow_and_exact_accepted_bytes(self):
        expected = delivery.digest(self.archive)
        item = {'id': 12, 'name': 'candidate-' + expected, 'expired': False, 'workflow_run': {'id': 42}}
        listing = {'total_count': 1, 'artifacts': [item]}
        run = {'repository': {'full_name': delivery.REPO}, 'head_repository': {'full_name': delivery.REPO},
               'head_sha': MAIN, 'head_branch': 'main', 'event': 'workflow_dispatch',
               'path': '.github/workflows/release-candidate.yml', 'workflow_id': 8,
               'status': 'completed', 'conclusion': 'success'}
        workflow = {'id': 8, 'path': '.github/workflows/release-candidate.yml'}
        with patch.object(delivery, 'git') as git, patch.object(
                delivery, 'download_artifact', return_value=self.archive) as download:
            for suffix in ('', '@main'):
                with self.subTest(suffix=suffix), patch.object(delivery, 'api', side_effect=[
                        listing, dict(run, path=workflow['path'] + suffix), workflow]) as api:
                    self.assertEqual(delivery.candidate_archive(expected, self.directory), self.archive)
                self.assertEqual(api.call_args.args, ('actions/workflows/release-candidate.yml',))
                git.assert_called_with('merge-base', '--is-ancestor', MAIN, 'origin/main')
            for field, bad in (('head_branch', 'develop'), ('event', 'pull_request'), ('conclusion', 'failure'),
                               ('path', '.github/workflows/other.yml'), ('workflow_id', 7),
                               ('path', workflow['path'] + '@'), ('path', workflow['path'] + '@develop')):
                download.reset_mock()
                with self.subTest(field=field), patch.object(delivery, 'api', side_effect=[
                        listing, dict(run, **{field: bad}), workflow]):
                    with self.assertRaises(ValueError):
                        delivery.candidate_archive(expected, self.directory)
                download.assert_not_called()
            self.archive.write_bytes(package(NEXT))
            with patch.object(delivery, 'api', side_effect=[listing, run, workflow]), self.assertRaises(ValueError):
                delivery.candidate_archive(expected, self.directory)
        for data in ({'total_count': 0, 'artifacts': []},
                     {'total_count': 2, 'artifacts': [item]},
                     {'total_count': 1, 'artifacts': [dict(item, expired=True)]}):
            with patch.object(delivery, 'api', return_value=data), self.assertRaises(ValueError):
                delivery.candidate_archive(expected, self.directory)

    def test_product_input_without_artifact_never_falls_back(self):
        run = {'head_sha': SOURCE, 'event': 'push', 'head_branch': 'develop'}
        with patch.object(delivery, 'producer_run', return_value=run), \
             patch.object(delivery, 'api', return_value={'total_count': 0, 'artifacts': []}), \
             patch.object(delivery, 'download_artifact') as download:
            with self.assertRaisesRegex(ValueError, 'live source-bound artifact'):
                delivery.artifact(42, SOURCE, self.directory)
            download.assert_not_called()
        self.assertEqual(self.repo.mutations, [])

    def test_nonproduct_event_exits_before_history_artifact_or_release_lookup(self):
        run = {'head_sha': SOURCE, 'event': 'push', 'head_branch': 'develop'}
        with patch.object(delivery, 'producer_run', return_value=run), \
             patch.object(delivery, 'distribution_change', return_value=False), \
             patch.object(delivery, 'api', side_effect=AssertionError('unexpected lookup')), \
             patch.object(delivery, 'artifact', side_effect=AssertionError('unexpected download')), \
             patch.object(delivery, 'publish', side_effect=AssertionError('unexpected publication')), \
             patch.object(sys, 'argv', ['delivery', 'early', '--run', '42', '--source', SOURCE]):
            delivery.main()
        self.assertEqual(self.repo.mutations, [])

    def test_early_follows_product_overtaken_by_skipped_head_in_both_completion_orders(self):
        self.repo.head = NEXT
        transport = io.BytesIO()
        with zipfile.ZipFile(transport, 'w') as archive:
            archive.writestr('plugin.zip', self.archive.read_bytes())
        self.repo.files[12] = transport.getvalue()
        item = {'id': 12, 'name': 'plugin-' + SOURCE, 'expired': False,
                'size_in_bytes': len(transport.getvalue()),
                'digest': 'sha256:' + hashlib.sha256(transport.getvalue()).hexdigest()}
        runs = {number: {'id': number, 'repository': {'full_name': delivery.REPO},
                        'head_repository': {'full_name': delivery.REPO}, 'head_sha': source,
                        'status': 'completed', 'conclusion': 'success', 'event': 'push',
                        'head_branch': 'develop', 'path': '.github/workflows/ci.yml', 'workflow_id': 7}
                for number, source in ((42, SOURCE), (43, NEXT))}
        prs = {n: {'number': n, 'merged': True, 'merge_commit_sha': r['head_sha'],
                   'base': {'ref': 'develop', 'repo': {'full_name': delivery.REPO}},
                   'head': {'repo': {'full_name': delivery.REPO}}} for n, r in runs.items()}
        def api(path, method='GET', data=None):
            if path == 'git/ref/heads/main': return {'object': {'sha': MAIN}}
            if path == 'actions/workflows/ci.yml': return {'id': 7, 'path': '.github/workflows/ci.yml'}
            if path.startswith('actions/workflows/'):
                selected = [r for r in runs.values() if 'head_sha=' + r['head_sha'] in path]
                return {'total_count': len(selected), 'workflow_runs': selected}
            if path.startswith('actions/runs/'):
                number = int(path.split('/')[2])
                if '/artifacts?' in path:
                    return {'total_count': 1 if number == 42 else 0, 'artifacts': [item] if number == 42 else []}
                if '/jobs?' in path:
                    return {'total_count': 1, 'jobs': [{'name': 'test', 'conclusion': 'success',
                            'steps': [{'name': 'Run actions/upload-artifact@v4', 'conclusion': 'skipped'}]}]}
                return runs[number]
            if path.startswith('pulls/'): return copy.deepcopy(prs[int(path.split('/')[1])])
            return self.repo.api(path, method, data)
        def pages(path):
            if path.startswith('commits/'):
                return [p for p in prs.values() if p['merge_commit_sha'] == path.split('/')[1]]
            return self.repo.pages(path)
        def git(*args):
            return NEXT + '\n' + SOURCE if args[0] == 'rev-list' else ''
        with patch.object(delivery, 'api', side_effect=api), patch.object(delivery, 'pages', side_effect=pages), \
             patch.object(delivery, 'git', side_effect=git), \
             patch.object(delivery, 'distribution_change', side_effect=lambda source: source == SOURCE):
            # A docs-only event never retries publication, regardless of product CI order.
            runs[42]['status'] = 'in_progress'
            with patch.object(sys, 'argv', ['delivery', 'early', '--run', '43', '--source', NEXT]), \
                 patch.object(delivery, 'publish') as publish:
                delivery.main()
                publish.assert_not_called()
            self.assertEqual(self.repo.releases, [])
            runs[42]['status'] = 'completed'
            for number in (42, 43):
                with patch.object(sys, 'argv', ['delivery', 'early', '--run', str(number),
                                              '--source', runs[number]['head_sha']]):
                    delivery.main()
                record = delivery.read_record(self.repo.releases[0]['body'])
                self.assertEqual((record['source'], record['develop_head'], record['run']), (SOURCE, NEXT, 42))
                self.assertEqual(len(self.repo.assets), 1)
                self.assertEqual(len(self.repo.releases), 1)
            before = copy.deepcopy(self.repo.releases)
            # A newer product without its artifact blocks the old product's delayed event.
            with patch.object(delivery, 'distribution_change', return_value=True), \
                 patch.object(sys, 'argv', ['delivery', 'early', '--run', '42', '--source', SOURCE]), \
                 self.assertRaisesRegex(ValueError, 'live source-bound artifact'):
                delivery.main()
            self.assertEqual(self.repo.releases, before)
            runs[43]['conclusion'] = 'failure'
            with patch.object(sys, 'argv', ['delivery', 'early', '--run', '42', '--source', SOURCE]), \
                 self.assertRaises(ValueError):
                delivery.main()
            self.assertEqual(self.repo.releases, before)
            runs[43]['conclusion'] = 'success'
            item['expired'] = True
            with patch.object(sys, 'argv', ['delivery', 'early', '--run', '42', '--source', SOURCE]), \
                 self.assertRaises(ValueError):
                delivery.main()
            self.assertEqual(self.repo.releases, before)

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

    def test_historical_initial_is_an_exact_exception_after_normal_promotion_gate(self):
        original = copy.deepcopy(delivery.INITIAL)
        accepted = dict(original, schema=1, repository=delivery.REPO)
        identity = {key: accepted[key] for key in ('source', 'sha256', 'size', 'version')}
        promotion = (original['source'], original['sha256'], original['main_merge'],
                     {'stage': 'initial-agent', 'stage_complete': True})
        with patch.object(delivery, 'promotion_gate', return_value=promotion) as gate, \
             patch.object(delivery, 'identity', return_value=identity) as identify, \
             patch.object(delivery, 'publish', return_value='published') as publish:
            self.assertEqual(delivery.stable_release(264, self.directory, self.archive), 'published')
            gate.assert_called_once_with(264)
            identify.assert_called_once_with(self.archive, original['source'], original['sha256'])
            self.assertEqual(publish.call_args.args[1], accepted)
            for key, value in (('source', NEXT), ('sha256', 'e' * 64), ('size', original['size'] + 1),
                               ('version', '0.1.0-dev.' + NEXT)):
                with self.subTest(key=key), patch.object(delivery, 'identity', return_value=dict(identity, **{key: value})):
                    with self.assertRaises(ValueError):
                        delivery.stable_release(264, self.directory, self.archive)
            for number, result in ((265, promotion), (264, (*promotion[:3], {'stage': 'full'})),
                                   (264, (promotion[0], promotion[1], NEXT, promotion[3]))):
                with patch.object(delivery, 'promotion_gate', return_value=result), self.assertRaises(ValueError):
                    delivery.stable_release(number, self.directory, self.archive)
            with self.assertRaises(ValueError):
                delivery.stable_release(264, self.directory, self.archive, version='0.1.0')
        self.assertIn('初期配布', delivery.release_body(accepted))
        self.assertIn('QA #65', delivery.release_body(accepted))
        self.assertIn(original['version'], delivery.release_body(accepted))
        self.assertEqual(self.repo.mutations, [])

    def test_historical_initial_and_numeric_latest_never_downgrade(self):
        initial = dict(delivery.INITIAL, schema=1, repository=delivery.REPO)
        self.assertEqual(delivery.latest_setting(initial), 'true')
        self.repo.releases = [{'tag_name': 'v' + initial['version'], 'draft': False, 'prerelease': False,
                               'body': delivery.release_body(initial)}]
        stable = self.record('stable')
        self.assertEqual(delivery.latest_setting(stable), 'true')
        for version in ('0.1.0', '0.2.0', '0.10.0'):
            self.repo.releases = [{'tag_name': 'v' + version, 'draft': False, 'prerelease': False}]
            self.assertEqual(delivery.latest_setting(initial), 'false')
        self.repo.releases = [{'tag_name': 'v0.1.0-dev.unknown', 'draft': False, 'prerelease': False}]
        self.assertEqual(delivery.latest_setting(stable), 'false')

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
