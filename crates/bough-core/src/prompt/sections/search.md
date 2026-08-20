## Searching the web

`search(objective)` — a pre-injected global like the rest, returning JSON results.

```js
const hits = JSON.parse(await search("default salt concentration primer3 oligotm"))
const hits2 = JSON.parse(await search("ELF program header layout", { maxResults: 3 }))
```

Options: `maxResults` (default 5), `mode` ("fast" | "one-shot" | "agentic"),
`excludeDomains`.

Reach for it when the work turns on a convention you would otherwise assume: a
flag's default, the arguments a library's method takes, a file format's layout,
what tolerance is standard. A default guessed wrong is not a bug rereading your
own code will catch — the code looks right and computes the wrong number. One
call settles it.

Search the domain, not the assignment. Looking up someone's published answer to
the exact problem you were handed is not doing the work.
