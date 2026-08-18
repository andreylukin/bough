//! Input handling (port of `src/tui/keys.ts`): keys are DATA, and the help
//! overlay is generated from that data.
//!
//! Invariants held (see the TS header for the full rationale):
//! - there is exactly one description of what a key does, and it is the thing
//!   that makes the key do it (`BINDINGS` + `lookup` + `help_sections`);
//! - resolution is pure and needs no terminal;
//! - the same chord may mean two things, and the guard says which;
//! - the panel's tab list is part of the keymap (`TABS`);
//! - a tab-local key says so in the table (`Binding::tab`), not in its prose;
//! - `key.super` is only believable under the kitty keyboard protocol.
//!
//! Line editing lives here too, as pure `LineState → LineState` functions.
//! The cursor is a CHAR index into the string (byte-vs-char matters in Rust).
//!
//! `word_left`/`word_right` mirror `src/tui/format.ts` until format.rs
//! (row 1.34) lands, at which point they become re-exports.

use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Modes and commands
// ---------------------------------------------------------------------------

/// Which surface has the keyboard. Not a view stack: a mode is answered by
/// exactly one binding set. `job` is one background shell's output, opened
/// from the rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiMode {
    Chat,
    Rail,
    Ask,
    Panel,
    Help,
    Job,
}

/// The panel's tabs, as data. Derived everywhere they are used, so a tab
/// cannot exist without a chord and cannot ship undocumented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelTab {
    Tree,
    Recap,
    Changes,
    Workflows,
    Model,
    Mcp,
    Skills,
    Hooks,
    Plugins,
    Context,
    Theme,
}

impl PanelTab {
    pub fn id(self) -> &'static str {
        match self {
            PanelTab::Tree => "tree",
            PanelTab::Recap => "recap",
            PanelTab::Changes => "changes",
            PanelTab::Workflows => "workflows",
            PanelTab::Model => "model",
            PanelTab::Mcp => "mcp",
            PanelTab::Skills => "skills",
            PanelTab::Hooks => "hooks",
            PanelTab::Plugins => "plugins",
            PanelTab::Context => "context",
            PanelTab::Theme => "theme",
        }
    }
}

/// Every command a key or `/name` can dispatch. Ported verbatim from the TS
/// union; `Tab(PanelTab)` covers the derived `tab.*` members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    // -- global --
    /// First ^c: show the quit hint. A single ^c must never tear the UI down.
    QuitArm,
    Quit,
    HelpOpen,
    HelpClose,
    // -- the one tabbed panel --
    PanelToggle,
    PanelClose,
    PanelNext,
    PanelPrev,
    PanelConfirm,
    PanelConfirmSummarize,
    PanelPick,
    PanelFilter,
    PanelFilterBack,
    PanelFilterTier,
    PanelFilterExit,
    Tab(PanelTab),
    TreeRewind,
    TreeExtract,
    TreeMoveInto,
    McpRestart,
    SessionNew,
    SessionCompact,
    SessionCopyId,
    SchedulesShow,
    SavedShow,
    ArtifactsShow,
    RulesShow,
    /// `/restart` — stop the server, start a fresh one, come back here.
    Restart,
    // -- composing --
    Send,
    SendQueue,
    Newline,
    ImagePaste,
    DraftClear,
    Cancel,
    AttachmentUp,
    AttachmentDown,
    TurnInterrupt,
    MessageUnsend,
    HistoryPrev,
    HistoryNext,
    /// ⇧⇥ from the composer: one step up the thinking-depth ladder, wrapping.
    /// The `^o` tab is the surface that EXPLAINS the depths; this is the reflex
    /// for changing them between turns without leaving the composer.
    EffortCycle,
    // -- the @/ completion popup --
    CompleteAccept,
    GhostAccept,
    CompletePrev,
    CompleteNext,
    CompleteDismiss,
    // -- reading --
    FoldAll,
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    // -- editing the line --
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    CursorWordLeft,
    CursorWordRight,
    CursorUp,
    CursorDown,
    DeleteBack,
    DeleteForward,
    DeleteWordBack,
    DeleteToEnd,
    DeleteToStart,
    DeleteLine,
    SessionOut,
    // -- the live work rail --
    RailEnter,
    RailUp,
    RailDown,
    RailOpen,
    RailExit,
    RailStop,
    // -- one job's output --
    JobClose,
    JobStop,
    // -- a question hold --
    AskPick,
    AskSend,
    AskDecline,
    // -- list navigation --
    MoveUp,
    MoveDown,
    MoveIn,
    MoveOut,
    MovePageUp,
    MovePageDown,
    // -- MCP --
    McpAuth,
    McpForget,
    McpRemove,
    McpConnect,
    McpAdd,
    // -- workflow steering --
    WfPause,
    WfResume,
    WfStop,
    WfRerun,
    WfScript,
    WfSave,
    WfFilter,
    WfOpenAgent,
    // -- the changes tab --
    ChangesRevert,
    ChangesRevertAll,
}

// ---------------------------------------------------------------------------
// The tabs of the one panel
// ---------------------------------------------------------------------------

pub struct TabDef {
    pub id: PanelTab,
    pub title: &'static str,
    pub chord: &'static str,
    pub desc: &'static str,
}

/// Every non-chat surface, as data. Adding a surface is adding a row — it
/// cannot add a mode, an open flag, or an escape path, and it cannot ship
/// without a key.
pub const TABS: [TabDef; 11] = [
    TabDef {
        id: PanelTab::Tree,
        title: "tree",
        chord: "ctrl+f",
        desc: "conversations, turns, branches",
    },
    TabDef {
        id: PanelTab::Changes,
        title: "changes",
        chord: "ctrl+d",
        desc: "what this session changed",
    },
    TabDef {
        id: PanelTab::Recap,
        title: "recap",
        // `meta+` is the second register the context tab already reaches into;
        // every ctrl chord was taken long before this tab existed.
        chord: "meta+r",
        desc: "what happened in this conversation, one line per beat",
    },
    TabDef {
        id: PanelTab::Workflows,
        title: "workflows",
        chord: "ctrl+w",
        desc: "workflow runs",
    },
    TabDef {
        id: PanelTab::Model,
        title: "model",
        chord: "ctrl+o",
        desc: "frontier · cheap · thinking depth",
    },
    TabDef {
        id: PanelTab::Mcp,
        title: "mcp",
        chord: "ctrl+p",
        desc: "servers, grants, authorization",
    },
    TabDef {
        id: PanelTab::Skills,
        title: "skills",
        chord: "ctrl+k",
        desc: "the /skills this install has",
    },
    TabDef {
        id: PanelTab::Hooks,
        title: "hooks",
        // NOT ctrl+h: that byte IS backspace (0x08), so the terminal delivers
        // it to the composer and the tab is unreachable. Driven in a real PTY
        // to find that out.
        chord: "ctrl+x",
        desc: "the lua that runs around each turn; toggle it",
    },
    TabDef {
        id: PanelTab::Plugins,
        title: "plugins",
        // `meta+` again: ctrl+p is the mcp tab's and every other ctrl chord
        // was taken long before this tab existed.
        chord: "meta+p",
        desc: "what each plugin ships, and the switch on every piece",
    },
    TabDef {
        id: PanelTab::Context,
        title: "context",
        // Every ctrl chord this TUI could reach was already bound; `meta+` is
        // the tree's own second register, and `meta+c` was free.
        chord: "meta+c",
        desc: "what the last turn put in the window, and what it cost",
    },
    TabDef {
        id: PanelTab::Theme,
        title: "theme",
        chord: "ctrl+y",
        desc: "browse colour themes live; leaving reverts",
    },
];

/// Tab ids in bar order. Derived, so the bar and the keymap cannot disagree.
pub const PANEL_TABS: [PanelTab; 11] = [
    PanelTab::Tree,
    PanelTab::Changes,
    PanelTab::Recap,
    PanelTab::Workflows,
    PanelTab::Model,
    PanelTab::Mcp,
    PanelTab::Skills,
    PanelTab::Hooks,
    PanelTab::Plugins,
    PanelTab::Context,
    PanelTab::Theme,
];

/// Opens and closes the panel. Never names a tab.
pub const PANEL_TOGGLE: &str = "ctrl+t";

/// The retired `sessions` tab's chord, still bound to the tree that replaced it.
pub const SESSIONS_ALIAS: &str = "ctrl+s";

/// Tabs whose body is a flat list long enough to need narrowing.
pub const FILTER_TABS: [PanelTab; 3] = [PanelTab::Tree, PanelTab::Model, PanelTab::Skills];

/// The tab a chord jumps to, or None.
pub fn tab_for_chord(chord: &str) -> Option<PanelTab> {
    TABS.iter().find(|t| t.chord == chord).map(|t| t.id)
}

/// The tab a `Tab(..)` command names, or None for every other command.
pub fn tab_for_command(command: Command) -> Option<PanelTab> {
    match command {
        Command::Tab(t) => Some(t),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// One `/name` row in the composer's popup: a command, not text to insert.
pub struct SlashCommand {
    pub name: &'static str,
    pub command: Command,
    pub desc: &'static str,
    /// Whether trailing text is an ARGUMENT rather than prose. Off by default,
    /// and that default is load-bearing.
    pub takes_arg: bool,
}

/// The built-in `/commands` — the tab rows derived from `TABS`, then the
/// non-tab commands.
pub static SLASH_COMMANDS: LazyLock<Vec<SlashCommand>> = LazyLock::new(|| {
    let mut out: Vec<SlashCommand> = TABS
        .iter()
        .map(|t| SlashCommand {
            name: t.id.id(),
            command: Command::Tab(t.id),
            desc: t.desc,
            takes_arg: false,
        })
        .collect();
    out.extend([
        SlashCommand {
            name: "new",
            command: Command::SessionNew,
            desc: "start a fresh conversation",
            takes_arg: false,
        },
        SlashCommand {
            name: "compact",
            command: Command::SessionCompact,
            desc: "hand off to a fresh conversation · /compact <goal>",
            takes_arg: true,
        },
        SlashCommand {
            name: "rewind",
            command: Command::TreeRewind,
            desc: "go back to a turn and say it differently",
            takes_arg: false,
        },
        SlashCommand {
            name: "schedules",
            command: Command::SchedulesShow,
            desc: "the recurring runs and when they fire",
            takes_arg: false,
        },
        SlashCommand {
            name: "saved",
            command: Command::SavedShow,
            desc: "workflows saved to run again by name",
            takes_arg: false,
        },
        SlashCommand {
            name: "artifacts",
            command: Command::ArtifactsShow,
            desc: "pages this conversation published",
            takes_arg: false,
        },
        SlashCommand {
            name: "rules",
            command: Command::RulesShow,
            desc: "the AGENTS.md files injected into every turn",
            takes_arg: false,
        },
        SlashCommand {
            name: "context",
            command: Command::Tab(PanelTab::Context),
            desc: "what is in the window, section by section, with sizes",
            takes_arg: false,
        },
        SlashCommand {
            name: "restart",
            command: Command::Restart,
            desc: "restart the server and reopen this conversation",
            takes_arg: false,
        },
        SlashCommand {
            name: "help",
            command: Command::HelpOpen,
            desc: "every key, by section",
            takes_arg: false,
        },
    ]);
    out
});

/// `^\/([a-z][a-z0-9-]*)$` over the trimmed draft.
fn lone_slash_word(draft: &str, allow_leading_digit: bool) -> Option<String> {
    let t = draft.trim();
    let rest = t.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    let first_ok = first.is_ascii_alphabetic() || (allow_leading_digit && first.is_ascii_digit());
    if !first_ok {
        return None;
    }
    let tail_ok = chars.clone().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || (allow_leading_digit && (c == ':' || c == '_'))
    });
    if !tail_ok {
        return None;
    }
    Some(rest.to_lowercase())
}

