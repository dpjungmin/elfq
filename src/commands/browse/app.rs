//! The browser's state: the pages the user has opened, kept as a web
//! browser keeps its history, so that Back and Forward move through them,
//! the go-to box, find's prompt, and how the screen is laid out around the
//! page on top.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
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
/// What find says on a page without rows, such as the help.
const NO_ROWS: &str = "this page has no rows to find in";

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
    /// What is typed after /, while find's prompt is open.
    finding: Option<String>,
    /// The text that find looked for last, for n and N.
    searched: Option<String>,
}

/// Where find looks from the selected row.
#[derive(Clone, Copy)]
enum FindFrom {
    /// The first row that has the text, from the selected one on.
    Here,
    /// The next or previous one, after or before the selected one.
    Next,
    Previous,
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
            finding: None,
            searched: None,
        };
        // The start page is worked out before it is first drawn. What
        // keeps each item waits until the first page is drawn, with a
        // status line that says so (see `event_loop`).
        app.work_out_start_page();
        app
    }

    /// What the browser has yet to work out for the screen, which the
    /// status line says while it does, once the go-to box isn't over the
    /// page: the start page; what keeps each item, which the headers of
    /// items' pages say, once after the first page is drawn; and tab 4 of
    /// an item's page, the first time it is shown.
    fn waiting(&self) -> Option<String> {
        if self.picker.is_some() {
            return None;
        }
        match self.history.last()? {
            View::Start(None) => Some(format!(
                "working out where {} starts…",
                self.session.file_name()
            )),
            _ if self.session.needs_keepers() => {
                Some("working out what keeps each item…".to_string())
            }
            View::Item(page) => page.waiting(),
            _ => None,
        }
    }

    /// Whether the page on the screen has something to work out, which
    /// can take a while in a large file: then the event loop draws the
    /// screen, which says so, before [`App::work`] does it.
    pub fn has_work(&self) -> bool {
        self.waiting().is_some()
    }

    /// Works out what the screen has yet to (see [`App::has_work`]).
    pub fn work(&mut self) {
        self.work_out_start_page();
        if self.picker.is_none() && self.session.needs_keepers() {
            self.session.work_out_keepers();
            // The pages so far say it in their headers too.
            for view in self.history.iter_mut().chain(&mut self.forward) {
                if let View::Item(page) = view {
                    page.add_keeping(&self.session);
                }
            }
        }
        if let Some(View::Item(page)) = self.history.last_mut()
            && page.waiting().is_some()
        {
            page.work(&self.session);
        }
    }

    /// Works out the start page, if it is on the screen and the go-to box
    /// isn't over it.
    fn work_out_start_page(&mut self) {
        if self.picker.is_none()
            && let Some(View::Start(start @ None)) = self.history.last_mut()
        {
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
        if self.finding.is_some() {
            return self.key_to_find(key);
        }
        let action = keys::action(key)?;
        self.message = None;
        match action {
            Action::Quit => return Some(Stop::Quit),
            Action::Suspend => return Some(Stop::Suspend),
            Action::GoTo => self.picker = Some(Picker::new(&self.session, "")),
            Action::Find => match self.page().cursor() {
                Some(_) => self.finding = Some(String::new()),
                None => self.message = Some(NO_ROWS.to_string()),
            },
            Action::FindNext | Action::FindPrevious => match self.searched.clone() {
                Some(text) => {
                    let from = match action {
                        Action::FindNext => FindFrom::Next,
                        _ => FindFrom::Previous,
                    };
                    self.find(&text, from);
                }
                None => self.message = Some("press / to find text first".to_string()),
            },
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
    /// before it: text typed into the go-to box or find's prompt,
    /// Backspace and Esc there, and a key that quits or suspends. Every
    /// other key moves by what the screen shows, such as PageDown by a
    /// screenful, so it is handled once the screen is drawn.
    pub fn can_wait_to_draw(&self, key: KeyEvent) -> bool {
        if self.picker.is_some() || self.finding.is_some() {
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
        if let Some(stop) = keys::while_typing(key) {
            return Some(stop);
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
            _ => {
                if let Some(c) = keys::typed_char(key) {
                    picker.type_char(Some(c));
                }
            }
        }
        None
    }

    /// Does what `key` asks of find's prompt. Keys type into it, so only
    /// Ctrl-C and Ctrl-Z do what they do elsewhere.
    fn key_to_find(&mut self, key: KeyEvent) -> Option<Stop> {
        if let Some(stop) = keys::while_typing(key) {
            return Some(stop);
        }
        let typed = self.finding.as_mut()?;
        match key.code {
            KeyCode::Esc => self.finding = None,
            KeyCode::Enter => {
                let text = std::mem::take(typed);
                self.finding = None;
                if !text.is_empty() {
                    self.find(&text, FindFrom::Here);
                    self.searched = Some(text);
                }
            }
            KeyCode::Backspace => {
                typed.pop();
            }
            _ => {
                if let Some(c) = keys::typed_char(key) {
                    typed.push(c);
                }
            }
        }
        None
    }

    /// Selects a row of the page that has `text`, ignoring case, looking
    /// `from` the selected one and going round the end, and says which of
    /// how many it is, or that none has it.
    fn find(&mut self, text: &str, from: FindFrom) {
        let lowercase = text.to_lowercase();
        let page = self.page();
        let (rows, noun) = (page.rows_with(&lowercase), page.row_noun());
        let Some(cursor) = page.cursor() else {
            self.message = Some(NO_ROWS.to_string());
            return;
        };
        if rows.is_empty() {
            self.message = Some(format!("no {noun} has \"{text}\""));
            return;
        }
        let at = cursor.selected;
        let count = rows.len();
        let index = match from {
            FindFrom::Here => rows.partition_point(|&row| row < at) % count,
            FindFrom::Next => rows.partition_point(|&row| row <= at) % count,
            FindFrom::Previous => (rows.partition_point(|&row| row < at) + count - 1) % count,
        };
        cursor.select(rows[index]);
        self.message = Some(match count {
            1 => format!("the only {noun} with \"{text}\""),
            _ => format!("{} of {count} {noun}s with \"{text}\"", index + 1),
        });
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
        let status_line = match (waiting, &self.finding, &self.message) {
            (Some(waiting), _, _) => Line::raw(cut(&waiting, width)),
            // What is typed is cut from the left, so that its end shows.
            (None, Some(typed), _) => Line::raw(cut_left(&format!("find: {typed}▏"), width)),
            (None, None, Some(message)) => Line::raw(cut(message, width)),
            (None, None, None) => {
                Line::raw(cut(&view.preview(&self.session).unwrap_or_default(), width)).dim()
            }
        };
        frame.render_widget(status_line, status);
        draw_lines(
            frame,
            command_area,
            command.into_iter().map(Line::raw).collect(),
        );
        let hint = match self.finding {
            Some(_) => "enter find  esc cancel".to_string(),
            None => keys::hints(
                width,
                view.has_links(),
                &view.tabs(),
                view.cursor().is_some(),
            ),
        };
        frame.render_widget(Line::raw(hint).dim(), hints);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;

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
    fn what_keeps_each_item_is_worked_out_after_the_first_draw() {
        let pages = [
            ("c-x86_64.elf", Some("main")),
            ("c-x86_64.elf", None),
            ("c-x86_64.so", Some("log_msg")),
        ];
        for (sample, symbol) in pages {
            let path = samples().into_iter().find(|path| path.ends_with(sample));
            let path = path.unwrap();
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let mut app = App::new(session, symbol);
            // Not in a shared library, about whose items `retained` and
            // `why` don't answer.
            let waits = !sample.ends_with(".so");
            let waiting = app.waiting();
            let says = waiting.as_deref() == Some("working out what keeps each item…");
            assert_eq!(says, waits, "{sample} {symbol:?}");
            app.work();
            assert!(!app.has_work(), "{sample} {symbol:?}");
            assert_eq!(
                app.session.keepers().is_some(),
                waits,
                "{sample} {symbol:?}"
            );
        }
    }

    #[test]
    fn the_tabs_show_their_selected_row_on_a_small_screen() {
        // Long names make long headers, notes and commands, which leave
        // the tabs few lines.
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
                finding: None,
                searched: None,
            };
            // As after the first page is drawn.
            app.session.work_out_keepers();
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes: Vec<Node> = (items.chain(imports))
                .filter(|&node| app.session.page_node(node) == node)
                .filter(|node| node.name(&binary).chars().count() > 100)
                .collect();
            for (node, tab) in nodes
                .into_iter()
                .flat_map(|node| [2, 3, 4].map(|tab| (node, tab)))
            {
                let name = node.name(&binary);
                let mut page = ItemPage::new(&app.session, node, None, name);
                if page.handle(&app.session, Action::Tab(tab)).is_some() {
                    // The item doesn't have the tab.
                    continue;
                }
                app.history = vec![View::Item(Box::new(page))];
                // Tab 4 is worked out once the screen says so.
                assert_eq!(app.has_work(), tab == 4, "{name}");
                if app.has_work() {
                    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                    let lines = lines(&terminal);
                    let said = lines
                        .iter()
                        .any(|line| line.starts_with("working out what "));
                    assert!(said, "{name}:\n{}", lines.join("\n"));
                    app.work();
                }
                for (width, height) in [(80, 24), (60, 16)] {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                    let lines = lines(&terminal);
                    let shown = |text: &str| lines.iter().any(|line| line.contains(text));
                    // The table's header and the selected row, when the
                    // tab has rows, each of which opens an item, and the
                    // header's facts.
                    let has_rows = app.page().has_links();
                    let rows = !has_rows
                        || (shown("HOW") || shown("ADDRESS"))
                            && lines.iter().any(|line| line.starts_with("> "));
                    let screen = lines.join("\n");
                    assert!(rows, "{}, {name}, {tab}:\n{screen}", path.display());
                    assert!(shown("used by"), "{}, {name}:\n{screen}", path.display());
                }
                pages += 1;
            }
        }
        assert!(pages > 300, "{pages}");
    }
}
