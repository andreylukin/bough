`regs/` is a plugin registry. `test_regs.py` passes — but only once, in a fresh
process, in the order it is written. Run it twice in the same process, or reorder
it, and it falls apart.

Make the registry properly isolated, without changing the tests.

## What must be true when you are done

1. **Two registries are independent.** Registering `alpha` in one registry does not
   put it in another, and registering the same name in two different registries is
   not a duplicate. A freshly constructed `Registry` has no plugins at all,
   whatever any earlier registry did.
2. **Duplicates are still rejected** *within* one registry: registering a name that
   registry already has raises `PluginError`.
3. **Options are per-registry.** `Registry(options)` starts from an empty
   configuration, copies the options in, and mutating one registry's options never
   shows up in another's — including in a registry constructed later with no
   options at all.
4. **`by_tag` stays correct.** It may be cached, but the cache belongs to one
   registry and must reflect every registration made on that registry.
5. `names()` is sorted; `get()` on an unknown name raises `PluginError`.

## Constraints

- `regs/api.py` is the published surface and is **protected**: do not modify it.
- `test_regs.py` is the checked-in test suite. It must still pass — and it must
  now also pass when it runs twice in the same process.
