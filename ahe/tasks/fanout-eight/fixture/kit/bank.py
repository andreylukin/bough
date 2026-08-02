class InsufficientFunds(Exception):
    pass


class Account:
    """Balances are integer cents. An overdraft is refused, not clamped."""

    def __init__(self, cents=0):
        self.cents = cents

    def deposit(self, amount):
        if amount < 0:
            raise ValueError("negative deposit")
        self.cents += amount

    def withdraw(self, amount):
        if amount < 0:
            raise ValueError("negative withdrawal")
        self.cents -= amount
        if self.cents < 0:
            raise InsufficientFunds(amount)
