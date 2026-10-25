//! The go-to box: the functions and data objects whose names have what the
//! user types, listed as they type. First the item that holds a typed
//! address, then what the text finds as SYMBOL (a name, or the end of
//! one), then every other item with a name that has the text, ignoring
//! case, largest first.
//!
//! It isn't a page: it opens over the page on the screen, and Esc closes
//! it, so that what is typed into it can't open pages by itself.

use std::cmp::Reverse;
use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{HighlightSpacing, Row, Table};

use super::{Cursor, cut, imported, wrap};
use crate::binary::{ItemKind, hex_number};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::symbol::Needle;
use crate::commands::text;
use crate::graph::Node;

/// The most rows that the box lists.
const MOST: usize = 500;
/// The most lines that the CLI's message over the rows takes.
const NOTE_LINES: usize = 3;

/// One row of the box.
pub struct Found {
    /// The item, by its index.
    pub index: usize,
    /// The address typed, when the item holds it.
    pub at: Option<u64>,
    /// For the item that the text finds as SYMBOL, its name as the CLI's
    /// answer gives it: "Default_Handler (also named NMI_Handler)".
    pub lead: Option<String>,
}

pub struct Picker {
    /// What the user has typed.
    pub text: String,
    /// The items whose names have it.
    found: Vec<Found>,
    /// How many more items have it than the box lists.
    more: usize,
    /// Why no item has it, in the CLI's words, when none does.
    nothing: Option<String>,
    cursor: Cursor,
    /// What the commands said about SYMBOL, with the SYMBOL, shown until
    /// the text changes.
    note: Option<(String, String)>,
    /// Whether the text has changed since the items were listed. They are
    /// listed again only when they are needed, so that keys typed all at
    /// once, as a paste types them, list them once.
    changed: bool,
}

impl Picker {
    /// The box with `text` typed in.
    pub fn new(session: &Session, text: &str) -> Picker {
        let mut picker = Picker {
            text: text.to_string(),
            found: Vec::new(),
            more: 0,
            nothing: None,
            cursor: Cursor::new(0),
            note: None,
            changed: false,
        };
        picker.find(session);
        picker
    }

    /// The box with `text` typed in, and `note` over the rows until the
    /// text changes, such as "`log` could mean 2 functions".
    pub fn noted(session: &Session, text: &str, note: String) -> Picker {
        let mut picker = Picker::new(session, text);
        picker.note = Some((text.to_string(), note));
        picker
    }

    /// Types `c`, or with `None`, takes the last character back.
    pub fn type_char(&mut self, c: Option<char>) {
        match c {
            Some(c) => self.text.push(c),
            None => {
                self.text.pop();
            }
        }
        self.changed = true;
    }

    /// Lists the items for the text typed, if it has changed since they
    /// were listed.
    fn catch_up(&mut self, session: &Session) {
        if self.changed {
            self.changed = false;
            self.find(session);
        }
    }

    /// Lists the items for the text typed.
    fn find(&mut self, session: &Session) {
        let binary = session.binary;
        let text = self.text.trim();
        let mut found: Vec<usize> = Vec::new();
        let mut listed: HashSet<usize> = HashSet::new();
        let mut at = None;
        self.more = 0;
        self.nothing = None;
        if !text.is_empty() {
            if let Some(Ok(address)) = hex_number(text)
                && let Some(index) = binary.item_at(address)
            {
                found.push(index);
                listed.insert(index);
                at = Some((index, address));
            }
            // Imports have no page yet.
            for &node in session.names.nodes(text) {
                if let Node::Item(index) = node
                    && listed.insert(index)
                {
                    found.push(index);
                }
            }
            let lower = text.to_lowercase();
            let needle = Needle::new(&lower);
            let mut others: Vec<usize> = (0..binary.items.len())
                .filter(|index| !listed.contains(index))
                .filter(|&index| session.names.name_has(index, &needle))
                .collect();
            self.more = (found.len() + others.len()).saturating_sub(MOST);
            // Largest first, and of the same size, first in the file. Only
            // the rows listed are sorted, which saves time when the text
            // is in most names.
            let order = |&index: &usize| (Reverse(binary.items[index].size), index);
            let room = MOST.saturating_sub(found.len());
            if others.len() > room {
                if room > 0 {
                    others.select_nth_unstable_by_key(room - 1, order);
                }
                others.truncate(room);
            }
            others.sort_unstable_by_key(order);
            found.extend(others);
            found.truncate(MOST);
        }
        // What SYMBOL finds is named as the CLI's answer names it.
        let symbol = (!text.is_empty())
            .then(|| session.names.find_one(text))
            .flatten();
        let lead = symbol.as_ref().and_then(|symbol| match symbol.node {
            Node::Item(index) => Some((index, symbol.lead_name(binary))),
            Node::Import(_) => None,
        });
        // When nothing is found, the CLI's error says why. Working that
        // out takes a while in a large file, so it is done only then.
        if found.is_empty() && !text.is_empty() {
            self.nothing = match symbol {
                Some(symbol) => {
                    matches!(symbol.node, Node::Import(_)).then(|| imported(session, symbol.node))
                }
                None => session
                    .names
                    .find(text)
                    .err()
                    .map(|error| first_line(&error)),
            };
        }
        self.found = found
            .into_iter()
            .map(|index| Found {
                index,
                at: at.filter(|&(held, _)| held == index).map(|(_, at)| at),
                lead: lead
                    .as_ref()
                    .filter(|&&(named, _)| named == index)
                    .map(|(_, lead)| lead.clone()),
            })
            .collect();
        self.cursor = Cursor::new(self.found.len());
    }

