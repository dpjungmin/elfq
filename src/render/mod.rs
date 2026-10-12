//! Turning answers into text for people. Commands fill a [`Table`] and write
//! it out.

use std::io::{self, Write};

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
    headers: Vec<&'static str>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(columns: &[(&'static str, Align)]) -> Table {
        Table {
            headers: columns.iter().map(|&(header, _)| header).collect(),
            aligns: columns.iter().map(|&(_, align)| align).collect(),
            rows: Vec::new(),
        }
    }

    /// Adds a row. It must have one cell per column.
    pub fn row(&mut self, cells: Vec<String>) {
        assert_eq!(cells.len(), self.headers.len(), "wrong number of cells");
        self.rows.push(cells);
    }

    pub fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let headers: Vec<String> = self.headers.iter().map(|h| h.to_string()).collect();
        for row in std::iter::once(&headers).chain(&self.rows) {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    line.push_str("  ");
                }
                let padding = " ".repeat(widths[column] - cell.chars().count());
                match self.aligns[column] {
                    Align::Left => {
                        line.push_str(cell);
                        line.push_str(&padding);
                    }
                    Align::Right => {
                        line.push_str(&padding);
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
