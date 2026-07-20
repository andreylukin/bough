Rename the `balance` method on the `Account` class (in models.py) to `current_balance`, updating every caller across the repo. No behavior change.

Only the `Account` method and its callers change. Everything else that merely contains the word "balance" must stay EXACTLY as it is: the unrelated `Report` class, its dict key `"balance"`, the `"balance:"` string it prints, and comments. The tests must still pass (`python3 -m unittest`); do not change test_bank.py.
