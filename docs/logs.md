# Large logs

A coding agent meets logs constantly: a failing CI job, a server that did not come up, a container
printing the same warning ten thousand times. The obvious move is to read the file, and it is the
wrong one. Piping a 200,000-line log into the model spends the whole context window on the least
informative representation of the data, and the answer is usually not in the part that fits anyway.

bough's answer is a streaming compressor that turns an arbitrarily large log into a fixed-size
analysis: the distinct statements the file is made of, with counts, per-variable statistics,
flagged anomalies, and the patterns that move together. It runs in two places. Automatically, on
any shell output too big to return, which is the part that matters most because that is the output
nobody chose the size of. And on demand as `bough patterns`, when you or the model point it at a
file.

```bash
bough patterns --llm --top 20 /path/to/app.log
kubectl logs deploy/api --since=1h | bough patterns --llm
```

It ships in the binary, needs no network, and runs against a stopped server, which matters because
"why did the server not come up" is a question you ask of a log when nothing is running.

This page is the mechanism. For using the subcommand well, including how to read every field of the
output, the shipped [`analyze-logs` skill](../crates/bough-core/skills/analyze-logs/SKILL.md) is
the practical guide, and [cli.md](cli.md) has the flags. [specs/small.md](../specs/small.md) §1 and
[specs/hostfn.md](../specs/hostfn.md) §spill are normative.

## Output nobody chose the size of

A shell command's output is the one thing in a turn whose size the model does not control. It asks
for a test run and gets 12MB. Returning all of it lets a single noisy command eat half a context
window; truncating it throws away the middle permanently, and the failing assertion is almost never
in the first or last 5,000 characters.

So past **20,000 characters**, `bash` and `sh` stop returning output and start describing it. Three
things happen:

1. **The full output goes to a file**, streamed as it arrives rather than written at the end. This
   detail was a bug once and is worth keeping: the in-memory retention buffer caps at 400,000
   characters, so writing it out afterwards saved a file that had *already* lost its middle, under
   a banner reading "FULL OUTPUT SAVED". `seq 1 200000` produced 1.29MB and the file held 400KB.
   A tool that says it kept your output and did not is worse than one that admits it truncated.
2. **5,000 characters of head and 5,000 of tail come back verbatim**, because the invocation and
   the exit are the two parts that are almost always worth having.
3. **The middle is replaced by a digest**, not by an ellipsis. The same pipeline described below
   runs over the file and returns what the omitted output *consists of*: which statements, how
   often each fired, which were errors. Capped at 4,000 characters and clipped at a pattern
   boundary rather than mid-template, because a summary whose last entry looks corrupted invites
   exactly the re-run it exists to prevent.

The digest is skipped when it would not earn its characters: under 40 lines, or when fewer than one
line in four is a repeat of another. Output where nearly every line is unique has nothing to
compress, and a "3 lines → 3 patterns" header is pure overhead. In that case the marker instead
suggests `bough patterns --llm <path>` explicitly, since a model that has just been handed a
9,000-line file would not otherwise think to reach for it. When a digest *is* present that hint is
dropped rather than kept alongside, because its whole job was to get the analysis run and it has
already been run.

The marker also names the file, its true size in characters and lines, and spells out `rg`,
`view()` and the analysis as runnable commands, ending with "do not re-run the command to see the
middle". Nothing is lost, and the model is told where the rest is instead of being left to guess.

Two honest limits. No session scratchpad means no file to spill into, and the fallback is the older
generous head and tail (100,000 and 300,000 characters) with the middle marked. And output above
8MB is pointed at rather than analyzed: the pipeline runs at roughly 30,000 lines a second, but a
command can produce more than it is worth spending that second on.

## Why a subcommand and not a host function

The manual door is deliberately not one of the nineteen [host functions](programs.md). A host
function is a permanent widening of every program's API and of the system prompt that has to
describe it; a subcommand costs nothing until something runs it. The model reaches it the same way
you do, by writing `bash("bough patterns --llm build.log", "ci:analyze")` inside a program, and
that command is tagged like any other, so a later session can recall the analysis it already ran.

Apart from the spill path, nothing in `bough-core` calls the pipeline. It is synchronous,
dependency-free apart from serde, and reads no clock, no filesystem and no randomness, so the same
file analyzed twice produces the same output byte for byte.

## The pipeline

```text
strip timestamp → mask values → tokenize → cluster → attribute → accumulate
                                                                     ↓
                     rank ← correlate ← detect anomalies ← summarize
```

