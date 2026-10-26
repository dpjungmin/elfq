//! Turning answers into text for people. Commands fill a [`Table`], with
//! rows or with a tree in its last column ([`Table::tree`]), and write it
//! out.

use std::io::{self, Write};

mod tree;

#[cfg(feature = "tui")]
pub use tree::flat_rows;
pub use tree::{TreeRow, TreeShape};

/// How a column's cells line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    /// For numbers, so their digits line up.
    Right,
}

/// A plain-text table: a header row, then one line per row, with columns
/// separated by two spaces.
#[derive(Debug)]
pub struct Table {
    headers: Vec<String>,
    /// How each column's cells line up.
    aligns: Vec<Align>,
    /// The rows added so far, each with one cell per column.
    rows: Vec<Vec<String>>,
}

impl Table {
    /// An empty table with these columns: each a header, and how its cells
    /// line up.
    pub fn new(columns: &[(impl AsRef<str>, Align)]) -> Table {
        Table {
            headers: columns
                .iter()
                .map(|(header, _)| header.as_ref().to_string())
                .collect(),
            aligns: columns.iter().map(|&(_, align)| align).collect(),
            rows: Vec::new(),
        }
    }

    /// Adds a row. It must have one cell per column.
    pub fn row(&mut self, cells: Vec<String>) {
        assert_eq!(cells.len(), self.headers.len(), "wrong number of cells");
        self.rows.push(cells);
    }

    /// Writes the header, then the rows, each column as wide as its widest
    /// cell, with no spaces at the ends of lines.
    pub fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        for row in std::iter::once(&self.headers).chain(&self.rows) {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    line.push_str("  ");
                }
                let padding = std::iter::repeat_n(' ', widths[column] - cell.chars().count());
                match self.aligns[column] {
                    // Nothing follows the last column, so it needs no
                    // padding: the line's trailing spaces are trimmed.
                    Align::Left if column + 1 == row.len() => line.push_str(cell),
                    Align::Left => {
                        line.push_str(cell);
                        line.extend(padding);
                    }
                    Align::Right => {
                        line.extend(padding);
                        line.push_str(cell);
                    }
                }
            }
            writeln!(out, "{}", line.trim_end())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns_without_trailing_spaces() {
        let mut table = Table::new(&[("SIZE", Align::Right), ("NAME", Align::Left)]);
        table.row(vec!["532".into(), "process_frame".into()]);
        table.row(vec!["59".into(), "crc32".into()]);
        let mut out = Vec::new();
        table.write(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "SIZE  NAME\n 532  process_frame\n  59  crc32\n"
        );
    }
}
