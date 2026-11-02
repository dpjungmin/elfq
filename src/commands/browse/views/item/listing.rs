//! Tab 1 of an item's page, as the page draws it and moves over it: a
//! function's code, or a data object's contents, one line each, with the
//! notes and links that `line_notes.rs` writes.

use std::fmt::Write;
use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table};

use super::super::{Cursor, Go, cut, draw_lines, follow, has_ignoring_case, link_preview, wrap};
use super::Tab;
use crate::binary::ItemKind;
use crate::commands::browse::{keys::Action, session::Session};
use crate::graph::Node;

/// The most columns that the instructions take, and the fewest they give
/// way to when the notes after them need room.
const INSTRUCTION_WIDTH: (usize, usize) = (48, 24);
/// The fewest columns that a note is cut to before its section goes.
const NOTE_WIDTH: usize = 16;

/// Where a line's link goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Link {
    /// An item or import, the address in it that the line uses, and the
    /// veneer or PLT stub that the code goes through to get there.
    Node {
        node: Node,
        at: u64,
        via: Option<usize>,
    },
    /// Another line of this function.
    Here(u64),
}

/// One line of a function's code, or one row of a data object's contents.
pub(super) struct CodeLine {
    pub(super) address: u64,
    /// The instruction, with the name of the item it calls or jumps to in
    /// place of its address: `call    strlen@plt`, or for a data object,
    /// the value: a word, a vector table's entry and its word, or a string
    /// (see `contents.rs`). Empty where elfq doesn't print the code.
    pub(super) instruction: String,
    /// What the line does with another item, or where it jumps.
    pub(super) note: String,
    /// The section of the item that the link goes to, when that is in
    /// another block of memory than this function or data object, as in
    /// firmware that calls from ITCM into flash. It is drawn in yellow.
    pub(super) elsewhere: Option<String>,
    pub(super) link: Option<Link>,
}

/// Tab 1: a function's code, or a data object's contents.
pub(super) struct CodeTab {
    /// The item, by its index.
    index: usize,
    pub(super) code: Rc<PageCode>,
    pub(super) cursor: Cursor,
    /// The rows the cursor was on before each jump inside the function,
    /// for Back.
    jumps: Vec<usize>,
}

/// An item's code, or a data object's contents, as its page shows them.
/// The session keeps it, so that every page opened on the item shares it,
/// and it is worked out once.
pub struct PageCode {
    /// Whether it is a function's code or a data object's contents.
    pub(super) kind: ItemKind,
    pub(super) lines: Vec<CodeLine>,
    /// The header of the column after the addresses, when the lines show
    /// their instructions, or a data object's values: "INSTRUCTION",
    /// "VALUE", or for a vector table, "ENTRY" over the entries' names,
    /// then "VALUE".
    pub(super) column: Option<String>,
    /// The widest address and instruction or value, worked out once, so
    /// that a redraw reads only the rows on the screen.
    pub(super) widest: (usize, usize),
    /// Said in place of the code when there is none to show.
    pub(super) empty: Option<String>,
    /// Said under the code.
    pub(super) footnote: Option<String>,
    /// The command under the tab, if any: the objdump command that prints
    /// a data object's bytes.
    pub(super) command: Option<String>,
}

impl CodeTab {
    /// The code of function `index`, or the contents of data object
    /// `index`, with the line that holds `at` selected.
    pub(super) fn new(session: &Session, index: usize, at: Option<u64>) -> CodeTab {
        let code = session.page_code(index);
        let mut tab = CodeTab {
            index,
            cursor: Cursor::new(code.lines.len()),
            code,
            jumps: Vec::new(),
        };
        if let Some(at) = at {
            tab.select_address(at);
        }
        tab
    }

    /// Whether it shows a data object's contents.
    pub(super) fn is_data(&self) -> bool {
        self.code.kind == ItemKind::Data
    }

    /// Whether the page opens on it: a function's code always does, but a
    /// data object's contents only when they have rows.
    pub(super) fn opens_page(&self) -> bool {
        !self.is_data() || !self.code.lines.is_empty()
    }

    /// Selects the line that holds `address`.
    fn select_address(&mut self, address: u64) {
        let after = self
            .code
            .lines
            .partition_point(|line| line.address <= address);
        self.cursor.select(after.saturating_sub(1));
    }

