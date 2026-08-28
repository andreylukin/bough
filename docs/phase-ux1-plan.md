# Phase ux1 — UX fixes round 1 (the persona audit): design and work breakdown

**Input.** `docs/ux-audit-1.md` — 39 deduped findings (8 blockers, 20 majors, 6 minors, 5 nits) from a
10-persona shell-use walk of the Phase 4 release binary, with SVG captures in `docs/ux-audit-1-shots/`.
Both are untracked in the worktree; the scaffold commit of this phase commits them, so the findings this
plan cites are in the tree that fixes them.

**Scope.** Every blocker (1–8), every major (9–28), minors 29–33; nits 34–39 as time allows (34 and 39
fall out of WP-3 for free, 36 out of WP-5, 37 out of the vocabulary sweep). Nothing in REQUIREMENTS §17
Phase 6/7 is opened. No new product capability is added: this phase makes the surface tell the truth
about the engine that is already there.

**The stance.** The audit's 39 findings are not 39 bugs. They are four root causes and their shadows:

1. **There is no focus model.** Clicking the transcript silently moves keyboard focus off an
   always-drawn composer (B1); the same invisible state gates scrolling, which is why the walks
   contradict each other about whether the wheel or PageUp works (B2, M23); and there is no keyboard
   path to a tool row at all (B6). One model fixes all three: **one always-live composer, a roving
   VISIBLE row focus inside the transcript, and a visible ring on the pane that has the keyboard.**
