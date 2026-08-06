//! The TUI palette, and the preview that lets you browse one without adopting
//! it (port of `src/tui/theme.ts` + `src/tui/components/Theme.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **browsing never commits.** The picker "previews
//! live on cursor move and reverts on exit", so a preview is not a different
//! code path from an applied theme: [`ThemePreview::select`] paints the real
//! palette, the whole TUI recolors on the next frame, and the *baseline* —
//! whatever was in force when the tab was entered — is held aside so
//! [`ThemePreview::cancel`] can put it back byte for byte. Only
//! [`ThemePreview::commit`] moves the baseline. A preview implemented as
//! "render this row differently" would preview the swatch and not the product,
//! which is the one thing a theme picker exists to show.
//!
//! SECOND INVARIANT — **a theme is pure data.** A preset is a *partial*
//! palette over a fixed set of semantic tokens layered on the server's
//! defaults; nothing here has a component, a hard-coded hue, or a rebuild.
//!
//! THIRD — **one apply paints every path.** The TUI draws through three:
//! ratatui styles (which read [`palette`] at render time), the hand-rolled SGR
//! line renderer in [`crate::format`] (SGR parameter bodies), and the screen
//! background. [`apply_theme`] writes all three — `format::set_colors` for the
//! second and a registered background painter for the third — so a theme
//! cannot land in half the screen. `format` deliberately does not import this
//! module; the dependency points this way, never back.
//!
//! FOURTH — **nothing in this file fetches or persists.** [`ThemeState`] is
//! the shape `server/theme.rs` serves; a [`ThemePreview`] takes the boot value
//! and the writer as injected parameters, and the composition root supplies
//! both. Called with neither, the preview still works and the choice lasts for
//! the process, which is what a fixture-driven test wants.
//!
//! RUST NOTE ON THE NOTIFICATION PAIR: React bailed out of re-rendering on an
//! unchanged state, so TS grew `subscribeTheme`/`themeEpoch`. Here the same
//! pair exists for the same reason in ratatui terms — the event loop redraws
//! when [`theme_epoch`] moves, and a subscriber may set a redraw flag.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Wire shape
// ---------------------------------------------------------------------------

/// A partial palette: semantic token → hex. Tokens the TUI ignores are inert.
pub type ThemeColors = BTreeMap<String, String>;

/// A stored theme: a name and the partial palette it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedTheme {
    pub name: String,
    #[serde(default)]
    pub colors: ThemeColors,
}

/// What `GET /theme` serves: the stored theme (if any) over the server defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeState {
    #[serde(default)]
    pub theme: Option<NamedTheme>,
    #[serde(default)]
    pub defaults: ThemeColors,
}

/// Mirror of the server's defaults for the tokens the TUI consumes. Present so
/// a TUI that cannot reach the server still paints a complete, contrast-checked
/// palette rather than terminal-default grey.
///
/// `muted2` is text, not decoration: the old `#656c77` measured 3.60:1 on bg
/// and missed WCAG AA. `#7a828e` clears it at 4.91:1 and still reads below
/// `muted`.
pub const FALLBACK: [(&str, &str); 12] = [
    ("green", "#4ec98f"),
    ("amber", "#d9b45f"),
    ("red", "#e2776e"),
    ("blue", "#5c88c9"),
    ("hairline", "#666d79"),
    ("bg", "#0e1013"),
    ("panel", "#14161a"),
    ("panelInset", "#1f2329"),
    ("text", "#e7e9ed"),
    ("text2", "#c9cdd4"),
    ("muted", "#9aa1ac"),
    ("muted2", "#7a828e"),
];

