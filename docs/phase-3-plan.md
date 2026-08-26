# Phase 3 — the TUI: design and work breakdown

REQUIREMENTS §17 Phase 3, resting on §11 (the whole of it), §14 (the old-feed adapter), §5's
catch-up and projection paragraphs, §9's tools paragraph (render intents), §13's stack, and §0
throughout. Phase 0 built the center, Phase 1 the ledger and the projection seam, Phase 2
everything between a message and a model call and back. This phase builds the only surface:

```
bough-tui-app (bundle)

  commands (Definition, ctx.commands) ──────────── consumers: tui-shell's built-ins,
      parse → resolve → dispatch, no model turn                Phase 6's `bough mcp call`

  tui-shell (Provider of ctx.tui) ── terminal, event loop, layout slots, composer,
      selection + OSC52 copy, mouse routing, panic/boot terminal restore
        │
        ├── tui-strip     (pane, Slot::Strip)  agent rail: glyphs + about-lines, click-to-focus
        ├── tui-focus     (pane, Slot::Main)   streaming turns, expandable tool calls, scrollback
        └── tui-search    (pane, Slot::Aux)    FTS across trajectories, clickable hits  ← SWAP subject
                                    ↑
                              tui-render (library, no row): the three render intents,
                              similar diffs, syntect+two-face highlighting, wrapping

  residents  ── §5 catch-up at launch: resume every agent row, ONE catch-up wake over queued mail
  old-feed   ── §14's throwaway adapter: ~/.jungler/jungler.db + ~/.bough/bough.db → mail,
                interim tier-1 rollups, and a priming query that is NEVER mail
```

The phase-shaped rules, restated because every work package below is judged by them:

1. **The TUI drives `ctx.agents` and reads `ctx.ledger` / `ctx.projection`. No pane, no command
   and no adapter may import `bough-plugin-agent-loop`.** A pane that needs loop behaviour asks
   for a seam method (this phase adds two, §2.5) rather than reaching around the seam.
2. **Rendering is a pure function of state the pane already holds.** `Pane::render` is
   synchronous, does no I/O, reads no clock and awaits nothing; panes keep their state current
   from listeners.
3. **Model-visible ⟺ ledgered still holds.** Nothing this phase renders becomes model-visible: a
   slash command's output goes to the pane, `command_history` goes to a priming query, and the
   one thing that DOES become model-visible — old-feed mail and its interim tier-1 rollups —
   goes through `mail/delivered` steps and `seal_rollup`, exactly like Phase 6's collectors will.
4. **The terminal is restored on every exit path**: clean quit, boot failure, panic, SIGINT.

This document is normative for signatures. An implementer may add private items freely and may not
change a signature here without editing this document first. Everything is `Send + Sync + 'static`;
one tokio runtime.

The cutover gate — one full real workday through the new TUI — is **Andrey's act**. It is recorded
in `BUILD.md` as a manual gate. No test in this document claims it and no work package may.

---

## 1. Crates

Seven new crates under `plugins/`, one of which (`tui-render`) has no catalog row, plus edits to
four Phase-2 crates and to the launcher. §15 item 6's granularity review applies at phase close:
`tui-render` folds into `tui-shell` if it never grows a second consumer beyond the two panes.

| crate (`plugins/…`) | package | catalog row(s) | provides | injects | role |
|---|---|---|---|---|---|
| `commands` | `bough-plugin-commands` | `commands` | `commands` | — | **Definition** §11: the human-command registry, `parse`, scoped resolution, `dispatch`, `commands/dispatched` |
| `tui-shell` | `bough-plugin-tui-shell` | `tui` | `tui` | `agents`, `ledger`, `commands` | **Provider** §11: terminal, event loop, layout slots, composer, selection/OSC52, mouse routing, terminal restore; **Consumer** of `commands` for `/help`, `/quit`, `/focus`, `/agents` |
| `tui-render` | `bough-plugin-tui-render` | — (library) | — | — | **Library**: the three render intents as pure `Vec<Line>` functions, `similar` diffs, `syntect`+`two-face` highlighting, wrapping and fold geometry |
| `tui-strip` | `bough-plugin-tui-strip` | `tui.strip` | — | `tui`, `agents`, `ledger` | **Consumer** §11: the agent rail — state glyphs, about-lines, click-to-focus |
| `tui-focus` | `bough-plugin-tui-focus` | `tui.focus` | — | `tui`, `agents`, `ledger`, `llm` | **Consumer** §11: the focused agent's chat/trajectory — streaming turns, clickable expanding tool calls, scrollback |
| `tui-search` | `bough-plugin-tui-search` | `tui.search` | — | `tui`, `ledger` | **Consumer** §11/§17: FTS over trajectories, clickable hits. The row the swap test disables |
| `residents` | `bough-plugin-residents` | `residents` | — | `agents`, `ledger` | **Consumer** §5: resume every agent row at launch, bootstrap a first lane, ONE catch-up wake per agent over queued mail |
| `old-feed-adapter` | `bough-plugin-old-feed-adapter` | `old-feed` | `old_feed` | `agents`, `ledger` | **§14's one sanctioned compatibility row**, explicitly throwaway: jungler events → cited mail with watermarks; `nodes.summary` / `lane_story` → interim tier-1 rollups; `command_history` → a priming query, never mail |
| `tui-probe` | `bough-plugin-tui-probe` | `tui.probe`, `tui.never` | — | `tui` (and, for `tui.never`, a key nobody provides) | **Fixture**, in the catalog and in NO bundle (the `projection-probe` precedent): a pane that panics on demand, a deterministic fixture pane, and a row that can never activate |

