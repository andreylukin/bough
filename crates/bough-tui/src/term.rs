//! Terminal capability detection, and the zero-width escape sequences that act
//! on the terminal itself (port of `src/tui/term.ts`).
//!
//! THE INVARIANT: **what a terminal can do is a pure function of its
//! environment, and every decision that depends on it is taken here.**
//! `term_caps` takes an env map and returns booleans; it reads no globals,
//! writes nothing, and needs no TTY.
//!
//! WHY THE GATING EXISTS: OSC 9;4 is taskbar progress in Ghostty/iTerm2 and a
//! DESKTOP NOTIFICATION in kitty — an ungated keep-alive pops a banner every
//! five seconds. Terminal.app accepts OSC 9 and displays nothing, so it gets
//! BEL. tmux swallows unknown OSC, so outer-terminal sequences are
//! passthrough-wrapped, and the iTerm2 tab tint is simply not sent under tmux.
//!
//! THE KITTY FLAG IS ABOUT TRUST, NOT ABOUT PUSHING: the keyboard protocol is
//! pushed unconditionally (`kitty_keyboard_mode`); `caps.kitty` decides whether
//! `super` can be BELIEVED.
//!
//! Effects are behind `Term` with an injected writer and injected timers, so a
//! test drives every sequence into a string buffer and never waits.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use base64::Engine as _;

// ---------------------------------------------------------------------------
// Capabilities (pure)
// ---------------------------------------------------------------------------

/// Just enough of an environment to classify a terminal.
pub type TermEnv = HashMap<String, String>;

