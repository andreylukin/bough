# Control room UI

The web UI `bough serve` hands out. React 19, bundled by
[bun](https://bun.sh) into `dist/app.js`, which is **committed** and
compiled into the binary by `//go:embed` in `../web.go`.

```sh
bun install          # once
bun run build        # after any change under src/
```

The bundle is checked in on purpose, the same way `plugins/artifacts`
ships its vendored viewer: it keeps `go build` the only build step this
repo needs, so nobody has to install a JavaScript toolchain to compile
bough. The cost is that a stale `dist/` ships in silence — **rebuild
before committing a `src/` change.** Nothing enforces this yet.

## Talking to the API

`src/types.ts` mirrors the Go wire types by hand. Five types over eleven
endpoints did not justify a protobuf or OpenAPI toolchain here; if they
drift, [tygo](https://github.com/gzuidhof/tygo) generates them from the
Go structs without any of that machinery.

Two things about the wire that are easy to get wrong:

- **`Event.Seq` is not a history seq.** The supervisor numbers events per
  session from 1; transcript entries carry history seqs. They collide.
  Events are a change signal only — new lines come from
  `GET /api/sessions/{id}?since=<history seq>`.
- **SSE frames are deliberately unnamed.** `EventSource` has no wildcard
  listener, so a named frame is invisible to a client that did not
  register that exact name, and bough's kind vocabulary is open-ended.
  The kind rides in the JSON payload instead.

## Storybook

The components under `src/` rendered in isolation, against the same
stylesheet the app ships (`design/bough.css`, a copy of the `<style>`
in `dist/index.html` kept honest by `bun run design:check`):

```sh
bun run storybook          # http://localhost:6006
bun run build-storybook    # static site in storybook-static/ (ignored)
```

Stories live in `src/stories/`, one file per area, with sample data in
`fixtures.ts`. `Controls` and `SkillPicker` fetch `/api/models` and
`/api/skills` on mount; the fixtures answer those two routes and pass
everything else through. No addons: the point is the components under
the real CSS, not the tooling.

The Claude Design sync is generated from these same stories — see
`design/README.md`.
