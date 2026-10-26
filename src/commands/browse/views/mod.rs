//! The pages, one file each, the tabs of an item's page other than its
//! code, the go-to box, and how long names are shortened to fit a column
//! (`names.rs`). [`View`] lists the pages, so that `app.rs` can keep any
//! of them in its history, and each `match` on it shows every page's part.
//! A page or tab that shows a command's answer, a table whose rows open
//! items, is a `Printed` (`printed.rs`).

mod callees;
mod callers;
mod help;
mod item;
mod names;
mod picker;
mod printed;
mod reach;
mod retained;
mod start;
#[cfg(test)]
mod thumb_text;
mod why;

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::TableState;

use super::keys::Action;
use super::session::Session;
use crate::binary::{Item, ItemKind};
use crate::commands::{item_in_file, text};
use crate::graph::Node;
use item::Tab;

pub use help::Help;
pub use item::{ItemPage, PageCode};
pub use names::{FittedRows, fit_name, name_fitter};
pub use picker::{Picker, first_line};
pub use printed::Printed;
pub use start::page as start_page;

/// Where an action takes the user.
pub enum Go {
    /// Opens a page after this one.
    Open(View),
    /// Something to say on the status line.
    Say(String),
}

/// One page of the browser.
pub enum View {
    /// Where the program starts, which opens when browse is given no
    /// SYMBOL, once it is worked out (see `App::work`).
    Start(Option<Box<Printed>>),
    /// An item's page: a function's code, and what uses the item.
    Item(Box<ItemPage>),
    /// Every key, and the words that the notes use.
    Help(Help),
}

impl View {
    /// Its name in the title line's trail, shortened to fit `room`
    /// columns: none for the start page, since the trail starts with the
    /// file's name.
    pub fn title(&mut self, room: usize) -> &str {
        match self {
            View::Start(_) => "",
            View::Item(page) => page.title(room),
            View::Help(_) => "help",
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            View::Start(Some(start)) => start.draw(frame, area),
            // Nothing yet, while the status line says what is worked out.
            View::Start(None) => {}
            View::Item(page) => page.draw(frame, area),
            View::Help(help) => help.draw(frame, area),
        }
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        match self {
            // A start's page has the tabs, which the start page hasn't.
            View::Start(Some(start)) if matches!(action, Action::Tab(_)) => Some(Go::Say(
                match start.has_links() {
                    true => "Enter opens the selected start's page, which has the tabs",
                    false => "the tabs are on an item's page; g goes to one",
                }
                .to_string(),
            )),
            View::Start(_) if action == Action::Keeper => Some(Go::Say(
                "u opens the item that keeps an item, from that item's page".to_string(),
            )),
            View::Start(start) => start.as_mut()?.handle(session, action),
            View::Item(page) => page.handle(session, action),
            View::Help(help) => {
                help.handle(action);
                None
            }
        }
    }

    /// Goes back within the page, as from a jump inside a function, when
    /// there is somewhere to go back to.
    pub fn back(&mut self) -> bool {
        match self {
            View::Item(page) => page.back(),
            View::Start(_) | View::Help(_) => false,
        }
    }

    /// A line about the selected row, such as what its link goes to.
    pub fn preview(&self, session: &Session) -> Option<String> {
        match self {
            View::Start(start) => start.as_ref()?.preview(session),
            View::Item(page) => page.preview(session),
            View::Help(_) => None,
        }
    }

    /// The selected row's value, when its row cuts it to fit a screen
    /// `width` columns wide, as a long string in a data object's contents.
    pub fn value(&self, width: usize) -> Option<&str> {
        match self {
            View::Item(page) => page.value(width),
            View::Start(_) | View::Help(_) => None,
        }
    }

    /// Whether the page has lines that link, which Tab moves between.
    pub fn has_links(&self) -> bool {
        match self {
            View::Start(start) => start.as_ref().is_some_and(|start| start.has_links()),
            View::Item(page) => page.has_links(),
            View::Help(_) => false,
        }
    }

    /// The numbers of the page's tabs, whose keys show them.
    pub fn tabs(&self) -> Vec<u8> {
        match self {
            View::Item(page) => page.tab_numbers(),
            View::Start(_) | View::Help(_) => Vec::new(),
        }
    }