2. **Typed text is destroyed.** Four separate paths (B3 slash lines, B4 raw multi-line paste, Esc on a
   draft, M20's Ctrl+U). The rule is absolute: **nothing the user typed is deleted by anything except
   an explicit clear.**
3. **The frame paints over the content, and says nothing.** Rail and strip share baselines with the
   transcript (M9), the rail is 34 columns at 80 and at 200 (M13), the strip carries no model, cwd,
   cost or context (M24), and the least legible text in the product is the help and every error (M22).
4. **The stream is rendered per network chunk and the chunk boundaries are durable** (M10, M19): the
   fix is to accumulate and wrap **on paint**, and to parse markdown over the whole accumulated
   document, never per chunk.

Everything else — search over raw ledger JSON (M11), no palette (M17), no key list (M18), invisible
patch reload (M15), the four no-op commands (M27), the wrong cwd (B5), the invented capabilities (M25)
— is one of those four wearing a different hat, or a surface that was never finished.

**Protect the delights.** §4 of the audit lists sixteen things nine or ten personas praised by name:
the tool-row disclosure, the unified diff, glyph+word status (never colour alone), message queueing,
history restore, Ctrl+C as a safe stop, bracketed paste, the fail-safe config watcher, resize safety,
turn boundaries, the palette's colour *choices*, and the honest `unknown command` wording. Every one of
them is load-bearing in a test already; none of them is rewritten by this phase.

**Two neighbours.** Phase 5 shipped `tui-focus` (claims cards, the branch picker, the chunk-join of
`thought/text` parts) and `tui-strip` (many agents) — this phase changes both and keeps their tests
green. A parallel branch `rebuild-b` adds a `tui-drafts` pane into a `tui-shell` slot; **§2.12 freezes
the slot API** for this phase and records the one additive change.

---

## 1. Crates

No crate is retired. One crate is added, because the SWAP gate requires the new status line to be a row
a patch can disable on its own.

| Package | Row id(s) | `inject` | `provide` | What this phase changes |
|---|---|---|---|---|
| `bough-plugin-tui-shell` | `tui` | `agents`, `ledger`, `commands` (req); `workspace` (opt) | `tui` | The focus model, focus-independent scroll routing, the interrupt/exit machine, the draft, overlay drawing and dismissal, the theme's contrast, the layout gutter |
| `bough-plugin-tui-focus` | `tui.focus` | `tui`, `ledger`, `agents` | — | Roving row focus, `Viewport` (follow + `↓ N new`), paint-time wrap, markdown over the joined document |
| `bough-plugin-tui-strip` | `tui.strip` | `tui`, `agents`, `ledger` | — | Rail breakpoint + collapse, hard clipping at the gutter, turn/message vocabulary |
| `bough-plugin-tui-status` **(new)** | `tui.status` | `tui`, `ledger` (req); `agents`, `workspace` (opt) | — | The whole crate: the `Slot::Status` row with name, cwd, model, %context, cost, spinner, key hints — and the SWAP subject |
| `bough-plugin-tui-search` | `tui.search` | `tui`, `ledger`, `agents` | — | Index rendered conversation text, snippet + highlight + `n of N`, real jump-to-step, Esc clears |
| `bough-plugin-tui-render` | — (library row-less) | — | — | `md.rs`: a block parser over the accumulated document; hanging indent; tables; the prose measure |
| `bough-plugin-commands` | `commands` | — | `commands` | The `/` palette (pure filter + keymap), did-you-mean on a miss, the send-as-message arm |
| `bough-plugin-tools` | `tools` | — | `tools`, **`workspace`** (Definition only) | Declares the `Workspace` service key and its vocabulary |
| `bough-plugin-tools-baseline` | `tools.baseline` | `tools` | **`workspace`** (Provider) | Pins the process cwd ONCE at activation; every path resolves against the pinned root |
| `bough-plugin-tool-actions` | `tool.actions` | `tools`, `actions` | — | Registers a tool only for an action kind that has a live Provider |
| `bough-plugin-projection-assembler` | `projection.assembler` | `ledger`, `projection`, **`tools`** | `projection` (Provider) | The identity band names the tools actually registered in this agent's scope |
| `bough-plugin-model-policy` | `model.policy` | `ledger` (new) | — | The price table, and the `usage/round` step it appends from `llm/stream` |
| `bough-plugin-llm` | `llm` | — | `llm` | Declares the `usage/round` step type (it owns model-call vocabulary) |
| `bough-plugin-ledger-sqlite` | `ledger.sqlite` | `ledger` | `ledger` (Provider) | `wal_checkpoint(TRUNCATE)` on disposal, so a relaunch always sees the whole ledger |
| `bough-plugin-residents` | `residents` | `agents`, `ledger` | — | The about-line is one clean sentence, markdown stripped, and it survives relaunch |
| `bough-plugin-old-feed-adapter`, `bough-plugin-drift-watch`, `bough-plugin-dormancy` | — | unchanged | — | Their commands render output or the reason they cannot (M27) |
| `bough` (launcher) | — | — | — | Bounded teardown, the farewell line, and the patch-reload notice reaching the screen |

`bough-plugin-tui-status` is a genuine row and not a pane inside `tui-strip` for one reason: the SWAP
bullet of this phase is "the new status-line row disabled by patch while the TUI runs disappears and the
layout reflows". A pane that is a field of another row cannot be disabled by a patch, and a swap test
that cannot fail proves nothing.

---

## 2. Public API

Everything below is what independent agents implement against. A signature here is a contract; a
`PURE` marker means no I/O, no clock, no lock — the function is unit-testable on its own.

### 2.1 The focus model — `plugins/tui-shell/src/keymap.rs` (new), `run.rs`, `lib.rs`

The rule, in one sentence: **the composer always has the keyboard unless the user deliberately gave it
away, and any printable key takes it back.**

```rust
/// Where the keyboard is. Exactly one of these is true at any moment, and the frame SHOWS which.
#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    /// The default, and where every session starts and returns to.
    Composer,
    /// A pane took the keyboard by Tab, Ctrl+F, or a deliberate click on a focusable pane.
    /// `row` is the pane's own roving row, opaque to the shell.
    Pane { pane: PaneId },
}

/// What the shell does with one key, decided BEFORE anything is dispatched. PURE and total.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Scroll the transcript, whatever has focus (V2). `to_latest` is `End`.
    Scroll { delta: i16 },
    JumpLatest,
    /// Tab / BackTab: one step around the focus ring.
    CycleFocus(i32),
    /// Esc while a wake is running.
    Interrupt,
    /// Esc with an overlay open: dismiss the topmost one.
    DismissOverlay,
    /// Ctrl+C: arm, or exit if already armed.
    ExitStep,
    FocusSearch,
    Redraw,
    /// Not the shell's: goes to the composer (Focus::Composer) or the focused pane.
    Pass,
}

/// Everything `action_for` needs. No handle, no lock: the caller reads the shell once.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    pub focus_is_composer: bool,
    pub draft_is_empty: bool,
    pub running: bool,
    pub overlay_open: bool,
    pub palette_open: bool,
    pub exit_armed: bool,
}

/// PURE: the whole keymap, as a function. `page` is `TuiConfig::page_lines`.
pub fn action_for(key: KeyEvent, cx: KeyContext, page: u16) -> Action;

/// PURE: does this key take the keyboard back to the composer? A printable character with no
/// CONTROL and no ALT — and nothing else (B1: "any printable key snaps focus back").
pub fn snaps_to_composer(key: &KeyEvent) -> bool;

/// The two-press exit (B7). `window` comes from `TuiConfig::exit_arm_ms`.
pub struct ExitArm { armed_at: Option<DateTime<Utc>>, window: Duration }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitStep { Arm, Exit }

impl ExitArm {
    pub fn new(window: Duration) -> ExitArm;
    /// PURE in `now`: first press arms and returns `Arm`; a second inside the window returns
    /// `Exit`; a press after the window re-arms.
    pub fn press(&mut self, now: DateTime<Utc>) -> ExitStep;
    pub fn is_armed(&self, now: DateTime<Utc>) -> bool;
    pub fn disarm(&mut self);
}
```

Fixed bindings after this phase (and `/help` prints exactly this list, generated from it):

| Key | Meaning | Finding |
|---|---|---|
| `Enter` | send, or run a `/` command | — |
| `Shift+Enter`, `Alt+Enter` | newline in the draft | M20 |
| `Esc` (running) | interrupt the turn | M14 |
| `Esc` (overlay open) | dismiss the topmost overlay | M12 |
| `Esc` (otherwise) | **nothing** — the draft is never destroyed | B3/M14 |
| `Ctrl+U` | clear the line | M20 |
| `↑`/`↓` (composer, empty draft) | sent-message history | M20 |
| `↑`/`↓` (pane focused) | move the roving row focus | B6 |
| `Enter`/`Space` (pane focused) | toggle the focused row's disclosure | B6 |
| `PageUp`/`PageDown`/`Home`/`End` | scroll the transcript, **whatever has focus** | B2 |
| wheel | scroll the pane under the pointer, focus untouched | M23 |
| `Tab`/`Shift+Tab` | cycle the focus ring (visible) | B1 |
| `Ctrl+F` | search | M18 |
| `Ctrl+C` | interrupt if running; else arm, then exit | B7 |
| `Ctrl+L` | redraw | — |
| `?` (empty draft) | `/help` | M16 |

`TuiHandle` additions (WP-1 lands these first; WP-4 and WP-6 read them):

```rust
impl TuiHandle {
    /// The pane the focus-independent scroll keys drive: `TuiConfig::transcript_pane`, matched
    /// EXACTLY (the same lesson `search_pane` already carries).
    pub fn transcript_pane(&self) -> Option<PaneId>;
    /// Whether the focused agent has a wake open right now. The status line's spinner and the
    /// `esc to interrupt` hint read this; `keymap` reads it to decide what Esc means.
    pub fn running(&self) -> bool;
    /// When the running wake started, for the elapsed clock (M32).
    pub fn running_since(&self) -> Option<DateTime<Utc>>;
    /// `Ctrl+C` has been pressed once and the window has not lapsed.
    pub fn exit_armed(&self) -> bool;
    /// A transient notice with a ROLE, so the theme can colour an error like an error (M22).
    pub fn notify_kind(&self, text: impl Into<String>, kind: NoticeKind);
    pub fn notice_now(&self, now: DateTime<Utc>) -> Option<Notice>;
    /// The one-line farewell printed AFTER the terminal is restored (B8).
    pub fn quit_with(&self, code: u8, farewell: impl Into<String>);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice { pub text: String, pub kind: NoticeKind, pub at: DateTime<Utc>, pub ttl: Option<Duration> }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoticeKind { Info, Error, Config, Copied }
```

`plugins/tui-focus/src/rowfocus.rs` (new) — the roving focus, PURE:

```rust
/// The roving row focus inside a transcript pane. `None` = no row focused, which is what a pane
/// that has never had the keyboard renders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowFocus { pub index: Option<usize> }

impl RowFocus {
    /// PURE: move by `delta` over `rows`, clamping. From `None`, a down-move lands on the LAST
    /// row (the newest) and an up-move on the last row too: a keyboard user arriving from the
    /// composer is at the bottom of the conversation, not the top.
    pub fn moved(self, delta: i32, rows: usize) -> RowFocus;
    /// The row a `FocusRequest { step }` names, so a search hit and the keyboard agree.
    pub fn on_step(rows: &[Row], step: &StepId) -> RowFocus;
    /// PURE: whether this row index should paint the focus indicator this frame.
    pub fn is_on(&self, index: usize) -> bool;
}

/// PURE: the indicator a focused row carries. Never colour alone (audit delight 3): a marker
/// glyph in the gutter column AND a `sel_bg` fill.
pub fn focus_marker() -> char; // '▌'
```

`Pane::render` already receives `view.is_focused`; the pane draws a one-column ring/edge when it is
true. That is the "Tab shows a focus ring" half of V1, and it needs no trait change.

### 2.2 Scroll and follow — `plugins/tui-focus/src/scroll.rs`

`Scroll` (Phase 3) keeps its shape and its semantics. It gains a wrapper that owns the unread count,
because "auto-follow at the tail" and "`↓ N new` when detached" are the same state machine.

```rust
/// Where the transcript is looking, and how much it has not shown. One per transcript pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Viewport { pub scroll: Scroll, pub unseen: usize }

impl Viewport {
    /// PURE: rows were appended. Following ⇒ nothing to count; anchored ⇒ `unseen += added`.
    pub fn on_rows_appended(&mut self, added: usize);
    /// PURE: a scroll input. Landing at the bottom re-arms `Follow` and zeroes `unseen`.
    pub fn scrolled(&mut self, delta: i32, rows: usize, height: u16);
    /// `End`, and what sending a message does: back to the tail, `unseen = 0` (B2).
    pub fn to_latest(&mut self);
    /// Anchor on a row (a search hit, a `FocusRequest { step }`).
    pub fn anchor_on(&mut self, row: usize);
    /// PURE: the affordance text, or `None` while following. `"↓ 3 new"`.
    pub fn badge(&self) -> Option<String>;
    pub fn top(&self, rows: usize, height: u16) -> usize;
    pub fn is_following(&self) -> bool;
}
```

Two shell-side rules make the keys work regardless of focus (B2, M23):

* `action_for` maps `PageUp/PageDown/Home/End` to `Action::Scroll`/`JumpLatest` in **every**
  `KeyContext`, and `run::on_key` routes them to `tui.transcript_pane()`, not to the focused pane.
  `Home`/`End` inside the composer still move the caret when the draft is non-empty and multi-line —
  that exception is spelled in `action_for` and tested, because it is the only one.
* `on_mouse`'s `ScrollUp`/`ScrollDown` arm keeps routing to the pane under the pointer without
  touching focus, and falls back to the transcript pane when the pointer is over the composer or a
  zero-size slot.

### 2.3 The draft — `plugins/tui-shell/src/draft.rs` (new), `composer.rs`

```rust
/// What Enter can mean. `Hint` is new: the line looked like a command and no command matched, so
/// the TEXT STAYS and the shell shows why (B3).
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerAction {
    None,
    Send(String),
    /// The buffer is NOT cleared: the shell clears it only when `ctx.commands` resolved the name.
    Command(String),
    /// Ctrl+U on a non-empty line. Esc no longer produces this (V3).
    Cleared,
    Newline,
}

impl Composer {
    /// Enter on a command line no longer clears; `clear()` is the shell's to call on success.
    pub fn clear(&mut self);
    /// A second Enter on an unchanged missed-command line sends it as a message. Any edit
    /// disarms it. This is the "send it as a message" path (B3).
    pub fn arm_send_as_message(&mut self);
    pub fn send_as_message_armed(&self) -> bool;
    /// Map a click's column/row to a caret offset (minor 33).
    pub fn caret_at(&mut self, col: u16, row: u16, area: Rect);
    /// The placeholder, now a sentence: "Type a message, or / for a command".
    pub fn placeholder() -> &'static str;
}

/// A newline burst that is really a paste (B4). A terminal that does not advertise bracketed
/// paste delivers a paste as N key events in microseconds; a human cannot type two newlines
/// `burst_ms` apart.
pub struct PasteBurst { window: Duration, last_key: Option<DateTime<Utc>>, }

impl PasteBurst {
    pub fn new(window: Duration) -> PasteBurst;
    /// PURE in `now`: record a key. Returns whether the Enter that just arrived is part of a
    /// burst and must be treated as a NEWLINE rather than a send.
    pub fn on_key(&mut self, now: DateTime<Utc>) -> bool;
    pub fn reset(&mut self);
}

/// Sent-message recall (M20). Bounded, deduped against the immediately previous entry.
pub struct SentHistory { items: VecDeque<String>, cursor: Option<usize>, held: Option<String>, cap: usize }

impl SentHistory {
    pub fn new(cap: usize) -> SentHistory;
    pub fn push(&mut self, text: &str);
    /// Up: the previous entry, holding the live draft so Down restores it.
    pub fn prev(&mut self, draft: &str) -> Option<String>;
    pub fn next(&mut self) -> Option<String>;
    pub fn reset(&mut self);
}

/// PURE: readline's kill-to-line-start. `Ctrl+U` deleted ONE character in 8 of 10 walks.
pub fn kill_to_line_start(line: &str, caret: usize) -> (String, usize);
```

Sequencing rule, stated once so WP-1 and WP-2 cannot disagree: `run::on_key` calls
`PasteBurst::on_key(now)` **before** handing the key to the composer, and passes the burst flag in;
`Composer::on_key(key, in_burst)` turns Enter into a newline when the flag is set. Bracketed paste
(`Event::Paste`) keeps its existing path — it already works and six personas called it a delight.

### 2.4 Interrupt and exit — `plugins/tui-shell/src/run.rs`, `term.rs`, `crates/bough/src/boot.rs`

* `Action::Interrupt` → `agent.cancel(CancelCause::User, true)` and a `NoticeKind::Info` notice
  `interrupted`. The durable `wake/end { reason: interrupted }` already renders as a turn marker; WP-3
  makes that marker read `— turn interrupted` in the transcript, at body contrast, where the user is
  looking (B7's "no visible confirmation").
* While a wake is open, the status line carries `esc to interrupt` (M14) — the only place the stop key
  has ever been named.
* `Ctrl+C` while running interrupts (unchanged: audit delight 6). `Ctrl+C` while idle arms:
  `press Ctrl+C again to exit`. A second inside `exit_arm_ms` calls `quit_with(0, farewell)`.
* `quit_with` stores the farewell in `term`, then asks the kernel to exit. `TerminalGuard`'s drop
  leaves the alt screen and **then** prints the farewell to the real screen, so `/quit` can never leave
  a black rectangle (B8):

```rust
// plugins/tui-shell/src/term.rs
/// The line printed after the terminal is restored, once, by whichever restore path runs first.
pub fn set_farewell(text: String);
pub fn take_farewell() -> Option<String>;
```

* Bounded teardown (B8's hang, and M28's uncheckpointed WAL downstream of it):

```rust
// crates/bough/src/boot.rs
/// Await `kernel.shutdown()` under a deadline. On timeout: restore the terminal, print
/// `bough: shutdown timed out after {ms}ms; leaving anyway` to stderr, and exit with `code`.
/// The deadline is `Cli::shutdown_ms` (default 2000), never a constant at the call site.
pub async fn shutdown_bounded(kernel: &Kernel, ms: u64, code: u8) -> ExitOutcome;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitOutcome { Clean, TimedOut }
```

### 2.5 The frame — `plugins/tui-status/src/…` (new), `tui-strip`, `tui-shell/src/pane.rs`, `contrast.rs`

```rust
// plugins/tui-status/src/lib.rs
pub const PLUGIN_NAME: &str = "tui-status";
pub const PANE_ID: &str = "tui.status";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusConfig {
    /// Longest cwd rendered before the middle is elided.
    pub cwd_max: u16,
    /// Spinner frames, as one string. Deployment-varying (a terminal without a good font).
    pub spinner: String,
    pub spinner_ms: u64,
    /// Key hints, in order, as `"key=meaning"` pairs. The hint list is config, not a constant,
    /// because it is the one chrome a user might want shortened.
    pub hints: Vec<String>,
}

/// Everything the line can show. Assembled by the row's listeners; `render` queries nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusView {
    pub product: String,               // "bough 0.x"
    pub cwd: Option<PathBuf>,          // from `ctx.workspace`, NOT from `std::env`
    pub model: Option<String>,         // last `request/header.call.model`
    pub context_left: Option<u8>,      // 100 - 100*projection_tokens/budget
    pub cost_usd: Option<f64>,         // Σ `usage/round.cost_usd` for this home
    pub running: bool,
    pub elapsed: Option<Duration>,
    pub spinner_frame: char,
    pub hints: Vec<(String, String)>,
}

/// PURE: the fields that survive at `width`, in drop order. Nothing overflows, nothing wraps —
/// the status line is exactly one row (M9).
pub fn fields(v: &StatusView, width: u16) -> Vec<Field>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Field { Product, Cwd, Model, Context, Cost, Elapsed, Hints }

/// PURE: `(view, width, theme) -> Line`. Every span names a theme ROLE.
pub fn status_line(v: &StatusView, width: u16, theme: &Theme) -> Line<'static>;

/// PURE: a path elided in the MIDDLE (`~/repos/bou…/ux/cwd`), never at the end — the last
/// component is the one a user checks (B5).
pub fn elide_path(p: &Path, home: Option<&Path>, max: u16) -> String;
```

Rail (`plugins/tui-strip`):

```rust
/// StripConfig gains four fields, all validated:
///   collapse_cols: u16   // below this total width the rail takes ZERO columns (default 100)
///   min_width: u16       // the rail never renders narrower than this when it renders at all
///   max_width: u16       // and never wider
///   gutter: u16          // blank columns between the rail and the transcript (default 1)

/// PURE: the rail's column count at a terminal width. `0` below `collapse_cols` (M13).
pub fn rail_width(total: u16, cfg: &StripConfig) -> u16;

/// PURE: every rail line is HARD-CLIPPED to `width - gutter`, with a `…` when it clipped. The
/// audit's `idlePlease` and `running──` were two text runs on one baseline; a clip that cannot
/// overflow is what makes that impossible rather than unlikely.
pub fn clip(line: Line<'static>, width: u16) -> Line<'static>;
```

Layout (`plugins/tui-shell/src/pane.rs`) — the ONE change, and it is additive:

```rust
/// `layout` gains a `gutter: u16` parameter: the Strip slot takes `width + gutter` columns and
/// hands the pane only `width`, so the blank column belongs to nobody and cannot be painted.
pub fn layout(size: Rect, panes: &[PaneInfo], composer_height: u16, gutter: u16) -> Vec<(PaneId, Rect)>;

/// PURE: the prose measure. `min(width, cap)`; `cap` is `TuiConfig::measure_cols` (default 90).
/// A 200-column terminal gets a 90-column paragraph and the rest is margin (M13).
pub fn measure(width: u16, cap: u16) -> u16;
```

Contrast (`plugins/tui-shell/src/contrast.rs`, new, PURE):

```rust
/// WCAG relative luminance of an sRGB colour. `Color::Reset` resolves to `Theme::measure_bg`.
pub fn luminance(c: Color, measure_bg: Color) -> f64;
/// WCAG 2.1 contrast ratio, 1.0..=21.0.
pub fn ratio(fg: Color, bg: Color, measure_bg: Color) -> f64;
/// Every foreground role of a theme against its background, by name. The V9 test reads this.
pub fn audit(theme: &Theme) -> Vec<(&'static str, f64)>;
```

`Theme` gains `measure_bg: Color` (the terminal background the palette was designed for — needed
because `bg` is `Color::Reset` and a ratio against "whatever the user's terminal is" is not a number),
and three roles move: `hint` from `#565f89` (2.1:1) to body contrast, `dim` from `#707680` (3.0:1) to
≥4.5:1, and errors render in `error`/`warn` with a `!` glyph instead of `hint`. The blue/purple/green
accents the audit measured at 5.5–7.6:1 do not move (delight 11).

### 2.6 Streaming and markdown — `plugins/tui-render/src/md.rs` (new), `tui-focus/src/rows.rs`

The invariant this phase adds, and the only one that matters here:

> **No wrapped line is ever stored.** The ledger holds the text; `Row` holds the text; wrapping and
> markdown happen in `render`, against the width of the frame being painted. A chunk boundary
> therefore cannot survive a repaint, a resize, or a relaunch (M10, M13's re-wrap, nit 39).

```rust
/// One block of an accumulated markdown document. PURE parse over the WHOLE string, never per
/// chunk — which is what made `**Code & File` / ` Operations:**` render its asterisks (M19).
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, text: String },
    Para(String),
    Item { level: u8, marker: String, text: String },
    Code { lang: Option<String>, body: String },
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
    Quote(String),
    Rule,
}

/// PURE and TOTAL: any string is a document. Unterminated fences, half-written tables and a
/// heading with no blank line after it all parse — the parser runs on a LIVE tail.
pub fn blocks(doc: &str) -> Vec<Block>;

/// PURE: blocks to styled lines at `width`. Items hang-indent to the text after the marker
/// (nit 34); tables lay out to their widest cell and scroll-clip, never wrap a cell; code goes
/// through `highlight`.
pub fn render(blocks: &[Block], width: u16, theme: &Theme) -> Vec<Line<'static>>;

/// The whole path in one call, which is what the pane uses.
pub fn document(doc: &str, width: u16, theme: &Theme) -> Vec<Line<'static>>;
```

`text::markdownish` is kept as a thin shim over `md::document` for one release so `tui-strip` and the
tool-result renderers do not all change at once; the shim is deleted in WP-3's last commit.

`rows.rs`: the Phase 5 chunk-join (`parts: Vec<StepId>`, raw concatenation) is already right and stays.
Two changes: `Row::Text::lines(width, theme)` calls `md::document` instead of per-step rendering, and
`Row::WakeMark` renders the **turn** vocabulary (`── turn`, `── turn ended · completed`,
`── turn interrupted`) at body contrast rather than `dim` (M22, nit 37). The `wake/*` step type names
are untouched — REQUIREMENTS §3's map does not move.

### 2.7 Search — `plugins/tui-search/src/index.rs` (new), `lib.rs`

```rust
/// One searchable unit: a RENDERED row of the conversation, not a ledger record (M11).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub step: StepId,
    pub agent: AgentName,
    /// "andrey", "sol", "write_file", "turn" — what the row shows as its speaker.
    pub speaker: String,
    /// The row's rendered text, markdown markers stripped, one line.
    pub text: String,
}

/// PURE: the rows of a trajectory as search entries. `request/header` and the other ENVELOPE
/// types produce NO entry — they are what the audit saw as `{"as_of":53,"budget":96000,…`.
pub fn entries(rows: &[Row]) -> Vec<Entry>;

/// One hit, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub step: StepId,
    pub speaker: String,
    /// The snippet, already clipped around the match.
    pub snippet: String,
    /// Byte range of the match INSIDE `snippet`, for the highlight span.
    pub at: Range<usize>,
}

/// PURE: case-insensitive substring search with a `radius`-character window around the match.
/// Substring, deliberately: the audit found `THREE` matching "5-step mechanism" through FTS
/// stemming and nobody could tell why.
pub fn search(entries: &[Entry], query: &str, radius: usize) -> Vec<Hit>;

/// PURE: `"3 of 17"`, or `"no matches"`, or `""` for an empty query.
pub fn counter(selected: usize, total: usize) -> String;

/// PURE: the pane's lines, with the match span carrying `Theme::sel_bg`.
pub fn lines(hits: &[Hit], selected: usize, query: &str, width: u16, theme: &Theme)
    -> Vec<(Line<'static>, Option<HitId>)>;
```

`SearchState` gains `n`/`N` (next/previous match, wrapping) and `Esc` clears the query, the hits and
the pane's rows in one call (`clear()`), which is minor 30's "never clears". Enter/click emits
`PaneOutcome::Focus(FocusRequest { step: Some(..), .. })` — the shell already routes that; WP-3's
`Viewport::anchor_on` is what makes the transcript actually move, and WP-1's `RowFocus::on_step`
is what makes the landed row visibly the focused one (delight 16 says the jump already works; this
makes it land somewhere the user can see).

### 2.8 Commands — `plugins/commands/src/palette.rs` (new), `parse.rs`, `tui-shell/src/builtins.rs`

```rust
/// The `/` palette. State only — the shell owns when it opens (a `/` at line start) and closes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Palette { pub open: bool, pub query: String, pub selected: usize }

#[derive(Clone, Debug, PartialEq)]
pub struct Item { pub name: CommandName, pub usage: String, pub summary: String }

/// PURE: prefix matches first, then substring, each group alphabetical. Stable, so the selection
/// does not jump under the user as they type.
pub fn filter(all: &[CommandInfo], query: &str) -> Vec<Item>;

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction { None, Moved, Accept(CommandName), Close }

/// PURE: Up/Down move, Tab and Enter accept, Esc closes, anything else falls through.
pub fn on_key(p: &mut Palette, key: KeyEvent, n: usize) -> PaletteAction;

/// PURE: the overlay's lines, selected row highlighted, sized to `min(items, max_rows)` — it
/// never reserves rows it has no content for (M12).
pub fn lines(items: &[Item], selected: usize, width: u16, max_rows: u16, theme: &Theme) -> Vec<Line<'static>>;
```

`CommandError::Unknown` already carries `did_you_mean`; this phase makes the shell render it and keep
the text:

```rust
/// PURE: the notice a command miss produces. Always three parts: what was typed, the nearest
/// known command if there is one (`CommandError::Unknown::did_you_mean`, already an
/// `Option<String>`), and the way out.
///   unknown command `tmp` — did you mean `focus`? · Enter again sends it as a message · /help
pub fn miss_notice(typed: &str, did_you_mean: Option<&str>) -> String;
```

`/help` is regenerated from `keymap::hints()` (the table in §2.1) plus `commands.list()`, with plain
descriptions. The rewrites, verbatim, because the audit quoted the old ones:

| Command | Was | Becomes |
|---|---|---|
| `/help` | "the commands and key hints this surface has" | "list the commands and keys this window understands" |
| `/quit` | "tear the tree down and leave" | "close bough" |
| `/agents` | "the roster: status, trajectory, unconsumed mail" | "list the agents, what each is doing, and how many messages are waiting" |
| `/focus` | "show one agent in the main pane" | "show one agent's conversation" |
| `/drift` | "per-agent stability signals from the ledger" | "show how much an agent's stated goal has moved lately" |
| `/oldfeed` | "what the old-feed bridge last swept" | "show what the old bough feed last imported" |
| `/prime` | (none rendered) | "load past shell history for a topic into the agent's context" |
| `/reconsolidate` | "distil, surface contradictions and expire stale evidence" | "re-read the agent's memory and flag anything that now contradicts" |

Each of `/focus`, `/drift`, `/oldfeed`, `/prime` (M27) either renders a real result or returns
`CommandError::Failed` with the reason the log already had —
`` /oldfeed: the old-feed bridge is off (no jungler.db at ~/.jungler/jungler.db) ``.

### 2.9 Feedback — notices, copy, patch reload

* The notice band already exists (P3-D23) and sizes to content; this phase gives it a ROLE colour, a
  TTL (`notice_ms`, transient) or none (an error waits for the next key), and makes Esc dismiss it.
* Copy (M21): `TuiHandle::copy` already writes OSC52; on success it now raises
  `notify_kind("copied {n} lines", NoticeKind::Copied)` and the selection stays painted for
  `flash_ms`. The status line's hint list carries `shift-drag: terminal's own selection` while a
  selection is live, which is the escape hatch for a user who wants the terminal's copy instead.
* Config patch reload (M15): the launcher's watch task learns to say so on screen.

```rust
// crates/bough/src/watch.rs
/// `config/reload` — EMIT. The launcher raises it after every recompose attempt; `tui-shell`
/// listens and renders the SAME TEXT the log gets (M15's whole complaint).
pub struct ConfigReloadEvent;
impl EmitEvent for ConfigReloadEvent {
    const NAME: &'static str = "config/reload";
    type Payload = ConfigReload;
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigReload {
    Applied { rows_changed: usize },
    Rejected { detail: String },
}
```

The listener lives in `tui-shell` (`NoticeKind::Config`), so a headless profile with no TUI simply has
no listener and the behaviour is unchanged.

### 2.10 Truth — cwd, capabilities, cost, checkpoint, about-line

**The cwd (B5).** The cause is in the tree and is not a daemon: `bundles/bough-base.yml` sets
`tools-baseline.root: "."`, and `fs::contain` canonicalises that relative root **on every call**,
against whatever the process cwd is at that moment. Any launch path that starts `bough` with an
inherited or later-changed cwd silently retargets every tool. The fix is to resolve it once and to
publish the answer:

```rust
// plugins/tools/src/lib.rs — Definition only, no implementation.
/// `ctx.workspace` — the one directory tool calls resolve against, pinned at boot.
pub struct Workspace;
impl ServiceKey for Workspace { const KEY: &'static str = "workspace"; type Value = WorkspaceRoot; }

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceRoot(Arc<PathBuf>);
impl WorkspaceRoot {
    pub fn new(p: PathBuf) -> WorkspaceRoot;
    pub fn path(&self) -> &Path;
}

// plugins/tools-baseline/src/lib.rs — the Provider.
/// PURE: a configured root against the process cwd, resolved ONCE at activation. A relative root
/// joins the cwd; an absolute one is taken as given; the result is canonicalised, and a root that
/// does not exist is a LOAD failure, not a per-call error (§0.2 fail loud).
pub fn pin_root(configured: &Path, process_cwd: &Path) -> Result<PathBuf, String>;
```

`BaselineConfig.root` keeps its `"."` default and its meaning; `fs::contain` takes the pinned absolute
root and never canonicalises again. `tui-status` renders `WorkspaceRoot` — so the value the tools use
and the value on screen are the same object, and a divergence is impossible rather than untested.

**Capability honesty (M25).** Two changes, and the second is the real one:

```rust
// plugins/projection-assembler/src/bands.rs
/// The identity band gains a `tools:` line naming the tools registered IN THIS AGENT'S SCOPE.
pub fn identity_section(name: &AgentName, row: Option<&AgentRow>, tools: &[ToolName]) -> RenderedSection;

// plugins/tool-actions/src/lib.rs
/// Register one tool per action kind THAT HAS A LIVE PROVIDER. `ActionsHandle::kinds()` already
/// answers this and is "empty in Phase 2, on purpose" — this row just stops ignoring it. With no
/// `actions-github` row, `open_pr` and `push_to_pr` are absent from the prompt entirely: §9's rule
/// that a filtered-away tool is indistinguishable from one that never existed.
///
/// Registrations are effects, so the set is RECONCILED, not registered once: the row re-reads
/// `kinds()` on its tick and disposes the tools whose kind withdrew. (There is no
/// `actions/provider-changed` event today and this phase does not add one — no Provider exists to
/// raise it before Phase 6. When `actions-github` lands, that event replaces the tick.)
pub fn reconcile_action_tools(actions: &ActionsHandle, tools: &ToolsHandle) -> Vec<ToolName>;
```

The agent stops advertising `open_pr` because it stops being offered `open_pr`, not because a prompt
line tells it to be modest.

**Cost and context (M24).** `%context-left` comes from the projection, which already measures itself;
cost comes from a new durable step, so both survive a relaunch.

```rust
// plugins/ledger/src/vocabulary.rs — RequestHeader gains ONE additive field.
#[serde(default)]
pub projection_tokens: usize,   // Assembled::tokens, the numerator of "% context left"

// plugins/llm/src/lib.rs — the new step type, owned by the seam that owns model calls.
/// `usage/round` — Thought, ignorable. One per model round that reported usage.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct UsageRound {
    pub step_index: u32,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)] pub cache_read_tokens: Option<i64>,
    #[serde(default)] pub cache_write_tokens: Option<i64>,
    /// The provider's number when it gives one, else computed from `model-policy.prices`.
    #[serde(default)] pub cost_usd: Option<f64>,
}

// plugins/model-policy/src/price.rs
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Price { pub input_per_mtok: f64, pub output_per_mtok: f64,
                   pub cache_read_per_mtok: f64, pub cache_write_per_mtok: f64 }
/// PURE: usage × price. `None` when the model has no row in the table — an unknown price is
/// reported as unknown, never as zero.
pub fn cost_usd(u: &Usage, p: Option<&Price>) -> Option<f64>;
```

`model-policy` gains an `llm/stream` listener that tees `Chunk::Usage` exactly the way
`tui-focus::stream::apply_tee` does (observe, replace nothing, short-circuit nothing) and appends
`usage/round`. It is the right home: it already owns which model runs, so it is the row that knows what
that model costs.

**Checkpoint and restore (M28).**

```rust
// plugins/ledger-sqlite/src/store.rs
/// `PRAGMA wal_checkpoint(TRUNCATE)`, then drop the connection. Called from the row's disposer,
/// before `retire()`. A 231k WAL beside a 4.1k db is what an unclosed shutdown looks like.
pub async fn checkpoint(&self) -> Result<(), LedgerError>;
```

**The about-line (minor 29).**

```rust
// plugins/residents/src/about.rs (new)
/// PURE: one clean sentence. Markdown stripped, emoji kept, whitespace collapsed, clipped on a
/// WORD boundary with `…`, never spliced with `;`. `read mail \`say hi\`; Hi; ! 👋 ; **` was
/// three fragments and a dangling bold marker.
pub fn one_sentence(raw: &str, max_chars: usize) -> String;
```

### 2.11 Step types and events added by this phase

| Name | Kind | Owner | Class | Notes |
|---|---|---|---|---|
| `usage/round` | durable step | `llm` | Thought, `ignorable` | Per model round; the status line's cost sums these |
| `config/reload` | EMIT event | `bough` (launcher) | — | Payload `ConfigReload`; `tui-shell` renders it as a notice |

`request/header` gains `projection_tokens` (additive, `#[serde(default)]`, not part of `same_four` — it
moves with every step and comparing it would put a header on every step, exactly as `as_of` would).
No other step type changes. The `wake/*` names stay: the vocabulary sweep is over rendered chrome only.

### 2.12 The slot API, frozen (note for `rebuild-b`)

`Slot`, `SlotSize`, `PaneSpec`, `PaneInfo`, `HitMap`, `RenderCx`, `ShellView`, `PaneEvent`,
`PaneOutcome`, `PaneCx` and the `Pane` trait are **unchanged** by this phase, with two exceptions,
both additive and both listed here so `rebuild-b`'s `tui-drafts` pane needs no edit beyond a
recompile:

1. `pane::layout` takes a fourth parameter `gutter: u16`. Call sites inside `tui-shell` only.
2. `ShellView` gains `pub row_focus: Option<usize>` and `pub following: bool` — a pane that ignores
   them renders exactly as before.

`ComposerAction` gains a `Newline` variant (matched only inside `tui-shell`). If `rebuild-b` matches on
`ComposerAction` exhaustively it needs one arm; that is the whole migration.

**Integration addendum (what the work packages actually landed).** Two further additive deltas, so
`rebuild-b` has the whole list in one place:

3. `SlotSize` gains a `Responsive { collapse, preferred, min, max }` variant (WP-4). `Cells`,
   `Percent` and `Fill` are untouched; a pane that registers one of those renders exactly as
   before. It exists because `PaneInfo::size` — not `rail_width` — is what `layout` reads, so the
   rail's collapse breakpoint could not otherwise make its slot cost zero columns. A `rebuild-b`
   pane needs an arm only if it matches `SlotSize` exhaustively.
4. `Composer::on_key` takes a second parameter, `in_burst: bool` (WP-2). Internal to `tui-shell`;
   listed only because a pane that reuses the composer would see it.

### 2.13 Bundle rows and config added

```yaml
# bundles/bough-tui-app.yml
- id: tui.status
  plugin: tui-status
  inject: [tui, ledger]
  config:
    cwd_max: 40
    spinner: "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
    spinner_ms: 80
    # R4: `esc to interrupt` is NOT a hint. It is `Field::StopKey`, present only while running.
    hints: ["? = help", "^f = search"]

# tui (tui-shell) gains:
    transcript_pane: "tui.focus"
    measure_cols: 90
    gutter: 1
    exit_arm_ms: 3000
    paste_burst_ms: 20
    history_cap: 200
    notice_ms: 6000
    flash_ms: 900

# tui.strip gains:
    collapse_cols: 100
    min_width: 22
    max_width: 40

# model.policy gains:
    prices:
      claude-haiku-4-5-20251001: { input_per_mtok: 1.0, output_per_mtok: 5.0,
                                   cache_read_per_mtok: 0.1, cache_write_per_mtok: 1.25 }
```

Every one of these is validated in the row's `validate()` and rejected loudly on a zero or a nonsense
value, the way `frame_ms` and `page_lines` already are. The price numbers above are placeholders in
this document: WP-7 sets them from the current published table at implementation time and cites it in
a comment, and a model with no row shows cost as `—`, never as `$0.00`.

---

## Work packages

Eight packages. File sets are disjoint. WP-1 and WP-7 land the two interfaces others read
(`TuiHandle`'s new methods; `Workspace` and `usage/round`) in their FIRST commit, so the rest can start
against a compiling tree. Each package's tests are unit tests beside the module unless the line says
shell-use; the shell-use suite itself is WP-8's.

### WP-1: the focus model, the keymap, interrupt and exit

**Files:** `plugins/tui-shell/src/run.rs`, `plugins/tui-shell/src/keymap.rs` (new),
`plugins/tui-shell/src/lib.rs`, `plugins/tui-shell/src/term.rs`,
`plugins/tui-shell/tests/focus.rs` (new), `plugins/tui-shell/tests/keymap.rs` (new),
`plugins/tui-focus/src/lib.rs`, `plugins/tui-focus/src/expand.rs`,
`plugins/tui-focus/src/rowfocus.rs` (new), `plugins/tui-focus/tests/rowfocus.rs` (new),
`crates/bough/src/boot.rs`, `crates/bough/src/cli.rs`.

Own §2.1 and §2.4. Make `Focus` explicit and always visible: a click on a transcript row expands the
row and does **not** move the keyboard; `Tab`/`Shift+Tab` move it deliberately and the receiving pane
draws a ring; any printable key returns it to the composer. Add `RowFocus` to the focus pane with
`↑`/`↓` moving it and `Enter`/`Space` toggling the disclosure, and fix the click hit-test origin so a
click toggles the row it landed on, expanded rows included (M26). Route `PageUp/PageDown/Home/End` to
`tui.transcript_pane()` from every `KeyContext`, and the wheel to the pane under the pointer with focus
untouched. Bind `Esc` to interrupt while running and to overlay-dismiss otherwise, never to draft
destruction. Implement `ExitArm`, the farewell, and `shutdown_bounded`. Draw the overlays (notice,
palette, flash) in `draw()` from the pure line-builders WP-5 and WP-2 provide.
Ships: `action_for` truth-table tests over all eight `KeyContext` combinations that differ;
`snaps_to_composer` accepts `a`, rejects `Ctrl+a`, `Alt+a`, `F1`, `Enter`; `RowFocus::moved` clamps at
both ends and enters from `None` at the last row; `ExitArm` arms, exits inside the window, re-arms after
it; `shutdown_bounded` returns `TimedOut` against a fiber that never quiesces and still restores.

### WP-2: the draft — paste, history, readline, the slash-miss

**Files:** `plugins/tui-shell/src/composer.rs`, `plugins/tui-shell/src/draft.rs` (new),
`plugins/tui-shell/tests/input.rs`, `plugins/tui-shell/tests/draft.rs` (new).

Own §2.3. `PasteBurst`: a newline arriving within `paste_burst_ms` of the previous key is a newline in
the draft, not a send, so a raw three-line paste becomes one three-line draft and one send (B4).
`SentHistory` on `↑`/`↓` over an empty draft, holding the live draft so `↓` gives it back (M20).
`kill_to_line_start` for `Ctrl+U`, which today deletes one character. `Esc` no longer clears anything.
Enter on a command line no longer clears the buffer; the shell clears it on a resolved dispatch, and a
miss leaves the text with `arm_send_as_message()` set, so a second unchanged Enter sends it as a
message (B3). `caret_at` maps a click to a character offset (minor 33). The placeholder becomes a
sentence (M16).
Ships: three-line burst ⇒ one `Send` with two `\n`; the same three lines typed slowly ⇒ three sends;
`Ctrl+U` on `abcdefgh|` leaves an empty line and on `abc|def` leaves `def`; history `prev`/`next`
round-trips and never loses the live draft; `Esc` on `draft` leaves `draft`; `//x` sends `/x` as a
message; a missed command's text is still in the buffer after `on_key(Enter)`.

### WP-3: the transcript — scroll, follow, streaming, markdown, reflow

**Files:** `plugins/tui-focus/src/scroll.rs`, `plugins/tui-focus/src/rows.rs`,
`plugins/tui-focus/src/stream.rs`, `plugins/tui-focus/tests/scroll.rs`,
`plugins/tui-focus/tests/rows.rs`, `plugins/tui-focus/tests/stream.rs`,
`plugins/tui-render/src/lib.rs`, `plugins/tui-render/src/text.rs`,
`plugins/tui-render/src/md.rs` (new), `plugins/tui-render/src/diff.rs`,
`plugins/tui-render/tests/md.rs` (new), `plugins/tui-render/tests/wrap.rs`.

Own §2.2 and §2.6. `Viewport` around the existing `Scroll`: follow at the tail, count unseen rows while
anchored, `badge()` for `↓ N new`, `to_latest()` on `End` and on sending. Enforce the no-stored-wrap
rule: `Row::lines` takes the width of the frame and calls `md::document`, so a resize re-wraps history
and a chunk boundary cannot persist. Write `md::blocks` — headings, bold, inline code, fenced code,
nested lists with hanging indent, tables, quotes, rules — TOTAL over unterminated input because it runs
on the live tail. Render turn markers in the turn/message vocabulary at body contrast. Fix reflow's
spurious blank lines (nit 39) and the diff/tool-output styling nits (35) that fall out of one container.
Ships: `blocks` on a 40-fixture corpus including a half-written fence, a half-written table and a
heading with no trailing newline; `document` at widths 40/80/90/200 with no mid-word break and no
orphan backtick; a replayed multi-chunk answer whose `document` output is byte-identical to the same
text delivered in one chunk; `Viewport` follows, counts, badges and re-arms; a `rows` golden that the
same trajectory renders identically before and after a width change.

### WP-4: the frame — the status row, the rail, the gutter, contrast

**Files:** `plugins/tui-status/**` (new crate), `plugins/tui-strip/src/lib.rs`,
`plugins/tui-strip/src/rail.rs`, `plugins/tui-strip/tests/rail.rs`,
`plugins/tui-shell/src/pane.rs`, `plugins/tui-shell/src/theme.rs`,
`plugins/tui-shell/src/contrast.rs` (new), `plugins/tui-shell/tests/layout.rs`,
`plugins/tui-shell/tests/contrast.rs` (new).

Own §2.5. Build `tui-status` as a one-row `Slot::Status` pane: product, cwd (from `ctx.workspace`),
model and `%` context (from the newest `request/header`), cost (Σ `usage/round`), a spinner and elapsed
while running, and the key hints — dropping fields in a fixed order as the width shrinks so the line is
always exactly one row. Give the rail a breakpoint (`0` columns below `collapse_cols`, clamped between
`min_width` and `max_width`), hard clipping, and a gutter column that belongs to no pane. Cap the prose
measure at `measure_cols`. Move `hint` and `dim` above 4.5:1, add `measure_bg`, and write the contrast
audit.
Ships: `fields` at widths 200/120/80/40 drops in the documented order and never exceeds the width;
`elide_path` elides the middle and keeps the last component; `rail_width` is 0 at 80, ≥`min_width` at
120, ≤`max_width` at 200; `layout` gives the Strip slot `width + gutter` and the pane `width`, and the
Main slot's `x` is `strip + gutter`; `contrast::audit` asserts every role of both themes ≥4.5:1 with the
failing role NAMED in the message.

### WP-5: commands — the palette, `/help`, the miss, the four no-ops

**Files:** `plugins/commands/src/lib.rs`, `plugins/commands/src/parse.rs`,
`plugins/commands/src/palette.rs` (new), `plugins/commands/tests/dispatch.rs`,
`plugins/commands/tests/palette.rs` (new), `plugins/tui-shell/src/builtins.rs`,
`plugins/old-feed-adapter/src/command.rs`, `plugins/drift-watch/src/command.rs`,
`plugins/dormancy/src/command.rs`.

Own §2.8. `Palette`: opened by `/` at line start, filtered as the user types, `↑`/`↓` to move, `Tab` to
complete the name, `Enter` to accept, `Esc` to close — and it never reserves rows it has no items for.
`miss_notice` renders did-you-mean plus `try /help` plus the send-as-message hint. Regenerate `/help`
from the keymap table and `commands.list()`, echo the typed command above its output, and rewrite every
summary into the plain-language column of §2.8's table. Make `/drift`, `/oldfeed`, `/prime` and
`/agents` render real output or a stated reason, and give `/agents` column headers.
Ships: `filter` orders prefix before substring and is stable under a growing query; `on_key` wraps at
both ends and `Tab` completes without accepting; `miss_notice("/tmp is where…", Some("focus"))` contains
the typed text, `did you mean`, and `/help`; every registered command's `summary` passes a lint test
that rejects the words `tree`, `lane`, `mail`, `wake`, `distil`; `/oldfeed` with no `jungler.db` returns
a `Failed` naming the missing file rather than `Ok("")`.

### WP-6: search — rendered text, snippets, highlight, real jumps

**Files:** `plugins/tui-search/src/lib.rs`, `plugins/tui-search/src/index.rs` (new),
`plugins/tui-search/src/invariant.rs`, `plugins/tui-search/tests/search.rs`,
`plugins/tui-search/tests/index.rs` (new).

Own §2.7. Index `Entry` values built from rendered `Row`s, with the ENVELOPE step types producing no
entry at all — that alone removes every `request/header  {"as_of":53,…` row the audit screenshotted.
Snippet with a window around the match, the match span highlighted, `n of N`, `n`/`N` to step, Enter or
click to jump. Give the field chrome and a label so it is not a dim floating string, and clear query,
hits and rows on `Esc` (minor 30). Keep the Phase 3 swap behaviour: the row disabled by patch reflows
the layout and `Ctrl+F` degrades to a notice.
Ships: `entries` over a fixture trajectory contains no `request/header`, no `step/start` and no JSON
braces; `search` finds a term spanning a joined multi-part `Row::Text`; `counter` reads `1 of 3` and
wraps under `n`/`N`; `lines` puts `sel_bg` exactly on the match bytes; `clear` empties all three fields;
a hit's `HitId` round-trips to the right `StepId`.

### WP-7: truth — cwd, capabilities, cost, checkpoint, about-line, patch notices

**Files:** `plugins/tools/src/lib.rs`, `plugins/tools-baseline/src/lib.rs`,
`plugins/tools-baseline/src/fs.rs`, `plugins/tools-baseline/tests/cwd.rs` (new),
`plugins/tool-actions/src/lib.rs`, `plugins/projection-assembler/src/bands.rs`,
`plugins/llm/src/lib.rs`, `plugins/model-policy/src/lib.rs`,
`plugins/model-policy/src/price.rs` (new), `plugins/ledger/src/vocabulary.rs`,
`plugins/ledger/src/types.rs`, `plugins/ledger-sqlite/src/lib.rs`,
`plugins/ledger-sqlite/src/store.rs`, `plugins/residents/src/lib.rs`,
`plugins/residents/src/about.rs` (new), `plugins/tui-shell/src/clip.rs`,
`crates/bough/src/watch.rs`.

Own §2.10. Declare `Workspace` in `tools`, provide it from `tools-baseline` via `pin_root` at
activation, and make `fs::contain` take the pinned absolute root. Register action tools only for kinds
with a live Provider, and name the scope's registered tools in the identity band. Add
`projection_tokens` to `request/header`, `usage/round` to `llm`, the price table and the usage tee to
`model-policy`. Checkpoint the WAL on disposal. Make the about-line one clean sentence. Raise
`config/reload` from the watch task. Add the copy flash to `clip.rs`.
Ships: `pin_root` resolves `"."` against a given cwd and is unaffected by a later `set_current_dir`; a
tool call with a relative path lands under the pinned root in a `tempdir` (the disk assertion, not a
string one); `kinds_with_providers` is empty with no Provider mounted and the tool list then omits
`open_pr`; `identity_section` lists exactly the tools passed; `cost_usd` returns `None` for an unpriced
model; `one_sentence` on the audit's `` read mail `say hi`; Hi; ! 👋 ; ** `` returns one sentence with
no markers; a ledger appended to, disposed, and reopened reports every step and leaves no WAL over a
page.

### WP-8: integration — rows, the shell-use suite, the swap, the re-audit

**Files:** `bundles/bough-tui-app.yml`, `bundles/bough-base.yml`, `profiles/tui.yml`,
`scripts/tui/16-focus.sh` … `scripts/tui/24-honesty.sh` (new), `scripts/tui/lib.sh`,
`scripts/tui/fixtures/**`, `Makefile`, `BUILD.md`, `docs/phase-ux1-plan.md`, `docs/ux-audit-2.md` (new).

Wire the `tui.status` row and every new config field, keep `--dump-config` honest, and extend
`scripts/tui/lib.sh` with the helpers the new scripts need (`t_cells` for a colour assertion,
`t_disk` for a file assertion, `t_size` for a three-size resize walk). Write the nine new numbered
scripts of §3 and keep the fifteen existing ones green in BOTH halves. Run the swap: disable
`tui.status` by patch while the TUI runs, assert the row disappears and the layout reflows, re-enable
and assert it returns; then the same for `tui.search`, unchanged from Phase 3. Finally, drive the
three-persona re-audit of V11 and write `docs/ux-audit-2.md`.
Ships: `make tui-test` green in the replay half and the live half; `make gates` green; the swap script;
`docs/ux-audit-2.md` with a screenshot per confirmed fix and a residuals table.

---

## 3. Verification map

Every bullet names the test that proves it. `scripts/tui/*.sh` names are shell-use scripts run by
`make tui-test` in both halves; Rust names are `#[test]` functions in the file given.

### V1 — the focus model

| Claim | Test |
|---|---|
| Click a tool row to expand it, then type and Enter, and the turn starts | `scripts/tui/16-focus.sh` → `click_then_type_still_sends` |
| `↑`/`↓` move a VISIBLE row focus over tool rows | `16-focus.sh` → `arrows_move_a_visible_row_focus`; `plugins/tui-focus/tests/rowfocus.rs` → `it_clamps_at_both_ends_and_never_wraps` |
| `Enter`/`Space` toggle the focused row | `16-focus.sh` → `enter_toggles_the_focused_row`, `space_toggles_the_focused_row` |
| `Tab` shows a focus ring | `16-focus.sh` → `no_ring_before_tab` then `tab_paints_a_focus_ring` (a BEFORE/AFTER pair on the ring glyph `▎`) |
| A printable key snaps focus back | `plugins/tui-shell/tests/keymap.rs` → `snaps_to_composer_accepts_a_printable_character_and_nothing_else` |
| No persona path loses typed text | `16-focus.sh` → `the_four_audit_paths_lose_nothing` (B1's, B6's, M23's and M26's exact repros in sequence) |
| Clicking an expanded row collapses it, on the row clicked | `16-focus.sh` → `click_toggles_the_row_it_landed_on` |

### V2 — scroll

| Claim | Test |
|---|---|
| PageUp/PageDown/Home/End scroll regardless of focus | `scripts/tui/17-scroll.sh` → `scroll_keys_work_from_the_composer`, `…_from_the_focus_pane`, `…_from_the_search_pane` |
| The wheel scrolls the transcript | `17-scroll.sh` → `the_wheel_scrolls_the_transcript` |
| The view follows new output at the tail | `plugins/tui-focus/tests/scroll.rs` → `follow_re_arms_at_the_bottom`; `17-scroll.sh` → `the_tail_follows_a_live_answer` |
| `↓ N new` when scrolled up | `scroll.rs` → `an_anchored_viewport_counts_what_arrives_and_badges_it`; `17-scroll.sh` → `scrolled_up_shows_the_new_badge` |
| `End` jumps to latest | `17-scroll.sh` → `end_returns_to_the_latest_row` |
| The viewport is stable while streaming | `17-scroll.sh` → `an_anchored_viewport_does_not_move_while_streaming` |

### V3 — text is never destroyed

| Claim | Test |
|---|---|
| `/tmp is where my files are` stays, with a hint | `scripts/tui/18-draft.sh` → `a_missed_command_keeps_the_sentence`; `plugins/commands/tests/palette.rs` → `a_miss_names_the_text_the_suggestion_and_the_way_out` |
| …and can be sent as a message | `18-draft.sh` → `a_second_enter_sends_the_missed_line_as_a_message` |
| A raw 3-line paste is one draft and one send | `18-draft.sh` → `a_raw_three_line_paste_is_one_draft_and_one_send`; `plugins/tui-shell/tests/draft.rs` → `a_three_line_paste_burst_becomes_one_draft_and_one_send` |
| `Esc` leaves a non-empty draft intact | `18-draft.sh` → `esc_leaves_the_draft`; `draft.rs` → `esc_on_a_non_empty_draft_leaves_it_alone` |
| `Ctrl+U` clears the line | `18-draft.sh` → `ctrl_u_clears_the_line`; `draft.rs` → `ctrl_u_kills_to_the_start_of_the_line` |
| `↑` recalls the last sent message | `18-draft.sh` → `up_recalls_the_last_sent_message`; `draft.rs` → `history_round_trips_and_hands_the_live_draft_back` |
| `Shift+Enter` inserts a newline | `18-draft.sh` → `shift_enter_inserts_a_newline` (and `alt_enter_inserts_a_newline`) |

### V4 — interrupt and exit

| Claim | Test |
|---|---|
| `Esc` interrupts and an `interrupted` marker renders | `scripts/tui/19-interrupt.sh` → `esc_interrupts_and_marks_it` |
| `esc to interrupt` shows while running | `19-interrupt.sh` → `the_stop_key_is_absent_while_idle` + `the_stop_key_is_named_while_running`; `plugins/tui-status/tests/status.rs` → `the_stop_key_exists_only_while_a_turn_is_running` |
| Idle `Ctrl+C` shows `press again to exit` | `19-interrupt.sh` → `an_idle_ctrl_c_asks_before_exiting`; `plugins/tui-shell/tests/keymap.rs` → `exit_arms_then_exits_inside_the_window_and_re_arms_after_it` |
| The second exits with the terminal restored | `19-interrupt.sh` → `the_second_ctrl_c_exits_with_the_terminal_restored` |
| `/quit` prints a goodbye and exits within 2s | `19-interrupt.sh` → `quit_exits_cleanly_within_three_seconds`; `crates/bough/src/boot.rs` → `bounded_teardown_tests::a_teardown_that_never_finishes_times_out_and_still_restores_the_terminal` (inline, not a `tests/` file) |

### V5 — the frame

| Claim | Test |
|---|---|
| The status line shows name, cwd, model, %context, cost, hints | `scripts/tui/20-frame.sh` → `the_status_line_names_the_six_things`; `plugins/tui-status/tests/status.rs` → `the_line_drops_fields_in_the_documented_order_and_never_exceeds_its_width` |
| At 80x24 the rail is collapsed and nothing overlaps | `20-frame.sh` → `at_80x24_the_rail_is_gone_and_no_row_carries_two_runs`; `plugins/tui-shell/tests/layout.rs` → `the_strip_slot_pays_for_the_gutter_and_the_pane_never_gets_it` |
| At 200x50 the prose measure is capped | `20-frame.sh` → `at_200x50_the_measure_is_capped_at_ninety` |
| Overlays dismiss with Esc | `20-frame.sh` → `esc_dismisses_help_then_search_then_nothing` |
| Resize re-wraps history with no spurious blank lines | `20-frame.sh` → `three_sizes_rewrap_with_no_blank_line_injected` (120x36 → 80x24 → 200x50) |

### V6 — streaming and markdown

| Claim | Test |
|---|---|
| A long answer wraps with no chunk-boundary breaks (replay) | `scripts/tui/21-stream.sh` → `a_multi_chunk_replay_has_no_mid_word_break` |
| …and live | `21-stream.sh` → `a_live_haiku_answer_has_no_mid_word_break` (runs only under `BOUGH_LIVE=1`; `skip`ped in the replay half) |
| Chunk delivery cannot change the render | `plugins/tui-render/tests/md.rs` → `the_corpus_parses_totally_and_loses_no_words` |
| Headings/bold/code/lists/tables render | `md.rs` → `the_structural_shapes_are_what_they_say`; `21-stream.sh` → `the_capabilities_answer_shows_no_literal_markers` |
| Identical after quit and relaunch | `21-stream.sh` → `the_same_answer_renders_identically_after_a_relaunch` |

### V7 — search

| Claim | Test |
|---|---|
| Snippets with the match highlighted and a hit count | `scripts/tui/22-search.sh` → `hits_are_snippets_with_a_highlight_and_a_count`; `plugins/tui-search/tests/index.rs` → `lines_highlight_exactly_the_match_bytes` |
| Enter/click jumps the transcript to the step | `22-search.sh` → `enter_moves_the_transcript_to_the_hit`, `click_moves_the_transcript_to_the_hit` (asserted on the visible row, not on state) |
| `Esc` clears | `22-search.sh` → `esc_clears_the_query_and_the_hits` |
| No raw JSON is shown | `22-search.sh` → `no_hit_row_contains_a_brace`; `index.rs` → `envelope_steps_produce_no_entry_and_no_json_reaches_the_index` |

### V8 — commands

| Claim | Test |
|---|---|
| `/` opens a filtering palette navigable by keys | `scripts/tui/23-commands.sh` → `slash_opens_a_palette_that_filters_and_moves`; `plugins/commands/tests/palette.rs` → `filter_is_prefix_then_substring_and_stable` |
| `/help` lists real bindings | `23-commands.sh` → `help_lists_the_keys_that_actually_work` (asserts `Esc`, `Ctrl+F`, `PageUp`, `End`, `Ctrl+U`, `Shift+Enter` all appear) |
| `/nonsense` gives did-you-mean + `try /help` and keeps the text | `23-commands.sh` → `an_unknown_command_suggests_and_keeps` |
| Every listed command produces visible output | `23-commands.sh` → `every_listed_command_renders_something` (iterates `/help`'s own list) |
| The four former no-ops work or are gone | `23-commands.sh` → `the_four_no_ops_answer_or_say_why` |

### V9 — feedback and contrast

| Claim | Test |
|---|---|
| A bad `bough.patch.yml` shows a strip notice with the log's message | `scripts/tui/24-honesty.sh` → `a_rejected_patch_says_so_on_screen_with_the_logs_words` |
| A good one shows `reloaded` | `24-honesty.sh` → `a_good_patch_says_reloaded` |
| Drag-select shows a `copied` flash and emits OSC52 | `24-honesty.sh` → `a_drag_select_flashes_copied_and_emits_osc52` |
| A running turn shows a spinner/elapsed | `24-honesty.sh` → `a_running_turn_shows_a_spinner_and_an_elapsed_clock` |
| Every theme role clears 4.5:1 | `plugins/tui-shell/tests/contrast.rs` → `every_foreground_role_of_both_themes_clears_wcag_aa`, `errors_are_a_warning_hue_not_the_hint_hue` |

### V10 — cwd and honesty

| Claim | Test |
|---|---|
| Launched from an empty dir, the file lands there | `24-honesty.sh` → `a_file_in_the_current_directory_lands_in_the_launch_cwd` (asserted on disk, plus `git status` clean in the repo); `plugins/tools-baseline/tests/cwd.rs` → `a_relative_path_lands_under_the_pinned_root` |
| The status line names that directory | `24-honesty.sh` → `the_status_line_names_the_launch_cwd` |
| A later `set_current_dir` cannot move the tools | `cwd.rs` → `pin_root_is_immune_to_a_later_chdir` |
| `what can you do` names only registered tools | `24-honesty.sh` → `the_capability_answer_names_no_tool_that_is_not_registered` (live half; replay half asserts the prompt's tool list instead); `plugins/tool-actions/tests/refusal.rs` → `no_provider_means_no_tool_in_the_registry` |
| The ledger is checkpointed on shutdown and relaunch restores | `scripts/tui/24-honesty.sh` → `a_quit_then_relaunch_restores_every_turn`, `the_shutdown_left_no_wal_over_a_page`; `plugins/ledger-sqlite/tests/checkpoint.rs` → `the_rows_own_disposal_checkpoints_and_a_relaunch_sees_every_step` |
| The about-line is one clean sentence and persists | `24-honesty.sh` → `the_about_line_is_one_sentence_before_and_after_a_relaunch`; `plugins/residents/tests/about.rs` → `one_sentence_strips_markers_and_never_splices` |

### V11 — the UX re-audit

`scripts/ux2/run.sh` drives three personas — **developer-critic**, **andrey-owner**,
**keyboard-only-user** — against the RELEASE binary through shell-use, each in its own empty
`BOUGH_HOME` and its own empty scratch cwd, live haiku for both tiers. Each persona re-walks the
top-12 findings of `docs/ux-audit-1.md` (B1–B8 and M9–M12) using the audit's own repro lines, captures
an SVG per step into `docs/ux-audit-2-shots/<persona>/`, and records a verdict. The gate: **every
blocker and every major is confirmed fixed with a screenshot**; anything not fixed, and anything newly
found, goes into a residuals table in `docs/ux-audit-2.md` with a severity and a named owner crate.
Test: `scripts/ux2/run.sh` exits non-zero if any blocker or major verdict is not `fixed`.

### SWAP — the status row, and the search row

`scripts/tui/25-swap-status.sh`:

1. Boot the tui profile; assert the status line is on screen and the transcript's last row is at
   `rows-2`.
2. Write `entries: {tui.status: {disabled: true}}` into `$BOUGH_HOME/bough.patch.yml` **while the TUI
   runs**; assert within the watch window that the status text is gone, that the transcript grew by
   exactly one row, and that nothing else moved (`shell-use text` diff is one row).
3. Remove the patch; assert the row returns and the layout reflows back.
4. The same three steps for `tui.search` (Phase 3's `09-swap-search.sh` behaviour, unchanged), then
   both disabled at once, then both restored.

No recompile at any step; `--dump-config` before and after matches the composed tree.

### The phase's own gates

`make gates` (build + lint + test + `tui-test-replay`) green; `make tui-test` green in both halves;
`make audit-plugins` still passes with `tui.status` added to `bough-tui-app`.

---

## 4. What this phase does NOT build

* **No new capability.** No `@` file picker (the audit's conventions table marks it "absent,
  untested"; no persona attempted it), no themes beyond dark/light, no scrollbar widget, no
  timestamps beyond the elapsed clock (nit 38's timestamp half is deferred; its speaker-differentiation
  half falls out of WP-3's turn markers).
* **No FTS redesign.** WP-6 indexes rendered rows in memory for the focused agent's trajectory. The
  cross-agent FTS surface is Phase 8's, and Phase 8 will read `index::entries` rather than the ledger.
* **No projection change.** `projection_tokens` is a number the assembler already computes; the
  assembler's behaviour is untouched.
* **No sandbox, no cwd switching.** The workspace is pinned at boot and there is no command to change
  it. A user who wants a different directory launches there.
* **Nits 38 (timestamps) and the "high-contrast theme" half of M22** are recorded as residuals in
  `docs/ux-audit-2.md` if they survive; neither blocks the phase.

---

## 5. Decisions taken where REQUIREMENTS is silent

**D-ux1-1 — one always-live composer; panes take the keyboard only deliberately.** REQUIREMENTS §11
says "full mouse + keyboard parity, clickable expanding tool calls, click-to-focus" and does not say
what focus IS. A click on a transcript row expands the row and does not move the keyboard; only `Tab`,
`Ctrl+F`, a click on a focusable pane's chrome, and an explicit `FocusRequest` move it; any printable
key returns it. "Click-to-focus" in §11 is read as click-to-focus-an-AGENT (the rail), which is what
Phase 5 built, not click-to-focus-a-pane.

**D-ux1-2 — scroll keys are shell-level, not pane-level.** `PageUp/PageDown/Home/End` and the wheel are
routed to the transcript pane from every focus state. The cost is that a future pane cannot own those
keys for itself; the benefit is that the single most-reported blocker cannot come back. `↑`/`↓` stay
context-dependent (composer history / row focus), which is the one exception and is tested as such.

**D-ux1-3 — "send it as a message" is a second Enter, not a chord.** `Ctrl+Enter` and `Shift+Enter` are
unreliably reported by terminals, and the audit already caught `Shift+Enter` behaving differently for
two personas. A missed command arms the buffer; an unchanged second Enter sends it; any edit disarms.
`//` remains the explicit escape and stays documented in `/help`.

**D-ux1-4 — a newline burst is a paste.** With no bracketed-paste wrapper there is no way to know for
certain, so the rule is temporal: a newline within `paste_burst_ms` (default 20ms) of the previous key
is a newline in the draft. A human cannot type that fast; a terminal always delivers a paste that fast.
The window is config, so a pathological input method can raise it.

**D-ux1-5 — the status line is its own row.** REQUIREMENTS §11 names the panes
(`strip`, `focus`, `trajectory`, `search`, `preview`, `timeline`, `drift`) and does not name a status
line. It becomes `tui-status`, a row of its own, because the phase's SWAP gate has to be able to
disable it — and because a status line that is a field of the rail dies with the rail at 80 columns,
which is exactly the width where a user most needs to know what model is running.

**D-ux1-6 — cost is durable, context is derived.** `%context-left` is
`100 − 100 × projection_tokens / budget`, both from the newest `request/header`: it is a property of the
request and nothing else needs to record it. Cost is a new `usage/round` step, because a number that
resets to `$0.00` on relaunch is worse than no number. `model-policy` owns the price table and appends
the step: it already decides which model runs, so it is the row that knows what that model costs. An
unpriced model renders `—`, never `$0.00`.

**D-ux1-7 — the workspace is a seam with one Provider.** §0.2 warns against splitting preemptively, but
the cwd has two consumers on day one (the tool executor and the status line) and they must agree by
construction — the audit's B5 is precisely the failure of two independent answers to "where am I". The
Definition lives in `tools` (it is tool vocabulary), the Provider in `tools-baseline`, and `tui-status`
injects it OPTIONALLY so a profile with no tools still shows a status line.

**D-ux1-8 — a filtered-away action kind is absent, not refusing.** §9 says a tool absent from a scope
"is absent from the prompt AND refuses execution, indistinguishably from a nonexistent one". This
phase applies that to action kinds: `tool-actions` registers a tool only for a kind with a live
Provider. It is the honest reading, and it is what stops the first answer every user sees from being
confident fiction.

**D-ux1-9 — the vocabulary boundary is the rendered surface.** "turn", "message", "agent" in every
user-facing string; `wake/*`, `mail/*`, `lane/*` unchanged in step types, service keys, config and
logs. A lint test in WP-5 keeps the boundary: a registered command whose `summary` contains an internal
word fails the build.

**D-ux1-10 — contrast is measured against a declared background.** `Theme::bg` is `Color::Reset`, so a
ratio against the user's actual terminal is not computable. The theme declares `measure_bg` — the
background the palette was designed for — and the V9 test measures against it. It is a design contract,
not a runtime claim, and it is the only thing that makes "≥4.5:1" a number a test can assert.

**D-ux1-11 — no wrapped line is ever stored.** The stronger version of "accumulate the stream": text
is stored, lines are computed at paint width. It costs a wrap per frame on the visible rows only, and
it makes M10 (persisted chunk breaks), M13 (no re-wrap on resize) and nit 39 (blank lines on reflow)
the same bug with the same fix.

**D-ux1-12 — teardown is bounded and always says something.** `kernel.shutdown()` is awaited under
`--shutdown-ms` (default 2000). On timeout the launcher restores the terminal, prints one line naming
the timeout, and exits with the requested code anyway. A shutdown that cannot complete is a bug to fix
in the row that hangs; a user watching a black screen is not the place to discover it.

---

## Appendix: scaffold deviations

Recorded by the scaffold commit; every implementer reads these as part of the API above.

**D1 — `palette::lines` lives in `tui-shell`, not in `commands`.** §2.8 puts the whole palette in
`plugins/commands/src/palette.rs`, but `lines(..., theme: &Theme)` needs `bough-plugin-tui-shell`'s
`Theme`, and `tui-shell` already depends on `commands` — the dependency would be a cycle. The split
is therefore: `commands::palette` owns `Palette`, `Item`, `filter`, `PaletteAction`, `on_key` and
`miss_notice` (state and pure filtering, `crossterm` added for `KeyEvent`); `tui_shell::palette::lines`
owns the drawing. Nothing else moves.

**D2 — `ServiceKey` spells its name `NAME`, not `KEY`.** §2.10's `Workspace` is written as the
kernel's trait actually is: `impl ServiceKey for Workspace { const NAME: &'static str = "workspace"; }`.

**D3 — `Cli::shutdown_ms` defaults to 2000 as a clap default**, so every existing `Cli` literal in
the tree names it explicitly rather than relying on a `Default` impl the type does not have.

**D4 — `tui-search` now depends on `tui-focus`.** §2.7 indexes `Row`s, which is `tui-focus`'s type.
The direction is acyclic (`tui-focus` does not depend on `tui-search`), and the search row stays the
Phase 3 SWAP subject because nothing depends on *it*.

**D5 — the `tui.status` bundle row is NOT in `bundles/bough-tui-app.yml` yet.** The crate, its
`StatusConfig` and its catalog registration exist; WP-8 adds the row and the config block of §2.13,
which is where the SWAP test that disables it also lands.

---

## Appendix: WP-8 seam notes (integration)

Recorded by WP-8 as it wired the rows and wrote the suite. Everything here is a note about the SEAM
between the packages, not a change to any package's public API.

**W1 — the `tui.status` row is linked from `crates/bough/src/lib.rs`, not only from `Cargo.toml`.**
The scaffold added `bough-plugin-tui-status` as a dependency of the launcher but not the
`use bough_plugin_tui_status as _;` line that every other row has. Without it `inventory::submit!`
never lands in the binary's catalog and the new bundle row fails to compose with "names plugin
`tui-status`, which the catalog does not have" — a fail-loud boot, correctly, but one whose cause is
a missing link line rather than a missing crate. WP-8 adds the one line. §2.13's D5 covers the
bundle row; this covers the catalog.

**W2 — the phase's new `tui` and `tui.strip` config fields are written into the bundle even though
every one of them has a `#[serde(default)]`.** A default that is never written down is a tunable
nobody can find: AGENTS.md's rule is that a deployment-varying value is a bundle field, and the
whole point of `transcript_pane` and `collapse_cols` is that a deployment might want them
different. `--dump-config` therefore shows the real numbers rather than an empty map.

**W3 — `n`/`N` in the search pane became `Ctrl+n`/`Ctrl+Shift+n`.** WP-6 changed this at the seam
while WP-8 was writing `22-search.sh`, and it is right: the query field is a TEXT input, so a bare
`n` has to stay typable or root cause (c) is violated in the one pane that exists to find text.
§3 V7's "`n`/`N` to step" reads as the chord in the shipped keymap; `/help` lists the chord.

**W4 — the swap script is `scripts/tui/25-swap-status.sh` and it re-runs the Phase 3 search swap.**
§3's SWAP bullet describes four steps; the script does all four in one file so the "both disabled at
once" state is reached from a tree that has already proven each row individually. `09-swap-search.sh`
is untouched and still runs.

**W5 — three fixtures were added under `scripts/tui/fixtures/`:** `markdown.patch.yml` (chunk
boundaries deliberately mid-word and mid-marker, for V6), `slow.patch.yml` (a round with seconds of
`delay_ms` between chunks, for V4's interrupt and V9's spinner — against the shared fixture every
"while it is running" bullet raced the end of the turn), and `cwd.patch.yml` (one `write_file` with
a bare relative path, for V10's disk assertion).

**W6 — `lib.sh` gained six helpers, not three.** §3 names `t_cells`, `t_disk` and `t_size`; the
walk also needs `no_blank_run` (nit 39's assertion, which `t_size` calls), `screen_rows` (the swap's
one-row screen diff) and `write_patch`/`clear_patch` (the patch-file write every swap script was
open-coding). All are exported for the `bash -c` subshells the suite drives assertions through.

---

## Deviations and open items (phase ux1 review)

Written by the review pass that closed the phase. Everything here is either a change to what §2
specified, or a thing that is knowingly not done.

### Changes to the public API §2 froze

**R1 — `ShellView` gains `measure_cols`, and `RenderCx` gains `measure()` and `report_rows()`.**
§2.5's prose measure was specified as `pane::measure(width, cap)` and then never called: `measure`
had no production call site, `TuiConfig::measure_cols` was read by nothing, and `tui-focus` wrapped
at `cx.area.width`, so a 200-column terminal got a ~159-column paragraph. `RenderCx::measure()` is
now the way a pane asks for the prose width, and `tui-focus` wraps at it.

**R2 — `ShellView::row_focus` / `following` are FILLED, from a pane report.** §2.12 froze both
fields and WP-1/WP-3 left them hardcoded `None` / `true`, so `following` lied permanently. The shell
cannot read inside a pane, so the honest shape is a report: `RenderCx::report_rows(row_focus,
following)` writes a `pane::RowReport`, `draw` collects one per pane, and the NEXT frame's
`ShellView` is fed from it. `row_focus` is the reported value for THAT pane; `following` is the
report from `TuiConfig::transcript_pane`. **A pane that reports nothing still renders exactly as
before** — the documented defaults are what an absent report means, so the slot API stays
source-compatible for `rebuild-b`'s `tui-drafts` pane. `RenderCx` is only constructible inside
`tui-shell`, so the added private field breaks no outside caller.

**R3 — the transcript reserves ONE column for a focus ring.** `PaneView::is_focused` was written by
the shell and read by no pane. `tui-focus` now reserves column 0 of its area unconditionally and
paints `▎` in `theme.accent` there only when it holds the keyboard. Reserved unconditionally on
purpose: a ring that appears and disappears must not reflow the transcript.

**R4 — `Field::StopKey` replaces the static `esc = interrupt` hint.** `esc to interrupt` was one of
`tui.status.hints`, rendered at every width that fits, idle or running — so M14's bullet (`see
"esc"`) could not fail. It is a field now, present only while `StatusView::running`, and the shipped
`hints` list drops it.

**R5 — `bough_kernel::ConfigReloadEvent` / `ConfigReload` moved out of the launcher.** M15's
listener was installed by `crates/bough/src/boot.rs`, capturing the `TuiHandle` Arc once at boot
through `peek_live`. Targets are provider-fiber identities: a `tui` row that reloads — which saving
a patch file, the very event being reported, can cause — installs a NEW handle, and the launcher
kept notifying the disposed one. The event now lives in the kernel (loader vocabulary, the same
family as `config-update-failed`) and the listener is an effect of `tui-shell`, so it is rebuilt on
every reload of that row and disappears when the row is disabled.

**R6 — `shutdown_bounded(kernel, ms)` lost its `code` parameter.** It opened with `let _ = code;`
and the doc claimed it exits with it; it does neither. The caller owns the exit code.

**R7 — `actions/providers-changed` is a new capability event.** `tool-actions`'s doc claimed the
tool set is "reconciled … on its tick"; there was no tick and no disposal. A Provider registers
INTO `ActionsHandle` rather than by re-providing the `actions` key, so §0.3's activation-driven
reload cannot see it. `ActionsHandle::provider` now emits on registration and on disposal, and
`tool-actions` listens, disposes what it registered, and reconciles.

**R8 — `WorkspaceRoot::new` returns `Result`.** The type's stated invariant ("an ABSOLUTE,
canonicalised directory") rested on its single call site. It is enforced by the constructor now.

**R9 — `run_query` returns `Found { hits, windowed }`, and the counter names the horizon.** The
per-agent scan is bounded by `SearchConfig::window`; a term older than the window returned "no
matches", indistinguishable from a term never said. The counter now reads `no matches · newest 400
steps` when the window was full. The Phase 1 FTS index is still used by no surface — see O1.

**R10 — `palette::echoed` writes `palette::NO_OUTPUT` for an empty answer**, and the house-word lint
moved from four hand-written literal lists to `CommandsHandle::register`, which every row goes
through.

**R11 — `one_sentence` moved from `bough-util` to `bough-plugin-tui-render` (`sentence.rs`).** §0.1
enumerates the center exhaustively as "branded ids, home paths, timeouts"; about-line vocabulary is
presentation.

**R12 — `StripConfig::gutter` is DELETED.** It was defaulted, written into the bundle and read by
nobody; the gutter layout honours is `TuiConfig::gutter`. One column, one knob.

**R13 — `Row::WakeMark` carries `cause`, and `turn_mark_words` takes it.** §5 reserves the
`interrupted` wake reason for a preempted wake, so a user's Esc lands as `aborted` with
`cause: user`. That pair is what renders `— turn interrupted`; `aborted` from any other cause still
reads `turn ended · aborted`.

### Tests that were vacuous and what replaced them

`16-focus.sh::tab_paints_a_focus_ring` (counted accent cells with no baseline, on a colour the
status line always paints) → `no_ring_before_tab` + a ring-glyph assertion. ·
`23-commands.sh::tab_completes_the_name_without_running_it` (`grep -q esc && exit 0; exit 0`) →
types an argument after Tab and asserts the composer reads `/help xyzzy`. ·
`23-commands.sh::every_listed_command_renders_something` and `the_four_no_ops_answer_or_say_why`
(screen diff, which a notice always changes) → assert `palette::NO_OUTPUT` never appears. ·
`19-interrupt.sh::the_stop_key_is_named_while_running` (`see "esc"` over a static hint) → the exact
phrase, with an idle baseline and an after-interrupt baseline. ·
`19-interrupt.sh::the_farewell_…` (`grep -i "bough\|bye"` matched the echoed `$BOUGH_BIN` path) →
`grep -F "bough: bye."`, exactly once. · `…_within_two_seconds` asserted `<= 6` → renamed
`quit_exits_cleanly_within_three_seconds` and tightened to 3. ·
`18-draft.sh::a_raw_three_line_paste_is_one_draft_and_one_send` (`-le 1`, so zero sends passed) →
`-eq 1`. · `20-frame.sh` cost (`grep "[$]\|—"`, and `—` is the failure) → asserts `—` with no
`usage/round` on the ledger and refuses an invented `$`; the positive half is a new live bullet in
`24-honesty.sh`. · `20-frame.sh::at_200x50_the_measure_is_capped_at_ninety` (`worst > 140` over
20-character fixture rows) → renders a 300-character paragraph and requires it to occupy four rows
or more. · `21-stream.sh::the_same_answer_renders_identically_after_a_relaunch` (could compare two
empty captures) → both captures asserted non-empty. ·
`24-honesty.sh::the_capability_answer_…` positive half (`grep -q tools` over a field
`RequestHeader` always carries) → parses the header and asserts the `tools` LIST is non-empty. ·
`ledger-sqlite/tests/checkpoint.rs` (hand-ran `checkpoint()`+`retire()` under a comment saying "what
the disposer does") → renamed, its WAL assertion made unconditional, and a second test added that
mounts the row in a kernel and calls `kernel.shutdown()`. · `ux2/run.sh`: `M13-rail` (no row wider
than 80 in an 80-column PTY — structurally unable to fail) → asserts the rail gave its columns
back; `B6-rowkeys` (`grep "notes.txt"`, already on screen from the walk's own prompt) → asserts the
ring, the row marker and a screen change on the toggle key; `M14-stopkey` → the exact phrase.

`lib.sh::tui_quit` sent ONE `Ctrl+u`, which cannot empty a multi-line draft, so `18-draft.sh` ended
by SENDING `/quit` as the third line of a message and being killed by the EXIT trap. It now clears
line by line. `lib.sh` gained `skip_all`, and the phase's nine new scripts print one SKIP line per
named bullet in the live half instead of one line for the whole script.

### Open items, honestly

**O1 — the FTS index built in Phase 1 is used by no surface.** `run_query` is a bounded in-memory
scan over rendered rows, which is the right fix for M11 (indexing ledger JSON is what put
`request/header {"as_of":53,…}` on screen) but leaves the index dead and the horizon at
`SearchConfig::window`. The pane now SAYS when it was windowed; making the FTS index carry rendered
text is a Phase 8 (digging) job and is not done here.

**O2 — `tool-actions` still has no Provider to reconcile against.** R7 gives it the event and the
disposal path, and `no_provider_means_no_tool_in_the_registry` still only exercises the empty case:
the positive branch of `kinds_with_providers` is executed by no test, because no `ActionProvider`
exists in the tree before Phase 6.

**O3 — `model-policy`'s `usage/round` writer is a channel-fed task owned by the row, not an awaited
append.** The bare `tokio::spawn` is gone (the task is an `effect_spawn`, failures are logged, and
the writer is registered before the stream tee so LIFO disposes it last), but the append is still
not awaited by the round that produced it: a process killed between the chunk and the write loses
that round's cost. Making it synchronous would put a ledger write on the streaming path.

**O4 — the live half of `make tui-test` still runs only `21-stream.sh` and `24-honesty.sh`.** The
other nine of the phase's scripts are replay-only by construction (layout, keys, swap gates). The
skip count is honest now; the coverage is unchanged.

**O5 — `?` is bound only on an EMPTY draft.** `hints()` and the status line advertise it flatly. A
`?` in a written sentence is a question mark, which is the right behaviour and a small dishonesty in
the hint text; the hint reads "this help, on an empty message".

## Close (partial): what was fixed, what was deferred

The close review returned **41 findings — 8 high, 21 medium, 12 low**, covering roughly 34 distinct
issues (several were filed twice: once against the crate that had the bug, once against the
shell-use bullet that could not catch it). Every one of them is addressed in the tree. Nothing was
left untouched and no test was deleted; five findings leave a residue, which is what "partial" in
the title means.

### Fixed (all 41)

**Product code — the thirteen decisions above.** R1 the prose measure is actually applied (#0, #17
second half) · R2 `ShellView::row_focus`/`following` are filled from a pane report (#5, #32) · R3 the
transcript reserves a focus-ring column (#17) · R4 `Field::StopKey` replaces the static
`esc = interrupt` hint (#20) · R5 the `config/reload` listener moved from the launcher into
`tui-shell` (#6) · R6 `shutdown_bounded` lost its unused `code` parameter (#40 second half) · R7
`actions/providers-changed` gives `tool-actions` a real reconcile-and-dispose path (#9, #34) · R8
`WorkspaceRoot::new` returns `Result` (#14) · R9 `run_query` returns `Found { hits, windowed }` and
the counter names the horizon (#11) · R10 `palette::echoed` writes `palette::NO_OUTPUT` and the
house-word lint moved onto `CommandsHandle::register` (#19, #39) · R11 `one_sentence` moved out of
the center into `tui-render` (#16) · R12 `StripConfig::gutter` deleted (#4) · R13 `Row::WakeMark`
carries `cause`, so a user's Esc renders `— turn interrupted` (#12).

**Also product code, outside R1-R13.** Tab reaches the open palette — `action_for` reads
`KeyContext::palette_open` and `on_key` consults the palette before `CycleFocus`, so
`PaletteAction::Complete` is live (#1, #18) — `keymap.rs::the_chords_mean_one_thing_each_from_every_context` was the one stale assertion this close had to repair: it asserted `CycleFocus` for Tab from EVERY context, which the fix deliberately breaks, and it now encodes the real contract (Tab reaches the palette while it is open, cycles panes otherwise) · the status line's running/elapsed state is re-derived
from `TuiHandle::running()` and the `AgentWake` listener filters on `ev.agent` against the focused
agent (#2, #23) · `spinner_ms` is read: `StatusPane::tick` advances one frame per `spinner_ms`, not
one per shell tick (#3, #35) · `tui-status` registers `invariant::forget` as a `defer_sync`, like
both sibling rows (#7) · `model-policy`'s `usage/round` writer is an `effect_spawn` task fed by a
channel, registered before the stream tee so LIFO disposes it last, failures logged (#8, #33) ·
`TuiShellPlugin::validate` now rejects all eight fields the phase added (#10) · the status row uses
`bough_util::home_dir()` (#13) · `PolicyConfig::validate` rejects a non-finite, negative or absurd
price (#15) · `?` is bound in `action_for` (#24) · the duplicated doc line in `run.rs` is gone (#40).

**Tests that could not fail, replaced.** `16-focus.sh` ring (#17) · `23-commands.sh`
`tab_completes_…`, `every_listed_command_renders_something`, `the_four_no_ops_…` (#19) and the
palette's "moves" half, now a cell diff over the selection (#27) · `19-interrupt.sh` stop key (#20),
farewell (`grep -F "bough: bye."`, exactly once) and `quit_exits_cleanly_within_three_seconds` (#26)
· `ux2/run.sh` `M13-rail` (#21) and `B6-rowkeys` (#22) · `18-draft.sh` `-le 1` → `-eq 1` (#25) ·
`20-frame.sh` cost (#28) and the 90-column measure bullet (#28, #0) · `ledger-sqlite`
`tests/checkpoint.rs`, renamed, its WAL assertion made unconditional and a second test added that
mounts the row and calls `kernel.shutdown()` (#29) · `24-honesty.sh` capability half, which now
parses the header and asserts the `tools` LIST is non-empty (#30) · `21-stream.sh`, both captures
asserted non-empty (#31) · `lib.sh::tui_quit` clears a multi-line draft line by line, and `skip_all`
prints one SKIP line per named bullet (#36, #38) · the verification map's wrong test names corrected
(#37).

### Deferred — the residue of five findings

1. **`make ux2` (V11, the three-persona LIVE re-audit) was not re-run after this review pass.**
   `M13-rail`, `B6-rowkeys` and `M14-stopkey` were rewritten here because they could not fail, and
   `docs/ux-audit-2.md` still records those three as fixed on the OLD evidence. The rewritten gate
   needs a live run to re-confirm them. This is the one deferral that could still be hiding a real
   product bug.
2. **O1 (#11) — the Phase 1 FTS index is used by no surface.** `run_query` is a bounded in-memory
   scan. The horizon is no longer silent and is pinned by
   `a_full_window_says_so_and_a_short_one_does_not`, but making the index carry rendered text is a
   Phase 8 (digging) job.
3. **O2 (#9, #34) — `tool-actions` has the event and the disposal path, and no Provider to
   reconcile against.** The positive branch of `kinds_with_providers` is executed by no test until
   Phase 6 lands an `ActionProvider`.
4. **O3 (#8, #33) — the `usage/round` append is owned but not awaited.** A process killed between
   the chunk and the write loses that round's cost. Making it synchronous would put a ledger write
   on the streaming path.
5. **O4 (#38) — the live half of `make tui-test` still runs only `21-stream.sh` and
   `24-honesty.sh`.** The skip count is honest now; the coverage is unchanged.
6. **O5 (#24) — `?` is bound only on an EMPTY draft**, while `hints()` and the status line advertise
   it flatly. A `?` inside a written sentence is a question mark, which is the right behaviour and a
   small dishonesty in the hint text.

### Tests marked `#[ignore]` by this close

None.
