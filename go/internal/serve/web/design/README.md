# Design system bundle

The control room's components as a Claude Design design-system
project: one preview HTML per card under `components/`, each linking
`bough.css` — a verbatim copy of the `<style>` block in
`../dist/index.html`, so the cards render with the real tokens, type
ramp and component rules, not a redrawing of them.

Each preview opens with a `<!-- @dsCard group="…" -->` line, which is
what the Design System pane indexes into cards.

```sh
bun run design:check   # bough.css still matches dist/index.html
bun run design:sync    # rewrite bough.css from dist/index.html
```

Run `design:sync` after any change to the stylesheet in `index.html`;
`design:check` is the drift alarm.

## Pushing to Claude Design

Not possible from claude.ai/code: the sync needs a login that only an
interactive Claude Code session on your own machine can do. Once, in
a terminal in this repo:

```
/design-login
/design-sync go/internal/serve/web/design
```

Pick or create the design-system project when asked. The sync is
incremental — it writes and deletes only the paths that changed, so
re-run the second command after any change under `components/`.

Storybook (`bun run storybook`, see `../README.md`) shows the same
components from the React source, under the same `bough.css`. When a
component changes: rebuild the app, `design:sync`, check its story,
then update its card here — the card is hand-written markup, so it
does not follow the source on its own.

This directory is not `go:embed`-ed (`../web.go` embeds `dist/` only)
and does not ship in the binary.
