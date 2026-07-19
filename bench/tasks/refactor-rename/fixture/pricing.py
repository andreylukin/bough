def calc(items, tax_rate):
    """Total price of (name, unit_price, qty) items with tax applied."""
    subtotal = sum(price * qty for _, price, qty in items)
    return round(subtotal * (1 + tax_rate), 2)
