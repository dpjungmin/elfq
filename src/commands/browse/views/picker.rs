//! The go-to box: the functions, data objects and imports whose names
//! have what the user types, listed as they type. First the item that
//! holds a typed address, then what the text finds as SYMBOL (a name, or
//! the end of one), then every other item or import with a name that has
//! the text, ignoring case, largest first. An import that has a PLT stub
//! is listed as the stub, whose page is the import's. A pattern, such as
//! `on_*`, lists the items that it matches instead, largest first, as the
//! commands read SYMBOL; one that matches none is read as part of a name.
//!
//! [`Listed`] works out the rows for the text typed, and when there are
//! none, why, in the CLI's words; [`Picker`] keeps them while the text
//! stays the same, and draws them with the cursor over one.
//!
//! It isn't a page: it opens over the page on the screen, and Esc closes
//! it, so that what is typed into it can't open pages by itself.

use std::cmp::Reverse;
use std::collections::HashSet;

use anyhow::anyhow;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{HighlightSpacing, Row, Table};

use super::{Cursor, FittedRows, cut, wrap};
use crate::binary::{Binary, ItemKind, hex_number};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::symbol::{
    Lookup, LookupOrMatch, Needle, PatternMatches, Symbol, Takes, USE_ONE,
};
use crate::commands::text;
use crate::graph::Node;

/// The most rows that the box lists.
const MOST: usize = 500;
/// The most lines that the CLI's message over the rows takes.
const NOTE_LINES: usize = 3;

/// One row of the box.
pub(in crate::commands::browse) struct Found {
    /// The item or import.
    pub(in crate::commands::browse) node: Node,
    /// The address typed, when the item holds it.
    pub(in crate::commands::browse) at: Option<u64>,
    /// For the item that the text finds as SYMBOL, its name as the CLI's
    /// answer gives it: "Default_Handler (also named NMI_Handler)", or for
    /// a pattern that matches only it, "on_button (the one item named
    /// on_b*)".
    pub(in crate::commands::browse) lead: Option<String>,
}

pub(in crate::commands::browse) struct Picker {
    /// What the user has typed.
    pub(in crate::commands::browse) text: String,
    /// The rows for the text, and what goes under them.
    listed: Listed,
    cursor: Cursor,
    /// The names of the rows on the screen, shortened.
    fitted: FittedRows,
    /// What the commands said about SYMBOL, with the SYMBOL, shown until
    /// the text changes.
    note: Option<(String, String)>,
    /// Whether the text has changed since the items were listed. They are
    /// listed again only when they are needed, so that keys typed all at
    /// once, as a paste types them, list them once.
    changed: bool,
}

/// The rows that the box lists for the text typed, and what goes under
/// them.
#[derive(Default)]
struct Listed {
    /// The rows, at most [`MOST`] of them.
    rows: Vec<Found>,
    /// How many more items and imports the text finds than the box lists:
    /// those whose names have it, or that it matches as a pattern.
    more: usize,
    /// Why no item has the text, in the CLI's words, when the box lists no
    /// row and the note over the rows doesn't say so already.
    nothing: Option<String>,
}

/// Whether the note over the rows already says why the box lists no row,
/// so that the box needn't work that out again.
#[derive(Clone, Copy)]
enum Explained {
    /// The note is SYMBOL's error, which browse opened the box with, and
    /// the text is still SYMBOL.
    Already,
    /// No note says it.
    NotYet,
}

impl Picker {
    /// The box with `text` typed in.
    pub(in crate::commands::browse) fn new(session: &Session, text: &str) -> Picker {
        Picker::opened(session, text, None)
    }

    /// The box with `text` typed in, and `note` over the rows until the
    /// text changes, such as "`log` could mean 2 functions".
    pub(in crate::commands::browse) fn noted(
        session: &Session,
        text: &str,
        note: String,
    ) -> Picker {
        Picker::opened(session, text, Some(note))
    }

