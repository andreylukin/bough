# design-sync notes — bough control room

First sync: 2026-09-12, into Claude Design project "Bough Web"
(`49994de9-4eaf-4559-b637-00d424ce3396`). Shape: storybook.

## What this repo needed that the defaults don't cover

- `[GENERAL]` **The DS is an app, not a package.** `dist/app.js` is the
  app bundle (`src/main.tsx`, mounts to the DOM) — useless as a library
  entry. `go/internal/serve/web/ds-entry.ts` is the barrel the converter
  bundles into `window.BoughWeb`; it re-exports `src/app`, `src/projects`,
  `src/skills`, `src/status`, `src/render`. **Keep it in step with what
  `src/stories/*.stories.tsx` import** — a story importing something the
  barrel doesn't export renders an undefined component.
- `[GENERAL]` **Component discovery needs a `.d.ts` tree.** `lib/dts.mjs`
  reads `package.json`'s `types` field; with none, 0 components are found
  and every storybook title drops as `[TITLE_UNMAPPED]`. Fix: `tsconfig.ds.json`
  (declarations only, stories excluded) → `types/`, `"types":
  "types/ds-entry.d.ts"` and a `build:types` script in the web package.
  `buildCmd` runs `bun run build && bun run build:types`, so a re-sync
  regenerates them. `types/` is gitignored.
- **`titleMap`** — three storybook titles aren't export names:
  - `Transcript` → `TurnView`. The story file showcases the whole transcript
    family (TurnView, Entry, CodeBlock, ResultBlock, JobBlock); TurnView is
    the composed one.
  - `Shell` → `App`. `Shell` is defined *inside* shell.stories.tsx — it is
    `App`'s composition driven by fixtures instead of live fetch. The card
    renders the story's Shell; the `.d.ts`/`.prompt.md` describe `App`.
  - `Foundations` → `null` (excluded). primitives.stories.tsx is class markup
    — tokens, type ramp, buttons — with no component behind it. Its vocabulary
    is covered by `conventions.md` instead.
- **`cardMode: "column"`** on `App`, `StatusMark` and `Thread` —
  `[GRID_OVERFLOW] wide`, their stories are wider than a grid cell.
- **CSS**: `cfg.cssEntry` is `design/bough.css`, *package-relative*. An
  absolute-from-repo-root path silently misses and the build falls back to
  `[CSS_FROM_STORYBOOK]` (which also works — it scrapes the compiled CSS out
  of sb-reference — but then drift in `design/bough.css` goes unnoticed).
- No fonts ship: the stylesheet uses system stacks via `--sans` / `--mono`,
  so `[FONT_MISSING]` never applies here.

## Grades — first sync, 7/7 match

App, ProjectsView, Sidebar, SkillPicker, StatusMark, Thread, TurnView.
No `close`, no skips, no owned previews in `.design-sync/previews/`.
The generated previews matched storybook on the first capture.

## Re-sync risks — read before trusting a carried grade

- **`ds-entry.ts` is hand-maintained.** A new component in `src/` is invisible
  to the sync until it's exported there. A renamed or removed export breaks
  the story that imports it, and the failure shows as an undefined-component
  cell, not a build error.
- **`design/bough.css` is a *copy* of the `<style>` block in `dist/index.html`,
  not the source.** `bun run design:check` is the drift alarm; run it before a
  re-sync. A stale copy means every card is verified against the wrong styling.
- **`installFakeApi()` in `.storybook/preview.tsx` is bundled into the
  previews** (via `preview-decorators.js`) but is **not** in the shipped
  `_ds_bundle.js`. So `Controls` and `SkillPicker` show populated dropdowns in
  the cards and empty ones in a real Claude Design render. Deliberate — the
  cards show the intended look — but don't read a populated card as proof the
  component works without a server. Called out in `conventions.md`.
- **`App`'s `PhoneList` / `PhoneThread` stories are not verified at phone
  width.** The capture viewport is desktop, so both panels render the desktop
  layout identically; the `data-pane` behaviour below 720px is untested by
  this loop.
- **`SkillPicker` has one story, `Closed`** — the open popover
  (`.skills-pop`, `.skills-list`, `.skills-filter`) is never rendered by the
  sync. Its card is a lone "Skills" button.
- **`TurnView` is captured at `--max-stories 10`** (default cap is 6). A plain
  `compare.mjs` re-run drops back to 6 and the four tail stories
  (JobFailed, Error, Subagent, MarkdownReply) go ungraded.
- **Group labels**: `App` and `TurnView` land in `misc/` because their story
  titles (`Shell`, `Transcript`) have no `Group/Name` prefix. Cosmetic, but if
  the pane's grouping matters, retitle those two stories — it changes story
  ids, so both re-grade.
- **A live bough session sharing this checkout will wipe untracked sync
  artifacts.** It happened during the first sync: `ds-entry.ts`,
  `tsconfig.ds.json`, `types/`, `.ds-sync/` and `ds-bundle/` all vanished
  mid-run and the `package.json` edits reverted. `.design-sync/` survived.
  Everything durable is committed now, so a repeat costs only a rebuild —
  but check `git status` before concluding the converter broke.
- **`go/internal/serve/web/design/`** is a *separate*, hand-written card set
  with its own README pointing at `/design-sync go/internal/serve/web/design`.
  That path is now redundant: this sync generates cards from the React source
  and the storybook oracle. Decide whether `design/components/*.html` still
  earns its keep, or whether only `bough.css` should stay.

## Toolchain assumed

bun 1.x (`bun.lock`), storybook 10.6 + `@storybook/react-vite`, vite 8,
React 19.3, typescript ^7 (a declared devDependency since the first sync —
`build:types` needs it).

## Known staleness at the first sync (2026-09-12)

Another bough session was editing this working tree *during* the sync. The
reference storybook was built at 13:56; `src/app.tsx` changed at 14:05 and the
final bundle (14:12) picked that change up. Consequences:

- **`Sidebar` and `App` shipped with a "Hooks" nav item that their graded
  screenshots don't show** (`View` gained `"hooks"`, `HooksPage` is rendered
  for it). The grade is honest for the 13:56 source, not for what shipped.
- **`HooksPage` (`src/hooks.tsx`, `src/stories/hooks.stories.tsx`) is not in
  the project at all** — its story file appeared after the reference build.
- `ds-entry.ts` does not export `./hooks` yet.

Fix on the next sync, once that work has landed: add `export * from "./hooks"`
to `ds-entry.ts`, rebuild `.design-sync/sb-reference`, re-run the driver. Both
components will re-grade (their sources moved), which is correct.