/// `FALLBACK` as the map form the layering works over.
pub fn fallback_colors() -> ThemeColors {
    FALLBACK
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Resolve a state to the flat token map the palette is read from:
/// FALLBACK ← defaults ← theme.colors.
pub fn resolve_colors(state: Option<&ThemeState>) -> ThemeColors {
    let mut out = fallback_colors();
    if let Some(s) = state {
        for (k, v) in &s.defaults {
            out.insert(k.clone(), v.clone());
        }
        if let Some(t) = &s.theme {
            for (k, v) in &t.colors {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The live palette
// ---------------------------------------------------------------------------

/// The semantic tokens every surface paints from. Hex strings, because that is
/// what the wire carries and what the SGR path needs; [`TuiPalette::color`]
/// turns one into a ratatui [`Color`] at render time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuiPalette {
    /// Identity and active markers.
    pub accent: String,
    /// Warnings and holds.
    pub warn: String,
    pub error: String,
    pub info: String,
    /// Panel borders and hairline separators.
    pub border: String,
    /// The screen background (root box).
    pub bg: String,
    /// Bordered containers: the panel, cards, pickers.
    pub panel: String,
    /// The composer's slightly-raised surface.
    pub panel_inset: String,
    /// Primary foreground.
    pub text: String,
    /// Slightly-recessed prose.
    pub text2: String,
    /// Hints, metadata, folded summaries.
    pub muted: String,
    /// The most de-emphasized text.
    pub muted2: String,
    /// Bumped on every [`apply_theme`] — the redraw dependency.
    pub epoch: u64,
}

impl Default for TuiPalette {
    fn default() -> Self {
        Self::from_colors(&fallback_colors(), 0)
    }
}

impl TuiPalette {
    fn from_colors(c: &ThemeColors, epoch: u64) -> Self {
        let get = |k: &str| -> String {
            c.get(k).cloned().unwrap_or_else(|| {
                FALLBACK
                    .iter()
                    .find(|(t, _)| *t == k)
                    .map(|(_, v)| (*v).to_string())
                    .unwrap_or_default()
            })
        };
        TuiPalette {
            accent: get("green"),
            warn: get("amber"),
            error: get("red"),
            info: get("blue"),
            border: get("hairline"),
            bg: get("bg"),
            panel: get("panel"),
            panel_inset: get("panelInset"),
            text: get("text"),
            text2: get("text2"),
            muted: get("muted"),
            muted2: get("muted2"),
            epoch,
        }
    }

    /// A token's hex as a ratatui truecolor. Malformed hex degrades to
    /// [`Color::Reset`] rather than panicking — a theme is user data.
    pub fn color(hex: &str) -> Color {
        match rgb(hex) {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::Reset,
        }
    }

    pub fn accent_color(&self) -> Color {
        Self::color(&self.accent)
    }
    pub fn warn_color(&self) -> Color {
        Self::color(&self.warn)
    }
    pub fn error_color(&self) -> Color {
        Self::color(&self.error)
    }
    pub fn info_color(&self) -> Color {
        Self::color(&self.info)
    }
    pub fn border_color(&self) -> Color {
        Self::color(&self.border)
    }
    pub fn bg_color(&self) -> Color {
        Self::color(&self.bg)
    }
    pub fn panel_color(&self) -> Color {
        Self::color(&self.panel)
    }
    pub fn panel_inset_color(&self) -> Color {
        Self::color(&self.panel_inset)
    }
    pub fn text_color(&self) -> Color {
        Self::color(&self.text)
    }
    pub fn text2_color(&self) -> Color {
        Self::color(&self.text2)
    }
    pub fn muted_color(&self) -> Color {
        Self::color(&self.muted)
    }
    pub fn muted2_color(&self) -> Color {
        Self::color(&self.muted2)
    }
}

fn palette_cell() -> &'static RwLock<TuiPalette> {
    static PALETTE: OnceLock<RwLock<TuiPalette>> = OnceLock::new();
    PALETTE.get_or_init(|| RwLock::new(TuiPalette::default()))
}

/// A snapshot of the live palette. Read at render time, every frame — that is
/// what makes a preview repaint the product rather than a swatch.
pub fn palette() -> TuiPalette {
    palette_cell().read().unwrap().clone()
}

// ---- the change-notification pair -------------------------------------------

type Listener = Arc<dyn Fn() + Send + Sync>;

fn listeners_cell() -> &'static Mutex<Vec<(u64, Listener)>> {
    static LISTENERS: OnceLock<Mutex<Vec<(u64, Listener)>>> = OnceLock::new();
    LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Drop this to unsubscribe (the RAII form of TS's returned closure).
pub struct ThemeSubscription(u64);

impl Drop for ThemeSubscription {
    fn drop(&mut self) {
        let mut list = listeners_cell().lock().unwrap();
        list.retain(|(id, _)| *id != self.0);
    }
}

/// Subscribe to theme applies. The event is "the palette changed", so a redraw
/// hangs off the palette rather than off whatever state a caller touches nearby.
pub fn subscribe_theme(listener: impl Fn() + Send + Sync + 'static) -> ThemeSubscription {
    let id = next_id();
    listeners_cell()
        .lock()
        .unwrap()
        .push((id, Arc::new(listener)));
    ThemeSubscription(id)
}

/// The palette generation — the snapshot half of the pair.
pub fn theme_epoch() -> u64 {
    palette_cell().read().unwrap().epoch
}

// ---- the screen background ---------------------------------------------------

type Painter = Arc<dyn Fn(&str) + Send + Sync>;

fn painter_cell() -> &'static Mutex<Option<Painter>> {
    static PAINTER: OnceLock<Mutex<Option<Painter>>> = OnceLock::new();
    PAINTER.get_or_init(|| Mutex::new(None))
}

/// Register the screen-background sink.
///
/// Painting happens on registration as well as on every later apply, because
/// the boot order is fetch-and-apply-theme BEFORE the renderer exists: without
/// that first call the very theme the user chose would be the one theme never
/// painted. [`clear_background_painter`] is the other half, for `bough exec`
/// and tests — they apply themes with no renderer and must paint nothing.
pub fn set_background_painter(painter: impl Fn(&str) + Send + Sync + 'static) {
    let painter: Painter = Arc::new(painter);
    let bg = palette().bg;
    *painter_cell().lock().unwrap() = Some(painter.clone());
    painter(&bg);
}

/// Deregister the background sink. Painting into a torn-down renderer is the
/// crash this avoids.
pub fn clear_background_painter() {
    *painter_cell().lock().unwrap() = None;
}

// ---- apply -------------------------------------------------------------------

/// Paint a theme.
///
/// Mutates the palette and pushes the same colours down every other path a
/// pixel can come from: [`crate::format`]'s SGR parameters (the transcript) and
/// the registered screen background. Subscribers are notified LAST, so nothing
/// re-renders against a half-applied palette.
pub fn apply_theme(state: Option<&ThemeState>) {
    let c = resolve_colors(state);
    let next = {
        let mut p = palette_cell().write().unwrap();
        let epoch = p.epoch + 1;
        *p = TuiPalette::from_colors(&c, epoch);
        p.clone()
    };
    crate::format::set_colors(|params| {
        params.muted = fg_params(&next.muted);
        params.accent = fg_params(&next.accent);
        params.warn = fg_params(&next.warn);
        params.error = fg_params(&next.error);
        params.info = fg_params(&next.info);
        params.surface_bg = bg_params(&next.panel_inset);
    });
    let painter = painter_cell().lock().unwrap().clone();
    if let Some(p) = painter {
        p(&next.bg);
    }
    // Copied out of the lock: a listener may unsubscribe (drop) as it runs.
    let listeners: Vec<Listener> = listeners_cell()
        .lock()
        .unwrap()
        .iter()
        .map(|(_, l)| l.clone())
        .collect();
    for l in listeners {
        l();
    }
}

/// `#rgb`/`#rrggbb` → components. `None` for anything else.
fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim_start_matches('#');
    let full: String = if h.len() == 3 {
        h.chars().flat_map(|c| [c, c]).collect()
    } else if h.len() >= 6 {
        h[..6].to_string()
    } else {
        return None;
    };
    let n = u32::from_str_radix(&full, 16).ok()?;
    Some((
        ((n >> 16) & 255) as u8,
        ((n >> 8) & 255) as u8,
        (n & 255) as u8,
    ))
}

/// hex → SGR truecolor foreground params (`38;2;r;g;b`) for [`crate::format`].
pub fn fg_params(hex: &str) -> String {
    match rgb(hex) {
        Some((r, g, b)) => format!("38;2;{r};{g};{b}"),
        None => "38;2;0;0;0".to_string(),
    }
}

/// hex → SGR truecolor background params (`48;2;r;g;b`) for block surfaces.
pub fn bg_params(hex: &str) -> String {
    let f = fg_params(hex);
    format!("48{}", &f[2..])
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

pub struct ThemePreset {
    pub name: &'static str,
    /// The right-hand description on the row.
    pub note: &'static str,
    /// Partial: tokens omitted fall through to the server defaults.
    pub colors: &'static [(&'static str, &'static str)],
}

impl ThemePreset {
    pub fn colors_map(&self) -> ThemeColors {
        self.colors
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }
}

/// The rows the theme tab lists. Most swap the single accent — bough is
/// neutral-dark plus one accent — while Midnight deepens the surfaces and Rosé
/// Pine Moon is a full third-party palette. "Default" is the empty partial: it
/// resets to the built-ins.
///
/// Ember/Rosewood put the accent near the reserved warn/error hues, so each
/// also moves the token it collides with: accent, warn and error must stay
/// three distinguishable hues, or a warning reads as an ordinary highlight.
pub static THEME_PRESETS: &[ThemePreset] = &[
    ThemePreset {
        name: "Default",
        note: "built-in palette",
        colors: &[],
    },
    ThemePreset {
        name: "Fjord",
        note: "accent #5c88c9",
        colors: &[("green", "#5c88c9")],
    },
    ThemePreset {
        name: "Iris",
        note: "accent #9a7fd1",
        colors: &[("green", "#9a7fd1")],
    },
    ThemePreset {
        name: "Ember",
        note: "accent #d9a04f",
        colors: &[("green", "#d9a04f"), ("amber", "#e6d47c")],
    },
    ThemePreset {
        name: "Rosewood",
        note: "accent #d97a8e",
        colors: &[("green", "#d97a8e"), ("red", "#c85850")],
    },
    ThemePreset {
        name: "Lagoon",
        note: "accent #3fbdb0",
        colors: &[("green", "#3fbdb0")],
    },
    ThemePreset {
        name: "Graphite",
        note: "accent #a7b5c8",
        colors: &[("green", "#a7b5c8")],
    },
    ThemePreset {
        name: "Midnight",
        note: "deeper surfaces",
        colors: &[
            ("bg", "#0a0b0e"),
            ("panel", "#101216"),
            ("panelInset", "#1a1e24"),
            ("hairline", "#636a76"),
        ],
    },
    // Roles mapped onto bough's tokens: base→bg, surface→panel,
    // overlay→panelInset, iris→accent (rose reads too warm as a primary),
    // gold→amber, love→red, foam→blue. Borders and `muted2` are lifted off the
    // official hexes, which sit at ~3:1 on this base — `muted2` is text and
    // owes AA like every other preset's.
    ThemePreset {
        name: "Rosé Pine Moon",
        note: "rosepinetheme.com",
        colors: &[
            ("bg", "#232136"),
            ("panel", "#2a273f"),
            ("panelInset", "#393552"),
            ("hairline", "#7d7996"),
            ("text", "#e0def4"),
            ("text2", "#c8c5dd"),
            ("muted", "#908caa"),
            ("muted2", "#8b86a8"),
            ("green", "#c4a7e7"),
            ("amber", "#f6c177"),
            ("red", "#eb6f92"),
            ("blue", "#9ccfd8"),
        ],
    },
];

/// One swatch cell on a preset row.
pub struct SwatchCell {
    pub token: &'static str,
    pub color: String,
    pub block: &'static str,
}

/// The swatch strip for one preset row: the surfaces first — near-identical
/// dark presets differ only there and need the wider cell — then the accent and
/// the text. Resolved from the preset's OWN colours, never the live palette: a
/// row must look like itself whether or not it is the theme currently painted.
pub fn preset_swatch(p: &ThemePreset) -> Vec<SwatchCell> {
    let c = resolve_colors(Some(&ThemeState {
        theme: Some(NamedTheme {
            name: p.name.to_string(),
            colors: p.colors_map(),
        }),
        defaults: ThemeColors::new(),
    }));
    ["bg", "panel", "panelInset", "green", "text"]
        .iter()
        .map(|token| SwatchCell {
            token,
            color: c.get(*token).cloned().unwrap_or_default(),
            block: if matches!(*token, "bg" | "panel" | "panelInset") {
                "███"
            } else {
                "██"
            },
        })
        .collect()
}

/// The preset a stored theme corresponds to, or `None` for a custom palette.
pub fn preset_index(state: Option<&ThemeState>) -> Option<usize> {
    let name = state
        .and_then(|s| s.theme.as_ref().map(|t| t.name.clone()))
        .unwrap_or_else(|| "Default".to_string());
    THEME_PRESETS.iter().position(|p| p.name == name)
}

/// A preset layered over the state's defaults — what `select()` paints.
pub fn state_for(base: Option<&ThemeState>, preset: &ThemePreset) -> ThemeState {
    let defaults = base.map(|b| b.defaults.clone()).unwrap_or_default();
    // The empty partial IS the reset: no stored theme, defaults only (DELETE /theme).
    if preset.colors.is_empty() {
        ThemeState {
            theme: None,
            defaults,
        }
    } else {
        ThemeState {
            theme: Some(NamedTheme {
                name: preset.name.to_string(),
                colors: preset.colors_map(),
            }),
            defaults,
        }
    }
}

/// What persisting a state means on the wire.
///
/// `theme == null` ⇒ DELETE. Never PUT an empty map: it would store a *named*
/// theme overriding nothing, and the next boot would read a custom palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeWrite {
    Delete,
    Put { name: String, colors: ThemeColors },
}

pub fn persist_request(state: &ThemeState) -> ThemeWrite {
    match &state.theme {
        None => ThemeWrite::Delete,
        Some(t) => ThemeWrite::Put {
            name: t.name.clone(),
            colors: t.colors.clone(),
        },
    }
}

// ---------------------------------------------------------------------------
// The preview controller
// ---------------------------------------------------------------------------

type ApplyFn = Box<dyn FnMut(Option<&ThemeState>) + Send>;
type PersistFn = Box<dyn FnMut(&'static ThemePreset, &ThemeState) + Send>;

/// One theme-tab browsing session.
///
/// [`ThemePreview::cancel`] is idempotent and safe to call on any exit —
/// closing the panel, jumping to another tab, pressing escape — which is what
/// lets the panel wire "leaving the theme tab reverts" in ONE place instead of
/// at every exit key. A revert remembered at four of the five exits is a picker
/// that silently keeps the theme you last scrolled past.
pub struct ThemePreview {
    presets: &'static [ThemePreset],
    baseline: Option<ThemeState>,
    index: usize,
    previewing: bool,
    apply: ApplyFn,
    persist: Option<PersistFn>,
}

impl ThemePreview {
    /// The live controller: paints through [`apply_theme`], persists nowhere
    /// until a writer is attached (the choice lasts for the process).
    pub fn new(current: Option<ThemeState>) -> Self {
        Self::with_apply(current, Box::new(apply_theme))
    }

    /// Injected apply — a test needs no terminal and no globals.
    pub fn with_apply(current: Option<ThemeState>, apply: ApplyFn) -> Self {
        let index = preset_index(current.as_ref()).unwrap_or(0);
        ThemePreview {
            presets: THEME_PRESETS,
            baseline: current,
            index,
            previewing: false,
            apply,
            persist: None,
        }
    }

    /// Attach the write-behind. Called by [`ThemePreview::commit`] with the
    /// adopted preset; a failed save must never unpaint the screen, so the
    /// writer swallows its own errors (the composition root spawns the request
    /// rather than awaiting it here).
    pub fn with_persist(mut self, persist: PersistFn) -> Self {
        self.persist = Some(persist);
        self
    }

    pub fn presets(&self) -> &'static [ThemePreset] {
        self.presets
    }
    /// Cursor row. Starts on the theme in force, or 0 for a custom palette.
    pub fn index(&self) -> usize {
        self.index
    }
    /// True while a preview is painted that the user has not kept.
    pub fn previewing(&self) -> bool {
        self.previewing
    }
    /// The name of the theme currently painted.
    pub fn name(&self) -> &'static str {
        self.presets
            .get(self.index)
            .map(|p| p.name)
            .unwrap_or("Default")
    }
    /// The baseline in force — what [`ThemePreview::cancel`] restores.
    pub fn baseline(&self) -> Option<&ThemeState> {
        self.baseline.as_ref()
    }

    fn baseline_name(&self) -> String {
        self.baseline
            .as_ref()
            .and_then(|b| b.theme.as_ref().map(|t| t.name.clone()))
            .unwrap_or_else(|| "Default".to_string())
    }

    fn paint(&mut self, i: usize) {
        self.index = i;
        let next = state_for(self.baseline.as_ref(), &self.presets[i]);
        let next_name = next
            .theme
            .as_ref()
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "Default".to_string());
        self.previewing = next_name != self.baseline_name();
        (self.apply)(Some(&next));
    }

    pub fn select(&mut self, i: usize) {
        if i >= self.presets.len() || i == self.index {
            return;
        }
        self.paint(i);
    }

    /// Move the cursor and preview what it lands on. Clamped, never wraps.
    pub fn move_by(&mut self, delta: i64) {
        let max = self.presets.len().saturating_sub(1) as i64;
        let i = (self.index as i64 + delta).clamp(0, max) as usize;
        if i != self.index {
            self.paint(i);
        }
    }

    /// Keep what is painted: the baseline moves and `persist` (if any) is called.
    pub fn commit(&mut self) {
        let preset = &self.presets[self.index];
        let state = state_for(self.baseline.as_ref(), preset);
        self.baseline = Some(state.clone());
        self.previewing = false;
        (self.apply)(Some(&state));
        if let Some(persist) = self.persist.as_mut() {
            // Fire-and-forget: persistence is a write-behind, never a gate on
            // the paint.
            persist(preset, &state);
        }
    }

    /// Restore the baseline. No-op when nothing is being previewed.
    pub fn cancel(&mut self) {
        if !self.previewing {
            return;
        }
        self.previewing = false;
        self.index = preset_index(self.baseline.as_ref()).unwrap_or(0);
        let baseline = self.baseline.clone();
        (self.apply)(baseline.as_ref());
    }
}

