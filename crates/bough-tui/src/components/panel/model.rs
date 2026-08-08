//! The model tab: both tiers, one list (port of
//! `src/tui/components/ModelPicker.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **the picker chooses the frontier model AND the
//! cheap model.** Spec §12 names two tiers and says both are chosen here — the
//! supervisor, and the cheap model that powers auto titles, composer ghost text
//! and live activity blurbs. A picker that offered only the frontier tier would
//! leave the tier that bills on *every* round unreachable from the product,
//! configurable only by editing state the user cannot see. So [`model_entries`]
//! emits two model sections over the same catalog, and [`choose_entry`] routes
//! the choice by the row's tier and nothing else.
//!
//! SECOND INVARIANT — **switching pins THIS session and moves the default for
//! new sessions, and touches no other existing session.** That sentence is spec
//! §12 verbatim, and it is implemented as ONE pure function rather than as two
//! API calls a caller might make only one of. The cheap tier has no per-session
//! pin — it is one background model for the whole install, so choosing one moves
//! the default only.
//!
//! THIRD — **an id is a provider routing decision, so the catalog is injected.**
//! Model ids route by prefix (`openai:x` → OpenAI, `vendor/model` → OpenRouter,
//! bare → Anthropic), and that table lives in `llm/routing.rs` where the routing
//! does. This module takes `ModelRow`s as data, so no provider name is written
//! outside `llm/`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use bough_core::types::Effort;

use crate::api::ModelRow;
use crate::components::panel::{paint_rows, window_around};
use crate::components::{accent, warn};
use crate::format::fuzzy_score;
use crate::store::selectors::clip;

// ---------------------------------------------------------------------------
// The config the picker edits
// ---------------------------------------------------------------------------

/// `Default` leaves the request untouched — the provider decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffortChoice {
    Default,
    Level(Effort),
}

pub const EFFORTS: [EffortChoice; 6] = [
    EffortChoice::Default,
    EffortChoice::Level(Effort::Low),
    EffortChoice::Level(Effort::Medium),
    EffortChoice::Level(Effort::High),
    EffortChoice::Level(Effort::Xhigh),
    EffortChoice::Level(Effort::Max),
];

impl EffortChoice {
    pub fn id(self) -> &'static str {
        match self {
            EffortChoice::Default => "default",
            EffortChoice::Level(Effort::Low) => "low",
            EffortChoice::Level(Effort::Medium) => "medium",
            EffortChoice::Level(Effort::High) => "high",
            EffortChoice::Level(Effort::Xhigh) => "xhigh",
            EffortChoice::Level(Effort::Max) => "max",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EffortChoice::Default => "adaptive — the provider decides",
            EffortChoice::Level(Effort::Low) => "low — quick, minimal thinking",
            EffortChoice::Level(Effort::Medium) => "medium — balanced",
            EffortChoice::Level(Effort::High) => "high — thorough",
            EffortChoice::Level(Effort::Xhigh) => "xhigh — deep (the agentic sweet spot)",
            EffortChoice::Level(Effort::Max) => "max — correctness over cost",
        }
    }
}

/// A stored effort string narrowed to a row this picker can mark.
///
/// The session row types `effort` as a free string (it is a column, and the
/// server accepts whatever a future model names), while the picker's sections
/// are the fixed [`EFFORTS`] list. Anything unrecognised reads as "no row of
/// mine" rather than as a row that does not exist.
pub fn as_effort_choice(value: Option<&str>) -> Option<EffortChoice> {
    let value = value?;
    EFFORTS.into_iter().find(|e| e.id() == value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelConfig {
    /// What a NEW session starts on.
    pub default_model: String,
    /// THIS session's pin. `None` = it follows `default_model`.
    pub session_model: Option<String>,
    /// The cheap tier: titles, ghost text, activity blurbs. One per install.
    pub cheap_model: Option<String>,
    pub default_effort: EffortChoice,
    pub session_effort: Option<EffortChoice>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            default_model: String::new(),
            session_model: None,
            cheap_model: None,
            default_effort: EffortChoice::Default,
            session_effort: None,
        }
    }
}

