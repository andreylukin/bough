//! Invariant: `Backend::Auto` is resolved ONCE, at boot, and the rest of the shell draws through
//! one type either way (P3-D2). A profile that mounts the `tui` row without a terminal — `--check`,
//! CI, the invariant audit — gets the headless `TestBackend` and every other behaviour unchanged;
//! nothing downstream branches on which one it is.

use std::io::Stdout;

use ratatui::backend::{Backend as RatatuiBackend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// The two backends the shell can own, behind one type so `Terminal<TermBackend>` is monomorphic.
pub enum TermBackend {
    Crossterm(ratatui::backend::CrosstermBackend<Stdout>),
    Headless(TestBackend),
}

impl TermBackend {
    /// The headless one, at `size`.
    pub fn headless(size: [u16; 2]) -> TermBackend {
        TermBackend::Headless(TestBackend::new(size[0].max(1), size[1].max(1)))
    }
    /// The real one, on stdout.
    pub fn crossterm() -> TermBackend {
        TermBackend::Crossterm(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
    }
}

/// `TestBackend`'s error is `Infallible` and crossterm's is `io::Error`; the union is `io::Error`,
/// and the `Infallible` arm is unreachable by construction rather than by assertion.
fn never<T>(r: Result<T, core::convert::Infallible>) -> Result<T, std::io::Error> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => match e {},
    }
}

impl RatatuiBackend for TermBackend {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        match self {
            TermBackend::Crossterm(b) => b.draw(content),
            TermBackend::Headless(b) => never(b.draw(content)),
        }
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.hide_cursor(),
            TermBackend::Headless(b) => never(b.hide_cursor()),
        }
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.show_cursor(),
            TermBackend::Headless(b) => never(b.show_cursor()),
        }
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.get_cursor_position(),
            TermBackend::Headless(b) => never(b.get_cursor_position()),
        }
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.set_cursor_position(position),
            TermBackend::Headless(b) => never(b.set_cursor_position(position)),
        }
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.clear(),
            TermBackend::Headless(b) => never(b.clear()),
        }
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.clear_region(clear_type),
            TermBackend::Headless(b) => never(b.clear_region(clear_type)),
        }
    }
    fn size(&self) -> Result<Size, Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.size(),
            TermBackend::Headless(b) => never(b.size()),
        }
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.window_size(),
            TermBackend::Headless(b) => never(b.window_size()),
        }
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.flush(),
            TermBackend::Headless(b) => never(b.flush()),
        }
    }
    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        match self {
            TermBackend::Crossterm(b) => b.append_lines(n),
            TermBackend::Headless(b) => never(b.append_lines(n)),
        }
    }
}
