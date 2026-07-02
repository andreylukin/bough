# Packaging bough-next as a desktop app

**Spike verdict: `deno desktop` works for this app.** Build succeeds and produces a
codesigned native bundle; no fallback needed. The browser path stays available as a
zero-risk alternative.

## How it fits our architecture

One Deno process serves both the API and the built web UI (`web/dist`) from the same
origin — see `src/server/static.ts` (static files + SPA fallback) and `src/server/app.ts`
(API routes first, then the SPA). A `deno desktop` entrypoint just needs to start that
server; the OS webview (WebKit on macOS via the bundled `laufey` backend) points at it.

Entrypoint: `src/desktop/main.ts`. It's the normal server bootstrap minus the fixed port
— in a desktop entrypoint `Deno.serve()` with no address auto-binds to the port the
webview opens and loads `/`.

## Commands (`deno.json` tasks)

- `deno task desktop` — run the app in a native window (dev; serves `web/dist` from disk).
- `deno task desktop:build` — compile to `bough.app`. Note the **`--include web/dist`**
  flag: it embeds the SPA into the binary so the packaged app is self-contained.

Prerequisite: build the web UI first (`cd web && npm run build`) so `web/dist` exists.

## The one gotcha: embed the SPA

The compiled binary has no source tree next to it, so `web/dist` **must** be embedded or
the app serves `{"error":"web build not found"}` at `/`. `--include web/dist` handles
this; `src/server/static.ts` resolves the dir relative to its own module URL
(`../../web/dist`), which Deno maps to the embedded copy inside the binary.

Verified headlessly with `deno compile --include web/dist … src/desktop/main.ts`: the
resulting binary, run from an unrelated cwd, serves `GET /` as the embedded SPA (200
`text/html`) and `GET /sessions` as JSON — no source tree present.

## Status / follow-ups (for task #6 final packaging)

- `deno desktop` is **experimental** in Deno 2.9. If it regresses, the fallback is
  `deno task dev` + open `http://localhost:4321` in a browser — the exact same handler.
- Add an app icon: `--icon <file.icns|.png>` (macOS). Not wired yet.
- `--output` extension picks the format: `.app`/`.dmg` (macOS), `.AppImage`/`.deb`/`.rpm`
  (Linux), `.msi` (Windows). Permissions are baked in at compile time from the `--allow-*`
  flags on the build command (mirror the `dev` task's set).