/// The command a DRAFT names, if the whole draft is one — `"/model"` →
/// `Tab(Model)`. EXACT AND WHOLE, deliberately: `/help me name this` is prose.
pub fn slash_command_for(draft: &str) -> Option<Command> {
    let name = lone_slash_word(draft, false)?;
    SLASH_COMMANDS
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.command)
}

/// A draft as an INVOCATION: the command it names plus its argument.
/// `/compact focus on the parser` reaches the handoff with a goal.
pub fn slash_invocation(draft: &str) -> Option<(Command, String)> {
    let trimmed = draft.trim();
    if let Some(exact) = slash_command_for(trimmed) {
        return Some((exact, String::new()));
    }
    let rest = trimmed.strip_prefix('/')?;
    let (word, arg) = rest.split_once(char::is_whitespace)?;
    if word.is_empty() || !word.chars().next().unwrap().is_ascii_alphabetic() {
        return None;
    }
    if !word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let spec = SLASH_COMMANDS
        .iter()
        .find(|c| c.name == word.to_lowercase())?;
    if !spec.takes_arg || arg.trim().is_empty() {
        return None;
    }
    Some((spec.command, arg.trim().to_string()))
}

/// The commands other harnesses have that bough answers under a different name.
/// NOT aliases — a suggestion tells them the name and lets them press it.
const FOREIGN_COMMANDS: [(&str, &str); 10] = [
    ("clear", "new"),
    ("reset", "new"),
    ("resume", "tree"),
    ("sessions", "tree"),
    ("history", "tree"),
    ("cost", "model"),
    ("status", "model"),
    ("diff", "changes"),
    ("exit", ""),
    ("quit", ""),
];

/// A bare `/word` that is NOT a command, with the nearest thing that is.
/// Returns None for anything that IS a command, names a skill, or is not a
/// lone `/word`.
pub fn unknown_command(draft: &str, skills: &[&str]) -> Option<(String, Option<String>)> {
    let name = lone_slash_word(draft, true)?;
    if SLASH_COMMANDS.iter().any(|c| c.name == name) {
        return None;
    }
    if skills.iter().any(|s| s.to_lowercase() == name) {
        return None;
    }
    if let Some((_, foreign)) = FOREIGN_COMMANDS.iter().find(|(k, _)| *k == name) {
        let suggestion = if foreign.is_empty() {
            None
        } else {
            Some(foreign.to_string())
        };
        return Some((name, suggestion));
    }
    // Nearest command by prefix, then by containment. Skills are candidates too.
    let candidates: Vec<String> = SLASH_COMMANDS
        .iter()
        .map(|c| c.name.to_string())
        .chain(skills.iter().map(|s| s.to_string()))
        .collect();
    let near = candidates
        .iter()
        .find(|c| c.starts_with(&name))
        .or_else(|| candidates.iter().find(|c| c.contains(&name)))
        .cloned();
    Some((name, near))
}

// ---------------------------------------------------------------------------
// Chords (pure)
// ---------------------------------------------------------------------------

/// The subset of the terminal key event this module reads. Structural, so a
/// crossterm `KeyEvent` normalizes into it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyFlags {
    pub up_arrow: bool,
    pub down_arrow: bool,
    pub left_arrow: bool,
    pub right_arrow: bool,
    pub page_up: bool,
    pub page_down: bool,
    pub home: bool,
    pub end: bool,
    pub r#return: bool,
    pub escape: bool,
    pub tab: bool,
    pub backspace: bool,
    pub delete: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
}

/// One keypress as a canonical string — `"ctrl+p"`, `"meta+enter"`, `"esc"`,
/// `"?"`. Returns `""` for anything that is not a chord: a paste, a coalesced
/// chunk of typing, a bare modifier — the caller treats that as text.
pub fn chord_of(input: &str, key: KeyFlags) -> String {
    let mut mods: Vec<&str> = Vec::new();
    if key.ctrl {
        mods.push("ctrl");
    }
    if key.meta {
        mods.push("meta");
    }
    if key.super_ {
        mods.push("super");
    }

    let base: String = if key.up_arrow {
        "up".into()
    } else if key.down_arrow {
        "down".into()
    } else if key.left_arrow {
        "left".into()
    } else if key.right_arrow {
        "right".into()
    } else if key.page_up {
        "pageup".into()
    } else if key.page_down {
        "pagedown".into()
    } else if key.home {
        "home".into()
    } else if key.end {
        "end".into()
    } else if key.escape {
        "esc".into()
    } else if key.tab {
        "tab".into()
    } else if key.backspace || key.delete {
        "backspace".into()
    } else if key.r#return {
        "enter".into()
    } else if input == "\n" {
        // A raw "\n" with no return flag can only be ^j. Terminals send \r for
        // Return, so this is the newline chord even with no ctrl modifier —
        // the old tree shipped a bug where ^j submitted half a message.
        return "ctrl+j".into();
    } else if input == " " {
        "space".into()
    } else if input.chars().count() == 1 {
        input.into()
    } else {
        return String::new();
    };

    if key.shift && (base == "enter" || base == "tab") {
        mods.push("shift");
    }
    if mods.is_empty() {
        base
    } else {
        format!("{}+{base}", mods.join("+"))
    }
}

fn chord_glyph(part: &str) -> &str {
    match part {
        "ctrl" => "^",
        "meta" => "⌥",
        "super" => "⌘",
        "shift" => "⇧",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "enter" => "⏎",
        "esc" => "esc",
        "tab" => "⇥",
        "backspace" => "⌫",
        "pageup" => "pgup",
        "pagedown" => "pgdn",
        "space" => "space",
        other => other,
    }
}

/// A chord as the help overlay prints it: `"ctrl+p"` → `"^p"`.
pub fn chord_label(chord: &str) -> String {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let base = parts.pop().unwrap_or("");
    let mods: String = parts.iter().map(|m| chord_glyph(m)).collect();
    format!("{mods}{}", chord_glyph(base))
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// The boolean fields of [`KeyContext`] — everything a `when`/`not` can name.
/// `tab` is excluded: it is matched by `Binding::tab` (set membership).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Guard {
    EmptyDraft,
    InSubagent,
    Multiline,
    Busy,
    JustSent,
    DoubleEsc,
    QuitArmed,
    RailLive,
    HasAttachments,
    Completing,
    PanelFiltering,
}

/// What a binding can be conditioned on. Every optional flag in TS defaults to
/// the safe degrade; here the struct's `Default` is all-false / no tab.
#[derive(Clone, Debug)]
pub struct KeyContext {
    pub mode: UiMode,
    /// The panel's open tab, or None when the panel is closed. NOT a Guard:
    /// it is the STRUCTURAL scope a tab-local binding is matched against.
    pub tab: Option<PanelTab>,
    pub empty_draft: bool,
    /// This conversation was DRILLED INTO — a subagent, a workflow agent, a
    /// schedule run. Not "has an `originId`": a handoff or a fork carries one
    /// too, and neither has a door back (`app::App::drilled_in_from`).
    pub in_subagent: bool,
    pub multiline: bool,
    pub busy: bool,
    pub just_sent: bool,
    pub double_esc: bool,
    pub quit_armed: bool,
    pub rail_live: bool,
    pub has_attachments: bool,
    pub completing: bool,
    pub panel_filtering: bool,
}

impl KeyContext {
    fn flag(&self, g: Guard) -> bool {
        match g {
            Guard::EmptyDraft => self.empty_draft,
            Guard::InSubagent => self.in_subagent,
            Guard::Multiline => self.multiline,
            Guard::Busy => self.busy,
            Guard::JustSent => self.just_sent,
            Guard::DoubleEsc => self.double_esc,
            Guard::QuitArmed => self.quit_armed,
            Guard::RailLive => self.rail_live,
            Guard::HasAttachments => self.has_attachments,
            Guard::Completing => self.completing,
            Guard::PanelFiltering => self.panel_filtering,
        }
    }
}

impl Default for KeyContext {
    fn default() -> Self {
        KeyContext {
            mode: UiMode::Chat,
            tab: None,
            empty_draft: true,
            in_subagent: false,
            multiline: false,
            busy: false,
            just_sent: false,
            double_esc: false,
            quit_armed: false,
            rail_live: false,
            has_attachments: false,
            completing: false,
            panel_filtering: false,
        }
    }
}

/// One row of the keymap. `mode: None` binds in every mode (`"*"`).
#[derive(Clone, Debug)]
pub struct Binding {
    pub mode: Option<UiMode>,
    pub chord: String,
    pub command: Command,
    /// Every named flag must be true.
    pub when: Vec<Guard>,
    /// Every named flag must be false.
    pub not: Vec<Guard>,
    /// Panel tabs this row is live in. None = every tab (and the closed panel).
    pub tab: Option<Vec<PanelTab>>,
    /// Help section. A binding with no section is an alias and is not documented.
    pub section: Option<&'static str>,
    pub desc: Option<&'static str>,
    /// Overrides the printed chord, for a run of rows sharing one description.
    pub label: Option<&'static str>,
}

fn b(mode: Option<UiMode>, chord: &str, command: Command) -> Binding {
    Binding {
        mode,
        chord: chord.to_string(),
        command,
        when: Vec::new(),
        not: Vec::new(),
        tab: None,
        section: None,
        desc: None,
        label: None,
    }
}