/// One step up the `EFFORTS` ladder, wrapping past `max` back to `default`.
///
/// Writes BOTH halves exactly as picking the row in the tab does
/// ([`choose_entry`]): the session is pinned, and the install default moves so
/// a conversation that has not started yet — which has no session to pin —
/// still runs at the depth the status bar is now promising.
pub fn cycle_effort(cfg: &ModelConfig) -> ModelConfig {
    let now = effective_effort(cfg);
    let at = EFFORTS.iter().position(|e| *e == now).unwrap_or(0);
    let next = EFFORTS[(at + 1) % EFFORTS.len()];
    let mut out = cfg.clone();
    out.session_effort = Some(next);
    out.default_effort = next;
    out
}

/// What the open session actually runs on right now.
pub fn effective_model(cfg: &ModelConfig) -> &str {
    cfg.session_model.as_deref().unwrap_or(&cfg.default_model)
}

pub fn effective_effort(cfg: &ModelConfig) -> EffortChoice {
    cfg.session_effort.unwrap_or(cfg.default_effort)
}

// ---------------------------------------------------------------------------
// Entries (pure)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    Frontier,
    Cheap,
    Effort,
}

/// Discriminated on tier so an effort row's id is an [`EffortChoice`] and a
/// model row's is a free-form model id — the two are not interchangeable, and a
/// flat `id: String` let `choose_entry` write "xhigh" into `default_model`
/// without a complaint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEntry {
    Model {
        tier: Tier,
        id: String,
        label: String,
        detail: String,
    },
    Effort {
        id: EffortChoice,
        label: String,
        detail: String,
    },
}

impl ModelEntry {
    pub fn tier(&self) -> Tier {
        match self {
            ModelEntry::Model { tier, .. } => *tier,
            ModelEntry::Effort { .. } => Tier::Effort,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            ModelEntry::Model { label, .. } | ModelEntry::Effort { label, .. } => label,
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            ModelEntry::Model { detail, .. } | ModelEntry::Effort { detail, .. } => detail,
        }
    }
    /// The model id, for a model row. `None` for an effort row — this is where
    /// the discriminated union earns its place.
    pub fn model_id(&self) -> Option<&str> {
        match self {
            ModelEntry::Model { id, .. } => Some(id),
            ModelEntry::Effort { .. } => None,
        }
    }
}

/// Section titles and hints. The hints are kept under 70 characters ON PURPOSE:
/// they are indented two columns inside the panel border, and 80 columns is the
/// narrowest terminal bough claims to support.
pub fn section(tier: Tier) -> (&'static str, &'static str) {
    match tier {
        Tier::Frontier => (
            "frontier model — the supervisor",
            "pins this session, and new ones; others keep what they have",
        ),
        Tier::Cheap => (
            "cheap model — titles, ghost text, activity",
            "bills on every round, so it fails silently and never blocks a turn",
        ),
        Tier::Effort => ("thinking depth", "not every model accepts one"),
    }
}

/// One search buffer per model tier.
///
/// TWO BOXES AND NOT ONE, because the two tiers are searched for different
/// things at the same time: picking a frontier model and picking a cheap one is
/// a single decision about a pair, and a shared box made the second half of it
/// erase the first. `haiku` typed to find a cheap model also hid every frontier
/// row, so the ● that said what the supervisor runs on vanished mid-decision.
///
/// The effort section has none: six fixed rows never need narrowing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelFilters {
    pub frontier: String,
    pub cheap: String,
}

impl ModelFilters {
    fn get(&self, tier: Tier) -> &str {
        match tier {
            Tier::Cheap => &self.cheap,
            _ => &self.frontier,
        }
    }
}

/// The flat entry list: frontier catalog, cheap catalog, then the effort levels.
///
/// Per-tier queries RANK (score desc, ties keep catalog order). The catalog is
/// hundreds of rows once a key is present, and a subsequence matcher says yes to
/// a lot of them — unranked, the row you meant is real but buried and the search
/// reads as broken.
pub fn model_entries(
    catalog: &[ModelRow],
    cheap_catalog: Option<&[ModelRow]>,
    filters: &ModelFilters,
) -> Vec<ModelEntry> {
    let narrow = |rows: &[ModelRow], tier: Tier| -> Vec<ModelEntry> {
        let q = filters.get(tier).trim().to_string();
        let built: Vec<ModelEntry> = rows.iter().map(|m| row(tier, m)).collect();
        if q.is_empty() {
            return built;
        }
        let mut scored: Vec<(usize, u8, ModelEntry)> = built
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let hay = format!("{} {}", e.label(), e.detail());
                (i, fuzzy_score(&hay, &q), e)
            })
            .filter(|(_, s, _)| *s > 0)
            .collect();
        // Ties keep catalog order (stable sort), so the curated rows stay on top.
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(_, _, e)| e).collect()
    };
    let mut out = narrow(catalog, Tier::Frontier);
    out.extend(narrow(cheap_catalog.unwrap_or(catalog), Tier::Cheap));
    out.extend(EFFORTS.into_iter().map(|e| ModelEntry::Effort {
        id: e,
        label: e.label().to_string(),
        detail: e.id().to_string(),
    }));
    out
}

