#!/usr/bin/env python3
"""Fail closed when any required CI family did not finish successfully."""

import json
import os
import sys

REQUIRED = frozenset({
    "unit", "cross", "web-dist", "model", "model-history",
    "web-e2e", "web-e2e-report",
})


def check(needs):
    # Missing dependencies must not turn an empty/all-success subset green.
    if not isinstance(needs, dict) or set(needs) != REQUIRED:
        return False
    return all(isinstance(job, dict) and job.get("result") == "success"
               for job in needs.values())


if __name__ == "__main__":
    try:
        needs = json.loads(os.environ["CI_NEEDS"])
    except (KeyError, ValueError) as error:
        print(f"Cannot read required CI results: {error}", file=sys.stderr)
        sys.exit(1)
    if not check(needs):
        print(f"Required CI jobs did not all succeed: {needs}", file=sys.stderr)
        sys.exit(1)
    print("Every required CI job succeeded.")
