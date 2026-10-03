# engine-clm: editable context on the unreal runtime

CLM is the shipped default for the `loop` row. Run normally, or select it
explicitly for a session:

```sh
bough --set loop.plugin=engine-clm
```

An existing overlay that names another engine is respected. To opt out, use
`bough --set loop.plugin=engine-unreal` (or `plugin: engine-unreal` on the
`loop` row). Its append-only request behavior is unchanged. The same native tools, providers, hooks, cancellation, budgets,
async jobs, history, and UI are reused. Windows is unsupported, as with unreal.

This is an independent, experimental **zero-shot** implementation of the core
idea in [Context Language Models](https://arxiv.org/abs/2609.37725) (2026-09-29):
let the acting model edit the context that its next invocation receives. It is
not a reproduction of the paper's reinforcement learning, trained model,
benchmarks, token-budget experiments, or KV-cache serving infrastructure. No
code or prompts from the paper's noncommercial reference implementation are
included; these Go changes follow bough's Apache-2.0 license.

## What the model can edit

Before the first model request the engine creates a session-specific Markdown
file under `$BOUGH_SCRATCH/.bough-clm/` (the working directory is the fallback).
Its exact path is included in immutable CLM guidance. The `context-tools` row provides bounded native tools for inspecting and editing
that context. The model can also use the same `view`, `write`, `patch`, or shell
tools it already has. There is no mandatory
summary schema and no hidden summarizer. The model can replace, reorganize,
shorten, or empty this file. Existing file-tool hooks and write guards still
apply to those ordinary file calls; selecting this engine adds no writable roots
or security permissions.
Toolkit mutations use the current native `write` policy; a session without that
authority can inspect and dry-run, but cannot commit model-directed edits.

The engine appends new messages and tool events, but never restores older
acknowledged history that the model removed, except the active user task described
below. Persisted model responses, including
final answers, are mirrored after the audit store accepts them. The next model
request uses the edited file rather than the full historical transcript.

The frozen system prompt, CLM provenance guidance, tool definitions and execution
policy are outside that editable file. The file is sent as explicitly labelled
model-authored notes, not authenticated instructions. Role labels or claims of
approval typed into it cannot alter those boundaries. The current admitted user request and its mid-turn corrections stay in their
actual user role outside the notes throughout tool continuations, even after the
model rewrites or empties the file. A new real user turn replaces that task;
background notices, heartbeats, and model-authored text cannot replace it.
Task provenance comes from admitted input IDs in the audit history, and is
reconstructed on resume/fork. Each child keeps its own delegated task. Other
newly delivered events retain their native roles until acknowledged. Fresh native tool updates retain their corresponding calls
and results outside the notes, even if those calls were erased from the file.
Unrelated historical calls do not come back. Unrelated historical reasoning and opaque provider IDs are omitted. Original
signed thinking required by retained native tool cycles stays outside the file,
with its provenance envelope intact for the existing provider adapter. The model
cannot edit those blocks. Providers may still impose conversation-prefix binding
constraints; structural offline rendering tests are not a live-provider guarantee.

Image results have `[[clm-image:...]]` markers instead of base64 bytes in the
Markdown. Keeping a marker retains the original image result and its native call
in subsequent requests; deleting the marker discards that visual context. Markers
only reference the current session's existing audit items. Other historical calls
are not replayed. As with any deliberate context removal, discarded information
may no longer be available without rereading its source. This is an image-marker
projection, not a claim of full multimodal equivalence with the paper.

## Context toolkit

The default `context-tools` row exposes six native tools only in CLM sessions:

- `context_inspect` captures an immutable snapshot and returns its `snapshot_id`,
  an opaque revision, content hash, and exact byte and line counts
- `context_read` returns a bounded page; `context_search` returns literal match
  ranges. Reuse one snapshot ID for parallel reads and searches, and follow
  `next_offset` for stable pagination even while another call changes the notes
- `context_edit` applies an atomic batch of up to 64 half-open UTF-8 byte ranges.
  Each edit addresses the result of the preceding edit. Use descending ranges
  when the offsets all came from the original snapshot
- `context_offload` durably archives an excerpt before replacing it in the live
  notes. Omit `replacement` to leave an archive-ID marker; an explicit empty
  string removes the excerpt. Retain the returned `archive_id`
- `context_restore` inserts all or a selected range of an archive into current
  notes. It preserves intervening messages and edits, rather than rewinding a
  private cursor or replaying an old session

All mutations require `expected_revision`; stale revisions return a structured
`revision_conflict` without overwriting anything. Inspect again and rebase the
intended changes. `dry_run: true` validates the candidate and returns its size
and hash without changing files or creating an archive; the real commit still
checks its revision and current write policy. Parallel writers never silently
win over each other. Preparation happens outside a short, per-context commit
gate, while reads of existing snapshots do not take that gate.

Toolkit commits use the native `write` row’s authoritative path guard, including
local write roots, project roots, symlink resolution, and row removal. They run
the normal `pre-code-exec` and `post-result` hooks under their own names:
`context_edit`, `context_offload`, and `context_restore`. Hooks can deny them or
rewrite their structured `event.args`; `event.code` contains their complete JSON
arguments. A hook explicitly matching only `write` or `patch` must opt into these
new names. Toolkit calls have no caller-controlled target path, and hooks cannot
rewrite one to reach another context. Dry runs require no write permission and
return validation, size and hash, not a rendered diff.

Read output is bounded to 16 KiB of raw text and 32 KiB of JSON-encoded text;
search returns at most 100 matches. Follow `next_offset`, not a guessed page
size. Eight snapshots are retained per live context; an expired snapshot returns
`snapshot_expired`, so inspect again. Archives are private to a context, checksum
verified, and capped at 128 per context. Historical forks copy their selected
checkpoint’s archives into an independent scope. Arbitrary archive paths and
references from another session are rejected.

Tool responses remain in the audit log and fresh native call/result protocol,
but the toolkit does not append full read results back into its own editable
notes. An inspection receipt also does not invalidate the revision it just
returned. The private cursor format is version 2; version-1 state is read
compatibly and upgraded on a persisted change. Older binaries refuse version-2
state rather than silently discarding its archive metadata.

The built-in `context-toolkit` skill ships in the binary. Request it with
`/context-toolkit` or read `view("builtin:context-toolkit")`; it does not install
files in HOME. Its short catalog entry is enabled only for CLM with the toolkit
row enabled, and ordinary user skill pools and off switches still take
precedence. Disable the `context-tools` row to retain only the ordinary file
editing workflow. No summarizer, tokenizer claim, or arbitrary expression
execution is added.

## Persistence and safety

Bough history and the unreal session log remain append-only audit records. The
editable projection does not rewrite either. A separate private cursor records
which events were mirrored and acknowledged. Do not edit or delete that cursor.
Resume retains the edited file; switching back to unreal restores its ordinary
audit-based request replay, without changing its saved system file.

Each child gets its own context and cursor. A fork restores an immutable snapshot
from the selected completed turn, not today's parent file. If a CLM fork point
has no usable snapshot, it is refused instead of reconstructing discarded text.
Forks of older unreal sessions intentionally start from their inherited audit
context. Snapshots and cursors live beside engine session state; the editable
file remains in the existing tool-writable area.

File/cursor writes use a recovery journal. An interrupted engine write is
completed or rolled back on the next access without duplicating the appended
event. A conflicting outside edit during recovery fails closed and preserves
the journal and file for repair. Missing cursors, invalid UTF-8, symlinks, or
missing live files are errors; they do not silently reset context from history.
For a missing live file, restore that file (an empty UTF-8 file is valid).
For missing/corrupt private state, restore its matching backup or use a new
session. Do not delete the cursor to “fix” the error.

Native `write` and `patch` calls to a context file are coordinated with that
context’s projection updates. Unrelated paths and independent child contexts
can run in parallel. This is cooperative coordination, not a process sandbox. Concurrent external editors,
background shell writers, or `run_js` file writes in `tools: both` do not share
that lock; use native `write`/`patch` for context changes and avoid those races.
A detected conflicting edit refuses the call, but no filesystem API can promise
compare-and-swap against arbitrary uncooperative writers.

## Size and provider limits

`context_max_bytes` sets a positive live-file cap (default 1 MiB). Every request
shows the current size and cap **in bytes, not tokens**, with a reminder to
shorten the file before either the cap or the model's context limit is reached.
The count excludes immutable instructions, the retained active user task, tool
schemas, retained images, and fresh protocol messages, so it is not a total provider-context measurement. Select a smaller
cap for a small-context model; no tokenizer-specific budget is claimed.

An invalid or oversized file is refused before a provider call; repair or shorten
it and send another message. If an incoming event itself exceeds the cap, use a
larger configured cap or a new session. A provider context overflow remains
sticky until the model changes or the live file's content actually changes;
shortening the file then sending a message permits retry, including after resume.
No automatic truncation or summary is substituted. As with the base engine,
paid-provider behavior needs an explicitly authorized live test; the regression
suite uses deterministic offline adapters.
