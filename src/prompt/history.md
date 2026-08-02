## Command history

Every bash() you run is remembered across sessions — command, tags, exit code,
duration, and the directories it touched — scoped to this project. Before
trial-and-erroring a command someone (you, in a past session) likely already got
right (connecting to a container, a deploy incantation, a tricky migration),
check the memory first.

await history.sql(query) — read-only SELECT over the memory, returning rows as
objects. Tables:

    command_history(id, session_id, ts, repo, cmd, tags, exit_code, duration_ms)
    command_tags(command_id, tag)          — one row per tag
    command_dirs(command_id, rel_dir)      — directories the command was about
    command_history_fts(cmd, tags)         — FTS5; MATCH for keyword search

await history.similar(text) — semantic recall over the same memory ("get into
the running container" finds docker exec commands no keyword would). On machines
without the local embedding layer it rejects catchably; history.sql() with MATCH
is the fallback it names.

The commands worth copying are the ones that WORKED — filter exit_code = 0.

    // what worked for docker here before
    await history.sql("SELECT cmd FROM command_history JOIN command_tags t ON
      t.command_id = id WHERE t.tag = 'docker' AND exit_code = 0
      ORDER BY ts DESC LIMIT 5")
    // keyword search when you don't know the tag
    await history.sql("SELECT h.cmd, h.exit_code FROM command_history_fts f JOIN
      command_history h ON h.id = f.command_id WHERE f.cmd MATCH 'migrate'
      ORDER BY h.ts DESC LIMIT 10")
