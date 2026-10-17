//! `elfq FILE inlined`: the functions that the compiler inlined, ranked by
//! the bytes their copies take across the file. `elfq FILE inlined SYMBOL`
//! shows the inlined code in one function, as a tree.
//!
//! Only DWARF says what was inlined: in the machine code, an inlined copy is
//! just part of the function that holds it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use crate::binary::{Binary, ItemKind};
use crate::cli::Format;
use crate::debuginfo::{self, InlinedCopy};
use crate::graph::Node;
use crate::render::{Align, Table, TreeRow};

pub fn run(
    path: &Path,
    binary: &Binary,
    symbol: Option<&str>,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    if !binary.has_dwarf {
        bail!(
            "{} has no DWARF debug info, which elfq needs to tell what the compiler inlined; \
             build it with debug info (such as -g) and don't strip it",
            path.display()
        );
    }
    let copies = debuginfo::inlined_copies(binary)?;
    match symbol {
        None => write_list(binary, &copies, all, format, out),
        Some(symbol) => write_tree(binary, &copies, symbol, all, format, out),
    }
}

/// The list as JSON: every inlined function, not only the largest.
#[derive(Serialize)]
struct ListJson<'a> {
    /// The bytes of code that inlined copies take in all.
    inlined_bytes: u64,
    /// The bytes of all the functions in the file.
    code_bytes: u64,
    functions: Vec<Total<'a>>,
}

/// One inlined function's copies, added up.
#[derive(Serialize)]
struct Total<'a> {
    name: &'a str,
    /// The bytes the copies take, not counting the functions inlined into
    /// them, which have totals of their own.
    bytes: u64,
    copies: usize,
}

/// Writes the inlined functions whose copies take the most bytes.
fn write_list(
    binary: &Binary,
    copies: &[InlinedCopy],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let mut by_name: BTreeMap<&str, Total> = BTreeMap::new();
    for copy in copies {
        let total = by_name.entry(&copy.name).or_insert(Total {
            name: &copy.name,
            bytes: 0,
            copies: 0,
        });
        total.bytes += copy.own;
        total.copies += 1;
    }
    let mut totals: Vec<Total> = by_name.into_values().collect();
    totals.sort_by_key(|total| (Reverse(total.bytes), Reverse(total.copies), total.name));

    let inlined: u64 = copies.iter().map(|copy| copy.own).sum();
    let functions = binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function);
    let code = super::total(functions.map(|item| item.size));
    if format == Format::Json {
        let list = ListJson {
            inlined_bytes: inlined,
            code_bytes: code,
            functions: totals,
        };
        return json::write(&list, out);
    }
    if copies.is_empty() {
        writeln!(out, "The debug info lists no inlined code.")?;
        return Ok(());
    }
    writeln!(
        out,
        "Inlined copies of {} take {inlined} of the {} of code:",
        super::plural(totals.len(), "function", "functions"),
        super::bytes_text(code)
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    let (shown, hidden) = totals.split_at(super::shown_count(totals.len(), super::shown(all)));
    for total in shown {
        table.row(vec![
            total.bytes.to_string(),
            total.copies.to_string(),
            total.name.to_string(),
        ]);
    }
    if !hidden.is_empty() {
        let bytes: u64 = hidden.iter().map(|total| total.bytes).sum();
        let copies: usize = hidden.iter().map(|total| total.copies).sum();
        table.row(vec![
            bytes.to_string(),
            copies.to_string(),
            format!("{} more", hidden.len()),
        ]);
    }
    table.write(out)?;
    if shown.iter().any(|total| total.bytes == 0) {
        write_empty_copies_note(out)?;
    }
    Ok(())
}

/// Writes the tree of the inlined code in SYMBOL's function.
fn write_tree(
    binary: &Binary,
    copies: &[InlinedCopy],
    symbol: &str,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);
    let Node::Item(index) = node else {
        bail!("{name} is imported from a shared library, so its code isn't in this file");
    };
    let item = &binary.items[index];
    if item.kind != ItemKind::Function {
        bail!("{name} is a data object, not a function, so nothing is inlined into it");
    }
    let groups = Groups::new(copies, index);
    let bytes: u64 = groups
        .top
        .iter()
        .map(|&group| groups.list[group].bytes)
        .sum();
    if format == Format::Json {
        let tree = TreeJson {
            function: json::Item::new(binary, node),
            inlined_bytes: bytes,
            inlined: groups.to_json(&groups.top),
        };
        return json::write(&tree, out);
    }
    if groups.top.is_empty() {
        writeln!(out, "The debug info lists no inlined code in {name}.")?;
        return Ok(());
    }

    let functions: BTreeSet<&str> = groups.list.iter().map(|group| group.name).collect();
    let functions = super::plural(functions.len(), "function", "functions");
    let are = if bytes == 1 { "is" } else { "are" };
    // The copies' code can reach past the function's symbol, into a part
    // that the compiler moved away from the rest, such as `name.cold`.
    if bytes <= item.size {
        writeln!(
            out,
            "Of {name}'s {}, {bytes} {are} inlined copies of {functions}:",
            super::bytes_text(item.size)
        )?;
    } else {
        writeln!(
            out,
            "{} of {name}'s code {are} inlined copies of {functions}:",
            super::bytes_text(bytes)
        )?;
    }
    writeln!(out)?;
    let shown = groups.largest(super::shown(all));
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    table.tree(&groups.rows(&shown, &groups.top));
    table.write(out)?;
    if shown.iter().any(|&group| groups.list[group].bytes == 0) {
        write_empty_copies_note(out)?;
    }
    Ok(())
}

