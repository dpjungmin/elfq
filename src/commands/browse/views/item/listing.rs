//! Tab 1 of an item's page: its listing, and the cursor over it.
//!
//! A [`Listing`] is what the tab lists: a function's code, which
//! `code.rs` builds, or a data object's contents, which `contents.rs`
//! builds, one line each, with the notes and links that `line_notes.rs`
//! writes. A [`ListingPane`] draws a listing, moves over its lines and
//! follows their links.

use std::fmt::Write;
use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table};

use super::super::{Cursor, Go, cut, draw_lines, follow, has_ignoring_case, link_preview, wrap};
use super::Tab;
use super::code::function_listing;
use super::contents::data_listing;
use crate::binary::ItemKind;
use crate::commands::browse::{keys::Action, session::Session};
use crate::graph::Node;

/// The most columns that the instructions take, and the fewest.
///
/// They give way to the fewest when the notes after them need room.
const INSTRUCTION_WIDTH: (usize, usize) = (48, 24);
/// The fewest columns that a note is cut to before its section goes.
const NOTE_WIDTH: usize = 16;

/// Where a line's link goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Link {
    /// An item or import, and the address in it that the line uses.
    ///
    /// `via` is the veneer or PLT stub that the code goes through to get
    /// there, if any.
    Node {
        node: Node,
        at: u64,
        via: Option<usize>,
    },
    /// Another line of this function.
    Here(u64),
}

/// One line of a function's code, or one row of a data object's contents.
pub(super) struct ListingLine {
    pub(super) address: u64,
    /// What the line holds: an instruction or a value.
    ///
    /// An instruction names the item that it calls or jumps to in place of
    /// its address: `call    strlen@plt`. A data object's value is a word,
    /// a vector table's entry and its word, or a string (see
    /// `contents.rs`). It is empty where elfq doesn't print the code.
    pub(super) text: String,
    /// What the line does with another item, or where it jumps.
    pub(super) note: String,
    /// The section of the item that the link goes to, if in another block.
    ///
    /// That is a block of memory other than this item's, as in firmware
    /// that calls from ITCM into flash. It is drawn in yellow.
    pub(super) elsewhere: Option<String>,
    pub(super) link: Option<Link>,
}

/// Tab 1's pane: an item's listing, and the cursor over its lines.
pub(super) struct ListingPane {
    /// The item, by its index.
    index: usize,
    listing: Rc<Listing>,
    cursor: Cursor,
    /// The rows the cursor was on before each jump inside the function.
    ///
    /// Back goes to the last of them.
    jumps: Vec<usize>,
}

/// What tab 1 of an item's page lists.
///
/// That is a function's code, or a data object's contents. The session
/// keeps it, so that every page opened on the item shares it, and it is
/// worked out once. [`Listing::new`] is the only way to make one, so
/// that the widest cells always match the lines.
pub struct Listing {
    /// Whether it is a function's code or a data object's contents.
    kind: ItemKind,
    lines: Vec<ListingLine>,
    /// The header of the column of the lines' text, if they have text.
    ///
    /// It is "INSTRUCTION", "VALUE", or for a vector table, "ENTRY" over
    /// the entries' names, then "VALUE".
    column: Option<String>,
    /// The widest address, worked out once.
    ///
    /// That way a redraw reads only the lines on the screen.
    widest_address: usize,
    /// The widest text, worked out once, as `widest_address` is.
    widest_text: usize,
    /// Said in place of the lines when there are none to show.
    said_instead: Option<String>,
    /// Said under the lines.
    footnote: Option<String>,
    /// The command under the tab, if any.
    ///
    /// That is the objdump command that prints a data object's bytes.
    command: Option<String>,
}

impl Listing {
    /// Item `index`'s listing, as tab 1 of its page shows it.
    pub fn of_item(session: &Session, index: usize) -> Listing {
        match session.binary.items[index].kind {
            ItemKind::Function => function_listing(session, index),
            ItemKind::Data => data_listing(session, index),
        }
    }

    /// The `lines` of an item of `kind`, with their widest cells.
    ///
    /// Their text, if they have any, is under a column headed `column`.
    pub(super) fn new(kind: ItemKind, column: Option<String>, lines: Vec<ListingLine>) -> Listing {
        let widest = |cell: fn(&ListingLine) -> usize| lines.iter().map(cell).max().unwrap_or(0);
        Listing {
            kind,
            widest_address: widest(|line| format!("{:#x}", line.address).len()),
            widest_text: widest(|line| line.text.chars().count()),
            lines,
            column,
            said_instead: None,
            footnote: None,
            command: None,
        }
    }

    /// The same, saying `why` in place of the lines, which it has none of.
    pub(super) fn saying_instead(mut self, why: String) -> Listing {
        self.said_instead = Some(why);
        self
    }

    /// The same, with `footnote` under the lines.
    pub(super) fn with_footnote(mut self, footnote: String) -> Listing {
        self.footnote = Some(footnote);
        self
    }

    /// The same, with `command` under the tab.
    pub(super) fn with_command(mut self, command: String) -> Listing {
        self.command = Some(command);
        self
    }
}

