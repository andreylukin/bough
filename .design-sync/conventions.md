# Bough Web — the control room's components

The UI of `bough serve`: a session list, a transcript, a composer. Dark by
commitment — there is no light theme and no theme switch. Every colour is
painted from a token; nothing inherits from the page.

## Setup

Two rules, both load-bearing:

1. **Paint the page.** The stylesheet styles components, not the document.
   A page that does not set its own ground renders dark text on white.

   ```jsx
   <div style={{ background: "var(--bg)", color: "var(--text-1)", minHeight: "100vh" }}>
     …
   </div>
   ```

2. **The shell wants `.app`.** `Sidebar` and `Thread` are laid out by a grid
   on `.app`; used outside it they stack full-width. `data-pane="list" | "thread"`
   on the same element is the phone breakpoint — below 720px only the named
   pane is shown.

There is no provider, no context, and no theme object. Components take plain
props and nothing else.

**Components that fetch on mount.** `App` loads `/api/sessions` and
`/api/projects`; `Controls` loads `/api/models`; `SkillPicker` loads
`/api/skills`. With no server answering they render their empty state rather
than failing — fine for a mockup, but for a populated design compose
`Sidebar` + `Thread` with your own fixture data instead of reaching for `App`.

## The styling idiom

Plain CSS classes from one hand-written stylesheet, plus custom properties for
colour. No utility classes, no CSS-in-JS, no class-name props on components —
they set their own classes internally. You write classes only for the layout
glue around them.

**Colour is always a token, never a literal:**

| Token | Use |
|---|---|
| `--bg` | page ground |
| `--surface` | sidebar, transcript blocks |
| `--raised` | fields, cards |
| `--sel` | selected row |
| `--line` / `--line-strong` | decorative rule / control border |
| `--text-1` / `--text-2` / `--text-3` | primary / secondary / labels and hints |
| `--accent` | interactive, success |
| `--amber` | waiting for you |
| `--red` | failed |
| `--sans` / `--mono` | the two families |

**Class families you can reuse for your own markup:**

| Family | Members |
|---|---|
| Shell | `.app`, `.sidebar`, `.sidebar-foot`, `.thread`, `.thread-head`, `.scroll` |
| Rows | `.row`, `.row-on`, `.row-title`, `.row-meta`, `.row-line`, `.group-head` |
| Transcript | `.transcript`, `.turn`, `.turn-body`, `.prompt`, `.say`, `.say-who`, `.block`, `.block-label`, `.block-detail`, `.block-lines`, `.block-failed`, `.thinking`, `.think-body`, `.err`, `.md` |
| Composer | `.composer`, `.composer-wrap`, `.controls`, `.ctl`, `.ctl-label`, `.hint`, `.ask`, `.ask-q` |
| Controls | `.btn`, `.btn-primary`, `.field`, `.field-label`, `.link`, `.back`, `.more` |
| Projects | `.proj`, `.proj-head`, `.proj-row`, `.proj-body`, `.proj-count`, `.proj-empty`, `.proj-none`, `.proj-when`, `.proj-move`, `.proj-open` |
| Text | `.mono`, `.meta-line`, `.num`, `.status`, `.toast` |

`.mono` is the one you will reach for most: ids, branches, paths, durations
and counts are all set in the mono family at a smaller size.

## Where the truth is

- `_ds/<folder>/styles.css` and the `_ds_bundle.css` it imports — the whole
  stylesheet, ~15 KB, worth reading before styling anything.
- `components/<group>/<Name>/<Name>.prompt.md` — per-component props and usage.
- `components/<group>/<Name>/<Name>.d.ts` — the exact prop types.

Beyond the seven carded components the bundle also exports `Entry`,
`CodeBlock`, `ResultBlock`, `JobBlock`, `Controls`, `Back`, `Markdown`,
`Sprout`, `STATUS` and the transcript helpers (`groupTurns`, `codeLabel`,
`doneSummary`, `lineCount`) on `window.BoughWeb` — use them to build a
transcript out of parts rather than re-rendering one.

## A build

```jsx
const { Sidebar, Thread } = window.BoughWeb;

<div style={{ background: "var(--bg)", color: "var(--text-1)", minHeight: "100vh" }}>
  <div className="app" data-pane="thread">
    <Sidebar rows={rows} selected={id} onSelect={setId}
             query={q} onQuery={setQ} view="sessions" onView={setView}
             showArchived={false} onToggleArchived={toggle} />
    <Thread row={row} lines={lines} projects={projects} busy={false}
            onSend={send} onAnswer={answer} onInterrupt={stop}
            onArchive={archive} onRename={rename}
            onModel={setModel} onEffort={setEffort} onAssign={assign} />
  </div>
</div>
```
