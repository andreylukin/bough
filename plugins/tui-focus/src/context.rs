//! Invariant: everything the pane says about the model's NEXT CONTEXT comes from the same
//! `assemble` the wake flow calls (`tui-preview`'s rule), so it is true by construction at every
//! DEPTH of the view. Round 11 made the pane the sectioned context, always (Andrey, 2026-08-28:
//! "we shove context in, it's sections based on previous history. I want to see those sections,
//! labeled"); the conversation brief (Andrey, 2026-08-31) amended it to "a chat by default,
//! truth on demand": the CHAT keeps the sections' structure (tier bands fold the rows they
//! summarise, unconsumed mail waits in the tray, never inline) without labels or counts; the
//! PEEK, while a message is typed, surfaces the fold line where the verbatim tail begins; `^p`
//! pins the FULL view, which is round 11's, band by labeled band.
//!
//! What this module owns is PURE: classifying an [`Assembled`] into the view's kinds, parsing the
//! pins and tier bodies the bands print, the standing block (digest + pins, capped, pins folding
//! to titles), the labeled rules, the footer, and the per-row PLAN that says which transcript rows
//! are in the context (the tail band), which are summarised (under a tier), which are gone, and
//! which are mail. `lib.rs` emits the lines and owns the I/O.

use std::collections::{BTreeSet, HashMap, HashSet};

use bough_plugin_ledger::{Seq, StepId};
use bough_plugin_projection::{Assembled, Slot};
use bough_plugin_tui_shell::pane::HitId;
use bough_plugin_tui_shell::Theme;
use chrono::{DateTime, Utc};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::hit::Hit;
use crate::rows::Row;

/// How long a changed section's rule stays lit after a rebuild (D11-5).
pub const FLASH_MS: i64 = 1500;
/// The clickable region ids this view registers.
pub const HIT_PREFIX: &str = "context:";

/// How much truth the pane shows (the conversation brief, 2026-08-31). The DATA is the same
/// assembly at every depth; the depth decides how loudly the chrome says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// The resting state: a chat. Tier bands and the dropped fold keep the history reachable,
    /// mail waits in the tray, and no rule, count or footer interrupts the reading.
    Chat,
    /// A message is being written: the fold line surfaces where the verbatim tail begins and the
    /// bands carry their token counts, so what the message lands on is visible while it is typed.
    Peek,
    /// `^p`: the full context view, every band labeled with its count, the standing block
    /// pinned, mail under its own band, the footer. Round 11's view, on demand instead of always.
    Full,
}

/// What the view makes of a section.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Identity and everything contributed around it, plus the skills: the FIXED material,
    /// folded to one line (D11-4).
    Head,
    Digest,
    Pins,
    /// One tier band: its rollups, each over a seq range.
    Tier {
        tier: u8,
        entries: Vec<TierEntry>,
    },
    /// The recent steps: the rows the transcript draws.
    Tail,
    Mail,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TierEntry {
    pub from: u64,
    pub to: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub tokens: usize,
    pub degraded: bool,
}

/// One pin as the pins band prints it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub title: String,
    pub step: Option<String>,
    pub text: String,
}

/// The view's state between frames.
#[derive(Clone, Debug, Default)]
pub struct ContextView {
    pub sections: Vec<Section>,
    pub tokens: usize,
    pub budget: usize,
    pub rebuilt_at: Option<DateTime<Utc>>,
    /// Section id → when its text last changed, for the flash.
    pub changed: HashMap<String, DateTime<Utc>>,
    /// The steps the tail band cites: the rows that are IN the context verbatim.
    pub tail_steps: HashSet<StepId>,
    /// The steps the mail band cites: drawn under the mail rule, last.
    pub mail_steps: HashSet<StepId>,
    pub head_open: bool,
    pub standing_open: bool,
    pub open_tiers: BTreeSet<String>,
    pub dropped_open: bool,
    /// The mail TRAY (chat and peek): opened, the queue itself is listed.
    pub mail_open: bool,
    /// A refresh is in flight (`lib.rs` arms one at a time; a request during one re-arms).
    pub refreshing: bool,
    pub dirty: bool,
}

impl ContextView {
    /// Whether there is a context to show. Before the first refresh the pane is the plain
    /// transcript.
    pub fn is_on(&self) -> bool {
        !self.sections.is_empty()
    }

    /// Land an assembly. Sections whose text changed since the last one are marked for the
    /// flash — never on the first landing, which would light everything.
    pub fn apply(&mut self, a: &Assembled, now: DateTime<Utc>) {
        let previous: HashMap<String, String> = self
            .sections
            .iter()
            .map(|s| (s.id.clone(), s.body.clone()))
            .collect();
        let first = self.sections.is_empty();
        let mut sections = Vec::with_capacity(a.sections.len());
        let mut tail = HashSet::new();
        let mut mail = HashSet::new();
        for s in &a.sections {
            let kind = classify(s.id.as_str(), s.position.slot, &s.title, &s.body);
            match kind {
                Kind::Tail => tail.extend(s.cites.steps.iter().cloned()),
                Kind::Mail => mail.extend(s.cites.steps.iter().cloned()),
                _ => {}
            }
            let id = s.id.to_string();
            if !first && previous.get(&id).is_some_and(|b| *b != s.body) {
                self.changed.insert(id.clone(), now);
            }
            sections.push(Section {
                id,
                kind,
                title: s.title.clone(),
                body: s.body.clone(),
                tokens: s.tokens,
                degraded: s.degraded.is_some(),
            });
        }
        self.sections = sections;
        self.tokens = a.tokens;
        self.budget = a.budget;
        self.rebuilt_at = Some(now);
        self.tail_steps = tail;
        self.mail_steps = mail;
        self.changed
            .retain(|_, at| (now - *at).num_milliseconds() < FLASH_MS * 4);
    }

