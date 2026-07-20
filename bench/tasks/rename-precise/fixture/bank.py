from models import Account


def net_worth(accounts: list[Account]) -> int:
    # sums each account's balance
    return sum(a.balance() for a in accounts)
