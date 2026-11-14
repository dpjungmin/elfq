//! The browser's state, and the screen's layout around the page on top.
//!
//! The state is the pages the user has opened, kept as a web browser
//! keeps its history, so that Back and Forward move through them, the
//! go-to box or find's prompt (`Prompt`), and what the screen has yet to
//! work out, in order (`Work`, `App::pending`).

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::Stop;
use super::keys::{self, Action, Typing};
use super::session::Session;
use super::views::{
    Go, Help, ItemPage, Picker, SlowTab, StartPage, View, cut, cut_left, draw_lines, first_line,
    keep_lines, wrap,
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
    /// The go-to box or find's prompt, while one is open.
    prompt: Option<Prompt>,
    /// The text that find looked for last, for n and N.
    searched: Option<String>,
}

/// What the screen has yet to work out, which the status line says first.
///
/// Each can take a while in a large file (see [`App::pending`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// The start page, until it is worked out.
    StartPage,
    /// What keeps each item.
    ///
    /// The headers of items' pages say it, and tabs 5 and 6 need it.
    Keepers,
    /// A tab of the item's page on the screen, the first time it is shown.
    Tab(SlowTab),
}

impl Work {
    /// What the status line says while `session` works it out.
    fn message(self, session: &Session) -> String {
        match self {
            Work::StartPage => format!("working out where {} starts…", session.file_name()),
            Work::Keepers => "working out what keeps each item…".to_string(),
            Work::Tab(tab) => {
                let name = session.row_name(tab.node());
                match tab {
                    SlowTab::Reach(_) => format!("working out what {name} reaches…"),
                    SlowTab::Why(_) => format!("working out what keeps {name} in the file…"),
                    SlowTab::Retained(_) => format!("working out what only {name} leads to…"),
                }
            }
        }
    }
}

/// A prompt that keys type into, over the page on the screen. One is open
/// at most.
enum Prompt {
    /// The go-to box, which lists what the text typed finds in place of
    /// the page. A Picker is far larger than find's String, so it is kept
    /// on the heap, as clippy's `large_enum_variant` asks.
    GoTo(Box<Picker>),
    /// Find's prompt, with what is typed after /.
    Find(String),
}

