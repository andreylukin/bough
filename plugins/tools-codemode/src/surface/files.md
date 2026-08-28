<!-- needs-any: view,patch,write -->
## Files — one editing idiom

<!-- needs: view -->
await view(path) — the file as a `[path#TAG]` header plus numbered `N:text` lines.

<!-- needs: patch -->
await patch(input) — hash-anchored line edits against that TAG. It echoes each
patched file's NEW tag.

<!-- needs: write -->
await write(path, content) — new files and wholesale rewrites. It echoes the new
tag too, so a file you just wrote can be patched immediately without viewing it.

<!-- needs: view,patch,write -->
That is the entire editing surface. There is no read(), no edit() and no
edit_file(old, new) — naming text you cannot see is exactly the regression these
three verbs exist to avoid. Raw file content comes from `bash` (`cat`, `rg`, `sed
-n`): the sandbox has no filesystem of its own, so every byte that reaches the
program came through a host function that recorded it.

<!-- needs: view,patch -->
view() + patch() is how you change an existing file. You NAME lines instead of
reproducing them, so the code you are editing never has to survive your own string
quoting: backticks and `${...}` in the target file cannot corrupt the match, which
is the most common way an edit round is wasted.

<!-- needs: patch -->
The `+` body rows are the exception, and the only place quoting still bites: that NEW
text does travel through your JS literal. A backtick or `${` you are INSERTING — a
Python docstring that names `resolve` in backticks, a shell snippet, a markdown span —
must be written escaped (`` \` `` and `\${`) inside a template literal, or the literal
ends early and the whole program never parses.

<!-- needs: patch -->
It is also what makes shared work safe. The TAG pins the version you read. If the
file changed underneath you but your lines were untouched, your edit is rebased
onto the new version and lands. If your lines WERE touched, you get a conflict
naming the file and the line range — someone else edited exactly there. Re-view
that file and redo the edit against the new text; never retry the same patch blind.
With workers sharing one checkout this is the only thing between two agents and a
silent clobber, so treat a conflict as information, not as a retryable hiccup.

<!-- needs: view,patch -->
Never pass view()'s output to patch(): the listing is for you to read, and its
`N:text` lines are not operations.
