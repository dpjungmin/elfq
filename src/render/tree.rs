//! Trees drawn in the last column of a [`Table`], the same way in every
//! command.
//!
//! Each level of a tree moves a name 3 columns to the right, so the last
//! name of a chain of calls 20 deep would start 60 columns in, and long
//! names would wrap. So a tree is drawn at most [`DEEPEST`] levels deep.
//! Where it goes deeper, the row on the level above ends with "(continued
//! below)", and the rows under it are drawn after the rest of the table,
//! under a row "NAME (continued)", by the same rule.

use super::Table;

/// The deepest level a row is drawn at. The rows at the top of a tree, or
/// of a part continued below, are on level 0, so no name starts more than
/// 27 columns into the last column.
pub const DEEPEST: usize = 9;

/// A row of a tree, and the rows under it.
pub struct TreeRow {
    /// The cells of every column but the last.
    pub cells: Vec<String>,
    /// What the last column shows after the tree's branches.
    pub name: String,
    /// Words after the name, such as "(through a pointer)".
    pub mark: Option<&'static str>,
    /// The rows under it, in the order they are drawn.
    pub children: Vec<TreeRow>,
}

impl TreeRow {
    /// A row with no mark.
    pub fn new(cells: Vec<String>, name: impl Into<String>, children: Vec<TreeRow>) -> TreeRow {
        TreeRow {
            cells,
            name: name.into(),
            mark: None,
            children,
        }
    }

    fn label(&self) -> String {
        match self.mark {
            Some(mark) => format!("{} {mark}", self.name),
            None => self.name.clone(),
        }
    }
}

impl Table {
    /// Adds the rows of a tree that starts with the rows in `top`, then the
    /// parts of it continued below.
    pub fn tree(&mut self, top: &[TreeRow]) {
        let mut cut = Vec::new();
        for row in top {
            self.tree_row(row, "", "", 0, &mut cut);
        }
        for row in cut {
            self.continued(row);
        }
    }

    /// Adds the rows under `row`, which were cut from the tree above them,
    /// after an empty line and a row with `row`'s name; then the parts cut
    /// from these in turn, so that a deep chain reads from top to bottom.
    fn continued(&mut self, row: &TreeRow) {
        let columns = self.headers.len();
        self.row(vec![String::new(); columns]);
        let mut cells = vec![String::new(); columns - 1];
        cells.push(format!("{} (continued)", row.name));
        self.row(cells);
        let mut cut = Vec::new();
        self.tree_children(row, "", 0, &mut cut);
        for row in cut {
            self.continued(row);
        }
    }

    /// Adds `row` on `level`, and the rows under it. If rows under those
    /// would go deeper than [`DEEPEST`], `row` goes in `cut` instead, to
    /// have the rows under it continued below. Rows with nothing under them
    /// stay in place on the deepest level, so a part continued below is
    /// never a single row. `prefix` draws the branches before `row`'s name,
    /// and `indent` those before the rows under it.
    fn tree_row<'a>(
        &mut self,
        row: &'a TreeRow,
        prefix: &str,
        indent: &str,
        level: usize,
        cut: &mut Vec<&'a TreeRow>,
    ) {
        let mut cells = row.cells.clone();
        let has_grandchildren = row.children.iter().any(|child| !child.children.is_empty());
        if has_grandchildren && level + 2 > DEEPEST {
            cells.push(format!("{prefix}{} (continued below)", row.label()));
            self.row(cells);
            cut.push(row);
            return;
        }
        cells.push(format!("{prefix}{}", row.label()));
        self.row(cells);
        self.tree_children(row, indent, level, cut);
    }

    /// Adds the rows under `row`, which is on `level`.
    fn tree_children<'a>(
        &mut self,
        row: &'a TreeRow,
        indent: &str,
        level: usize,
        cut: &mut Vec<&'a TreeRow>,
    ) {
        for (i, child) in row.children.iter().enumerate() {
            let (branch, below) = if i + 1 == row.children.len() {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            self.tree_row(
                child,
                &format!("{indent}{branch}"),
                &format!("{indent}{below}"),
                level + 1,
                cut,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Align;

    /// A chain of `length` rows numbered from `first`, each under the one
    /// before.
    fn chain(first: usize, length: usize) -> TreeRow {
        let children = if length > 1 {
            vec![chain(first + 1, length - 1)]
        } else {
            Vec::new()
        };
        TreeRow::new(vec![first.to_string()], first.to_string(), children)
    }

    fn text(top: &[TreeRow]) -> String {
        let mut table = Table::new(&[("N", Align::Right), ("ITEM", Align::Left)]);
        table.tree(top);
        let mut out = Vec::new();
        table.write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn rows_reach_the_deepest_level_in_place() {
        let text = text(&[chain(0, DEEPEST + 1)]);
        assert!(!text.contains("continued"), "{text}");
        assert!(
            text.ends_with(&format!("\n9  {}└─ 9\n", " ".repeat(24))),
            "{text}"
        );
    }

    #[test]
    fn deeper_rows_continue_below_in_order() {
        // 0 to 19 is cut at 8 and at 16. A second tree is cut at 108, with
        // both rows under it, and its part comes after both parts of the
        // first.
        let mut second = chain(100, 11);
        let mut row = &mut second;
        for _ in 0..8 {
            row = &mut row.children[0];
        }
        row.mark = Some("(marked)");
        row.children
            .push(TreeRow::new(vec![String::new()], "leaf", Vec::new()));
        assert_eq!(
            text(&[chain(0, 20), second]),
            "  N  ITEM
  0  0
  1  └─ 1
  2     └─ 2
  3        └─ 3
  4           └─ 4
  5              └─ 5
  6                 └─ 6
  7                    └─ 7
  8                       └─ 8 (continued below)
100  100
101  └─ 101
102     └─ 102
103        └─ 103
104           └─ 104
105              └─ 105
106                 └─ 106
107                    └─ 107
108                       └─ 108 (marked) (continued below)

     8 (continued)
  9  └─ 9
 10     └─ 10
 11        └─ 11
 12           └─ 12
 13              └─ 13
 14                 └─ 14
 15                    └─ 15
 16                       └─ 16 (continued below)

     16 (continued)
 17  └─ 17
 18     └─ 18
 19        └─ 19

     108 (continued)
109  ├─ 109
110  │  └─ 110
     └─ leaf
"
        );
    }
}