    /// The box with `text` typed in, and `note`, if there is one, over the
    /// rows. The note is there before the rows are listed, so that they
    /// know whether it says why there are none.
    fn opened(session: &Session, text: &str, note: Option<String>) -> Picker {
        let mut picker = Picker {
            text: text.to_string(),
            listed: Listed::default(),
            cursor: Cursor::new(0),
            fitted: FittedRows::default(),
            note: note.map(|note| (text.to_string(), note)),
            changed: false,
        };
        picker.find(session);
        picker
    }

    /// Types `c`, or with `None`, takes the last character back.
    pub(in crate::commands::browse) fn type_char(&mut self, c: Option<char>) {
        match c {
            Some(c) => self.text.push(c),
            None => {
                self.text.pop();
            }
        }
        self.changed = true;
    }

    /// Lists the items and imports for the text typed, if it has changed
    /// since they were listed.
    fn catch_up(&mut self, session: &Session) {
        if self.changed {
            self.changed = false;
            self.find(session);
        }
    }

    /// Lists the items and imports for the text typed.
    fn find(&mut self, session: &Session) {
        let text = self.text.trim();
        // A box opened with SYMBOL's error has it over the rows while the
        // text is SYMBOL.
        let explained = match self.note_shown() {
            true => Explained::Already,
            false => Explained::NotYet,
        };
        self.listed = match text.is_empty() {
            true => Listed::default(),
            false => Listed::typed(session, text, explained),
        };
        self.cursor = Cursor::new(self.listed.rows.len());
        self.fitted = FittedRows::default();
    }

    pub(in crate::commands::browse) fn handle(&mut self, session: &Session, action: Action) {
        self.catch_up(session);
        self.cursor.handle(action);
    }

    /// The selected row.
    pub(in crate::commands::browse) fn selected(&mut self, session: &Session) -> Option<&Found> {
        self.catch_up(session);
        self.listed.rows.get(self.cursor.selected)
    }

