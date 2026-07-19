import pricing


def invoice_line(items, tax_rate):
    return f"TOTAL: {pricing.calc(items, tax_rate):.2f}"
