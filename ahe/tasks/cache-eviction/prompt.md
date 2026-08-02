`lru/` is a bounded cache. Its behaviour does not match the spec below. Fix it.

## Spec

1. **Capacity is cost, not count.** Every entry carries a `cost`; the cache holds
   entries while their costs sum to at most `capacity`. `load()` reports that sum.
2. **Recency.** A successful `get()` counts as a use and makes that key the most
   recent. A `put()` — new key or replacement — also does. A `get()` that misses
   changes nothing. Eviction always takes the least recently used key first, and
   `evicted()` lists keys in the order they were evicted.
3. **Pinning.** A pinned entry is never evicted. If the only way to fit a new entry
   would be to evict a pinned one, the `put()` raises `CapacityError` and the cache
   is left exactly as it was — nothing stored, nothing evicted.
4. **Oversized entries.** An entry whose own `cost` exceeds `capacity` can never
   fit. That `put()` raises `CapacityError` and, likewise, changes nothing: it must
   not evict anything on the way out.
5. **Replacement.** Putting a key that is already present replaces its entry,
   including its `cost` and its `pinned` flag, and `load()` reflects the new cost.
   A replacement is not an eviction and must not appear in `evicted()`.

## Constraints

- `lru/api.py` is the published surface and is **protected**: do not modify it.
- `test_cache.py` is the checked-in test suite. It must still pass.
