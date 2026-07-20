This repo has four INDEPENDENT modules — mod_a.py, mod_b.py, mod_c.py, mod_d.py — and each one has its own distinct bug that makes a test in test_all.py fail. The four modules have nothing to do with each other.

Fix the bug in each module so the whole suite passes (`python3 -m unittest`). Do not change test_all.py — the tests encode the intended behavior. Keep each fix minimal and scoped to its own module.
