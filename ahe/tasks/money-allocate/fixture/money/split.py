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
        out = [total * x // denom for x in w]
        return out

    def apply_rate(self, amount, num, den):
        if den == 0:
            raise ValueError("zero denominator")
        return round(amount * num / den)
