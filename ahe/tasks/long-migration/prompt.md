`svc/ops/` is twenty small modules that all still return the legacy `(ok, payload)`
tuple. `svc/result.py` already defines the replacement. Migrate every one of them.

## What must be true when you are done

1. **Every `svc/ops/<name>.py`** returns `Ok(value)` on success and `Err(message)`
   on failure, imported from `svc.result`. No function under `svc/ops/` returns a
   tuple any more, and the literals `(True,` and `(False,` do not appear there.
2. **The error messages are unchanged** — same text, same failure conditions. A
   migration that also "fixes" what counts as an error is not this task.
3. **The `_or` wrappers keep their signature and behaviour**: `<name>_or(default,
   arg)` returns the value on success and `default` on failure. Internally they
   use the `Result`, not tuple unpacking.
4. **`svc/result.py` is not modified.** It is already correct.
5. `svc/ops/__init__.py` keeps re-exporting all forty names.

All twenty. A migration that lands nineteen has not landed.

`test_ops.py` is the checked-in test suite. It must still pass.