    /// Whether a section's rule is lit right now.
    pub fn lit(&self, id: &str, now: DateTime<Utc>) -> bool {
        self.changed
            .get(id)
            .is_some_and(|at| (now - *at).num_milliseconds() < FLASH_MS)
    }

    fn by_kind(&self, f: impl Fn(&Kind) -> bool) -> Vec<&Section> {
        self.sections.iter().filter(|s| f(&s.kind)).collect()
    }

    pub fn digest(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.kind == Kind::Digest)
    }

    pub fn pins(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.kind == Kind::Pins)
    }

    pub fn tiers(&self) -> Vec<&Section> {
        self.by_kind(|k| matches!(k, Kind::Tier { .. }))
    }

    pub fn head(&self) -> Vec<&Section> {
        self.by_kind(|k| *k == Kind::Head)
    }

    /// Toggle whatever a `context:` hit names. `true` if it was one.
    pub fn toggle(&mut self, hit: &HitId) -> bool {
        let Some(rest) = hit.as_str().strip_prefix(HIT_PREFIX) else {
            return false;
        };
        match rest {
            "head" => self.head_open = !self.head_open,
            "standing" => self.standing_open = !self.standing_open,
            "dropped" => self.dropped_open = !self.dropped_open,
            "mail" => self.mail_open = !self.mail_open,
            other => {
                if let Some(id) = other.strip_prefix("tier:") {
                    if !self.open_tiers.remove(id) {
                        self.open_tiers.insert(id.to_string());
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }
}

/// PURE: a section's kind from its id, slot, title and body. The built-in bands have fixed ids
/// (`bands.rs`); everything contributed around Identity, and the skills, is fixed material.
pub fn classify(id: &str, slot: Slot, title: &str, body: &str) -> Kind {
    match id {
        "identity" => return Kind::Head,
        "digest" => return Kind::Digest,
        "pins" => return Kind::Pins,
        "tail" => return Kind::Tail,
        "mail" => return Kind::Mail,
        _ => {}
    }
    if let Some(rest) = id.strip_prefix("tier-") {
        // `tier_section_id` is `u8::MAX - tier`, so the coarse tiers sort first.
        let tier = rest
            .parse::<u8>()
            .ok()
            .map(|n| u8::MAX - n)
            .or_else(|| {
                title
                    .strip_prefix("Tier ")
                    .and_then(|t| t.split(' ').next())
                    .and_then(|n| n.parse().ok())
            })
            .unwrap_or(0);
        return Kind::Tier {
            tier,
            entries: parse_tier(body),
        };
    }
    if slot == Slot::Identity || title.starts_with("Skill: ") {
        return Kind::Head;
    }
    Kind::Other
}

/// PURE: the tier band's `- [from..to] text` entries.
pub fn parse_tier(body: &str) -> Vec<TierEntry> {
    let mut out: Vec<TierEntry> = Vec::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- [") {
            if let Some((range, text)) = rest.split_once("] ") {
                if let Some((a, b)) = range.split_once("..") {
                    if let (Ok(from), Ok(to)) = (a.parse(), b.parse()) {
                        out.push(TierEntry {
                            from,
                            to,
                            text: text.trim().to_string(),
                        });
                        continue;
                    }
                }
            }
        }
        if let Some(last) = out.last_mut() {
            if !line.trim().is_empty() {
                last.text.push(' ');
                last.text.push_str(line.trim());
            }
        }
    }
    out
}

/// PURE: the pins band's `- title (step:ID)` + indented text, or rung 4's titles-only list.
pub fn parse_pins(body: &str) -> Vec<Pin> {
    let mut out: Vec<Pin> = Vec::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            let (title, step) = match rest.rsplit_once(" (step:") {
                Some((t, s)) => (t.to_string(), Some(s.trim_end_matches(')').to_string())),
                None => (rest.to_string(), None),
            };
            out.push(Pin {
                title,
                step,
                text: String::new(),
            });
        } else if let Some(last) = out.last_mut() {
            let t = line.trim();
            if !t.is_empty() {
                if !last.text.is_empty() {
                    last.text.push(' ');
                }
                last.text.push_str(t);
            }
        }
    }
    out
}

/// PURE: `4.1k` / `620`.
pub fn tokens_text(n: usize) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// PURE: the folded head's words: `identity · about · boundary · 2 skills`.
pub fn head_words(view: &ContextView) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut skills = 0usize;
    for s in view.head() {
        if s.title.starts_with("Skill: ") {
            skills += 1;
        } else {
            words.push(s.title.to_lowercase());
        }
    }
    if skills > 0 {
        words.push(format!(
            "{skills} skill{}",
            if skills == 1 { "" } else { "s" }
        ));
    }
    words.join(" \u{b7} ")
}

fn hit(name: &str) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{name}"))
}

/// Full-width ground under a line (the conversation brief, direction A): pad to `width`, then
/// patch the wash UNDER the spans' own colours, so membership in the next context is a ground a
/// line stands on rather than a rule above it.
pub fn washed(line: Line<'static>, width: u16, bg: ratatui::style::Color) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let mut spans = line.spans;
    spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
    Line::from(spans).patch_style(Style::default().bg(bg))
}

