#!/bin/sh
# check.sh <dir> — validate <dir>/.index. Exits non-zero on a broken
# link, a path:line citation past EOF or at a missing file, a stale
# manifest hash, or an orphaned leaf.
dir="${1:?usage: check.sh <dir>}"
exec python3 - "$dir" <<'PY'
import hashlib, os, re, sys

root = os.path.abspath(sys.argv[1])
idx = os.path.join(root, ".index")
bad = []
if not os.path.isfile(os.path.join(idx, "README.md")):
    print(f"missing {idx}/README.md")
    sys.exit(1)

pages = []
for d, _, fs in os.walk(idx):
    pages += [os.path.join(d, f) for f in fs if f.endswith(".md")]

link = re.compile(r"\]\(([^)#\s]+)(?:#[^)]*)?\)")
cite = re.compile(r"`([^`\s]+):(\d+)(?:-(\d+))?`")
linked = set()
for p in pages:
    rel = os.path.relpath(p, idx)
    text = open(p, encoding="utf-8").read()
    for m in link.finditer(text):
        t = m.group(1)
        if re.match(r"^[a-z]+:", t):
            continue
        dest = os.path.normpath(os.path.join(os.path.dirname(p), t))
        if not os.path.exists(dest):
            bad.append(f"{rel}: broken link {t}")
        else:
            linked.add(dest)
    for m in cite.finditer(text):
        path, line = m.group(1), int(m.group(3) or m.group(2))
        f = os.path.join(root, path)
        if not os.path.isfile(f):
            bad.append(f"{rel}: citation {path}:{line} missing file")
            continue
        with open(f, "rb") as fh:
            n = sum(1 for _ in fh)
        if line < 1 or line > n:
            bad.append(f"{rel}: citation {path}:{line} past EOF ({n} lines)")

leaves = os.path.join(idx, "leaves")
if os.path.isdir(leaves):
    for f in sorted(os.listdir(leaves)):
        p = os.path.join(leaves, f)
        if f.endswith(".md") and p not in linked:
            bad.append(f"leaves/{f}: orphaned (no page links to it)")

man = os.path.join(idx, "manifest.tsv")
if not os.path.isfile(man):
    bad.append("manifest.tsv missing")
else:
    for i, row in enumerate(open(man, encoding="utf-8"), 1):
        row = row.rstrip("\n")
        if not row:
            continue
        parts = row.split("\t")
        if len(parts) != 3:
            bad.append(f"manifest.tsv:{i}: want path<TAB>sha256<TAB>leaf")
            continue
        path, want, leaf = parts
        f = os.path.join(root, path)
        if not os.path.isfile(f):
            bad.append(f"manifest.tsv:{i}: stale, {path} is gone")
        elif hashlib.sha256(open(f, "rb").read()).hexdigest() != want:
            bad.append(f"manifest.tsv:{i}: stale hash for {path}")
        lp = leaf if leaf.endswith(".md") else leaf + ".md"
        if not os.path.isfile(os.path.join(leaves, os.path.basename(lp))):
            bad.append(f"manifest.tsv:{i}: leaf {leaf} missing")

for b in bad:
    print(b)
sys.exit(1 if bad else 0)
PY
