`plan/` pushes filter predicates down a query plan so they run as close to the
scans as possible. It pushes some predicates it must not, and accepts some plans it
must reject. Fix it.

## Spec

1. **Conjunctions split.** `("and", a, b)` is two independently placeable
   predicates. `("or", ...)` is one — it moves only if *every* column it
   references lives on the same side.
2. **A predicate moves to the deepest node that provides all its columns.** If both
   sides of a join are needed, it stays as a `Filter` above that join.
3. **A LEFT join is not symmetric.** A predicate that references only the
   **left** side may be pushed into it. A predicate on the **right** side must
   **not** be: pushing it discards the null-extended rows the left join exists to
   produce, silently turning it into an inner join. Such a predicate stays as a
   `Filter` above the join. An inner join has no such restriction.
4. **Unknown columns are an error.** A predicate referencing a column no scan in
   the subtree provides raises `ValueError` naming that column, rather than being
   parked at the top of the plan.
5. **Order is preserved.** When two conjuncts land on the same node, the one
   written first ends up **innermost** — closest to the scan.

Scans and joins with no filter above them come back unchanged.

## Constraints

- `plan/api.py` is the published surface and is **protected**: do not modify it.
- `test_plan.py` is the checked-in test suite. It must still pass.