/// A section's band line: `title ····· right` on its section's ground. Lit = just changed, in
/// the same warning hue the rules used.
pub fn band(
    title: &str,
    right: &str,
    lit: bool,
    width: u16,
    color: ratatui::style::Color,
    bg: ratatui::style::Color,
    theme: &Theme,
) -> Line<'static> {
    let fg = if lit { theme.warn } else { color };
    let mut spans = vec![Span::styled(title.to_string(), Style::default().fg(fg))];
    if !right.is_empty() {
        let used = title.chars().count() + right.chars().count() + 1;
        spans.push(Span::raw(
            " ".repeat((width as usize).saturating_sub(used).max(1)),
        ));
        spans.push(Span::styled(
            right.to_string(),
            Style::default().fg(theme.dim),
        ));
    }
    washed(Line::from(spans), width, bg)
}

fn wrapped(
    text: &str,
    indent: &str,
    width: u16,
    color: ratatui::style::Color,
) -> Vec<Line<'static>> {
    let w = width.saturating_sub(indent.chars().count() as u16).max(8);
    bough_plugin_tui_render::wrap(text.trim_end(), w)
        .into_iter()
        .map(|l| {
            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(l, Style::default().fg(color)),
            ])
        })
        .collect()
}

/// PURE: the STANDING block (D11-2): the folded head line, the digest, the pins — never scrolls.
/// Past `cap` rows the pins fold to titles; past that they are cut with `… N more`; opened, it
/// takes what it needs. Returns the lines and the hits, `first_line`-relative.
pub fn standing_lines(
    view: &ContextView,
    cap: usize,
    width: u16,
    theme: &Theme,
    first_line: u16,
    now: DateTime<Utc>,
) -> (Vec<Line<'static>>, Vec<Hit>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<Hit> = Vec::new();
    // The fixed material, one dim line (D11-4).
    let head: Vec<&Section> = view.head();
    let head_tokens: usize = head.iter().map(|s| s.tokens).sum();
    let marker = if view.head_open {
        "\u{25be}"
    } else {
        "\u{25b8}"
    };
    let text = format!(
        "{marker} {} \u{b7} fixed \u{b7} {}",
        head_words(view),
        tokens_text(head_tokens)
    );
    hits.push(Hit {
        id: hit("head"),
        line: first_line,
        x: 0,
        width: text.chars().count() as u16,
    });
    lines.push(Line::styled(text, Style::default().fg(theme.dim)));

    let block_start = lines.len();
    let field = Style::default().bg(theme.wash_head);
    let header = |title: &str, note: &str, tokens: usize, width: u16| -> Line<'static> {
        let right = tokens_text(tokens);
        let left = format!(" {title}");
        let mid_w = (width as usize).saturating_sub(
            left.chars().count() + note.chars().count() + right.chars().count() + 4,
        );
        Line::from(vec![
            Span::styled(
                left,
                Style::default()
                    .fg(theme.fg)
                    .bg(theme.sel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {note}"),
                Style::default().fg(theme.dim).bg(theme.sel_bg),
            ),
            Span::styled(" ".repeat(mid_w), Style::default().bg(theme.sel_bg)),
            Span::styled(
                format!(" {right} "),
                Style::default().fg(theme.dim).bg(theme.sel_bg),
            ),
        ])
    };
    let body_line = |l: Line<'static>, width: u16| -> Line<'static> {
        let used: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
        let mut spans: Vec<Span<'static>> = l.spans;
        spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
        Line::from(spans).patch_style(field)
    };
    if let Some(d) = view.digest() {
        let note = if view.lit(&d.id, now) { "changed" } else { "" };
        lines.push(header("digest", note, d.tokens, width));
        for l in wrapped(&d.body, " ", width, theme.fg) {
            lines.push(body_line(l, width));
        }
    }
    if let Some(p) = view.pins() {
        let pins = parse_pins(&p.body);
        let open_rows: usize = pins
            .iter()
            .map(|x| {
                1 + if x.text.is_empty() {
                    0
                } else {
                    wrapped(&x.text, "   ", width, theme.fg).len()
                }
            })
            .sum();
        let so_far = lines.len() - block_start;
        let fold = !view.standing_open && so_far + 1 + open_rows > cap;
        let mut note = String::new();
        if fold {
            note.push_str("folded to titles \u{b7} click to open");
        } else if view.lit(&p.id, now) {
            note.push_str("changed");
        }
        lines.push(header(
            &format!("pins \u{b7} {}", pins.len()),
            &note,
            p.tokens,
            width,
        ));
        let room = if view.standing_open {
            usize::MAX
        } else {
            cap.saturating_sub(lines.len() - block_start)
        };
        let mut shown = 0usize;
        for (i, pin) in pins.iter().enumerate() {
            let left = pins.len() - i;
            if shown + 1 > room.saturating_sub(if left > 1 { 1 } else { 0 }) && left > 1 {
                lines.push(body_line(
                    Line::styled(
                        format!("   \u{2026} {left} more"),
                        Style::default().fg(theme.dim),
                    ),
                    width,
                ));
                break;
            }
            let mut spans = vec![
                Span::styled(" \u{2691} ", Style::default().fg(theme.warn)),
                Span::styled(pin.title.clone(), Style::default().fg(theme.fg)),
            ];
            if let Some(step) = &pin.step {
                spans.push(Span::styled(
                    format!(" \u{b7} step {step}"),
                    Style::default().fg(theme.dim),
                ));
            }
            lines.push(body_line(Line::from(spans), width));
            shown += 1;
            if !fold && !pin.text.is_empty() {
                for l in wrapped(&pin.text, "   ", width, theme.fg) {
                    lines.push(body_line(l, width));
                    shown += 1;
                }
            }
        }
    }
    if lines.len() > block_start {
        hits.push(Hit {
            id: hit("standing"),
            line: first_line + block_start as u16,
            x: 0,
            width,
        });
    }
    (lines, hits)
}

