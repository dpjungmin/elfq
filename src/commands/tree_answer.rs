//! The answer of a command whose table holds a tree, as `why` and
//! `retained` write it, and as a tab of browse shows it.

use std::io::Write;

use anyhow::Result;

use crate::render::{Align, Table, TreeRow, TreeShape};

/// The text of an answer whose table holds a tree, as `why` and `retained`
/// write it: its first lines, the table, and the notes under it, each
/// after an empty line.
pub(super) struct TreeAnswer {
    /// The first lines, or what the answer says in place of a table.
    pub(super) lead: Vec<String>,
    /// The table's columns, the last of which holds the tree.
    pub(super) columns: Vec<(&'static str, Align)>,
    /// The rows at the top of the tree. Empty when the answer has no table.
    pub(super) rows: Vec<TreeRow>,
    /// How the table shows the tree, which the rows were made for, since
    /// rows find the short names they show only when the tree isn't drawn.
    pub(super) shape: TreeShape,
    pub(super) notes: Vec<String>,
}

impl TreeAnswer {
    /// An answer of sentences, without a table, whose shape doesn't matter.
    pub(super) fn sentences(lead: Vec<String>) -> TreeAnswer {
        TreeAnswer {
            lead,
            columns: Vec::new(),
            rows: Vec::new(),
            shape: TreeShape::Drawn,
            notes: Vec::new(),
        }
    }

    /// What [`TreeAnswer::write`] writes.
    #[cfg(feature = "tui")]
    pub(super) fn written(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out).expect("writing to memory");
        out
    }

    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if !self.rows.is_empty() {
            writeln!(out)?;
            let mut table = Table::new(&self.columns);
            table.tree(&self.rows, self.shape);
            table.write(out)?;
        }
        for note in &self.notes {
            writeln!(out)?;
            writeln!(out, "{note}")?;
        }
        Ok(())
    }
}

/// An answer of `why` or `retained` as a tab of browse shows it.
#[cfg(feature = "tui")]
pub(super) struct TabAnswer {
    /// The answer with every row and name, as with `--all`.
    pub(super) answer: TreeAnswer,
    /// Its notes as the command words them without `--all`, which name
    /// fewer items.
    pub(super) brief: Vec<String>,
    /// Its notes naming only the first item of each list, shortened, for a
    /// screen with little room for them.
    pub(super) briefest: Vec<String>,
    /// Whether the command needs `--all` to print all of `answer`.
    pub(super) cut: bool,
}

#[cfg(feature = "tui")]
impl TabAnswer {
    /// An answer of sentences, the same with `--all` and without.
    pub(super) fn sentences(lead: Vec<String>) -> TabAnswer {
        TabAnswer {
            answer: TreeAnswer::sentences(lead),
            brief: Vec::new(),
            briefest: Vec::new(),
            cut: false,
        }
    }
}
