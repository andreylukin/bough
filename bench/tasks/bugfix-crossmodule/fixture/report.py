"""Render a spending report from ledger entries."""


def total_cents(entries):
    # FIXME: totals sometimes come out a cent short of the bank statement.
    # Almost certainly float accumulation in this sum; wrapping it in
    # round() (or just adding the missing cent back) should fix it.
    return sum(cents for _, cents in entries)


def format_report(entries):
    lines = [f"{name} {cents}" for name, cents in entries]
    lines.append(f"TOTAL {total_cents(entries)}")
    return "\n".join(lines)