Edited Phase-2 crates (owner: **WP-1 only**, so every other package's file set stays disjoint):

- `plugins/agents` — `Agent::deliver` (§2.5), `Agent::request_wake` + `AgentDriver::wake_now`,
  `initiator` set around a wake.
- `plugins/agent-loop` — implements `wake_now`; wraps each wake in `initiator::with`.
- `plugins/agent-loop-scripted` — implements `wake_now`.
- `plugins/llm-replay` — an optional per-chunk `delay_ms`, so a replayed stream can be observed
  arriving rather than landing whole (V1).

Edited center (owner: **WP-7 only**): `crates/bough/src/main.rs` (log to a file, not stderr, when
the process owns a terminal), `crates/bough/src/boot.rs` (tear down BEFORE printing the
boot-failure report, so the report survives leaving the alt screen), `crates/bough/Cargo.toml`
(catalog links), `Makefile` (`tui-test`), `bundles/bough-tui-app.yml`, `profiles/tui.yml`.

New workspace dependencies, all named by §13:

```toml
ratatui-textarea = "0.9"                       # the composer (§13's "the ratatui org's textarea")
similar          = "3"                         # diff rendering
syntect          = { version = "5", default-features = false, features = ["default-fancy"] }
two-face         = { version = "0.5", default-features = false, features = ["syntect-default-fancy"] }
arboard          = "3"                         # OS clipboard, best effort
insta            = "1"                         # dev-only, TestBackend snapshots, sparingly
```

`ratatui` 0.30 and `crossterm` 0.29 (`event-stream`, `osc52`) are already in
`[workspace.dependencies]` from Phase 0 and are not touched. `ratatui-textarea` 0.9.2 builds on
`ratatui-core` ^0.1.1 / `ratatui-widgets` ^0.3.1, which is what `ratatui` 0.30.2 depends on, so
one `ratatui-core` unifies (P3-D1). `syntect`/`two-face` are pinned to the fancy-regex path so no
C `onig` enters the build; the exact feature spelling is re-checked when WP-3 first compiles.

**Dependency direction.** `tui-shell` depends on `commands`, `agents`, `ledger`. The three pane
crates depend on `tui-shell` (for the pane API), `tui-render`, `agents`, `ledger`. Nothing depends
on a pane crate. `tui-render` depends on `tools` (for `RenderIntent` and `ToolResultBody`) and
`ratatui` and on nothing else. `old-feed-adapter` and `residents` depend on `agents` and `ledger`
and know nothing about the TUI — both are usable headless, and that is deliberate: §5's catch-up
and §14's bridge are not terminal features.

---

## 2. Public API

### 2.1 The shell seam (`plugins/tui-shell/src/…`)

```rust
// lib.rs
pub struct Tui;
impl ServiceKey for Tui {
    type Value = TuiHandle;
    const NAME: &'static str = "tui";
}

#[derive(Clone)]
pub struct TuiHandle(pub Arc<TuiInner>);

impl TuiHandle {
    /// Register a pane. An EFFECT (§0.2): the returned disposer removes the pane from its slot,
    /// drops its hit map and requests a redraw, so a pane row unloading reflows the layout with
    /// no restart (the SWAP gate).
    pub async fn register_pane(
        &self,
        ctx: &Context,
        spec: PaneSpec,
    ) -> Result<EffectHandle, PluginError>;

    /// Every live pane, sorted by (slot, order, id). Stable across frames.
    pub fn panes(&self) -> Vec<PaneInfo>;

    pub fn focused_agent(&self) -> Option<AgentId>;
    pub fn focused_pane(&self) -> PaneId;
    /// Moves focus and emits `tui/focus`. `step` is a request the focus pane consumes.
    pub async fn focus(&self, req: FocusRequest);
    pub async fn focus_pane(&self, pane: PaneId);

    /// Coalesced: many calls in one frame budget cost one frame.
    pub fn redraw(&self);
    /// One-line transient message in `Slot::Status`.
    pub fn notify(&self, text: impl Into<String>);
    /// OSC52 to the terminal + `arboard` when configured. Never fails the caller.
    pub async fn copy(&self, text: &str) -> CopyOutcome;

    pub fn size(&self) -> Rect;
    pub fn backend(&self) -> Backend;
    /// The last rendered buffer. The selection reads from it; tests assert against it.
    pub fn last_frame(&self) -> Arc<ratatui::buffer::Buffer>;
    /// Ask the process to end. Delegates to `Kernel::request_exit` (P2-D23): the launcher still
    /// owns teardown, and teardown is what restores the terminal.
    pub fn quit(&self, code: u8);
}
```

```rust
// pane.rs
bough_util::brand_id! { pub struct PaneId; }
bough_util::brand_id! { pub struct HitId; }

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug,
         serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// Left rail, full height.
    Strip,
    /// The rest of the width: the focused agent.
    Main,
    /// Under `Main`: search, and Phase 8's preview/timeline/drift.
    Aux,
    /// One line above the composer: toasts, key hints, the composition fingerprint.
    Status,
}

/// How much of its slot a pane asks for. A slot whose panes are all gone takes ZERO rows/columns.
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub enum SlotSize {
    Cells(u16),
    Percent(u16),
    /// Share of what is left, by weight.
    Fill(u16),
}

pub struct PaneSpec {
    pub id: PaneId,
    pub slot: Slot,
    /// Ties are broken by id, so two rows in one slot lay out deterministically.
    pub order: i32,
    pub size: SlotSize,
    pub title: String,
    /// `false` ⇒ never takes keyboard focus (the status line).
    pub focusable: bool,
    pub pane: Arc<dyn Pane>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub id: PaneId,
    pub slot: Slot,
    pub order: i32,
    pub size: SlotSize,
    pub title: String,
    pub focusable: bool,
    /// The row that registered it. The shell's invariant reads this.
    pub owner: EntryId,
}

#[async_trait::async_trait]
pub trait Pane: Send + Sync + 'static {
    /// SYNCHRONOUS and non-blocking: no I/O, no clock, no `block_on`. Renders from state the pane
    /// already holds, and records this frame's clickable regions through `cx.hit`.
    fn render(&self, cx: &mut RenderCx<'_>);

    /// Input routed to this pane. Async, so a pane may call `ctx.agents` / `ctx.ledger`.
    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        PaneOutcome::Ignored
    }

    /// `("↑/↓", "scroll")` pairs for the status line and `/help`.
    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

pub struct RenderCx<'a> {
    pub frame: &'a mut ratatui::Frame<'a>,
    pub area: ratatui::layout::Rect,
    pub view: &'a ShellView,
    hits: &'a mut HitMap,
}

impl RenderCx<'_> {
    /// Record a clickable region for THIS frame. Later records win on overlap.
    pub fn hit(&mut self, rect: Rect, id: HitId);
    pub fn theme(&self) -> &Theme;
}

/// Read-only shell state handed to every render. `now` is passed in; a pane never reads a clock.
pub struct ShellView {
    pub focused_agent: Option<AgentId>,
    pub focused_pane: PaneId,
    /// Whether THIS pane has keyboard focus.
    pub is_focused: bool,
    pub selection: Option<Rect>,
    pub size: Rect,
    pub theme: Theme,
    pub now: DateTime<Utc>,
    pub composer_focused: bool,
}

#[derive(Clone, Debug)]
pub enum PaneEvent {
    Key(crossterm::event::KeyEvent),
    Click { at: (u16, u16), hit: Option<HitId>, button: MouseButton, clicks: u8 },
    Scroll { delta: i16 },
    /// Keyboard focus entered/left this pane.
    FocusChanged(bool),
    /// The focused agent changed, or a step focus was requested.
    Focus(FocusRequest),
    Tick,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneOutcome {
    /// Not mine: the shell tries the next handler (its own keymap).
    Ignored,
    /// Handled; redraw.
    Handled,
    /// Handled; the shell moves focus.
    Focus(FocusRequest),
    /// Handled; the shell dispatches this line through `ctx.commands`.
    Command(String),
    /// Handled; the shell puts this text in the composer.
    Compose(String),
}

pub struct PaneCx {
    pub ctx: Context,
    pub tui: TuiHandle,
    /// The focused agent's live handle, when there is one.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}
```

```rust
// events.rs — dispatch modes are part of the public contract (§0.2)
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FocusRequest {
    pub agent: Option<AgentId>,
    pub pane: Option<PaneId>,
    /// Scroll the trajectory so this step is visible and highlighted.
    pub step: Option<StepId>,
}

/// `tui/focus` — EMIT. A live mirror of shell state; nothing durable rides it (P2-D25).
pub struct TuiFocusEvent;
impl EmitEvent for TuiFocusEvent {
    const NAME: &'static str = "tui/focus";
    type Payload = FocusRequest;
}

#[derive(Clone, Debug)]
pub struct KeyDispatch {
    pub key: crossterm::event::KeyEvent,
    pub target: PaneId,
    pub composer_focused: bool,
    /// A listener that sets this to `true` consumes the key; the shell's keymap then skips it.
    pub handled: bool,
}

/// `tui/key` — WATERFALL. The extension point for a plugin that wants a keybinding without
/// touching the shell. Listeners MUST call `next()` to delegate.
pub struct TuiKeyEvent;
impl WaterfallEvent for TuiKeyEvent {
    const NAME: &'static str = "tui/key";
    type Value = KeyDispatch;
}
```

```rust
// term.rs — the one module that touches the real terminal.
pub struct TerminalGuard { /* private */ }

impl TerminalGuard {
    /// raw mode → alt screen → mouse capture → bracketed paste → hide cursor, in that order.
    /// Every step it completes is remembered, so `leave` undoes exactly what `enter` did.
    pub fn enter(cfg: &TuiConfig) -> Result<TerminalGuard, TuiError>;
}

/// Idempotent, synchronous, allocation-free, safe from a panic hook and safe to call twice:
/// show cursor → disable bracketed paste → disable mouse capture → leave alt screen →
/// disable raw mode. Guarded by a process-wide `AtomicBool`.
pub fn restore_now();

/// Chains to the previous hook AFTER `restore_now()`, so a panic message lands on the normal
/// screen. Returns an inverse that reinstalls the previous hook.
pub fn install_panic_hook() -> impl FnOnce();
```

```rust
// composer.rs
pub enum ComposerAction {
    None,
    /// Enter on a non-empty buffer that does not start with the command prefix.
    Send(String),
    /// Enter on a buffer that starts with the command prefix.
    Command(String),
    /// Esc on a non-empty buffer clears it; on an empty one the shell handles it.
    Cleared,
}

pub struct Composer { /* ratatui_textarea::TextArea<'static> */ }

impl Composer {
    pub fn new(cfg: &TuiConfig) -> Composer;
    /// Enter sends; Alt+Enter and Shift+Enter (on terminals that report it) insert a newline.
    pub fn on_key(&mut self, key: KeyEvent) -> ComposerAction;
    pub fn on_paste(&mut self, text: &str);
    pub fn height(&self, max: u16) -> u16;
    pub fn render(&self, cx: &mut RenderCx<'_>);
    pub fn set_text(&mut self, text: &str);
    pub fn text(&self) -> String;
}
```

```rust
// select.rs + clip.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection { pub anchor: (u16, u16), pub head: (u16, u16) }
impl Selection { pub fn rect(&self) -> Rect; }

/// Block select out of the LAST RENDERED BUFFER, per-line trailing spaces trimmed, `\n` joined.
/// The shell owns this rather than each pane: every pane draws into that buffer, and a per-pane
/// `select()` would duplicate layout knowledge (P3-D6).
pub fn text_from_buffer(buf: &Buffer, rect: Rect) -> String;

#[derive(Clone, Debug, PartialEq)]
pub enum CopyOutcome { Osc52AndLocal, Osc52Only, LocalOnly, Nothing(String) }

/// OSC52 first (crossterm's `clipboard::CopyToClipboard`, feature `osc52`), then `arboard` when
/// `clipboard: true`. An `arboard` failure is a `notify` line, never an error: a PTY has no
/// display server and must still copy (P3-D7).
pub async fn copy(text: &str, cfg: &TuiConfig, out: &mut impl std::io::Write) -> CopyOutcome;
```

```rust
// The row's config. Every deployment-varying value is here; nothing is a DEFAULT_ constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    /// `auto` (default): crossterm when stdout is a TTY, else the headless TestBackend, so
    /// `--check` and CI can mount the tui profile without a terminal (P3-D2).
    pub backend: Backend,
    /// Size of the headless backend. Ignored by crossterm.
    pub size: [u16; 2],
    /// Redraw coalescing budget.
    pub frame_ms: u64,
    /// Relative-time refresh; also the `PaneEvent::Tick` cadence.
    pub tick_ms: u64,
    pub theme: ThemeName,
    pub mouse: bool,
    pub osc52: bool,
    /// Best-effort `arboard` in addition to OSC52.
    pub clipboard: bool,
    pub composer_max_lines: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Backend { Auto, Crossterm, Headless }

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName { Dark, Light }

/// Named roles, not colours at call sites.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub bg: Color, pub fg: Color, pub dim: Color, pub accent: Color,
    pub evidence: Color, pub thought: Color, pub warn: Color, pub error: Color,
    pub added: Color, pub removed: Color, pub sel_bg: Color, pub hint: Color,
}
```

**The keymap** (fixed in code: a keymap is protocol between Andrey's fingers and the surface, and
`tui/key` is the extension point for anything else).

| key | effect |
|---|---|
| `Enter` | composer: send to the focused agent (`Agent::followup`), or dispatch a `/` line |
| `Alt+Enter` / `Shift+Enter` | newline in the composer |
| `Esc` | clear the composer; if empty, drop keyboard focus back to `Main` |
| `Tab` / `Shift+Tab` | cycle keyboard focus over focusable panes and the composer |
| `↑ ↓ PgUp PgDn Home End` | scroll the focused pane (the composer keeps them when it has focus) |
| `Ctrl+F` | focus the search pane (no-op when the row is disabled) |
| `Ctrl+C` | cancel the focused agent's running wake (`CancelCause::User`, `keep_inbox: true`); with nothing running, quit |
| `Ctrl+L` | force a full redraw |
| mouse: click | focus the pane, then the pane's `hit` (rail row → focus agent; tool-call header → toggle) |
| mouse: wheel | scroll the pane under the pointer, focus unchanged |
| mouse: drag | selection; release copies through OSC52 |

**The event loop** (`run.rs`), one task, spawned as the row's effect:

```
select! {
  crossterm EventStream event  -> route (key → tui/key waterfall → composer|pane|keymap;
                                          mouse → hit test → pane; resize → relayout; paste → composer)
  redraw notification          -> coalesce for frame_ms, then draw
  tick every tick_ms           -> PaneEvent::Tick to every pane, then draw
  effect halted                -> break
}
```
Drawing is: layout slots → for each pane `render` into a fresh `HitMap` → overlay the selection
highlight → publish `last_frame`. A panic inside a pane's render unwinds the loop task; the panic
hook has already restored the terminal, and the loop's `catch_unwind` asks the kernel to exit with
code 101 so the launcher tears the tree down (V8).

### 2.2 The commands seam (`plugins/commands/src/…`)

```rust
pub struct Commands;
impl ServiceKey for Commands {
    type Value = CommandsHandle;
    const NAME: &'static str = "commands";
}

bough_util::brand_id! { pub struct CommandName; }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandScope { Global, Agent(AgentName) }

pub struct CommandSpec {
    pub name: CommandName,
    /// One line, for `/help` and completion.
    pub summary: String,
    /// `"/focus <agent>"`.
    pub usage: String,
    /// Structured args, so Phase 6's `bough mcp call` can validate without a parser of its own.
    pub args: schemars::Schema,
    pub scope: CommandScope,
    pub run: Arc<dyn Command>,
}

#[async_trait::async_trait]
pub trait Command: Send + Sync + 'static {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub name: CommandName,
    /// The whole line as typed, prefix included.
    pub raw: String,
    /// Shell-style split of the remainder; quoted runs stay whole.
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    pub text: String,
    pub render: OutputRender,
    /// A command MAY cite; the pane renders cites under the output.
    pub cites: Vec<Cite>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputRender { Plain, KeyValue, Terminal }

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command `{name}`{}", suggestion_suffix(.did_you_mean))]
    Unknown { name: String, did_you_mean: Option<String> },
    #[error("usage: {usage}")]
    BadArgs { usage: String, detail: String },
    #[error("{0}")]
    Failed(String),
}

pub struct CommandCx {
    pub ctx: Context,
    /// The focused agent, when there is one. A command may steer/inject through it; that is
    /// durable (`inbox/spliced`) and is the ONLY way a command reaches a model.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}

impl CommandsHandle {
    /// An EFFECT: unloading the registering row removes the command (V5).
    pub async fn register(&self, ctx: &Context, spec: CommandSpec)
        -> Result<EffectHandle, PluginError>;
    /// Global commands plus the named agent's scoped ones; most-specific-wins on a name clash.
    pub fn list(&self, scope: Option<&AgentName>) -> Vec<CommandInfo>;
    pub fn resolve(&self, name: &CommandName, scope: Option<&AgentName>) -> Option<CommandSpec>;
    /// Resolve, validate args against the schema, run. Appends NO step, starts NO wake, and
    /// emits `commands/dispatched` when it returns.
    pub async fn dispatch(&self, inv: Invocation, cx: CommandCx)
        -> Result<CommandOutput, CommandError>;
}

/// PURE. `None` when the line does not start with the prefix; a doubled prefix (`//x`) is
/// literal text and yields `None`, so a message can begin with a slash.
pub fn parse(line: &str, prefix: char) -> Option<Invocation>;

/// `commands/dispatched` — EMIT, observability only.
pub struct CommandDispatched;
impl EmitEvent for CommandDispatched {
    const NAME: &'static str = "commands/dispatched";
    type Payload = DispatchRecord;   // { name, ok, scope, at }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    /// The command prefix. One character.
    pub prefix: char,
    /// Levenshtein suggestions on an unknown name.
    pub suggestions: bool,
}
```

The four built-ins live in `tui-shell` (it is the row that needs them), each a `CommandSpec`
registered as an effect: `/help` (commands + key hints, `OutputRender::KeyValue`), `/quit`,
`/focus <agent>`, `/agents` (name, status, traj, unconsumed mail count).

### 2.3 The render library (`plugins/tui-render/src/…`)

Pure functions over `ratatui::text::Line<'static>`; no state, no ctx, no I/O.

```rust
pub struct ToolCallView<'a> {
    pub name: &'a str,
    pub intent: RenderIntent,            // bough_plugin_tools::RenderIntent
    pub args: &'a serde_json::Value,
    pub result: Option<&'a ToolResultBody>,
    pub expanded: bool,
    pub width: u16,
    pub theme: &'a Theme,
}

/// The collapsed header: `▸ bash  ls -la …            ✓ 0.4s`. Always exactly one line.
pub fn tool_header(v: &ToolCallView<'_>) -> Line<'static>;

/// The expanded body, per §9's declared intent. `max_lines` folds the tail with a `… N more`
/// marker rather than truncating silently.
pub fn tool_body(v: &ToolCallView<'_>, max_lines: usize) -> Vec<Line<'static>>;

/// GENERIC: sorted key/value block over the args object, then the result content, wrapped.
pub fn generic_block(args: &serde_json::Value, result: Option<&ToolResultBody>, width: u16,
                     theme: &Theme) -> Vec<Line<'static>>;

/// TERMINAL: monospace output, ANSI stripped, with the exit-code / failure line.
pub fn terminal_block(content: &str, result: Option<&ToolResultBody>, width: u16,
                      theme: &Theme) -> Vec<Line<'static>>;

/// DIFF: `similar::TextDiff::from_lines`, unified hunks with ±  gutters, each line syntax
/// highlighted by the path's extension.
pub fn diff_block(spec: &DiffSpec, width: u16, theme: &Theme) -> Vec<Line<'static>>;

#[derive(Clone, Debug, PartialEq)]
pub struct DiffSpec { pub path: Option<String>, pub before: String, pub after: String }

/// The ARGS CONVENTION a `RenderIntent::Diff` tool must satisfy, in this order (P3-D9):
///   `{path, old, new}` | `{path, old_string, new_string}` | `{path, content}` (whole-file add).
/// `None` ⇒ the renderer falls back to `generic_block` with a dim note, never to nothing.
pub fn diff_spec_from_args(args: &serde_json::Value) -> Option<DiffSpec>;

/// syntect + two-face, fancy-regex, loaded once through a `OnceLock`. An unknown extension
/// returns unstyled lines rather than guessing.
pub fn highlight(code: &str, ext: Option<&str>, theme: &Theme) -> Vec<Line<'static>>;

/// Assistant text: wrap at `width`, style `**bold**` and `` `code` ``, and highlight fenced
/// blocks through `highlight`. No termimad in this phase (P3-D10).
pub fn markdownish(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>>;

/// Grapheme-aware wrapping used by all of the above.
pub fn wrap(text: &str, width: u16) -> Vec<String>;
```

### 2.4 The panes

**`tui-strip`** (`Slot::Strip`). One block per live agent:

```
● sol            running
  state: rebased the loop onto the new header rule  [s41,s44]
  intent (self-declared): finish the swap gate
```

```rust
pub struct StripConfig {
    pub width: u16,
    pub show_about: bool,
    pub about_lines: u16,
    /// Refresh cadence for counters the ledger owns (unconsumed mail).
    pub refresh_ms: u64,
}

/// PURE, unit-tested: status + pending wake ⇒ glyph and style role.
pub fn glyph(status: Status, wake_pending: bool, disposed: bool) -> (char, &'static str);

/// The strip reads step kind `"about/line"` BY NAME out of the ledger and renders `state` /
/// `intent` from the body; it does not depend on `bough-plugin-about-line` (P3-D11). The intent
/// half is always rendered under its label, never as truth (§2).
pub struct AboutView { pub state: String, pub intent: String, pub cites: Vec<Cite> }
pub fn about_from_step(step: &Step) -> Option<AboutView>;
```

Click on a rail row ⇒ `PaneOutcome::Focus(FocusRequest { agent: Some(id), .. })`.
State is kept current by listeners on `agent/status`, `agent/created`, `agent/disposed`,
`agent/wake` and `ledger/step` (for `about/line`).

**`tui-focus`** (`Slot::Main`). The focused agent's chat/trajectory.

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Mail { step: StepId, from: String, subject: String, class: MailClass },
    Andrey { step: StepId, text: String },
    Text { step: StepId, wake: WakeId, index: u32, text: String },
    Reasoning { step: StepId, text: String },
    Tool { call: ToolCallId, name: String, intent: RenderIntent,
           args: serde_json::Value, result: Option<ToolResultBody>, call_step: StepId },
    WakeMark { step: StepId, wake: WakeId, phase: Phase, reason: Option<String> },
    About { step: StepId, view: AboutView },
    Other { step: StepId, kind: StepType },
}

