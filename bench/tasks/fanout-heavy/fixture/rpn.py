"""Evaluate a reverse-Polish-notation expression given as a list of string
tokens. Operators pop the top two operands; for the non-commutative operators
(- and /) the operand pushed FIRST is the left-hand side:

    ["6", "2", "-"]  ->  6 - 2  ->  4.0
    ["6", "2", "/"]  ->  6 / 2  ->  3.0
"""

_OPS = {"+", "-", "*", "/"}


def evaluate(tokens):
    stack = []
    for tok in tokens:
        if tok in _OPS:
            if len(stack) < 2:
                raise ValueError(f"not enough operands for {tok!r}")
            right = stack.pop()
            left = stack.pop()
            if tok == "+":
                stack.append(left + right)
            elif tok == "-":
                stack.append(right - left)
            elif tok == "*":
                stack.append(left * right)
            else:
                stack.append(right / left)
        else:
            stack.append(float(tok))
    if len(stack) != 1:
        raise ValueError("malformed expression")
    return stack[0]
