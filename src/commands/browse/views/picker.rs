//! The go-to box: the functions, data objects and imports whose names
//! have what the user types, listed as they type. First the item that
//! holds a typed address, then what the text finds as SYMBOL (a name, or
//! the end of one), then every other item or import with a name that has
//! the text, ignoring case, largest first. An import that has a PLT stub
//! is listed as the stub, whose page is the import's. A pattern, such as
//! `on_*`, lists the items that it matches instead, largest first, as the
//! commands read SYMBOL; one that matches none is read as part of a name.
//!
//! [`Listed`] works out the rows for the text typed, with their cells, and
//! when there are none, why, in the CLI's words; [`Picker`] keeps them
//! while the text stays the same, and draws those on the screen with the
//! cursor over one.
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

use super::{Cursor, FittedRows, PageAt, cut, wrap};
use crate::binary::{Binary, hex_number};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::symbol::{
    Lookup, LookupOrMatch, Needle, PatternMatches, Symbol, USE_ONE, with_note,
};
use crate::commands::text;
use crate::graph::Node;

/// The most rows that the box lists.
const MOST: usize = 500;
/// The most lines that an error takes, over the rows or under the header.
const ERROR_LINES: usize = 3;
/// The table's headers. The columns before ITEM are as wide as the rows
/// listed need, so that they don't move while the cursor does. A new
/// column needs a field of [`PickerRow`], set in [`PickerRow::new`], its
/// cell in [`PickerRow::cells`] and its place in [`table_row`]; the
/// compiler then asks for the rest.
const COLUMNS: [&str; 5] = ["ADDRESS", "SIZE", "KIND", "SECTION", "ITEM"];

/// One row of the box, with its cells, which are worked out once, when
/// the text is listed, and not on each draw.
pub(in crate::commands::browse) struct PickerRow {
    /// The item or import, with the address typed, when the item holds
    /// it.
    pub(in crate::commands::browse) page: PageAt,
    /// For the item that the text finds as SYMBOL, what the CLI's answer
    /// adds to its name, if anything: "also named NMI_Handler", or for a
    /// pattern that matches only it, "the one item named on_b*" (see
    /// [`Symbol::note`]). ITEM shows it after the name.
    pub(in crate::commands::browse) note: Option<String>,
    /// The text of the ADDRESS and SIZE cells, blank for an import.
    address: String,
    size: String,
    /// What the item is, "function" or "data", as
    /// [`ItemKind`](crate::binary::ItemKind) writes it, or "import".
    kind: String,
    /// The name of the item's section, blank for an import.
    section_name: String,
    /// The name of the item or import in full, which ITEM shortens to fit.
    name: String,
}

pub(in crate::commands::browse) struct Picker {
    /// What the user has typed.
    pub(in crate::commands::browse) text: String,
    /// The rows for the text, and what goes under them.
    listed: Listed,
    cursor: Cursor,
    /// The names of the rows on the screen, shortened.
    fitted: FittedRows,
    /// SYMBOL and its error, when browse opened the box with them.
    opened_with: Option<OpenedWith>,
    /// Whether the text has changed since the items were listed. They are
    /// listed again only when they are needed, so that keys typed all at
    /// once, as a paste types them, list them once.
    changed: bool,
}

/// What browse opens the box with when SYMBOL finds several items or none:
/// SYMBOL, typed in, and the first line of the CLI's error for it, which
/// shows over the rows until the text changes.
struct OpenedWith {
    /// SYMBOL, as typed.
    text: String,
    /// The error's first line, such as "`log` could mean 2 functions".
    error: String,
}

/// The rows that the box lists for the text typed, and what goes under
/// them.
struct Listed {
    /// The rows, at most [`MOST`] of them.
    rows: Vec<PickerRow>,
    /// How wide each column before ITEM is: as wide as its header and the
    /// rows' cells in it.
    widths: [usize; 4],
    /// How many more items and imports the text finds than the box lists:
    /// those whose names have it, or that it matches as a pattern.
    more: usize,
    /// Why no item has the text, in the CLI's words, when the box lists no
    /// row and the error over the rows doesn't say so already.
    nothing: Option<String>,
}

/// Whether the error over the rows already says why the box lists no row,
/// so that the box needn't work that out again.
#[derive(Clone, Copy)]
enum Explained {
    /// The error over the rows is SYMBOL's, which browse opened the box
    /// with, and the text is still SYMBOL.
    Already,
    /// No error over the rows says it.
    NotYet,
}

