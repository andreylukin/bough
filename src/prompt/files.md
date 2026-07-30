## Files — one editing idiom

await view(path) — the file as a `[path#TAG]` header plus numbered `N:text` lines.

await patch(input) — hash-anchored line edits against that TAG. It echoes each
patched file's NEW tag.

await write(path, content) — new files and wholesale rewrites. It echoes the new
tag too, so a file you just wrote can be patched immediately without viewing it.

That is the entire editing surface. There is no read() and no edit(). Raw file
content comes from `Bun.file(path).text()` or `bash` — you have the full runtime and do
not need a host function for it. The path must be ABSOLUTE: your program's own working
directory is not the workspace (see Workspace), so `Bun.file("notes.txt")` reads
somewhere else and throws ENOENT for a file that is sitting right there. The file verbs
above and bash() resolve relative paths against the workspace; the raw runtime does not.

view() + patch() is how you change an existing file. You NAME lines instead of
reproducing them, so the code you are editing never has to survive your own string
quoting: backticks and `${...}` in the target file cannot corrupt the match, which
is the most common way an edit round is wasted.

It is also what makes shared work safe. The TAG pins the version you read. If the
file changed underneath you but your lines were untouched, your edit is rebased
onto the new version and lands. If your lines WERE touched, you get a conflict
naming the file and the line range — someone else edited exactly there. Re-view
that file and redo the edit against the new text; never retry the same patch blind.
With subagents sharing one checkout this is the only thing between two agents and a
silent clobber, so treat a conflict as information, not as a retryable hiccup.

Never pass view()'s output to patch(): the listing is for you to read, and its
`N:text` lines are not operations.

