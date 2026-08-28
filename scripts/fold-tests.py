#!/usr/bin/env python3
"""Fold a crate's tests/*.rs into ONE integration-test target, tests/main.rs (AGENTS.md).

Idempotent: a crate that already has tests/main.rs is left alone. Run it after merging a branch
that added crates with tests/ (or after adding one yourself), then `scripts/check-test-mods.sh`.

    scripts/fold-tests.py            # every crate under plugins/, crates/ and bench/
    scripts/fold-tests.py plugins/x  # one crate

What it does per crate: writes tests/main.rs declaring `mod <helper>;` for each tests/<helper>/mod.rs
and `mod <stem>;` for each tests/<stem>.rs; rewrites `mod support;` / `mod common;` in those files
to `use crate::support;` (a file under main.rs is no longer a crate root, so the helper is reached
through the crate); adds `autotests = false` and a `[[test]]` to Cargo.toml. Files are not moved, so
blame survives. insta snapshots in a folded crate are named `tests__<module>__<name>.snap` — rename
the old `<module>__<name>.snap` files with `git mv`; the content does not change.
"""
import pathlib
import re
import sys

HELPERS = ("common", "support")


HELPER_MOD = re.compile(r"^(pub )?mod (" + "|".join(HELPERS) + r");[ \t]*$", re.M)


def rewrite_helpers(files) -> int:
    """`mod support;` -> `use crate::support;` — a file under main.rs is not a crate root."""
    n = 0
    for p in files:
        src = p.read_text()
        new = HELPER_MOD.sub(r"use crate::\2;", src)
        if new != src:
            p.write_text(new)
            n += 1
    return n


def fold(crate: pathlib.Path) -> bool:
    tests = crate / "tests"
    files = sorted(tests.glob("*.rs")) if tests.is_dir() else []
    if not files:
        return False
    if (tests / "main.rs").exists():
        # Already folded. A MERGE brings test files written against the old one-target-per-file
        # layout, so their helper declarations still need rewriting even though main.rs is there.
        n = rewrite_helpers([p for p in files if p.name != "main.rs"])
        if n:
            print(f"{crate}: rewrote the helper mod line in {n} already-folded file(s)")
        return False
    helpers = sorted(d.name for d in tests.iterdir() if d.is_dir() and (d / "mod.rs").exists())
    stems = [p.stem for p in files]
    clash = set(stems) & set(helpers)
    if clash:
        sys.exit(f"{crate}: a test file and a helper dir share a name: {sorted(clash)}")

    rewrite_helpers(files)

    lines = [
        "//! The crate's integration tests, as ONE target (`autotests = false` in Cargo.toml).",
        "//! Every `tests/*.rs` file is a module here — `scripts/check-test-mods.sh` fails the",
        "//! gate when a file is missing. One target means one link instead of one per file; test",
        "//! isolation comes from nextest running every test in its own process (`make test`).",
        "",
    ]
    for h in helpers:
        # A helper that does not silence its own dead code needs it silenced here: not every test
        # module uses every helper. One that already does must NOT get it twice (clippy's
        # `duplicated_attributes` is an error under -D warnings).
        if not re.search(r"allow\([^)]*\bdead_code\b", (tests / h / "mod.rs").read_text()):
            lines.append("#[allow(dead_code)]")
        lines.append(f"mod {h};")
    if helpers:
        lines.append("")
    lines += [f"mod {s};" for s in stems]
    (tests / "main.rs").write_text("\n".join(lines) + "\n")

    toml = crate / "Cargo.toml"
    t = toml.read_text()
    if "autotests" in t or "[[test]]" in t:
        sys.exit(f"{crate}: Cargo.toml already declares a test target; fold by hand")
    t = re.sub(r"^(name = \"[^\"]+\"\n)", r"\1autotests = false\n", t, count=1, flags=re.M)
    t = t.rstrip("\n") + '\n\n[[test]]\nname = "tests"\npath = "tests/main.rs"\n'
    toml.write_text(t)
    print(f"{crate}: {len(files)} files -> tests/main.rs, helpers={helpers}")
    return True


def main() -> None:
    root = pathlib.Path(__file__).resolve().parent.parent
    if len(sys.argv) > 1:
        crates = [pathlib.Path(a).resolve() for a in sys.argv[1:]]
    else:
        crates = sorted(
            list(root.glob("plugins/*")) + list(root.glob("crates/*")) + list(root.glob("bench/*"))
        )
    n = sum(1 for c in crates if c.is_dir() and fold(c))
    print(f"folded {n} crate(s)")


if __name__ == "__main__":
    main()