fn write_empty_copies_note(out: &mut dyn Write) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "A copy of 0 bytes is one the compiler merged into the code around it, so the debug \
         info gives it no bytes of its own."
    )?;
    Ok(())
}

/// The tree as JSON: every group, not only the largest.
#[derive(Serialize)]
struct TreeJson<'a> {
    function: json::Item<'a>,
    /// The bytes of the function's code that inlined copies take.
    inlined_bytes: u64,
    inlined: Vec<GroupJson<'a>>,
}

#[derive(Serialize)]
struct GroupJson<'a> {
    name: &'a str,
    /// The bytes the copies take, including the copies inlined into them.
    bytes: u64,
    copies: usize,
    /// The functions inlined into these copies.
    inlined: Vec<GroupJson<'a>>,
}

/// The inlined copies in one function, as a tree in which the copies of
/// the same function in the same place, such as the six calls to a helper
/// from one function, are merged into one group.
struct Groups<'a> {
    list: Vec<Group<'a>>,
    /// The groups inlined into the function itself, largest first.
    top: Vec<usize>,
}

struct Group<'a> {
    name: &'a str,
    /// The bytes the copies take, including the copies inlined into them.
    bytes: u64,
    copies: usize,
    parent: Option<usize>,
    /// The groups inlined into these copies, largest first.
    children: Vec<usize>,
}

impl<'a> Groups<'a> {
    /// Groups the copies held by the item at `into`.
    fn new(copies: &'a [InlinedCopy], into: usize) -> Groups<'a> {
        let mut groups = Groups {
            list: Vec::new(),
            top: Vec::new(),
        };
        // The group each copy went into. Copies come before the copies
        // inlined into them, so a copy's parent is always grouped first.
        let mut group_of: HashMap<usize, usize> = HashMap::new();
        for (index, copy) in copies.iter().enumerate() {
            if copy.into != into {
                continue;
            }
            let parent = copy
                .parent
                .and_then(|parent| group_of.get(&parent).copied());
            let siblings = match parent {
                Some(parent) => &groups.list[parent].children,
                None => &groups.top,
            };
            let same = siblings
                .iter()
                .copied()
                .find(|&group| groups.list[group].name == copy.name);
            let group = same.unwrap_or_else(|| groups.add(&copy.name, parent));
            groups.list[group].bytes += copy.size;
            groups.list[group].copies += 1;
            group_of.insert(index, group);
        }
        let by_size = |list: &[Group], siblings: &mut Vec<usize>| {
            siblings.sort_by_key(|&group| (Reverse(list[group].bytes), list[group].name));
        };
        by_size(&groups.list, &mut groups.top);
        for group in 0..groups.list.len() {
            let mut children = std::mem::take(&mut groups.list[group].children);
            by_size(&groups.list, &mut children);
            groups.list[group].children = children;
        }
        groups
    }

    fn add(&mut self, name: &'a str, parent: Option<usize>) -> usize {
        let group = self.list.len();
        self.list.push(Group {
            name,
            bytes: 0,
            copies: 0,
            parent,
            children: Vec::new(),
        });
        match parent {
            Some(parent) => self.list[parent].children.push(group),
            None => self.top.push(group),
        }
        group
    }

    /// The groups to show of a large tree: the `count` largest, and every
    /// group between each of them and the top, so that the tree holds
    /// together. A group that would be left out on its own, with nothing
    /// inlined into it, is shown too, since its row takes no more room than
    /// "1 more" would.
    fn largest(&self, count: usize) -> HashSet<usize> {
        let mut by_size: Vec<usize> = (0..self.list.len()).collect();
        by_size.sort_by_key(|&group| (Reverse(self.list[group].bytes), group));
        let mut shown = HashSet::new();
        for &group in by_size.iter().take(count) {
            let mut group = Some(group);
            while let Some(index) = group {
                if !shown.insert(index) {
                    break;
                }
                group = self.list[index].parent;
            }
        }
        let lone: Vec<usize> = shown
            .iter()
            .map(|&group| &self.list[group].children)
            .chain([&self.top])
            .filter_map(|groups| self.lone(groups, &shown))
            .collect();
        shown.extend(lone);
        shown
    }

    /// The one group in `groups` that isn't in `shown`, if there is only
    /// one and nothing is inlined into it.
    fn lone(&self, groups: &[usize], shown: &HashSet<usize>) -> Option<usize> {
        let mut hidden = groups.iter().filter(|group| !shown.contains(group));
        match (hidden.next(), hidden.next()) {
            (Some(&lone), None) if self.list[lone].children.is_empty() => Some(lone),
            _ => None,
        }
    }

    fn to_json(&self, groups: &[usize]) -> Vec<GroupJson<'a>> {
        groups
            .iter()
            .map(|&group| {
                let group = &self.list[group];
                GroupJson {
                    name: group.name,
                    bytes: group.bytes,
                    copies: group.copies,
                    inlined: self.to_json(&group.children),
                }
            })
            .collect()
    }