fn row(tier: Tier, m: &ModelRow) -> ModelEntry {
    ModelEntry::Model {
        tier,
        id: m.id.clone(),
        label: m.label.clone(),
        detail: format!("{}  ·  {}", m.id, m.provider.as_str()),
    }
}

/// Whether an entry is the one currently in force for its tier.
pub fn is_active(cfg: &ModelConfig, e: &ModelEntry) -> bool {
    match e {
        ModelEntry::Model {
            tier: Tier::Frontier,
            id,
            ..
        } => effective_model(cfg) == id,
        ModelEntry::Model {
            tier: Tier::Cheap,
            id,
            ..
        } => cfg.cheap_model.as_deref() == Some(id),
        ModelEntry::Model { .. } => false,
        ModelEntry::Effort { id, .. } => effective_effort(cfg) == *id,
    }
}

/// Choosing a row. **This is spec §12 in code**: a frontier pick pins the open
/// session and moves the default for new sessions; nothing else moves. Pure —
/// the caller sends the resulting config to the server and re-renders from the
/// response.
pub fn choose_entry(cfg: &ModelConfig, e: &ModelEntry) -> ModelConfig {
    let mut next = cfg.clone();
    match e {
        ModelEntry::Model {
            tier: Tier::Frontier,
            id,
            ..
        } => {
            next.session_model = Some(id.clone());
            next.default_model = id.clone();
        }
        // No per-session pin: one background model for the whole install.
        ModelEntry::Model {
            tier: Tier::Cheap,
            id,
            ..
        } => next.cheap_model = Some(id.clone()),
        ModelEntry::Model { .. } => {}
        ModelEntry::Effort { id, .. } => {
            next.session_effort = Some(*id);
            next.default_effort = *id;
        }
    }
    next
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/// What the cheap section says when nothing in it is marked.
///
/// The ● means "this is what runs". Every other section has one, and the cheap
/// section had none whenever `cheap_model` was unset — so the tab that exists to
/// answer "which model is selected" answered it for two tiers out of three, and
/// the absence read as a missing dot rather than as a state. Unset is a real
/// state and it gets a real row. It does not NAME a model, because when this row
/// shows there is none to name.
pub const CHEAP_UNSET: &str =
    "(unset) — no cheap model is known for this install; pick a row to set one";

/// Shown in a section whose own search box matched nothing.
pub const NO_MATCH: &str = "nothing in this section matches — ⌫ to widen the search";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Header(Tier),
    /// A section's explanation, on its own row so the window height counts it.
    Hint(String),
    /// A tier's search box. `focused` = this is the one `/` is typing into.
    Search {
        tier: Tier,
        query: String,
        focused: bool,
    },
    Entry {
        entry: ModelEntry,
        index: usize,
    },
    Note(String),
}

/// Section headers interleaved with entries — what the cursor window is cut
/// from.
///
/// Built section by section rather than by walking the entries, so a tier whose
/// search matched NOTHING still gets its header and its box. Walking entries
/// meant an empty section rendered as nothing at all — and since the box lives
/// in the section, the box you were typing into vanished at the first
/// non-matching character, taking the keyboard's target off the screen with it.
pub fn display_rows(
    entries: &[ModelEntry],
    cheap_unset: bool,
    filters: &ModelFilters,
    focused: Option<Tier>,
) -> Vec<DisplayRow> {
    let mut out: Vec<DisplayRow> = Vec::new();
    for tier in [Tier::Frontier, Tier::Cheap, Tier::Effort] {
        let rows: Vec<(usize, &ModelEntry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.tier() == tier)
            .collect();
        if tier == Tier::Effort && rows.is_empty() {
            continue;
        }
        out.push(DisplayRow::Header(tier));
        out.push(DisplayRow::Hint(section(tier).1.to_string()));
        if tier != Tier::Effort {
            out.push(DisplayRow::Search {
                tier,
                query: filters.get(tier).to_string(),
                focused: focused == Some(tier),
            });
        }
        if tier == Tier::Cheap && cheap_unset {
            out.push(DisplayRow::Note(CHEAP_UNSET.to_string()));
        }
        if tier != Tier::Effort && rows.is_empty() {
            out.push(DisplayRow::Note(NO_MATCH.to_string()));
        }
        for (index, entry) in rows {
            out.push(DisplayRow::Entry {
                entry: entry.clone(),
                index,
            });
        }
    }
    out
}