/// PURE: the whole projection of a trajectory into rows. `tool/call` and `tool/result` fold into
/// ONE `Row::Tool` by call id; envelope steps (`step/start`, `request/header`, `inbox/spliced`)
/// fold into their neighbours or are dropped. Unit-tested against a fixture step list.
pub fn rows_from_steps(steps: &[Step]) -> Vec<Row>;

/// The live tail that has streamed but not yet flushed to `thought/text`.
#[derive(Clone, Debug, Default)]
pub struct LiveText { pub agent: Option<AgentId>, pub text: String }

/// PURE, and the rule that makes streaming flicker-free (P3-D12): the durable `thought/text`
/// steps of a step index concatenate to a prefix of what streamed, so the trailing step renders
/// `live` whenever `live.len() >= durable.len()`, and the durable text otherwise.
pub fn trailing_text<'a>(durable: &'a str, live: &'a str) -> &'a str;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    /// Pinned to the bottom; new steps keep it pinned.
    Follow,
    /// Anchored to a step: new steps DO NOT move the viewport (V3).
    Anchored { top: usize, offset: u16 },
}

pub struct FocusConfig {
    pub max_rows: usize,        // rows held in memory; older paged from the ledger on demand
    pub max_tool_lines: usize,  // fold marker past this
    pub page_lines: u16,
    pub expand_new_tools: bool,
    pub show_reasoning: bool,
}
```

Live streaming (`stream.rs`): a `llm/stream` waterfall listener registered as an effect. It reads
`bough_plugin_agents::initiator::current()` for attribution, calls `next(value)` so the adapter
fills the slot, then `take()`s the stream and puts back a wrapper that appends every
`Chunk::TextDelta` into `LiveText` and calls `TuiHandle::redraw`. It replaces nothing and
short-circuits nothing; if the initiator is absent it delegates untouched. Clearing is driven by
`agent/step` `Phase::Start` and `agent/wake` `Phase::End`.

Hit map: every tool header line records `HitId::new(format!("tool:{call_id}"))`; a click toggles
membership of `expanded` and redraws. Scroll: wheel/`↑↓`/`PgUp`/`PgDn` move `Scroll`; `End` and
scrolling to the bottom re-arm `Follow`. A `FocusRequest { step: Some(id) }` sets
`Scroll::Anchored` on that row and flashes it with `theme.accent`.

**`tui-search`** (`Slot::Aux`).

```rust
pub struct SearchConfig {
    pub height: u16,
    pub limit: usize,
    pub debounce_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HitRow {
    pub agent: Option<AgentName>,   // resolved traj → agents row; `None` for a rowless traj
    pub traj: TrajId,
    pub step: StepId,
    pub seq: Seq,
    pub kind: StepType,
    pub snippet: String,
}

/// PURE: `SearchHit` + the agents rows ⇒ display rows (agent, seq, kind, snippet).
pub fn hit_rows(hits: &[SearchHit], agents: &[AgentRow]) -> Vec<HitRow>;
```

A one-line input the pane owns (the composer belongs to the shell), debounced by
`debounce_ms`, running `LedgerStore::search(SearchQuery { text, trajs: vec![], limit })`. An FTS
syntax error renders inline in `theme.error` and clears the result list. Each row records
`HitId::new(format!("hit:{step_id}"))`; a click returns
`PaneOutcome::Focus(FocusRequest { agent, step: Some(step), pane: Some(focus_pane) })`.

### 2.5 Catch-up at launch (`plugins/residents`), and the two seam methods it needs

§5's "on lid-open each active agent does a catch-up wake over queued mail", with TUI launch as the
lid-open proxy (§13: there is no lid notification on macOS; the `sleep-listener` row arrives in
Phase 7 and will call the same method).

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentsConfig {
    /// Agent names to CREATE when the ledger has no row for them. Empty ⇒ create nothing.
    pub bootstrap: Vec<String>,
    /// Trajectory id prefix for a bootstrapped agent: `lane/` + name.
    pub traj_prefix: String,
    /// Resume every `agents` row at launch and hold its disposer.
    pub resume_all: bool,
    /// Run §5's catch-up wake once the roster is up.
    pub catch_up: bool,
}

/// PURE: which agents get a catch-up wake, given the roster and each one's unconsumed mail.
/// Empty for an agent with nothing queued — that is V6's "and none when nothing is queued".
pub fn catch_up_set(roster: &[(AgentName, usize)]) -> Vec<AgentName>;
```

The row holds every resumed agent's `AgentDisposer` inside its own effect, so disabling
`residents` by patch tears the roster down and leaves the ledger untouched. It waits for the
`agents` factory slot before resuming (the `exec` row's `wait_for_factory` precedent: row order
carries no load semantics).

Two additions to the Phase-2 seams, both owned by WP-1:

```rust
// plugins/agents/src/agent.rs
impl Agent {
    /// §5's catch-up / schedule entry point. Opens ONE wake of `kind` if there is anything to
    /// process, and does nothing at all otherwise. Never appends a synthetic message.
    pub async fn request_wake(&self, kind: WakeKind, cause: WakeCause) -> WakeRequest;

    /// DELIVERED mail (§3, §5): appends `mail/delivered` (EVIDENCE, cited) and then splices the
    /// message carrying that step's seq, so the pair can never be half-written by a producer.
    /// This is what Phase 6's collectors will use; the old-feed adapter is its first caller.
    pub async fn deliver(&self, mail: Delivery) -> Result<InboxReceipt, AgentError>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakeCause { CatchUp, Schedule(&'static str) }

#[derive(Clone, Debug, PartialEq)]
pub enum WakeRequest { Started(WakeId), Nothing }

pub struct Delivery {
    pub from: Sender,
    pub class: MailClass,
    pub subject: String,
    pub summary: String,
    pub text: String,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}

// plugins/agents/src/factory.rs
#[async_trait::async_trait]
pub trait AgentDriver: Send + Sync + 'static {
    // …the four Phase-2 methods…
    /// Both drivers implement it. `agent-loop`: `Nothing` unless `pending(NextWake)` is non-empty
    /// or unconsumed ordinary mail exists; otherwise one wake with the oldest queued item as
    /// trigger. `agent-loop-scripted`: one scripted wake if the transcript has one left.
    async fn wake_now(&self, kind: WakeKind, cause: WakeCause) -> WakeRequest;
}
```

`agent-loop` additionally wraps each wake body in `bough_plugin_agents::initiator::with(agent_id, …)`.
§2 already sanctions the ambient initiator as attribution; Phase 2 defined it and set it nowhere,
which is why the focus pane's `llm/stream` tee had no way to name the agent it was watching.
Waterfall listeners run inline in the dispatching task (`run_chain` is awaited), so the task-local
is visible to them.

### 2.6 The old-feed adapter (`plugins/old-feed-adapter`) — §14, throwaway by design

```rust
pub struct OldFeed;
impl ServiceKey for OldFeed {
    type Value = OldFeedHandle;
    const NAME: &'static str = "old_feed";
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OldFeedConfig {
    /// `!!expr home_path(".jungler/jungler.db")`. MAY BE ABSENT (§14, AGENTS.md).
    pub jungler_db: PathBuf,
    /// `!!expr home_path(".bough/bough.db")`. Opened READ-ONLY, always.
    pub bough_db: PathBuf,
    /// The adapter's OWN watermark store, `!!expr bough_path("old-feed.db")` (P3-D13).
    pub state_db: PathBuf,
    pub poll_ms: u64,
    pub batch: usize,
    /// Which agent receives jungler mail until Phase 5's `mail-router` exists (P2-D17's shape).
    pub deliver_to: String,
    pub priming_limit: usize,
    /// Seal `nodes.summary` / `lane_story` rows as interim tier-1 rollups.
    pub tier1: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FeedProbe {
    Present { tables: Vec<String>, missing_columns: Vec<String> },
    Missing,
    Unreadable(String),
}

/// Reads `sqlite_master`. NEVER an error: an absent or unreadable jungler db means the jungler
/// half is disabled, one line is logged, and the row still ACTIVATES (§14, V7).
pub fn probe(path: &Path) -> FeedProbe;

impl OldFeedHandle {
    /// §14's cheap win: command memory for PRIMING. Never mail, never a step, never a
    /// projection section in this phase.
    pub async fn prime(&self, q: &PrimingQuery) -> Result<Vec<CommandMemory>, OldFeedError>;
    /// `note_sections` as CITED EVIDENCE: each carries `Cite { ref: "note:<note>#<ord>" }`.
    pub async fn notes(&self, q: &NoteQuery) -> Result<Vec<NoteEvidence>, OldFeedError>;
    /// What the last sweep did. The `/oldfeed` command renders it.
    pub fn status(&self) -> FeedStatus;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimingQuery {
    pub repo: Option<String>,
    pub tags: Vec<String>,
    pub contains: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandMemory {
    pub cmd: String, pub tags: Vec<String>, pub repo: String,
    pub at: DateTime<Utc>, pub exit_code: Option<i64>, pub output_head: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NoteEvidence {
    pub note: i64, pub ord: i64, pub heading: String, pub body: String,
    pub author: String, pub cite: Cite,
}
```

**The expected jungler shape**, read defensively. `~/.jungler/jungler.db` does not exist on this
machine (BUILD.md's standing assumptions; jungler is a design repo, and its daemon was never
built), so the shape below is the CONTRACT this adapter reads and the fixture in its tests is
authoritative for it. Required columns are `id` and a timestamp; every other column is optional
and read as NULL. A source missing a required column is disabled with one logged line — never a
panic, never a boot failure.

```sql
events     (id INTEGER PK, at INTEGER, kind TEXT, subject TEXT, body TEXT, ref TEXT, url TEXT, lane TEXT)
nodes      (id INTEGER PK, kind TEXT, title TEXT, summary TEXT, updated_at INTEGER, lane TEXT)
lane_story (id INTEGER PK, lane TEXT, ord INTEGER, heading TEXT, body TEXT, updated_at INTEGER)
```

**Watermarks** (`state_db`, the adapter's own file — the ledger is append-only and its schema is
`ledger-sqlite`'s):

```sql
CREATE TABLE IF NOT EXISTS feed_watermarks (
  source     TEXT PRIMARY KEY,   -- 'jungler.events' | 'jungler.nodes' | 'jungler.lane_story'
  last_row   INTEGER NOT NULL,
  last_at    INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

Delivery is **at-least-once with a ref guard**: for each batch the adapter first asks the ledger
`StepQuery { refs: [jungler:event:<id> …], kinds: ["mail/delivered"] }` and drops rows already
delivered, then calls `Agent::deliver` (which writes the `mail/delivered` step citing
`Cite { ref: "jungler:event:<id>", url }`), then advances the watermark. A crash between the
append and the watermark write therefore cannot duplicate — the ref guard catches it on restart
(V7).

Interim tier-1 blocks: `nodes.summary` (non-empty) and `lane_story` rows become
`LedgerStore::seal_rollup(NewRollup { kind: RollupKind::Tier, tier: 1, body, notable_refs,
prompt_ver: "old-feed/1", .. })` on the receiving agent's trajectory, watermarked the same way.
Seal-once (§3) means a re-seal would be a violation, so the watermark plus the ref guard is what
keeps a restart clean. The projection consumes them through the ordinary tiers band — no
projection change is needed, which is the point of §17's "softening the no-tiers window".

`command_history` / `command_tags` are read for `prime()` only. The crate's invariant asserts the
rule rather than documenting it: **no step this row appends carries a `cmd:` / `bough:command:`
ref, and no `mail/delivered` step exists with two identical `jungler:event:` refs.**

### 2.7 Event catalog added by this phase

| event | dispatch | payload | owner |
|---|---|---|---|
| `tui/focus` | emit | `FocusRequest` | `tui-shell` |
| `tui/key` | waterfall | `KeyDispatch` | `tui-shell` |
| `commands/dispatched` | emit | `DispatchRecord` | `commands` |

Three new events; §15 item 7's ~30-event catalog gate is not reached by this phase.

### 2.8 Step types added by this phase

**None.** The TUI renders; it does not write. Every durable fact this phase produces uses a step
type an earlier phase owns: `mail/delivered` and `rollup/sealed` (ledger), `inbox/spliced`
(agents). A slash command's output is not a step (P3-D8), and `command_history` is not mail (§17).
V6 therefore identifies catch-up wakes by counting `wake/start` steps appended after the
`residents` row activates, not by a new field on `WakeStart` (P3-D14).

### 2.9 Bundle rows

`bundles/bough-tui-app.yml`:

```yaml
- id: commands
  plugin: commands
  config: { prefix: "/", suggestions: true }

- id: tui
  plugin: tui-shell
  config:
    backend: auto
    size: [120, 40]
    frame_ms: 16
    tick_ms: 250
    theme: dark
    mouse: true
    osc52: true
    clipboard: true
    composer_max_lines: 6

- id: tui.strip
  plugin: tui-strip
  config: { width: 34, show_about: true, about_lines: 2, refresh_ms: 1000 }

- id: tui.focus
  plugin: tui-focus
  config:
    max_rows: 2000
    max_tool_lines: 200
    page_lines: 20
    expand_new_tools: false
    show_reasoning: true

- id: tui.search
  plugin: tui-search
  config: { height: 12, limit: 50, debounce_ms: 150 }

- id: residents
  plugin: residents
  config:
    bootstrap: [sol]
    traj_prefix: "lane/"
    resume_all: true
    catch_up: true

- id: old-feed
  plugin: old-feed-adapter
  config:
    jungler_db: !!expr 'home_path(".jungler/jungler.db")'
    bough_db:   !!expr 'home_path(".bough/bough.db")'
    state_db:   !!expr 'bough_path("old-feed.db")'
    poll_ms: 30000
    batch: 200
    deliver_to: sol
    priming_limit: 40
    tier1: true
```

`profiles/tui.yml` is unchanged except that it stays the default profile. `tui-probe`,
`llm-replay` and `agent-loop-scripted` are in the catalog and in NO bundle; the tui-test patch
names them (the `ledger-memory` precedent).

---

## Work packages

Seven packages, file sets disjoint. The one shared file is the root `Cargo.toml`: each package
appends its own `bough-plugin-*` line to `[workspace.dependencies]`, and `plugins/*` already
covers the members glob. `crates/bough/Cargo.toml` (which links the catalog) is WP-7's alone, so
every package before it tests with `Catalog::from_parts(..)` rather than the full binary.

Order: WP-1 first (it is the only package that touches Phase-2 crates, and everything else
compiles against it), then WP-2 and WP-3 in parallel, then WP-4, WP-5 and WP-6 in parallel, then
WP-7.

### WP-1: the `commands` seam, and the Phase-2 seam edits

Files: `plugins/commands/**`, `plugins/agents/src/{agent.rs,factory.rs,mail.rs,lib.rs}`,
`plugins/agent-loop/src/{driver.rs,wake.rs}`, `plugins/agent-loop-scripted/src/lib.rs`,
`plugins/llm-replay/src/{transcript.rs,lib.rs}`, root `Cargo.toml` (one line).

§2.2 in full: `parse`, the scoped registry with most-specific-wins, `dispatch` with schema
validation, the typed `CommandError`, `commands/dispatched`, `src/invariant.rs` (name uniqueness
per scope; every dispatch resolves to a command that was registered at dispatch time). Then §2.5's
two seam methods: `Agent::deliver` (the `mail/delivered` + splice pair, one transaction's worth of
ordering: the step first, then the splice carrying its seq), `Agent::request_wake` +
`AgentDriver::wake_now` on both drivers, and `initiator::with` around each wake in `agent-loop`.
Finally `llm-replay`'s optional per-chunk `delay_ms` (default 0, `#[serde(default)]`), which is
what lets a replayed answer be OBSERVED arriving instead of landing whole.

Tests it must ship. `commands/tests/dispatch.rs`: `a_slash_line_parses_into_a_name_and_args`,
`a_doubled_prefix_is_literal_text_and_does_not_parse`, `an_unknown_name_reports_unknown_with_a_suggestion`,
`bad_args_report_the_usage_string`, `a_scoped_command_shadows_its_global_twin_for_that_agent_only`,
`unloading_the_registering_row_removes_the_command`, `a_dispatch_appends_no_step_and_starts_no_wake`
(a `ledger-memory` handle: zero rows before and after). `commands/src/invariant.rs::tests`: a
planted duplicate name is reported; a dispatch of an unregistered name is reported.
`agents/tests/deliver.rs`: `deliver_appends_mail_delivered_then_splices_carrying_its_seq`,
`delivered_mail_is_evidence_and_must_cite`, `an_undelivered_send_still_has_no_mail_seq`.
`agents/tests/wake_now.rs`: `request_wake_with_nothing_queued_starts_no_wake`,
`request_wake_with_queued_mail_starts_exactly_one`, `both_drivers_implement_wake_now` (parameterised
over the live and scripted drivers). `agent-loop/tests/flow.rs` gains
`the_initiator_is_set_for_the_whole_wake_including_the_llm_stream_waterfall`.
`llm-replay/tests/replay.rs` gains `a_delayed_round_yields_its_chunks_over_time`.

### WP-2: `tui-shell` — the terminal, the loop, the layout, the composer

Files: `plugins/tui-shell/**`, root `Cargo.toml` (three lines: the crate, `ratatui-textarea`,
`arboard`).

§2.1 in full: the `tui` key and `TuiHandle`, `PaneSpec`/`Pane`/`RenderCx`/`PaneEvent`/`PaneOutcome`,
the slot layout with the zero-space rule, `TerminalGuard::enter` + `restore_now` +
`install_panic_hook`, the async event loop over `crossterm::event::EventStream`, mouse routing and
hit testing, drag selection + `text_from_buffer` + OSC52/arboard copy, the composer over
`ratatui-textarea`, the keymap table, `tui/focus` and `tui/key`, the four built-in commands, the
`Backend::Auto` rule, and `src/invariant.rs` (every registered pane's owner row is still Active,
and no two panes share an id).

Tests: `tests/layout.rs` — `panes_lay_out_by_slot_then_order_then_id`,
`a_slot_with_no_panes_takes_no_space`, `removing_a_pane_reflows_the_remaining_ones`,
`a_resize_relayouts_without_losing_pane_state`. `tests/input.rs` —
`enter_on_plain_text_sends_a_followup_to_the_focused_agent` (a stub agent through a
`ledger-memory` tree), `enter_on_a_slash_line_dispatches_a_command_and_never_sends`,
`alt_enter_inserts_a_newline`, `a_tui_key_listener_that_sets_handled_consumes_the_key`,
`a_click_focuses_the_pane_under_the_pointer_and_forwards_its_hit`,
`a_wheel_event_scrolls_the_pane_under_the_pointer_without_moving_focus`.
`tests/select.rs` — `a_drag_rect_extracts_the_rendered_cells_with_trailing_space_trimmed`,
`copy_writes_an_osc52_sequence_carrying_the_selection` (asserted against a `Vec<u8>` writer),
`a_clipboard_failure_is_a_notice_not_an_error`. `tests/restore.rs` —
`restore_now_is_idempotent`, `the_panic_hook_restores_before_delegating`. One `insta` snapshot of
the empty three-slot layout, and no more.

### WP-3: `tui-render` — the three render intents

Files: `plugins/tui-render/**`, root `Cargo.toml` (three lines: the crate, `similar`, `syntect` +
`two-face`).

§2.3 in full, all pure: `tool_header`, `tool_body` dispatching on `RenderIntent`, `generic_block`,
`terminal_block`, `diff_block` over `similar::TextDiff`, `diff_spec_from_args` with the documented
args convention and the generic fallback, `highlight` over syntect+two-face behind a `OnceLock`,
`markdownish`, `wrap`. No ctx, no service key, no row; `src/lib.rs` states
`No runtime invariant:` with the reason (a pure library owns no event stream or data relation).

Tests: `tests/intents.rs` — `generic_renders_sorted_key_values_and_the_result_content`,
`terminal_renders_output_monospace_with_the_exit_code`, `terminal_strips_ansi_from_output`,
`diff_renders_added_and_removed_lines_with_the_theme_roles`,
`diff_highlights_by_the_paths_extension`, `a_diff_intent_with_unmatched_args_falls_back_to_generic`,
`a_body_over_max_lines_ends_in_a_fold_marker_not_a_truncation`,
`the_collapsed_header_is_exactly_one_line_at_every_width`. `tests/wrap.rs` — grapheme and CJK width
cases. `tests/args.rs` — `diff_spec_from_args` accepts each of the three documented shapes and
rejects a fourth, checked against `bough_plugin_tools_baseline::specs`' real `edit_file` and
`write_file` argument names so the convention cannot drift from the tools that declare `Diff`.
One `insta` snapshot of an expanded diff body.

### WP-4: `tui-strip` and `tui-focus` — the two agent-facing panes

Files: `plugins/tui-strip/**`, `plugins/tui-focus/**`, root `Cargo.toml` (two lines).

§2.4's first two panes. Strip: `glyph`, `about_from_step`, the rail rows, click-to-focus, the four
listeners. Focus: `rows_from_steps` (pure, the heart of the pane), `trailing_text`, the `Scroll`
state machine with the stability rule, the `llm/stream` tee keyed by the ambient initiator,
tool-call hit ids and expand/collapse, paging older rows in from the ledger, and rendering through
`tui-render`. Both `src/invariant.rs`: the strip's is "a rendered state half comes only from an
`about/line` step that cites at least one step" (§16's cited-truth rule at the surface); the
focus pane's is "no step is rendered twice — the live tail and the durable rows never overlap".

Tests: `tui-strip/tests/rail.rs` — `each_status_maps_to_its_glyph`,
`the_intent_half_is_always_rendered_under_its_label`, `an_agent_with_no_about_line_still_renders`,
`a_click_on_a_rail_row_returns_a_focus_outcome`. `tui-focus/tests/rows.rs` —
`a_call_and_its_result_fold_into_one_row`, `an_unanswered_call_renders_as_pending`,
`envelope_steps_do_not_produce_rows`, `mail_and_andrey_messages_render_as_their_own_rows`,
`an_unknown_step_type_renders_as_other_and_never_panics`. `tui-focus/tests/stream.rs` —
`live_deltas_render_before_the_durable_step_lands`,
`the_durable_step_replaces_the_live_tail_without_flicker`,
`a_stream_with_no_initiator_is_delegated_untouched`. `tui-focus/tests/scroll.rs` —
`new_steps_do_not_move_an_anchored_viewport`, `follow_re_arms_at_the_bottom`,
`page_down_past_the_end_clamps`. `tui-focus/tests/expand.rs` —
`clicking_a_tool_header_toggles_expansion`, `expansion_survives_new_steps_arriving`.

### WP-5: `tui-search` — the FTS pane

Files: `plugins/tui-search/**`, root `Cargo.toml` (one line).

§2.4's third pane: the one-line input, the debounce, `LedgerStore::search`, `hit_rows`, the
clickable hit rows, inline error rendering for a bad FTS query, and `src/invariant.rs` (every hit
row rendered names a step that still exists in the ledger). The row is deliberately small and
self-contained because it is the SWAP subject: nothing else may depend on it, and disabling it
must be indistinguishable from never having mounted it.

Tests: `tests/search.rs` — `a_query_returns_hits_across_two_trajectories`,
`hits_carry_the_agent_name_for_a_traj_with_an_agents_row`,
`a_rowless_trajectory_renders_without_an_agent_name`,
`a_bad_fts_query_renders_an_inline_error_and_clears_the_list`,
`the_debounce_collapses_a_burst_of_keystrokes_into_one_query`,
`clicking_a_hit_returns_a_focus_outcome_naming_the_step`.

### WP-6: `old-feed-adapter` — §14's throwaway bridge

Files: `plugins/old-feed-adapter/**`, root `Cargo.toml` (one line).

§2.6 in full: `probe`, the defensive column reader, the watermark store in its own db, the ref
guard, the sweep loop on `poll_ms` (an ordinary effect; `ctx.schedule` arrives in Phase 6 and this
row retires there anyway), `Agent::deliver` for events, `seal_rollup` for `nodes.summary` and
`lane_story`, the read-only `bough.db` priming and notes queries, a `/oldfeed` command showing the
last sweep, and `src/invariant.rs` (no `cmd:` ref on any appended step; no duplicate
`jungler:event:` ref across `mail/delivered` steps). The module comment must say, in one line,
that this crate is scheduled for deletion in Phase 6 and that `disabled: true` is its off switch.

Tests, all against fixture databases the tests build with `rusqlite`:
`tests/events.rs` — `events_become_cited_mail_on_the_configured_agent`,
`the_watermark_advances_past_the_last_delivered_row`,
`a_restart_delivers_nothing_twice`, `a_crash_between_the_append_and_the_watermark_still_delivers_once`
(the watermark is rolled back by hand and the sweep re-run).
`tests/tier1.rs` — `nodes_summary_rows_seal_as_tier_one_rollups`,
`lane_story_sections_seal_in_ord_order`, `a_second_sweep_seals_nothing_again`,
`the_projection_shows_them_in_the_tiers_band` (through `projection-assembler`, both ledger
providers). `tests/priming.rs` — `command_history_is_never_delivered_as_mail`,
`prime_returns_command_history_filtered_by_repo_and_tag`,
`notes_carry_a_cite_naming_the_note_section`. `tests/absent.rs` —
`an_absent_jungler_db_activates_the_row_and_logs_one_line`,
`an_unreadable_jungler_db_activates_the_row_and_logs_one_line`,
`a_missing_required_column_disables_that_source_only`.

### WP-7: integration — bundle, launcher, `make tui-test`, the shell-use suite

Files: `plugins/residents/**`, `plugins/tui-probe/**`, `bundles/bough-tui-app.yml`,
`profiles/tui.yml`, `crates/bough/src/{main.rs,boot.rs}`, `crates/bough/Cargo.toml`,
`crates/bough/tests/{tui_swap.rs,tui_boot.rs}`, `scripts/tui/**`, `Makefile`, `BUILD.md`,
root `Cargo.toml` (two lines).

`residents` (§2.5) with `catch_up_set`, the roster held as an effect, the factory wait, and its
invariant (at most one catch-up wake per agent per activation). `tui-probe`: the `tui.probe` pane
(a deterministic fixture pane that panics on a configured key and renders a known string) and the
`tui.never` row, which declares an injection nobody provides so an enabled row can be made to never
activate on purpose. Launcher: log to `bough_path("bough.log")` instead of stderr when stdout is a
TTY and no subcommand was given (P3-D3), and swap `boot.rs`'s two lines so `kernel.shutdown()`
runs BEFORE the unresolved-row report is printed — today the report is written into the alt screen
and then wiped by the restore. `make tui-test`: build `--release`, then run every
`scripts/tui/*.sh` against the binary with `BOUGH_HOME` pointed at a scratch dir and a generated
patch that swaps `llm.anthropic` for `llm-replay`; when `ANTHROPIC_API_KEY` is present in
`~/.bough/env`, run the whole suite a SECOND time with `BOUGH_LIVE=1` and no replay patch,
asserting a real streamed answer lands in the focus pane. `scripts/tui/lib.sh` gives the scripts
`t <name> <cmd…>` so every assertion prints `ok - <name>` / `not ok - <name>` and the suite exits
non-zero on the first failure — those names are the test names the verification map cites.

Tests: `crates/bough/tests/tui_swap.rs` (headless backend, through the launcher's own live
recompose, the `ledger_swap.rs` precedent) —
`the_tui_bundle_boots_with_all_five_rows_active`,
`disabling_the_search_row_removes_its_pane_and_reflows`,
`re_enabling_the_search_row_returns_the_pane`,
`the_retired_search_row_leaves_no_pane_no_listener_and_no_binding`.
`crates/bough/tests/tui_boot.rs` — `a_row_that_never_activates_fails_the_boot_after_teardown`,
`the_unresolved_report_is_printed_after_the_teardown`.
`plugins/residents/tests/catchup.rs` — `every_agent_row_is_resumed_at_launch`,
`one_catch_up_wake_per_agent_with_queued_mail`, `no_wake_when_nothing_is_queued`,
`bootstrap_creates_the_first_lane_only_when_the_ledger_has_none`,
`disabling_the_row_disposes_the_roster_and_leaves_the_ledger_untouched`.
Plus the nine shell-use scripts named in the verification map.

---

## 3. Verification map

Every bullet of the phase brief and of §17 Phase 3, against the test that proves it. A bullet with
no green named test is not done. Script assertions are named through `scripts/tui/lib.sh`'s `t`
helper and print `ok - <name>`; Rust tests are named `fn`s.

**V1 — boot, composer, streaming turn, ledger steps; and the same script live.**
`scripts/tui/01-boot-and-turn.sh`:
`the_tui_boots_into_a_strip_and_a_focus_pane`,
`the_composer_accepts_a_message_and_enter_sends_it`,
`the_answer_streams_in_before_it_is_complete` (waits for the first fragment while the rest of the
sentence is NOT yet on screen — this is what `llm-replay`'s per-chunk `delay_ms` from WP-1 exists
for), `the_whole_answer_is_on_screen_when_the_wake_ends`,
`the_turn_landed_as_ledger_steps` (`sqlite3 $BOUGH_HOME/ledger.db` after the script: one
`wake/start`, ≥1 `inbox/spliced`, ≥1 `thought/text`, one `wake/end`, in seq order),
`the_status_glyph_returned_to_idle`.
Live half, run by `make tui-test` when `ANTHROPIC_API_KEY` is set, same script under `BOUGH_LIVE=1`
with the replay patch omitted: `a_live_haiku_answer_streams_into_the_focus_pane` (the prompt asks
haiku to reply with a fixed token, and the assertion is that token plus ≥1 `thought/text` step).
Unit halves: `tui-shell/tests/input.rs::enter_on_plain_text_sends_a_followup_to_the_focused_agent`,
`tui-focus/tests/stream.rs::{live_deltas_render_before_the_durable_step_lands,
the_durable_step_replaces_the_live_tail_without_flicker}`.

**V2 — clicking a tool call expands and collapses it; each intent renders.**
`scripts/tui/02-tool-calls.sh`: `a_tool_call_renders_collapsed_on_one_line`,
`clicking_the_header_expands_it`, `clicking_again_collapses_it`,
`a_generic_intent_shows_a_key_value_block`,
`a_terminal_intent_shows_monospace_output_and_the_exit_code`,
`a_diff_intent_shows_added_and_removed_lines_in_colour` (`shell-use cells` asserts the fg colour of
a `+` line differs from a `-` line and from body text — that is the syntect/theme proof at the
screen).
Unit halves: `tui-render/tests/intents.rs` (all eight),
`tui-focus/tests/expand.rs::{clicking_a_tool_header_toggles_expansion,
expansion_survives_new_steps_arriving}`.

**V3 — scroll, scroll stability under streaming, drag-select + OSC52.**
`scripts/tui/03-scroll-and-copy.sh`: `the_wheel_scrolls_the_trajectory`,
`page_up_and_arrow_keys_scroll_the_trajectory`,
`the_viewport_does_not_move_while_new_steps_stream` (scroll up, start a second replayed turn, assert
the top visible line is byte-identical before and after),
`end_re_arms_follow_and_jumps_to_the_bottom`,
`a_drag_selection_is_highlighted`,
`the_release_emits_an_osc52_sequence_carrying_the_selected_text` (`shell-use get-recording` is
grepped for `\x1b]52;c;<base64>`, and the base64 is decoded and compared against the selected
cells).
Unit halves: `tui-focus/tests/scroll.rs` (three), `tui-shell/tests/select.rs` (three).

**V4 — the search pane.**
`scripts/tui/04-search.sh`: `ctrl_f_focuses_the_search_pane`,
`a_query_lists_hits_with_agent_and_step_id`,
`a_hit_names_the_agent_that_owns_it`,
`clicking_a_hit_focuses_that_step_in_the_trajectory` (the focused row is the one the hit named, and
it is flashed with the accent colour, asserted through `shell-use cells`),
`a_bad_query_reports_inline_and_clears_the_list`.
Unit halves: `tui-search/tests/search.rs` (all six).

**V5 — `ctx.commands` dispatches without a model turn.**
`scripts/tui/05-commands.sh`: `a_slash_command_renders_its_output_in_the_pane`,
`the_slash_command_started_no_wake` (the ledger has no new `wake/start`, `step/start` or
`request/header` after the dispatch — read with `sqlite3` before and after),
`an_unknown_command_reports_an_error_inline`,
`help_lists_the_registered_commands`.
Unit halves: `commands/tests/dispatch.rs::{a_dispatch_appends_no_step_and_starts_no_wake,
an_unknown_name_reports_unknown_with_a_suggestion, unloading_the_registering_row_removes_the_command,
a_scoped_command_shadows_its_global_twin_for_that_agent_only}`, and
`crates/bough/tests/tui_swap.rs::the_retired_search_row_leaves_no_pane_no_listener_and_no_binding`
for the "registrations are effects" half at the row level.

**V6 — catch-up at TUI launch.**
`scripts/tui/06-catch-up.sh`: `queued_mail_at_boot_produces_exactly_one_catch_up_wake_per_agent`
(the ledger is seeded with `mail/delivered` + `inbox/spliced` before the binary starts; after boot
exactly one new `wake/start` exists per agent trajectory), `the_catch_up_wake_consumed_the_queued_mail`
(`wake/end.consumed` covers the seeded seqs), `an_empty_inbox_produces_no_wake_at_all`.
Unit halves: `plugins/residents/tests/catchup.rs` (all five),
`agents/tests/wake_now.rs::{request_wake_with_nothing_queued_starts_no_wake,
request_wake_with_queued_mail_starts_exactly_one, both_drivers_implement_wake_now}`.

**V7 — the old-feed adapter.**
`plugins/old-feed-adapter/tests/events.rs::{events_become_cited_mail_on_the_configured_agent,
the_watermark_advances_past_the_last_delivered_row, a_restart_delivers_nothing_twice,
a_crash_between_the_append_and_the_watermark_still_delivers_once}` ·
`tests/tier1.rs::{nodes_summary_rows_seal_as_tier_one_rollups, lane_story_sections_seal_in_ord_order,
a_second_sweep_seals_nothing_again, the_projection_shows_them_in_the_tiers_band}` ·
`tests/priming.rs::{command_history_is_never_delivered_as_mail,
prime_returns_command_history_filtered_by_repo_and_tag, notes_carry_a_cite_naming_the_note_section}` ·
`tests/absent.rs::{an_absent_jungler_db_activates_the_row_and_logs_one_line,
an_unreadable_jungler_db_activates_the_row_and_logs_one_line,
a_missing_required_column_disables_that_source_only}` ·
`src/invariant.rs::tests::{a_planted_command_ref_on_an_appended_step_is_a_violation,
a_planted_duplicate_jungler_ref_is_a_violation}` ·
and one screen-level assertion, `scripts/tui/07-old-feed.sh::jungler_mail_appears_in_the_focus_pane`,
so the bridge is proven at the surface and not only in a unit test.

**V8 — the terminal is restored on a boot failure and on a panic.**
`scripts/tui/08-restore.sh`: `a_row_that_never_activates_leaves_the_alt_screen_before_reporting`
(boots with a `--patch` that enables `tui.never`; asserts the shell's normal screen is back, the
cursor is visible, the shell prompt echoes typed characters again — i.e. raw mode is off — and the
unresolved-row report is READABLE on screen, which is the half `boot.rs`'s current print-then-
shutdown order gets wrong), `a_panic_inside_a_pane_restores_the_terminal_and_exits_non_zero`
(`tui.probe` panics on a configured key; the same three restoration assertions, plus a non-zero
exit code and the panic message visible on the normal screen).
Unit halves: `tui-shell/tests/restore.rs::{restore_now_is_idempotent,
the_panic_hook_restores_before_delegating}`,
`crates/bough/tests/tui_boot.rs::{a_row_that_never_activates_fails_the_boot_after_teardown,
the_unresolved_report_is_printed_after_the_teardown}`.

**SWAP — disable the search pane row by patch while the TUI is running.**
`scripts/tui/09-swap-search.sh`: `the_search_pane_is_on_screen_before_the_patch`,
`writing_the_patch_removes_the_pane_without_a_restart` (the patch file under `$BOUGH_HOME` is
rewritten while the binary runs; the launcher's watch recomposes),
`the_remaining_panes_resized_to_fill_the_freed_rows`,
`removing_the_patch_returns_the_pane`,
`the_process_never_restarted` (the PID is unchanged and the conversation above is still on screen).
Rust half, through the launcher's own recompose:
`crates/bough/tests/tui_swap.rs::{the_tui_bundle_boots_with_all_five_rows_active,
disabling_the_search_row_removes_its_pane_and_reflows, re_enabling_the_search_row_returns_the_pane,
the_retired_search_row_leaves_no_pane_no_listener_and_no_binding}`.

**GATE (manual, Andrey's act, not claimed by any test): one full real workday through the new
TUI.** Recorded in `BUILD.md`'s Phase 3 row as a manual gate with the date he ran it. No work
package may mark Phase 3 done without that line, and no test in this document asserts it.

---

## 4. What Phase 3 does NOT build

- **`preview`, `timeline` and `drift` panes** (§11, §17 Phase 8). The slot vocabulary has room
  (`Slot::Aux`), and nothing else is owed.
- **`/compact` and `/goal`** (§11 names them). `/compact` needs `rollups` (Phase 4) and `/goal`
  needs the claims flow (Phase 5). The seam they register on exists now; the commands do not.
- **Focus switching between MULTIPLE agents** as a verified behaviour. §17 Phase 3 says so
  explicitly: "focus switch is verified in Phase 5 when a second agent exists". The strip renders
  every live agent and click-to-focus works with one; the multi-agent assertion is Phase 5's.
- **`mail-router`** (Phase 5). The old-feed adapter delivers to one configured agent.
- **`ctx.schedule`** (Phase 6). The adapter's sweep is an ordinary effect loop on `poll_ms`.
- **`ctx.power` / the macOS sleep listener** (Phase 7). TUI launch is the lid-open proxy, exactly
  as §13 says it must be until then.
- **Hot-lib-reloader** (§13, dev loop only). Not adopted in this phase; `make tui-test` against
  the release binary is the iteration loop, and §13's Rust-2024 `unsafe(no_mangle)` question stays
  open.
- **`bough mcp call`** (Phase 6). `ctx.commands` carries a `schemars::Schema` per command so that
  phase has something to call into; nothing in this phase uses it beyond arg validation.

---

## 5. Decisions taken where REQUIREMENTS is silent

- **P3-D1 — the composer is `ratatui-textarea` 0.9.2, not the brief's 0.8.x.** 0.9.2 is the
  current release of the same (ratatui-org) crate and depends on `ratatui-core` ^0.1.1 /
  `ratatui-widgets` ^0.3.1, which unify with `ratatui` 0.30.2's ^0.1.2 / ^0.3.2. 0.8.0 predates
  those bumps. §13's intent — the maintained fork, not the stale `tui-textarea` — is what is
  honoured.
- **P3-D2 — `Backend::Auto`: no TTY means the headless TestBackend, not a boot failure.** §0.2's
  "misconfiguration fails loud" is about COMPOSITION; the presence of a terminal is runtime state
  (P2-D7's reasoning for a missing API key). Failing the boot would make `bough --profile tui
  --check`, CI and Phase 8's everything-is-a-plugin audit unable to mount the default profile.
- **P3-D3 — when the process owns a terminal, tracing goes to `~/.bough/bough.log`, not stderr.**
  The launcher installs the subscriber before anything is composed, so the shell cannot redirect
  it later, and a log line written into the alt screen corrupts the display and every shell-use
  assertion. This is a launcher detail, not a behaviour switch: `--check`, `exec` and a piped
  stdout all keep stderr.
- **P3-D4 — the focus pane IS the trajectory pane.** §11 lists `focus` and `trajectory` as
  separate names; a second pane rendering the same trajectory into a second slot has one
  conceivable consumer and no second, and §0.2 forbids splitting preemptively. `tui-focus` owns
  the live tail AND the scrollback, and §11's `trajectory` name is honoured by the pane's
  scrollback mode. Phase 5's fork branch picker or Phase 8's timeline give it a second consumer;
  splitting then is a rename plus a slot.
- **P3-D5 — `tui-render` is a crate, not a module of `tui-shell`.** Two pane crates consume it and
  work packages need disjoint file sets. It has no row and no key, the `plugins/ledger`
  precedent. §15 item 6's phase-close review folds it back if it never grows a third consumer.
- **P3-D6 — the selection is extracted from the shell's last rendered buffer, not from the pane.**
  Every pane draws into that buffer; a per-pane `select()` would duplicate layout knowledge and
  would disagree with what is on screen the moment a pane's wrapping changed. The cost is that a
  selection is a rectangle of what is VISIBLE, never of the scrolled-away text; that is what a
  terminal selection means and it is what OSC52 can carry.
- **P3-D7 — OSC52 is the copy path; `arboard` is best effort.** OSC52 works over SSH and inside
  the PTY the tests drive, which is the environment the gate is measured in. An `arboard` failure
  (no display server, Wayland without a compositor) is a `notify` line and never an error return.
- **P3-D8 — a slash command's output is NOT a step.** It is rendered locally and is not
  model-visible, so §0.2's model-visible ⟺ ledgered does not oblige a step type; adding one would
  put terminal chatter into every future projection. A command that WANTS the agent to see
  something calls `Agent::inject` / `Agent::steer`, which are durable already.
- **P3-D9 — `RenderIntent::Diff` implies an args convention.** A generic renderer cannot know
  each tool's argument names, so the intent carries a contract: `{path, old, new}`, or
  `{path, old_string, new_string}`, or `{path, content}`. A `Diff` tool whose args match none of
  them falls back to `generic_block` with a dim note rather than rendering nothing, and
  `tui-render/tests/args.rs` checks the convention against `tools-baseline`'s real schemas so the
  two cannot drift apart silently.
- **P3-D10 — no termimad in this phase.** §13 keeps it in the crate list; nothing requires using
  it. `markdownish` + `highlight` reuse the syntect path the diff renderer already loads, and one
  text renderer is easier to keep consistent than two. If daily use says otherwise, termimad
  returns as a `tui-render` internal with no API change.
- **P3-D11 — the strip reads `about/line` by step-type NAME, not through
  `bough-plugin-about-line`.** A pane depending on a Consumer crate would invert the seam rule;
  the merge-extensible step-type map (§3) exists precisely so a renderer can read a type it does
  not own. With `about-line` disabled the strip renders the glyph and no about-lines.
- **P3-D12 — the live tail supersedes the durable prefix by LENGTH.** `agent-loop`'s
  `flush_text` drains its accumulator, so the durable `thought/text` steps of a step index
  concatenate to a prefix of what streamed. Rendering `live` whenever `live.len() >=
  durable.len()` makes the handover flicker-free without any coordination between the tee
  listener and the `ledger/step` listener, which race by construction.
- **P3-D13 — the old-feed watermarks live in the adapter's OWN sqlite file.** The ledger is
  append-only and its schema belongs to `ledger-sqlite`; a mutable collector watermark has no
  business there. §14 calls the adapter throwaway, and a separate file dies with one `rm` when
  Phase 6 sets `disabled: true`.
- **P3-D14 — no ledger vocabulary change for catch-up.** `WakeStart` carries `urgency` and
  `trigger`, not a wake kind, and `WakeKind` lives in `plugins/llm` (which depends on `ledger`),
  so putting it on the step would invert a dependency. V6 counts `wake/start` steps appended
  after the `residents` row activates — the test controls that boundary because it seeds the
  ledger before the process starts.
- **P3-D15 — `Agent::deliver` is a seam method, not adapter code.** The `mail/delivered` step and
  the inbox splice that carries its seq are a pair; §5's consumption rules are defined over
  delivered mail, and a producer that wrote one without the other would corrupt drain
  scheduling silently. Phase 6's collectors are the second caller, which is what makes it a seam
  method rather than a helper.
- **P3-D16 — `Agent::request_wake` / `AgentDriver::wake_now` rather than a synthetic message.**
  Delivering a fake "catch up" message to make an agent wake would put harness chatter in the
  transcript and in every future projection. The driver already knows whether there is anything
  to do; asking it is one method, and it is the same method Phase 7's `sleep-listener` needs.
- **P3-D17 — `residents` owns the roster as well as the catch-up wake.** Something has to resume
  the agents at launch and hold their disposers (the disposer is a capability, §2), and "each
  ACTIVE agent" is not a defined set until someone has resumed them. Splitting the roster from
  the wake would give two rows with one dependency and no second consumer. Disabling the row is
  what "start with no residents" means.
- **P3-D18 — the keymap is fixed in code; `tui/key` is the extension point.** A keymap is
  protocol between Andrey's fingers and the surface, not a deployment-varying value, so §0.2 puts
  it in code. A plugin that wants a binding registers on the `tui/key` waterfall and sets
  `handled`.
- **P3-D19 — `make tui-test` runs the release binary, and `make gates` does not run it.** The
  shell-use suite needs a built binary, a scratch `$BOUGH_HOME` and `sqlite3`; it is a
  minutes-scale integration suite, not a unit suite. `make gates` stays the fast hermetic gate
  (AGENTS.md), and `make tui-test` is the surface gate, run before every Phase-3 commit and named
  in `BUILD.md`.
- **P3-D20 — `llm-replay` gains a per-chunk `delay_ms`.** Without it a replayed answer arrives in
  one poll and "streams token by token" is unobservable offline, which would leave V1's first
  half provable only against a live model. The field defaults to 0, so every existing transcript
  and every Phase-2 test behaves exactly as before.