    pub(in crate::commands::browse) fn draw(
        &mut self,
        session: &Session,
        frame: &mut Frame,
        area: Rect,
    ) {
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

        let (rows, widths, room) = self.cells(session, width);
        let header = ["ADDRESS", "SIZE", "KIND", "SECTION", "ITEM"];

        // Under the rows, how many more there are, or under the header,
        // why there are none, unless the note over them says so.
        let listed = &self.listed;
        let below = if listed.more > 0 {
            vec![Line::raw(format!("and {} more; type more of the name", listed.more)).dim()]
        } else {
            match &listed.nothing {
                Some(nothing) => lines(nothing, width),
                None => Vec::new(),
            }
        };
        let [area, below_area] = if listed.rows.is_empty() {
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
        let line = |row: &[String; 5], name: String| {
            Row::new([
                row[0].clone(),
                format!("{:>size_width$}", row[1]),
                row[2].clone(),
                row[3].clone(),
                name,
            ])
        };
        let header = header.map(str::to_string);
        let header = line(&header, header[4].clone()).bold();
        let constraints = widths
            .iter()
            .map(|&width| Constraint::Length(width as u16))
            .chain([Constraint::Fill(1)]);
        // Long names are shortened the same way on every row shown, and
        // what SYMBOL said about the row it finds, such as "(also named
        // NMI_Handler)", goes after its name, cut to what is left.
        let rows = &rows[visible.clone()];
        let names = self.fitted.names(visible.clone(), room, || {
            rows.iter().map(|row| row[4].as_str()).collect()
        });
        let items = (names.iter().zip(&listed.rows[visible])).map(|(name, found)| {
            match found.note(binary) {
                Some(note) => cut(&format!("{name}{note}"), room),
                None => name.clone(),
            }
        });
        // The cursor's column stays when no row is selected, so that the
        // header doesn't move.
        let table = Table::new(
            rows.iter().zip(items).map(|(row, item)| line(row, item)),
            constraints,
        )
        .column_spacing(2)
        .header(header)
        .row_highlight_style(Style::new().reversed())
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The cells of every row found, with the name of its item or import
    /// for ITEM, the widths of the columns before ITEM, and the columns
    /// left for ITEM on a screen `width` columns wide. The columns are as
    /// wide as the rows found need, so that they don't move while the
    /// cursor does.
    fn cells(&self, session: &Session, width: usize) -> (Vec<[String; 5]>, Vec<usize>, usize) {
        let binary = session.binary;
        let rows: Vec<[String; 5]> = (self.listed.rows.iter())
            .map(|found| {
                let name = found.node.name(binary).to_string();
                let Node::Item(index) = found.node else {
                    // An import has no address, size or section here.
                    let blank = String::new;
                    return [blank(), blank(), "import".to_string(), blank(), name];
                };
                let item = &binary.items[index];
                let kind = match item.kind {
                    ItemKind::Function => "function",
                    ItemKind::Data => "data",
                };
                [
                    format!("{:#x}", item.address),
                    text::size_text(item),
                    kind.to_string(),
                    binary.sections[item.section].name.clone(),
                    name,
                ]
            })
            .collect();
        let header = ["ADDRESS", "SIZE", "KIND", "SECTION"];
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
        (rows, widths, room)
    }

    /// Whether the note over the rows shows.
    fn note_shown(&self) -> bool {
        self.note
            .as_ref()
            .is_some_and(|(text, _)| *text == self.text)
    }
}

impl Listed {
    /// The rows for `text`, which isn't empty, and when there are none,
    /// why, unless `explained` says that the note over them says so. The
    /// text is looked up once, as SYMBOL, as a name or as a pattern, for
    /// the rows that it finds that way and for the error.
    fn typed(session: &Session, text: &str, explained: Explained) -> Listed {
        let names = &session.names;
        match names.look_up_or_match(text) {
            Ok(LookupOrMatch::Name(lookup)) => {
                let listed = Listed::named(session, text, &lookup);
                listed.or_error(explained, || {
                    names.one_or_error(text, lookup, Takes::One).err()
                })
            }
            Ok(LookupOrMatch::Pattern(matched)) => Listed::matched(session, &matched),
            // A pattern that matches no item can still be part of a name, as
            // `Reading*` is of `app::sort_readings(app::Reading*, unsigned
            // long)`, so that typing a `*` takes away no row.
            Ok(LookupOrMatch::NoMatch(pattern)) => {
                let listed = Listed::named(session, text, &Lookup::NoName);
                listed.or_error(explained, || Some(anyhow!(names.nothing_matches(&pattern))))
            }
            // An address that no item holds, or that isn't one, finds no
            // item as SYMBOL, but names can still have the text.
            Err(error) => {
                let listed = Listed::named(session, text, &Lookup::NoName);
                listed.or_error(explained, || Some(error))
            }
        }
    }

    /// The rows for `text` read as a name, the end of one, part of one, or
    /// an address: first what it finds as SYMBOL, which `lookup` gives,
    /// then every other item or import with a name that has the text,
    /// ignoring case, largest first, where an import with a PLT stub is
    /// there as the stub. When the text finds one item, its row names it
    /// as the CLI's answer does, as in "log_msg (which holds 0x162e)".
    fn named(session: &Session, text: &str, lookup: &Lookup) -> Listed {
        let binary = session.binary;
        let first: Vec<Found> = match lookup {
            Lookup::One(symbol) => {
                // The item holds the address typed, if one was.
                let at = hex_number(text).and_then(Result::ok);
                vec![Found::symbol(binary, symbol, at)]
            }
            Lookup::Several(nodes) => nodes.iter().map(|&node| Found::plain(node)).collect(),
            Lookup::NoName => Vec::new(),
        };
        let in_first: HashSet<Node> = first.iter().map(|found| found.node).collect();
        let lower = text.to_lowercase();
        let needle = Needle::new(&lower);
        let items = (0..binary.items.len()).map(Node::Item);
        let imports = (0..binary.imports.len()).map(Node::Import);
        let others: Vec<Node> = items
            .chain(imports)
            .filter(|node| !in_first.contains(node))
            .filter(|&node| session.names.name_has(node, &needle))
            .filter(|&node| session.page_node(node) == node)
            .collect();
        keep_largest(binary, first, others)
    }

    /// The rows for `matched`, a pattern that matches items: those items,
    /// largest first, as many as the box has room for. The one item that a
    /// pattern matches alone is what it finds as SYMBOL, and its row names
    /// it as the CLI's answer does, as in "on_button (the one item named
    /// on_b*)", which the header of the page that Enter opens says too.
    fn matched(session: &Session, matched: &PatternMatches) -> Listed {
        let binary = session.binary;
        match matched.only_item() {
            Some(symbol) => Listed {
                rows: vec![Found::symbol(binary, &symbol, None)],
                more: 0,
                nothing: None,
            },
            None => {
                let items = (matched.matches.iter())
                    .map(|found| Node::Item(found.item))
                    .collect();
                keep_largest(binary, Vec::new(), items)
            }
        }
    }

    /// These rows, and when there are none, why, in the first line of the
    /// CLI's error, which `error` works out, unless `explained` says that
    /// the note over the rows says so already. Working out the error can
    /// take a while in a large file, as it looks for names like the text,
    /// and for a name, matches every item against the pattern for what is
    /// in a module of that name, so it is done only then.
    fn or_error(
        mut self,
        explained: Explained,
        error: impl FnOnce() -> Option<anyhow::Error>,
    ) -> Listed {
        if self.rows.is_empty() {
            self.nothing = match explained {
                Explained::Already => None,
                Explained::NotYet => error().map(|error| first_line(&error)),
            };
        }
        self
    }
}

/// The rows of `first`, then of the largest of `others`, as many as the
/// box has room for, and how many more there are than it lists. `others`
/// go largest first, and of the same size, first in the file, then the
/// imports. Only those listed are sorted, which saves time when the text
/// is in most names.
fn keep_largest(binary: &Binary, first: Vec<Found>, mut others: Vec<Node>) -> Listed {
    let more = (first.len() + others.len()).saturating_sub(MOST);
    let order = |&node: &Node| (Reverse(node.size(binary)), node);
    let room = MOST.saturating_sub(first.len());
    if others.len() > room {
        if room > 0 {
            others.select_nth_unstable_by_key(room - 1, order);
        }
        others.truncate(room);
    }
    others.sort_unstable_by_key(order);
    let mut rows = first;
    rows.extend(others.into_iter().map(Found::plain));
    rows.truncate(MOST);
    Listed {
        rows,
        more,
        nothing: None,
    }
}

impl Found {
    /// The row of `symbol`, what the text typed finds as SYMBOL, which names
    /// its item or import as the CLI's answer does, with `at`, the address
    /// typed, when one was.
    fn symbol(binary: &Binary, symbol: &Symbol, at: Option<u64>) -> Found {
        Found {
            node: symbol.node,
            at,
            lead: Some(symbol.lead_name(binary)),
        }
    }

    /// The row of `node`, with no address typed and nothing after its
    /// name.
    fn plain(node: Node) -> Found {
        Found {
            node,
            at: None,
            lead: None,
        }
    }

    /// What SYMBOL said about the item it finds, after its name: " (also
    /// named NMI_Handler)".
    fn note<'a>(&'a self, binary: &Binary) -> Option<&'a str> {
        let lead = self.lead.as_deref()?;
        lead.strip_prefix(self.node.name(binary))
            .filter(|note| !note.is_empty())
    }
}