impl Binding {
    fn when(mut self, guards: &[Guard]) -> Self {
        self.when = guards.to_vec();
        self
    }
    fn not(mut self, guards: &[Guard]) -> Self {
        self.not = guards.to_vec();
        self
    }
    fn tabs(mut self, tabs: &[PanelTab]) -> Self {
        self.tab = Some(tabs.to_vec());
        self
    }
    fn doc(mut self, section: &'static str, desc: &'static str) -> Self {
        self.section = Some(section);
        self.desc = Some(desc);
        self
    }
    fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

/// The help section the direct-jump chords are printed under.
const PANEL_SECTION: &str = "the panel — ^t, or jump straight to a tab";

/// How long after a send Escape still means "take that back". The legend row
/// states the number to the user, so it lives with the keymap.
pub const UNSEND_MS: i64 = 3_000;

/// Chords that reach the panel from outside it and move between its tabs
/// inside it. The four composer-owned chords (`^f ^d ^w ^k`) are guarded on an
/// empty draft; the rest are not. Generated from `TABS`.
fn panel_chords() -> Vec<Binding> {
    let composer_owned = ["ctrl+f", "ctrl+d", "ctrl+w", "ctrl+k"];
    let mut rows: Vec<Binding> = Vec::new();
    let mut entries: Vec<(&str, Command, &'static str)> =
        vec![(PANEL_TOGGLE, Command::PanelToggle, "open / close the panel")];
    for t in TABS.iter() {
        entries.push((t.chord, Command::Tab(t.id), t.desc));
    }
    for (chord, command, desc) in entries {
        // Documented once, on the chat row — the overlay is read from chat.
        let mut chat = b(Some(UiMode::Chat), chord, command).doc(PANEL_SECTION, desc);
        if composer_owned.contains(&chord) {
            chat = chat.when(&[Guard::EmptyDraft]);
        }
        rows.push(chat);
        // A direct jump must work from anywhere it is not being typed into —
        // INCLUDING while a question is held (`ask`).
        rows.push(b(Some(UiMode::Panel), chord, command));
        rows.push(b(Some(UiMode::Rail), chord, command));
        rows.push(b(Some(UiMode::Ask), chord, command));
    }
    // `^s` alias for the tree, documented on the chat row.
    for mode in [UiMode::Chat, UiMode::Panel, UiMode::Rail, UiMode::Ask] {
        let mut row = b(Some(mode), SESSIONS_ALIAS, Command::Tab(PanelTab::Tree));
        if mode == UiMode::Chat {
            row = row
                .when(&[Guard::EmptyDraft])
                .doc(PANEL_SECTION, "the tree, too");
        }
        rows.push(row);
    }
    rows
}

/// `1`…`9`, one row each, documented once as `1-9`.
fn digits(
    mode: UiMode,
    command: Command,
    section: &'static str,
    desc: &'static str,
    not: &[Guard],
) -> Vec<Binding> {
    (1..=9)
        .map(|i| {
            let mut row = b(Some(mode), &i.to_string(), command).not(not);
            if i == 1 {
                row = row.doc(section, desc).label("1-9");
            }
            row
        })
        .collect()
}

/// Every binding in the TUI, in resolution order within a mode. Ordering is
/// only ever used to put a GUARDED row ahead of its unguarded fallback.
// The table is built by pushing in order because the ORDER is the semantics
// (first match wins, guarded rows ahead of their fallbacks) and the rows are
// grouped by comments explaining each block. A single `vec![]` literal would
// read as one 120-element expression.
#[allow(clippy::vec_init_then_push)]
pub static BINDINGS: LazyLock<Vec<Binding>> = LazyLock::new(|| {
    use Command as C;
    use Guard as G;
    use PanelTab as T;
    use UiMode as M;
    let mut rows: Vec<Binding> = Vec::new();

    // -- global --
    // Two rows: a SINGLE ^c must not quit. Bound in every mode.
    rows.push(
        b(None, "ctrl+c", C::Quit)
            .when(&[G::QuitArmed])
            .doc("leaving", "quit · subagents keep running")
            .label("^c ^c"),
    );
    rows.push(b(None, "ctrl+c", C::QuitArm));

    // -- chat --
    rows.push(
        b(Some(M::Chat), "?", C::HelpOpen)
            .when(&[G::EmptyDraft])
            .doc("leaving", "this overlay"),
    );

    // The popup owns ⏎ while it is open, so it sits AHEAD of `send`.
    rows.push(
        b(Some(M::Chat), "enter", C::CompleteAccept)
            .when(&[G::Completing])
            .doc("compose", "accept the @ or / suggestion")
            .label("⏎ ⇥"),
    );
    rows.push(
        b(Some(M::Chat), "enter", C::Send).doc("compose", "send · interjects while a turn runs"),
    );
    rows.push(
        b(Some(M::Chat), "meta+enter", C::SendQueue).doc("compose", "queue for after this turn"),
    );
    rows.push(b(Some(M::Chat), "ctrl+j", C::Newline).doc("compose", "newline"));
    rows.push(b(Some(M::Chat), "ctrl+v", C::ImagePaste).doc("compose", "attach clipboard image"));
    rows.push(b(Some(M::Chat), "super+v", C::ImagePaste).label("⌘v"));
    rows.push(b(Some(M::Chat), "meta+v", C::ImagePaste));
    rows.push(
        b(Some(M::Chat), "ctrl+n", C::SessionNew).doc("compose", "start a fresh conversation"),
    );
    rows.push(
        b(Some(M::Chat), "ctrl+g", C::SessionCopyId).doc("compose", "copy this conversation's id"),
    );
    // The chord every Claude Code user presses on reflex. bough has no
    // permission modes to cycle — it sandboxes instead of asking — so it moves
    // the other thing that changes how the next turn runs.
    rows.push(
        b(Some(M::Chat), "shift+tab", C::EffortCycle)
            .doc("compose", "cycle thinking depth")
            .label("⇧⇥"),
    );
    // `not: [emptyDraft]` is not decoration: an empty-draft double-tap must
    // fall through to the stop, not be swallowed.
    rows.push(
        b(Some(M::Chat), "esc", C::DraftClear)
            .when(&[G::DoubleEsc])
            .not(&[G::EmptyDraft])
            .doc("compose", "clear the draft")
            .label("esc esc"),
    );
    // esc esc with nothing typed and nothing running: open the tree on your
    // last turn. `not: [busy]` keeps the stop below un-lost.
    rows.push(
        b(Some(M::Chat), "esc", C::TreeRewind)
            .when(&[G::DoubleEsc, G::EmptyDraft])
            .not(&[G::Busy, G::Completing])
            .doc("compose", "go back to a turn and fork it")
            .label("esc esc"),
    );
    // The @// popup rows sit AHEAD of the composer's own ↑/↓/esc — and ahead
    // of `turn.interrupt`: escape unwinds exactly ONE level, nearest surface
    // first.
    rows.push(b(Some(M::Chat), "tab", C::CompleteAccept).when(&[G::Completing]));
    rows.push(
        b(Some(M::Chat), "tab", C::GhostAccept)
            .not(&[G::Completing])
            .doc("compose", "take the suggested next message"),
    );
    rows.push(b(Some(M::Chat), "up", C::CompletePrev).when(&[G::Completing]));
    rows.push(b(Some(M::Chat), "down", C::CompleteNext).when(&[G::Completing]));
    rows.push(b(Some(M::Chat), "esc", C::CompleteDismiss).when(&[G::Completing]));
    // The take-back window: for UNSEND_MS after ⏎, one Escape means "I did not
    // mean to send that". Gated on emptyDraft — a draft is never traded for
    // the sent message. It outranks the stop INSIDE the window because unsend
    // stops the turn anyway.
    rows.push(
        b(Some(M::Chat), "esc", C::MessageUnsend)
            .when(&[G::JustSent, G::EmptyDraft])
            .doc("compose", "take back the message you just sent (3s)"),
    );
    rows.push(
        b(Some(M::Chat), "esc", C::TurnInterrupt)
            .when(&[G::Busy])
            .doc("leaving", "stop the running turn"),
    );
    rows.push(b(Some(M::Chat), "esc", C::Cancel));
    rows.push(
        b(Some(M::Chat), "up", C::CursorUp)
            .when(&[G::Multiline])
            .doc("compose", "history · lines if multiline")
            .label("↑/↓"),
    );
    rows.push(b(Some(M::Chat), "up", C::AttachmentUp).when(&[G::EmptyDraft, G::HasAttachments]));
    rows.push(b(Some(M::Chat), "up", C::HistoryPrev));
    rows.push(b(Some(M::Chat), "down", C::CursorDown).when(&[G::Multiline]));
    rows.push(
        b(Some(M::Chat), "down", C::AttachmentDown).when(&[G::EmptyDraft, G::HasAttachments]),
    );
    rows.push(
        b(Some(M::Chat), "down", C::RailEnter)
            .when(&[G::EmptyDraft, G::RailLive])
            .doc("read", "into the live work rail"),
    );
    rows.push(b(Some(M::Chat), "down", C::HistoryNext));

    // -- reading --
    rows.push(
        b(Some(M::Chat), "ctrl+e", C::FoldAll)
            .when(&[G::EmptyDraft])
            .doc("read", "fold/unfold every tool call"),
    );
    rows.push(
        b(Some(M::Chat), "pageup", C::ScrollPageUp)
            .doc("read", "scroll back / forward")
            .label("pgup pgdn"),
    );
    rows.push(b(Some(M::Chat), "pagedown", C::ScrollPageDown));

    // -- the one tabbed panel --
    rows.extend(panel_chords());

    // -- editing the line --
    rows.push(
        b(Some(M::Chat), "ctrl+a", C::CursorHome)
            .doc("edit the line", "line start / end")
            .label("^a ^e"),
    );
    rows.push(b(Some(M::Chat), "ctrl+e", C::CursorEnd));
    rows.push(b(Some(M::Chat), "home", C::CursorHome));
    rows.push(b(Some(M::Chat), "end", C::CursorEnd));
    // AHEAD of `cursor.left`, guarded on an empty draft.
    rows.push(
        b(Some(M::Chat), "left", C::SessionOut)
            .when(&[G::EmptyDraft, G::InSubagent])
            .doc("read", "back to the session that spawned this one")
            .label("←"),
    );
    rows.push(b(Some(M::Chat), "left", C::CursorLeft));
    rows.push(b(Some(M::Chat), "right", C::CursorRight));
    rows.push(
        b(Some(M::Chat), "ctrl+b", C::CursorLeft)
            .doc("edit the line", "char back / forward")
            .label("^b ^f"),
    );
    rows.push(b(Some(M::Chat), "ctrl+f", C::CursorRight));
    rows.push(
        b(Some(M::Chat), "meta+b", C::CursorWordLeft)
            .doc("edit the line", "word back / forward")
            .label("⌥b ⌥f"),
    );
    rows.push(b(Some(M::Chat), "meta+f", C::CursorWordRight));
    rows.push(b(Some(M::Chat), "meta+left", C::CursorWordLeft));
    rows.push(b(Some(M::Chat), "meta+right", C::CursorWordRight));
    rows.push(
        b(Some(M::Chat), "ctrl+d", C::DeleteForward)
            .doc("edit the line", "delete char ahead · word behind")
            .label("^d · ^w"),
    );
    rows.push(b(Some(M::Chat), "ctrl+w", C::DeleteWordBack).not(&[G::EmptyDraft]));
    rows.push(b(Some(M::Chat), "meta+backspace", C::DeleteWordBack));
    rows.push(
        b(Some(M::Chat), "ctrl+k", C::DeleteToEnd)
            .doc("edit the line", "kill to end / whole line")
            .label("^k ^u"),
    );
    rows.push(b(Some(M::Chat), "ctrl+u", C::DeleteLine));
    rows.push(
        b(Some(M::Chat), "super+backspace", C::DeleteToStart)
            .doc("edit the line", "to line start · jump to ends")
            .label("⌘⌫ ⌘←→"),
    );
    rows.push(b(Some(M::Chat), "super+left", C::CursorHome));
    rows.push(b(Some(M::Chat), "super+right", C::CursorEnd));
    rows.push(b(Some(M::Chat), "backspace", C::DeleteBack));

    // -- the live subagent rail --
    rows.push(
        b(Some(M::Rail), "up", C::RailUp)
            .doc("the rail", "move")
            .label("↑/↓"),
    );
    rows.push(b(Some(M::Rail), "down", C::RailDown));
    rows.push(
        b(Some(M::Rail), "enter", C::RailOpen).doc("the rail", "open this agent / shell output"),
    );
    rows.push(b(Some(M::Rail), "esc", C::RailExit).doc("the rail", "back to the composer"));
    rows.push(
        b(Some(M::Rail), "x", C::RailStop)
            .doc("the rail", "stop this shell / agent / run")
            .label("x x"),
    );

    // -- a question hold --
    rows.extend(digits(
        M::Ask,
        C::AskPick,
        "when bough asks",
        "pick an option",
        &[],
    ));
    rows.push(b(Some(M::Ask), "enter", C::AskSend).doc("when bough asks", "send what you typed"));
    rows.push(
        b(Some(M::Ask), "esc", C::AskDecline)
            .doc("when bough asks", "decline (the program catches it)"),
    );

    // -- inside the panel --
    rows.push(
        b(Some(M::Panel), "up", C::MoveUp)
            .doc("inside the panel", "move")
            .label("↑↓ j/k"),
    );
    rows.push(b(Some(M::Panel), "down", C::MoveDown));
    // Bare letters, so they are text while the filter buffer has the keyboard.
    rows.push(b(Some(M::Panel), "k", C::MoveUp).not(&[G::PanelFiltering]));
    rows.push(b(Some(M::Panel), "j", C::MoveDown).not(&[G::PanelFiltering]));
    rows.push(
        b(Some(M::Panel), "pageup", C::MovePageUp)
            .doc("inside the panel", "a screenful at a time")
            .label("pgup pgdn"),
    );
    rows.push(b(Some(M::Panel), "pagedown", C::MovePageDown));
    rows.push(
        b(Some(M::Panel), "tab", C::PanelFilterTier)
            .tabs(&[T::Model])
            .when(&[G::PanelFiltering])
            .doc(
                "inside the panel",
                "switch between the frontier and cheap search boxes",
            ),
    );
    // AHEAD of `panel.next`: while a search box has the keyboard, ⇥ belongs to
    // the box.
    rows.push(
        b(Some(M::Panel), "tab", C::PanelNext)
            .doc("inside the panel", "next / previous tab")
            .label("⇥ ⇧⇥"),
    );
    rows.push(b(Some(M::Panel), "shift+tab", C::PanelPrev));
    rows.push(b(Some(M::Panel), "enter", C::PanelConfirm).doc(
        "inside the panel",
        "open · grant · keep — what the tab affirms",
    ));
    rows.push(
        b(Some(M::Panel), "right", C::MoveIn)
            .doc("inside the panel", "drill into delegated work (tree)")
            .label("→ ←"),
    );
    rows.push(b(Some(M::Panel), "left", C::MoveOut));
    rows.extend(digits(
        M::Panel,
        C::PanelPick,
        "inside the panel",
        "jump to that row and affirm it",
        &[G::PanelFiltering],
    ));
    rows.push(
        b(Some(M::Panel), "s", C::PanelConfirmSummarize)
            .tabs(&[T::Tree])
            .not(&[G::PanelFiltering])
            .doc(
                "inside the panel",
                "tree: branch, carrying a summary of what you left",
            ),
    );

    // -- type-to-filter, the panel's one modal buffer --
    rows.push(
        b(Some(M::Panel), "/", C::PanelFilter)
            .tabs(&FILTER_TABS)
            .not(&[G::PanelFiltering])
            .doc(
                "inside the panel",
                "filter this list — in the tree, searches every message · esc clears",
            ),
    );
    rows.push(b(Some(M::Panel), "backspace", C::PanelFilterBack).when(&[G::PanelFiltering]));
    // Ahead of `panel.close`: escape unwinds exactly ONE level.
    rows.push(b(Some(M::Panel), "esc", C::PanelFilterExit).when(&[G::PanelFiltering]));
    rows.push(b(Some(M::Panel), "esc", C::PanelClose).doc("inside the panel", "back to chat"));

    // -- the MCP tab's verbs --
    rows.push(
        b(Some(M::Panel), "a", C::McpAuth)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc("the mcp tab", "authorize — prints the URL to open"),
    );
    rows.push(
        b(Some(M::Panel), "n", C::McpAdd)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc("the mcp tab", "add a remote server by URL"),
    );
    rows.push(
        b(Some(M::Panel), "F", C::McpForget)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc("the mcp tab", "forget this server's credentials"),
    );
    rows.push(
        b(Some(M::Panel), "c", C::McpConnect)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc(
                "the mcp tab",
                "test the connection · names the tools, or the error",
            ),
    );
    rows.push(
        b(Some(M::Panel), "r", C::McpRestart)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc("the mcp tab", "restart this server's process"),
    );
    rows.push(
        b(Some(M::Panel), "d", C::McpRemove)
            .tabs(&[T::Mcp])
            .not(&[G::PanelFiltering])
            .doc("the mcp tab", "delete this registration · d again confirms"),
    );

    // -- workflow steering — letters live only in the workflows tab --
    rows.push(
        b(Some(M::Panel), "p", C::WfPause)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "pause · in-flight agents finish"),
    );
    rows.push(
        b(Some(M::Panel), "P", C::WfResume)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "resume"),
    );
    rows.push(
        b(Some(M::Panel), "x", C::WfStop)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "stop · pause first to keep work"),
    );
    rows.push(
        b(Some(M::Panel), "r", C::WfRerun)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "relaunch from the journal"),
    );
    rows.push(
        b(Some(M::Panel), "e", C::WfScript)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "the run's script"),
    );
    rows.push(
        b(Some(M::Panel), "s", C::WfSave)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "save this run as a reusable workflow"),
    );
    rows.push(
        b(Some(M::Panel), "f", C::WfFilter)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc(
                "the workflows tab",
                "cycle agents: all/running/queued/done/error",
            ),
    );
    rows.push(
        b(Some(M::Panel), "o", C::WfOpenAgent)
            .tabs(&[T::Workflows])
            .not(&[G::PanelFiltering])
            .doc("the workflows tab", "open this agent's session"),
    );

    // -- the tree tab --
    rows.push(
        b(Some(M::Panel), "e", C::TreeExtract)
            .tabs(&[T::Tree])
            .not(&[G::PanelFiltering])
            .doc(
                "the tree tab",
                "split here — this turn on becomes its own conversation",
            ),
    );
    rows.push(
        b(Some(M::Panel), "m", C::TreeMoveInto)
            .tabs(&[T::Tree])
            .not(&[G::PanelFiltering])
            .doc(
                "the tree tab",
                "bring this turn on into the open conversation",
            ),
    );

    // -- the changes tab --
    rows.push(
        b(Some(M::Panel), "x", C::ChangesRevert)
            .tabs(&[T::Changes])
            .not(&[G::PanelFiltering])
            .doc("the changes tab", "revert this file — ⏎ confirms"),
    );
    rows.push(
        b(Some(M::Panel), "X", C::ChangesRevertAll)
            .tabs(&[T::Changes])
            .not(&[G::PanelFiltering])
            .doc("the changes tab", "revert everything — ⏎ confirms"),
    );

    // -- one job's output --
    rows.push(
        b(Some(M::Job), "esc", C::JobClose)
            .doc("a background job (⏎ on a rail row)", "back to the rail"),
    );
    rows.push(b(Some(M::Job), "q", C::JobClose));
    rows.push(b(Some(M::Job), "left", C::JobClose));
    rows.push(
        b(Some(M::Job), "up", C::ScrollUp)
            .doc("a background job (⏎ on a rail row)", "scroll the output")
            .label("↑/↓"),
    );
    rows.push(b(Some(M::Job), "down", C::ScrollDown));
    rows.push(b(Some(M::Job), "k", C::ScrollUp));
    rows.push(b(Some(M::Job), "j", C::ScrollDown));
    rows.push(
        b(Some(M::Job), "pageup", C::ScrollPageUp)
            .doc("a background job (⏎ on a rail row)", "a screenful")
            .label("pgup/pgdn"),
    );
    rows.push(b(Some(M::Job), "pagedown", C::ScrollPageDown));
    rows.push(
        b(Some(M::Job), "x", C::JobStop)
            .doc("a background job (⏎ on a rail row)", "kill this job")
            .label("x x"),
    );

    // -- the overlay itself --
    rows.push(b(Some(M::Help), "esc", C::HelpClose));
    rows.push(b(Some(M::Help), "?", C::HelpClose));
    rows.push(b(Some(M::Help), "q", C::HelpClose));
    rows.push(b(Some(M::Help), "up", C::ScrollUp));
    rows.push(b(Some(M::Help), "down", C::ScrollDown));
    rows.push(b(Some(M::Help), "k", C::ScrollUp));
    rows.push(b(Some(M::Help), "j", C::ScrollDown));
    rows.push(b(Some(M::Help), "pageup", C::ScrollPageUp));
    rows.push(b(Some(M::Help), "pagedown", C::ScrollPageDown));

    rows
});

