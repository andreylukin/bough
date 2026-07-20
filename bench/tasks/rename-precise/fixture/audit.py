from models import Account


def is_solvent(acct: Account) -> bool:
    return acct.balance() >= 0
