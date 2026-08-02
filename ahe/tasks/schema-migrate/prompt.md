`store/` migrates stored records between schema versions. It only goes forward, it
corrupts its input, and it mangles names. Fix it.

## Schema history

- **v1** `{"v": 1, "name": "Ada Lovelace", "email": "a@b.c"}`
- **v2** `name` splits into `first` / `last`
- **v3** `email` becomes a list `emails`, primary first
- **v4** adds `active` (bool), defaulting `True`

## Spec

1. **The input is never mutated**, at any depth. `migrate` returning a record that
   shares a list with its argument is the same bug as mutating it.
2. **Names split on the last space.** `"Ada Lovelace"` is `first="Ada"`,
   `last="Lovelace"`; `"Ada King Lovelace"` is `first="Ada King"`,
   `last="Lovelace"`. A single-word name is `first=""`, `last=` the word. An empty
   name is `first=""`, `last=""`. Splitting must not raise.
3. **Downgrades work too.** `migrate(record, to=n)` with `n` below the record's
   version reverses the steps:
   - v4 → v3 drops `active`
   - v3 → v2 takes `emails[0]` as `email` and **discards the rest**; a record with
     an empty `emails` raises `MigrationError`
   - v2 → v1 rejoins `first` and `last` with a single space, with no leading or
     trailing space when either is empty
4. **Round trips are stable.** Downgrading and re-upgrading a record must give back
   what the upgrade would have produced directly, for every pair of versions.
5. **Migrating to the version a record already has returns an equal record** — and
   still not the same object.
6. **Bad input raises `MigrationError`**, not `KeyError` or `AttributeError`: a
   record with no `v`, a `v` outside 1..4, or a `to` outside 1..4.

## Constraints

- `store/api.py` is the published surface and is **protected**: do not modify it.
- `test_store.py` is the checked-in test suite. It must still pass.
