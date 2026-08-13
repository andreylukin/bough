## Command history

Every bash() you run is remembered across sessions — command, tags, exit code,
duration, what it printed, and the program it ran in — scoped to this project.
Before trial-and-erroring a command someone (you, in a past session) likely
already got right — connecting to a container, a deploy incantation, a tricky
migration — check the memory first.

This holds for TOPICS, not just commands: when a request names something that
matches one of this project's tags (in the popular-tags note or a [history]
hint), query that tag before exploring fresh. It holds for REFERENCES too — if
the work is for a ticket or a PR, `bough tags show linear.eng-1234` is every
command already run for it.

When you do not know something, search the tags before exploring — and if a tag
comes back empty, try another spelling of it before concluding the memory is.

Some of it reaches you without being asked. A failing command gets a `[history]`
line under its output when the memory has something on it — that this exact
command has failed here before, or that OTHER commands here have already failed
the same way. The second one is the one to slow down for: it means the thing you
keep changing is not the thing that is wrong. Read the error, not the arguments.

A command that has failed three times in this session inside two minutes is NOT
RUN a fourth time: the output starts `[not run]`, nothing was spawned, and the
previous error is quoted. That is a loop, not a flake. Any edit makes it a
different command and it runs again.

The rest is reached through the CLI, in bash. There is no host function:

    bough tags                  this project's tag vocabulary, ranked
    bough tags last TAG...      per tag, the newest command AND WHAT IT PRINTED
    bough tags show TAG         the commands under TAG, newest first, exit code
                                first — add --program for the round each ran in
    bough tags sql "SELECT …"   a read-only SELECT, rows as JSON
    bough tags similar "text"   semantic recall, where the vector layer exists

`last` is the one to reach for when the question is "what did that say" rather
than "what have we run". It takes as MANY tags as you like in one invocation,
and answers in the order you asked:

    bough tags last pr.7911 pr.7913 pr.12337 pr.12395

That is one command, not four. Asking about a dozen entities one at a time is a
dozen round trips for an answer the memory could have given in one, and writing
the SELECT by hand is a dozen chances to write it wrong.

ONE TAG PER ARGUMENT. A colon separates tags, it does not build a compound one:
a command tagged `gh:inspect:pr.7911` carries the three tags `gh`, `inspect` and
`pr.7911`, so `t.tag = 'gh:inspect:pr.7911'` matches NOTHING. When recall comes
back empty, this is the first thing to suspect.

Add --json to any of them for parseable output, --all to cross projects, and
--limit N to widen a list.

`sql` refuses anything that is not SELECT or WITH, and opens the file read-only,
so it is safe against the database the server is writing to right now. Tables:

    command_history(id, session_id, ts, repo, cmd, tags, exit_code, duration_ms,
                    output_head, spill_path, message_id)
    command_tags(command_id, tag)          — one row per tag
    command_dirs(command_id, rel_dir)      — directories the command was about
    command_history_fts(cmd, tags, output_head) — FTS5; MATCH for keyword search

output_head is the first ~2k chars a command PRINTED, so you can recall results,
not just invocations; spill_path names the file holding a big output in full (it
may have been cleaned since — check before reading); message_id is the message
whose run_steps program ran it, so `messages.parts` gives you the whole round.
The same query can read the transcript: messages(id, session_id, role, parts,
created_at), messages_fts(text, message_id, session_id), sessions and turns.

THIS conversation's id is `$BOUGH_SESSION` in every shell you run — so a query
about your own session names it rather than guessing, and a command that needs
it passes it through the variable instead of a literal you composed:

    bough tags sql "SELECT cmd FROM command_history
      WHERE session_id = '$BOUGH_SESSION' ORDER BY ts DESC LIMIT 10"

Never write the id out yourself; you do not know it, and an invented one reads
as another conversation. To use it inside the program rather than in a command,
read it once: `const session = (await bash("echo $BOUGH_SESSION",
"bough:session:id")).trim()`.

The commands worth copying are the ones that WORKED — filter exit_code = 0.

    # what worked for docker here before
    bough tags sql "SELECT cmd FROM command_history JOIN command_tags t ON
      t.command_id = id WHERE t.tag = 'docker' AND exit_code = 0
      ORDER BY ts DESC LIMIT 5"
    # keyword search when you do not know the tag
    bough tags sql "SELECT h.cmd, h.exit_code FROM command_history_fts f JOIN
      command_history h ON h.id = f.command_id WHERE f.cmd MATCH 'migrate'
      ORDER BY h.ts DESC LIMIT 10"

Never open the database file yourself — not with sqlite3, not with a library.
The CLI is read-only by construction and the file is one a live server is
writing to; a stray write or a long lock there is a broken turn for everyone.

## Notes — the why beside the how

The command memory records what ran and whether it worked. It cannot record
what it MEANT. That is `bough notes`, keyed on the same tags:

    bough notes                 every note, most out of date first
    bough notes show PATH       one note, and what resolves into it
    bough notes write PATH      replace its prose (markdown on stdin)
    bough notes append PATH "…" add one line to its log
    bough notes search WORDS    sections matching every word
    bough notes cites PATH      what its claims rest on

`bough tags show TAG` already prints the note above the commands, so most of
the time you do not have to ask for it separately.

A PATH is one or more TAGS, colon separated — `atlas`, `kubectl:rollout`. One
tag is a note about that word; more is a note about the combination.

A note holds a decision and its reason, a constraint, a thing that will bite
the next session. It holds NO commands and no output — those are already in
`command_history`, and copying them there makes two records of one fact that
age apart. Cite instead.

**Cite what a claim rests on.** `[cmd:1234]` for a command whose id you got
from `bough tags sql`, `[file:src/x.rs@3c1c78e]`, `[url:…]`. A citation that
does not resolve is refused and named, so do not invent one — a claim you
cannot cite is fine, and it will simply be reported as uncited.

**Sections and where they show up.** `## Heading` starts a section. By default
a section inherits its note's tags and appears only on that note. A `tags:`
line under the heading NARROWS it, and a narrowed section appears on every note
whose tags include its own:

    ## Executor ordering
    tags: atlas
    DAG removal must land before the executor swap. [cmd:8812]

Written on `atlas:rollout:prod`, that section now also shows up when reading
`atlas:backfill:dev`. Narrow a section when what you learned is true of the
subject generally, not of the particular combination you were working on.
Leave it alone otherwise — the default is right most of the time.

Append to the log when you learn something a future session would otherwise
re-derive. Not progress reports, and not what a command already says.

If a note's claim turns out to be false, do not delete the sentence — rewrite
it with `bough notes write`, which keeps the old version on the record
(`bough notes history PATH`). A `> [!WARNING]` in a body means the memory
already knows the claim is disputed: read it before relying on the sentence
above it.