    pub fn handle(&mut self, session: &Session, action: Action) {
        self.catch_up(session);
        self.cursor.handle(action);
    }

    /// The selected row.
    pub fn selected(&mut self, session: &Session) -> Option<&Found> {
        self.catch_up(session);
        self.found.get(self.cursor.selected)
    }

    pub fn draw(&mut self, session: &Session, frame: &mut Frame, area: Rect) {
        self.catch_up(session);
        let binary = session.binary;
        let width = usize::from(area.width);
        let note = (self.note.as_ref())
            .filter(|_| self.note_shown())
            .map(|(_, note)| lines(note, width));
        let [note_area, area] = Layout::vertical([
            Constraint::Length(note.as_ref().map_or(0, |note| note.len() as u16)),
            Constraint::Fill(1),
        ])
        .areas(area);
        if let Some(note) = note {
            frame.render_widget(Text::from(note), note_area);
        }
        if self.text.trim().is_empty() {
            let prompt = "type a name, the end of a name, or an address";
            frame.render_widget(Line::raw(prompt).dim(), area);
            return;
        }

        // The columns are as wide as the rows found need, so that they
        // don't move while the cursor does.
        let rows: Vec<[String; 5]> = self
            .found
            .iter()
            .map(|found| {
                let item = &binary.items[found.index];
                let kind = match item.kind {
                    ItemKind::Function => "function",
                    ItemKind::Data => "data",
                };
                [
                    format!("{:#x}", item.address),
                    text::size_text(item),
                    kind.to_string(),
                    binary.sections[item.section].name.clone(),
                    found.lead.clone().unwrap_or_else(|| item.name.clone()),
                ]
            })
            .collect();
        let header = ["ADDRESS", "SIZE", "KIND", "SECTION", "ITEM"];
        let widths: Vec<usize> = (0..4)
            .map(|column| {
                rows.iter()
                    .map(|row| row[column].chars().count())
                    .chain([header[column].len()])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        // What is left for ITEM, after the cursor's "> " and two spaces
        // after each column.
        let room = width.saturating_sub(2 + widths.iter().map(|width| width + 2).sum::<usize>());

        // Under the rows, how many more there are, or under the header,
        // why there are none, unless the note over them says so.
        let below = if self.more > 0 {
            vec![Line::raw(format!("and {} more; type more of the name", self.more)).dim()]
        } else {
            match &self.nothing {
                Some(nothing) if !self.note_shown() => lines(nothing, width),
                _ => Vec::new(),
            }
        };
        let [area, below_area] = if self.found.is_empty() {
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area)
        } else {
            Layout::vertical([Constraint::Fill(1), Constraint::Length(below.len() as u16)])
                .areas(area)
        };
        frame.render_widget(Text::from(below), below_area);

        let (visible, mut state) = self
            .cursor
            .visible(usize::from(area.height.saturating_sub(1)));
        let size_width = widths[1];
        let line = |row: &[String; 5]| {
            Row::new([
                row[0].clone(),
                format!("{:>size_width$}", row[1]),
                row[2].clone(),
                row[3].clone(),
                cut(&row[4], room),
            ])
        };
        let header = line(&header.map(str::to_string)).bold();
        let constraints = widths
            .iter()
            .map(|&width| Constraint::Length(width as u16))
            .chain([Constraint::Fill(1)]);
        // The cursor's column stays when no row is selected, so that the
        // header doesn't move.
        let table = Table::new(rows[visible].iter().map(line), constraints)
            .column_spacing(2)
            .header(header)
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ")
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// Whether the note over the rows shows.
    fn note_shown(&self) -> bool {
        self.note
            .as_ref()
            .is_some_and(|(text, _)| *text == self.text)
    }
}

/// The first line of an error that SYMBOL gets, such as "`log` could mean 2
/// functions", without what it says about the list of names that the CLI
/// prints after it.
pub fn first_line(error: &anyhow::Error) -> String {
    let error = error.to_string();
    let line = error.lines().next().unwrap_or_default();
    let line = line.trim_end_matches(':');
    line.trim_end_matches("; use one on the left instead")
        .to_string()
}

/// `text` wrapped to `width`, in at most [`NOTE_LINES`] lines, the last
/// one cut when there is more.
fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut wrapped = wrap(text, width);
    if wrapped.len() > NOTE_LINES {
        let rest = wrapped.split_off(NOTE_LINES - 1).join(" ");
        wrapped.push(cut(&rest, width));
    }
    wrapped.into_iter().map(Line::raw).collect()
}
