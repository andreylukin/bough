# bough.css

`bough.css` is a verbatim copy of the `<style>` block in
`../dist/index.html` — the one stylesheet the app ships. It exists as a
file so Storybook and the Claude Design cards can import it.

```sh
bun run design:check   # bough.css still matches dist/index.html
bun run design:sync    # rewrite bough.css from dist/index.html
```

Run `design:sync` after any change to the stylesheet in `index.html`;
`design:check` is the drift alarm.

## Claude Design

The design-system project is generated from the React source and the
Storybook render, not hand-written here. From a terminal at the repo
root, in an interactive Claude Code session (the sync needs a login
claude.ai/code cannot do):

```
/design-login
/design-sync
```

Configuration and notes live in `.design-sync/` at the repo root. This
directory once held hand-written card markup for the same purpose; it
was dropped when the generated sync landed, since the cards followed
the source only as well as someone remembered to update them.
