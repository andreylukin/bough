"""A checking account with a fixed overdraft limit."""


class Account:
    def __init__(self, balance=0, overdraft=0):
        # overdraft is a non-negative allowance below zero.
        self.balance = balance
        self.overdraft = overdraft

    def deposit(self, amount):
        if amount <= 0:
            raise ValueError("deposit must be positive")
        self.balance += amount

    def withdraw(self, amount):
        if amount <= 0:
            raise ValueError("withdraw must be positive")
        if self.balance - amount <= -self.overdraft:
            raise ValueError("insufficient funds")
        self.balance -= amount

    def available(self):
        return self.balance + self.overdraft
