//! A command's answer as a page or a tab shows it: the lines that the
//! command prints before its table, the table, each row of which opens the
//! item that it is about, and the notes under it. The start page and the
//! callees, callers, reach, why and retained tabs are each one, made from
//! their command's own answer, with every row, and under them is that
//! command, ready to paste.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Row, Table};

use super::item::Tab;
use super::{
    Cursor, FittedRows, Go, cut, draw_lines, fit_name, follow, has_ignoring_case, keep_lines,
    link_preview, wrap,
};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::{TabAnswer, TreeAnswer, reach};
use crate::graph::Node;
use crate::render::{Align, flat_rows};

/// Lines of a command's text around the table, before it or under its
/// notes.
pub enum Lines {
    /// Words, which wrap to fit the screen.
    Words(String),
    /// The lines of a table without links, such as the blocks of memory,
    /// each cut to fit, ending in "…". They are set in as far as the rows
    /// of the table, so that the columns of both line up.
    Table(Vec<String>),
}

/// A row of the table.
pub struct PrintedRow {
    /// Every cell but the last.
    pub cells: Vec<String>,
    /// The last cell: a name, which is shortened to fit, then what the row
    /// says about it, such as ", used by main".
    pub name: String,
    pub rest: String,
    /// Where in `rest` the name of the row that this one hangs under
    /// starts, if `rest` ends with one, as `main` in ", kept by main": it
    /// is shortened to fit, as the row's own name is.
    pub under: Option<usize>,
    /// The item or import that Enter opens, and the address in it to
    /// select.
    pub link: Option<(Node, Option<u64>)>,
}

/// The columns of words, which give way to the names on a narrow screen
/// (see [`Printed::columns`]). The others hold numbers and addresses,
/// which are never cut.
const WORDS: [&str; 2] = ["WHY", "HOW"];
/// The columns that the column of words is cut down to, for the last
/// column to show its widest cell, up to [`LAST_WIDTH`] columns of it.
const WORDS_WIDTH: usize = 10;
const LAST_WIDTH: usize = 32;
/// The fewest columns that names keep when they need them: the column of
/// words is cut further for the last column to have them, down to one,
/// and a name is shortened to fewer only when the room is narrower.
const NAME_WIDTH: usize = 16;
/// The columns that a note naming only the first item of a list, for a
/// screen with little room for it, shortens that item's name to.
const NOTE_NAME_WIDTH: usize = 40;

pub struct Printed {
    /// Lines of another command, before the answer's own, which give way
    /// first on a short screen: the overview's, on the start page.
    context: Vec<Lines>,
    above: Vec<Lines>,
    /// The table's columns, the last of which names an item.
    columns: Vec<(String, Align)>,
    rows: Vec<PrintedRow>,
    /// How wide each column but the last is, worked out once, so that the
    /// columns don't move as the rows scroll.
    widths: Vec<usize>,
    /// The column of words, if the table has one, by its index.
    words: Option<usize>,
    /// The widest last cell, a name and what follows it, or the header.
    widest_last: usize,
    cursor: Cursor,
    /// The names of the rows on the screen, shortened.
    fitted: FittedRows,
    notes: Vec<String>,
    /// The notes as the command words them without `--all`, if it needs
    /// it to print them all: they name fewer, and are shown when the notes
    /// in full don't fit, or would take lines that rows need (see
    /// [`Printed::lay_out`]).
    brief: Option<Vec<String>>,
    /// The notes naming only the first item of each list, shortened, if
    /// they name lists that can be long: they are shown when the notes
    /// without `--all` don't fit either, or would take lines that rows
    /// need, so that what a note says about the items it names shows.
    briefest: Option<Vec<String>>,
    /// Lines of other commands, under the notes: the retained bytes in
    /// each block of memory, on the retained tab.
    below: Vec<Lines>,
    /// The lines before the table and the notes, as last laid out, since
    /// a note can be long.
    laid_out: Option<LaidOut>,
    /// The command that prints the same answer.
    command: String,
    /// The tab that Enter opens the selected row's item on, when it has
    /// that tab.
    tab: Tab,
    /// The item or import whose page it is on, if any.
    page: Option<Node>,
}

/// The lines before a table and the notes under it, laid out for a size
/// of screen.
struct LaidOut {
    /// The width and the height.
    size: (usize, usize),
    above: Vec<String>,
    notes: Vec<String>,
    below: Vec<String>,
}

