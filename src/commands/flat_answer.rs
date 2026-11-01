//! The answer of a command whose table is flat, one row to a line, as
//! `callers`, `callees`, `reach` and `inlined` write it, and as a tab of
//! browse shows it. Unlike a [`TreeAnswer`], whose rows hang under one another and
//! whose notes each follow an empty line, it can have a table of the
//! blocks of memory before its rows, and its notes follow one empty line
//! together.
//!
//! [`TreeAnswer`]: super::tree_answer::TreeAnswer

use std::io::Write;

use anyhow::Result;

use crate::graph::Node;
use crate::render::{Align, Table};

/// The text answer of `callers`, `callees`, `reach` or `inlined`, before
/// it is written, which browse shows too: the lines before its tables, the bytes
/// in each block of memory, the table of rows, with the item that each row
/// is about, and the notes under them. An empty line comes before each
/// table, and before the notes when there is a table of rows.
#[derive(Default)]
pub(super) struct FlatAnswer {
    /// The first line, or what the answer says in place of its tables.
    pub lead: Vec<String>,
    pub memory: Option<Table>,
    /// The columns of the table of rows, the last of which names an item.
    pub columns: Vec<(String, Align)>,
    /// Empty when the answer has no table of rows.
    pub rows: Vec<FlatRow>,
    pub notes: Vec<String>,
}

/// A row of [`FlatAnswer`]'s table.
pub(super) struct FlatRow {
    /// Every cell but the last.
    pub cells: Vec<String>,
    /// The last cell: a name, which may be empty, then what the row says
    /// about it, such as " (may reach more)" or ", used by main".
    pub name: String,
    pub rest: String,
    /// The item or import that the row is about, if any, which browse
    /// opens.
    #[cfg_attr(not(feature = "tui"), expect(dead_code))]
    pub node: Option<Node>,
    /// The address in it that browse selects when it opens it, if any:
    /// for a caller, the first place where it uses SYMBOL.
    pub at: Option<u64>,
}

impl FlatRow {
    pub(super) fn new(cells: Vec<String>, name: String, node: Option<Node>) -> FlatRow {
        FlatRow {
            cells,
            name,
            rest: String::new(),
            node,
            at: None,
        }
    }

    /// The same row, saying `rest` about its name, such as ", used by main".
    pub(super) fn with_rest(self, rest: String) -> FlatRow {
        FlatRow { rest, ..self }
    }

    /// Its last cell as the table writes it: the name, then what the row
    /// says about it.
    fn last_cell(&self) -> String {
        format!("{}{}", self.name, self.rest)
    }

    /// Its cells as the table writes them.
    pub(super) fn into_cells(self) -> Vec<String> {
        let last = self.last_cell();
        let mut cells = self.cells;
        cells.push(last);
        cells
    }
}

/// A [`FlatAnswer`] as a tab of browse shows it, with what the command
/// that prints it leaves out without `--all`.
#[cfg(feature = "tui")]
pub(super) struct FlatTab {
    /// The answer with every row and name, as with `--all`.
    pub(super) answer: FlatAnswer,
    /// Its notes as the command words them without `--all`, which name
    /// fewer items.
    pub(super) brief: Vec<String>,
    /// Whether the command needs `--all` to print all of `answer`.
    pub(super) needs_all: bool,
}

#[cfg(feature = "tui")]
impl FlatTab {
    /// The tab of `answer`, which `brief` is as the command writes it
    /// without `--all`.
    pub(super) fn new(answer: FlatAnswer, brief: FlatAnswer) -> FlatTab {
        FlatTab {
            needs_all: brief.written() != answer.written(),
            brief: brief.notes,
            answer,
        }
    }
}

/// Writes a table, or what takes its place in an [`FlatAnswer`].
pub(super) type WriteTable<'a> = dyn Fn(&mut dyn Write) -> Result<()> + 'a;

impl FlatAnswer {
    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        if self.rows.is_empty() {
            return self.write_with(None, out);
        }
        let table = self.table();
        self.write_with(Some(&|out: &mut dyn Write| Ok(table.write(out)?)), out)
    }

    /// Writes it with what `table` writes, if anything, in place of its
    /// table of rows, as `--tree` draws a tree there.
    pub(super) fn write_with(&self, table: Option<&WriteTable>, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if let Some(memory) = &self.memory {
            writeln!(out)?;
            memory.write(out)?;
        }
        if let Some(table) = table {
            writeln!(out)?;
            table(out)?;
            if !self.notes.is_empty() {
                writeln!(out)?;
            }
        }
        for note in &self.notes {
            writeln!(out, "{note}")?;
        }
        Ok(())
    }

    /// What [`FlatAnswer::write`] writes.
    #[cfg(feature = "tui")]
    pub(super) fn written(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out).expect("writing to memory");
        out
    }

    /// How many items and imports its rows are about.
    #[cfg(feature = "tui")]
    pub(super) fn node_count(&self) -> usize {
        let nodes: std::collections::HashSet<Node> =
            self.rows.iter().filter_map(|row| row.node).collect();
        nodes.len()
    }

    /// The table of rows, as the text writes it.
    fn table(&self) -> Table {
        let mut table = Table::new(&self.columns);
        for row in &self.rows {
            let mut cells = row.cells.clone();
            cells.push(row.last_cell());
            table.row(cells);
        }
        table
    }
}
