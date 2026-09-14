#!/bin/sh
# bench.sh <dir> <evals.yml> <plugin/model> — for each eval question run
# a headless session told to use <dir>/.index via /folder-index query,
# and a baseline with no index. Prints a TSV: q, arm, hit, seconds,
# input_tokens, output_tokens.
dir="${1:?usage: bench.sh <dir> <evals.yml> <plugin/model>}"
evals="${2:?usage: bench.sh <dir> <evals.yml> <plugin/model>}"
pm="${3:?usage: bench.sh <dir> <evals.yml> <plugin/model>}"
exec python3 - "$dir" "$evals" "$pm" <<'PY'
import json, os, re, subprocess, sys, time

d, evals, pm = sys.argv[1:4]
plugin, _, model = pm.partition("/")
bin_ = os.environ.get("BOUGH_BIN", "bough")

# evals.yml is a flat list of {q, expect_path}; parse it without a yaml dep.
items, cur = [], None
for line in open(evals, encoding="utf-8"):
    m = re.match(r"^\s*(-\s+)?(q|expect_path):\s*(.*?)\s*$", line)
    if not m:
        continue
    if m.group(1):
        cur = {}
        items.append(cur)
    v = m.group(3)
    if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
        v = v[1:-1]
    cur[m.group(2)] = v

env = dict(os.environ, BOUGH_WEB_ADDR="127.0.0.1:0")
print("q\tarm\thit\tseconds\tinput_tokens\toutput_tokens")
for it in items:
    q, want = it.get("q", ""), it.get("expect_path", "")
    arms = {
        "index": f"use {d}/.index via /folder-index query {d} {q}",
        "baseline": f"Answer about the folder {d}, citing file paths: {q}",
    }
    for arm, prompt in arms.items():
        t0 = time.time()
        try:
            out = subprocess.run(
                [bin_, "--headless", "--set", f"llm.plugin={plugin}", "--set", f"llm.model={model}"],
                input=prompt, capture_output=True, text=True, env=env, timeout=900,
            ).stdout
        except subprocess.TimeoutExpired as e:
            out = e.stdout or ""
            if isinstance(out, bytes):
                out = out.decode(errors="replace")
        secs = time.time() - t0
        reply = "\n".join(l for l in out.splitlines() if not l.startswith("[usage] "))
        inp = outp = ""
        for l in out.splitlines():
            if l.startswith("[usage] "):
                u = json.loads(l[len("[usage] "):])
                inp, outp = u.get("input_tokens", ""), u.get("output_tokens", "")
        hit = int(bool(want) and want in reply)
        print(f"{q}\t{arm}\t{hit}\t{secs:.1f}\t{inp}\t{outp}", flush=True)
PY