/// Build a [`TermEnv`] from pairs — test/fixture convenience.
pub fn env_of(pairs: &[(&str, &str)]) -> TermEnv {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notify {
    Osc9,
    Bell,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TermCaps {
    /// `TERM_PROGRAM`, verbatim — kept so a caller can log what was detected.
    pub program: String,
    /// `TERM`, verbatim.
    pub term: String,
    /// Inside tmux: the outer terminal is unknowable and OSC needs wrapping.
    pub tmux: bool,
    /// Inside zellij: use its action CLI to rename the focused tab.
    pub zellij: bool,
    /// The terminal is known to implement the kitty keyboard protocol, so
    /// `super` is a real modifier. False under tmux — not because tmux cannot
    /// pass it, but because we cannot tell from here whether it will.
    pub kitty: bool,
    /// OSC 9;4 is rendered as progress rather than popped as a notification.
    pub progress: bool,
    /// iTerm2's tab tint is available (and the outer terminal is known).
    pub tab_color: bool,
    /// How a desktop notification is delivered.
    pub notify: Notify,
}

/// Terminals that ship the kitty keyboard protocol. Membership, not versions.
const KITTY_PROGRAMS: [&str; 4] = ["ghostty", "WezTerm", "iTerm.app", "rio"];
/// …and the ones identifiable only by `TERM`.
const KITTY_TERMS: [&str; 3] = ["xterm-kitty", "foot", "foot-extra"];
/// Terminals that render OSC 9;4. kitty parses OSC 9 as a notification — never here.
const PROGRESS_PROGRAMS: [&str; 3] = ["ghostty", "iTerm.app", "WezTerm"];

pub fn term_caps(env: &TermEnv) -> TermCaps {
    let program = env.get("TERM_PROGRAM").cloned().unwrap_or_default();
    let term = env.get("TERM").cloned().unwrap_or_default();
    let tmux = env.get("TMUX").is_some_and(|v| !v.is_empty());
    let zellij = env.get("ZELLIJ").is_some();
    let kitty = !tmux
        && (KITTY_PROGRAMS.contains(&program.as_str())
            || KITTY_TERMS.contains(&term.as_str())
            || env.get("KITTY_WINDOW_ID").is_some_and(|v| !v.is_empty()));
    TermCaps {
        tmux,
        zellij,
        kitty,
        progress: PROGRESS_PROGRAMS.contains(&program.as_str()),
        tab_color: program == "iTerm.app" && !tmux,
        notify: if program == "Apple_Terminal" {
            Notify::Bell
        } else {
            Notify::Osc9
        },
        program,
        term,
    }
}

/// The kitty keyboard mode — always on, never "auto". "auto" probes with a
/// round trip that tmux eats; an unsupported push is ignored.
pub fn kitty_keyboard_mode() -> &'static str {
    "enabled"
}

/// Titles and notification bodies must not smuggle control bytes into the stream.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if code <= 0x1f || code == 0x7f {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// tmux swallows unknown OSC — passthrough-wrap so it reaches the outer terminal.
pub fn tmux_wrap(seq: &str, in_tmux: bool) -> String {
    if in_tmux {
        format!(
            "\u{1b}Ptmux;{}\u{1b}\\",
            seq.replace('\u{1b}', "\u{1b}\u{1b}")
        )
    } else {
        seq.to_string()
    }
}

/// "rgb:1e1e/1e1e/2e2e" (1–4 hex digits per channel) → "#rrggbb", else None.
pub fn parse_bg_spec(spec: &str) -> Option<String> {
    let t = spec.trim();
    let rest = t.strip_prefix("rgb:").or_else(|| t.strip_prefix("RGB:"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let chan = |h: &str| -> Option<u8> {
        if h.is_empty() || h.len() > 4 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = 16u32.pow(h.len() as u32) - 1;
        Some(((v as f64 / max as f64) * 255.0).round() as u8)
    };
    let (r, g, b) = (chan(parts[0])?, chan(parts[1])?, chan(parts[2])?);
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

/// Rec. 709 luma, split at the midpoint. Pure, so the boundary is testable.
pub fn classify_bg(hex: &str) -> (String, Scheme) {
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64;
    let (r, g, b) = (byte(1), byte(3), byte(5));
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (
        hex.to_string(),
        if luma < 128.0 {
            Scheme::Dark
        } else {
            Scheme::Light
        },
    )
}

/// Frames a bough turn was once animated with IN THE TITLE. Kept only so the
/// regression test in `ident` can assert no title carries one: an animated
/// title is rewritten every frame, which clobbers whatever name the user or
/// the multiplexer set and — under tmux — spawns a rename process per tick.
/// Progress belongs in OSC 9;4, which Ghostty draws over the split itself.
pub const TITLE_SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

/// Injected timers so a test never waits on the keep-alive. The app loop
/// (row 1.39) supplies tokio-backed ones; [`NoopTimers`] never fires, which
/// degrades to "no keep-alive" — the immediate writes still happen.
pub trait TermTimers {
    fn set_interval(&self, f: Box<dyn Fn()>, ms: u64) -> u64;
    fn clear_interval(&self, handle: u64);
    fn set_timeout(&self, f: Box<dyn Fn()>, ms: u64) -> u64;
    fn clear_timeout(&self, handle: u64);
}

/// Timers that never fire. Handles are still unique so clear() bookkeeping works.
pub struct NoopTimers {
    next: Cell<u64>,
}

impl Default for NoopTimers {
    fn default() -> Self {
        NoopTimers { next: Cell::new(1) }
    }
}

impl TermTimers for NoopTimers {
    fn set_interval(&self, _f: Box<dyn Fn()>, _ms: u64) -> u64 {
        let h = self.next.get();
        self.next.set(h + 1);
        h
    }
    fn clear_interval(&self, _handle: u64) {}
    fn set_timeout(&self, _f: Box<dyn Fn()>, _ms: u64) -> u64 {
        let h = self.next.get();
        self.next.set(h + 1);
        h
    }
    fn clear_timeout(&self, _handle: u64) {}
}

pub struct TermOptions {
    pub caps: TermCaps,
    /// Where sequences go. A test passes a collector; production writes stdout.
    pub write: Rc<dyn Fn(&str)>,
    /// Renames tmux's current window; injected so effects remain testable.
    pub rename_tmux_window: Option<Rc<dyn Fn(&str)>>,
    /// Renames zellij's focused tab; injected likewise.
    pub rename_zellij_tab: Option<Rc<dyn Fn(&str)>>,
    pub timers: Rc<dyn TermTimers>,
}

/// The terminal-effect surface. See the TS `Term` interface.
pub struct Term {
    pub caps: TermCaps,
    write: Rc<dyn Fn(&str)>,
    rename_tmux_window: Rc<dyn Fn(&str)>,
    rename_zellij_tab: Rc<dyn Fn(&str)>,
    timers: Rc<dyn TermTimers>,
    progress_timer: Cell<Option<u64>>,
    progress_err_timer: Rc<Cell<Option<u64>>>,
    focused: Cell<bool>,
    bg: RefCell<Option<String>>,
}

pub fn create_term(options: TermOptions) -> Term {
    Term {
        caps: options.caps,
        write: options.write,
        rename_tmux_window: options
            .rename_tmux_window
            .unwrap_or_else(|| Rc::new(|_| {})),
        rename_zellij_tab: options.rename_zellij_tab.unwrap_or_else(|| Rc::new(|_| {})),
        timers: options.timers,
        progress_timer: Cell::new(None),
        progress_err_timer: Rc::new(Cell::new(None)),
        focused: Cell::new(true),
        bg: RefCell::new(None),
    }
}

impl Term {
    /// Name the terminal pane and any enclosing tmux window or zellij tab.
    ///
    /// Two forms, because the slots have wildly different widths: a window
    /// title has room for the semantic name, a tmux window name has about
    /// fourteen columns and gets the stable handle instead (see `ident`).
    pub fn set_title(&self, long: &str, short: &str) {
        (self.write)(&format!("\u{1b}]0;{}\u{7}", sanitize(long)));
        // OSC 0 names the pane; tmux's local action names its window even where
        // allow-rename disables terminal sequences.
        if self.caps.tmux {
            (self.rename_tmux_window)(&sanitize(short));
        }
        // OSC titles do not affect zellij's tab bar; its documented action does.
        if self.caps.zellij {
            (self.rename_zellij_tab)(&sanitize(short));
        }
    }

    /// Only while unfocused: a banner about the screen you are looking at is noise.
    pub fn notify_desktop(&self, body: &str) {
        if self.focused.get() {
            return;
        }
        if self.caps.notify == Notify::Bell {
            (self.write)("\u{7}");
            return;
        }
        (self.write)(&tmux_wrap(
            &format!("\u{1b}]9;{}\u{7}", sanitize(body)),
            self.caps.tmux,
        ));
    }

    /// Indeterminate progress while a turn runs, kept alive until `progress_end`.
    /// Ghostty expires stale progress after ~15s, so it is re-asserted every 5s.
    pub fn progress_start(&self) {
        if !self.caps.progress {
            return;
        }
        if let Some(h) = self.progress_err_timer.get() {
            self.timers.clear_timeout(h);
            self.progress_err_timer.set(None);
        }
        (self.write)("\u{1b}]9;4;3\u{7}");
        if self.progress_timer.get().is_none() {
            let write = Rc::clone(&self.write);
            let h = self
                .timers
                .set_interval(Box::new(move || write("\u{1b}]9;4;3\u{7}")), 5000);
            self.progress_timer.set(Some(h));
        }
    }

    pub fn progress_end(&self, error: bool) {
        if !self.caps.progress {
            return;
        }
        if let Some(h) = self.progress_timer.get() {
            self.timers.clear_interval(h);
            self.progress_timer.set(None);
        }
        if !error {
            (self.write)("\u{1b}]9;4;0\u{7}");
            return;
        }
        (self.write)("\u{1b}]9;4;2;100\u{7}");
        if self.progress_err_timer.get().is_none() {
            let write = Rc::clone(&self.write);
            let slot = Rc::clone(&self.progress_err_timer);
            let h = self.timers.set_timeout(
                Box::new(move || {
                    slot.set(None);
                    write("\u{1b}]9;4;0\u{7}");
                }),
                4000,
            );
            self.progress_err_timer.set(Some(h));
        }
    }

    /// Tint the iTerm2 tab; None resets. No-op anywhere else.
    pub fn tab_color(&self, hex: Option<&str>) {
        if !self.caps.tab_color {
            return;
        }
        let Some(hex) = hex else {
            (self.write)("\u{1b}]6;1;bg;*;default\u{7}");
            return;
        };
        let h = hex.strip_prefix('#').unwrap_or(hex);
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
        let (r, g, b) = (byte(0), byte(2), byte(4));
        (self.write)(&format!(
            "\u{1b}]6;1;bg;red;brightness;{r}\u{7}\u{1b}]6;1;bg;green;brightness;{g}\u{7}\u{1b}]6;1;bg;blue;brightness;{b}\u{7}"
        ));
    }

    /// Escape-sequence clipboard write — reaches the LOCAL terminal over
    /// SSH/tmux. Capped well under xterm's ~100KB whole-sequence limit. No
    /// tmux wrap: tmux translates OSC 52 itself when set-clipboard is on.
    pub fn osc52_copy(&self, text: &str) {
        let bytes = text.as_bytes();
        let capped = &bytes[..bytes.len().min(72_000)];
        let b64 = base64::engine::general_purpose::STANDARD.encode(capped);
        (self.write)(&format!("\u{1b}]52;c;{b64}\u{7}"));
    }

    /// Ask for the background colour; the reply arrives on stdin via the filter.
    pub fn query_term_bg(&self) {
        (self.write)("\u{1b}]11;?\u{7}");
    }

    /// Fed by the stdin filter. A malformed report never clobbers a good one.
    pub fn report_term_bg(&self, spec: &str) {
        if let Some(hex) = parse_bg_spec(spec) {
            *self.bg.borrow_mut() = Some(hex);
        }
    }

    pub fn term_background(&self) -> Option<(String, Scheme)> {
        self.bg.borrow().as_ref().map(|hex| classify_bg(hex))
    }

    /// Fed by the stdin filter (`\x1b[I` / `\x1b[O`).
    pub fn set_focused(&self, v: bool) {
        self.focused.set(v);
    }

    pub fn is_focused(&self) -> bool {
        self.focused.get()
    }

    /// Clear every sticky state this object set. Called on the way out.
    pub fn cleanup(&self) {
        if let Some(h) = self.progress_timer.get() {
            self.timers.clear_interval(h);
            self.progress_timer.set(None);
        }
        if let Some(h) = self.progress_err_timer.get() {
            self.timers.clear_timeout(h);
            self.progress_err_timer.set(None);
        }
        if self.caps.progress {
            (self.write)("\u{1b}]9;4;0\u{7}");
        }
        if self.caps.tab_color {
            (self.write)("\u{1b}]6;1;bg;*;default\u{7}");
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal size
// ---------------------------------------------------------------------------

/// Columns and rows, with the fallbacks the renderer needs when there is no tty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// The terminal's current size, measured rather than remembered; clamps
/// (min 20×8) and falls back to 80×24 for a pipe.
pub fn terminal_size() -> TermSize {
    match crossterm::terminal::size() {
        Ok((cols, rows)) => TermSize {
            cols: if cols == 0 { 80 } else { cols }.max(20),
            rows: if rows == 0 { 24 } else { rows }.max(8),
        },
        Err(_) => TermSize { cols: 80, rows: 24 },
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/term.test.ts (all 22 cases)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A term wired to a string log, with timers that never fire on their own.
    struct FakeTimers {
        next: Cell<u64>,
        timers: RefCell<HashMap<u64, Rc<dyn Fn()>>>,
    }

    impl FakeTimers {
        fn new() -> Self {
            FakeTimers {
                next: Cell::new(1),
                timers: RefCell::new(HashMap::new()),
            }
        }
    }

    impl TermTimers for FakeTimers {
        fn set_interval(&self, f: Box<dyn Fn()>, _ms: u64) -> u64 {
            let h = self.next.get();
            self.next.set(h + 1);
            self.timers.borrow_mut().insert(h, Rc::from(f));
            h
        }
        fn clear_interval(&self, handle: u64) {
            self.timers.borrow_mut().remove(&handle);
        }
        fn set_timeout(&self, f: Box<dyn Fn()>, _ms: u64) -> u64 {
            let h = self.next.get();
            self.next.set(h + 1);
            self.timers.borrow_mut().insert(h, Rc::from(f));
            h
        }
        fn clear_timeout(&self, handle: u64) {
            self.timers.borrow_mut().remove(&handle);
        }
    }

    struct Harness {
        term: Term,
        out: Rc<RefCell<Vec<String>>>,
        timers: Rc<FakeTimers>,
    }

    impl Harness {
        fn text(&self) -> String {
            self.out.borrow().join("")
        }
        fn last(&self) -> String {
            self.out.borrow().last().cloned().unwrap_or_default()
        }
        fn timer_count(&self) -> usize {
            self.timers.timers.borrow().len()
        }
        fn fire_all(&self) {
            let fns: Vec<Rc<dyn Fn()>> = self.timers.timers.borrow().values().cloned().collect();
            for f in fns {
                f();
            }
        }
    }

    fn harness(caps: TermCaps) -> Harness {
        let out: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let timers = Rc::new(FakeTimers::new());
        let sink = Rc::clone(&out);
        let term = create_term(TermOptions {
            caps,
            write: Rc::new(move |seq| sink.borrow_mut().push(seq.to_string())),
            rename_tmux_window: None,
            rename_zellij_tab: None,
            timers: Rc::clone(&timers) as Rc<dyn TermTimers>,
        });
        Harness { term, out, timers }
    }

    // ---- capabilities ----

    #[test]
    fn kitty_support_is_detected_by_program_by_term_and_by_kittys_own_env_var() {
        assert!(term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])).kitty);
        assert!(term_caps(&env_of(&[("TERM_PROGRAM", "WezTerm")])).kitty);
        assert!(term_caps(&env_of(&[("TERM", "xterm-kitty")])).kitty);
        assert!(term_caps(&env_of(&[("KITTY_WINDOW_ID", "3")])).kitty);
        assert!(!term_caps(&env_of(&[("TERM_PROGRAM", "Apple_Terminal")])).kitty);
        assert!(!term_caps(&env_of(&[])).kitty);
    }

    #[test]
    fn under_tmux_the_outer_terminal_is_unknowable_so_super_is_not_trusted() {
        let env = env_of(&[("TERM_PROGRAM", "ghostty"), ("TMUX", "/tmp/x,1,0")]);
        assert!(!term_caps(&env).kitty);
        assert!(term_caps(&env).tmux);
    }

    #[test]
    fn the_keyboard_protocol_is_pushed_unconditionally_never_probed() {
        assert_eq!(kitty_keyboard_mode(), "enabled");
    }

    #[test]
    fn osc_9_4_progress_is_only_sent_to_terminals_that_render_it() {
        assert!(term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])).progress);
        assert!(term_caps(&env_of(&[("TERM_PROGRAM", "iTerm.app")])).progress);
        // kitty parses OSC 9 as a notification: progress here is banner spam.
        assert!(!term_caps(&env_of(&[("TERM", "xterm-kitty")])).progress);
        assert!(!term_caps(&env_of(&[])).progress);
    }

    #[test]
    fn the_tab_tint_is_iterm2s_alone_and_not_under_tmux() {
        assert!(term_caps(&env_of(&[("TERM_PROGRAM", "iTerm.app")])).tab_color);
        assert!(!term_caps(&env_of(&[("TERM_PROGRAM", "iTerm.app"), ("TMUX", "x")])).tab_color);
        assert!(!term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])).tab_color);
    }

    #[test]
    fn terminal_app_gets_a_bell_because_it_accepts_osc_9_and_shows_nothing() {
        assert_eq!(
            term_caps(&env_of(&[("TERM_PROGRAM", "Apple_Terminal")])).notify,
            Notify::Bell
        );
        assert_eq!(
            term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])).notify,
            Notify::Osc9
        );
        assert!(term_caps(&env_of(&[("ZELLIJ", "0")])).zellij);
        assert!(!term_caps(&env_of(&[])).zellij);
    }

    // ---- pure helpers ----

    #[test]
    fn sanitize_strips_control_bytes_from_titles_and_notification_bodies() {
        assert_eq!(sanitize("ok\u{7}\u{1b}]0;evil\u{7}"), "ok  ]0;evil ");
        assert_eq!(sanitize("plain title"), "plain title");
    }

    #[test]
    fn tmux_wrap_doubles_every_esc_and_wraps_in_the_passthrough_dcs() {
        assert_eq!(
            tmux_wrap("\u{1b}]9;hi\u{7}", true),
            "\u{1b}Ptmux;\u{1b}\u{1b}]9;hi\u{7}\u{1b}\\"
        );
        assert_eq!(tmux_wrap("\u{1b}]9;hi\u{7}", false), "\u{1b}]9;hi\u{7}");
    }

    #[test]
    fn parse_bg_spec_scales_16_8_4_bit_channels_to_rrggbb() {
        assert_eq!(
            parse_bg_spec("rgb:1e1e/1e1e/2e2e").as_deref(),
            Some("#1e1e2e")
        );
        assert_eq!(parse_bg_spec("rgb:fa/fa/fa").as_deref(), Some("#fafafa"));
        assert_eq!(parse_bg_spec("rgb:f/0/f").as_deref(), Some("#ff00ff"));
        assert_eq!(parse_bg_spec("not-a-color"), None);
    }

    #[test]
    fn classify_bg_splits_dark_from_light_on_rec_709_luma() {
        assert_eq!(
            classify_bg("#1e1e2e"),
            ("#1e1e2e".to_string(), Scheme::Dark)
        );
        assert_eq!(
            classify_bg("#fafafa"),
            ("#fafafa".to_string(), Scheme::Light)
        );
    }

    // ---- effects ----

    #[test]
    fn a_malformed_background_report_never_clobbers_a_good_one() {
        let h = harness(term_caps(&env_of(&[])));
        assert_eq!(h.term.term_background(), None); // null until the terminal answers
        h.term.report_term_bg("rgb:1e1e/1e1e/2e2e");
        assert_eq!(
            h.term.term_background(),
            Some(("#1e1e2e".to_string(), Scheme::Dark))
        );
        h.term.report_term_bg("garbage");
        assert_eq!(h.term.term_background().unwrap().0, "#1e1e2e");
    }

    #[test]
    fn a_notification_fires_only_while_the_terminal_is_unfocused() {
        let h = harness(term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])));
        h.term.notify_desktop("done"); // focused by default
        assert_eq!(h.text(), "");
        h.term.set_focused(false);
        assert!(!h.term.is_focused());
        h.term.notify_desktop("done");
        assert_eq!(h.last(), "\u{1b}]9;done\u{7}");
    }

    #[test]
    fn progress_is_a_no_op_where_it_would_be_read_as_a_notification() {
        let kitty = harness(term_caps(&env_of(&[("TERM", "xterm-kitty")])));
        kitty.term.progress_start();
        kitty.term.progress_end(false);
        assert_eq!(kitty.text(), "");
        assert_eq!(kitty.timer_count(), 0); // and no keep-alive left running

        let ghostty = harness(term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])));
        ghostty.term.progress_start();
        assert_eq!(ghostty.out.borrow()[0], "\u{1b}]9;4;3\u{7}");
        assert_eq!(ghostty.timer_count(), 1); // Ghostty expires stale progress; re-assert
        ghostty.term.progress_end(false);
        assert_eq!(ghostty.last(), "\u{1b}]9;4;0\u{7}");
        assert_eq!(ghostty.timer_count(), 0);
    }

    #[test]
    fn an_errored_turn_flashes_the_error_state_then_clears_it_on_a_timer() {
        let h = harness(term_caps(&env_of(&[("TERM_PROGRAM", "ghostty")])));
        h.term.progress_start();
        h.term.progress_end(true);
        assert_eq!(h.last(), "\u{1b}]9;4;2;100\u{7}");
        assert_eq!(h.timer_count(), 1);
        h.fire_all();
        assert_eq!(h.last(), "\u{1b}]9;4;0\u{7}");
    }

    #[test]
    fn the_tab_tint_parses_a_hex_colour_and_resets_on_null() {
        let h = harness(term_caps(&env_of(&[("TERM_PROGRAM", "iTerm.app")])));
        h.term.tab_color(Some("#ff8800"));
        assert_eq!(
            h.last(),
            "\u{1b}]6;1;bg;red;brightness;255\u{7}\u{1b}]6;1;bg;green;brightness;136\u{7}\u{1b}]6;1;bg;blue;brightness;0\u{7}"
        );
        h.term.tab_color(Some("not a colour"));
        assert_eq!(h.out.borrow().len(), 1); // unparseable: nothing written at all
        h.term.tab_color(None);
        assert_eq!(h.last(), "\u{1b}]6;1;bg;*;default\u{7}");
    }

    #[test]
    fn cleanup_clears_every_sticky_state_and_cancels_every_timer() {
        let h = harness(term_caps(&env_of(&[("TERM_PROGRAM", "iTerm.app")])));
        h.term.progress_start();
        h.term.cleanup();
        assert_eq!(h.timer_count(), 0);
        assert!(h.out.borrow().iter().any(|s| s == "\u{1b}]9;4;0\u{7}"));
        assert!(h
            .out
            .borrow()
            .iter()
            .any(|s| s == "\u{1b}]6;1;bg;*;default\u{7}"));
    }

    #[test]
    fn the_title_names_the_terminal_pane() {
        let h = harness(term_caps(&env_of(&[])));
        h.term.set_title(
            "● fix\u{7} the parser · brisk-heron · bough",
            "● brisk-heron",
        );
        assert_eq!(
            *h.out.borrow(),
            vec!["\u{1b}]0;● fix  the parser · brisk-heron · bough\u{7}".to_string()]
        );
    }

    #[test]
    fn a_tmux_session_also_names_its_current_window() {
        let titles: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&titles);
        let term = create_term(TermOptions {
            caps: term_caps(&env_of(&[("TMUX", "1")])),
            write: Rc::new(|_| {}),
            rename_tmux_window: Some(Rc::new(move |t| sink.borrow_mut().push(t.to_string()))),
            rename_zellij_tab: None,
            timers: Rc::new(NoopTimers::default()),
        });
        term.set_title(
            "● fix the parser · brisk-heron · bough",
            "● brisk\u{7}-heron",
        );
        assert_eq!(*titles.borrow(), vec!["● brisk -heron".to_string()]);
    }

    #[test]
    fn a_zellij_session_also_names_its_focused_multiplexer_tab() {
        let titles: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&titles);
        let term = create_term(TermOptions {
            caps: term_caps(&env_of(&[("ZELLIJ", "1")])),
            write: Rc::new(|_| {}),
            rename_tmux_window: None,
            rename_zellij_tab: Some(Rc::new(move |t| sink.borrow_mut().push(t.to_string()))),
            timers: Rc::new(NoopTimers::default()),
        });
        term.set_title(
            "● fix the parser · brisk-heron · bough",
            "● brisk\u{7}-heron",
        );
        assert_eq!(*titles.borrow(), vec!["● brisk -heron".to_string()]);
    }

    #[test]
    fn osc_52_base64_encodes_the_clipboard_payload_and_caps_it() {
        let h = harness(term_caps(&env_of(&[])));
        h.term.osc52_copy("hi");
        assert_eq!(
            h.last(),
            format!(
                "\u{1b}]52;c;{}\u{7}",
                base64::engine::general_purpose::STANDARD.encode("hi")
            )
        );
        h.term.osc52_copy(&"x".repeat(200_000));
        // 72_000 bytes → 96_000 base64 chars.
        assert_eq!(
            h.last().chars().count(),
            96_000 + "\u{1b}]52;c;\u{7}".chars().count()
        );
    }
}
