from pricing import calc


def cart_total(cart, tax_rate=0.1):
    return calc(cart, tax_rate)
