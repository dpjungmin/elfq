//! The browser's state: the pages the user has opened, kept as a web
//! browser keeps its history, so that Back and Forward move through them,
//! the go-to box, and how the screen is laid out around the page on top.

use anyhow::{Result, bail};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::Stop;
use super::keys::{self, Action};
use super::session::Session;
use super::views::{Go, Help, ItemPage, Picker, View, cut, cut_left, first_line, imported};
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
    /// The go-to box, while it is open.
    picker: Option<Picker>,
}

impl<'a> App<'a> {
    /// Opens the page of what `symbol` names, as the commands find it, or
    /// without one, of the function that the overview starts from: `main`,
    /// else the entry point, else the largest function. When `symbol`
    /// finds several items or none, that page opens with the go-to box
    /// over it, `symbol` typed in. The file's first warning is on the
    /// status line, unless something about `symbol` is.
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
        let mut picker = None;
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
                // The CLI's error goes over the rows.
                Err(error) => {
                    picker = Some(Picker::noted(&session, query, first_line(&error)));
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
            picker,
        })
    }

    /// Does what `key` asks, and says whether the browser should stop.
    pub fn key(&mut self, key: KeyEvent) -> Option<Stop> {
        if self.picker.is_some() {
            return self.key_to_picker(key);
        }
        let action = keys::action(key)?;
        self.message = None;
        match action {
            Action::Quit => return Some(Stop::Quit),
            Action::Suspend => return Some(Stop::Suspend),
            Action::GoTo => self.picker = Some(Picker::new(&self.session, "")),
            Action::Help => {
                if !matches!(self.page(), View::Help(_)) {
                    self.open(View::Help(Help::new()));
                }
            }
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
                Some(Some(Go::Open(view))) => self.open(view),
                Some(Some(Go::Say(text))) => self.message = Some(text),
                _ => {}
            },
        }
        None
    }

    /// Does what `key` asks of the go-to box. Keys type into it, so only
    /// Ctrl-C and Ctrl-Z do what they do elsewhere.
    fn key_to_picker(&mut self, key: KeyEvent) -> Option<Stop> {
        match keys::while_typing(key) {
            Some(Action::Quit) => return Some(Stop::Quit),
            Some(Action::Suspend) => return Some(Stop::Suspend),
            _ => {}
        }
        let picker = self.picker.as_mut()?;
        let session = &self.session;
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => {
                if let Some(found) = picker.selected() {
                    // The header names the item as SYMBOL's answer does,
                    // with what holds an address typed, or the other name
                    // that found it.
                    let name = &session.binary.items[found.index].name;
                    let lead = found.lead.as_ref().unwrap_or(name);
                    let page = ItemPage::new(session, found.index, found.at, lead);
                    self.picker = None;
                    self.message = None;
                    self.open(View::Item(Box::new(page)));
                }
            }
            KeyCode::Up => picker.handle(Action::Up),
            KeyCode::Down => picker.handle(Action::Down),
            KeyCode::PageUp => picker.handle(Action::PageUp),
            KeyCode::PageDown => picker.handle(Action::PageDown),
            KeyCode::Home => picker.handle(Action::First),
            KeyCode::End => picker.handle(Action::Last),
            KeyCode::Backspace => picker.type_char(session, None),
            // Other Ctrl and Alt keys type nothing.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                picker.type_char(session, Some(c));
            }
            _ => {}
        }
        None
    }

    /// Opens `view` after the page on the screen, as following a link
    /// does, which leaves nothing to go forward to.
    fn open(&mut self, view: View) {
        self.history.push(view);
        self.forward.clear();
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
    /// pages that led here, the page or the go-to box, the status line,
    /// and the hints.
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
        if self.picker.is_some() {
            trail += " › go to";
        }
        frame.render_widget(Line::raw(cut_left(&trail, width)).bold(), title);

        if let Some(picker) = &mut self.picker {
            picker.draw(&self.session, frame, body);
            // What is typed is cut from the left, so that its end shows.
            let typed = format!("go to: {}▏", picker.text);
            frame.render_widget(Line::raw(cut_left(&typed, width)), status);
            let hint = "↑↓ move  enter open  esc cancel";
            frame.render_widget(Line::raw(hint).dim(), hints);
            return;
        }
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