impl Printed {
    /// The answer with these lines before the table, the table's columns
    /// and rows, and notes under them, printed by `command`. Enter opens a
    /// row's item on its code.
    pub fn new(
        above: Vec<Lines>,
        columns: Vec<(String, Align)>,
        rows: Vec<PrintedRow>,
        notes: Vec<String>,
        command: String,
    ) -> Printed {
        let widths = (0..columns.len().saturating_sub(1))
            .map(|column| {
                let cells = rows.iter().map(|row| row.cells[column].chars().count());
                cells.chain([columns[column].0.chars().count()]).max()
            })
            .map(Option::unwrap_or_default)
            .collect();
        let last = (rows.iter()).map(|row| row.name.chars().count() + row.rest.chars().count());
        let header = columns
            .last()
            .map_or(0, |(header, _)| header.chars().count());
        Printed {
            context: Vec::new(),
            above,
            words: (columns.iter()).position(|(header, _)| WORDS.contains(&header.as_str())),
            widest_last: last.chain([header]).max().unwrap_or(0),
            columns,
            widths,
            cursor: Cursor::new(rows.len()),
            fitted: FittedRows::default(),
            rows,
            notes,
            brief: None,
            briefest: None,
            below: Vec::new(),
            laid_out: None,
            command,
            tab: Tab::Code,
            page: None,
        }
    }

    /// `answer`, an answer of `reach`, printed by `command`. Each row
    /// opens the item it is about.
    pub fn of_reach(answer: reach::Answer, command: String) -> Printed {
        let mut above: Vec<Lines> = answer.lead.into_iter().map(Lines::Words).collect();
        if let Some(memory) = answer.memory {
            above.push(Lines::Table(table_lines(&memory)));
        }
        let rows = (answer.rows.into_iter())
            .map(|row| PrintedRow {
                cells: row.cells,
                name: row.name,
                rest: row.rest,
                under: None,
                link: row.node.map(|node| (node, None)),
            })
            .collect();
        Printed::new(above, answer.columns, rows, answer.notes, command)
    }

    /// `answer`, an answer of `why` or `retained` as a tab shows it,
    /// printed by `command`, with every row and name, and its notes in
    /// shorter forms for when they don't fit (see [`Printed::lay_out`]).
    pub fn of_tab(answer: TabAnswer, command: String) -> Printed {
        let mut printed = Printed::of_tree(answer.answer, command);
        printed.brief = Some(answer.brief);
        printed.briefest = Some(answer.briefest);
        printed
    }

    /// `answer`, an answer of `why` or `retained`, printed by `command`,
    /// with the tree's rows one per line, each naming the row it hangs
    /// under where the command's does. Each row opens the item it is about.
    fn of_tree(answer: TreeAnswer, command: String) -> Printed {
        let above = answer.lead.into_iter().map(Lines::Words).collect();
        let columns = (answer.columns.iter())
            .map(|&(header, align)| (header.to_string(), align))
            .collect();
        // An answer of sentences has no rows, and no shape to show them in.
        let rows = match answer.rows.is_empty() {
            true => Vec::new(),
            false => flat_rows(&answer.rows, answer.shape),
        };
        let rows = (rows.into_iter())
            .map(|flat| PrintedRow {
                cells: flat.row.cells.clone(),
                name: flat.row.name.clone(),
                rest: flat.rest,
                under: flat.under,
                link: flat.row.node.map(|node| (node, None)),
            })
            .collect();
        Printed::new(above, columns, rows, answer.notes, command)
    }

    /// The same, after `context`, lines of another command.
    pub fn with_context(mut self, context: Vec<Lines>) -> Printed {
        self.context = context;
        self
    }

    /// The same, but with `brief`, the notes as the command words them
    /// without `--all`, to show when the notes in full don't fit.
    pub fn with_brief(mut self, brief: Vec<String>) -> Printed {
        self.brief = Some(brief);
        self
    }

    /// The same, with `below`, lines of other commands, under the notes.
    pub fn with_below(mut self, below: Vec<Lines>) -> Printed {
        self.below = below;
        self
    }

    /// The same, but Enter opens a row's item on `tab`, when it has that
    /// tab, as a link opens a page on the tab that it is on. `page` is the
    /// item or import whose page it is on.
    pub fn opening(mut self, tab: Tab, page: Node) -> Printed {
        self.tab = tab;
        self.page = Some(page);
        self
    }

