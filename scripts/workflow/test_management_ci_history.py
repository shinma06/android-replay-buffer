"""Disposable Git proofs for the single registered management CI transition."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import verification as v
from test_observation_reuse import ARTIFACT, reused
from test_verification import change, observation


class ManagementCIHistoryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.git('init', '-q')
        self.git('config', 'maintenance.auto', 'false')
        self.git('config', 'user.name', 'Test')
        self.git('config', 'user.email', 'test@example.invalid')
        self.case_path = 'docs/verification/changes/issue-36.json'
        self.write(self.case_path, json.dumps(change(36)))
        for path in v.CI_PATHS:
            self.write(path, "python-version: '3.11'\n")
        self.predecessor = self.commit('predecessor')
        self.git('checkout', '-qb', 'observed')
        self.write('.github/workflows/ci.yml', "python-version: '3.11'\nJava/Gradle/Plugin/upload\n")
        self.source = self.commit('observed product CI')
        self.before = self.git('ls-tree', self.source, '--', '.github/workflows/ci.yml').split()[2]
        self.original = dict(observation(), head=self.source)
        self.write(v.PROMOTION, json.dumps({'schema': 1, 'candidate': self.source,
                                         'results': {'36:QA-1': self.original}}))
        self.record = self.commit('original record')
        self.git('checkout', '-qb', 'writer', self.predecessor)
        for path in v.CI_PATHS:
            self.write(path, "python-version: '3.15.0'\n")
        self.origin = self.commit('management CI writer')
        self.git('checkout', '-qb', 'trusted-main', self.predecessor)
        self.git('read-tree', self.origin)
        self.merge = self.git('commit-tree', self.origin + '^{tree}', '-p', self.predecessor,
                              '-m', 'protected squash result')
        self.git('reset', '--hard', self.merge)
        self.write('README.md', 'trusted main docs')
        self.write(v.PROMOTION, json.dumps({'schema': 1, 'candidate': self.source,
                                         'results': {'36:QA-1': self.original}}))
        self.base = self.commit('trusted main')
        self.git('checkout', '-q', 'observed')
        for path in v.CI_PATHS:
            self.write(path, "python-version: '3.15.0'\n" +
                       ('Java/Gradle/Plugin/upload\n' if path.endswith('/ci.yml') else ''))
        self.git('add', '.')
        self.update = self.git('ls-files', '-s', '--', '.github/workflows/ci.yml').split()[1]
        self.candidate = self.git('commit-tree', self.git('write-tree'), '-p', self.source,
                                  '-m', 'squash sync preserving observed product CI')
        self.git('reset', '--hard', self.candidate)
        for name, value in (('CI_PREDECESSOR', self.predecessor), ('CI_MERGE', self.merge),
                            ('CI_OBSERVED_SOURCE', self.source),
                            ('CI_OBSERVED_BEFORE', self.before), ('CI_OBSERVED_UPDATE', self.update)):
            mocked = patch.object(v, name, value)
            mocked.start()
            self.addCleanup(mocked.stop)

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args],
            text=True, stderr=subprocess.DEVNULL).strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message):
        self.git('add', '.')
        self.git('commit', '-qm', message)
        return self.git('rev-parse', 'HEAD')

    def history(self, candidate=None, base=None, git=None):
        v.unchanged_artifact_history(self.source, candidate or self.candidate, '36:QA-1',
                                    git or self.git, self.base if base is None else base)

    def test_fixed_source_to_squash_preserves_full_ci_and_rejects_branch_history(self):
        self.history()
        text = self.git('show', self.candidate + ':.github/workflows/ci.yml')
        self.assertIn('Java/Gradle/Plugin/upload', text)
        candidate = self.git('commit-tree', self.candidate + '^{tree}', '-p', self.candidate,
                             '-p', self.origin, '-m', 'include original writer history')
        with self.assertRaisesRegex(ValueError, 'source-to-squash'):
            self.history(candidate)

    def test_reuse_and_listing_require_current_proof_and_fixed_original_record(self):
        result = reused(self.original, self.candidate, self.source)
        result['source_record'] = self.base
        result['confirmation']['at'] = '2099-01-01T00:00:00Z'
        before = copy.deepcopy(result)
        self.assertEqual(v.validate_candidate_result(result, '36:QA-1', None, None,
            self.candidate, ARTIFACT, None, None, self.git, self.base), self.original)
        manifest = {'schema': 1, 'base': self.base, 'candidate': self.candidate,
                    'artifact_sha256': ARTIFACT, 'results': {'36:QA-1': result}}
        view = v.render_queue([self.root / self.case_path], manifest, self.git)
        self.assertIn('同一成果物の旧観察を再利用', view)
        self.assertEqual(result, before)
        for name in ('equivalence_evidence', 'environment_evidence', 'loaded_identity'):
            invalid = copy.deepcopy(result)
            invalid['confirmation'][name] = ''
            with self.subTest(name=name), self.assertRaises(ValueError):
                v.validate_candidate_result(invalid, '36:QA-1', None, None,
                    self.candidate, ARTIFACT, None, None, self.git, self.base)

    def test_retained_fact_assessment_uses_the_same_trusted_history(self):
        policy = {'cases': {'36:QA-1': {}}, 'responsibility_revision': {
            'previous_plan_sha256': v.canonical_hash({}),
            'groups': {'fixture': {'cases': ['36:QA-1']}}}}
        aggregate = dict(self.original, status='pending')
        observed = dict(observation(), head=self.candidate, at='2099-01-01T00:00:00Z',
            responsibility_evidence={'artifact_binding': 'Synthetic complete binding.'})
        observed['responsibility_assessment'] = {
            'source_candidate': self.source, 'source_result': aggregate,
            'source_result_sha256': v.canonical_hash(aggregate), 'source_record_evidence': 'Fixed report.',
            'facts': {'output': self.original}, 'coverage': {'artifact_binding': ['output']},
            'missing_requirements': [], 'unresolved_owned_failures': []}
        with patch.object(v, 'initial_stage_plan', return_value={}):
            v.validate_fact_assessment(observed, '36:QA-1', policy, self.candidate,
                                       ARTIFACT, self.git, self.base)
            with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                v.validate_fact_assessment(observed, '36:QA-1', policy, self.candidate,
                                           ARTIFACT, self.git, self.predecessor)
            observed['responsibility_assessment']['missing_requirements'] = ['Native integration']
            with self.assertRaisesRegex(ValueError, 'complete coverage'):
                v.validate_fact_assessment(observed, '36:QA-1', policy, self.candidate,
                                           ARTIFACT, self.git, self.base)

    def test_changed_unknown_product_case_and_reverted_ci_are_rejected(self):
        for path in ('.github/workflows/ci.yml', '.github/workflows/unknown.yml',
                     'plugin/build.gradle.kts', 'tests/fixture.json', self.case_path):
            self.git('checkout', '--detach', '-q', self.candidate)
            self.write(path, 'unknown edit')
            self.commit('unobserved change')
            self.git('revert', '--no-edit', 'HEAD')
            self.assertEqual(self.git('diff', '--name-only', self.candidate, 'HEAD'), '')
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.history(self.git('rev-parse', 'HEAD'))

    def test_same_content_on_unregistered_commit_and_nonancestor_are_rejected(self):
        self.git('checkout', '--detach', '-q', self.record)
        for path in v.CI_PATHS:
            self.write(path, "python-version: '3.15.0'\n" +
                       ('Java/Gradle/Plugin/upload\n' if path.endswith('/ci.yml') else ''))
        self.commit('unregistered linear content copy')
        copied = self.git('rev-parse', 'HEAD')
        candidate = self.git('commit-tree', copied + '^{tree}', '-p', copied, '-p', self.base,
                             '-m', 'later main import cannot authorize earlier copy')
        with self.assertRaisesRegex(ValueError, 'source-to-squash'):
            self.history(candidate)
        with self.assertRaises(subprocess.CalledProcessError):
            self.history(self.base)

    def test_partial_transition_and_different_observed_source_are_rejected(self):
        self.git('checkout', '--detach', '-q', self.source)
        self.write('.github/workflows/ci.yml', "python-version: '3.15.0'\nJava/Gradle/Plugin/upload\n")
        partial = self.commit('incomplete CI transition')
        with self.assertRaisesRegex(ValueError, 'content or mode'):
            self.history(partial)
        with self.assertRaisesRegex(ValueError, 'source-to-squash'):
            v.unchanged_artifact_history(self.predecessor, self.candidate, '36:QA-1',
                                        self.git, self.base)

    def test_missing_trust_changed_source_proof_and_dropped_product_ci_are_rejected(self):
        for base in ('', self.predecessor):
            with self.subTest(base=base), self.assertRaises((ValueError, subprocess.CalledProcessError)):
                self.history(base=base)
        for mutation in ('source-parent', 'missing-blob', 'symlink'):
            def git(*args):
                if mutation == 'source-parent' and args == ('rev-list', '--parents', '-n', '1', self.merge):
                    return self.merge + ' ' + self.record
                if args == ('ls-tree', self.merge, '--', '.github/workflows/acceptance.yml'):
                    if mutation == 'missing-blob':
                        return ''
                    if mutation == 'symlink':
                        return self.git(*args).replace('100644', '120000')
                return self.git(*args)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.history(git=git)
        self.git('checkout', '--detach', '-q', self.candidate)
        self.write('.github/workflows/ci.yml', "python-version: '3.15.0'\n")
        self.git('add', '.')
        dropped = self.git('commit-tree', self.git('write-tree'), '-p', self.source,
                           '-m', 'drop observed product CI in squash')
        with self.assertRaisesRegex(ValueError, 'content or mode'):
            self.history(dropped)
