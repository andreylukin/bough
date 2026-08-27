"""V6's live half: assert the SCREEN's prose, on whatever haiku happened to say.

Reads a `shell-use text` capture on stdin and the prompt that produced it as argv. Two checks,
selected by `--markers-only`:

  * no markdown marker survived into the answer's prose, and
  * the greedy-wrap invariant: a line followed by a continuation line is FULL — the next line's
    first word could not have fitted on it. A durable chunk boundary (M10) is exactly a violation
    of that: a short line broken early whose successor's first word would have fitted. This is
    the model-independent way to say "the accumulated document was wrapped on paint, not the
    chunk", because it holds for any text a greedy wrapper produced and fails for a wrapper that
    honoured a network boundary.
"""

import sys

argv = sys.argv[1:]
markers_only = "--markers-only" in argv
argv = [a for a in argv if a != "--markers-only"]
prompt = " ".join(argv[0].split())

rows = [r.rstrip() for r in sys.stdin.read().split("\n")]

# The transcript's own columns and the end of the answer, anchored on the step marker the turn
# always ends with — never on the echoed prompt, which a long answer scrolls off the top.
end = next((i for i, r in enumerate(rows) if "turn ended" in r), None)
if end is None:
    sys.exit("the turn has not ended on screen")
left = rows[end].index("\u2500")
col = [r[left:] if len(r) > left else "" for r in rows[:end]]

# What is NOT the answer's prose, and what therefore breaks a paragraph run:
#   * the echoed prompt (its own measure, and it butts up against the first block),
#   * a tool-call row, which carries a command VERBATIM - a heredoc of markdown source is still a
#     command, so its `**` is not evidence either way,
#   * the step markers, which are chrome.
body = []
for r in col:
    s_ = r.strip()
    echo = bool(s_) and s_ in prompt
    chrome = r.lstrip().startswith(("\u25b8", "\u00b7", "\u2500"))
    body.append("" if (echo or chrome) else r)
# Trim the blank run above the answer.
while body and not body[0].strip():
    body.pop(0)
if not [r for r in body if r.strip()]:
    sys.exit("the answer is not on screen")

for r in body:
    if "**" in r or "`" in r:
        sys.exit("a literal inline marker survived to the screen: %r" % r)
    if r.lstrip().startswith(("## ", "# ", "- ", "* ")):
        sys.exit("a block marker was never parsed: %r" % r)
if markers_only:
    print("prose rows=%d, no marker" % len([r for r in body if r.strip()]))
    raise SystemExit(0)

for r in body:
    if r.endswith("-"):
        sys.exit("a row ends in a bare hyphen: a word was split: %r" % r)

# Paragraph by paragraph: a contiguous run of non-blank rows is one wrapped block, and its own
# widest row is its measure.
runs, cur = [], []
for r in body:
    if r.strip():
        cur.append(r)
    else:
        if len(cur) > 1:
            runs.append(cur)
        cur = []
if len(cur) > 1:
    runs.append(cur)
if not runs:
    sys.exit("no wrapped paragraph on screen: nothing to check")

checked = 0
for run in runs:
    measure = max(len(r) for r in run)
    for j in range(len(run) - 1):
        a, b = run[j], run[j + 1]
        # A bullet starts a new block, not a continuation of the line above it.
        bullet_a = a.lstrip().startswith("•")
        bullet_b = b.lstrip().startswith("•")
        if bullet_b or bullet_a != bullet_b:
            continue
        word = b.strip().split(" ")[0]
        checked += 1
        if len(a) + 1 + len(word) <= measure:
            sys.exit(
                "a line broke before it was full — a chunk boundary became a line break:\n"
                "  %r\n  + %r (measure %d)" % (a, word, measure)
            )
if checked == 0:
    sys.exit("no wrap boundary was exercised: the answer never wrapped")
print("paragraphs=%d, wrap boundaries checked=%d" % (len(runs), checked))
