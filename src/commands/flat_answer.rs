//! The answer of a command whose table is flat, one row to a line, as
//! `callers`, `callees`, `reach` and `inlined` write it, and as a tab of
//! browse shows it. Unlike a [`TreeAnswer`], whose rows hang under one another and
//! whose notes each follow an empty line, it can have a table of the
//! blocks of memory before its rows, and its notes follow one empty line
//! together. `reach --tree` draws a tree in place of the rows.
//!
//! [`TreeAnswer`]: super::tree_answer::TreeAnswer

use std::io::Write;
use std::slice;

use anyhow::Result;

use crate::graph::Node;
use crate::render::{Align, Table, TreeRow, TreeShape};

/// The text answer of `callers`, `callees`, `reach` or `inlined`, before
/// it is written, which browse shows too: the lines before its tables, the bytes
/// in each block of memory, the table of rows, with the item that each row
/// is about, or a tree in their place, and the notes under them. An empty
/// line comes before each table, and before the notes when there is a
/// table of rows or a tree.
#[derive(Default)]
pub(super) struct FlatAnswer {
    /// The first line, or what the answer says in place of its tables.
    pub(super) lead: Vec<String>,
    pub(super) memory: Option<Table>,
    /// The columns of the table of rows, the last of which names an item.
    pub(super) columns: Vec<(String, Align)>,
    /// Empty when the answer has no table of rows.
    pub(super) rows: Vec<FlatRow>,
    /// The tree drawn with branches in place of the rows, as `reach --tree`
    /// draws it, under the same columns. Browse shows rows, never a tree.
    pub(super) tree: Option<TreeRow>,
    pub(super) notes: Vec<String>,
}

/// A row of [`FlatAnswer`]'s table.
pub(super) struct FlatRow {
    /// Every cell but the last.
    pub(super) cells: Vec<String>,
    /// The last cell: a name, which may be empty, then what the row says
    /// about it, such as " (may reach more)" or ", used by main".
    pub(super) name: String,
    pub(super) rest: String,
    /// The item or import that the row is about, if any, which browse
    /// opens.
    #[cfg_attr(not(feature = "tui"), expect(dead_code))]
    pub(super) node: Option<Node>,
    /// The address in it that browse selects when it opens it, if any:
    /// for a caller, the first place where it uses SYMBOL.
    pub(super) at: Option<u64>,
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

    /// Its cells as the table writes them, the last of which is the name,
    /// then what the row says about it.
    pub(super) fn all_cells(&self) -> Vec<String> {
        let mut cells = self.cells.clone();
        cells.push(format!("{}{}", self.name, self.rest));
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

impl FlatAnswer {
    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if let Some(memory) = &self.memory {
            writeln!(out)?;
            memory.write(out)?;
        }
        if let Some(table) = self.table() {
            writeln!(out)?;
            table.write(out)?;
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

    /// The table of rows, or of the tree, as the text writes it, if the
    /// answer has either.
    fn table(&self) -> Option<Table> {
        let mut table = Table::new(&self.columns);
        match &self.tree {
            Some(tree) => table.tree(slice::from_ref(tree), TreeShape::Drawn),
            None if self.rows.is_empty() => return None,
            None => {
                for row in &self.rows {
                    table.row(row.all_cells());
                }
            }
        }
        Some(table)
    }
}
