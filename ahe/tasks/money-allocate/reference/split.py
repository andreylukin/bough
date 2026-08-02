class Splitter:
    def __init__(self, weights):
        self.weights = list(weights)

    def allocate(self, total):
        w = self.weights
        if not w:
            return []
        if any(x < 0 for x in w):
            raise ValueError("negative weight")
        denom = sum(w)
        if denom == 0:
            raise ValueError("weights sum to zero")

        # R4: solve the positive problem and mirror it, so the sign never reaches
        # the flooring.
        sign = -1 if total < 0 else 1
        n = abs(total)

        base = [n * x // denom for x in w]
        rem = [(n * x) % denom for x in w]
        leftover = n - sum(base)

        # R2 + R3: largest remainder first, earlier index wins a tie, a zero
        # weight is never a candidate.
        order = sorted(
            (i for i in range(len(w)) if w[i] > 0),
            key=lambda i: (-rem[i], i),
        )
        for i in order[:leftover]:
            base[i] += 1

        return [sign * x for x in base]

    def apply_rate(self, amount, num, den):
        if den == 0:
            raise ValueError("zero denominator")
        # R5: integer half-away-from-zero. No float anywhere on this path.
        prod = amount * num
        sign = -1 if (prod < 0) != (den < 0) else 1
        p, d = abs(prod), abs(den)
        return sign * ((2 * p + d) // (2 * d))
