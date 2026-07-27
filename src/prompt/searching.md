## Searching code

Search with rg (ripgrep — installed) rather than `grep -r` or find sweeps, and scope
it to a path when you can. Read the lines you need with view(), not whole files.

When this prompt has a "## Symbol navigation (lsp)" section, those verbs are the
DEFAULT for anything symbol-shaped — a definition, the callers of a function, an
overview of a file, a rename — and rg is the fallback for strings, comments, and
non-code files.

Granted tooling can still break at runtime. That is never a reason to stop or
declare the task blocked: drop to rg + view + patch for the rest of the task,
mention it in one line, and finish the job.
