//! The browser's state: the pages the user has opened, kept as a web
//! browser keeps its history, so that Back and Forward move through them,
//! and how the screen is laid out around the page on top.

use anyhow::{Result, bail};
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::Stop;
use super::keys::{self, Action};
use super::session::Session;
use super::views::{Go, ItemPage, View, cut, cut_left, imported};
use crate::binary::hex_number;
use crate::commands::{found_functions, overview, text};
use crate::graph::Node;

/// The smallest screen the browser draws on.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 16;

/// The browser: the loaded file and the pages opened on it.
pub struct App<'a> {
    session: Session<'a>,
    /// The pages opened, oldest first; the last is on the screen.
    history: Vec<View>,
    /// The pages that Back left, the last one left last, for Forward.
    forward: Vec<View>,
    /// Said on the status line until the next key.
    message: Option<String>,
}

impl<'a> App<'a> {
    /// Opens the page of what `symbol` names, as the commands find it, or
    /// without one, of the function that the overview starts from: `main`,
    /// else the entry point, else the largest function. The file's first
    /// warning is on the status line, unless something about `symbol` is.
    pub fn new(session: Session<'a>, symbol: Option<&str>) -> Result<App<'a>> {
        let warnings = &session.binary.warnings;
        let mut message = warnings.first().map(|first| match warnings.len() {
            1 => format!("warning: {first}"),
            more => format!(
                "warning: {first}; and {} more, printed when you quit",
                more - 1
            ),
        });
        let mut first = None;
        if let Some(query) = symbol {
            match session.names.find(query) {
                Ok(found) => match found.node {
                    Node::Item(index) => {
                        // An address selects the line that holds it.
                        let at = hex_number(query).and_then(Result::ok);
                        let lead = found.lead_name(session.binary);
                        first = Some(ItemPage::new(&session, index, at, &lead));
                    }
                    Node::Import(_) => message = Some(imported(&session, found.node)),
                },
                // Such as "`log` could mean 2 functions", the first line of
                // the CLI's error, without what it says about the list of
                // them that the CLI prints after it.
                Err(error) => {
                    let error = error.to_string();
                    let line = error.lines().next().unwrap_or_default();
                    let line = line.trim_end_matches(':');
                    message = Some(
                        line.trim_end_matches("; use one on the left instead")
                            .to_string(),
                    );
                }
            }
        }
        let first = match first {
            Some(page) => page,
            None => starting_page(&session)?,
        };
        Ok(App {
            session,
            history: vec![View::Item(Box::new(first))],
            forward: Vec::new(),
            message,
        })
    }

    /// Does what `key` asks, and says whether the browser should stop.
    pub fn key(&mut self, key: KeyEvent) -> Option<Stop> {
        let action = keys::action(key)?;
        self.message = None;
        match action {
            Action::Quit => return Some(Stop::Quit),
            Action::Suspend => return Some(Stop::Suspend),
            Action::Back => self.back(),
            Action::Forward => {
                if let Some(view) = self.forward.pop() {
                    self.history.push(view);
                }
            }
            _ => match self
                .history
                .last_mut()
                .map(|view| view.handle(&self.session, action))
            {
                Some(Some(Go::Open(view))) => {
                    self.history.push(view);
                    self.forward.clear();
                }
                Some(Some(Go::Say(text))) => self.message = Some(text),
                _ => {}
            },
        }
        None
    }

    /// Goes back within the page, as from a jump inside a function, or
    /// else to the page before.
    fn back(&mut self) {
        if self.page().back() {
            return;
        }
        if self.history.len() == 1 {
            self.message = Some("this is the first page; q quits".to_string());
            return;
        }
        if let Some(view) = self.history.pop() {
            self.forward.push(view);
        }
    }

    /// The page on the screen.
    fn page(&mut self) -> &mut View {
        self.history
            .last_mut()
            .expect("the first page is never closed")
    }

    /// Draws the screen: the title line, with the file's name and the
    /// pages that led here, the page, the status line, and the hints.
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            // Wrapped, so that the size shows on the narrowest screen.
            let text = format!("make the terminal at least {MIN_WIDTH}x{MIN_HEIGHT}");
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
            return;
        }
        let width = usize::from(area.width);
        let [title, body, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        // The trail is cut from the left, so that the page on the screen
        // shows.
        let mut trail = self.session.file_name();
        for view in &self.history {
            trail += " › ";
            trail += view.title();
        }
        frame.render_widget(Line::raw(cut_left(&trail, width)).bold(), title);

        let view = self
            .history
            .last_mut()
            .expect("the first page is never closed");
        view.draw(frame, body);
        let status_line = match &self.message {
            Some(message) => Line::raw(cut(message, width)),
            None => Line::raw(cut(&view.preview(&self.session).unwrap_or_default(), width)).dim(),
        };
        frame.render_widget(status_line, status);
        frame.render_widget(Line::raw(keys::hints(width, view.has_links())).dim(), hints);
    }
}

/// The page that opens when no SYMBOL does: the function that the
/// overview starts from.
fn starting_page(session: &Session) -> Result<ItemPage> {
    let binary = session.binary;
    let Some(index) = overview::starting_point(binary, &session.names) else {
        if !found_functions(binary) {
            bail!("{}", text::NO_FUNCTIONS);
        }
        bail!("elfq found no function with code in the file to start from; give browse a SYMBOL");
    };
    Ok(ItemPage::new(
        session,
        index,
        None,
        &binary.items[index].name,
    ))
}
