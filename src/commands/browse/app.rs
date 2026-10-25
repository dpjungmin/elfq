//! The browser's state: the pages the user has opened, kept as a web
//! browser keeps its history, so that Back and Forward move through them,
//! the go-to box, and how the screen is laid out around the page on top.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::Stop;
use super::keys::{self, Action};
use super::session::Session;
use super::views::{
    Go, Help, ItemPage, Picker, View, cut, cut_left, draw_lines, first_line, keep_lines,
    start_page, wrap,
};
use crate::binary::hex_number;

/// The smallest screen the browser draws on.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 16;
/// The fewest lines that the page keeps when the command under it is
/// long: two lines of the item's name and where it is, its facts, the tab
/// line, and the header and one row of a table.
const PAGE_LINES: usize = 6;

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
    /// without one, the start page. When `symbol` finds several items or
    /// none, the go-to box opens with `symbol` typed in, over the start
    /// page, which is worked out only once the box is closed over it. The
    /// file's first warning is on the status line.
    pub fn new(session: Session<'a>, symbol: Option<&str>) -> App<'a> {
        let warnings = &session.binary.warnings;
        let message = warnings.first().map(|first| match warnings.len() {
            1 => format!("warning: {first}"),
            more => format!(
                "warning: {first}; and {} more, printed when you quit",
                more - 1
            ),
        });
        let mut first = View::Start(None);
        let mut picker = None;
        if let Some(query) = symbol {
            match session.names.find(query) {
                Ok(found) => {
                    // An address selects the line that holds it.
                    let at = hex_number(query).and_then(Result::ok);
                    let lead = found.lead_name(session.binary);
                    let page = ItemPage::new(&session, found.node, at, &lead);
                    first = View::Item(Box::new(page));
                }
                // The CLI's error goes over the rows.
                Err(error) => {
                    picker = Some(Picker::noted(&session, query, first_line(&error)));
                }
            }
        }
        let mut app = App {
            session,
            history: vec![first],
            forward: Vec::new(),
            message,
            picker,
        };
        app.work();
        app
    }

    /// What the page on the screen has yet to work out, which the status
    /// line says while it does: the start page, once the go-to box isn't
    /// over it.
    fn waiting(&self) -> Option<String> {
        if self.picker.is_some() {
            return None;
        }
        match self.history.last()? {
            View::Start(None) => Some(format!(
                "working out where {} starts…",
                self.session.file_name()
            )),
            _ => None,
        }
    }

    /// Whether the page on the screen has something to work out, which
    /// can take a while in a large file: then the event loop draws the
    /// screen, which says so, before [`App::work`] does it.
    pub fn has_work(&self) -> bool {
        self.waiting().is_some()
    }

    /// Works out what the page on the screen has yet to.
    pub fn work(&mut self) {
        if !self.has_work() {
            return;
        }
        if let Some(View::Start(start)) = self.history.last_mut() {
            *start = Some(Box::new(start_page(&self.session)));
        }
    }

    /// Does what `key` asks, and says whether the browser should stop.
    pub fn key(&mut self, key: KeyEvent) -> Option<Stop> {
        // Every key acts on the page as worked out.
        self.work();
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

    /// Whether `key` can be handled before the screen shows the keys
    /// before it: text typed into the go-to box, Backspace and Esc there,
    /// and a key that quits or suspends. Every other key moves by what the
    /// screen shows, such as PageDown by a screenful, so it is handled
    /// once the screen is drawn.
    pub fn can_wait_to_draw(&self, key: KeyEvent) -> bool {
        if self.picker.is_some() {
            return matches!(
                key.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc
            );
        }
        matches!(keys::action(key), Some(Action::Quit | Action::Suspend))
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
                if let Some(found) = picker.selected(session) {
                    // The header names the item as SYMBOL's answer does,
                    // with what holds an address typed, or the other name
                    // that found it.
                    let name = found.node.name(session.binary);
                    let lead = found.lead.as_deref().unwrap_or(name);
                    let page = ItemPage::new(session, found.node, found.at, lead);
                    self.picker = None;
                    self.message = None;
                    self.open(View::Item(Box::new(page)));
                }
            }
            KeyCode::Up => picker.handle(session, Action::Up),
            KeyCode::Down => picker.handle(session, Action::Down),
            KeyCode::PageUp => picker.handle(session, Action::PageUp),
            KeyCode::PageDown => picker.handle(session, Action::PageDown),
            KeyCode::Home => picker.handle(session, Action::First),
            KeyCode::End => picker.handle(session, Action::Last),
            KeyCode::Backspace => picker.type_char(None),
            // Other Ctrl and Alt keys type nothing.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                picker.type_char(Some(c));
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
    /// the command that prints what the page shows, if one does, and the
    /// hints.
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            // Wrapped, so that the size shows on the narrowest screen.
            let text = format!("make the terminal at least {MIN_WIDTH}x{MIN_HEIGHT}");
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
            return;
        }
        let width = usize::from(area.width);
        // The command wraps, rather than being cut, so that all of it
        // can be copied, unless that leaves the page fewer than
        // PAGE_LINES lines.
        let command = match (&self.picker, self.history.last()) {
            (None, Some(view)) => view.command().map_or(Vec::new(), |line| wrap(line, width)),
            _ => Vec::new(),
        };
        let most = usize::from(area.height).saturating_sub(3 + PAGE_LINES);
        let command = keep_lines(command, most.max(1), width);
        let [title, body, status, command_area, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(command.len() as u16),
            Constraint::Length(1),
        ])
        .areas(area);

        // The trail is cut from the left, so that the page on the screen
        // shows, and long names in it are shortened to fit after the
        // file's name.
        let mut trail = self.session.file_name();
        let room = width.saturating_sub(trail.chars().count() + 3).max(20);
        for view in &mut self.history {
            let title = view.title(room);
            if !title.is_empty() {
                trail += " › ";
                trail += title;
            }
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
        let waiting = self.waiting();
        let view = self
            .history
            .last_mut()
            .expect("the first page is never closed");
        view.draw(frame, body);
        let status_line = match (waiting, &self.message) {
            (Some(waiting), _) => Line::raw(cut(&waiting, width)),
            (None, Some(message)) => Line::raw(cut(message, width)),
            (None, None) => {
                Line::raw(cut(&view.preview(&self.session).unwrap_or_default(), width)).dim()
            }
        };
        frame.render_widget(status_line, status);
        draw_lines(
            frame,
            command_area,
            command.into_iter().map(Line::raw).collect(),
        );
        let hint = keys::hints(width, view.has_links(), !view.tabs().is_empty());
        frame.render_widget(Line::raw(hint).dim(), hints);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::views::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::graph::Node;

    /// The screen as lines of text.
    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_start_page_under_go_to_is_worked_out_once_the_box_closes() {
        let path = samples()
            .into_iter()
            .find(|path| path.ends_with("c-x86_64.elf"));
        let path = path.unwrap();
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        let mut app = App::new(session, Some("no_such_symbol"));
        assert!(matches!(app.history[..], [View::Start(None)]) && !app.has_work());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.has_work());
        app.work();
        assert!(matches!(app.history[..], [View::Start(Some(_))]) && !app.has_work());
    }

    #[test]
    fn the_callers_tab_shows_its_selected_row_on_a_small_screen() {
        // Long names make long headers, notes and commands, which leave
        // the tab few lines.
        let mut pages = 0;
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let mut app = App {
                session,
                history: Vec::new(),
                forward: Vec::new(),
                message: None,
                picker: None,
            };
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes: Vec<Node> = (items.chain(imports))
                .filter(|&node| app.session.page_node(node) == node)
                .filter(|node| node.name(&binary).chars().count() > 100)
                .collect();
            for node in nodes {
                let name = node.name(&binary);
                let mut page = ItemPage::new(&app.session, node, None, name);
                page.handle(&app.session, Action::Tab(3));
                app.history = vec![View::Item(Box::new(page))];
                for (width, height) in [(80, 24), (60, 16)] {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                    let lines = lines(&terminal);
                    let shown = |text: &str| lines.iter().any(|line| line.contains(text));
                    // The table's header and the selected row, or what
                    // the command says when nothing uses the item, and
                    // the header's facts.
                    let rows = shown("Nothing in the file")
                        || shown("HOW") && lines.iter().any(|line| line.starts_with("> "));
                    let screen = lines.join("\n");
                    assert!(rows, "{}, {name}:\n{screen}", path.display());
                    assert!(shown("used by"), "{}, {name}:\n{screen}", path.display());
                }
                pages += 1;
            }
        }
        assert!(pages > 100, "{pages}");
    }
}
