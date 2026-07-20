This repo has SIX independent modules — intervals.py, lru.py, percentile.py, tokenizer.py, ledger.py, rpn.py — and each one has its own distinct, non-obvious bug that makes tests in test_all.py fail. The six modules are unrelated: different domains, no shared code. Each bug takes some reading of that module's logic to find; none is a one-line typo you can spot from the test alone.

Fix the bug in each module so the whole suite passes (`python3 -m unittest`). Do not change test_all.py — the tests encode the intended behavior. Keep each fix minimal and scoped to its own module.