    /// The command that prints what the page shows, ready to paste, if
    /// one does.
    pub fn command(&self) -> Option<&str> {
        match self {
            View::Start(start) => start.as_ref().map(|start| start.command()),
            View::Item(page) => page.command(),
            View::Help(_) => None,
        }
    }

    /// The cursor over the page's rows, which find moves: the lines of a
    /// function's code, or the rows of a table. The help has none, and
    /// nor does a table without rows.
    pub fn cursor(&mut self) -> Option<&mut Cursor> {
        match self {
            View::Start(start) => Some(start.as_mut()?.cursor()),
            View::Item(page) => page.cursor(),
            View::Help(_) => None,
        }
        .filter(|cursor| !cursor.is_empty())
    }

    /// What find calls the page's rows: "line" on a function's code, and
    /// "row" in a table.
    pub fn row_noun(&self) -> &'static str {
        match self {
            View::Item(page) => page.row_noun(),
            View::Start(_) | View::Help(_) => "row",
        }
    }

    /// The rows that have `lowercase`, text in lower case, ignoring case,
    /// in order. A row's columns are looked in one at a time, as the page
    /// shows them, but with names in full.
    pub fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        match self {
            View::Start(start) => {
                (start.as_ref()).map_or_else(Vec::new, |start| start.rows_with(lowercase))
            }
            View::Item(page) => page.rows_with(lowercase),
            View::Help(_) => Vec::new(),
        }
    }
}

/// Whether `text` has `lowercase`, text in lower case, ignoring case. Text
/// in ASCII, as most is, is compared where it is, without making a copy in
/// lower case, since find looks through every line of a function.
pub fn has_ignoring_case(text: &str, lowercase: &str) -> bool {
    if text.is_ascii() && lowercase.is_ascii() {
        let (text, wanted) = (text.as_bytes(), lowercase.as_bytes());
        return wanted.is_empty()
            || (text.windows(wanted.len())).any(|window| window.eq_ignore_ascii_case(wanted));
    }
    text.to_lowercase().contains(lowercase)
}

/// Which row of a list is selected, and which rows are on the screen. A
/// list draws only those rows, so that a function of 100,000 lines draws
/// as fast as one of ten.
pub struct Cursor {
    pub selected: usize,
    /// The first row on the screen.
    offset: usize,
    /// How many rows the screen held when last drawn.
    page: usize,
    /// Whether the rows have been drawn, so that `page` is known.
    drawn: bool,
    len: usize,
}

impl Cursor {
    pub fn new(len: usize) -> Cursor {
        Cursor {
            selected: 0,
            offset: 0,
            page: 1,
            drawn: false,
            len,
        }
    }

    /// Whether there are no rows to move over.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Selects `row`. When it is off the screen, it comes to a third of
    /// the way down, so that what leads to it shows too.
    pub fn select(&mut self, row: usize) {
        self.selected = row.min(self.len.saturating_sub(1));
        if !(self.offset..self.offset + self.page).contains(&self.selected) {
            self.offset = self.selected.saturating_sub(self.page / 3);
        }
    }

    /// Moves as `action` says, and says whether it was a move. A page up
    /// or down moves the rows on the screen by a page too, as in less, so
    /// that the selected row keeps its place on the screen.
    pub fn handle(&mut self, action: Action) -> bool {
        let last = self.len.saturating_sub(1);
        let at = self.selected;
        self.selected = match action {
            Action::Up => at.saturating_sub(1),
            Action::Down => (at + 1).min(last),
            Action::PageUp => {
                self.offset = self.offset.saturating_sub(self.page);
                at.saturating_sub(self.page)
            }
            Action::PageDown => {
                self.offset = (self.offset + self.page).min(self.len.saturating_sub(self.page));
                (at + self.page).min(last)
            }
            Action::First => 0,
            Action::Last => last,
            _ => return false,
        };
        true
    }