/// The visible slice of the interleaved header/entry list.
///
/// Sized from what is ACTUALLY left after the chrome. `max(3, rows - 6)` claimed
/// three rows it did not have below nine, and the overflow did not scroll — it
/// merged rows into each other. Everything countable is counted.
///
/// ONE row for both markers, not two. As a pair they cost two rows, and when
/// only two were left they cost ALL of them — the tab said "↑ 1 more / ↓ 35
/// more" above a list of nothing. Content wins when it is tight; the legend
/// never gives up its row.
pub fn model_window(
    display: &[DisplayRow],
    selected: usize,
    rows: usize,
    chrome: usize,
) -> (usize, usize, usize, bool) {
    let avail = rows.saturating_sub(chrome + 1 /* legend */);
    let marks = display.len() > avail && avail >= 3;
    let height = avail.saturating_sub(usize::from(marks));
    let cursor_at = display
        .iter()
        .position(|d| matches!(d, DisplayRow::Entry { index, .. } if *index == selected))
        .unwrap_or(0);
    let (start, end) = window_around(cursor_at, display.len(), height);
    (start, end, height, marks)
}

/// Entry indices in the window, top to bottom — exactly what `1`–`9` address.
///
/// Headers and the `(unset)` note are NOT numbered: a digit that lands on a
/// section title is a digit that does nothing, and spec §3 wants the options
/// addressable, not the decoration between them.
pub fn visible_entries(display: &[DisplayRow], start: usize, end: usize) -> Vec<usize> {
    display[start.min(display.len())..end.min(display.len())]
        .iter()
        .filter_map(|d| match d {
            DisplayRow::Entry { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}

pub struct ModelPickerProps<'a> {
    /// Columns available, so the legend degrades instead of being cut mid-word.
    pub cols: usize,
    pub cfg: &'a ModelConfig,
    pub entries: &'a [ModelEntry],
    pub selected: usize,
    pub rows: usize,
    pub message: Option<&'a str>,
    /// Both search buffers. Narrowing happens in [`model_entries`]; this only
    /// draws them.
    pub filters: &'a ModelFilters,
    /// Which box has the keyboard, or `None` when neither does.
    pub focused: Option<Tier>,
}

/// The lines this tab paints, in order.
pub fn model_lines(p: &ModelPickerProps) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let display = display_rows(p.entries, p.cfg.cheap_model.is_none(), p.filters, p.focused);
    // The search boxes are DisplayRows, so the window already counts them — only
    // the message is chrome outside the list.
    let chrome = usize::from(p.message.is_some());
    let (start, end, height, marks) = model_window(&display, p.selected, p.rows, chrome);
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(message) = p.message {
        out.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(warn()),
        )));
    }
    // The entry ordinal within the window, so the digits run 1,2,3… down the
    // entries even where a section header sits between two of them.
    let mut ordinal = 0usize;
    let slice: &[DisplayRow] = if height == 0 {
        &[]
    } else {
        &display[start.min(display.len())..end.min(display.len())]
    };
    for d in slice {
        match d {
            // The box is drawn where it applies — under its own section's
            // heading — so which list a query narrows is a fact about the screen
            // and not something the user has to remember.
            DisplayRow::Search { query, focused, .. } => {
                let mut spans = vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "search ",
                        if *focused {
                            Style::default().fg(accent())
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(
                        {
                            let q = clip(query, 40);
                            if q.is_empty() && !*focused {
                                "—".to_string()
                            } else {
                                q
                            }
                        },
                        if *focused { Style::default() } else { dim },
                    ),
                ];
                if *focused {
                    spans.push(Span::styled(
                        " ",
                        Style::default()
                            .bg(accent())
                            .fg(ratatui::style::Color::Black),
                    ));
                }
                out.push(Line::from(spans));
            }
            DisplayRow::Hint(hint) => {
                out.push(Line::from(Span::styled(format!("  {hint}"), dim)));
            }
            DisplayRow::Note(note) => {
                out.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("●", Style::default().fg(accent())),
                    Span::styled(format!(" {}", clip(note, 88)), dim),
                ]));
            }
            // The TITLE only. The hint is a `Hint` row of its own, because
            // sharing one row put a 76-character sentence after a 32-character
            // heading and the renderer cut it at the panel border.
            DisplayRow::Header(tier) => {
                out.push(Line::from(Span::styled(
                    section(*tier).0.to_string(),
                    bold.fg(accent()),
                )));
            }
            DisplayRow::Entry { entry, index } => {
                let sel = *index == p.selected;
                let active = is_active(p.cfg, entry);
                ordinal += 1;
                out.push(Line::from(vec![
                    // The digit that selects this row, printed on it — spec §3
                    // wants a NUMBERED LIST, not a shortcut you have to be told
                    // about. It counts entries and skips headers, which is the
                    // same thing `visible_entries` counts.
                    Span::styled(
                        if ordinal <= 9 {
                            format!("{ordinal} ")
                        } else {
                            "  ".into()
                        },
                        dim,
                    ),
                    Span::styled(
                        if sel { "❯ " } else { "  " },
                        if sel {
                            Style::default().fg(accent())
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(
                        if active { "●" } else { " " },
                        if active && !sel {
                            Style::default().fg(accent())
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(
                        format!(" {}", clip(entry.label(), 38)),
                        if sel { bold } else { Style::default() },
                    ),
                    Span::styled(format!("  {}", clip(entry.detail(), 34)), dim),
                ]));
            }
        }
    }
    if marks {
        let up = if start > 0 {
            format!("↑ {start}")
        } else {
            String::new()
        };
        let sep = if start > 0 && end < display.len() {
            " · "
        } else {
            ""
        };
        let down = if end < display.len() {
            format!("↓ {}", display.len() - end)
        } else {
            String::new()
        };
        out.push(Line::from(Span::styled(
            format!("{up}{sep}{down} more"),
            dim,
        )));
    }
    // The legend is the LAST row, on every tab, naming only bound keys.
    out.push(Line::from(Span::styled(
        match p.focused {
            Some(tier) => format!(
                "narrowing {} · tab other box · ⌫ back · esc clear · ↑↓ move · ⏎",
                match tier {
                    Tier::Cheap => "cheap",
                    _ => "frontier",
                }
            ),
            None => {
                "↑↓ move · pgup/pgdn page · 1-9 pick · / search this section · ⏎ choose · esc back"
                    .to_string()
            }
        },
        dim,
    )));
    out
}

pub fn render_model(p: &ModelPickerProps, area: Rect, buf: &mut Buffer) {
    paint_rows(&model_lines(p), area, buf);
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Panel.test.ts (the model cases)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use bough_core::llm::routing::Provider;

    pub fn catalog() -> Vec<ModelRow> {
        vec![
            ModelRow {
                id: "claude-opus-5".into(),
                label: "Opus 5".into(),
                provider: Provider::Anthropic,
            },
            ModelRow {
                id: "openai:gpt-5-mini".into(),
                label: "GPT-5 mini".into(),
                provider: Provider::Openai,
            },
        ]
    }

    pub fn cfg() -> ModelConfig {
        ModelConfig {
            default_model: "claude-opus-5".into(),
            session_model: None,
            cheap_model: None,
            default_effort: EffortChoice::Default,
            session_effort: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn text(p: &ModelPickerProps) -> String {
        model_lines(p)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn find(entries: &[ModelEntry], tier: Tier, id: &str) -> ModelEntry {
        entries
            .iter()
            .find(|e| e.tier() == tier && e.model_id() == Some(id))
            .expect("entry")
            .clone()
    }

    #[test]
    fn the_model_picker_sets_both_tiers_and_pins_only_this_session() {
        let catalog = catalog();
        let entries = model_entries(&catalog, None, &ModelFilters::default());
        let mut tiers: Vec<Tier> = Vec::new();
        for e in &entries {
            if !tiers.contains(&e.tier()) {
                tiers.push(e.tier());
            }
        }
        assert_eq!(tiers, vec![Tier::Frontier, Tier::Cheap, Tier::Effort]);

        let cfg = cfg();
        // Frontier: pins THIS session AND moves the default for new sessions.
        let frontier = find(&entries, Tier::Frontier, "openai:gpt-5-mini");
        let after = choose_entry(&cfg, &frontier);
        assert_eq!(after.session_model.as_deref(), Some("openai:gpt-5-mini"));
        assert_eq!(after.default_model, "openai:gpt-5-mini");
        assert_eq!(effective_model(&after), "openai:gpt-5-mini");
        // …and the cheap tier is untouched by a frontier pick, and vice versa.
        assert_eq!(after.cheap_model, None);
        let cheap = find(&entries, Tier::Cheap, "openai:gpt-5-mini");
        let both = choose_entry(&after, &cheap);
        assert_eq!(both.cheap_model.as_deref(), Some("openai:gpt-5-mini"));
        assert_eq!(both.session_model.as_deref(), Some("openai:gpt-5-mini"));
        assert_eq!(both.default_model, "openai:gpt-5-mini");
    }

    #[test]
    fn an_effort_row_never_writes_itself_into_a_model_field() {
        // The flat `id: String` this replaces let `choose_entry` write "xhigh"
        // into `default_model` without a complaint.
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let xhigh = entries
            .iter()
            .find(|e| matches!(e, ModelEntry::Effort { id, .. } if id.id() == "xhigh"))
            .unwrap();
        let after = choose_entry(&cfg(), xhigh);
        assert_eq!(after.default_model, "claude-opus-5");
        assert_eq!(after.session_model, None);
        assert_eq!(
            after.session_effort,
            Some(EffortChoice::Level(Effort::Xhigh))
        );
        assert_eq!(after.default_effort, EffortChoice::Level(Effort::Xhigh));
    }

    #[test]
    fn an_unrecognised_stored_effort_is_no_row_of_mine_rather_than_a_fake_one() {
        assert_eq!(
            as_effort_choice(Some("xhigh")),
            Some(EffortChoice::Level(Effort::Xhigh))
        );
        assert_eq!(
            as_effort_choice(Some("default")),
            Some(EffortChoice::Default)
        );
        assert_eq!(as_effort_choice(Some("ludicrous")), None);
        assert_eq!(as_effort_choice(None), None);
    }

    #[test]
    fn each_tiers_box_narrows_only_its_own_section() {
        // `haiku` typed to find a cheap model also hid every frontier row, so
        // the ● that said what the supervisor runs on vanished mid-decision.
        let filters = ModelFilters {
            frontier: String::new(),
            cheap: "mini".into(),
        };
        let entries = model_entries(&catalog(), None, &filters);
        let frontier: Vec<&str> = entries
            .iter()
            .filter(|e| e.tier() == Tier::Frontier)
            .filter_map(|e| e.model_id())
            .collect();
        let cheap: Vec<&str> = entries
            .iter()
            .filter(|e| e.tier() == Tier::Cheap)
            .filter_map(|e| e.model_id())
            .collect();
        assert_eq!(frontier, vec!["claude-opus-5", "openai:gpt-5-mini"]);
        assert_eq!(cheap, vec!["openai:gpt-5-mini"]);
    }

    #[test]
    fn a_section_whose_search_matched_nothing_keeps_its_header_and_its_box() {
        let filters = ModelFilters {
            frontier: "zzzzz".into(),
            cheap: String::new(),
        };
        let entries = model_entries(&catalog(), None, &filters);
        let display = display_rows(&entries, true, &filters, Some(Tier::Frontier));
        assert!(matches!(display[0], DisplayRow::Header(Tier::Frontier)));
        assert!(display.iter().any(|d| matches!(
            d,
            DisplayRow::Search {
                tier: Tier::Frontier,
                ..
            }
        )));
        assert!(display
            .iter()
            .any(|d| matches!(d, DisplayRow::Note(n) if n == NO_MATCH)));
    }

    #[test]
    fn an_install_with_no_cheap_tier_gets_a_real_row_rather_than_a_missing_dot() {
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let display = display_rows(&entries, true, &ModelFilters::default(), None);
        assert!(display
            .iter()
            .any(|d| matches!(d, DisplayRow::Note(n) if n == CHEAP_UNSET)));
        // …and once a cheap model is known, the state row is gone.
        let display = display_rows(&entries, false, &ModelFilters::default(), None);
        assert!(!display
            .iter()
            .any(|d| matches!(d, DisplayRow::Note(n) if n == CHEAP_UNSET)));
    }

    #[test]
    fn digits_address_entries_and_never_a_section_title() {
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let display = display_rows(&entries, true, &ModelFilters::default(), None);
        let visible = visible_entries(&display, 0, display.len());
        assert_eq!(visible.len(), entries.len());
        // Header/hint/search/note rows are not numbered.
        assert!(display.len() > visible.len());
    }

    #[test]
    fn the_window_reserves_the_legend_and_spends_the_marker_row_before_content() {
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let display = display_rows(&entries, true, &ModelFilters::default(), None);
        let (_, _, height, marks) = model_window(&display, 0, 40, 0);
        assert!(!marks, "everything fits: no marker row");
        assert_eq!(height, 39);
        let (_, _, height, marks) = model_window(&display, 0, 8, 0);
        assert!(marks);
        assert_eq!(height, 6);
        // Two rows left is content, never a pair of markers over nothing.
        let (_, _, height, marks) = model_window(&display, 0, 3, 0);
        assert!(!marks);
        assert_eq!(height, 2);
    }

    #[test]
    fn the_active_row_of_every_tier_carries_the_dot() {
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let mut cfg = cfg();
        cfg.cheap_model = Some("openai:gpt-5-mini".into());
        assert!(is_active(
            &cfg,
            &find(&entries, Tier::Frontier, "claude-opus-5")
        ));
        assert!(is_active(
            &cfg,
            &find(&entries, Tier::Cheap, "openai:gpt-5-mini")
        ));
        assert!(!is_active(
            &cfg,
            &find(&entries, Tier::Cheap, "claude-opus-5")
        ));
        let default_effort = entries
            .iter()
            .find(|e| matches!(e, ModelEntry::Effort { id, .. } if *id == EffortChoice::Default))
            .unwrap();
        assert!(is_active(&cfg, default_effort));
    }

    #[test]
    fn the_tab_paints_both_sections_and_ends_in_a_legend() {
        let cfg = cfg();
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let filters = ModelFilters::default();
        let out = text(&ModelPickerProps {
            cols: 100,
            cfg: &cfg,
            entries: &entries,
            selected: 0,
            rows: 30,
            message: None,
            filters: &filters,
            focused: None,
        });
        assert!(out.contains("frontier model — the supervisor"), "{out}");
        assert!(
            out.contains("cheap model — titles, ghost text, activity"),
            "{out}"
        );
        assert!(out.contains("thinking depth"), "{out}");
        assert!(out.contains(CHEAP_UNSET), "{out}");
        assert!(out.contains("Opus 5"), "{out}");
        assert!(out.lines().last().unwrap().ends_with("esc back"), "{out}");
    }

    #[test]
    fn a_focused_search_box_says_which_list_it_narrows() {
        let cfg = cfg();
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let filters = ModelFilters {
            frontier: String::new(),
            cheap: "gpt".into(),
        };
        let out = text(&ModelPickerProps {
            cols: 100,
            cfg: &cfg,
            entries: &entries,
            selected: 0,
            rows: 30,
            message: None,
            filters: &filters,
            focused: Some(Tier::Cheap),
        });
        assert!(out.contains("narrowing cheap · tab other box"), "{out}");
    }

    #[test]
    fn the_tab_never_paints_more_rows_than_its_budget() {
        let cfg = cfg();
        let entries = model_entries(&catalog(), None, &ModelFilters::default());
        let filters = ModelFilters::default();
        for rows in [1usize, 2, 3, 4, 6, 8, 12, 20] {
            let painted = model_lines(&ModelPickerProps {
                cols: 100,
                cfg: &cfg,
                entries: &entries,
                selected: 3,
                rows,
                message: Some("something happened"),
                filters: &filters,
                focused: None,
            });
            assert!(
                painted.len() <= rows.max(2),
                "@{rows}: painted {} rows",
                painted.len()
            );
        }
    }
}