**Strip the timestamp.** It runs first because a timestamp is the one field guaranteed to differ on
every line: leave it in and every line becomes its own cluster, reducing the whole tool to a slow
`cat`. The match is anchored at the start of the line, because a mid-line scan turns every bare
integer into a candidate epoch and logs are full of bare integers. A line without a timestamp is
normal, not an error; build output and stack traces have none and cluster perfectly well.

**Mask the values.** Two lines that differ only in their values are the same log statement, and the
cheapest way to recognize that is to delete the values (the idea comes from CLP, Rodrigues et al.,
OSDI 2021). Each removed span leaves a placeholder that carries its *kind*, `<ipv4>`, `<duration>`,
`<uuid>`, `<path>`, rather than an anonymous blob. Typing separates statements a shapeless mask
would merge, and it tells the accumulator how to treat a slot before it has seen a single value.
One left-to-right scan, first alternative wins, and the order is load-bearing: every kind that
contains digits is a special case of "there is a number here", so `int` is tried last and only
claims digits nothing else wanted.

**Cluster what masking could not make identical.** This is Drain (He et al., ICWS 2017): a
fixed-depth tree whose first level is token count and whose next levels are the leading tokens. A
line that reaches a leaf is compared against the few templates already there and joins one if it
agrees on enough positions, generalizing the positions where they differ to `<*>`. The defaults are
the paper's, apart from the cap: similarity `0.4`, depth `4`, 100 children before a node stops
splitting.

A cluster is created or updated the moment a line arrives and is never revisited. That one-pass
property is what lets the whole thing stream, and it has a consequence worth stating: templates
depend on arrival order. Counts and statistics do not, only which positions happened to generalize
first.

**Accumulate, keyed on template position.** A template mutates as it generalizes, so statistics
keyed on what a token *said* at insertion time would scatter one slot's values across several
buckets. Two slot kinds are decided here rather than by the masker, because they are properties of
a distribution rather than of any single value: `enum` (a few values, repeated) and `id` (a
different value nearly every time). Every pattern buckets time against one shared origin and width
that only ever coarsens, so bucket 3 covers the same minutes for all of them and two patterns'
shapes are comparable.

**Anomalies, with a high bar.** Each detector is a plain, explicable rule that produces a sentence
about what it saw rather than what it concluded. The bar for firing is high on purpose, because
this output is mostly read by a language model, which will dutifully investigate whatever it is
told is anomalous. A missed anomaly costs a reader one scan of a table they already have; a phantom
one costs them the investigation.

**Correlation, as a lead and not a cause.** The most common question asked of a log is not "what
happened" but "what happened at the same time as this". Scoring is cosine rather than Pearson's r:
subtracting the mean on sparse log data makes co-absence the dominant signal and scores pairs of
unrelated rare patterns near 1, whereas cosine reads a zero as "nothing happened", which is what a
zero means here. No result says "caused".

**Ranking is the last decision and the most important one.** Sorting by count alone puts a million
INFO request lines above three FATAL ones, and the three are the reason anyone opened the file.
`--llm` therefore leads with a `## Problems` section and then `## Everything else`.

## What is bounded, and what that costs

One pass over the input, and nothing that scales with line count is retained. A line is folded into
its cluster's accumulators and dropped; what survives is a fixed cost per pattern. Three bounded
sketches do the work, each a clean-room implementation of a published algorithm:

| | | |
|---|---|---|
| DDSketch | quantiles | within 1% relative error |
| HyperLogLog | distinct counts | within about 1.6%, printed with a `~` |
| Reservoir (Vitter's Algorithm R) | examples | seeded, so a rerun prints the same lines |

So: counts are exact, quantiles and unique counts are approximate and marked as such, and the
examples are a uniform random sample rather than the first occurrences, which makes an example
representative but not chronologically first.

Clusters are capped at 10,000 and evicted least-recently-used. When the cap binds, the header says
so and the counts become lower bounds rather than being quietly absorbed.

## When the output looks wrong

One statement split across several near-identical templates, or several distinct statements merged
into one that is mostly `<*>`, is a threshold problem and not a bug:

```bash
bough patterns --threshold 0.6 app.log   # stricter: splits more
bough patterns --threshold 0.3 app.log   # looser: merges more
```

For a syslog-style file whose timestamps omit the year, pass `--year 2026` so the span is not
reported as 1970. And narrowing before the analysis is often right on a very large file:
`grep -i timeout huge.log | bough patterns --llm` answers "what shapes do the timeouts come in",
which the unfiltered run would bury.
