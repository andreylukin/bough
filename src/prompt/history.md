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
    bough tags similar "text"   semantic recall, where the vector layer exists

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
