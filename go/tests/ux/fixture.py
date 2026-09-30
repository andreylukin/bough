#!/usr/bin/env python3
"""Fixture HOME helpers for go/tests/ux.

  fixture.py age DIR          spread DIR's sessions over the last two weeks
  fixture.py clone SRC DEST   copy a fixture HOME and repoint its paths

seed.sh writes every session within a minute or two, so the sidebar's
recency groups and "3 days ago" labels never show; `age` moves each
session's entry times back by a whole number of days and hours (entries
keep their spacing) and sets the file's mtime to match.

Sessions record absolute paths (cwd, engine store, worktrees), so a
fixture copied to a persona's own HOME would point back into the
original; `clone` rewrites SRC's path to DEST's in every text file.
"""
import datetime as dt
import os
import random
import shutil
import sys


def age(home: str) -> None:
    hist = os.path.join(home, ".bough", "history")
    names = sorted(f for f in os.listdir(hist) if f.endswith(".jsonl"))
    rnd = random.Random(7)  # the same fixture ages the same way every time
    for i, name in enumerate(names):
        # A third today, the rest spread over two weeks, newest first.
        days = 0 if i % 3 == 0 else rnd.randint(1, 14)
        shift = dt.timedelta(days=days, hours=rnd.randint(0, 6), minutes=rnd.randint(0, 59))
        path = os.path.join(hist, name)
        out: list[str] = []
        last: dt.datetime | None = None
        with open(path) as f:
            for line in f:
                if '"at":"' in line:
                    head, rest = line.split('"at":"', 1)
                    stamp, tail = rest.split('"', 1)
                    t = dt.datetime.fromisoformat(stamp) - shift
                    last = t
                    line = f'{head}"at":"{t.isoformat()}"{tail}'
                out.append(line)
        with open(path, "w") as f:
            f.writelines(out)
        if last:
            os.utime(path, (last.timestamp(), last.timestamp()))


def clone(src: str, dest: str) -> None:
    src, dest = os.path.realpath(src), os.path.abspath(dest)
    if os.path.exists(dest):
        sys.exit(f"fixture: {dest} exists")
    shutil.copytree(src, dest, symlinks=True)
    old, new = src.encode(), os.path.realpath(dest).encode()
    for base, dirs, files in os.walk(dest):
        dirs[:] = [d for d in dirs if d != ".git"]
        for name in files:
            p = os.path.join(base, name)
            if os.path.islink(p):
                continue
            with open(p, "rb") as f:
                data = f.read()
            if old in data and b"\0" not in data[:4096]:
                st = os.stat(p)
                with open(p, "wb") as f:
                    f.write(data.replace(old, new))
                os.utime(p, (st.st_atime, st.st_mtime))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "age":
        age(sys.argv[2])
    elif len(sys.argv) == 4 and sys.argv[1] == "clone":
        clone(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