fn guards_hold(binding: &Binding, ctx: &KeyContext) -> bool {
    if binding.when.iter().any(|g| !ctx.flag(*g)) {
        return false;
    }
    if binding.not.iter().any(|g| ctx.flag(*g)) {
        return false;
    }
    // A tab-scoped row is dead outside its tabs — including with the panel
    // closed, where `ctx.tab` is None.
    if let Some(tabs) = &binding.tab {
        match ctx.tab {
            Some(t) if tabs.contains(&t) => {}
            _ => return false,
        }
    }
    true
}

fn modes_overlap(a: Option<UiMode>, b: Option<UiMode>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// The command a chord means in this context, or None when nothing is bound.
pub fn lookup(ctx: &KeyContext, chord: &str) -> Option<Command> {
    if chord.is_empty() {
        return None;
    }
    BINDINGS
        .iter()
        .find(|b| modes_overlap(b.mode, Some(ctx.mode)) && b.chord == chord && guards_hold(b, ctx))
        .map(|b| b.command)
}

/// `lookup` straight off a keypress. The one entry point a component needs.
pub fn resolve(ctx: &KeyContext, input: &str, key: KeyFlags) -> Option<Command> {
    lookup(ctx, &chord_of(input, key))
}

// ---------------------------------------------------------------------------
// The help overlay, generated from the table
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct HelpSection {
    pub section: String,
    pub keys: Vec<(String, String)>,
    /// Prose rows with no key column.
    pub limits: bool,
    /// Chords a terminal veteran will try that bough does not bind.
    pub unavailable: bool,
    /// `/name` rows rather than chords.
    pub commands: bool,
}

/// Things bough deliberately WON'T do, so a user stops waiting for them.
pub fn limits_section() -> HelpSection {
    HelpSection {
        section: "won't do".into(),
        limits: true,
        keys: vec![
            (String::new(), "^c ^c quits; subagents keep running".into()),
            (String::new(), "programs run as you — no sandbox".into()),
            (
                String::new(),
                "changes land in your checkout as they happen".into(),
            ),
            (
                String::new(),
                "a running workflow takes no input — stop, edit, relaunch".into(),
            ),
        ],
        ..Default::default()
    }
}

/// Chords a terminal veteran WILL try that bough does not bind.
pub fn unavailable_section() -> HelpSection {
    HelpSection {
        section: "not bound".into(),
        unavailable: true,
        keys: vec![
            (
                "^r".into(),
                "no reverse search · ^f then / searches every message".into(),
            ),
            ("^z".into(), "no suspend · ^c ^c quits".into()),
            ("⌥d".into(), "use ^k".into()),
            (
                "home end".into(),
                "not delivered by the terminal layer · use pgup/pgdn".into(),
            ),
        ],
        ..Default::default()
    }
}

/// The overlay's sections, in table order. Derived, never authored.
pub fn help_sections(bindings: &[Binding]) -> Vec<HelpSection> {
    let mut out: Vec<HelpSection> = Vec::new();
    for b in bindings {
        let (Some(section), Some(desc)) = (b.section, b.desc) else {
            continue;
        };
        // A GUARDED row must say it is guarded, both ways round.
        let desc = if b.when.contains(&Guard::EmptyDraft) {
            format!("{desc} · empty draft")
        } else if b.not.contains(&Guard::EmptyDraft) {
            format!("{desc} · with a draft")
        } else {
            desc.to_string()
        };
        let label = b
            .label
            .map(str::to_string)
            .unwrap_or_else(|| chord_label(&b.chord));
        match out.iter_mut().find(|s| s.section == section) {
            Some(s) => s.keys.push((label, desc)),
            None => out.push(HelpSection {
                section: section.to_string(),
                keys: vec![(label, desc)],
                ..Default::default()
            }),
        }
    }
    // The `/` commands, listed BY NAME.
    let mut typed = HelpSection {
        section: "typed at the prompt".into(),
        commands: true,
        keys: vec![
            (
                "!cmd".into(),
                "run it in your shell — not a message, not billed; output in the rail".into(),
            ),
            (
                "@path".into(),
                "complete a file or directory into the message".into(),
            ),
        ],
        ..Default::default()
    };
    for c in SLASH_COMMANDS.iter() {
        typed
            .keys
            .push((format!("/{}", c.name), c.desc.to_string()));
    }
    out.push(typed);
    // WHAT THE TREE'S MARKS MEAN.
    out.push(HelpSection {
        section: "marks in the tree".into(),
        commands: true,
        keys: vec![
            ("●".into(), "a conversation you started".into()),
            (
                "↦".into(),
                "a fresh conversation handed off from another".into(),
            ),
            (
                "⑂".into(),
                "a fork — the same thread, cut at one turn".into(),
            ),
            (
                "≣".into(),
                "a compaction — a span replaced by a summary".into(),
            ),
            (
                "◆".into(),
                "a subagent — including a workflow's own agents".into(),
            ),
            (
                "⋯ ✓ ✗ ◼".into(),
                "running · finished · failed · stopped by a restart".into(),
            ),
        ],
        ..Default::default()
    });
    out.push(limits_section());
    out.push(unavailable_section());
    out
}

/// One PHYSICAL row of the overlay — flat, so `visible` can be a slice and a
/// slice cannot lose a header the way a squashed flexbox can.
#[derive(Clone, Debug, PartialEq)]
pub struct HelpLine {
    pub kind: HelpLineKind,
    pub chord: String,
    pub desc: String,
    pub muted: bool,
    pub prose: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpLineKind {
    Header,
    Row,
    Blank,
}

/// The overlay as a flat list of rows, one per line the terminal will draw.
pub fn help_lines(sections: &[HelpSection]) -> Vec<HelpLine> {
    let mut out: Vec<HelpLine> = Vec::new();
    for s in sections {
        if !out.is_empty() {
            out.push(HelpLine {
                kind: HelpLineKind::Blank,
                chord: String::new(),
                desc: String::new(),
                muted: false,
                prose: false,
            });
        }
        out.push(HelpLine {
            kind: HelpLineKind::Header,
            chord: String::new(),
            desc: s.section.clone(),
            muted: s.unavailable,
            prose: false,
        });
        for (chord, desc) in &s.keys {
            out.push(HelpLine {
                kind: HelpLineKind::Row,
                chord: chord.clone(),
                desc: desc.clone(),
                muted: s.unavailable || s.limits,
                prose: s.limits,
            });
        }
    }
    out
}

/// Bindings that can never fire, as `"mode chord"` strings. `a` shadows `b`
/// when every context `b` accepts is one `a` also accepts — `a`'s guards a
/// subset of `b`'s, and `a`'s tabs a superset.
pub fn dead_bindings(bindings: &[Binding]) -> Vec<String> {
    let mut dead: Vec<String> = Vec::new();
    let sig = |b: &Binding| {
        let mut when: Vec<String> = b.when.iter().map(|g| format!("{g:?}")).collect();
        when.sort();
        let mut not: Vec<String> = b.not.iter().map(|g| format!("{g:?}")).collect();
        not.sort();
        let tab = match &b.tab {
            Some(tabs) => {
                let mut t: Vec<&str> = tabs.iter().map(|t| t.id()).collect();
                t.sort();
                format!("@{}", t.join(","))
            }
            None => String::new(),
        };
        format!("{}/{}{}", when.join(","), not.join(","), tab)
    };
    for i in 0..bindings.len() {
        for j in i + 1..bindings.len() {
            let a = &bindings[i];
            let bb = &bindings[j];
            if !modes_overlap(a.mode, bb.mode) || a.chord != bb.chord {
                continue;
            }
            let tab_shadows = match (&a.tab, &bb.tab) {
                (None, _) => true,
                (Some(at), Some(bt)) => bt.iter().all(|t| at.contains(t)),
                (Some(_), None) => false,
            };
            let shadows = tab_shadows
                && a.when.iter().all(|g| bb.when.contains(g))
                && a.not.iter().all(|g| bb.not.contains(g));
            if shadows {
                let mode = match bb.mode {
                    None => "*".to_string(),
                    Some(m) => format!("{m:?}").to_lowercase(),
                };
                let s = sig(bb);
                if s == "/" {
                    dead.push(format!("{mode} {}", bb.chord));
                } else {
                    dead.push(format!("{mode} {} ({s})", bb.chord));
                }
            }
        }
    }
    dead
}

// ---------------------------------------------------------------------------
// Line editing (pure). Cursor is a CHAR index.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineState {
    pub text: String,
    pub cursor: usize,
}

pub const EMPTY_LINE: LineState = LineState {
    text: String::new(),
    cursor: 0,
};

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn from_chars(c: &[char]) -> String {
    c.iter().collect()
}

fn clamp(text: Vec<char>, cursor: usize) -> LineState {
    let len = text.len();
    LineState {
        text: from_chars(&text),
        cursor: cursor.min(len),
    }
}

/// Start of the logical line the cursor sits on (char index).
fn line_start(text: &[char], cursor: usize) -> usize {
    let mut i = cursor;
    while i > 0 {
        if text[i - 1] == '\n' {
            return i;
        }
        i -= 1;
    }
    0
}

fn line_end(text: &[char], cursor: usize) -> usize {
    let mut i = cursor;
    while i < text.len() {
        if text[i] == '\n' {
            return i;
        }
        i += 1;
    }
    text.len()
}

/// Move the cursor one visual line, keeping its column where it can. No
/// goal-column memory, deliberately.
fn move_line(s: &LineState, dir: i8) -> LineState {
    let text = chars(&s.text);
    let start = line_start(&text, s.cursor);
    let col = s.cursor - start;
    if dir < 0 {
        if start == 0 {
            return s.clone();
        }
        let prev_start = line_start(&text, start - 1);
        return clamp(text, (prev_start + col).min(start - 1));
    }
    let end = line_end(&text, s.cursor);
    if end >= text.len() {
        return s.clone();
    }
    let next_end = line_end(&text, end + 1);
    clamp(text, (end + 1 + col).min(next_end))
}

/// Readline word motion (mirrors format.ts).
pub fn word_left(text: &str, cursor: usize) -> usize {
    let t = chars(text);
    let mut i = cursor.min(t.len());
    while i > 0 && t[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !t[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

pub fn word_right(text: &str, cursor: usize) -> usize {
    let t = chars(text);
    let mut i = cursor.min(t.len());
    while i < t.len() && t[i].is_whitespace() {
        i += 1;
    }
    while i < t.len() && !t[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Apply an editing command. Returns an UNCHANGED clone on a no-op (the TS
/// same-object contract becomes value equality here).
pub fn edit_line(s: &LineState, command: Command) -> LineState {
    let text = chars(&s.text);
    match command {
        Command::CursorLeft => {
            if s.cursor == 0 {
                s.clone()
            } else {
                clamp(text, s.cursor - 1)
            }
        }
        Command::CursorRight => {
            if s.cursor >= text.len() {
                s.clone()
            } else {
                clamp(text, s.cursor + 1)
            }
        }
        Command::CursorHome => {
            let start = line_start(&text, s.cursor);
            clamp(text, start)
        }
        Command::CursorEnd => {
            let end = line_end(&text, s.cursor);
            clamp(text, end)
        }
        Command::CursorWordLeft => {
            let to = word_left(&s.text, s.cursor);
            clamp(text, to)
        }
        Command::CursorWordRight => {
            let to = word_right(&s.text, s.cursor);
            clamp(text, to)
        }
        Command::CursorUp => move_line(s, -1),
        Command::CursorDown => move_line(s, 1),

        Command::DeleteBack => {
            if s.cursor == 0 {
                return s.clone();
            }
            let mut t = text;
            t.remove(s.cursor - 1);
            LineState {
                text: from_chars(&t),
                cursor: s.cursor - 1,
            }
        }
        Command::DeleteForward => {
            if s.cursor >= text.len() {
                return s.clone();
            }
            let mut t = text;
            t.remove(s.cursor);
            LineState {
                text: from_chars(&t),
                cursor: s.cursor,
            }
        }
        Command::DeleteWordBack => {
            let from = word_left(&s.text, s.cursor);
            if from == s.cursor {
                return s.clone();
            }
            let mut t: Vec<char> = Vec::with_capacity(text.len());
            t.extend_from_slice(&text[..from]);
            t.extend_from_slice(&text[s.cursor..]);
            LineState {
                text: from_chars(&t),
                cursor: from,
            }
        }
        Command::DeleteToEnd => {
            let end = line_end(&text, s.cursor);
            if end == s.cursor {
                return s.clone();
            }
            let mut t: Vec<char> = Vec::with_capacity(text.len());
            t.extend_from_slice(&text[..s.cursor]);
            t.extend_from_slice(&text[end..]);
            LineState {
                text: from_chars(&t),
                cursor: s.cursor,
            }
        }
        Command::DeleteToStart => {
            let start = line_start(&text, s.cursor);
            if start == s.cursor {
                return s.clone();
            }
            let mut t: Vec<char> = Vec::with_capacity(text.len());
            t.extend_from_slice(&text[..start]);
            t.extend_from_slice(&text[s.cursor..]);
            LineState {
                text: from_chars(&t),
                cursor: start,
            }
        }
        Command::DeleteLine => {
            if s.text.is_empty() {
                s.clone()
            } else {
                EMPTY_LINE
            }
        }
        Command::Newline => insert_text(s, "\n"),

        _ => s.clone(),
    }
}

/// Insert text at the cursor. The one mutation a non-chord keypress makes.
pub fn insert_text(s: &LineState, text: &str) -> LineState {
    if text.is_empty() {
        return s.clone();
    }
    let t = chars(&s.text);
    let add = chars(text);
    let mut out: Vec<char> = Vec::with_capacity(t.len() + add.len());
    out.extend_from_slice(&t[..s.cursor]);
    out.extend_from_slice(&add);
    out.extend_from_slice(&t[s.cursor..]);
    LineState {
        text: from_chars(&out),
        cursor: s.cursor + add.len(),
    }
}

// ---------------------------------------------------------------------------
// Raw input
// ---------------------------------------------------------------------------

/// Invisible control bytes must never reach the draft — WHOLE SEQUENCES, never
/// just the escape byte (else `⌥⏎` types `[27;3;13~` into the draft).
///
/// Order mirrors the TS: SS3 (`ESC O <char>`) first, then CSI/OSC (strip-ansi's
/// territory), then any remaining `ESC <printable>` pair, then C0 bytes except
/// `\n`/`\t` (regex `[\x00-\x08\x0b-\x1f\x7f]`).
pub fn strip_ctl(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\u{1b}' {
            // A sequence goes whole or not at all.
            if i + 1 < cs.len() {
                match cs[i + 1] {
                    '[' => {
                        // CSI: params [0-?], intermediates [ -/], one final [@-~].
                        let mut j = i + 2;
                        while j < cs.len() && ('\u{30}'..='\u{3f}').contains(&cs[j]) {
                            j += 1;
                        }
                        while j < cs.len() && ('\u{20}'..='\u{2f}').contains(&cs[j]) {
                            j += 1;
                        }
                        if j < cs.len() && ('\u{40}'..='\u{7e}').contains(&cs[j]) {
                            i = j + 1;
                        } else {
                            i = j; // truncated sequence: drop what was there
                        }
                        continue;
                    }
                    ']' => {
                        // OSC: until BEL or ST (ESC \).
                        let mut j = i + 2;
                        while j < cs.len() {
                            if cs[j] == '\u{7}' {
                                j += 1;
                                break;
                            }
                            if cs[j] == '\u{1b}' && j + 1 < cs.len() && cs[j + 1] == '\\' {
                                j += 2;
                                break;
                            }
                            j += 1;
                        }
                        i = j;
                        continue;
                    }
                    'O' => {
                        // SS3: ESC O <one printable>.
                        if i + 2 < cs.len() && ('\u{20}'..='\u{7e}').contains(&cs[i + 2]) {
                            i += 3;
                        } else {
                            i += 2;
                        }
                        continue;
                    }
                    n if ('\u{20}'..='\u{7e}').contains(&n) => {
                        // Any other ESC <printable> pair: still a sequence.
                        i += 2;
                        continue;
                    }
                    _ => {
                        i += 1; // bare ESC before a control byte: drop the ESC
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }
        // C0 except \n and \t, plus DEL.
        let code = c as u32;
        if (code <= 0x08) || (0x0b..=0x1f).contains(&code) || code == 0x7f {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// What a coalesced stdin chunk means for the composer. Only a *trailing* `\r`
/// means "…then send"; a bare `\n` can only have come from ^j and is always a
/// literal newline. The old tree shipped the other rule and sent half messages.
pub fn chunk_input(chunk: &str) -> (String, bool) {
    let send = chunk.ends_with('\r');
    let body = if send {
        &chunk[..chunk.len() - 1]
    } else {
        chunk
    };
    // `\r\n?` → `\n`
    let mut normalized = String::with_capacity(body.len());
    let mut it = body.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(c);
        }
    }
    (strip_ctl(&normalized), send)
}

/// Is this keypress ordinary text rather than a chord?
pub fn is_text_input(input: &str, key: KeyFlags) -> bool {
    if input.is_empty() {
        return false;
    }
    if key.ctrl || key.meta || key.super_ {
        return false;
    }
    if key.r#return || key.escape || key.tab || key.backspace || key.delete {
        return false;
    }
    if key.up_arrow || key.down_arrow || key.left_arrow || key.right_arrow {
        return false;
    }
    if key.page_up || key.page_down || key.home || key.end {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/keys.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> KeyContext {
        KeyContext::default()
    }

    fn line(text: &str, cursor: usize) -> LineState {
        LineState {
            text: text.to_string(),
            cursor,
        }
    }

    // ---- chords ----

    #[test]
    fn chord_of_canonicalizes_modifiers_named_keys_and_plain_characters() {
        assert_eq!(
            chord_of(
                "p",
                KeyFlags {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            "ctrl+p"
        );
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    r#return: true,
                    meta: true,
                    ..Default::default()
                }
            ),
            "meta+enter"
        );
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    escape: true,
                    ..Default::default()
                }
            ),
            "esc"
        );
        assert_eq!(chord_of("?", KeyFlags::default()), "?");
        assert_eq!(chord_of(" ", KeyFlags::default()), "space");
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    up_arrow: true,
                    ..Default::default()
                }
            ),
            "up"
        );
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    tab: true,
                    shift: true,
                    ..Default::default()
                }
            ),
            "shift+tab"
        );
        // Backspace and delete flags both mean backspace.
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    backspace: true,
                    ..Default::default()
                }
            ),
            "backspace"
        );
        assert_eq!(
            chord_of(
                "",
                KeyFlags {
                    delete: true,
                    ..Default::default()
                }
            ),
            "backspace"
        );
        assert_eq!(
            chord_of(
                "v",
                KeyFlags {
                    super_: true,
                    ..Default::default()
                }
            ),
            "super+v"
        );
    }

    #[test]
    fn ctrl_j_is_one_chord_however_the_terminal_spells_it() {
        assert_eq!(chord_of("\n", KeyFlags::default()), "ctrl+j");
        assert_eq!(
            chord_of(
                "\n",
                KeyFlags {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            "ctrl+j"
        );
        // \r with the return flag is enter, never ^j.
        assert_eq!(
            chord_of(
                "\r",
                KeyFlags {
                    r#return: true,
                    ..Default::default()
                }
            ),
            "enter"
        );
    }

    #[test]
    fn a_coalesced_chunk_is_not_a_chord_it_is_text() {
        assert_eq!(chord_of("hello", KeyFlags::default()), "");
        assert_eq!(chord_of("ab", KeyFlags::default()), "");
        assert_eq!(chord_of("", KeyFlags::default()), "");
    }

    #[test]
    fn chord_label_prints_what_the_overlay_shows() {
        assert_eq!(chord_label("ctrl+p"), "^p");
        assert_eq!(chord_label("meta+enter"), "⌥⏎");
        assert_eq!(chord_label("super+left"), "⌘←");
        assert_eq!(chord_label("esc"), "esc");
        assert_eq!(chord_label("shift+tab"), "⇧⇥");
        assert_eq!(chord_label("pageup"), "pgup");
    }

    // ---- the table ----

    #[test]
    fn no_binding_is_dead_nothing_is_shadowed_by_an_earlier_row() {
        assert_eq!(dead_bindings(&BINDINGS), Vec::<String>::new());
    }

    #[test]
    fn dead_bindings_catches_the_two_ways_a_row_goes_dead() {
        use Command as C;
        use UiMode as M;
        // Identical guards: the second row is dead.
        let dup = vec![
            b(Some(M::Chat), "x", C::Send),
            b(Some(M::Chat), "x", C::Cancel),
        ];
        assert_eq!(dead_bindings(&dup).len(), 1);
        // An unguarded row ahead of a guarded one: the guarded one is dead.
        let shadowed = vec![
            b(Some(M::Chat), "esc", C::Cancel),
            b(Some(M::Chat), "esc", C::TurnInterrupt).when(&[Guard::Busy]),
        ];
        assert_eq!(dead_bindings(&shadowed).len(), 1);
        // Complementary guards are the design, not a bug.
        let fine = vec![
            b(Some(M::Chat), "esc", C::TurnInterrupt).when(&[Guard::Busy]),
            b(Some(M::Chat), "esc", C::Cancel),
        ];
        assert_eq!(dead_bindings(&fine), Vec::<String>::new());
        // Disjoint tab sets on one chord are the design too.
        let tabs = vec![
            b(Some(M::Panel), "x", C::WfStop).tabs(&[PanelTab::Workflows]),
            b(Some(M::Panel), "x", C::ChangesRevert).tabs(&[PanelTab::Changes]),
        ];
        assert_eq!(dead_bindings(&tabs), Vec::<String>::new());
        // An unscoped row ahead of a scoped one still kills it.
        let unscoped = vec![
            b(Some(M::Panel), "x", C::Cancel),
            b(Some(M::Panel), "x", C::WfStop).tabs(&[PanelTab::Workflows]),
        ];
        assert_eq!(dead_bindings(&unscoped).len(), 1);
    }

    #[test]
    fn the_same_chord_means_two_things_and_the_guard_decides_which() {
        let empty = ctx();
        let typing = KeyContext {
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&empty, "ctrl+f"), Some(Command::Tab(PanelTab::Tree)));
        assert_eq!(lookup(&typing, "ctrl+f"), Some(Command::CursorRight));
        assert_eq!(lookup(&empty, "ctrl+e"), Some(Command::FoldAll));
        assert_eq!(lookup(&typing, "ctrl+e"), Some(Command::CursorEnd));
        let multi = KeyContext {
            multiline: true,
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&multi, "up"), Some(Command::CursorUp));
        assert_eq!(lookup(&ctx(), "up"), Some(Command::HistoryPrev));
    }

    #[test]
    fn down_enters_the_rail_only_when_a_subagent_is_actually_working() {
        let live = KeyContext {
            rail_live: true,
            ..ctx()
        };
        assert_eq!(lookup(&live, "down"), Some(Command::RailEnter));
        assert_eq!(lookup(&ctx(), "down"), Some(Command::HistoryNext));
        let typing = KeyContext {
            empty_draft: false,
            rail_live: true,
            ..ctx()
        };
        assert_eq!(lookup(&typing, "down"), Some(Command::HistoryNext));
    }

    #[test]
    fn a_single_ctrl_c_arms_a_second_quits_in_every_mode() {
        for mode in [
            UiMode::Chat,
            UiMode::Panel,
            UiMode::Help,
            UiMode::Rail,
            UiMode::Ask,
        ] {
            let unarmed = KeyContext { mode, ..ctx() };
            assert_eq!(
                lookup(&unarmed, "ctrl+c"),
                Some(Command::QuitArm),
                "{mode:?}"
            );
            let armed = KeyContext {
                mode,
                quit_armed: true,
                ..ctx()
            };
            assert_eq!(lookup(&armed, "ctrl+c"), Some(Command::Quit), "{mode:?}");
        }
    }

    #[test]
    fn esc_alone_cancels_esc_esc_clears_the_draft() {
        assert_eq!(lookup(&ctx(), "esc"), Some(Command::Cancel));
        let double_typed = KeyContext {
            double_esc: true,
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&double_typed, "esc"), Some(Command::DraftClear));
        // With nothing typed there is nothing to clear: the double-tap must
        // FALL THROUGH rather than swallow the gesture.
        let double_busy = KeyContext {
            double_esc: true,
            busy: true,
            ..ctx()
        };
        assert_eq!(lookup(&double_busy, "esc"), Some(Command::TurnInterrupt));
    }

    #[test]
    fn esc_unwinds_exactly_one_level_popup_then_turn_then_notice() {
        // The picker's own legend says `esc closes`, so it must — even mid-turn.
        let popup = KeyContext {
            completing: true,
            busy: true,
            ..ctx()
        };
        assert_eq!(lookup(&popup, "esc"), Some(Command::CompleteDismiss));
        let busy = KeyContext {
            busy: true,
            ..ctx()
        };
        assert_eq!(lookup(&busy, "esc"), Some(Command::TurnInterrupt));
        assert_eq!(lookup(&ctx(), "esc"), Some(Command::Cancel));
    }

    #[test]
    fn esc_inside_the_take_back_window_unsends_outside_it_it_stops_the_turn() {
        let sent_busy = KeyContext {
            just_sent: true,
            busy: true,
            ..ctx()
        };
        assert_eq!(lookup(&sent_busy, "esc"), Some(Command::MessageUnsend));
        let busy = KeyContext {
            busy: true,
            ..ctx()
        };
        assert_eq!(lookup(&busy, "esc"), Some(Command::TurnInterrupt));
        // A queued send arms it with no turn of this client's running.
        let sent_idle = KeyContext {
            just_sent: true,
            ..ctx()
        };
        assert_eq!(lookup(&sent_idle, "esc"), Some(Command::MessageUnsend));
    }

    #[test]
    fn the_take_back_never_costs_a_draft_and_never_outranks_a_popup() {
        let typed_busy = KeyContext {
            just_sent: true,
            busy: true,
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&typed_busy, "esc"), Some(Command::TurnInterrupt));
        let typed_idle = KeyContext {
            just_sent: true,
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&typed_idle, "esc"), Some(Command::Cancel));
        let with_popup = KeyContext {
            just_sent: true,
            busy: true,
            completing: true,
            ..ctx()
        };
        assert_eq!(lookup(&with_popup, "esc"), Some(Command::CompleteDismiss));
    }

    #[test]
    fn enter_commits_the_highlighted_completion_before_it_sends() {
        let popup = KeyContext {
            completing: true,
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&popup, "enter"), Some(Command::CompleteAccept));
        let typing = KeyContext {
            empty_draft: false,
            ..ctx()
        };
        assert_eq!(lookup(&typing, "enter"), Some(Command::Send));
        assert_eq!(lookup(&popup, "tab"), Some(Command::CompleteAccept));
        assert_eq!(lookup(&typing, "tab"), Some(Command::GhostAccept));
    }

    #[test]
    fn the_panel_binds_its_own_keys_and_nothing_from_chat() {
        let panel = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Model),
            ..ctx()
        };
        assert_eq!(lookup(&panel, "up"), Some(Command::MoveUp));
        assert_eq!(lookup(&panel, "j"), Some(Command::MoveDown));
        assert_eq!(lookup(&panel, "k"), Some(Command::MoveUp));
        assert_eq!(lookup(&panel, "enter"), Some(Command::PanelConfirm));
        assert_eq!(lookup(&panel, "tab"), Some(Command::PanelNext));
        assert_eq!(lookup(&panel, "shift+tab"), Some(Command::PanelPrev));
        assert_eq!(lookup(&panel, "esc"), Some(Command::PanelClose));
        // Chat's send is not reachable from the panel.
        assert_eq!(lookup(&panel, "meta+enter"), None);
    }

    #[test]
    fn a_bare_letter_means_what_the_open_tab_says_it_means() {
        let wf = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Workflows),
            ..ctx()
        };
        assert_eq!(lookup(&wf, "p"), Some(Command::WfPause));
        assert_eq!(lookup(&wf, "P"), Some(Command::WfResume));
        assert_eq!(lookup(&wf, "x"), Some(Command::WfStop));
        assert_eq!(lookup(&wf, "r"), Some(Command::WfRerun));
        let changes = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Changes),
            ..ctx()
        };
        assert_eq!(lookup(&changes, "x"), Some(Command::ChangesRevert));
        assert_eq!(lookup(&changes, "X"), Some(Command::ChangesRevertAll));
        // Outside its tab a letter is nothing at all — including with no tab.
        let model = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Model),
            ..ctx()
        };
        assert_eq!(lookup(&model, "p"), None);
        assert_eq!(lookup(&model, "x"), None);
        let closed = KeyContext {
            mode: UiMode::Panel,
            tab: None,
            ..ctx()
        };
        assert_eq!(lookup(&closed, "x"), None);
        // The tree's letters.
        let tree = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Tree),
            ..ctx()
        };
        assert_eq!(lookup(&tree, "s"), Some(Command::PanelConfirmSummarize));
        assert_eq!(lookup(&tree, "e"), Some(Command::TreeExtract));
        assert_eq!(lookup(&tree, "m"), Some(Command::TreeMoveInto));
        // The mcp tab's.
        let mcp = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Mcp),
            ..ctx()
        };
        assert_eq!(lookup(&mcp, "a"), Some(Command::McpAuth));
        assert_eq!(lookup(&mcp, "n"), Some(Command::McpAdd));
        assert_eq!(lookup(&mcp, "F"), Some(Command::McpForget));
        assert_eq!(lookup(&mcp, "c"), Some(Command::McpConnect));
        assert_eq!(lookup(&mcp, "r"), Some(Command::McpRestart));
        assert_eq!(lookup(&mcp, "d"), Some(Command::McpRemove));
    }

    #[test]
    fn the_workflow_verbs_the_run_view_prints_are_all_bound() {
        let wf = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Workflows),
            ..ctx()
        };
        assert_eq!(lookup(&wf, "e"), Some(Command::WfScript));
        assert_eq!(lookup(&wf, "f"), Some(Command::WfFilter));
        assert_eq!(lookup(&wf, "o"), Some(Command::WfOpenAgent));
        assert_eq!(lookup(&wf, "s"), Some(Command::WfSave));
    }

    #[test]
    fn digits_address_panel_rows_and_pgup_pgdn_page_them() {
        let panel = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Model),
            ..ctx()
        };
        for d in ["1", "5", "9"] {
            assert_eq!(lookup(&panel, d), Some(Command::PanelPick));
        }
        assert_eq!(lookup(&panel, "pageup"), Some(Command::MovePageUp));
        assert_eq!(lookup(&panel, "pagedown"), Some(Command::MovePageDown));
    }

    #[test]
    fn the_filter_buffer_takes_the_keyboard_and_gives_every_letter_back_as_text() {
        // `/` opens it, but only where a list is long enough to need narrowing.
        for tab in FILTER_TABS {
            let c = KeyContext {
                mode: UiMode::Panel,
                tab: Some(tab),
                ..ctx()
            };
            assert_eq!(lookup(&c, "/"), Some(Command::PanelFilter), "{tab:?}");
        }
        let changes = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Changes),
            ..ctx()
        };
        assert_eq!(lookup(&changes, "/"), None);
        // While it is open, every bare letter and digit in the panel is unbound.
        let filtering = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Model),
            panel_filtering: true,
            ..ctx()
        };
        for chord in ["j", "k", "1", "9", "/"] {
            assert_eq!(lookup(&filtering, chord), None, "{chord}");
        }
        let wf_filtering = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Workflows),
            panel_filtering: true,
            ..ctx()
        };
        assert_eq!(lookup(&wf_filtering, "x"), None);
        // Arrows still move and ⏎ still commits.
        assert_eq!(lookup(&filtering, "up"), Some(Command::MoveUp));
        assert_eq!(lookup(&filtering, "enter"), Some(Command::PanelConfirm));
        // Escape unwinds ONE level — the buffer, not the panel.
        assert_eq!(lookup(&filtering, "esc"), Some(Command::PanelFilterExit));
        let plain = KeyContext {
            mode: UiMode::Panel,
            tab: Some(PanelTab::Model),
            ..ctx()
        };
        assert_eq!(lookup(&plain, "esc"), Some(Command::PanelClose));
        assert_eq!(
            lookup(&filtering, "backspace"),
            Some(Command::PanelFilterBack)
        );
        // The model tab's ⇥ while filtering reaches the OTHER search box.
        assert_eq!(lookup(&filtering, "tab"), Some(Command::PanelFilterTier));
    }

    #[test]
    fn the_rail_can_stop_what_it_lists() {
        let rail = KeyContext {
            mode: UiMode::Rail,
            ..ctx()
        };
        assert_eq!(lookup(&rail, "x"), Some(Command::RailStop));
        // …and only there: `x` in the composer is a character.
        assert_eq!(lookup(&ctx(), "x"), None);
    }

    #[test]
    fn every_tab_has_exactly_one_chord_and_panel_toggle_names_no_tab() {
        let mut chords: Vec<&str> = vec![PANEL_TOGGLE];
        chords.extend(TABS.iter().map(|t| t.chord));
        let unique: std::collections::HashSet<&&str> = chords.iter().collect();
        assert_eq!(unique.len(), chords.len());
        assert_eq!(tab_for_chord(PANEL_TOGGLE), None);
        assert_eq!(tab_for_chord("ctrl+zzz"), None);
        for t in TABS.iter() {
            let command = lookup(&ctx(), t.chord);
            assert_eq!(command, Some(Command::Tab(t.id)), "{}", t.chord);
            assert_eq!(tab_for_command(command.unwrap()), Some(t.id));
            assert_eq!(tab_for_chord(t.chord), Some(t.id));
        }
        assert_eq!(tab_for_command(Command::PanelToggle), None);
        assert_eq!(tab_for_command(Command::Send), None);
    }

    #[test]
    fn sessions_alias_lands_on_the_tree_in_all_four_modes() {
        for mode in [UiMode::Chat, UiMode::Panel, UiMode::Rail, UiMode::Ask] {
            let c = KeyContext { mode, ..ctx() };
            assert_eq!(
                lookup(&c, SESSIONS_ALIAS),
                Some(Command::Tab(PanelTab::Tree)),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn resolve_is_lookup_straight_off_a_keypress() {
        assert_eq!(
            resolve(
                &ctx(),
                "",
                KeyFlags {
                    escape: true,
                    ..Default::default()
                }
            ),
            Some(Command::Cancel)
        );
        assert_eq!(resolve(&ctx(), "hello world", KeyFlags::default()), None);
    }

    #[test]
    fn job_mode_closes_back_to_the_rail_and_kills_with_x() {
        let job = KeyContext {
            mode: UiMode::Job,
            ..ctx()
        };
        assert_eq!(lookup(&job, "esc"), Some(Command::JobClose));
        assert_eq!(lookup(&job, "q"), Some(Command::JobClose));
        assert_eq!(lookup(&job, "left"), Some(Command::JobClose));
        assert_eq!(lookup(&job, "up"), Some(Command::ScrollUp));
        assert_eq!(lookup(&job, "j"), Some(Command::ScrollDown));
        assert_eq!(lookup(&job, "pageup"), Some(Command::ScrollPageUp));
        assert_eq!(lookup(&job, "x"), Some(Command::JobStop));
    }

    // ---- the help overlay ----

    #[test]
    fn a_guarded_row_says_which_state_it_belongs_to_both_ways_round() {
        let sections = help_sections(&BINDINGS);
        let all: Vec<&(String, String)> = sections.iter().flat_map(|s| s.keys.iter()).collect();
        assert!(all.iter().any(|(_, d)| d.contains("· empty draft")));
        assert!(all.iter().any(|(_, d)| d.contains("· with a draft")));
    }

    #[test]
    fn the_not_bound_section_is_true_none_of_those_chords_is_bound() {
        // ^r, ^z are not bound anywhere.
        for chord in ["ctrl+r", "ctrl+z", "meta+d"] {
            for mode in [
                UiMode::Chat,
                UiMode::Panel,
                UiMode::Rail,
                UiMode::Ask,
                UiMode::Help,
                UiMode::Job,
            ] {
                let c = KeyContext {
                    mode,
                    empty_draft: false,
                    ..ctx()
                };
                assert_eq!(lookup(&c, chord), None, "{chord} in {mode:?}");
            }
        }
    }

    #[test]
    fn every_section_header_survives_flattening_and_carries_its_rows() {
        let sections = help_sections(&BINDINGS);
        let lines = help_lines(&sections);
        let headers: Vec<&str> = lines
            .iter()
            .filter(|l| l.kind == HelpLineKind::Header)
            .map(|l| l.desc.as_str())
            .collect();
        for s in &sections {
            assert!(headers.contains(&s.section.as_str()), "{}", s.section);
        }
        // Each section's row count is intact under its header.
        let total_rows = lines.iter().filter(|l| l.kind == HelpLineKind::Row).count();
        let expected: usize = sections.iter().map(|s| s.keys.len()).sum();
        assert_eq!(total_rows, expected);
    }

    #[test]
    fn the_overlay_is_taller_than_a_terminal_which_is_why_it_scrolls() {
        assert!(help_lines(&help_sections(&BINDINGS)).len() > 24);
    }

    #[test]
    fn the_prose_sections_carry_no_key_column_of_their_own() {
        let lines = help_lines(&[limits_section()]);
        for l in lines.iter().filter(|l| l.kind == HelpLineKind::Row) {
            assert!(l.prose);
            assert!(l.muted);
            assert_eq!(l.chord, "");
        }
        let unavailable = help_lines(&[unavailable_section()]);
        for l in unavailable.iter().filter(|l| l.kind == HelpLineKind::Row) {
            assert!(l.muted);
            assert!(!l.prose, "not-bound rows keep their chord column");
        }
    }

    #[test]
    fn the_clipboard_image_gesture_is_reachable_by_a_chord_terminals_actually_send() {
        assert_eq!(lookup(&ctx(), "ctrl+v"), Some(Command::ImagePaste));
        assert_eq!(lookup(&ctx(), "super+v"), Some(Command::ImagePaste));
        assert_eq!(lookup(&ctx(), "meta+v"), Some(Command::ImagePaste));
    }

    // ---- line editing ----

    #[test]
    fn cursor_motion_clamps_at_both_ends_and_is_a_no_op_at_them() {
        let start = line("abc", 0);
        assert_eq!(edit_line(&start, Command::CursorLeft), start);
        let end = line("abc", 3);
        assert_eq!(edit_line(&end, Command::CursorRight), end);
        assert_eq!(
            edit_line(&line("abc", 1), Command::CursorRight),
            line("abc", 2)
        );
    }

    #[test]
    fn home_end_are_the_logical_lines_not_the_whole_drafts() {
        let s = line("first\nsecond", 8); // inside "second"
        assert_eq!(edit_line(&s, Command::CursorHome), line("first\nsecond", 6));
        assert_eq!(edit_line(&s, Command::CursorEnd), line("first\nsecond", 12));
    }

    #[test]
    fn up_down_hold_the_column_against_the_line_they_land_on_and_stop_at_the_ends() {
        let text = "hello\nhi\nworld";
        let up = edit_line(&line(text, 13), Command::CursorUp); // column 4 of "world"
        assert_eq!(up, line(text, 8)); // "hi" is shorter: land on its end
        assert_eq!(edit_line(&up, Command::CursorUp), line(text, 2));
        assert_eq!(edit_line(&up, Command::CursorDown), line(text, 11));
        let only = line("one", 1);
        assert_eq!(edit_line(&only, Command::CursorUp), only);
        assert_eq!(edit_line(&only, Command::CursorDown), only);
    }

    #[test]
    fn word_motion_and_word_delete_agree_on_where_a_word_starts() {
        let s = line("alpha beta gamma", 16);
        let back = edit_line(&s, Command::CursorWordLeft);
        assert_eq!(back.cursor, 11);
        assert_eq!(
            edit_line(&s, Command::DeleteWordBack),
            line("alpha beta ", 11)
        );
    }

    #[test]
    fn the_kill_keys_cut_to_the_ends_of_the_current_line_only() {
        let s = line("first\nsecond half", 12); // inside "second half"
        assert_eq!(
            edit_line(&s, Command::DeleteToEnd),
            line("first\nsecond", 12)
        );
        assert_eq!(
            edit_line(&s, Command::DeleteToStart),
            line("first\n half", 6)
        );
        assert_eq!(edit_line(&s, Command::DeleteLine), EMPTY_LINE);
    }

    #[test]
    fn backspace_and_delete_forward_move_the_cursor_the_way_each_should() {
        assert_eq!(
            edit_line(&line("abc", 2), Command::DeleteBack),
            line("ac", 1)
        );
        assert_eq!(
            edit_line(&line("abc", 1), Command::DeleteForward),
            line("ac", 1)
        );
        let at_start = line("abc", 0);
        assert_eq!(edit_line(&at_start, Command::DeleteBack), at_start);
        let at_end = line("abc", 3);
        assert_eq!(edit_line(&at_end, Command::DeleteForward), at_end);
    }

    #[test]
    fn newline_inserts_at_the_cursor_rather_than_sending() {
        assert_eq!(edit_line(&line("ab", 1), Command::Newline), line("a\nb", 2));
        assert_eq!(insert_text(&line("ab", 1), "XY"), line("aXYb", 3));
    }

    // ---- raw input ----

    #[test]
    fn only_a_trailing_cr_sends_a_coalesced_chunk() {
        assert_eq!(chunk_input("hello\r"), ("hello".to_string(), true));
        // ^j after fast typing arrives in the same read and must NOT send.
        assert_eq!(chunk_input("hello\n"), ("hello\n".to_string(), false));
        assert_eq!(
            chunk_input("two\r\nlines\r"),
            ("two\nlines".to_string(), true)
        );
    }

    #[test]
    fn strip_ctl_removes_invisible_bytes_but_keeps_newlines_and_tabs_out_of_harm() {
        assert_eq!(strip_ctl("a\u{0}b\u{7}c"), "abc");
        assert_eq!(strip_ctl("keep\nthe newline"), "keep\nthe newline");
        assert_eq!(strip_ctl("\u{1b}[31mred"), "red");
    }

    #[test]
    fn is_text_input_tells_typing_from_a_chord() {
        assert!(is_text_input("a", KeyFlags::default()));
        assert!(!is_text_input(
            "a",
            KeyFlags {
                ctrl: true,
                ..Default::default()
            }
        ));
        assert!(!is_text_input(
            "",
            KeyFlags {
                up_arrow: true,
                ..Default::default()
            }
        ));
        assert!(!is_text_input(
            "\r",
            KeyFlags {
                r#return: true,
                ..Default::default()
            }
        ));
        assert!(!is_text_input("", KeyFlags::default()));
    }

    #[test]
    fn an_escape_sequence_is_dropped_whole_never_typed_into_the_draft() {
        // Alt+Enter under the kitty / modifyOtherKeys encoding.
        assert_eq!(strip_ctl("\u{1b}[27;3;13~"), "");
        assert_eq!(strip_ctl("hi\u{1b}[27;3;13~there"), "hithere");
        assert_eq!(strip_ctl("\u{1b}[1;5D"), ""); // CSI, ctrl+left
        assert_eq!(strip_ctl("\u{1b}OP"), ""); // SS3, F1
        assert_eq!(strip_ctl("\u{1b}[200~pasted\u{1b}[201~"), "pasted"); // bracketed paste
        assert_eq!(strip_ctl("\u{1b}[31mred\u{1b}[39m"), "red"); // SGR from a paste
                                                                 // Ordinary text, including punctuation that merely LOOKS like a sequence.
        assert_eq!(strip_ctl("a[27;3;13~b"), "a[27;3;13~b");
        assert_eq!(strip_ctl("emoji 🎉 and 日本語"), "emoji 🎉 and 日本語");
        // Newlines and tabs are content, not control noise.
        assert_eq!(strip_ctl("one\ntwo\tthree"), "one\ntwo\tthree");
    }

    // ---- slash commands ----

    #[test]
    fn slash_command_for_a_draft_that_is_a_command_and_nothing_looser() {
        assert_eq!(
            slash_command_for("/model"),
            Some(Command::Tab(PanelTab::Model))
        );
        assert_eq!(
            slash_command_for("  /tree "),
            Some(Command::Tab(PanelTab::Tree))
        );
        assert_eq!(slash_command_for("/HELP"), Some(Command::HelpOpen));
        assert_eq!(slash_command_for("/new"), Some(Command::SessionNew));
        // Free from the TABS table rather than a second registration — a tab
        // and its `/name` cannot drift apart because they are one row.
        assert_eq!(
            slash_command_for("/recap"),
            Some(Command::Tab(PanelTab::Recap))
        );
        assert_eq!(slash_command_for("/rewind"), Some(Command::TreeRewind));
        // Prose about a command is prose.
        assert_eq!(slash_command_for("/help me name this"), None);
        assert_eq!(slash_command_for("model"), None);
        assert_eq!(slash_command_for("/"), None);
        assert_eq!(slash_command_for("/nope"), None);
    }

    #[test]
    fn slash_invocation_an_argument_reaches_the_commands_that_declare_one() {
        assert_eq!(
            slash_invocation("/compact focus on the parser"),
            Some((Command::SessionCompact, "focus on the parser".to_string()))
        );
        assert_eq!(
            slash_invocation("/compact"),
            Some((Command::SessionCompact, String::new()))
        );
        // A no-arg command with trailing text is NOT an invocation.
        assert_eq!(slash_invocation("/help me name this"), None);
        assert_eq!(slash_invocation("plain prose"), None);
    }

    #[test]
    fn a_bare_slash_word_that_is_not_a_command_is_caught_with_the_nearest_name() {
        // Foreign commands map to the bough name, never silently alias.
        assert_eq!(
            unknown_command("/clear", &[]),
            Some(("clear".into(), Some("new".into())))
        );
        assert_eq!(
            unknown_command("/resume", &[]),
            Some(("resume".into(), Some("tree".into())))
        );
        assert_eq!(
            unknown_command("/cost", &[]),
            Some(("cost".into(), Some("model".into())))
        );
        assert_eq!(unknown_command("/quit", &[]), Some(("quit".into(), None)));
        // A real command or a skill passes through untouched.
        assert_eq!(unknown_command("/model", &[]), None);
        assert_eq!(unknown_command("/prewalk", &["prewalk"]), None);
        // Prefix suggestion.
        let (name, suggestion) = unknown_command("/mod", &[]).unwrap();
        assert_eq!(name, "mod");
        assert_eq!(suggestion.as_deref(), Some("model"));
        // Not a lone /word: not intercepted.
        assert_eq!(unknown_command("/prewalk fix the parser", &[]), None);
        assert_eq!(unknown_command("prose", &[]), None);
    }

    #[test]
    fn unsend_window_is_the_number_the_help_states() {
        assert_eq!(UNSEND_MS, 3_000);
    }
}
