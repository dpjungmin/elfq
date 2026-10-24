//! `elfq FILE browse [SYMBOL]`: a function's code in the terminal, with a
//! link on each line that calls, jumps to or uses another item, to follow
//! as in a web browser: Enter opens the item's page, and Esc comes back.
//!
//! The parts, each in its own file:
//!
//! - `session.rs`: the loaded file and its reference graph, and the
//!   answers that pages ask for.
//! - `keys.rs`: which key does what, as an `Action`, in one table that the
//!   hint line is printed from too.
//! - `app.rs`: the pages the user has opened, as a browser's history, and
//!   the screen's layout around the page on top.
//! - `views/`: one file per kind of page. A page draws itself, and answers
//!   an action by moving, by opening another page, or with a message.
//!
//! This file takes the terminal and reads its keys. [`event_loop`] takes
//! the keys from anywhere and draws on any ratatui backend, so that tests
//! drive the browser with keys and read what it draws, without a terminal.
//! Keys that come faster than a large file's answers, as a paste's do,
//! aren't each answered and drawn in turn: what is typed into go to waits
//! for the next draw, which comes when no key is left waiting.

mod app;
mod keys;
mod session;
mod views;

pub use app::App;
pub use session::Session;

use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::{DefaultTerminal, Terminal};

use super::text;
use crate::binary::Binary;

/// Why the event loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The user quit, or the keys ran out.
    Quit,
    /// The user pressed Ctrl-Z, to get back to the shell for now.
    Suspend,
}

/// Browses the file at `path`, starting at what `symbol` names, until the
/// user quits. The file's warnings go to stderr after that, once the
/// screen is back, or before the error when browse refuses the file.
pub fn run(path: &Path, symbol: Option<&str>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "browse needs a terminal; to print answers, run elfq {} or another command",
            text::shell_quote(&path.display().to_string())
        );
    }
    // On elfq's own thread, with its large stack, as for every command:
    // the browser starts no other thread.
    let binary = Binary::load(path)?;
    let result = browse(path, &binary, symbol);
    super::write_warnings(&binary);
    result
}

/// Browses `binary`, loaded from `path`, on the real terminal until the
/// user quits. When elfq can't read the file's code, or finds nothing to
/// start from, it says so before it takes the terminal.
fn browse(path: &Path, binary: &Binary, symbol: Option<&str>) -> Result<()> {
    let session = Session::new(path, binary)?;
    let mut app = App::new(session, symbol)?;
    // ratatui sets a panic hook that gives the terminal back first.
    let mut terminal = ratatui::try_init()?;
    let result = read_keys(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// Runs the event loop on the real terminal until the user quits,
/// stepping out for each Ctrl-Z.
fn read_keys(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let next_event = || Ok(Some(event::read()?));
    let more_waiting = || Ok(event::poll(Duration::ZERO)?);
    while event_loop(terminal, app, next_event, more_waiting)? == Stop::Suspend {
        suspend(terminal)?;
    }
    Ok(())
}

/// Draws `app` on `terminal`, then gives it each key that `next_event`
/// returns, and draws it again, until the user quits or suspends, or
/// `next_event` returns `None`. While `more_waiting` says that another
/// key is already waiting, as when the user pastes or types faster than
/// the screen keeps up, it takes that key before drawing, if the key can
/// wait for the draw (see [`App::can_wait_to_draw`]). Any other key is
/// handled on the screen that the keys before it give, as when each is
/// drawn in turn, since it moves by what the screen shows.
pub fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut next_event: impl FnMut() -> Result<Option<Event>>,
    mut more_waiting: impl FnMut() -> Result<bool>,
) -> Result<Stop>
where
    B::Error: Send + Sync + 'static,
{
    // Whether the screen doesn't show the keys taken yet.
    let mut behind = true;
    loop {
        if behind && !more_waiting()? {
            terminal.draw(|frame| app.draw(frame))?;
            behind = false;
        }
        let Some(event) = next_event()? else {
            return Ok(Stop::Quit);
        };
        // Only presses: Windows also reports each key's release. Anything
        // else, such as a new size, needs only the drawing above.
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            if behind && !app.can_wait_to_draw(key) {
                terminal.draw(|frame| app.draw(frame))?;
            }
            if let Some(stop) = app.key(key) {
                return Ok(stop);
            }
        }
        behind = true;
    }
}

/// Gives the terminal back and stops elfq, as Ctrl-Z stops other programs,
/// then takes the screen again when the shell continues it.
#[cfg(unix)]
fn suspend(terminal: &mut DefaultTerminal) -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
    use rustix::process::{Signal, kill_current_process_group};
    use signal_hook::consts::SIGCONT;

    terminal.show_cursor()?;
    ratatui::restore();
    // The whole process group stops, as when the terminal itself sends
    // Ctrl-Z, so that a parent that waits for elfq, such as `sh -c` or
    // make, stops too, and the shell gets the terminal back. The signal
    // may stop elfq a moment after this thread goes on, so it waits until
    // the shell continues elfq before it takes the screen again.
    let continued = Arc::new(AtomicBool::new(false));
    let handler = signal_hook::flag::register(SIGCONT, Arc::clone(&continued))?;
    kill_current_process_group(Signal::TSTP)?;
    // A process group that no shell controls, as when tmux runs elfq
    // without one, doesn't stop: then elfq takes the screen back at once.
    for _ in 0..50 {
        if continued.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    signal_hook::low_level::unregister(handler);
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

/// Elsewhere, Ctrl-Z does nothing, and the help doesn't list it.
#[cfg(not(unix))]
fn suspend(_: &mut DefaultTerminal) -> Result<()> {
    Ok(())
}
