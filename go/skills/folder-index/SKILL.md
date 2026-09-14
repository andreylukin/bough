---
name: folder-index
description: "Build, refresh or query a routed markdown index of a folder at <dir>/.index. Use as /folder-index build|refresh|query <dir>."
manual: true
---

# folder-index

Install once: `ln -s "$PWD/go/skills/folder-index" ~/.bough/skills/folder-index`
(from the bough checkout). `check.sh` and `bench.sh` live next to this file.

The index is read on demand only. Never inject it and never reference it from
AGENTS.md or CLAUDE.md.

## Layout

```
<dir>/.index/
  README.md            routing root: what lives here, links to topics
  topics/<topic>.md    links to themes
  themes/<theme>.md    links to leaves
  leaves/<leaf>.md     facts, each citing path:line
  manifest.tsv         path<TAB>sha256<TAB>leaf  (one row per indexed file)
```

Links are relative markdown links (`[auth](../themes/auth.md)`). Citations are
`path:line` with `path` relative to `<dir>` in backticks, e.g.
`` `pkg/auth/token.go:42` ``. Every leaf is linked from at least one theme,
every theme from a topic, every topic from README.md. Names are kebab-case.

## build <dir>

1. List files: `git -C <dir> ls-files` if a repo, else `find <dir> -type f`
   (skip `.index/`, `.git/`, binaries, vendored and generated files).
2. Read the files. Cluster into 3-10 topics, each into themes, each theme into
   leaves of one concern (a leaf covers a few related files).
3. Write leaves: short facts, each ending with a `path:line` citation you
   verified by reading that line.
4. Write themes, topics and README.md as routing pages: one line per link
   saying what is behind it.
5. Write manifest.tsv: for each indexed file,
   `printf '%s\t%s\t%s\n' "$path" "$(shasum -a 256 "<dir>/$path" | cut -d' ' -f1)" "$leaf"`.
6. Run `~/.bough/skills/folder-index/check.sh <dir>` and fix until it exits 0.

## refresh <dir>

1. For each manifest row, recompute `shasum -a 256`. Changed or deleted files
   mark their leaf stale; files not in the manifest (from step 1 of build) need
   a leaf.
2. Rewrite only the stale leaves (and new ones), then only the themes, topics
   and README.md that route to them. Drop leaves and links that no longer have
   files.
3. Rewrite the affected manifest rows. Run `check.sh` until it exits 0.

## query <dir> <question>

1. Read `<dir>/.index/README.md`, pick the topic, then the theme, then the leaf.
   Open only pages on that route.
2. Open only the cited lines (`sed -n 'A,Bp'` around each `path:line`), not
   whole files.
3. Answer with the citations. If the route dead-ends, say so and name the
   closest leaf rather than scanning the tree.