    /// The rows that fit in `height` lines, scrolled so that the selected
    /// one shows, and the table's state for drawing them.
    pub fn visible(&mut self, height: usize) -> (Range<usize>, TableState) {
        self.page = height.max(1);
        // A row selected before the first draw, as when a page opens at an
        // address, comes a third of the way down too, now that the height
        // is known.
        if !self.drawn {
            self.drawn = true;
            self.offset = self.selected.saturating_sub(self.page / 3);
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + self.page {
            self.offset = self.selected + 1 - self.page;
        }
        // No empty lines at the end while rows above are hidden.
        self.offset = self.offset.min(self.len.saturating_sub(self.page));
        let end = (self.offset + self.page).min(self.len);
        let state =
            TableState::new().with_selected((self.len > 0).then(|| self.selected - self.offset));
        (self.offset..end, state)
    }

    /// The first row after the selected one, or before it, for which
    /// `wanted` holds, going round the end.
    pub fn next(&self, forward: bool, wanted: impl Fn(usize) -> bool) -> Option<usize> {
        let n = self.len;
        (1..=n)
            .map(|step| {
                if forward {
                    (self.selected + step) % n
                } else {
                    (self.selected + n - step) % n
                }
            })
            .find(|&row| wanted(row))
    }
}

/// What following a link to `node` does, from the page of `this`, if
/// any, as a tab that opens pages on `tab` follows it: it opens the page
/// that stands for `node` (see [`Session::page_node`]), with the line that
/// holds `at` selected when that is `node`'s own page, on `tab` when the
/// page has that tab. But on the page of an import's PLT stub, a link to
/// the import has nowhere else to go, and says that its code isn't here.
pub fn follow(session: &Session, node: Node, at: Option<u64>, this: Option<Node>, tab: Tab) -> Go {
    let opened = session.page_node(node);
    if is_own_import(node, opened, this) {
        return Go::Say(imported(session, node));
    }
    let at = at.filter(|_| opened == node);
    let page = ItemPage::new(session, opened, at, opened.name(session.binary)).on(session, tab);
    Go::Open(View::Item(Box::new(page)))
}

/// What following a link to `node` opens, from the page of `this`, in
/// full: "→ app_tick  function at 0x300002e4, 60 bytes, in .text", then
/// the veneer that the code goes through to get there, if any.
pub fn link_preview(
    session: &Session,
    node: Node,
    this: Option<Node>,
    through: Option<usize>,
) -> String {
    let binary = session.binary;
    let opened = session.page_node(node);
    match opened {
        Node::Item(index) if !is_own_import(node, opened, this) => {
            let through = match through {
                Some(veneer) => format!(", through {}", binary.items[veneer].name),
                None => String::new(),
            };
            let item = &binary.items[index];
            format!(
                "→ {}  {}{through}",
                item.name,
                whereabouts(item, &binary.sections[item.section].name)
            )
        }
        _ => format!("→ {}  imported from a shared library", node.name(binary)),
    }
}

/// Whether `node` is an import whose page, `opened`, is `this`: its PLT
/// stub's page, which has nowhere else to go for it.
fn is_own_import(node: Node, opened: Node, this: Option<Node>) -> bool {
    opened != node && Some(opened) == this
}

/// Says that `node`, an import, has no code here, as the commands say
/// it: its code is in a shared library.
pub fn imported(session: &Session, node: Node) -> String {
    let error = item_in_file(session.binary, node).expect_err("an import has no item");
    error.to_string()
}

/// What and where an item is: "function at 0x1080, 16 bytes, in .plt.sec".
pub fn whereabouts(item: &Item, section: &str) -> String {
    let kind = match item.kind {
        ItemKind::Function => "function",
        ItemKind::Data => "data object",
    };
    let size = match (item.size, item.size_estimated) {
        (0, _) => "size unknown".to_string(),
        (size, true) => format!("~{}", text::bytes_text(size)),
        (size, false) => text::bytes_text(size),
    };
    format!("{kind} at {:#x}, {size}, in {section}", item.address)
}

/// Draws `lines` one under the other in `area`, as many as fit.
pub fn draw_lines(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    for (row, line) in (area.y..area.bottom()).zip(lines) {
        frame.render_widget(
            line,
            Rect {
                y: row,
                height: 1,
                ..area
            },
        );
    }
}

/// `text` cut to `width` characters, ending in "…" when cut.
pub fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// At most `most` of `lines`, a text wrapped to `width`: when some are
/// left out, the last line kept ends in "…".
pub fn keep_lines(mut lines: Vec<String>, most: usize, width: usize) -> Vec<String> {
    if lines.len() > most {
        let rest = lines.split_off(most.saturating_sub(1)).join(" ");
        if most > 0 {
            lines.push(cut(&rest, width));
        }
    }
    lines
}

/// `text` cut to `width` characters from the left, starting with "…".
pub fn cut_left(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - width).collect();
    format!("…{kept}")
}