/// What the status line says: the first of these that the screen has, in
/// this order (see [`App::status_line`]).
enum StatusLine {
    /// What the screen works out next, which can take a while
    /// ([`Work::message`]).
    Working(String),
    /// What is typed into the go-to box or find's prompt, after the
    /// prompt's name: "find: main▏".
    Typed(String),
    /// What was said, until the next key: the file's first warning on the
    /// first screen, or what a key said, such as which of the rows that
    /// find found is selected.
    Message(String),
    /// The selected row's value, when its row cuts it, as it cuts a long
    /// string, broken into lines of the screen's width, so that the status
    /// line shows it in full.
    Value(Vec<String>),
    /// What the selected row's link opens, or an empty line when it has
    /// none ([`View::preview`]). It comes last, so it is what a screen with
    /// nothing else to say shows.
    Preview(String),
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
    /// Opens the page of what `symbol` names, or of the one item that it
    /// matches as a pattern, as the commands find it, or without one, the
    /// start page. When `symbol` finds several items, as a name or as a
    /// pattern, or none, the go-to box opens with `symbol` typed in, over
    /// the start page, which is worked out only once the box is closed over
    /// it. The file's first warning is on the status line.
    pub fn new(session: Session<'a>, symbol: Option<&str>) -> App<'a> {
        let warnings = &session.binary.warnings;
        let message = warnings.first().map(|first| match warnings.len() {
            1 => format!("warning: {first}"),
            more => format!(
                "warning: {first}; and {} more, printed when you quit",
                more - 1
            ),
        });
        let mut first = View::Start(StartPage::new());
        let mut prompt = None;
        if let Some(typed) = symbol {
            match session.names.find(typed) {
                Ok(found) => {
                    // An address selects the line that holds it.
                    let at = hex_number(typed).and_then(Result::ok);
                    let page = ItemPage::new(&session, found.node, at, found.note());
                    first = View::Item(Box::new(page));
                }
                // The CLI's error goes over the rows.
                Err(error) => {
                    let picker = Picker::with_error(&session, typed, first_line(&error));
                    prompt = Some(Prompt::GoTo(Box::new(picker)));
                }
            }
        }
        let mut app = App {
            session,
            history: vec![first],
            forward: Vec::new(),
            message,
            prompt,
            searched: None,
        };
        // Only the start page is worked out before the first draw. What
        // keeps each item waits until the first page is drawn, with a
        // status line that says so (see `event_loop`).
        if app.pending() == Some(Work::StartPage) {
            app.run(Work::StartPage);
        }
        app
    }

    /// What the screen has yet to work out first, if anything.
    ///
    /// The order is this:
    ///
    /// 1. The start page, which has nothing to show until it is worked
    ///    out.
    /// 2. What keeps each item, once, after the first page is drawn, on
    ///    any page: the headers of items' pages say it, and tabs 5 and 6
    ///    need it.
    /// 3. Tab 4, 5 or 6 of an item's page, the first time it is shown.
    ///
    /// Nothing is while the go-to box is open, so that what is typed there
    /// is answered at once, and so that the start page under the box, as
    /// when SYMBOL finds no item, is worked out only once the box closes
    /// over it. Find's prompt needs no such rule: it opens only on a page
    /// that [`App::key`] has worked out, and what is typed there opens no
    /// page.
    fn pending(&self) -> Option<Work> {
        match self.prompt {
            Some(Prompt::GoTo(_)) => return None,
            None | Some(Prompt::Find(_)) => {}
        }
        match self.history.last()? {
            View::Start(start) if !start.is_worked_out() => Some(Work::StartPage),
            _ if self.session.needs_keepers() => Some(Work::Keepers),
            View::Item(page) => page.pending().map(Work::Tab),
            View::Start(_) | View::Help(_) => None,
        }
    }

    /// Whether the screen has something to work out.
    ///
    /// That can take a while in a large file, so the event loop draws the
    /// screen, which says so, before [`App::work`] does it.
    pub(super) fn has_work(&self) -> bool {
        self.pending().is_some()
    }

    /// Works out what the screen has yet to.
    ///
    /// It works out each in the order that `App::pending` gives.
    pub(super) fn work(&mut self) {
        while let Some(work) = self.pending() {
            self.run(work);
            // Each piece of work leaves another, or none, so the loop ends.
            debug_assert_ne!(self.pending(), Some(work), "{work:?} is still pending");
        }
    }

    /// Works out `work`.
    fn run(&mut self, work: Work) {
        match work {
            // The page on the screen knows what it waits for.
            Work::StartPage | Work::Tab(_) => {
                let view = (self.history.last_mut()).expect("the first page is never closed");
                view.work_out(&self.session);
            }
            Work::Keepers => {
                self.session.work_out_keepers();
                // The pages so far say it in their headers too.
                for view in self.history.iter_mut().chain(&mut self.forward) {
                    if let View::Item(page) = view {
                        page.add_keeping(&self.session);
                    }
                }
            }
        }
    }

    /// Does what `key` asks, and says whether the browser should stop.
    pub(super) fn key(&mut self, key: KeyEvent) -> Option<Stop> {
        // Every key acts on the page as worked out.
        self.work();
        if self.prompt.is_some() {
            return self.key_to_prompt(key);
        }
        let action = keys::action(key)?;
        self.message = None;
        match action {
            Action::Quit => return Some(Stop::Quit),
            Action::Suspend => return Some(Stop::Suspend),
            Action::GoTo => {
                let picker = Picker::new(&self.session, "");
                self.prompt = Some(Prompt::GoTo(Box::new(picker)));
            }
            Action::Find => match self.page().cursor() {
                Some(_) => self.prompt = Some(Prompt::Find(String::new())),
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
    /// screenful, so it is handled once the screen is drawn. While a
    /// prompt is open, the key's code alone decides, so that a character
    /// with Alt waits as the character does, though it types nothing.
    pub(super) fn can_wait_to_draw(&self, key: KeyEvent) -> bool {
        if self.prompt.is_some() {
            return matches!(
                key.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc
            );
        }
        matches!(keys::action(key), Some(Action::Quit | Action::Suspend))
    }

    /// Does what `key` asks of the go-to box or find's prompt, whichever
    /// is open, as [`keys::typing`] reads it. Ctrl-C and Ctrl-Z stop the
    /// browser, and Esc closes the prompt, the same in both.
    fn key_to_prompt(&mut self, key: KeyEvent) -> Option<Stop> {
        let typing = keys::typing(key)?;
        let session = &self.session;
        match (typing, self.prompt.as_mut()?) {
            (Typing::Stop(stop), _) => return Some(stop),
            (Typing::Cancel, _) => self.prompt = None,
            (Typing::Char(c), Prompt::GoTo(picker)) => picker.push(c),
            (Typing::Char(c), Prompt::Find(typed)) => typed.push(c),
            (Typing::Erase, Prompt::GoTo(picker)) => picker.pop(),
            (Typing::Erase, Prompt::Find(typed)) => {
                typed.pop();
            }
            (Typing::Move(action), Prompt::GoTo(picker)) => picker.handle(session, action),
            // Find's prompt has no rows of its own to move over.
            (Typing::Move(_), Prompt::Find(_)) => {}
            (Typing::Submit, Prompt::GoTo(picker)) => {
                if let Some(picker_row) = picker.selected(session) {
                    // The header names the item as SYMBOL's answer does,
                    // with what holds an address typed, or the other name
                    // that found it.
                    let (node, at) = (picker_row.page.node, picker_row.page.at);
                    let note = picker_row.note.as_deref();
                    let page = ItemPage::new(session, node, at, note);
                    self.prompt = None;
                    self.message = None;
                    self.open(View::Item(Box::new(page)));
                }
            }
            (Typing::Submit, Prompt::Find(typed)) => {
                let text = std::mem::take(typed);
                self.prompt = None;
                if !text.is_empty() {
                    self.find(&text, FindFrom::Here);
                    self.searched = Some(text);
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

    /// What the status line says on a screen `width` columns wide: the
    /// first of what [`StatusLine`] lists that the screen has.
    ///
    /// It may run before the page is drawn, as [`App::draw`] runs it to
    /// know how many lines a value takes. That is safe: a draw moves only
    /// which rows are on the screen, never the selected row, so the value
    /// and the preview read the same before it as after.
    fn status_line(&self, width: usize) -> StatusLine {
        if let Some(work) = self.pending() {
            return StatusLine::Working(work.message(&self.session));
        }
        match &self.prompt {
            Some(Prompt::GoTo(picker)) => {
                return StatusLine::Typed(format!("go to: {}▏", picker.text));
            }
            Some(Prompt::Find(typed)) => return StatusLine::Typed(format!("find: {typed}▏")),
            None => {}
        }
        if let Some(message) = &self.message {
            return StatusLine::Message(message.clone());
        }
        let view = (self.history.last()).expect("the first page is never closed");
        match view.value(width) {
            // Broken where each line ends, rather than at spaces, so that
            // every character shows, as the row would show it.
            Some(value) => {
                let characters: Vec<char> = value.chars().collect();
                let lines = characters.chunks(width.max(1)).map(String::from_iter);
                StatusLine::Value(lines.collect())
            }
            None => StatusLine::Preview(view.preview(&self.session).unwrap_or_default()),
        }
    }

    /// What [`App::status_line`] says, as the lines drawn on a screen
    /// `width` columns wide: one line, or a value's lines, at most `most`
    /// of them.
    fn status_lines(&self, width: usize, most: usize) -> Vec<Line<'static>> {
        match self.status_line(width) {
            StatusLine::Working(text) | StatusLine::Message(text) => {
                vec![Line::raw(cut(&text, width))]
            }
            // What is typed is cut from the left, so that its end shows.
            StatusLine::Typed(typed) => vec![Line::raw(cut_left(&typed, width))],
            StatusLine::Value(lines) => (keep_lines(lines, most, width).into_iter())
                .map(|line| Line::raw(line).dim())
                .collect(),
            StatusLine::Preview(preview) => vec![Line::raw(cut(&preview, width)).dim()],
        }
    }

    /// The title line's trail on a screen `width` columns wide: the file's
    /// name, the titles of the pages that led here, and " › go to" while
    /// the go-to box is open. Long names in it are shortened to fit after
    /// the file's name.
    fn trail(&mut self, width: usize) -> String {
        let mut trail = self.session.file_name();
        let room = width.saturating_sub(trail.chars().count() + 3).max(20);
        for view in &mut self.history {
            let title = view.title(room);
            if !title.is_empty() {
                trail += " › ";
                trail += title;
            }
        }
        match self.prompt {
            Some(Prompt::GoTo(_)) => trail += " › go to",
            None | Some(Prompt::Find(_)) => {}
        }
        trail
    }

    /// The hint line on a screen `width` columns wide: the keys of the
    /// prompt that is open, or of the page (see [`keys::hints`]).
    fn hint_line(&mut self, width: usize) -> String {
        match self.prompt {
            Some(Prompt::GoTo(_)) => keys::GO_TO_HINT.to_string(),
            Some(Prompt::Find(_)) => keys::FIND_HINT.to_string(),
            None => keys::hints(width, &self.page().page_keys()),
        }
    }

    /// Draws the screen: the title line, with the file's name and the
    /// pages that led here, the page or the go-to box, the status line,
    /// the command that prints what the page shows, if one does, and the
    /// hints. The status line takes more than one line only to show in
    /// full a value that its row cuts, such as a long string.
    pub(super) fn draw(&mut self, frame: &mut Frame) {
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
        // PAGE_LINES lines. The go-to box hides it.
        let command = match (&self.prompt, self.history.last()) {
            (None | Some(Prompt::Find(_)), Some(view)) => {
                view.command().map_or(Vec::new(), |line| wrap(line, width))
            }
            (Some(Prompt::GoTo(_)), _) | (_, None) => Vec::new(),
        };
        let most = usize::from(area.height).saturating_sub(3 + PAGE_LINES);
        let command = keep_lines(command, most.max(1), width);
        // A value that its row cuts wraps too, in the lines that the
        // command leaves.
        let status_lines =
            self.status_lines(width, (most + 1).saturating_sub(command.len()).max(1));
        let [title, body, status, command_area, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(status_lines.len() as u16),
            Constraint::Length(command.len() as u16),
            Constraint::Length(1),
        ])
        .areas(area);

        // The trail is cut from the left, so that the page on the screen
        // shows.
        let trail = self.trail(width);
        frame.render_widget(Line::raw(cut_left(&trail, width)).bold(), title);
        match &mut self.prompt {
            Some(Prompt::GoTo(picker)) => picker.draw(&self.session, frame, body),
            None | Some(Prompt::Find(_)) => self.page().draw(frame, body),
        }
        draw_lines(frame, status, status_lines);
        draw_lines(
            frame,
            command_area,
            command.into_iter().map(Line::raw).collect(),
        );
        let hint = self.hint_line(width);
        frame.render_widget(Line::raw(hint).dim(), hints);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MediaKeyCode, ModifierKeyCode};

    use super::*;
    use crate::binary::Binary;
    use crate::binary::test_samples::samples;
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
        let worked_out = |app: &App| match &app.history[..] {
            [View::Start(start)] => start.is_worked_out(),
            _ => panic!("the start page isn't the only page"),
        };
        assert!(!worked_out(&app) && !app.has_work());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.has_work());
        app.work();
        assert!(worked_out(&app) && !app.has_work());
    }

    /// One key of each kind that crossterm reads, with every function key,
    /// the printable characters of ASCII and some others, and some media
    /// and modifier keys.
    fn every_key_code() -> Vec<KeyCode> {
        let mut codes = vec![
            KeyCode::Backspace,
            KeyCode::Enter,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Null,
            KeyCode::Esc,
            KeyCode::CapsLock,
            KeyCode::ScrollLock,
            KeyCode::NumLock,
            KeyCode::PrintScreen,
            KeyCode::Pause,
            KeyCode::Menu,
            KeyCode::KeypadBegin,
        ];
        codes.extend((0..=u8::MAX).map(KeyCode::F));
        codes.extend((' '..='~').chain(['é', '→', '\t', '\0']).map(KeyCode::Char));
        codes.extend([MediaKeyCode::Play, MediaKeyCode::Stop].map(KeyCode::Media));
        let modifiers = [ModifierKeyCode::LeftControl, ModifierKeyCode::RightAlt];
        codes.extend(modifiers.map(KeyCode::Modifier));
        // A kind of key that crossterm adds must be listed above too: this
        // match names every kind.
        for &code in &codes {
            match code {
                KeyCode::Backspace
                | KeyCode::Enter
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Delete
                | KeyCode::Insert
                | KeyCode::F(_)
                | KeyCode::Char(_)
                | KeyCode::Null
                | KeyCode::Esc
                | KeyCode::CapsLock
                | KeyCode::ScrollLock
                | KeyCode::NumLock
                | KeyCode::PrintScreen
                | KeyCode::Pause
                | KeyCode::Menu
                | KeyCode::KeypadBegin
                | KeyCode::Media(_)
                | KeyCode::Modifier(_) => {}
            }
        }
        codes
    }

    #[test]
    fn keys_that_type_into_a_prompt_or_stop_can_wait_to_draw() {
        let path = samples()
            .into_iter()
            .find(|path| path.ends_with("c-x86_64.elf"));
        let path = path.unwrap();
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        let mut app = App::new(session, None);
        // Every key, with every set of modifiers.
        let modifiers = (0..=KeyModifiers::all().bits()).map(KeyModifiers::from_bits_truncate);
        let keys: Vec<KeyEvent> = (every_key_code().into_iter())
            .flat_map(|code| modifiers.clone().map(move |with| KeyEvent::new(code, with)))
            .collect();
        // Without a prompt, only the keys that quit or suspend can wait.
        for &key in &keys {
            let pressed = (key.code, key.modifiers - KeyModifiers::SHIFT);
            let quits = matches!(
                pressed,
                (KeyCode::Char('q'), KeyModifiers::NONE)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL)
            );
            let suspends = cfg!(unix) && pressed == (KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(app.can_wait_to_draw(key), quits || suspends, "{key:?}");
        }
        let prompts = [
            Prompt::GoTo(Box::new(Picker::new(&app.session, ""))),
            Prompt::Find(String::new()),
        ];
        for prompt in prompts {
            app.prompt = Some(prompt);
            for &key in &keys {
                // What a key that types, erases or cancels does doesn't
                // depend on the screen, and nor does what Ctrl-C and Ctrl-Z
                // do. A move or Enter acts on the rows that the screen
                // shows. A character with Ctrl or Alt types nothing, but
                // waits as the character does.
                let typing = keys::typing(key);
                let waits = match typing {
                    Some(Typing::Char(_) | Typing::Erase | Typing::Cancel | Typing::Stop(_)) => {
                        true
                    }
                    Some(Typing::Move(_) | Typing::Submit) => false,
                    None => matches!(key.code, KeyCode::Char(_)),
                };
                assert_eq!(app.can_wait_to_draw(key), waits, "{key:?}");
                // The six moves that the go-to box takes, and no other
                // action.
                if let Some(Typing::Move(action)) = typing {
                    let moves = [
                        Action::Up,
                        Action::Down,
                        Action::PageUp,
                        Action::PageDown,
                        Action::First,
                        Action::Last,
                    ];
                    assert!(moves.contains(&action), "{key:?}");
                }
            }
        }
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
            let says = app.pending().map(|work| work.message(&app.session));
            let says = says.as_deref() == Some("working out what keeps each item…");
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
    fn tabs_5_and_6_are_worked_out_once_the_screen_says_so() {
        let path = samples()
            .into_iter()
            .find(|path| path.ends_with("c-cortex-m7.elf"));
        let path = path.unwrap();
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        let mut app = App::new(session, Some("main"));
        app.work();
        let tabs = [
            ('5', "working out what keeps main in the file…"),
            ('6', "working out what only main leads to…"),
        ];
        for (key, says) in tabs {
            app.key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            let work = app.pending().map(|work| work.message(&app.session));
            assert_eq!(work.as_deref(), Some(says));
            app.work();
            assert!(!app.has_work(), "{key}");
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
                prompt: None,
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
                .flat_map(|node| [2, 3, 4, 5, 6].map(|tab| (node, tab)))
            {
                let name = node.name(&binary);
                let mut page = ItemPage::new(&app.session, node, None, None);
                if page.handle(&app.session, Action::Tab(tab)).is_some() {
                    // The item doesn't have the tab.
                    continue;
                }
                app.history = vec![View::Item(Box::new(page))];
                // Tabs 4, 5 and 6 are worked out once the screen says so.
                assert_eq!(app.has_work(), tab >= 4, "{name}");
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
                    let has_links = app.page().page_keys().links;
                    let header = ["HOW", "ADDRESS", "RETAINED"].into_iter().any(shown);
                    let rows =
                        !has_links || header && lines.iter().any(|line| line.starts_with("> "));
                    let screen = lines.join("\n");
                    assert!(rows, "{}, {name}, {tab}:\n{screen}", path.display());
                    assert!(shown("used by"), "{}, {name}:\n{screen}", path.display());
                }
                pages += 1;
            }
        }
        assert!(pages > 500, "{pages}");
    }
}
