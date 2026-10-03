"""Adoption bootstrap and protection checks without a device or GitHub mutations."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import Mock, patch

import agent_loop
import product_check
from verification import baseline_cases
from test_verification import change


class AdoptionTests(unittest.TestCase):
    def test_bootstrap_is_limited_to_known_base_and_named_migration(self):
        workflow = (product_check.ROOT / '.github/workflows/acceptance.yml').read_text()
        script = textwrap.dedent(workflow.split('        run: |\n', 1)[1])
        with tempfile.TemporaryDirectory() as tmp:
            for name, source in {'git': '#!/bin/sh\nprintf "%s" "$FIXTURE_SHA"\n',
                                 'python3': '#!/bin/sh\nexit 17\n'}.items():
                path = Path(tmp) / name
                path.write_text(source); path.chmod(0o755)
            initial = 'e6fb021b144d4f0f6a1f7916e92449610b3ff80c'
            cases = [(initial, 'main', 'codex/1-adopt-harness', 0),
                     (initial, 'main', 'codex/2-adopt-harness', 17),
                     ('a' * 40, 'main', 'codex/1-adopt-harness', 17),
                     (initial, 'main', 'codex/2-other', 17),
                     (initial, 'develop', 'codex/1-adopt-harness', 17)]
            for sha, target, branch, expected in cases:
                env = dict(os.environ, PATH=tmp, FIXTURE_SHA=sha, BASE_REF=target, HEAD_REF=branch)
                result = subprocess.run(['/bin/bash', '-c', script], env=env, capture_output=True)
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_legacy_history_needs_exact_trusted_plan_and_real_cases(self):
        base, old = 'a' * 40, 'b' * 40
        entries = [{'commit': old, 'baseline': True}]
        document = {'schema': 1, 'issue': 35, 'commits': [old], 'acceptance': change()}
        with patch('verification.regular_json', return_value=document) as read:
            self.assertEqual(baseline_cases(base, entries, None), {'35:QA-1': (None, 'app')})
            self.assertEqual(read.call_args.args[0], base)
            with self.assertRaises(ValueError):
                baseline_cases(base, [{'commit': 'c' * 40, 'baseline': True}], None)
            document['acceptance']['cases'] = []
            with self.assertRaises(ValueError): baseline_cases(base, entries, None)





    def test_missing_protection_or_bypass_blocks_merge(self):
        gh = agent_loop.GitHub()
        pull = {'number': 1, 'head': {'sha': 'a' * 40}, 'base': {'ref': 'develop'}, 'draft': False,
                'body': 'Issue: #1\nIntegration: develop\nVerification: docs/verification/changes/issue-1.json'}
        rules = [{'type': name, 'ruleset_id': 12} for name in ('deletion', 'non_fast_forward')]
        rules += [{'type': 'pull_request', 'ruleset_id': 12, 'parameters': {'required_review_thread_resolution': True}},
                  {'type': 'required_status_checks', 'ruleset_id': 12, 'parameters': {
                      'strict_required_status_checks_policy': True,
                      'required_status_checks': [{'context': x} for x in ('test', 'PR policy', 'Acceptance gate', 'Agent review')]}}]
        for invalid in ([], rules[:-1]):
            gh.api = Mock(return_value=invalid)
            with self.assertRaises(ValueError): gh.merge(pull)
            self.assertEqual(gh.api.call_count, 1)
        gh.api = Mock(side_effect=[rules, {'bypass_actors': [{}]}])
        with self.assertRaises(ValueError): gh.merge(pull)
        gh.api = Mock(side_effect=[copy.deepcopy(rules), {'bypass_actors': []}, {'merged': True}])
        gh.merge(pull)
        self.assertEqual(gh.api.call_args.args[1], 'PUT')
        self.assertEqual(gh.api.call_args.args[2], {'sha': 'a' * 40, 'merge_method': 'squash'})


if __name__ == '__main__':
    unittest.main()
