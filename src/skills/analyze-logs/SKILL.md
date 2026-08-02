---
name: analyze-logs
description: Read a log file that is too big to read — compress it into its distinct statements with per-variable statistics, anomalies and correlations, using `bough patterns`
---

# Reading a log that is too big to read

`bough patterns` compresses a log into the handful of statements it is actually
made of. A 200,000-line file becomes forty templates with counts, typed variable
statistics, flagged anomalies, and the pairs that move together.

```bash
bough patterns --llm --top 20 /path/to/app.log
```

It ships with bough. There is nothing to install, no network access, and it runs
on a stopped server — which matters, because "why did the server not come up" is
answered by reading its log.

## Use it instead of reading the file when

- the file is more than a few hundred lines
- you do not yet know what you are looking for
- you want to know what is *most common* or *most severe*, not what is *last*

Use `grep`/`tail` instead when you already know the exact string, or when the
file is short enough to read whole. This tool answers "what is in here", not
"find me this".

**Never `cat` a large log into context.** That is the failure this exists to
prevent: it burns the context window on the least informative representation of
the data, and the answer is usually not in the visible portion anyway.

## Reading the output

`--llm` leads with a `## Problems` section (ERROR and FATAL patterns) and then
`## Everything else`. That ordering is deliberate — work through it in order.

Each pattern shows a template with typed placeholders, then one line per variable
slot:

```
### #1 [ERROR] 18,402 lines (1.5%)
​```
ERROR [<hex>] Timeout connecting to <ipv4>:<int> after <duration>
​```
- slot 0  id  ~12,204 distinct / 18,402
- slot 1  ipv4  12 unique  10.0.1.15 (34%), 10.0.1.22 (28%)
- slot 3  duration  40 unique  p50=120ms p90=3.1s p99=4.8s max=5.00s
- ⚠ burst: peak bucket held 4,102 lines against a median of 31
```

What the slot kinds mean:

- `id` — a different value nearly every line. Something to *join on*, not to
  trend. Top values are deliberately suppressed; three arbitrary request IDs are
  not hot spots.
- `enum` — few values, repeated. The percentages are the fact worth having.
  Categorical, so no quantiles are shown.
- `duration` / `bytes` — normalized to ms and to bytes, so quantiles are
  comparable even when the log mixes `1.5s` and `900ms`.
- everything else — `ipv4`, `uuid`, `path`, `url`, `hex`, `int`, `float`.

## What the numbers are, and are not

- Counts are **exact**.
- Quantiles are within **1% relative error**; unique counts within about **1.6%**
  and shown with a `~`. Do not report either as exact.
- `⚠` lines are observations, not diagnoses. "These rise and fall together" is
  not "this caused that" — co-occurrence cannot distinguish a common cause.
- If the header says the cluster cap was reached, counts are **lower bounds**.
- Examples are a uniform random sample of the pattern's lines, not the first
  ones — so an example is representative, not chronologically first.

## When the patterns look wrong

One statement split across several near-identical templates, or several distinct
statements merged into one that is mostly `<*>`, is a clustering threshold
problem:

```bash
bough patterns --threshold 0.6 app.log   # stricter: splits more
bough patterns --threshold 0.3 app.log   # looser: merges more
```

For a syslog-style file whose timestamps carry no year, pass `--year 2026` so the
time span is not reported as 1970.

## Other formats

```bash
bough patterns --json app.log     # stable shape, for post-processing in code
bough patterns --human app.log    # colored bars, for a person at a terminal
```

Reach for `--json` when you want to compute over the result — sort slots, diff
two runs, extract every pattern above a rate — rather than read it.

## Working from a shell pipeline

It reads stdin, so it composes:

```bash
kubectl logs deploy/api --since=1h | bough patterns --llm
journalctl -u nginx --no-pager | bough patterns --llm --top 30
git log --format=%s | bough patterns --human      # any line-oriented text
```

Narrowing *before* the analysis is often the right move on a very large file —
`grep -i timeout huge.log | bough patterns --llm` answers "what shapes do the
timeouts come in", which the unfiltered run would bury.
