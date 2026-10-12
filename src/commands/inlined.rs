//! `elfq FILE inlined`: the functions that the compiler inlined, ranked by
//! the bytes their copies take across the file. With `--in SYMBOL`, the
//! inlined code in one function, as a tree.
//!
//! Only DWARF says what was inlined: in the machine code, an inlined copy is
//! just part of the function that holds it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};

use super::symbol::Names;
use crate::binary::{Binary, ItemKind};
use crate::debuginfo::{self, InlinedCopy};
use crate::graph::Node;
use crate::render::{Align, Table};

/// How many functions the list shows, or the tree, at most, besides the
/// lines that sum up the rest.
const SHOWN: usize = 30;

pub fn run(path: &Path, binary: &Binary, within: Option<&str>, out: &mut dyn Write) -> Result<()> {
    if !binary.has_dwarf {
        bail!(
            "{} has no DWARF debug info, which elfq needs to tell what the compiler inlined; \
             build it with debug info (such as -g) and don't strip it",
            path.display()
        );
    }
    let copies = debuginfo::inlined_copies(binary)?;
    match within {
        None => write_list(path, binary, &copies, out),
        Some(symbol) => write_tree(binary, &copies, symbol, out),
    }
}

/// One inlined function's copies, added up.
struct Total<'a> {
    name: &'a str,
    /// The bytes the copies take, not counting the functions inlined into
    /// them, which have totals of their own.
    bytes: u64,
    copies: usize,
}

/// Writes the inlined functions whose copies take the most bytes.
fn write_list(
    path: &Path,
    binary: &Binary,
    copies: &[InlinedCopy],
    out: &mut dyn Write,
) -> Result<()> {
    if copies.is_empty() {
        writeln!(out, "The debug info lists no inlined code.")?;
        return Ok(());
    }
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
    let code: u64 = binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| item.size)
        .sum();
    writeln!(
        out,
        "Inlined copies of {} take {inlined} of the file's {code} bytes of code:",
        super::plural(totals.len(), "function", "functions")
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    let (shown, hidden) = totals.split_at(totals.len().min(SHOWN));
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

    // The function that holds the most inlined code is the one most worth
    // a closer look.
    let mut held: HashMap<usize, u64> = HashMap::new();
    for copy in copies {
        *held.entry(copy.into).or_default() += copy.own;
    }
    if let Some((&into, _)) = held
        .iter()
        .max_by_key(|&(&into, &bytes)| (bytes, Reverse(into)))
    {
        let symbol = Names::new(binary).query_for(Node::Item(into));
        writeln!(out)?;
        writeln!(
            out,
            "To see the inlined code in one function, as a tree:\n  elfq {} inlined --in {}",
            super::shell_quote(&path.display().to_string()),
            super::shell_quote(&symbol)
        )?;
    }
    Ok(())
}

/// Writes the tree of the inlined code in SYMBOL's function.
fn write_tree(
    binary: &Binary,
    copies: &[InlinedCopy],
    symbol: &str,
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
    if groups.top.is_empty() {
        writeln!(out, "The debug info lists no inlined code in {name}.")?;
        return Ok(());
    }

    let bytes: u64 = groups
        .top
        .iter()
        .map(|&group| groups.list[group].bytes)
        .sum();
    let functions: BTreeSet<&str> = groups.list.iter().map(|group| group.name).collect();
    let functions = super::plural(functions.len(), "function", "functions");
    let are = if bytes == 1 { "is" } else { "are" };
    // The copies' code can reach past the function's symbol, into a part
    // that the compiler moved away from the rest, such as `name.cold`.
    if bytes <= item.size {
        writeln!(
            out,
            "Of {name}'s {} bytes, {bytes} {are} inlined copies of {functions}:",
            item.size
        )?;
    } else {
        writeln!(
            out,
            "{bytes} bytes of {name}'s code are inlined copies of {functions}:"
        )?;
    }
    writeln!(out)?;
    let shown = groups.largest(SHOWN);
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    let (visible, hidden) = split(&groups.top, &shown);
    for &group in &visible {
        groups.add_rows(&shown, group, "", "", &mut table);
    }
    groups.add_more(&hidden, "", &mut table);
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
    /// together.
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
        shown
    }

    /// Adds `group`'s row and, below it, its shown children, with one line
    /// that sums up the children not shown. `prefix` draws the tree's
    /// branches for this row, and `indent` for the rows below it.
    fn add_rows(
        &self,
        shown: &HashSet<usize>,
        group: usize,
        prefix: &str,
        indent: &str,
        table: &mut Table,
    ) {
        let row = &self.list[group];
        table.row(vec![
            row.bytes.to_string(),
            row.copies.to_string(),
            format!("{prefix}{}", row.name),
        ]);
        let (visible, hidden) = split(&row.children, shown);
        let lines = visible.len() + usize::from(!hidden.is_empty());
        for (i, &child) in visible.iter().enumerate() {
            let last = i + 1 == lines;
            self.add_rows(
                shown,
                child,
                &format!("{indent}{}", if last { "└─ " } else { "├─ " }),
                &format!("{indent}{}", if last { "   " } else { "│  " }),
                table,
            );
        }
        self.add_more(&hidden, &format!("{indent}└─ "), table);
    }

    /// Adds the line that sums up `hidden` groups, if there are any.
    fn add_more(&self, hidden: &[usize], prefix: &str, table: &mut Table) {
        if hidden.is_empty() {
            return;
        }
        let bytes: u64 = hidden.iter().map(|&group| self.list[group].bytes).sum();
        let copies: usize = hidden.iter().map(|&group| self.list[group].copies).sum();
        table.row(vec![
            bytes.to_string(),
            copies.to_string(),
            format!("{prefix}{} more", hidden.len()),
        ]);
    }
}

/// Splits `groups` into those shown and those not.
fn split(groups: &[usize], shown: &HashSet<usize>) -> (Vec<usize>, Vec<usize>) {
    groups.iter().partition(|group| shown.contains(group))
}