/// The first line of an error that SYMBOL gets, such as "`log` could mean 2
/// functions", without what it says about the list of names that the CLI
/// prints after it ([`USE_ONE`]).
pub(in crate::commands::browse) fn first_line(error: &anyhow::Error) -> String {
    let error = error.to_string();
    let line = error.lines().next().unwrap_or_default();
    let line = line.trim_end_matches(':');
    line.trim_end_matches(&format!("; {USE_ONE}")).to_string()
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

#[cfg(test)]
mod tests {
    use super::super::names::alike_in_a_window;
    use super::super::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::commands::symbol::Pattern;

    #[test]
    fn a_box_opened_with_the_error_for_symbol_doesnt_work_it_out_again() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        let error = |text: &str| format!("no function or data object is named `{text}`");
        // Typed, a text that finds nothing gets the error under the header.
        let typed = Picker::new(&session, "nosuch");
        assert_eq!(typed.listed.nothing, Some(error("nosuch")));
        // Opened with the error over the rows, as `browse nosuch` opens it,
        // the box leaves it there, until the text changes.
        let mut opened = Picker::noted(&session, "nosuch", error("nosuch"));
        assert_eq!(opened.listed.nothing, None);
        opened.type_char(Some('x'));
        opened.catch_up(&session);
        assert_eq!(opened.listed.nothing, Some(error("nosuchx")));
        opened.type_char(None);
        opened.catch_up(&session);
        assert_eq!(opened.listed.nothing, None);
    }

    #[test]
    fn a_pattern_lists_the_largest_of_the_items_that_it_matches() {
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let file = path.display();
            for text in ["*", "_*", "*_*", "main*", "*.*"] {
                let pattern = Pattern::parse(text).unwrap();
                let matched: Vec<Node> = (session.names.matching(&pattern).iter())
                    .map(|found| Node::Item(found.item))
                    .collect();
                if matched.is_empty() {
                    // Then the text is read as part of a name.
                    continue;
                }
                let listed = Picker::new(&session, text).listed;
                // At most as many rows as the box lists, and how many more.
                assert!(listed.rows.len() <= MOST, "{file}, {text}");
                assert_eq!(
                    listed.rows.len() + listed.more,
                    matched.len(),
                    "{file}, {text}"
                );
                // Each is a match, largest first, and no match left out is
                // larger than the smallest listed.
                let sizes: Vec<u64> = (listed.rows.iter())
                    .map(|found| found.node.size(&binary))
                    .collect();
                assert!(sizes.is_sorted_by(|a, b| a >= b), "{file}, {text}");
                let rows: HashSet<Node> = listed.rows.iter().map(|found| found.node).collect();
                assert!(
                    rows.iter().all(|node| matched.contains(node)),
                    "{file}, {text}"
                );
                let smallest = sizes.last().copied().unwrap_or(0);
                let left_out = matched.iter().filter(|node| !rows.contains(node));
                assert!(
                    left_out
                        .map(|node| node.size(&binary))
                        .all(|size| size <= smallest),
                    "{file}, {text}"
                );
                // The one item that it matches alone is named as SYMBOL's
                // answer names it.
                let leads: Vec<Option<&str>> = (listed.rows.iter())
                    .map(|found| found.lead.as_deref())
                    .collect();
                let one_item = match matched[..] {
                    [only] => Some(format!(
                        "{} (the one item named {text})",
                        only.name(&binary)
                    )),
                    _ => None,
                };
                match one_item {
                    Some(lead) => assert_eq!(leads, [Some(lead.as_str())], "{file}"),
                    None => assert!(leads.iter().all(Option::is_none), "{file}, {text}"),
                }
            }
        }
    }

    #[test]
    fn rows_that_would_read_the_same_read_differently_on_the_screen() {
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            for text in ["_", "vtable", "closure", "split", "drop", "new", "log"] {
                let picker = Picker::new(&session, text);
                for width in [60, 80, 100] {
                    let (rows, _, room) = picker.cells(&session, width);
                    let names: Vec<&str> = rows.iter().map(|row| row[4].as_str()).collect();
                    if let Some((one, other, both)) = alike_in_a_window(&names, room) {
                        panic!(
                            "{}, {text}: {one} and {other} both read {both} at {width}",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}
