`conf/` resolves configuration from four sources. Its behaviour does not match the
spec below. Fix it.

## Precedence spec

Sources, from lowest precedence to highest: `defaults`, `file`, `env`, `flags`.

1. **Scalars.** The value from the highest-precedence source that defines the key
   wins.
2. **Pinning.** A value marked `pinned` in a *lower* source beats an unpinned
   value from any higher source. When two or more pinned values compete, the
   highest-precedence pinned one wins. Pinning applies to every source, not to
   some of them.
3. **Lists.** A list-valued key does not replace — every source that defines it
   contributes. The result is the concatenation in source order (lowest first)
   with duplicates removed, keeping each value at the position of its **first**
   occurrence.
4. A key defined by no source raises `KeyError`.

`explain(key)` must report the same outcome `resolve(key)` produces — it is the
same question asked for a human, and the two must never disagree.

## Constraints

- `conf/api.py` is the published surface and is **protected**: do not modify it.
- `test_resolve.py` is the checked-in test suite. It must still pass.
