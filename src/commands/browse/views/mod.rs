//! The pages, one file each. [`View`] lists them, so that `app.rs` can
//! keep any of them in its history, and each `match` on it shows every
//! page's part.

mod item;

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use super::keys::Action;
use super::session::Session;
use crate::graph::Node;

pub use item::ItemPage;

/// Where an action takes the user.
pub enum Go {
    /// Opens a page after this one.
    Open(View),
    /// Something to say on the status line.
    Say(String),
}

/// One page of the browser.
pub enum View {
    /// A function's code, or what a data object is.
    Item(Box<ItemPage>),
}

impl View {
    /// Its name in the title line's trail.
    pub fn title(&self) -> &str {
        match self {
            View::Item(page) => &page.title,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            View::Item(page) => page.draw(frame, area),
        }
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        match self {
            View::Item(page) => page.handle(session, action),
        }
    }

    /// Goes back within the page, as from a jump inside a function, when
    /// there is somewhere to go back to.
    pub fn back(&mut self) -> bool {
        match self {
            View::Item(page) => page.back(),
        }
    }

    /// A line about the selected row, such as what its link goes to.
    pub fn preview(&self, session: &Session) -> Option<String> {
        match self {
            View::Item(page) => page.preview(session),
        }
    }

    /// Whether the page has lines that link, which Tab moves between.
    pub fn has_links(&self) -> bool {
        match self {
            View::Item(page) => page.has_links(),
        }
    }
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
    len: usize,
}

impl Cursor {
    pub fn new(len: usize) -> Cursor {
        Cursor {
            selected: 0,
            offset: 0,
            page: 1,
            len,
        }
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

/// Says that `node`, an import, has no page: its code is in a shared
/// library.
pub fn imported(session: &Session, node: Node) -> String {
    format!(
        "{} is imported from a shared library: its code isn't in this file",
        node.name(session.binary)
    )
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
}
