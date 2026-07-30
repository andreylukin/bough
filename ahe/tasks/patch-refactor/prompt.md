`pipe/` threads a request context through a chain of stages. The context is passed
as a bare dict everywhere, which has caused three separate bugs. Replace it with the
`Ctx` dataclass already defined in `pipe/context.py`.

## What must be true when you are done

1. Every stage in `pipe/` takes and returns a `Ctx`, not a dict. No `dict` typing
   remains on a stage signature, and no stage indexes the context with `[...]`.
2. `Ctx` is **frozen**: a stage that needs to change a field returns a new one via
   `dataclasses.replace`. Nothing mutates a context in place.
3. Behaviour is unchanged for every input the checked-in tests cover, and for the
   error paths they do not: a missing key raised `KeyError` before and must raise
   `AttributeError` now, but an *unset optional* must still come back as `None`
   rather than raising.
4. `pipe/legacy.py` is a third-party vendored file and is **protected**: do not
   modify it. It calls the chain with a dict, so the entry point has to accept a
   dict from it and convert — without changing `legacy.py` and without leaving the
   dict shape anywhere downstream.

`test_pipe.py` is the checked-in suite and must still pass.