// ---------------------------------------------------------------------------
// The theme tab (Theme.tsx)
// ---------------------------------------------------------------------------

/// Slice bounds for a viewport of `height` rows keeping `selected` centered,
/// clamped so the window never runs past either edge (format.ts::windowAround).
fn window_around(selected: usize, total: usize, height: usize) -> (usize, usize) {
    let start = (selected as i64 - (height / 2) as i64)
        .min(total as i64 - height as i64)
        .max(0) as usize;
    (start, start + height)
}

/// The theme tab's rows: browse a palette by wearing it.
///
/// The preview is the product, not a swatch — moving the cursor repaints the
/// whole TUI through the live palette; this function renders rows and nothing
/// else. The revert is not here either: it belongs to whatever owns the tabs.
///
/// One row of chrome — the legend — and it is the LAST row, like every other
/// tab. (`max(3, rows - 5)` reserved five rows for one and then floored the
/// list at three, which is how a short panel came to paint more rows than it
/// had.)
pub fn theme_tab_lines(preview: Option<&ThemePreview>, rows: usize) -> Vec<Line<'static>> {
    let p = palette();
    let Some(preview) = preview else {
        return vec![Line::from(Span::styled(
            "loading theme…",
            Style::default().add_modifier(Modifier::DIM),
        ))];
    };
    let height = rows.saturating_sub(1);
    let total = preview.presets().len();
    let (start, end) = window_around(preview.index(), total, height);
    let mut out: Vec<Line<'static>> = Vec::new();
    if height > 0 {
        let end = end.min(total);
        for (i, preset) in preview.presets()[start.min(end)..end].iter().enumerate() {
            let sel = start + i == preview.index();
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled(
                if sel { "❯ " } else { "  " },
                if sel {
                    Style::default().fg(p.accent_color())
                } else {
                    Style::default()
                },
            ));
            spans.push(Span::styled(
                format!("{:<16}", preset.name),
                if sel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ));
            for cell in preset_swatch(preset) {
                spans.push(Span::styled(
                    cell.block,
                    Style::default().fg(TuiPalette::color(&cell.color)),
                ));
            }
            spans.push(Span::styled(
                format!("  {}", preset.note),
                Style::default().add_modifier(Modifier::DIM),
            ));
            out.push(Line::from(spans));
        }
    }
    out.push(Line::from(Span::styled(
        format!(
            "{}{} — ↑↓ preview live · ⏎ keep · esc back (leaving reverts)",
            if preview.previewing() {
                "previewing "
            } else {
                "current: "
            },
            preview.name()
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));
    out
}

/// Draw the theme tab into `area`.
pub fn render_theme_tab(preview: Option<&ThemePreview>, area: Rect, buf: &mut Buffer) {
    let lines = theme_tab_lines(preview, area.height as usize);
    Paragraph::new(lines).render(area, buf);
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The palette is process state; tests that apply themes serialize on this
    /// and leave it as found.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn preset(name: &str) -> &'static ThemePreset {
        THEME_PRESETS.iter().find(|p| p.name == name).unwrap()
    }

    fn state_of(p: &ThemePreset) -> ThemeState {
        ThemeState {
            theme: Some(NamedTheme {
                name: p.name.to_string(),
                colors: p.colors_map(),
            }),
            defaults: ThemeColors::new(),
        }
    }

    /// Relative luminance, WCAG 2.1 §relative-luminance.
    fn luminance(hex: &str) -> f64 {
        let (r, g, b) = rgb(hex).unwrap();
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn drag_selection_is_legible_in_every_preset() {
        // THE BUG THIS EXISTS FOR: the selection overlay resolved BOTH sides to
        // white and every selected cell came out #ffffff on #ffffff — the text
        // you were dragging over disappeared while you dragged over it.
        // Through `apply_theme`, not `resolve_colors`: a preset carries a
        // PARTIAL palette and the gaps are filled at apply time.
        let _g = guard();
        for p in THEME_PRESETS {
            apply_theme(Some(&state_of(p)));
            let live = palette();
            let ratio = contrast(&live.bg, &live.accent);
            assert!(
                ratio >= 4.5,
                "{}: selection is {ratio:.2}:1 ({} on {}) — WCAG AA for text is 4.5:1",
                p.name,
                live.bg,
                live.accent
            );
        }
        apply_theme(None);
    }

    #[test]
    fn applying_paints_the_screen_background_not_just_the_field() {
        let _g = guard();
        let painted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = painted.clone();
        // Registering paints immediately: the theme is applied BEFORE the
        // renderer exists, so a painter that waited for the next apply would
        // leave the user's own stored theme as the one theme never painted.
        set_background_painter(move |hex: &str| sink.lock().unwrap().push(hex.to_string()));
        assert_eq!(painted.lock().unwrap().len(), 1);

        apply_theme(Some(&state_of(preset("Midnight"))));
        assert_eq!(painted.lock().unwrap().last().unwrap(), "#0a0b0e");
        assert_eq!(palette().bg, "#0a0b0e");

        clear_background_painter();
        let before = painted.lock().unwrap().len();
        apply_theme(None);
        // Deregistered: `bough exec` and every test apply themes with no
        // renderer, and painting into a torn-down one is the crash this avoids.
        assert_eq!(painted.lock().unwrap().len(), before);
    }

    #[test]
    fn the_sgr_path_moves_with_the_theme() {
        // THE BUG THIS EXISTS FOR: the component palette was frozen ANSI names,
        // so the transcript wore Rosé Pine and the composer's border beside it
        // stayed terminal-green. One screen, two palettes.
        let _g = guard();
        apply_theme(Some(&state_of(preset("Rosé Pine Moon"))));
        let live = palette();
        assert_eq!(live.accent, "#c4a7e7");
        assert_eq!(live.warn, "#f6c177");
        assert_eq!(live.error, "#eb6f92");
        assert_eq!(live.accent_color(), Color::Rgb(0xc4, 0xa7, 0xe7));
        assert_eq!(crate::format::colors().accent, fg_params("#c4a7e7"));
        assert_eq!(crate::format::colors().surface_bg, bg_params("#393552"));
        apply_theme(None);
    }

    #[test]
    fn every_apply_bumps_the_epoch_and_notifies() {
        let _g = guard();
        let seen = Arc::new(Mutex::new(Vec::<u64>::new()));
        let sink = seen.clone();
        let stop = subscribe_theme(move || sink.lock().unwrap().push(theme_epoch()));
        let before = theme_epoch();
        apply_theme(None);
        assert_eq!(*seen.lock().unwrap(), vec![before + 1]);
        // The epoch a listener reads is the one AFTER the apply — a subscriber
        // that re-renders must never see a half-applied palette.
        assert_eq!(seen.lock().unwrap()[0], theme_epoch());

        drop(stop);
        apply_theme(None);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "unsubscribed listeners stop hearing"
        );
    }

    #[test]
    fn previewing_notifies_so_a_repaint_has_something_to_hang_off() {
        // The picker mutates process state and the panel's `move` returns its
        // state unchanged, so without this notification an idle TUI had no
        // reason to redraw and the live preview repainted nothing.
        let _g = guard();
        let bumps = Arc::new(AtomicUsize::new(0));
        let sink = bumps.clone();
        let stop = subscribe_theme(move || {
            sink.fetch_add(1, Ordering::SeqCst);
        });
        let mut preview = ThemePreview::new(None);
        preview.move_by(1);
        assert_eq!(bumps.load(Ordering::SeqCst), 1);
        preview.cancel();
        assert_eq!(bumps.load(Ordering::SeqCst), 2);
        drop(stop);
        apply_theme(None);
    }

    // -- the preview controller, with no globals ---------------------------

    fn recording() -> (ThemePreview, Arc<Mutex<Vec<Option<ThemeState>>>>) {
        let log: Arc<Mutex<Vec<Option<ThemeState>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let preview = ThemePreview::with_apply(
            None,
            Box::new(move |s: Option<&ThemeState>| sink.lock().unwrap().push(s.cloned())),
        );
        (preview, log)
    }

    #[test]
    fn cancel_restores_the_baseline_byte_for_byte() {
        let base = state_of(preset("Fjord"));
        let log: Arc<Mutex<Vec<Option<ThemeState>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let mut preview = ThemePreview::with_apply(
            Some(base.clone()),
            Box::new(move |s: Option<&ThemeState>| sink.lock().unwrap().push(s.cloned())),
        );
        assert_eq!(preview.index(), 1);
        preview.move_by(3);
        assert!(preview.previewing());
        assert_eq!(preview.name(), "Rosewood");
        preview.cancel();
        assert!(!preview.previewing());
        assert_eq!(preview.index(), 1);
        assert_eq!(log.lock().unwrap().last().unwrap().as_ref(), Some(&base));
        // Idempotent: any exit key may call it.
        let n = log.lock().unwrap().len();
        preview.cancel();
        assert_eq!(log.lock().unwrap().len(), n);
    }

    #[test]
    fn move_is_clamped_and_never_wraps() {
        let (mut preview, _log) = recording();
        preview.move_by(-1);
        assert_eq!(preview.index(), 0);
        preview.move_by(100);
        assert_eq!(preview.index(), THEME_PRESETS.len() - 1);
        preview.move_by(100);
        assert_eq!(preview.index(), THEME_PRESETS.len() - 1);
    }

    #[test]
    fn select_out_of_range_or_onto_itself_is_a_no_op() {
        let (mut preview, log) = recording();
        preview.select(0);
        preview.select(THEME_PRESETS.len());
        assert!(log.lock().unwrap().is_empty());
        preview.select(2);
        assert_eq!(preview.index(), 2);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn commit_moves_the_baseline_and_persists() {
        let persisted: Arc<Mutex<Vec<(String, ThemeWrite)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = persisted.clone();
        let (preview, _log) = recording();
        let mut preview =
            preview.with_persist(Box::new(move |p: &'static ThemePreset, s: &ThemeState| {
                sink.lock()
                    .unwrap()
                    .push((p.name.to_string(), persist_request(s)));
            }));
        preview.move_by(2); // Iris
        preview.commit();
        assert!(!preview.previewing());
        assert_eq!(
            preview.baseline().unwrap().theme.as_ref().unwrap().name,
            "Iris"
        );
        // Cancel after a commit is a no-op: the baseline moved with it.
        preview.cancel();
        assert_eq!(preview.index(), 2);
        let out = persisted.lock().unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "Iris");
        assert!(matches!(&out[0].1, ThemeWrite::Put { name, .. } if name == "Iris"));
    }

    #[test]
    fn default_persists_as_a_delete() {
        // Never PUT an empty map — it would store a named theme overriding
        // nothing.
        let state = state_for(Some(&state_of(preset("Iris"))), preset("Default"));
        assert_eq!(state.theme, None);
        assert_eq!(persist_request(&state), ThemeWrite::Delete);

        let persisted: Arc<Mutex<Vec<ThemeWrite>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = persisted.clone();
        let mut preview =
            ThemePreview::with_apply(Some(state_of(preset("Iris"))), Box::new(|_| {}))
                .with_persist(Box::new(move |_p, s: &ThemeState| {
                    sink.lock().unwrap().push(persist_request(s))
                }));
        preview.select(0);
        preview.commit();
        assert_eq!(*persisted.lock().unwrap(), vec![ThemeWrite::Delete]);
    }

    #[test]
    fn a_committed_state_keeps_the_servers_defaults() {
        let base = ThemeState {
            theme: None,
            defaults: [("green".to_string(), "#010203".to_string())]
                .into_iter()
                .collect(),
        };
        let next = state_for(Some(&base), preset("Lagoon"));
        assert_eq!(next.defaults["green"], "#010203");
        assert_eq!(resolve_colors(Some(&next))["green"], "#3fbdb0");
    }

    #[test]
    fn preset_index_is_none_for_a_custom_palette() {
        assert_eq!(preset_index(None), Some(0));
        assert_eq!(preset_index(Some(&state_of(preset("Lagoon")))), Some(5));
        let custom = ThemeState {
            theme: Some(NamedTheme {
                name: "Handmade".into(),
                colors: [("green".to_string(), "#123456".to_string())]
                    .into_iter()
                    .collect(),
            }),
            defaults: ThemeColors::new(),
        };
        assert_eq!(preset_index(Some(&custom)), None);
        // …and a custom palette parks the cursor on row 0 rather than nowhere.
        let preview = ThemePreview::with_apply(Some(custom), Box::new(|_| {}));
        assert_eq!(preview.index(), 0);
    }

    #[test]
    fn swatches_come_from_the_presets_own_colors() {
        let _g = guard();
        apply_theme(Some(&state_of(preset("Rosé Pine Moon"))));
        let cells = preset_swatch(preset("Default"));
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[0].token, "bg");
        assert_eq!(cells[0].color, "#0e1013"); // FALLBACK bg, not the live one
        assert_eq!(cells[0].block, "███");
        assert_eq!(cells[3].token, "green");
        assert_eq!(cells[3].color, "#4ec98f");
        assert_eq!(cells[3].block, "██");
        apply_theme(None);
    }

    #[test]
    fn defaults_layer_under_the_theme_and_over_fallback() {
        let state = ThemeState {
            theme: Some(NamedTheme {
                name: "X".into(),
                colors: [("green".to_string(), "#111111".to_string())]
                    .into_iter()
                    .collect(),
            }),
            defaults: [
                ("green".to_string(), "#222222".to_string()),
                ("amber".to_string(), "#333333".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let c = resolve_colors(Some(&state));
        assert_eq!(c["green"], "#111111");
        assert_eq!(c["amber"], "#333333");
        assert_eq!(c["red"], "#e2776e");
    }

    #[test]
    fn sgr_params_expand_short_hex() {
        assert_eq!(fg_params("#4ec98f"), "38;2;78;201;143");
        assert_eq!(fg_params("#abc"), "38;2;170;187;204");
        assert_eq!(bg_params("#4ec98f"), "48;2;78;201;143");
        // Junk in a stored theme paints black rather than panicking.
        assert_eq!(fg_params("nope"), "38;2;0;0;0");
        assert_eq!(TuiPalette::color("nope"), Color::Reset);
    }

    #[test]
    fn the_state_round_trips_the_wire_shape() {
        let json = r##"{"theme":{"name":"Iris","colors":{"green":"#9a7fd1"}},"defaults":{}}"##;
        let state: ThemeState = serde_json::from_str(json).unwrap();
        assert_eq!(preset_index(Some(&state)), Some(2));
        // A server that omits the key entirely still parses (older servers).
        let empty: ThemeState = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.theme, None);
    }

    // -- the tab render ----------------------------------------------------

    fn rendered(preview: Option<&ThemePreview>, w: u16, h: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render_theme_tab(preview, f.area(), f.buffer_mut()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_tab_lists_presets_with_the_cursor_and_the_legend_last() {
        let (mut preview, _log) = recording();
        preview.move_by(1);
        let out = rendered(Some(&preview), 80, 10);
        assert!(out[0].starts_with("  Default"), "{:?}", out[0]);
        assert!(out[1].starts_with("❯ Fjord"), "{:?}", out[1]);
        assert!(out[0].contains("built-in palette"), "{:?}", out[0]);
        assert_eq!(
            out[9],
            "previewing Fjord — ↑↓ preview live · ⏎ keep · esc back (leaving reverts)"
        );
    }

    #[test]
    fn the_legend_says_current_when_nothing_is_previewed() {
        let (preview, _log) = recording();
        let out = rendered(Some(&preview), 70, 10);
        assert!(
            out[9].starts_with("current: Default — ↑↓ preview live"),
            "{:?}",
            out[9]
        );
    }

    #[test]
    fn a_one_row_panel_paints_only_the_legend() {
        // `max(3, rows - 5)` floored the list at three, which is how a short
        // panel came to paint more rows than it had.
        let (preview, _log) = recording();
        let out = rendered(Some(&preview), 70, 1);
        assert!(out[0].starts_with("current: Default"), "{:?}", out[0]);
    }

    #[test]
    fn the_window_follows_the_cursor_off_the_bottom() {
        let (mut preview, _log) = recording();
        preview.move_by(100);
        let out = rendered(Some(&preview), 70, 4);
        // Three list rows + the legend, ending on the selected last preset.
        assert!(out[2].starts_with("❯ Rosé Pine Moon"), "{:?}", out[2]);
        assert!(
            out[3].starts_with("previewing Rosé Pine Moon"),
            "{:?}",
            out[3]
        );
    }

    #[test]
    fn no_preview_yet_says_so() {
        let out = rendered(None, 40, 3);
        assert_eq!(out[0], "loading theme…");
    }
}