/// What the tests of `contents.rs` read of a listing.
#[cfg(test)]
impl Listing {
    pub(super) fn lines(&self) -> &[ListingLine] {
        &self.lines
    }

    pub(super) fn column(&self) -> Option<&str> {
        self.column.as_deref()
    }

    pub(super) fn said_instead(&self) -> Option<&str> {
        self.said_instead.as_deref()
    }

    pub(super) fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }
}

impl ListingPane {
    /// The listing of item `index`, with the line that holds `at` selected.
    pub(super) fn new(session: &Session, index: usize, at: Option<u64>) -> ListingPane {
        let listing = session.listing(index);
        let mut pane = ListingPane {
            index,
            cursor: Cursor::new(listing.lines.len()),
            listing,
            jumps: Vec::new(),
        };
        if let Some(at) = at {
            pane.select_address(at);
        }
        pane
    }

    /// Whether it shows a data object's contents.
    pub(super) fn is_data(&self) -> bool {
        self.listing.kind == ItemKind::Data
    }

    /// The cursor over the lines.
    pub(super) fn cursor(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// The command under the tab, if any.
    pub(super) fn command(&self) -> Option<&str> {
        self.listing.command.as_deref()
    }

    /// Whether the page opens on it.
    ///
    /// A function's code always does, but a data object's contents only
    /// when they have rows.
    pub(super) fn opens_page(&self) -> bool {
        !self.is_data() || !self.listing.lines.is_empty()
    }

    /// Selects the line that holds `address`.
    fn select_address(&mut self, address: u64) {
        let after = self
            .listing
            .lines
            .partition_point(|line| line.address <= address);
        self.cursor.select(after.saturating_sub(1));
    }

    /// Draws the lines, or why there are none, with the footnote under it.
    ///
    /// The footnote wraps, so that nothing is cut.
    pub(super) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let bottom: Vec<String> = self
            .listing
            .footnote
            .iter()
            .flat_map(|text| wrap(text, width))
            .collect();
        let [body, foot] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(bottom.len() as u16)])
                .areas(area);
        draw_lines(frame, foot, bottom.into_iter().map(Line::raw).collect());
        if let Some(text) = &self.listing.said_instead {
            draw_lines(
                frame,
                body,
                wrap(text, width).into_iter().map(Line::raw).collect(),
            );
            return;
        }
        self.draw_table(frame, body);
    }

    /// Draws the lines, as a table.
    ///
    /// Its columns are ADDRESS, INSTRUCTION and the notes, or for a data
    /// object, ADDRESS, VALUE and the notes.
    fn draw_table(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        // The table's header takes a line.
        let (visible, mut state) = self
            .cursor
            .visible(usize::from(area.height).saturating_sub(1));
        let (address_width, text_width) = self.columns(width);
        // Before the notes: the selection's "> ", the columns, and the
        // spaces after each.
        let mut used = 2 + address_width + 2;
        if self.listing.column.is_some() {
            used += text_width + 2;
        }
        let room = width.saturating_sub(used);
        let rows = self.listing.lines[visible].iter().map(|line| {
            let mut cells = vec![Line::raw(format!("{:#x}", line.address))];
            if self.listing.column.is_some() {
                cells.push(Line::raw(cut(&line.text, text_width)));
            }
            cells.push(note_cell(line, room));
            Row::new(cells)
        });
        let mut header = vec!["ADDRESS"];
        let mut columns = vec![Constraint::Length(address_width as u16)];
        if let Some(column) = &self.listing.column {
            header.push(column);
            columns.push(Constraint::Length(text_width as u16));
        }
        header.push("");
        columns.push(Constraint::Fill(1));
        let table = Table::new(rows, columns)
            .column_spacing(2)
            .header(Row::new(header).bold())
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The widths of the address and text columns, on a screen `width` wide.
    ///
    /// The instructions give way to the notes, down to a point. A data
    /// object's values are short, or strings, which have no note after them
    /// (see `contents.rs`), so they keep no room for notes.
    fn columns(&self, width: usize) -> (usize, usize) {
        let address_width = self.listing.widest_address.max("ADDRESS".len());
        if self.listing.column.is_none() {
            return (address_width, 0);
        }
        let ((most, fewest), notes) = match self.listing.kind {
            ItemKind::Function => (INSTRUCTION_WIDTH, NOTE_WIDTH),
            ItemKind::Data => ((usize::MAX, 0), 0),
        };
        let mut text_width = self.listing.widest_text.min(most);
        // The selection's "> ", the columns, and the spaces after each.
        let wanted = 2 + address_width + 2 + text_width + 2 + notes;
        if wanted > width {
            text_width = text_width.saturating_sub(wanted - width).max(fewest);
        }
        (address_width, text_width)
    }

    pub(super) fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if self.cursor.handle(action) {
            return None;
        }
        let selected = self.cursor.selected;
        match action {
            Action::NextLink | Action::PreviousLink => {
                // Jumps inside the function don't count: Tab goes from one
                // use of another item to the next.
                let lines = &self.listing.lines;
                let next = self.cursor.next(action == Action::NextLink, |row| {
                    matches!(lines[row].link, Some(Link::Node { .. }))
                });
                if let Some(row) = next {
                    self.cursor.select(row);
                }
                None
            }
            Action::Open => match self.listing.lines.get(selected)?.link? {
                Link::Node { node, at, .. } => {
                    let this = Some(Node::Item(self.index));
                    Some(follow(session, node, Some(at), this, Tab::Code))
                }
                // A jump inside the function moves the cursor, and Back
                // brings it back.
                Link::Here(address) => {
                    self.jumps.push(selected);
                    self.select_address(address);
                    None
                }
            },
            _ => None,
        }
    }

    /// Goes back to the line before the last jump inside the function.
    ///
    /// It says whether it did: it does only if there was a jump.
    pub(super) fn back(&mut self) -> bool {
        match self.jumps.pop() {
            Some(row) => {
                self.cursor.select(row);
                true
            }
            None => false,
        }
    }

    /// Where the selected line's link goes, in full.
    ///
    /// An example: "→ strlen@plt function at 0x1080, 16 bytes, in
    /// .plt.sec".
    pub(super) fn preview(&self, session: &Session) -> Option<String> {
        let link = self.listing.lines.get(self.cursor.selected)?.link?;
        Some(match link {
            Link::Node { node, via, .. } => {
                link_preview(session, node, Some(Node::Item(self.index)), via)
            }
            Link::Here(address) => format!("→ {address:#x}, in this function"),
        })
    }

    /// The selected row's value, when its row cuts it.
    ///
    /// That happens to a long string, on a screen `width` columns wide. The
    /// status line shows it in full.
    pub(super) fn value(&self, width: usize) -> Option<&str> {
        let line = self.listing.lines.get(self.cursor.selected)?;
        let (_, room) = self.columns(width);
        (self.is_data() && line.text.chars().count() > room).then_some(&line.text)
    }

    /// Whether some line links to another item.
    pub(super) fn has_links(&self) -> bool {
        self.listing
            .lines
            .iter()
            .any(|line| matches!(line.link, Some(Link::Node { .. })))
    }

    /// The lines that have `lowercase`, ignoring case.
    ///
    /// It looks in their address, text and note, and the section that the
    /// note names.
    pub(super) fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        let has = |text: &str| has_ignoring_case(text, lowercase);
        let mut address = String::new();
        (self.listing.lines.iter().enumerate())
            .filter(|(_, line)| {
                address.clear();
                write!(address, "{:#x}", line.address).expect("writing to a string");
                has(&address)
                    || has(&line.text)
                    || has(&line.note)
                    || line.elsewhere.as_deref().is_some_and(has)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// A line's note, cut to `room` columns.
///
/// The section of an item in another block of memory follows it in
/// yellow. When both don't fit, the note keeps at least [`NOTE_WIDTH`]
/// columns before the section goes, and then the note alone is yellow.
fn note_cell(line: &ListingLine, room: usize) -> Line<'static> {
    let yellow = Style::new().fg(Color::Yellow);
    let Some(section) = &line.elsewhere else {
        return Line::raw(cut(&line.note, room));
    };
    let place = format!(" in {section}");
    let left = room.saturating_sub(place.chars().count());
    if line.note.chars().count() <= left || left >= NOTE_WIDTH {
        Line::from(vec![
            Span::raw(cut(&line.note, left)),
            Span::styled(place, yellow),
        ])
    } else {
        Line::styled(cut(&line.note, room), yellow)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::samples;
    use super::*;
    use crate::binary::Binary;

    #[test]
    fn the_status_line_names_what_enter_opens() {
        // Lines that link to the function they are in, as a recursive
        // call does, and to an import through its own PLT stub.
        let (mut to_itself, mut to_own_import) = (0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = &Session::new(&path, &binary).unwrap();
            let functions = (0..binary.items.len())
                .filter(|&index| binary.items[index].kind == ItemKind::Function);
            for index in functions {
                let mut pane = ListingPane::new(session, index, None);
                for row in 0..pane.listing.lines.len() {
                    let Some(Link::Node { node, via, .. }) = pane.listing.lines[row].link else {
                        continue;
                    };
                    pane.cursor.select(row);
                    let preview = pane.preview(session).unwrap();
                    let opened = session.page_node(node);
                    let name = match opened {
                        _ if matches!(node, Node::Import(_)) && opened == Node::Item(index) => {
                            to_own_import += 1;
                            format!("{}  imported", node.name(&binary))
                        }
                        Node::Item(index) => format!("{}  ", binary.items[index].name),
                        Node::Import(_) => format!("{}  imported", node.name(&binary)),
                    };
                    to_itself += usize::from(node == Node::Item(index) && via.is_none());
                    assert!(preview.starts_with(&format!("→ {name}")), "{preview}");
                }
            }
        }
        assert!(
            to_itself > 50 && to_own_import > 30,
            "{to_itself} {to_own_import}"
        );
    }
}