    /// The rows of the shown groups in `groups`, each with the rows of its
    /// shown children under it, and one row that sums up those not shown
    /// and the groups inlined into them.
    fn rows(&self, shown: &HashSet<usize>, groups: &[usize]) -> Vec<TreeRow> {
        let (visible, hidden): (Vec<usize>, Vec<usize>) =
            groups.iter().partition(|group| shown.contains(group));
        let mut rows: Vec<TreeRow> = visible
            .iter()
            .map(|&group| self.tree_row(shown, group))
            .collect();
        rows.extend(self.more(&hidden));
        rows
    }

    /// The row of `group`, with the rows of its shown children under it.
    fn tree_row(&self, shown: &HashSet<usize>, group: usize) -> TreeRow {
        let row = &self.list[group];
        let cells = vec![row.bytes.to_string(), row.copies.to_string()];
        TreeRow::new(cells, row.name, self.rows(shown, &row.children))
    }

    /// The groups in `groups` and all the groups inlined into them.
    fn with_inlined(&self, groups: &[usize]) -> Vec<usize> {
        groups
            .iter()
            .flat_map(|&group| {
                std::iter::once(group).chain(self.with_inlined(&self.list[group].children))
            })
            .collect()
    }

    /// The row that sums up `hidden` groups and the groups inlined into
    /// them, if there are any.
    fn more(&self, hidden: &[usize]) -> Option<TreeRow> {
        if hidden.is_empty() {
            return None;
        }
        // A group's bytes include those inlined into it, but its copies
        // don't.
        let bytes: u64 = hidden.iter().map(|&group| self.list[group].bytes).sum();
        let all = self.with_inlined(hidden);
        let copies: usize = all.iter().map(|&group| self.list[group].copies).sum();
        let cells = vec![bytes.to_string(), copies.to_string()];
        Some(TreeRow::new(
            cells,
            format!("{} more", all.len()),
            Vec::new(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(name: &str, size: u64, parent: Option<usize>) -> InlinedCopy {
        InlinedCopy {
            name: name.to_string(),
            into: 0,
            size,
            own: size,
            parent,
        }
    }

    /// Each row as "NAME BYTES COPIES", indented two spaces a level.
    fn lines(rows: &[TreeRow], depth: usize) -> Vec<String> {
        rows.iter()
            .flat_map(|row| {
                let line = format!("{}{} {}", "  ".repeat(depth), row.name, row.cells.join(" "));
                std::iter::once(line).chain(lines(&row.children, depth + 1))
            })
            .collect()
    }

    #[test]
    fn a_summary_counts_every_group_it_stands_for() {
        let copies = [
            copy("big", 200, None),
            copy("parent", 100, None),
            copy("child_a", 60, Some(1)),
            copy("child_a", 0, Some(1)),
            copy("child_b", 30, Some(1)),
            copy("leaf", 5, None),
        ];
        let groups = Groups::new(&copies, 0);
        // The last line stands for parent, the two groups inlined into it,
        // and leaf. Its bytes are parent's and leaf's, since parent's
        // include child_a's and child_b's, and its copies are all five.
        let rows = groups.rows(&groups.largest(1), &groups.top);
        assert_eq!(lines(&rows, 0), ["big 200 1", "4 more 105 5"]);
        // With parent picked too, leaf would be left out on its own, so it
        // is shown instead of "1 more".
        let rows = groups.rows(&groups.largest(2), &groups.top);
        assert_eq!(
            lines(&rows, 0),
            ["big 200 1", "parent 100 1", "  2 more 90 3", "leaf 5 1"]
        );
    }
}
