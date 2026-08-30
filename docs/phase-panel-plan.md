# The panel — plan (2026-08-30)

What Andrey asked for: "I should have pages I can open to look at connectors I have set up,
the model I am using, etc." Old bough had one `^t` panel with ten tabs; the rebuild has
`--dump-config` and nothing on screen. Decided with him: ONE panel, tabbed, interactive
toggles, as few tabs as the verbs allow. This doc is the plan, the decisions, and the
deviations; §11 carries the normative sentence.

## Shape

One new plugin row, `tui.panel` (crate `plugins/tui-panel`, package `bough-plugin-tui-panel`),
an ordinary `Slot::Aux` pane in the existing shell — no new pane kind, no new mode. Closed it
reports `aux_rows = 0` and costs nothing; open it takes the Aux band. Three tabs, a data table
in one place:

| tab | slash | shows | verbs |
|---|---|---|---|
| config | `/config` | every composed row: state glyph, plugin, disabled, which layer last wrote each field, warnings, fingerprint | `x` toggle, `⏎` expand, `R` raw dump |
| connectors | `/connectors` | MCP servers (transport, connect state, tool count, credential *reference*, error + remediation) and collector rows (cadence, next fire, last outcome) | `s` sweep now, `r` refresh tools |
| model | `/model` | sol / terra from `model.policy`, what the next interactive turn and next unattended wake resolve to per agent, adapter claims, env-key presence, last request's model + cost | `x` clear an agent's `model_override` |

