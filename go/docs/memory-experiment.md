# The local memory experiment (September 2026)

What was tried, what it measured, and why bough's memory is the graph plus
the reasoner rather than a local model. Written so nobody re-runs this
without reading it first.

## The question

Could a small local language model hold everything a session (and then
every session) has seen, and answer the hosted reasoner's questions about
it, so the reasoner only ever sees what it needs? Two lines of research
suggested yes:

- *Language Models Can Control Their Own Attention* (Ho et al., arXiv
  2609.02737): the model declares which context segments it attends to
  and the engine masks the KV cache; tool results are the natural
  segments; "reversible compaction".
- Qwen3.8-Flash-Next's hybrid architecture: recurrent (Gated DeltaNet)
  layers that compress history into a fixed-size state, plus a 51B
  hashed n-gram embedding table held in host RAM. The recurrent state is
  the part that looked like a memory.

Non-Chinese models were a hard constraint on the work machine, so the
candidates were IBM Granite 4.0 H (Mamba-2 hybrid), NVIDIA Nemotron 3
Nano 4B, and Google Gemma 4 E2B/E4B.

## What was built and measured

Everything ran on mlx-lm on Apple silicon. Benchmarks used the agent's own
history: exported bough sessions on the work machine (never copied off
it; only scores came back) and this session's own Claude Code transcript
on the development Mac, where the answers were known and questions could
be written by hand.

### 1. Full-text recurrent state (Granite 4.0 H-Small, 32B/9B active)

Every history entry fed into one recurrent state per session, saved to
disk with `save_prompt_cache`, reloaded on resume, forked per question.

| session tokens | state size | exact-value recall | answer latency |
|---|---|---|---|
| 108K | 1.9 GB (16-bit), 0.63 GB at 4-bit | 2–3 of 3 | 3.5 s |
| 400K | 6.5 GB, 1.9 GB at 4-bit | 4 of 11 | 14 s |
| 128K, own transcript, 25 hand-written questions | | 8 of 25 | 2.5 s |

Narrative questions on the own transcript (8, judged by hand): 1 right,
1 mostly right, 5 half right with a fabricated half, 1 wrong. The state
kept the gist and invented the details. Its chunk citations were noise:
none within three chunks of the true source.

Findings that matter:

- Ingest runs at ~230 tokens/s on an M5 Pro, ~110 on an M4 Pro; a 1M
  session is over an hour of compute and cannot be parallelised for one
  state.
- A 4-bit KV cache during prefill is ~4x slower in mlx-lm; keep the
  resident state 16-bit and quantise only forks.
- Forcing a JSON reply out of these models dropped accuracy from 3 of 3
  to 0 of 3 on the same questions; the model commits to a value before it
  has retrieved anything. Prose plus mechanical verification works.
- H-Tiny (7B/1B active) ingests 5x faster and was wrong about half the
  time on realistic outputs; random-string needles fail on both.

### 2. Descriptor (gist) state

The same state fed one 120-character line per tool output instead of the
text, to fit whole histories. 12x smaller. On the work machine at 1M
tokens: 0 of 27 at naming the right chunk, 3 of 27 answering directly.
BM25 and vector search over the same descriptors: 3 and 6 of 27 in the
top five. The descriptors did not carry the values the questions asked
for. Dead.

### 3. Drawer, index, reader

Every chunk verbatim in SQLite; FTS5 plus a static embedding
(model2vec `potion-base-8M`) over the full text; a small model reads the
top hits; every value it asserts must occur verbatim in a hit chunk.
Same 25 questions:

| reader | chunks | vote | correct | wrong | abstained | latency |
|---|---|---|---|---|---|---|
| Granite H-Tiny | 8 full | no | 14 | 6 | 5 | 3.7 s |
| Granite H-Tiny | 4 windowed | no | 6 | 9 | 10 | 1.2 s |
| Granite H-Tiny | 8 full | two reads must agree | 11 | 3 | 11 | 6.6 s |
| Granite H-Small | 8 full | no | 12 | 12 | 1 | 7.1 s |
| Gemma 4 E2B | 8 full | no | 6 | 2 | 17 | 6.4 s |
| Gemma 4 E2B | 8 full | yes | 4 | 0 | 21 | 8.5 s |
| Gemma 4 E4B | 8 full | no | 5 | 1 | 19 | 15 s |

Retrieval put the right chunk in the top hits nearly every time. The
misses were the reader's: a grounded value from the wrong line. A bigger
reader did not help; Gemma was precise and timid; windowing chunks to the
lines around query terms removed answer lines as often as distractors.

Two poisoning bugs, found only by live runs, are worth remembering for
any future design that lets a model answer from indexed history: a
session's own question was indexed and then "verified" an invented
answer against itself; and one session's memory answer, stored as a tool
output, became evidence for the next session's invented answer. Evidence
has to be restricted to tool outputs and ledger records, and outputs of
the memory itself must never be evidence.

Per-chunk KV caches for a reader (so reading costs no prefill) were
sized and rejected: hybrids carry a 40–60 MB fixed recurrent state per
cache, pure-attention models cost 72–512 KB per token; only Gemma 4 E2B
lands near 6 MB per 1K-token chunk, and the saving is ~1.5 s per recall.

## The decision

Every benchmark said the same two things: retrieval over verbatim text is
reliable, and a small local model reading it is the weak link (50–70%
precision, or high precision with most questions abstained). The best
reader available is the hosted reasoner itself, which already sees
citations and can pull a chunk in full with the focus tag.

So bough's memory is:

- **The graph** (`plugins/graph`): typed, bi-temporal relations with
  evidence, backfilled from history, injected into the prompt by
  workspace, and traversable by the reasoner with `tools.graph.search`,
  `neighbors`, `timeline`, `resolve`, `assert`, `invalidate`. Auto-memory
  writes facts per turn on the small model.
- **The placeholder projector** (`plugins/memtier`): past a size budget,
  old tool outputs collapse to one-line placeholders; `<focus seq=N>`
  brings one back for the turn. History on disk is never touched. The
  llm-small row writes the index lines and picks per turn; without it,
  first lines and recency.

What the graph lacks and the drawer had: an edge's evidence is free
text, not a `session#seq`, so traversal ends at a paraphrase rather
than at a line the reasoner can focus. That is the one piece of the
experiment worth carrying over, and it needs no model: evidence that
names a chunk, and a citation check on the reasoner's replies against
stored text.

## Not worth re-running

- A recurrent state as an oracle for exact values, at any size tested.
- Descriptors in a state as a navigator.
- Small readers under 10B as the memory's voice to the reasoner.
- JSON-schema output from Granite or Gemma small models on retrieval
  tasks.

Worth running if the constraint changes: a reader that is trained for
extractive QA over long inputs, or a bigger allowed local model; the
drawer-and-index half of the design is a day's work to bring back and
the poisoning rules above are the part that took the day.
