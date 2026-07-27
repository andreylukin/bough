## Symbol navigation (lsp)

These verbs are the DEFAULT whenever the target is a symbol: lsp.overview on a file
instead of reading it whole, lsp.find to locate a symbol instead of an rg sweep,
lsp.refs for callers instead of grepping a name (and BEFORE changing any signature,
so you know every call site), lsp.show to read one definition instead of the file
around it, lsp.rename instead of hand-editing each site. They answer in symbols
rather than dumped text — far fewer tokens and no false matches.

Await each; results are plain text. A symbol is a name or a dot path like
"Gate.decide"; an ambiguous name errors with the candidates.

- lsp.find({pattern, path?}) — search symbols by name regex, optionally scoped
- lsp.overview({path}) — every symbol in a file or directory
- lsp.show({symbol, context?}) — a symbol's full definition body
- lsp.def({symbol}) — the declaration site
- lsp.refs({symbol, context?}) — all references across the workspace
- lsp.impls({symbol}) — implementations of an interface or abstract method
- lsp.calls({to}) or lsp.calls({from}) — incoming or outgoing call hierarchy
- lsp.rename({symbol, new_name}) — rename across the codebase (this edits files)

An EMPTY result is an ordinary answer, not a failure — usually the name is wrong or
the symbol lives elsewhere. Adjust the query or fall back to rg for THAT lookup, and
keep using the verbs for the next one.

Only when the BACKEND itself errors (the language server is missing or will not
start) should you drop to rg + view + patch for the rest of the task. Say so in one
line and keep working; do not retry other verbs to confirm it.

The first call in a session may take seconds — language-server startup and indexing.
