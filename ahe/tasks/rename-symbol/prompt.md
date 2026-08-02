In `svc/`, the method `PathResolver.resolve` is too vaguely named — it collides
with two unrelated things in the same package. Rename it to `resolve_path`, and
update every call site.

Rename **only that method**. These are different things that share the spelling and
must be left exactly as they are:

- `NameResolver.resolve` in `svc/dns.py`, and the call to it in `svc/index.py`
- the module-level function `resolve` in `svc/report.py`
- every occurrence inside a string literal, docstring or comment — including
  `HELP` in `svc/index.py` and `TEMPLATE` in `svc/report.py`, whose text is
  user-visible output and must not change

When you are done, `PathResolver` has a `resolve_path` and no `resolve`, and
nothing else in the package has changed name or behaviour.

`test_svc.py` is the checked-in test suite. It must still pass.
