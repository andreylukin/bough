class Account:
    def __init__(self, start):
        self._amt = start

    def balance(self):
        """Return the account's current balance."""
        return self._amt

    def deposit(self, n):
        self._amt += n


class Report:
    """A printed summary. 'balance' here is UNRELATED to Account.balance."""

    def __init__(self):
        # dict key decoy — must stay "balance"
        self.rows = {"balance": 0, "count": 0}

    def render(self):
        # string-literal decoy — must stay "balance:"
        return f"balance: {self.rows['balance']}"