    /// The command that prints the same answer.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Draws the lines before the table, the table, and the notes under
    /// it, as [`Printed::lay_out`] lays them out. A row too wide for the
    /// screen is cut at its edge, ending in "…", rather than any of its
    /// numbers.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        self.lay_out(width, usize::from(area.height));
        let LaidOut {
            above,
            notes,
            below,
            ..
        } = self.laid_out.as_ref().expect("laid out above");
        let [top, body, foot, bottom] = Layout::vertical([
            Constraint::Length(above.len() as u16),
            Constraint::Fill(1),
            Constraint::Length(notes.len() as u16),
            Constraint::Length(below.len() as u16),
        ])
        .areas(area);
        draw_lines(frame, top, above.iter().map(Line::raw).collect());
        draw_lines(frame, foot, notes.iter().map(Line::raw).collect());
        draw_lines(frame, bottom, below.iter().map(Line::raw).collect());
        if self.rows.is_empty() {
            return;
        }

        // The table's header takes a line.
        let (visible, mut state) = self
            .cursor
            .visible(usize::from(body.height).saturating_sub(1));
        let (widths, room) = self.columns(width);
        // A name and what the row says after it are shown in full when
        // they fit. Longer names are shortened, the same way on every such
        // row shown, to leave room for what follows them, but to at least
        // two thirds of the room, or NAME_WIDTH columns, which that is cut
        // to; the selected one is in full on the status line. What follows
        // them can name the row that the row hangs under, after "kept by"
        // or "used by": that name is shortened too, to what the row's own
        // name leaves of the room, which is NAME_WIDTH columns or more
        // where the names can leave it.
        let rows = &self.rows[visible.clone()];
        let length = |text: &str| text.chars().count();
        let fits = |row: &PrintedRow| length(&row.name) + length(&row.rest) <= room;
        let longer = || rows.iter().filter(|row| !fits(row));
        // The columns that what follows a name needs, with the name after
        // "kept by" shortened to NAME_WIDTH columns.
        let fewest_after = |row: &PrintedRow| {
            let under = row.under.map_or(0, |at| length(&row.rest[at..]));
            length(&row.rest) - under + under.min(NAME_WIDTH)
        };
        let after = longer().map(fewest_after).max().unwrap_or(0);
        let widest = longer().map(|row| length(&row.name)).max().unwrap_or(0);
        let fewest = (room * 2 / 3).max(room.min(NAME_WIDTH));
        let names_room = room.saturating_sub(after).max(widest.min(fewest));
        let names = self.fitted.names(visible, names_room, || {
            rows.iter().map(|row| row.name.as_str()).collect()
        });
        let rests: Vec<Cow<str>> = (rows.iter().zip(names))
            .map(|(row, name)| match row.under {
                Some(at) if !fits(row) => {
                    let (words, under) = row.rest.split_at(at);
                    let left = room.saturating_sub(length(name) + length(words));
                    Cow::Owned(format!("{words}{}", fit_name(under, left.max(1))))
                }
                _ => Cow::Borrowed(row.rest.as_str()),
            })
            .collect();
        let last = |row: &PrintedRow, name: &str, rest: &str| {
            if fits(row) {
                format!("{}{}", row.name, row.rest)
            } else if !rest.is_empty() && length(name) + 4 > room {
                // Too little would be left of what follows the name for it
                // to say anything: the name takes the room.
                cut(&format!("{}{rest}", fit_name(&row.name, room)), room)
            } else {
                cut(&format!("{name}{rest}"), room)
            }
        };
        // Each row is one line, after the cursor's "> ".
        let columns = &self.columns;
        let line = |cells: Vec<&str>, last: &str| {
            let mut line = String::new();
            for ((cell, &(_, align)), &width) in cells.into_iter().zip(columns).zip(&widths) {
                let cell = cut(cell, width);
                line += &match align {
                    Align::Left => format!("{cell:<width$}  "),
                    Align::Right => format!("{cell:>width$}  "),
                };
            }
            line += last;
            Row::new([cut(line.trim_end(), width.saturating_sub(2))])
        };
        let lines = (rows.iter().zip(names).zip(&rests)).map(|((row, name), rest)| {
            let cells = row.cells.iter().map(String::as_str).collect();
            line(cells, &last(row, name, rest))
        });
        let ((last, _), headers) = columns.split_last().expect("a table has columns");
        let headers = headers.iter().map(|(header, _)| header.as_str()).collect();
        let header = line(headers, &cut(last, room));
        let table = Table::new(lines, [Constraint::Fill(1)])
            .header(header.bold())
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        frame.render_stateful_widget(table, body, &mut state);
    }

    /// The lines before the table and the notes under it, on a screen
    /// `width` columns wide with `height` lines for all of it, laid out
    /// once for each size. They wrap, so that nothing is cut, when the
    /// screen has room for them and the table.
    ///
    /// Otherwise the table keeps its header and a few rows, or a third of
    /// the lines, and the notes take what the lines before the table and
    /// under the notes leave of the rest, or at least a third of it, but a
    /// line for the answer's own first line. Notes that don't fit in full
    /// are shown as the command words them without `--all`, if it does, or
    /// failing that, naming only the first item of each list, if they can:
    /// the end of a note that names thousands of functions says what they
    /// are. Where the table can't show every row, the notes take the first
    /// of these forms that needs at most a third of the lines, if one
    /// does, so that the rows keep the screen.
    ///
    /// Then the answer's own lines before the table take what they need
    /// of what is left, the other commands' lines under the notes next,
    /// and the other command's lines before the answer's own, which come
    /// first, what is left after that. What doesn't fit is cut, ending in
    /// "…".
    fn lay_out(&mut self, width: usize, height: usize) {
        let size = (width, height);
        if self
            .laid_out
            .as_ref()
            .is_none_or(|laid_out| laid_out.size != size)
        {
            let table = match self.rows.len() {
                0 => 0,
                rows => (rows + 1).min((height / 3).max(3)),
            };
            let room = height.saturating_sub(table);
            let own = blocks(&self.above, width);
            let own_lines: usize = own.iter().map(|(_, lines)| lines.len()).sum();
            let below = blocks(&self.below, width);
            let below_lines: usize = below.iter().map(|(_, lines)| lines.len()).sum();
            let most = (room.saturating_sub(own_lines + below_lines))
                .max((room / 3).max(2))
                .min(room.saturating_sub(own_lines.min(1)));
            let wrapped = |notes: &[String]| -> Vec<String> {
                (notes.iter()).flat_map(|note| wrap(note, width)).collect()
            };
            // Notes with more characters than the lines hold, and the
            // space that each line can end at, can't fit, and aren't
            // wrapped only to be cut.
            let fit = |notes: &Vec<String>| {
                let length: usize = notes.iter().map(|note| note.chars().count()).sum();
                let lines = (length <= most * (width + 1)).then(|| wrapped(notes))?;
                (lines.len() <= most).then_some(lines)
            };
            let forms: Vec<&Vec<String>> = [Some(&self.notes), self.brief.as_ref()]
                .into_iter()
                .chain([self.briefest.as_ref()])
                .flatten()
                .collect();
            // Whether every row shows, beside `lines` lines of notes.
            let all_rows = |lines: usize| {
                let rows = self.rows.len();
                rows == 0 || rows + 1 + own_lines + below_lines + lines <= height
            };
            let third = (height / 3).max(2);
            let notes = (forms.iter())
                .find_map(|notes| {
                    fit(notes).filter(|lines| all_rows(lines.len()) || lines.len() <= third)
                })
                .or_else(|| forms.iter().find_map(|notes| fit(notes)))
                .unwrap_or_else(|| {
                    let shortest = forms.last().expect("the notes in full are a form");
                    keep_lines(wrapped(shortest), most, width)
                });
            let room = room.saturating_sub(notes.len());
            let own = keep_blocks(own, room, width);
            let room = room - own.len();
            let below = keep_blocks(below, room, width);
            let mut above = keep_blocks(blocks(&self.context, width), room - below.len(), width);
            above.extend(own);
            self.laid_out = Some(LaidOut {
                size,
                above,
                notes,
                below,
            });
        }
    }

    /// The width of each column but the last on a screen `width` columns
    /// wide, and what is left for the last, after the cursor's "> " and
    /// two spaces after each column. When the last column has less room
    /// than its widest cell, up to [`LAST_WIDTH`] columns, the column of
    /// words gives way, cut down to [`WORDS_WIDTH`] columns, and further,
    /// down to one, when the last still has fewer than [`NAME_WIDTH`]: the
    /// names come first, since they are what the rows open, then what the
    /// rows say after them, then the words.
    fn columns(&self, width: usize) -> (Vec<usize>, usize) {
        let mut widths = self.widths.clone();
        let used: usize = widths.iter().map(|width| width + 2).sum();
        // Less than nothing when the other columns don't fit.
        let mut room = width as isize - 2 - used as isize;
        if let Some(words) = self.words {
            for (wanted, fewest) in [(LAST_WIDTH, WORDS_WIDTH), (NAME_WIDTH, 1)] {
                let wanted = self.widest_last.min(wanted) as isize;
                let can_give = widths[words].saturating_sub(fewest) as isize;
                let give = (wanted - room).clamp(0, can_give);
                widths[words] -= give as usize;
                room += give;
            }
        }
        (widths, room.max(0) as usize)
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if self.cursor.handle(action) {
            return None;
        }
        match action {
            Action::NextLink | Action::PreviousLink => {
                let forward = action == Action::NextLink;
                let rows = &self.rows;
                if let Some(row) = self.cursor.next(forward, |row| rows[row].link.is_some()) {
                    self.cursor.select(row);
                }
                None
            }
            Action::Open => {
                let (node, at) = self.rows.get(self.cursor.selected)?.link?;
                // The page's own row, as the first of what it reaches.
                if Some(node) == self.page && session.page_node(node) == node {
                    let name = node.name(session.binary);
                    return Some(Go::Say(format!("this is {name}'s page")));
                }
                Some(follow(session, node, at, self.page, self.tab))
            }
            _ => None,
        }
    }

    /// What the selected row opens, in full: "→ app_tick  function at
    /// 0x300002e4, 60 bytes, in .text".
    pub fn preview(&self, session: &Session) -> Option<String> {
        let (node, _) = self.rows.get(self.cursor.selected)?.link?;
        Some(link_preview(session, node, self.page, None))
    }

    /// Whether some row opens an item.
    pub fn has_links(&self) -> bool {
        self.rows.iter().any(|row| row.link.is_some())
    }

    pub fn cursor(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// The rows that have `lowercase` in a cell, or in the last one's name
    /// in full or what follows it, ignoring case.
    pub fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        let has = |text: &str| has_ignoring_case(text, lowercase);
        (self.rows.iter().enumerate())
            .filter(|(_, row)| {
                row.cells.iter().any(|cell| has(cell)) || has(&row.name) || has(&row.rest)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// `lines`, wrapped to `width`, as blocks of lines: each with whether it
/// is words, which can be cut where a line ends, or a table, whose lines
/// can't be joined.
fn blocks(lines: &[Lines], width: usize) -> Vec<(bool, Vec<String>)> {
    (lines.iter())
        .map(|line| match line {
            Lines::Words(words) => (true, wrap(words, width)),
            Lines::Table(lines) => {
                let lines = lines.iter().map(|line| cut(&format!("  {line}"), width));
                (false, lines.collect())
            }
        })
        .collect()
}

/// `name` as a note that names only the first item of a list shows it,
/// on a screen with little room for the notes.
pub fn note_name(name: &str) -> String {
    fit_name(name, NOTE_NAME_WIDTH)
}

/// At most `most` lines of `blocks`, the first blocks that fit. When a
/// block doesn't fit, it and the blocks after it are left out, and the
/// last line kept ends in "…", or where none is, a line of "…" takes their
/// place; words are cut where the lines end, but a table goes whole, so
/// that what is kept of it can't read as all of it.
fn keep_blocks(blocks: Vec<(bool, Vec<String>)>, most: usize, width: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for (words, lines) in blocks {
        let left = most - kept.len();
        if lines.len() <= left {
            kept.extend(lines);
            continue;
        }
        if words && left > 0 {
            kept.extend(keep_lines(lines, left, width));
        } else if let Some(last) = kept.last_mut() {
            *last = cut(&format!("{last} …"), width);
        } else if left > 0 {
            kept.push("…".to_string());
        }
        break;
    }
    kept
}

/// The lines of `table`, as the commands write it.
pub fn table_lines(table: &crate::render::Table) -> Vec<String> {
    let mut out = Vec::new();
    table.write(&mut out).expect("writing to memory");
    String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
impl Lines {
    /// `lines` as their commands print them, one to a string.
    fn printed(lines: &[Lines]) -> Vec<String> {
        (lines.iter())
            .flat_map(|line| match line {
                Lines::Words(line) => vec![line.clone()],
                Lines::Table(lines) => lines.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
impl Printed {
    /// Its lines as the command prints them, without the empty lines: the
    /// lines before the table, the table, with every name in full, and the
    /// notes.
    pub fn printed(&self) -> Vec<String> {
        let mut lines = Lines::printed(&self.context);
        lines.extend(Lines::printed(&self.above));
        if !self.rows.is_empty() {
            let mut table = crate::render::Table::new(&self.columns);
            for row in &self.rows {
                let mut cells = row.cells.clone();
                cells.push(format!("{}{}", row.name, row.rest));
                table.row(cells);
            }
            lines.extend(table_lines(&table));
        }
        lines.extend(self.notes.iter().cloned());
        lines
    }

    /// The lines under the notes, as their commands print them.
    pub fn below(&self) -> Vec<String> {
        Lines::printed(&self.below)
    }

    /// The names that the rows' last cells start with.
    pub fn names(&self) -> Vec<&str> {
        self.rows.iter().map(|row| row.name.as_str()).collect()
    }

    /// What is left for the last column on a screen `width` columns wide.
    pub fn name_room(&self, width: usize) -> usize {
        self.columns(width).1
    }

    /// What it draws on a screen `width` columns wide and `height` lines
    /// high, as lines without the spaces at their ends.
    pub fn drawn(&mut self, (width, height): (u16, u16)) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| self.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..height)
            .map(|row| {
                let line: String = (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect();
                line.trim_end().to_string()
            })
            .collect();
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str], name: &str, rest: &str) -> PrintedRow {
        PrintedRow {
            cells: cells.iter().map(|cell| cell.to_string()).collect(),
            name: name.to_string(),
            rest: rest.to_string(),
            under: None,
            link: None,
        }
    }

    /// A table of starts, as on the start page.
    fn starts(above: Vec<Lines>, notes: &[&str]) -> Printed {
        let columns = [
            ("CODE", Align::Right),
            ("WHY", Align::Left),
            ("START", Align::Left),
        ];
        let rows = vec![
            row(&["2218", "main function"], "main", " (may reach more)"),
            row(
                &["2256", "entry point"],
                "Reset_Handler",
                " (may reach more)",
            ),
            row(&["80", "NMI, HardFault and 6 more"], "Default_Handler", ""),
        ];
        Printed::new(
            above,
            columns
                .map(|(header, align)| (header.to_string(), align))
                .into(),
            rows,
            notes.iter().map(|note| note.to_string()).collect(),
            String::new(),
        )
    }

    #[test]
    fn notes_take_every_line_they_need_when_they_fit() {
        let note = "elfq can't follow 3 calls through pointers, which this note names \
                    in full, however many lines that takes on a short screen.";
        let mut printed = starts(Vec::new(), &[note]);
        assert_eq!(
            printed.drawn((60, 7)),
            "  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
    80  NMI, HardFault and …  Default_Handler
elfq can't follow 3 calls through pointers, which this note
names in full, however many lines that takes on a short
screen."
        );
        // On a shorter screen, the table keeps its header and two rows.
        assert_eq!(
            printed.drawn((60, 5)),
            "  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
elfq can't follow 3 calls through pointers, which this note
names in full, however many lines that takes on a short scr…"
        );
    }

    #[test]
    fn notes_that_dont_fit_are_as_the_command_words_them_without_all() {
        let handlers: Vec<String> = (0..40).map(|n| format!("handler_{n}")).collect();
        let note = format!(
            "{} also call through pointers, which elfq can't follow.",
            handlers.join(", ")
        );
        let brief = "handler_0, handler_1 and 38 more also call through pointers, \
                     which elfq can't follow.";
        let mut printed = starts(Vec::new(), &[&note]).with_brief(vec![brief.to_string()]);
        assert_eq!(
            printed.drawn((60, 7)),
            "  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
    80  NMI, HardFault and …  Default_Handler

handler_0, handler_1 and 38 more also call through pointers,
which elfq can't follow."
        );
        // With room for it, the note is in full.
        let tall = printed.drawn((60, 16));
        let end = "handler_39\nalso call through pointers, which elfq can't follow.";
        assert!(tall.ends_with(end), "{tall}");
    }

    #[test]
    fn names_come_before_what_follows_them_and_the_words() {
        let mut printed = starts(Vec::new(), &[]);
        assert_eq!(
            printed.drawn((62, 4)),
            "  CODE  WHY                     START
> 2218  main function           main (may reach more)
  2256  entry point             Reset_Handler (may reach more)
    80  NMI, HardFault and 6 …  Default_Handler"
        );
        assert_eq!(
            printed.drawn((52, 4)),
            "  CODE  WHY           START
> 2218  main functi…  main (may reach more)
  2256  entry point   Reset_Handler (may reach more)
    80  NMI, HardFa…  Default_Handler"
        );
        assert_eq!(
            printed.drawn((36, 4)),
            "  CODE  WHY         START
> 2218  main func…  main (may reach…
  2256  entry poi…  Reset_Handler (…
    80  NMI, Hard…  Default_Handler"
        );
    }

    #[test]
    fn the_other_commands_lines_give_way_first() {
        let memory = ["MEMORY  SIZE", "0x0      224", "0x2000  1104"];
        let overview = vec![
            Lines::Words("app.elf: 32-bit Arm executable".to_string()),
            Lines::Table(memory.map(str::to_string).into()),
        ];
        let lead = vec![Lines::Words("Where app.elf starts:".to_string())];
        let mut printed = starts(lead, &["A note."]).with_context(overview);
        assert_eq!(
            printed.drawn((60, 11)),
            "\
app.elf: 32-bit Arm executable
  MEMORY  SIZE
  0x0      224
  0x2000  1104
Where app.elf starts:
  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
    80  NMI, HardFault and …  Default_Handler

A note."
        );
        assert_eq!(
            printed.drawn((60, 9)),
            "\
app.elf: 32-bit Arm executable
  MEMORY  SIZE
  0x0      224
  0x2000  1104
Where app.elf starts:
  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
A note."
        );
        assert_eq!(
            printed.drawn((60, 6)),
            "\
app.elf: 32-bit Arm executable …
Where app.elf starts:
  CODE  WHY                   START
> 2218  main function         main (may reach more)
  2256  entry point           Reset_Handler (may reach more)
A note."
        );
    }

    #[test]
    fn notes_that_would_hide_rows_take_a_form_that_needs_a_third_of_the_lines() {
        let rows = (0..12)
            .map(|n| row(&[&n.to_string()], &format!("handler_{n}"), ""))
            .collect();
        let columns = [("SIZE", Align::Right), ("NAME", Align::Left)]
            .map(|(header, align)| (header.to_string(), align));
        let note = |names: &str| vec![format!("{names} also call through pointers.")];
        let every: Vec<String> = (0..40).map(|n| format!("handler_{n}")).collect();
        let mut printed = Printed::new(
            Vec::new(),
            columns.into(),
            rows,
            note(&every.join(", ")),
            String::new(),
        )
        .with_brief(note(&format!("{} and 30 more", every[..10].join(", "))));
        printed.briefest = Some(note("handler_0 and 39 more"));
        // With room for every row and the notes in full.
        let screen = printed.drawn((40, 30));
        let end = "handler_36, handler_37, handler_38,\nhandler_39 also call through pointers.";
        assert!(screen.ends_with(end), "{screen}");
        // Without, the notes take a third of the lines at most, as the
        // command words them without --all, or failing that, naming only
        // the first.
        let screen = printed.drawn((40, 20));
        let end = "handler_9 and 30 more also call through\npointers.";
        assert!(
            screen.ends_with(end) && screen.contains("handler_11"),
            "{screen}"
        );
        let screen = printed.drawn((40, 8));
        let end = "handler_4\nhandler_0 and 39 more also call through\npointers.";
        assert!(screen.ends_with(end), "{screen}");
    }

    #[test]
    fn the_name_that_a_row_hangs_under_is_shortened_as_names_are() {
        let name = "small_sort_general::<Reading, sort_unstable_by_key<Reverse<u64>, \
                    sort_demo::{closure#3}>::{closure#0}>";
        let rest = format!(", kept by {name}");
        let rows = vec![
            row(&["1218"], "small_sort_general::<…>", ""),
            PrintedRow {
                under: Some(", kept by ".len()),
                ..row(&["1218"], "sort8_stable::<…>", &rest)
            },
        ];
        let columns = [("RETAINED", Align::Right), ("ITEM", Align::Left)]
            .map(|(header, align)| (header.to_string(), align));
        let mut printed = Printed::new(Vec::new(), columns.into(), rows, Vec::new(), String::new());
        assert_eq!(
            printed.drawn((64, 3)),
            "  RETAINED  ITEM
>     1218  small_sort_general::<…>
      1218  sort8_stable::<…>, kept by small_sort_general::<…>"
        );
    }
}