/// `text` broken into lines of at most `width` characters, at spaces
/// where it can be.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        // A word longer than the line is broken where the line ends.
        while line.chars().count() > width {
            lines.push(line.chars().take(width).collect());
            line = line.chars().skip(width).collect();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Every sample in testdata/bin, for the tests that check every page.
#[cfg(test)]
pub(super) fn samples() -> Vec<std::path::PathBuf> {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin");
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "elf" || e == "so"))
        .collect();
    paths.sort();
    paths
}

/// `line` split into words as a shell splits it, for the words that
/// `text::shell_quote` writes: plain, or in single quotes, with `'\''`
/// for a quote. The tests of each tab split the command under it.
#[cfg(test)]
pub(super) fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_and_wrapped_to_fit() {
        assert_eq!(cut("process_frame", 8), "process…");
        assert_eq!(cut("main", 8), "main");
        assert_eq!(cut_left("c-x86_64.elf › log_msg", 10), "…› log_msg");
        assert_eq!(
            wrap("process_frame  function at 0x30000320, 376 bytes", 30),
            ["process_frame  function at", "0x30000320, 376 bytes"]
        );
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
    }

    #[test]
    fn the_selected_row_stays_on_the_screen() {
        let mut cursor = Cursor::new(100);
        cursor.visible(10);
        // Rows on the screen are selected where they are.
        cursor.select(5);
        assert_eq!(cursor.visible(10).0, 0..10);
        // Others come a third of the way down.
        cursor.select(50);
        assert_eq!(cursor.visible(10).0, 47..57);
        cursor.handle(Action::Last);
        assert_eq!(cursor.visible(10).0, 90..100);
        assert_eq!(cursor.next(true, |row| row % 7 == 0), Some(0));
        assert_eq!(cursor.next(false, |row| row % 7 == 0), Some(98));
        // So does a row selected before the screen is drawn.
        let mut cursor = Cursor::new(100);
        cursor.select(50);
        assert_eq!(cursor.visible(10).0, 47..57);
    }

    #[test]
    fn a_page_down_or_up_moves_the_screen_by_a_page() {
        let mut cursor = Cursor::new(100);
        cursor.visible(10);
        cursor.handle(Action::PageDown);
        assert_eq!(cursor.visible(10).0, 10..20);
        assert_eq!(cursor.selected, 10);
        cursor.select(13);
        cursor.handle(Action::PageDown);
        assert_eq!((cursor.visible(10).0, cursor.selected), (20..30, 23));
        cursor.handle(Action::PageUp);
        assert_eq!((cursor.visible(10).0, cursor.selected), (10..20, 13));
        // At the end, the last page shows, and the last row is selected.
        cursor.select(95);
        cursor.handle(Action::PageDown);
        assert_eq!((cursor.visible(10).0, cursor.selected), (90..100, 99));
        // A list shorter than the screen doesn't scroll.
        let mut short = Cursor::new(4);
        short.visible(10);
        short.handle(Action::PageDown);
        assert_eq!((short.visible(10).0, short.selected), (0..4, 3));
    }

    #[test]
    fn shell_words_undo_shell_quote() {
        for text in [
            "log_msg",
            "log(unsigned int)",
            "vtable for Gain",
            "<&'a T>",
            "",
        ] {
            let line = format!("elfq {} --all", text::shell_quote(text));
            assert_eq!(shell_words(&line), ["elfq", text, "--all"], "{line}");
        }
    }
}