Open/close: `^t` via the `tui/key` waterfall (the keymap table is closed to plugins; the
waterfall is the sanctioned global binding). Each slash command opens the panel on its tab and
focuses it. Esc closes (the shell's dismiss path routes Esc to the focused pane first). Tab
switching inside the pane: `[` / `]` and `1`/`2`/`3` — Tab itself belongs to `cycle_focus`.

Why one panel and not three panes: the user's instinct ("maybe one config page is enough?")
plus old bough's lesson — everything mergeable merged into one tab, but mcp and model stayed
separate because the verbs differ. Tabs are rows in a table; adding one later is data.

## Read paths (all established by exploration, none invented)

- **config**: `ctx.kernel().composition()` (tree, raw, provenance, layers, warnings,
  fingerprint) joined by `EntryId` with `kernel.rows_snapshot()` (state, unmet, error, uid).
  Runtime nested mounts appear in the snapshot but no config tree: shown, marked
  non-toggleable. Row `config` bodies are COLLAPSED by default (secrets discipline, see below).
  `R` prints `bough_kernel::render(&comp, Yaml)` verbatim — the page is a second *consumer* of
  `Composition`, never a second formatter of the dump (Decision D9 stands; the raw mode is the
  proof).
- **connectors**: server rows from the composed `mcp.rmcp` / `mcp.subprocess` configs, joined
  with `rows_snapshot` (a failed connect is `mcp.rmcp.<name>` in `Failed` with the rendered
  error) and the seam (`mcp.servers()`, `tools()`). Credential cells render the
  `${keychain:…}` reference text, never a resolved value; an auth failure renders the
  remediation the keychain module already wrote ("open that client once"). No Authorize
  button — there is deliberately nothing behind one (keychain.rs rules 2 and 3).
  Collector rows from the composed `collect.*` configs joined with `Scheduler::jobs()` by
  `owner`: cadence, next fire, `last.outcome` (`Ran` detail / `Pending` reasons / `Failed`).
  `s` = `fire_now(JobName)`, which is real and synchronous.
- **model**: `model-policy::choose()` is pure and public — the page re-runs it per agent for
  both `answers_andrey` cases, with `PolicyConfig` from the composed `model.policy` row and
  overrides from `ledger.agents()`. Adapters from `LlmHandle::adapters()`; "would this id
  route" from `resolve()` (no I/O). "Is the provider keyed" is honestly unknowable without a
  call (P2-D7); the page shows only whether `api_key_env`'s variable is set in the process
  env. What actually ran and cost: `request/header` + `usage/round` read by NAME from the
  ledger, exactly as `tui-status` does, so the panel and the status line cannot disagree.

Listeners (all effects of the row, registered in `apply`, never in the launcher):
`config-updated`, `config/reload` (banner text is `ConfigReload::line()`, verbatim),
`kernel/rows-unresolved`, `mcp/servers-changed`, `schedule/fired`, `ledger/step`. No waterfall
is touched.

## Write path: the `ui` patch layer

Toggles do not mutate live state. The panel writes `$BOUGH_HOME/bough.ui.patch.yml` and the
launcher's existing watch recomposes; the page then renders whatever actually happened,
including a rejection (`config rejected, last good tree still running: …`). One mechanism for
human edits and panel edits alike; `--dump-config` stays truthful by construction.

- The file only ever contains `entries: { <row-id>: { disabled: <bool> } }`. Never `config:`,
  never `plugin:`, never `remove:` — `disabled` keeps the fiber (re-enable is a Load with the
  same uid), and a disabled-only file cannot leak or pin anything else.
- Returning a row to its default REMOVES the entry (the layer stays a diff, and "reset
  everything" is deleting the file); ids absent from the composed tree are pruned on the next
  write, so stale entries do not accumulate `AbsentRowId` warnings.
- Write-then-rename, never truncate-then-write.
- Layer order — DEVIATION from the exploration's suggestion: `ui` stacks after `user` and
  BEFORE `--patch` overlays. An explicit per-invocation flag outranks a persisted preference,
  and the `scripts/tui/` fixtures that mount unbundled rows by `--patch` must keep working on
  a machine whose panel once toggled something. The provenance column is the mystery-killer
  in both directions: whoever loses sees which layer won.
- Disabling a row other rows require is NOT rejected: dependents park in `Pending` (swap.rs:212
  semantics). The page listens to `kernel/rows-unresolved` and shows the parked rows rather
  than pre-computing a dependency warning it would have to keep correct.

Launcher touches (the only three, per "the launcher composes and tears down and does nothing
else"): `bough_util::ui_patch_path()`, one push in `plan_layers` (`LayerId::new("ui")`, absent
file skipped silently), and the watch filter widened to the two file names built against the
canonicalised dir. Everything else — the write, the prune, the rendering — lives in the plugin.

## Secrets, first

`render()` redacts nothing, and `collect.linear`'s `api_key: !!expr env_or("LINEAR_API_KEY", "")`
means today's `--dump-config` prints a plaintext key. Landing order:

1. **WP0 (own commit, before any panel code):** a redaction pass in `render.rs` — the ONE
   formatter, so the dump and every future consumer get it — masking values whose field name
   matches `api_key`, `token`, `secret`, `password`, `*_key`, with the two carve-outs the
   shipped bundles force: `*_key_env` (a variable NAME) and `*_tokens` (numbers: budget_tokens,
   max_tokens, …). Raw expressions stay visible; resolved secret values render `«redacted»`.
   dump_config.rs and swap.rs:178 adjusted.
2. The panel additionally collapses `config` bodies by default; expanding is a keypress.

## Work packages

| WP | what | verify |
|---|---|---|
| 0 | redaction in `render.rs` | new render tests; dump tests still byte-equal live render |
| 1 | `ui_patch_path` + `plan_layers` + watch filter | layer-order test (`ui` after `user`, before `patch:0:*`); absent-file test; watch picks up the second file |
| 2 | spec: §0.5 layer chain + §11 panel bullet; this doc | reads clean against the code |
| 3 | `tui-panel` crate, read-only: pane, tabs, commands, listeners, key_hints | crate tests (pure line builders, tui-focus style); `make gate-crate` |
| 4 | seam additions: `McpHandle::is_ready(&ServerName)`; nothing else | mcp crate test |
| 5 | toggle write-back (patch store module in the plugin) | store tests: disabled-only, prune, remove-on-default, rename-not-truncate |
| 6 | bundle row in `bough-tui-app.yml`, `scripts/tui/37-panel.sh`, swap test, invariant | `make tui-test`; swap test per AGENTS.md; `make gates` |

Deferred, deliberately:
- **sol/terra editing from the page.** It needs whole-config restatement of `model.policy`
  (prices included), which pins the row against future bundle changes — exactly the problem
  remove-on-default avoids for toggles. The page shows the values and where they came from;
  changing them is an `$EDITOR` edit of the user patch, which the watch applies live anyway.
- **SweepReport surfacing.** The per-source delivered/skipped/watermark detail is stranded
  inside the collectors (no service, no scheduler accessor). `JobOutcome` already carries the
  compressed form ("N delivered from M sources" / the disabled reasons). Exposing the full
  report is a collector seam change that deserves its own package.
- **Watermark display/reset.** Read would mean opening `collect-*.db` directly; reset does not
  exist anywhere. Neither belongs in this package.
- **The composition fingerprint on the strip line.** `pane.rs:51`'s doc comment claims it and
  nothing renders it; the panel's config tab now shows the fingerprint, and the stale clause
  in that comment is corrected as part of WP3. Putting it on the strip stays open.

## Invariant

`tui-panel/src/invariant.rs`: every document the panel has ever written to the ui layer parses
as a `Patch` whose every `EntryPatch` sets `disabled` and nothing else, and whose every id was
in the composed tree at write time. Checked on each write (the panel owns this relation; nobody
else writes the file).