/// PURE: the fixed material, OPENED (D11-4): each head section under its own rule, its text dim.
/// Drawn at the top of the scrolling half — a system prefix is hundreds of lines, and the
/// standing block must not grow by that.
pub fn head_lines(view: &ContextView, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !view.head_open {
        return lines;
    }
    for s in view.head() {
        lines.push(band(
            &s.title.to_lowercase(),
            &tokens_text(s.tokens),
            false,
            width,
            theme.accent,
            theme.wash_head,
            theme,
        ));
        for para in reflow(&s.body) {
            if para.is_empty() {
                lines.push(Line::raw(""));
                continue;
            }
            for l in bough_plugin_tui_render::wrap(&para, width.max(8)) {
                lines.push(Line::styled(l, Style::default().fg(theme.dim)));
            }
        }
    }
    lines
}

/// PURE: a prompt's paragraphs, each on one line, so the pane wraps them at ITS width rather
/// than at the width the author's editor had. A line that starts with whitespace, a list
/// marker, a heading, a fence or a table bar keeps its own line; a blank line is an empty entry.
pub fn reflow(body: &str) -> Vec<String> {
    let own_line = |l: &str| {
        l.starts_with(' ')
            || l.starts_with('\t')
            || l.starts_with("- ")
            || l.starts_with("* ")
            || l.starts_with('#')
            || l.starts_with("```")
            || l.starts_with('|')
            || l.starts_with('>')
            || l.split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for l in body.lines() {
        if l.trim().is_empty() {
            out.push(String::new());
            open = false;
        } else if own_line(l) || !open {
            out.push(l.trim_end().to_string());
            open = !own_line(l);
        } else if let Some(last) = out.last_mut() {
            last.push(' ');
            last.push_str(l.trim());
        }
    }
    out
}

/// A line the plan asks the pane to emit before a row, with its hit if it has one.
#[derive(Clone, Debug)]
pub struct Piece {
    pub line: Line<'static>,
    pub hit: Option<(HitId, u16)>,
}

impl Piece {
    fn plain(line: Line<'static>) -> Piece {
        Piece { line, hit: None }
    }
    fn hit(line: Line<'static>, id: HitId, width: u16) -> Piece {
        Piece {
            line,
            hit: Some((id, width)),
        }
    }
}

/// PURE: which rows the pane draws and what goes before them (D11-3).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Row i is drawn in the scrolling half.
    pub show: Vec<bool>,
    /// Row i is drawn but NOT in the next context (an open tier's rows, the dropped fold's):
    /// the pane dims it, so an unfolded past never reads as something the model will see.
    pub summarized: Vec<bool>,
    /// Row i is mail: drawn after everything else, under the mail rule.
    pub mail: Vec<bool>,
    /// Lines to emit before row i (rules, tier bodies, the mail rule).
    pub before: HashMap<usize, Vec<Piece>>,
    /// Lines to emit after the last row when no row carried them (an empty transcript).
    pub trailing: Vec<Piece>,
}