impl Picker {
    /// The box with `text` typed in.
    pub(in crate::commands::browse) fn new(session: &Session, text: &str) -> Picker {
        Picker::opened(session, text, None)
    }

    /// The box with `text` typed in, and `error`, the first line of the
    /// error that `text` gets as SYMBOL, such as "`log` could mean 2
    /// functions", over the rows until the text changes.
    pub(in crate::commands::browse) fn with_error(
        session: &Session,
        text: &str,
        error: String,
    ) -> Picker {
        Picker::opened(session, text, Some(error))
    }

    /// The box with `text` typed in, and `error`, if there is one, over
    /// the rows. The error is there before the rows are listed, so that
    /// they know whether it says why there are none.
    fn opened(session: &Session, text: &str, error: Option<String>) -> Picker {
        let opened_with = error.map(|error| OpenedWith {
            text: text.to_string(),
            error,
        });
        let mut picker = Picker {
            text: text.to_string(),
            listed: Listed::new(Vec::new(), 0),
            cursor: Cursor::new(0),
            fitted: FittedRows::default(),
            opened_with,
            changed: false,
        };
        picker.find(session);
        picker
    }

    /// Types `c` after the text.
    pub(in crate::commands::browse) fn push(&mut self, c: char) {
        self.text.push(c);
        self.changed = true;
    }