    /// Draws the code, or why there is none, with the footnote under it.
    /// The footnote wraps, so that nothing is cut.
    pub(super) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let bottom: Vec<String> = self
            .code
            .footnote
            .iter()
            .flat_map(|text| wrap(text, width))
            .collect();
        let [body, foot] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(bottom.len() as u16)])
                .areas(area);
        draw_lines(frame, foot, bottom.into_iter().map(Line::raw).collect());
        if let Some(text) = &self.code.empty {
            draw_lines(
                frame,
                body,
                wrap(text, width).into_iter().map(Line::raw).collect(),
            );
            return;
        }
        self.draw_code(frame, body);
    }

    /// The lines of code, as a table: ADDRESS, INSTRUCTION and the notes,
    /// or for a data object, ADDRESS, VALUE and the notes.
    fn draw_code(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        // The table's header takes a line.
        let (visible, mut state) = self
            .cursor
            .visible(usize::from(area.height).saturating_sub(1));
        let (address_width, instruction_width) = self.columns(width);
        // Before the notes: the selection's "> ", the columns, and the
        // spaces after each.
        let mut used = 2 + address_width + 2;
        if self.code.column.is_some() {
            used += instruction_width + 2;
        }
        let room = width.saturating_sub(used);
        let rows = self.code.lines[visible].iter().map(|line| {
            let mut cells = vec![Line::raw(format!("{:#x}", line.address))];
            if self.code.column.is_some() {
                cells.push(Line::raw(cut(&line.instruction, instruction_width)));
            }
            cells.push(note_cell(line, room));
            Row::new(cells)
        });
        let mut header = vec!["ADDRESS"];
        let mut columns = vec![Constraint::Length(address_width as u16)];
        if let Some(column) = &self.code.column {
            header.push(column);
            columns.push(Constraint::Length(instruction_width as u16));
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

    /// The columns that the addresses and the instructions or values take
    /// on a screen `width` columns wide. The instructions give way to the
    /// notes, down to a point. A data object's values are short, or
    /// strings, which have no note after them (see `contents.rs`), so they
    /// keep no room for notes.
    fn columns(&self, width: usize) -> (usize, usize) {
        let (widest_address, widest_instruction) = self.code.widest;
        let address_width = widest_address.max("ADDRESS".len());
        if self.code.column.is_none() {
            return (address_width, 0);
        }
        let ((most, fewest), notes) = match self.code.kind {
            ItemKind::Function => (INSTRUCTION_WIDTH, NOTE_WIDTH),
            ItemKind::Data => ((usize::MAX, 0), 0),
        };
        let mut instruction_width = widest_instruction.min(most);
        // The selection's "> ", the columns, and the spaces after each.
        let wanted = 2 + address_width + 2 + instruction_width + 2 + notes;
        if wanted > width {
            instruction_width = instruction_width.saturating_sub(wanted - width).max(fewest);
        }
        (address_width, instruction_width)
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
                let lines = &self.code.lines;
                let next = self.cursor.next(action == Action::NextLink, |row| {
                    matches!(lines[row].link, Some(Link::Node { .. }))
                });
                if let Some(row) = next {
                    self.cursor.select(row);
                }
                None
            }
            Action::Open => match self.code.lines.get(selected)?.link? {
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

    /// Goes back to the line before the last jump inside the function, if
    /// there was one.
    pub(super) fn back(&mut self) -> bool {
        match self.jumps.pop() {
            Some(row) => {
                self.cursor.select(row);
                true
            }
            None => false,
        }
    }

    /// Where the selected line's link goes, in full: "→ strlen@plt
    /// function at 0x1080, 16 bytes, in .plt.sec".
    pub(super) fn preview(&self, session: &Session) -> Option<String> {
        let link = self.code.lines.get(self.cursor.selected)?.link?;
        Some(match link {
            Link::Node { node, via, .. } => {
                link_preview(session, node, Some(Node::Item(self.index)), via)
            }
            Link::Here(address) => format!("→ {address:#x}, in this function"),
        })
    }

    /// The selected row's value, when its row cuts it to fit a screen
    /// `width` columns wide, as it cuts a long string: the status line
    /// shows it in full.
    pub(super) fn value(&self, width: usize) -> Option<&str> {
        let line = self.code.lines.get(self.cursor.selected)?;
        let (_, room) = self.columns(width);
        (self.is_data() && line.instruction.chars().count() > room).then_some(&line.instruction)
    }

    /// Whether some line links to another item.
    pub(super) fn has_links(&self) -> bool {
        self.code
            .lines
            .iter()
            .any(|line| matches!(line.link, Some(Link::Node { .. })))
    }

    /// The lines that have `lowercase` in their address, instruction or
    /// note, or the section that the note names, ignoring case.
    pub(super) fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        let has = |text: &str| has_ignoring_case(text, lowercase);
        let mut address = String::new();
        (self.code.lines.iter().enumerate())
            .filter(|(_, line)| {
                address.clear();
                write!(address, "{:#x}", line.address).expect("writing to a string");
                has(&address)
                    || has(&line.instruction)
                    || has(&line.note)
                    || line.elsewhere.as_deref().is_some_and(has)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// A line's note, cut to `room` columns, with the section of an item in
/// another block of memory after it in yellow. When both don't fit, the
/// note keeps at least [`NOTE_WIDTH`] columns before the section goes, and
/// then the note alone is yellow.
fn note_cell(line: &CodeLine, room: usize) -> Line<'static> {
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

/// The widest address and instruction of `lines`.
pub(super) fn widest(lines: &[CodeLine]) -> (usize, usize) {
    let widest = |cell: fn(&CodeLine) -> usize| lines.iter().map(cell).max().unwrap_or(0);
    (
        widest(|line| format!("{:#x}", line.address).len()),
        widest(|line| line.instruction.chars().count()),
    )
}
