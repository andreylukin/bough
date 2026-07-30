`conf/` resolves configuration from four sources. Its behaviour does not match the
spec below. Fix it.

## Precedence spec

Sources, from lowest precedence to highest: `defaults`, `file`, `env`, `flags`.

1. **Aliases.** Any source may declare aliases, mapping an old key name to a
   canonical one. The alias map is itself resolved by precedence first: when two
   sources alias the same old name to different canonical names, the one from the
   higher-precedence source wins. The resulting single map then applies to the keys
   of **every** source — including sources below the one that declared the alias,
   and including sources that declared no aliases at all.
2. **Scalars.** The value from the highest-precedence source that defines the key
   wins.
3. **Pinning.** A value marked `pinned` in a *lower* source beats an unpinned value
   from any higher source. When two or more pinned values compete, the
   highest-precedence pinned one wins. Pinning applies to every source, not to some
   of them.
4. **Lists.** A list-valued key does not replace — every source that defines it
   contributes, concatenated in source order (lowest first) with duplicates removed,
   keeping each value at the position of its **first** occurrence. A *pinned* list
   truncates that merge: sources strictly below the highest-precedence pinned list
   contribute nothing.
5. `explain(key)` must report the same outcome `resolve(key)` produces — it is the
   same question asked for a human, and the two must never disagree.
6. A key defined by no source raises `KeyError`.

## Constraints

- `conf/api.py` is the published surface and is **protected**: do not modify it.
- `test_resolve.py` is the checked-in test suite. It must still pass.
