from ledger.core import balance, by_category

LINES = ["2026-09-01 groceries -42.10", "# a comment", "2026-09-02 salary 3000", ""]


def test_balance_skips_comments():
    assert str(balance(LINES)) == "2957.90"


def test_by_category():
    assert by_category(LINES)["salary"] == 3000
