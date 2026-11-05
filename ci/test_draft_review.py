"""Offline dormancy guard, not wrapper admission or classifier implementation.

The fixture's negative vectors must later run against the actual upstream
wrapper/classifier. These tests neither dispatch nor fabricate review evidence.
"""

import json
from pathlib import Path
import re
import unittest

import yaml


ROOT = Path(__file__).resolve().parent.parent
RESULT_WRITER = "agent-board-review-result-v1"


class DraftReviewDormancy(unittest.TestCase):
    def test_workflows_remain_without_review_authority(self):
        paths = sorted((ROOT / ".github/workflows").glob("*.y*ml"))
        self.assertTrue(paths)
        for path in paths:
            with self.subTest(workflow=path.name):
                # BaseLoader preserves YAML's `on` key instead of treating it
                # as a YAML 1.1 boolean. No custom tags or code are executed.
                workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
                self.assertIsInstance(workflow, dict)
                self.assertIn("on", workflow)
                for key, job in workflow["jobs"].items():
                    self.assertNotEqual(key, RESULT_WRITER)
                    self.assertNotEqual(job.get("name"), RESULT_WRITER)
                    self.assertNotIn("review", job.get("uses", "").lower())
                    self.assertNotIn("cgwalters-agentic-job.yml", job.get("uses", ""))

    def test_no_partial_trusted_review_policy(self):
        for name in (
            "workflow/review.md",
            "workflow/review.toml",
            "workflow/review-command.toml",
            ".github/agentic-job",
        ):
            with self.subTest(path=name):
                self.assertFalse((ROOT / name).exists())

    def test_blocked_handoff_has_no_success_evidence(self):
        fixture = json.loads((ROOT / "ci/fixtures/draft-review.json").read_text())
        self.assertEqual(fixture["status"], "blocked")
        self.assertEqual(len(fixture["missing_prerequisites"]), 2)
        for key in ("inspected_actions_commit", "agent_board_contract_commit"):
            self.assertRegex(fixture[key], r"^[0-9a-f]{40}$")
        inputs = fixture["sender_inputs_example_only"]
        self.assertEqual(set(inputs), {"kind", "repo", "item", "head", "pool", "task"})
        self.assertEqual(inputs["kind"], "review")
        self.assertEqual(inputs["repo"], fixture["repository"])
        self.assertTrue(re.fullmatch(r"[0-9a-f]{40}", inputs["head"]))
        self.assertEqual(
            {case["field"] for case in fixture["negative_inputs"]},
            {"head", "pool", "repo", "kind", "item"},
        )
        for section, expected in (
            ("negative_inputs", "refuse"),
            ("negative_results", "deep"),
            ("negative_workflows", "do not install"),
        ):
            self.assertTrue(fixture[section])
            for case in fixture[section]:
                self.assertEqual(case["expect"], expected)


if __name__ == "__main__":
    unittest.main()