    /// Takes the text's last character back, if it has one.
    pub(in crate::commands::browse) fn pop(&mut self) {
        self.text.pop();
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
        let explained = match self.error_shown() {
            true => Explained::Already,
            false => Explained::NotYet,
        };
        self.listed = match text.is_empty() {
            true => Listed::new(Vec::new(), 0),
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
    pub(in crate::commands::browse) fn selected(
        &mut self,
        session: &Session,
    ) -> Option<&PickerRow> {
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
        let width = usize::from(area.width);
        let error = (self.opened_with.as_ref())
            .filter(|_| self.error_shown())
            .map(|opened_with| lines(&opened_with.error, width));
        let [error_area, area] = Layout::vertical([
            Constraint::Length(error.as_ref().map_or(0, |error| error.len() as u16)),
            Constraint::Fill(1),
        ])
        .areas(area);
        if let Some(error) = error {
            frame.render_widget(Text::from(error), error_area);
        }
        if self.text.trim().is_empty() {
            let placeholder = "type a name, the end of a name, or an address";
            frame.render_widget(Line::raw(placeholder).dim(), area);
            return;
        }

        let room = self.name_room(width);
        // Under the rows, how many more there are, or under the header,
        // why there are none, unless the error over them says so.
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
        let widths = &listed.widths;
        let [.., item_header] = COLUMNS;
        let header = table_row(headers_before_item(), widths, item_header.to_string());
        let constraints = (widths.iter())
            .map(|&width| Constraint::Length(width as u16))
            .chain([Constraint::Fill(1)]);
        // Only the rows shown are drawn. Long names are shortened the same
        // way on every row shown, and what SYMBOL said about the row it
        // finds, such as "(also named NMI_Handler)", goes after its name,
        // cut to what is left.
        let rows = &listed.rows[visible.clone()];
        let names = self.fitted.names(visible, room, || {
            rows.iter().map(|row| row.name.as_str()).collect()
        });
        let table_rows = rows.iter().zip(names).map(|(row, name)| {
            let item_cell = match row.note.as_deref() {
                Some(note) => cut(&with_note(name, Some(note)), room),
                None => name.clone(),
            };
            table_row(row.cells(), widths, item_cell)
        });
        // The cursor's column stays when no row is selected, so that the
        // header doesn't move.
        let table = Table::new(table_rows, constraints)
            .column_spacing(2)
            .header(header.bold())
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ")
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The columns left for ITEM on a screen `width` columns wide, after
    /// the cursor's "> " and two spaces after each column before it.
    fn name_room(&self, width: usize) -> usize {
        let used: usize = self.listed.widths.iter().map(|width| width + 2).sum();
        width.saturating_sub(2 + used)
    }

    /// Whether the error that browse opened the box with shows over the
    /// rows: while the text is still SYMBOL.
    fn error_shown(&self) -> bool {
        (self.opened_with.as_ref()).is_some_and(|opened_with| opened_with.text == self.text)
    }
}

impl Listed {
    /// `rows`, and `more`, how many more items and imports the text finds
    /// than the box lists, with the widths of the columns before ITEM,
    /// worked out once for the text.
    fn new(rows: Vec<PickerRow>, more: usize) -> Listed {
        let mut widths = headers_before_item().map(str::len);
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row.cells()) {
                *width = (*width).max(cell.chars().count());
            }
        }
        Listed {
            rows,
            widths,
            more,
            nothing: None,
        }
    }

    /// The rows for `text`, which isn't empty, and when there are none,
    /// why, unless `explained` says that the error over them says so. The
    /// text is looked up once, as SYMBOL, as a name or as a pattern, for
    /// the rows that it finds that way and for the error.
    fn typed(session: &Session, text: &str, explained: Explained) -> Listed {
        let names = &session.names;
        match names.look_up_or_match(text) {
            Ok(LookupOrMatch::Name(lookup)) => {
                let listed = Listed::named(session, text, &lookup);
                listed.or_error(explained, || names.one_or_error(text, lookup).err())
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
        // Every item or import not in `first` with a name that has the
        // text.
        let others = |first: &[Node]| -> Vec<Node> {
            let in_first: HashSet<Node> = first.iter().copied().collect();
            let lower = text.to_lowercase();
            let needle = Needle::new(&lower);
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            items
                .chain(imports)
                .filter(|node| !in_first.contains(node))
                .filter(|&node| session.names.name_has(node, &needle))
                .filter(|&node| session.page_node(node) == node)
                .collect()
        };
        match lookup {
            Lookup::One(symbol) => {
                // The item holds the address typed, if one was.
                let at = hex_number(text).and_then(Result::ok);
                let first = [PickerRow::symbol(binary, symbol, at)];
                keep_largest(binary, first.into_iter(), others(&[symbol.node]))
            }
            Lookup::Several(nodes) => {
                let first = nodes.iter().map(|&node| PickerRow::plain(binary, node));
                keep_largest(binary, first, others(nodes))
            }
            Lookup::NoName => keep_largest(binary, std::iter::empty(), others(&[])),
        }
    }

    /// The rows for `matched`, a pattern that matches items: those items,
    /// largest first, as many as the box has room for. The one item that a
    /// pattern matches alone is what it finds as SYMBOL, and its row names
    /// it as the CLI's answer does, as in "on_button (the one item named
    /// on_b*)", which the header of the page that Enter opens says too.
    fn matched(session: &Session, matched: &PatternMatches) -> Listed {
        let binary = session.binary;
        match matched.only_item() {
            Some(symbol) => Listed::new(vec![PickerRow::symbol(binary, &symbol, None)], 0),
            None => {
                let items = (matched.matches.iter())
                    .map(|found| Node::Item(found.item))
                    .collect();
                keep_largest(binary, std::iter::empty(), items)
            }
        }
    }

    /// These rows, and when there are none, why, in the first line of the
    /// CLI's error, which `error` works out, unless `explained` says that
    /// the error over the rows says so already. Working out the error can
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
/// box has room for, and how many more there are than it lists. Only
/// those listed are sorted, which saves time when the text is in most
/// names, and only those listed are made into rows, with their cells,
/// which saves time when the text finds thousands as SYMBOL. `first` is
/// read lazily, so pass it as a map over the nodes, as [`Listed::named`]
/// does.
fn keep_largest(
    binary: &Binary,
    first: impl ExactSizeIterator<Item = PickerRow>,
    others: Vec<Node>,
) -> Listed {
    let more = (first.len() + others.len()).saturating_sub(MOST);
    let others = largest(binary, others, MOST.saturating_sub(first.len()))
        .into_iter()
        .map(|node| PickerRow::plain(binary, node));
    Listed::new(first.chain(others).take(MOST).collect(), more)
}

/// The largest `room` of `others`, largest first, and of the same size,
/// first in the file, then the imports. It isn't generic, as
/// [`keep_largest`] is, so that the sort is compiled once rather than
/// once for each kind of `first`.
fn largest(binary: &Binary, others: Vec<Node>, room: usize) -> Vec<Node> {
    let order = |&node: &Node| (Reverse(node.size(binary)), node);
    let mut largest = text::first_by_key(others, room, order);
    largest.sort_unstable_by_key(order);
    largest
}

impl PickerRow {
    /// The row of `symbol`, what the text typed finds as SYMBOL, which names
    /// its item or import as the CLI's answer does, with `at`, the address
    /// typed, when one was.
    fn symbol(binary: &Binary, symbol: &Symbol, at: Option<u64>) -> PickerRow {
        let page = PageAt {
            node: symbol.node,
            at,
        };
        PickerRow::new(binary, page, symbol.note().map(str::to_string))
    }

    /// The row of `node`, with no address typed and nothing after its
    /// name.
    fn plain(binary: &Binary, node: Node) -> PickerRow {
        PickerRow::new(binary, PageAt { node, at: None }, None)
    }

    /// The row that opens `page`, with `note` after its name, and the
    /// cells of its item or import.
    fn new(binary: &Binary, page: PageAt, note: Option<String>) -> PickerRow {
        let node = page.node;
        let [address, size] = text::address_and_size(binary, node);
        let (kind, section_name) = match node {
            Node::Item(index) => {
                let item = &binary.items[index];
                let section_name = binary.sections[item.section].name.clone();
                (item.kind.to_string(), section_name)
            }
            // An import has no section here.
            Node::Import(_) => ("import".to_string(), String::new()),
        };
        PickerRow {
            page,
            note,
            address,
            size,
            kind,
            section_name,
            name: node.name(binary).to_string(),
        }
    }

    /// The cells before ITEM, in the order of [`COLUMNS`].
    fn cells(&self) -> [&str; 4] {
        [&self.address, &self.size, &self.kind, &self.section_name]
    }
}

/// The headers in [`COLUMNS`] before ITEM, which is the last.
fn headers_before_item() -> [&'static str; 4] {
    let [before_item @ .., _] = COLUMNS;
    before_item
}

/// A row of the table: `cells`, the cells before ITEM, then `item_cell`.
/// SIZE goes on the right of its column, as wide as `widths` says, as
/// numbers do; the table sets the other cells in their columns.
fn table_row(cells: [&str; 4], widths: &[usize; 4], item_cell: String) -> Row<'static> {
    let [address, size, kind, section_name] = cells;
    let [_, size_width, _, _] = *widths;
    Row::new([
        address.to_string(),
        format!("{size:>size_width$}"),
        kind.to_string(),
        section_name.to_string(),
        item_cell,
    ])
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

/// `text` wrapped to `width`, in at most [`ERROR_LINES`] lines, the last
/// one cut when there is more.
fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut wrapped = wrap(text, width);
    if wrapped.len() > ERROR_LINES {
        let rest = wrapped.split_off(ERROR_LINES - 1).join(" ");
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
        let mut opened = Picker::with_error(&session, "nosuch", error("nosuch"));
        assert_eq!(opened.listed.nothing, None);
        opened.push('x');
        opened.catch_up(&session);
        assert_eq!(opened.listed.nothing, Some(error("nosuchx")));
        opened.pop();
        opened.catch_up(&session);
        assert_eq!(opened.listed.nothing, None);
    }

    #[test]
    fn the_box_lists_at_most_500_rows_however_many_items_symbol_finds() {
        // As SYMBOL, a text such as `fmt` finds hundreds of functions in
        // a large Rust file, more than the box lists; the other items
        // whose names have it would come after them, and are left out.
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&path).unwrap();
        let first = (0..MOST + 100).map(|_| PickerRow::plain(&binary, Node::Item(0)));
        let listed = keep_largest(&binary, first, vec![Node::Item(1)]);
        assert_eq!((listed.rows.len(), listed.more), (MOST, 101));
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
                    .map(|picker_row| picker_row.page.node.size(&binary))
                    .collect();
                assert!(sizes.is_sorted_by(|a, b| a >= b), "{file}, {text}");
                let rows: HashSet<Node> = (listed.rows.iter())
                    .map(|picker_row| picker_row.page.node)
                    .collect();
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
                // The one item that it matches alone has the note that
                // SYMBOL's answer gives it.
                let notes: Vec<Option<&str>> = (listed.rows.iter())
                    .map(|picker_row| picker_row.note.as_deref())
                    .collect();
                match matched[..] {
                    [_] => {
                        let note = format!("the one item named {text}");
                        assert_eq!(notes, [Some(note.as_str())], "{file}");
                    }
                    _ => assert!(notes.iter().all(Option::is_none), "{file}, {text}"),
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
                let names: Vec<&str> = (picker.listed.rows.iter())
                    .map(|row| row.name.as_str())
                    .collect();
                for width in [60, 80, 100] {
                    let room = picker.name_room(width);
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
