"""Regression checks for the stable GitHub branch-protection gate."""

import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name("ci-required.py")
SPEC = importlib.util.spec_from_file_location("ci_required", SCRIPT)
POLICY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(POLICY)
WORKFLOW = SCRIPT.parent.parent / ".github/workflows/ci-go.yml"


class RequiredCITest(unittest.TestCase):
    def results(self):
        return {name: {"result": "success"} for name in POLICY.REQUIRED}

    def test_all_success(self):
        self.assertTrue(POLICY.check(self.results()))

    def test_each_family_must_succeed(self):
        for name in POLICY.REQUIRED:
            for result in ("failure", "cancelled", "skipped", "", None):
                with self.subTest(name=name, result=result):
                    needs = self.results()
                    needs[name]["result"] = result
                    self.assertFalse(POLICY.check(needs))

    def test_missing_unexpected_and_malformed_results(self):
        for name in POLICY.REQUIRED:
            needs = self.results()
            del needs[name]
            self.assertFalse(POLICY.check(needs))
            needs = self.results()
            needs[name] = {}
            self.assertFalse(POLICY.check(needs))
        needs = self.results()
        needs["new-job"] = {"result": "success"}
        for invalid in (needs, {}, [], None):
            self.assertFalse(POLICY.check(invalid))

    def test_exit_status(self):
        for value, status in ((json.dumps(self.results()), 0), ("{}", 1), ("invalid", 1)):
            result = subprocess.run([sys.executable, str(SCRIPT)],
                                    env={"CI_NEEDS": value}, capture_output=True)
            self.assertEqual(result.returncode, status)

    def test_workflow_wiring(self):
        workflow = WORKFLOW.read_text()
        jobs = workflow.split("\njobs:\n", 1)[1]
        # Check the actual workflow wiring as well as the Python policy.
        families = set(re.findall(r"^  ([\w-]+):$", jobs, re.MULTILINE))
        self.assertEqual(families, POLICY.REQUIRED | {"ci-go-required"})
        gate = jobs.split("\n  ci-go-required:\n", 1)[1]
        self.assertIn("    if: ${{ always() }}\n", gate)
        self.assertNotIn("continue-on-error:", gate)
        self.assertIn("python3 -m unittest discover -s scripts -p 'ci_required_test.py'", gate)
        dependencies = re.search(r"^    needs: \[(.+)\]$", gate, re.MULTILINE)
        self.assertIsNotNone(dependencies)
        self.assertEqual(set(dependencies[1].split(", ")), POLICY.REQUIRED)
        self.assertIn("          CI_NEEDS: ${{ toJSON(needs) }}\n", gate)
        self.assertIn("        run: python3 scripts/ci-required.py\n", gate)
        triggers = workflow.split("\non:\n", 1)[1].split("\nconcurrency:", 1)[0]
        self.assertRegex(triggers, r"(?m)^  pull_request:\s*\n(?:  #.*\n)*  schedule:")


if __name__ == "__main__":
    unittest.main()
