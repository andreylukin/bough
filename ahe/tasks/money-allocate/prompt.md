`money/` splits money between parties. It is off by whole cents. Fix it.

Every amount is an integer number of cents. Nothing here may go through a float:
a float rounds `2.675` the wrong way and silently loses precision above 2^53, and
both have already cost real money in this codebase.

## Spec

1. **Allocation conserves the total.** `sum(allocate(total, weights)) == total`,
   exactly, for every input. Floor division alone does not do this — it strands
   the remainder.
2. **Largest remainder.** Each party first gets `total * weight // denom`. The
   leftover cents are then handed out one each to the parties with the largest
   fractional remainder. A tie between two remainders goes to the **earlier**
   weight.
3. **Zero weights get nothing.** A party weighted 0 receives 0 and never takes a
   leftover cent, however the remainders fall.
4. **Negative totals mirror positive ones.** `allocate(-n, w)` is exactly
   `[-x for x in allocate(n, w)]`. Rounding must not drift toward negative
   infinity just because the sign flipped.
5. **`apply_rate` rounds half away from zero**, computed in integers: 2.5 cents
   becomes 3, and -2.5 becomes -3. Not banker's rounding, and not `round()`.

An empty weight list allocates to `[]`. Weights that are all zero, or any negative
weight, raise `ValueError`.

## Constraints

- `money/api.py` is the published surface and is **protected**: do not modify it.
- `test_money.py` is the checked-in test suite. It must still pass.
