`kit/` is eight small, unrelated utility modules. Each one has at least one bug on
an edge case its docstring already describes. The checked-in `test_kit.py` covers
only the happy paths, so it passes today and tells you nothing about the bugs.

Find and fix the bug in every module. They are independent — no fix depends on
another.

- `roman.py` — integer to roman numerals, 1..3999
- `base_convert.py` — non-negative integers between bases 2..36
- `flatten.py` — depth-first flatten of nested lists/tuples, strings are leaves
- `stats.py` — median of a non-empty sequence
- `matrix.py` — transpose, rejecting ragged input
- `calendar_days.py` — count weekdays in an **inclusive** date range
- `graph.py` — Kahn topological sort, deterministic order, cycles rejected
- `bank.py` — integer-cent account, overdrafts refused rather than clamped

Each module's docstrings state the contract; where a docstring and the code
disagree, the docstring is right.

`test_kit.py` is the checked-in test suite. It must still pass.
