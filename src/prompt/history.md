## Command history

Every bash() you run is remembered across sessions — command, tags, exit code,
duration, what it printed, and the program it ran in — scoped to this project.

**OPEN THE MEMORY FIRST when the request names something that has a name.** A
ticket or PR (`NME-1666`, `#31`), a service, a repo, an environment, a tool with
a fiddly invocation. One `bough tags show` costs a second and answers "has this
been done here, and what worked" — the question you would otherwise spend three
exploratory commands guessing at. Reconnaissance you can skip is the point of
this memory; it is not an archive to consult when curious.

Concretely, before you explore: the work is for NME-1666, so run
`bough tags show linear.nme-1666` and read what the last session already did.

### Picking the right lookup

The three verbs answer different questions and do not substitute for each other.

    you know the NAME            bough tags show TAG
    you know a WORD it printed   bough tags sql "… command_history_fts MATCH …"
    you know neither             bough tags similar "what you are trying to do"

`show` is an EXACT match on a tag. It is the right verb for a ticket, and it is
the only one that reliably is — `similar` scores the text of commands, and a
ticket's commands are `git add`, `cargo test`, `gh pr edit`, whose text says
nothing about the ticket. Asking `similar` for a ticket you can name returns
somebody else's ticket that happens to phrase things alike.

### Reference tags have a shape, and guessing it wrong finds nothing

A reference is `tracker.full-id`: the NAMESPACE is the system it lives in, the
ID is the key exactly as that system writes it, lowercased.

    NME-1666 in Linear   →  linear.nme-1666     NOT nme.1666, not linear.1666
    PR #31               →  pr.31
    commit 3c1c78e       →  commit.3c1c78e

`bough tags show nme.1666` answers "no commands tagged" even when the ticket has
a hundred rows under `linear.nme-1666`, because it is an exact match and that is
a different tag. A miss is worth one more attempt, not an assumption that the
memory is empty: `bough tags sql "SELECT DISTINCT tag FROM command_tags WHERE
tag LIKE '%1666%'"` finds the real name in one query. Tag the work with the same
reference while you do it, and the next session finds it by name.

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
    bough tags show TAG         the commands under TAG, newest first, exit code
                                first — add --program for the round each ran in
    bough tags sql "SELECT …"   a read-only SELECT, rows as JSON
    bough tags similar "text"   semantic recall — for when you cannot name it

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
