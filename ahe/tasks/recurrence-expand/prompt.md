`rrule/` expands a recurrence into the dates it occurs on. Its behaviour does not
match the spec below. Fix it.

## Spec

1. **Daily.** `start`, then every `interval` days after it.
2. **Weekly, no `byday`.** The same weekday as `start`, every `interval` weeks.
3. **Weekly with `byday`.** Weeks run Monday to Sunday, and the week containing
   `start` is the first one. Within each selected week the rule occurs on every
   weekday named in `byday`, **ascending by date regardless of the order `byday`
   was given in**. Weeks then advance by `interval`. An occurrence that falls
   **before `start`** — in that first week — is not an occurrence and is not
   emitted.
4. **Monthly.** The same day-of-month as `start`, every `interval` months. A month
   that has no such day is **skipped, not clamped**: the 31st recurring monthly
   never lands on the 30th of April or the 28th of February. Skipping does not
   change the phase — `interval` counts calendar months, not emitted occurrences.
5. **Stopping.** `until` is **inclusive**: an occurrence exactly on `until` is
   emitted. `count` limits the number of dates actually **returned**, so a date
   removed by `exclude` does not consume it. When both are set, whichever stops the
   expansion first wins. A rule with neither raises `ValueError`.
6. **Exclusions.** A date in `exclude` never appears in the result.

The result is always ascending.

## Constraints

- `rrule/api.py` is the published surface and is **protected**: do not modify it.
- `test_rrule.py` is the checked-in test suite. It must still pass.