/// The plan for `rows`, given each row's seq. Tiers are placed before the first row at or past
/// their range; an open tier's rows are shown; rows in no tier and not in the tail are counted
/// under one `not in this context` rule; the tail rule sits before the first tail row; the mail
/// rule before the first mail row.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    view: &ContextView,
    rows: &[Row],
    seq_of: &HashMap<StepId, Seq>,
    now: DateTime<Utc>,
    width: u16,
    theme: &Theme,
    depth: Depth,
) -> Plan {
    let n = rows.len();
    let seqs: Vec<Option<u64>> = rows
        .iter()
        .map(|r| seq_of.get(r.step()).map(|s| s.0))
        .collect();
    let tail_from: Option<u64> = rows
        .iter()
        .zip(seqs.iter())
        .filter(|(r, _)| view.tail_steps.contains(r.step()))
        .filter_map(|(_, s)| *s)
        .min();
    let mut plan = Plan {
        show: vec![false; n],
        summarized: vec![false; n],
        mail: vec![false; n],
        before: HashMap::new(),
        trailing: Vec::new(),
    };
    // Tiers, ascending by range start so a rule lands where its range begins.
    let mut tiers: Vec<(&Section, u8, &Vec<TierEntry>)> = view
        .tiers()
        .into_iter()
        .filter_map(|s| match &s.kind {
            Kind::Tier { tier, entries } => Some((s, *tier, entries)),
            _ => None,
        })
        .collect();
    tiers.sort_by_key(|(_, tier, e)| {
        (
            e.iter().map(|x| x.from).min().unwrap_or(0),
            std::cmp::Reverse(*tier),
        )
    });
    let covered = |seq: u64| -> Option<&Section> {
        tiers
            .iter()
            .find(|(_, _, e)| e.iter().any(|x| x.from <= seq && seq <= x.to))
            .map(|(s, _, _)| *s)
    };
    let mut dropped = 0usize;
    for i in 0..n {
        if view.mail_steps.contains(rows[i].step()) {
            plan.mail[i] = true;
            continue;
        }
        let in_tail = match (seqs[i], tail_from) {
            (Some(s), Some(from)) => s >= from,
            (_, None) => view.tail_steps.contains(rows[i].step()),
            (None, Some(_)) => view.tail_steps.contains(rows[i].step()),
        };
        if in_tail {
            plan.show[i] = true;
            continue;
        }
        match seqs[i].and_then(covered) {
            Some(t) => plan.show[i] = view.open_tiers.contains(&t.id),
            None => {
                dropped += 1;
                plan.show[i] = view.dropped_open;
            }
        }
        plan.summarized[i] = plan.show[i];
    }
    // Where each rule goes: before the first row at or past its start.
    let mut pieces_at: Vec<(usize, Vec<Piece>)> = Vec::new();
    let first_row_at = |seq: u64| -> usize {
        (0..n)
            .find(|&i| !plan.mail[i] && seqs[i].is_some_and(|s| s >= seq))
            .unwrap_or(n)
    };
    if dropped > 0 {
        let first = (0..n)
            .find(|&i| !plan.mail[i] && !plan.show[i] || (plan.show[i] && view.dropped_open))
            .unwrap_or(0);
        let (marker, verb) = if view.dropped_open {
            ("\u{25be}", "close")
        } else {
            ("\u{25b8}", "open")
        };
        let title = format!("{marker} {dropped} older steps not in this context \u{b7} {verb}");
        let w = title.chars().count() as u16;
        pieces_at.push((
            first,
            vec![Piece::hit(
                Line::styled(title, Style::default().fg(theme.dim)),
                hit("dropped"),
                w,
            )],
        ));
    }
    for (s, tier, entries) in &tiers {
        let start = entries.iter().map(|e| e.from).min().unwrap_or(0);
        let at = first_row_at(start);
        let range = if entries.len() == 1 {
            format!("[{}..{}]", entries[0].from, entries[0].to)
        } else {
            format!("{} ranges", entries.len())
        };
        let open = view.open_tiers.contains(&s.id);
        let title = format!(
            "{} tier {tier} summary \u{b7} {range}",
            if open { "\u{25be}" } else { "\u{25b8}" }
        );
        let right = match depth {
            Depth::Chat => String::new(),
            _ => tokens_text(s.tokens),
        };
        let mut v = vec![Piece::hit(
            band(
                &title,
                &right,
                view.lit(&s.id, now),
                width,
                theme.thought,
                theme.wash_tier,
                theme,
            ),
            hit(&format!("tier:{}", s.id)),
            width,
        )];
        for e in entries.iter() {
            let indent = if entries.len() == 1 { "" } else { "  " };
            let text = if entries.len() == 1 {
                e.text.clone()
            } else {
                format!("[{}..{}] {}", e.from, e.to, e.text)
            };
            for l in wrapped(&text, indent, width, theme.fg) {
                v.push(Piece::plain(washed(l, width, theme.wash_tier)));
            }
        }
        pieces_at.push((at, v));
    }
    if let Some(from) = tail_from {
        if depth != Depth::Chat {
            let at = first_row_at(from);
            let count = view.tail_steps.len();
            let t = view.sections.iter().find(|s| s.kind == Kind::Tail);
            let (tokens, lit) = t
                .map(|t| (t.tokens, view.lit(&t.id, now)))
                .unwrap_or((0, false));
            let title = match depth {
                // The FOLD LINE (peek): where what is being typed will land, said while it is
                // typed. Everything above this line reaches the model only as rollups.
                Depth::Peek => {
                    format!("\u{25bc} verbatim from here \u{b7} {count} steps \u{b7} rollups above")
                }
                _ => format!("recent steps \u{b7} [{from}..] \u{b7} {count} verbatim"),
            };
            pieces_at.push((
                at,
                vec![Piece::plain(band(
                    &title,
                    &tokens_text(tokens),
                    lit,
                    width,
                    theme.evidence,
                    theme.wash_tail,
                    theme,
                ))],
            ));
        }
    }
    if depth == Depth::Full {
        if let Some(first_mail) = (0..n).find(|&i| plan.mail[i]) {
            let m = view.sections.iter().find(|s| s.kind == Kind::Mail);
            let (tokens, lit) = m
                .map(|m| (m.tokens, view.lit(&m.id, now)))
                .unwrap_or((0, false));
            let count = plan.mail.iter().filter(|m| **m).count();
            pieces_at.push((
                first_mail,
                vec![Piece::plain(band(
                    &format!("mail \u{b7} {count} unconsumed"),
                    &tokens_text(tokens),
                    lit,
                    width,
                    theme.warn,
                    theme.wash_mail,
                    theme,
                ))],
            ));
        }
    }
    for (at, v) in pieces_at {
        if at >= n {
            plan.trailing.extend(v);
        } else {
            plan.before.entry(at).or_default().extend(v);
        }
    }
    plan
}

/// PURE: the footer: `rebuilt 2s ago · 14.2k of 200k · 62%`, plus what was degraded.
pub fn footer(view: &ContextView, now: DateTime<Utc>, theme: &Theme) -> Line<'static> {
    let ago = match view.rebuilt_at {
        Some(at) => {
            let s = (now - at).num_seconds().max(0);
            if s < 2 {
                "just now".to_string()
            } else if s < 60 {
                format!("{s}s ago")
            } else {
                format!("{}m ago", s / 60)
            }
        }
        None => "never".to_string(),
    };
    let pct = (view.tokens * 100)
        .checked_div(view.budget)
        .unwrap_or(0)
        .min(999);
    let mut text = format!(
        "rebuilt {ago} \u{b7} {} of {} \u{b7} {pct}%",
        tokens_text(view.tokens),
        tokens_text(view.budget)
    );
    let degraded: Vec<&str> = view
        .sections
        .iter()
        .filter(|s| s.degraded)
        .map(|s| s.id.as_str())
        .collect();
    if !degraded.is_empty() {
        text.push_str(&format!(" \u{b7} degraded: {}", degraded.join(", ")));
    }
    Line::styled(text, Style::default().fg(theme.dim))
}

