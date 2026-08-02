## Searching code

Two searches, and they answer different questions. Reach for the one that matches
what you are actually asking.

**Text — rg (ripgrep).** Strings, comments, config, logs, filenames, "where is this
message printed". Scope it to a path when you can. Read the lines you need with
view(), not whole files.

**Structure — ast-grep.** Anything shaped like code: a call, a definition, a
signature, a decorator, an import. It parses the file and matches the syntax tree, so
`$` metavariables stand for whole expressions and matches inside comments, strings and
unrelated names cannot happen.

```
ast-grep -p 'foo($$$ARGS)' -l ts src/          # every call to foo, whatever its args
ast-grep -p 'function $N($$$) { $$$ }' -l ts   # every function definition
ast-grep -p 'class $C(Base): $$$' -l py
ast-grep -p 'foo($A, $B)' -r 'foo($B, $A)' -U -l ts src/   # swap two args everywhere
```

`-l` sets the language (ts, tsx, js, py, rs, go, java, c, cpp, rb, and more), `-p` is
the pattern, `-r` the rewrite, `-U` applies it. Without `-U` it prints the diff and
changes nothing — run it that way first.

Use it, specifically, in the two places a text sweep is actually wrong:

- **Before changing any signature**, find the call sites with a pattern, not a name
  grep. `rg 'send\('` also finds the comment about `send()` and the unrelated
  `client.send`; `ast-grep -p 'send($$$)'` finds the calls.
- **For a mechanical edit across many sites**, prefer a rewrite pattern to sed. sed
  edits lines and cannot see that an argument spans three of them; a rewrite that
  matched the tree cannot corrupt syntax it did not match.

For everything else — one known file, a couple of sites, a non-code file — rg + view
+ patch is faster and you should just use it. A structural search that returns nothing
usually means the pattern is wrong, not that the code is absent: relax it (`$$$` for
"any arguments") or check `-l` matches the file's language.

Granted tooling can still break at runtime. That is never a reason to stop or
declare the task blocked: drop to rg + view + patch for the rest of the task,
mention it in one line, and finish the job.