/// The mail TRAY (chat and peek): queued mail waits above the composer rather than reading as
/// conversation (nobody said it yet, it arrives at the NEXT wake). One washed line, count and
/// the newest item; opened by click, the queue itself. The rows are the SAME ones the full view
/// draws under its mail band, so the tray can never disagree with what the model will read.
pub fn tray_pieces(
    view: &ContextView,
    rows: &[Row],
    mail: &[bool],
    width: u16,
    theme: &Theme,
) -> Vec<Piece> {
    let queued: Vec<&Row> = rows
        .iter()
        .zip(mail.iter())
        .filter(|(_, m)| **m)
        .map(|(r, _)| r)
        .collect();
    if queued.is_empty() {
        return Vec::new();
    }
    let tokens = view
        .sections
        .iter()
        .find(|s| s.kind == Kind::Mail)
        .map(|m| m.tokens)
        .unwrap_or(0);
    let marker = if view.mail_open {
        "\u{25be}"
    } else {
        "\u{25b8}"
    };
    let newest = match queued.last() {
        Some(Row::Mail { from, subject, .. }) if !view.mail_open => {
            format!(" \u{2014} {from} \u{b7} {subject}")
        }
        _ => String::new(),
    };
    let title = format!(
        "{marker} \u{2709} next wake \u{b7} {} queued{newest}",
        queued.len()
    );
    let mut pieces = vec![Piece::hit(
        band(
            &title,
            &tokens_text(tokens),
            false,
            width,
            theme.warn,
            theme.wash_mail,
            theme,
        ),
        hit("mail"),
        width,
    )];
    if view.mail_open {
        for row in queued {
            if let Row::Mail { from, subject, .. } = row {
                pieces.push(Piece::plain(washed(
                    crate::mail_line(from, subject, theme),
                    width,
                    theme.wash_mail,
                )));
            }
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::{Position, RenderedSection, SectionCites, SectionId};
    use bough_plugin_tui_shell::ThemeName;

    fn section(
        id: &str,
        slot: Slot,
        title: &str,
        body: &str,
        tokens: usize,
        steps: &[&str],
    ) -> RenderedSection {
        RenderedSection {
            id: SectionId::new(id),
            position: Position::band(slot),
            title: title.into(),
            body: body.into(),
            cites: SectionCites {
                steps: steps.iter().map(|s| StepId::new(*s)).collect(),
                rollups: vec![],
            },
            tokens,
            degraded: None,
        }
    }

    fn assembled(sections: Vec<RenderedSection>) -> Assembled {
        Assembled {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            sections,
            flags: Default::default(),
            tokens: 14_200,
            budget: 200_000,
            cites: SectionCites::default(),
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_bands_classify_and_the_bodies_parse() {
        assert_eq!(
            classify("identity", Slot::Identity, "Identity", ""),
            Kind::Head
        );
        assert_eq!(classify("about", Slot::Identity, "About", ""), Kind::Head);
        assert_eq!(classify("skill.x", Slot::Tiers, "Skill: x", ""), Kind::Head);
        assert_eq!(classify("pins", Slot::Pins, "Pins", ""), Kind::Pins);
        let t = classify(
            &format!("tier-{:03}", u8::MAX - 2),
            Slot::Tiers,
            "Tier 2 summary",
            "- [1..410] a b\n  c\n- [410..980] d\n",
        );
        assert_eq!(
            t,
            Kind::Tier {
                tier: 2,
                entries: vec![
                    TierEntry {
                        from: 1,
                        to: 410,
                        text: "a b c".into()
                    },
                    TierEntry {
                        from: 410,
                        to: 980,
                        text: "d".into()
                    },
                ]
            }
        );
        let pins = parse_pins("- the race is the join (step:s87)\n  detached child\n  exits first\n- never stash (step:s12)\n");
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].title, "the race is the join");
        assert_eq!(pins[0].step.as_deref(), Some("s87"));
        assert_eq!(pins[0].text, "detached child exits first");
        assert_eq!(pins[1].text, "");
        let folded = parse_pins("2 pins, collapsed to titles:\n- a\n- b\n");
        assert_eq!(
            folded.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(tokens_text(620), "620");
        assert_eq!(tokens_text(4100), "4.1k");
        assert_eq!(tokens_text(14_200), "14k");
    }

    #[test]
    fn a_rebuild_marks_only_the_sections_whose_text_changed() {
        let now = Utc::now();
        let mut view = ContextView::default();
        view.apply(
            &assembled(vec![
                section("digest", Slot::Digest, "Digest", "one\n", 10, &[]),
                section("tail", Slot::Tail, "Recent steps", "x", 20, &["s1", "s2"]),
            ]),
            now,
        );
        assert!(view.is_on());
        assert!(view.changed.is_empty(), "the first landing lights nothing");
        assert!(view.tail_steps.contains(&StepId::new("s2")));
        let later = now + chrono::Duration::milliseconds(10);
        view.apply(
            &assembled(vec![
                section("digest", Slot::Digest, "Digest", "two\n", 10, &[]),
                section(
                    "tail",
                    Slot::Tail,
                    "Recent steps",
                    "x",
                    20,
                    &["s1", "s2", "s3"],
                ),
            ]),
            later,
        );
        assert!(view.lit("digest", later));
        assert!(!view.lit("tail", later), "same text, no flash");
        assert!(!view.lit(
            "digest",
            later + chrono::Duration::milliseconds(FLASH_MS + 1)
        ));
        assert!(view.toggle(&HitId::new("context:standing")) && view.standing_open);
        assert!(
            view.toggle(&HitId::new("context:tier:tier-253"))
                && view.open_tiers.contains("tier-253")
        );
        assert!(!view.toggle(&HitId::new("tool:x")));
    }

    #[test]
    fn the_standing_block_folds_the_pins_to_titles_past_the_cap_and_opens_whole() {
        let theme = Theme::of(ThemeName::Dark);
        let now = Utc::now();
        let mut view = ContextView::default();
        let pins: String = (1..=5)
            .map(|i| format!("- pin {i} (step:s{i})\n  a long line of text for pin number {i} that says something\n"))
            .collect();
        view.apply(
            &assembled(vec![
                section(
                    "identity",
                    Slot::Identity,
                    "Identity",
                    "name: sol\n",
                    3000,
                    &[],
                ),
                section("about", Slot::Identity, "About", "..", 100, &[]),
                section("skill.a", Slot::Tiers, "Skill: a", "..", 500, &[]),
                section("digest", Slot::Digest, "Digest", "sol leads.\n", 40, &[]),
                section("pins", Slot::Pins, "Pins", &pins, 300, &[]),
            ]),
            now,
        );
        let (lines, hits) = standing_lines(&view, 8, 80, &theme, 0, now);
        let t = text(&lines);
        assert_eq!(
            t[0],
            "\u{25b8} identity \u{b7} about \u{b7} 1 skill \u{b7} fixed \u{b7} 3.6k"
        );
        assert!(t[1].starts_with(" digest"), "{t:?}");
        assert!(
            t.iter()
                .any(|l| l.contains("pins \u{b7} 5") && l.contains("folded to titles")),
            "{t:?}"
        );
        assert!(
            t.iter()
                .any(|l| l.contains("\u{2691} pin 1 \u{b7} step s1")),
            "{t:?}"
        );
        assert!(
            !t.iter().any(|l| l.contains("a long line")),
            "folded: titles only {t:?}"
        );
        assert!(lines.len() <= 1 + 8, "{} lines", lines.len());
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id.as_str(), "context:head");
        assert_eq!(hits[1].id.as_str(), "context:standing");
        view.standing_open = true;
        let (opened, _) = standing_lines(&view, 8, 80, &theme, 0, now);
        let t = text(&opened);
        assert!(
            t.iter()
                .any(|l| l.contains("a long line of text for pin number 5")),
            "{t:?}"
        );
        assert!(!t.iter().any(|l| l.contains("folded")), "{t:?}");
        // The head, closed, draws nothing below the line; opened, every fixed section's text.
        assert!(head_lines(&view, 80, &theme).is_empty());
        view.head_open = true;
        let t = text(&head_lines(&view, 80, &theme));
        assert!(t[0].contains("identity") && t[0].contains("3.0k"), "{t:?}");
        assert!(t.iter().any(|l| l == "name: sol"), "{t:?}");
        assert!(t.iter().any(|l| l.contains("skill: a")), "{t:?}");
        assert_eq!(
            reflow("one two\nthree.\n\n- a\n- b\n  more\n## h\nafter\nthe heading\n"),
            [
                "one two three.",
                "",
                "- a",
                "- b",
                "  more",
                "## h",
                "after the heading"
            ]
        );
    }

    #[test]
    fn the_plan_hides_summarised_rows_places_the_rules_and_moves_mail_last() {
        let theme = Theme::of(ThemeName::Dark);
        let now = Utc::now();
        let mut view = ContextView::default();
        view.apply(
            &assembled(vec![
                section(
                    &format!("tier-{:03}", u8::MAX - 1),
                    Slot::Tiers,
                    "Tier 1 summary",
                    "- [1..3] early work\n",
                    50,
                    &[],
                ),
                section("tail", Slot::Tail, "Recent steps", "x", 20, &["s4", "s5"]),
                section("mail", Slot::Mail, "Unconsumed mail", "x", 9, &["s2"]),
            ]),
            now,
        );
        let rows: Vec<Row> = vec![
            Row::Andrey {
                step: StepId::new("s1"),
                text: "old".into(),
            },
            Row::Andrey {
                step: StepId::new("s2"),
                text: "a mail step".into(),
            },
            Row::Andrey {
                step: StepId::new("s3"),
                text: "older".into(),
            },
            Row::Andrey {
                step: StepId::new("s4"),
                text: "recent".into(),
            },
            Row::Andrey {
                step: StepId::new("s5"),
                text: "newest".into(),
            },
            Row::Andrey {
                step: StepId::new("s6"),
                text: "landed after the rebuild".into(),
            },
        ];
        let seq_of: HashMap<StepId, Seq> = (1..=6)
            .map(|i| (StepId::new(format!("s{i}")), Seq(i)))
            .collect();
        let p = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Full);
        assert_eq!(
            p.show,
            [false, false, false, true, true, true],
            "{:?}",
            p.show
        );
        assert_eq!(p.mail, [false, true, false, false, false, false]);
        let before0 = text(
            &p.before[&0]
                .iter()
                .map(|x| x.line.clone())
                .collect::<Vec<_>>(),
        );
        assert!(
            before0[0].contains("tier 1 summary \u{b7} [1..3]"),
            "{before0:?}"
        );
        assert!(
            before0.iter().any(|l| l.contains("early work")),
            "{before0:?}"
        );
        let before3 = text(
            &p.before[&3]
                .iter()
                .map(|x| x.line.clone())
                .collect::<Vec<_>>(),
        );
        assert!(
            before3[0].contains("recent steps \u{b7} [4..] \u{b7} 2 verbatim"),
            "{before3:?}"
        );
        let before1 = text(
            &p.before[&1]
                .iter()
                .map(|x| x.line.clone())
                .collect::<Vec<_>>(),
        );
        assert!(
            before1[0].contains("mail \u{b7} 1 unconsumed"),
            "{before1:?}"
        );
        // An open tier shows its rows, marked as NOT in the context.
        view.open_tiers.insert(format!("tier-{:03}", u8::MAX - 1));
        let p = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Full);
        assert_eq!(p.show, [true, false, true, true, true, true]);
        assert_eq!(
            p.summarized,
            [true, false, true, false, false, false],
            "the unfolded rows are dimmed; the tail and mail never are"
        );
        // Rows no tier covers and the tail does not hold are counted, not silently gone.
        view.open_tiers.clear();
        view.sections
            .retain(|s| !matches!(s.kind, Kind::Tier { .. }));
        let p = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Full);
        let before0 = text(
            &p.before[&0]
                .iter()
                .map(|x| x.line.clone())
                .collect::<Vec<_>>(),
        );
        assert!(
            before0[0].contains("2 older steps not in this context"),
            "{before0:?}"
        );
        let f = text(&[footer(&view, now, &theme)]);
        assert!(
            f[0].contains("rebuilt just now \u{b7} 14k of 200k \u{b7} 7%"),
            "{f:?}"
        );
    }
    #[test]
    fn the_chat_keeps_bands_quiet_and_the_peek_surfaces_the_fold_line() {
        let now = Utc::now();
        let mut view = ContextView::default();
        view.apply(
            &assembled(vec![
                section(
                    &format!("tier-{:03}", u8::MAX - 1),
                    Slot::Tiers,
                    "Tier 1 summary",
                    "- [1..3] early work\n",
                    50,
                    &[],
                ),
                section("tail", Slot::Tail, "Recent steps", "x", 20, &["s4", "s5"]),
                section("mail", Slot::Mail, "Unconsumed mail", "x", 9, &["s2"]),
            ]),
            now,
        );
        let rows: Vec<Row> = vec![
            Row::Andrey {
                step: StepId::new("s1"),
                text: "old".into(),
            },
            Row::Mail {
                step: StepId::new("s2"),
                from: "slack:#nm-echo".into(),
                subject: "CI is red".into(),
                class: bough_plugin_ledger::vocabulary::MailClass::Ordinary,
            },
            Row::Andrey {
                step: StepId::new("s4"),
                text: "recent".into(),
            },
            Row::Andrey {
                step: StepId::new("s5"),
                text: "newest".into(),
            },
        ];
        let seq_of: HashMap<StepId, Seq> = [("s1", 1), ("s2", 2), ("s4", 4), ("s5", 5)]
            .into_iter()
            .map(|(s, n)| (StepId::new(s), Seq(n)))
            .collect();
        let theme = Theme::of(ThemeName::Dark);
        let all_text = |p: &Plan| {
            let mut out: Vec<String> = Vec::new();
            for v in p.before.values() {
                out.extend(text(&v.iter().map(|x| x.line.clone()).collect::<Vec<_>>()));
            }
            out.extend(text(
                &p.trailing
                    .iter()
                    .map(|x| x.line.clone())
                    .collect::<Vec<_>>(),
            ));
            out
        };
        // Chat: the band carries no count, and neither the tail nor the mail band exists.
        let chat = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Chat);
        let t = all_text(&chat);
        assert!(
            t.iter().any(|l| l.contains("tier 1 summary")),
            "the band itself stays: {t:?}"
        );
        assert!(
            !t.iter()
                .any(|l| l.contains("verbatim") || l.contains("unconsumed")),
            "no tail rule and no mail band in the chat: {t:?}"
        );
        assert!(
            !t.iter().any(|l| l.contains("50")),
            "no token count on the chat band: {t:?}"
        );
        assert!(chat.mail[1], "the queue is still marked for the tray");
        // Peek: the fold line, and the counts.
        let peek = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Peek);
        let t = all_text(&peek);
        assert!(
            t.iter()
                .any(|l| l.contains("verbatim from here \u{b7} 2 steps")),
            "{t:?}"
        );
        assert!(
            !t.iter().any(|l| l.contains("unconsumed")),
            "mail stays in the tray while typing: {t:?}"
        );
        // Full: round 11's view, unchanged in words.
        let full = plan(&view, &rows, &seq_of, now, 80, &theme, Depth::Full);
        let t = all_text(&full);
        assert!(t.iter().any(|l| l.contains("recent steps \u{b7} [4..]")));
        assert!(t.iter().any(|l| l.contains("mail \u{b7} 1 unconsumed")));

        // The tray: closed, one line with the count and the newest item; open, the queue.
        let pieces = tray_pieces(&view, &rows, &chat.mail, 80, &theme);
        let t = text(&pieces.iter().map(|x| x.line.clone()).collect::<Vec<_>>());
        assert_eq!(t.len(), 1);
        assert!(
            t[0].contains("next wake \u{b7} 1 queued") && t[0].contains("CI is red"),
            "{t:?}"
        );
        assert!(
            pieces[0]
                .hit
                .as_ref()
                .is_some_and(|(id, _)| id.as_str() == "context:mail"),
            "the tray is a button"
        );
        assert!(view.toggle(&HitId::new("context:mail")) && view.mail_open);
        let pieces = tray_pieces(&view, &rows, &chat.mail, 80, &theme);
        let t = text(&pieces.iter().map(|x| x.line.clone()).collect::<Vec<_>>());
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(t[1].contains("slack:#nm-echo") && t[1].contains("CI is red"));
        // No queue, no tray.
        assert!(tray_pieces(&view, &rows, &[false; 4], 80, &theme).is_empty());
    }
}
