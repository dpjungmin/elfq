//! `elfq FILE inlined`: the functions that the compiler inlined, ranked by
//! the bytes their copies take across the file. `elfq FILE inlined SYMBOL`
//! shows the inlined code in one function, as rows that name the copy they
//! were inlined into.
//!
//! Only DWARF says what was inlined: in the machine code, an inlined copy is
//! just part of the function that holds it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{json, text};
use crate::binary::{Binary, ItemKind, short_names};
use crate::cli::Format;
use crate::debuginfo::{self, InlinedCopy};
use crate::graph::Node;
use crate::render::{Align, Table, TreeRow, TreeShape};

/// Answers `elfq FILE inlined`, or with `symbol`, `inlined SYMBOL`. Needs
/// DWARF.
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
    // SYMBOL first, since reading the DWARF takes a while in a large file.
    let function = match symbol {
        Some(symbol) => Some(find_function(binary, symbol)?),
        None => None,
    };
    let copies = debuginfo::inlined_copies(binary)
        .with_context(|| format!("cannot read the DWARF debug info in {}", path.display()))?;
    match function {
        None => write_list(binary, &copies, all, format, out),
        Some((index, lead_name)) => {
            write_tree(binary, &copies, index, &lead_name, all, format, out)
        }
    }
}

/// The index of the function that `symbol` names, with its name as the
/// answer's first line gives it (see [`Symbol::lead_name`]), or why nothing
/// is inlined into it.
///
/// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
fn find_function(binary: &Binary, symbol: &str) -> Result<(usize, String)> {
    let symbol = super::symbol::find(binary, symbol)?;
    let index = super::item_in_file(binary, symbol.node)?;
    if binary.items[index].kind != ItemKind::Function {
        bail!(
            "{} is a data object, not a function, so nothing is inlined into it",
            symbol.node.name(binary)
        );
    }
    Ok((index, symbol.lead_name(binary)))
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
        total.bytes = total.bytes.saturating_add(copy.own);
        total.copies += 1;
    }
    let mut totals: Vec<Total> = by_name.into_values().collect();
    totals.sort_by_key(|total| (Reverse(total.bytes), Reverse(total.copies), total.name));

    let inlined = super::total(copies.iter().map(|copy| copy.own));
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
        text::plural(totals.len(), "function", "functions"),
        text::bytes_text(code)
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    let (shown, hidden) = totals.split_at(text::shown_count(totals.len(), text::shown(all)));
    for total in shown {
        table.row(vec![
            total.bytes.to_string(),
            total.copies.to_string(),
            total.name.to_string(),
        ]);
    }
    if !hidden.is_empty() {
        let bytes = super::total(hidden.iter().map(|total| total.bytes));
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

/// Writes the tree of the inlined code in SYMBOL's function, the item at
/// `index`, called `lead_name` in the answer's first line.
fn write_tree(
    binary: &Binary,
    copies: &[InlinedCopy],
    index: usize,
    lead_name: &str,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = Node::Item(index);
    let name = node.name(binary);
    let item = &binary.items[index];
    let groups = Groups::new(copies, index);
    let bytes = super::total(groups.top.iter().map(|&group| groups.list[group].bytes));
    if format == Format::Json {
        let tree = TreeJson {
            function: json::Symbol::new(binary, node),
            inlined_bytes: bytes,
            inlined: groups.to_json(&groups.top),
        };
        return json::write(&tree, out);
    }
    if groups.top.is_empty() {
        writeln!(out, "The debug info lists no inlined code in {lead_name}.")?;
        return Ok(());
    }

    let functions: BTreeSet<&str> = groups.list.iter().map(|group| group.name).collect();
    let functions = text::plural(functions.len(), "function", "functions");
    let are = if bytes == 1 { "is" } else { "are" };
    // "process_frame's code", or "the code of sniprintf (also named
    // snprintf)", since a name with a note can't take "'s".
    let of_function = |what: &str| {
        if lead_name == name {
            format!("{name}'s {what}")
        } else {
            format!("the {what} of {lead_name}")
        }
    };
    // The copies' code can reach past the function's symbol, into a part
    // that the compiler moved away from the rest, such as `name.cold`.
    if bytes <= item.size {
        writeln!(
            out,
            "Of {}, {bytes} {are} inlined copies of {functions}:",
            of_function(&text::bytes_text(item.size))
        )?;
    } else {
        writeln!(
            out,
            "{} of {} {are} inlined copies of {functions}:",
            text::bytes_text(bytes),
            of_function("code")
        )?;
    }
    writeln!(out)?;
    let shown = groups.largest(text::shown(all));
    let short_names = groups.short_names(name);
    let mut table = Table::new(&[
        ("BYTES", Align::Right),
        ("COPIES", Align::Right),
        ("FUNCTION", Align::Left),
    ]);
    let shape = match format {
        Format::Tree => TreeShape::Drawn,
        _ => TreeShape::Rows {
            words: "inlined into",
        },
    };
    table.tree(&groups.rows(&shown, &groups.top, &short_names), shape);
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
    function: json::Symbol<'a>,
    /// The bytes of the function's code that inlined copies take.
    inlined_bytes: u64,
    /// The groups, as the rows of the tree in the text's order.
    inlined: Vec<GroupJson<'a>>,
}

/// One row of the tree: the copies of one function in one place.
#[derive(Serialize)]
struct GroupJson<'a> {
    /// Which row this is, counting from 0.
    id: usize,
    name: &'a str,
    /// The bytes the copies take, including the copies inlined into them.
    bytes: u64,
    copies: usize,
    /// The id of the row of the copies that these were inlined into, or
    /// `null` for copies inlined into the function itself.
    parent: Option<usize>,
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
        // The group of each function in each place: inlined into a group,
        // or into the function itself.
        let mut named: HashMap<(Option<usize>, &str), usize> = HashMap::new();
        for (index, copy) in copies.iter().enumerate() {
            if copy.into != into {
                continue;
            }
            let parent = copy
                .parent
                .and_then(|parent| group_of.get(&parent).copied());
            let group = *named
                .entry((parent, &copy.name))
                .or_insert_with(|| groups.add(&copy.name, parent));
            let bytes = &mut groups.list[group].bytes;
            *bytes = bytes.saturating_add(copy.size);
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

    /// The rows of `groups` and of the groups inlined into them, in the
    /// order of the text's rows: each group, largest first, followed by the
    /// groups inlined into it.
    fn to_json(&self, groups: &[usize]) -> Vec<GroupJson<'a>> {
        let mut rows = Vec::new();
        self.add_json(groups, None, &mut rows);
        rows
    }

    /// Adds the rows of `groups`, which were inlined into the row `parent`,
    /// each followed by the rows of the groups inlined into it in turn.
    fn add_json(&self, groups: &[usize], parent: Option<usize>, rows: &mut Vec<GroupJson<'a>>) {
        for &group in groups {
            let group = &self.list[group];
            let id = rows.len();
            rows.push(GroupJson {
                id,
                name: group.name,
                bytes: group.bytes,
                copies: group.copies,
                parent,
            });
            self.add_json(&group.children, Some(id), rows);
        }
    }

    /// What the rows inlined into each group call it, when the tree is
    /// shown as rows: the shortest name of its function that no other
    /// function in the answer has, `symbol` included (see [`short_names`]).
    /// Where several groups are copies of that function, or `symbol` is that
    /// function too, " in " and a place follow: the nearest group above it
    /// whose function is inlined in one place only, or `symbol`, as in
    /// "fetch_add in update_stats". Where that still leaves groups with rows
    /// under them called the same, the place of each is what the rows call
    /// the group it sits in instead, as in "spec_next in next in write".
    fn short_names(&self, symbol: &str) -> Vec<String> {
        // Damaged debug info can nest copies of a function in copies of it
        // thousands deep, and names as long as that. Past this many names, a
        // group's place is the nearest group above it again, and naming
        // stops after as many rounds.
        const MOST_NAMES: usize = 16;

        let mut groups_named: HashMap<&str, usize> = HashMap::new();
        for group in &self.list {
            *groups_named.entry(group.name).or_default() += 1;
        }
        // The function itself is one more place its own copies can be.
        *groups_named.entry(symbol).or_default() += 1;
        // Each function's names, and how many functions have each name.
        let names: HashMap<&str, Vec<String>> = groups_named
            .keys()
            .map(|&name| {
                let mut names = short_names(name);
                names.push(name.to_string());
                names.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
                (name, names)
            })
            .collect();
        let mut functions_named: HashMap<&str, usize> = HashMap::new();
        for name in names.values().flatten() {
            *functions_named.entry(name).or_default() += 1;
        }
        let shortest: HashMap<&str, &str> = names
            .iter()
            .map(|(&function, names)| {
                let unique = names
                    .iter()
                    .find(|name| functions_named[name.as_str()] == 1);
                (function, unique.map_or(function, String::as_str))
            })
            .collect();

        // The nearest group above each group whose function is inlined in
        // one place only. A group comes after the group it sits in.
        let mut nearest: Vec<Option<usize>> = Vec::with_capacity(self.list.len());
        for group in &self.list {
            let above = group.parent.and_then(|parent| {
                if groups_named[self.list[parent].name] == 1 {
                    Some(parent)
                } else {
                    nearest[parent]
                }
            });
            nearest.push(above);
        }

        // Each group's name and how many names it strings together, named
        // top down, so that the group it sits in is named first. Groups
        // marked in `by_parent` take that group's name as their place, each
        // round marking those still called the same as another.
        let mut by_parent = vec![false; self.list.len()];
        let mut labels: Vec<(String, usize)> = Vec::new();
        for _ in 0..MOST_NAMES {
            labels.clear();
            for (index, group) in self.list.iter().enumerate() {
                let name = shortest[group.name];
                let label = if groups_named[group.name] == 1 {
                    (name.to_string(), 1)
                } else {
                    let (place, names) = match (group.parent, nearest[index]) {
                        (Some(parent), _) if by_parent[index] && labels[parent].1 < MOST_NAMES => {
                            (labels[parent].0.as_str(), labels[parent].1)
                        }
                        (_, Some(above)) => (shortest[self.list[above].name], 1),
                        (_, None) => (shortest[symbol], 1),
                    };
                    (format!("{name} in {place}"), names + 1)
                };
                labels.push(label);
            }

            // Only the names of groups with rows under them are shown.
            let mut called: HashMap<&str, usize> = HashMap::new();
            for (index, (label, _)) in labels.iter().enumerate() {
                if !self.list[index].children.is_empty() {
                    *called.entry(label).or_default() += 1;
                }
            }
            let mut marked = false;
            for (index, group) in self.list.iter().enumerate() {
                if group.parent.is_some()
                    && !group.children.is_empty()
                    && !by_parent[index]
                    && called[labels[index].0.as_str()] > 1
                {
                    by_parent[index] = true;
                    marked = true;
                }
            }
            if !marked {
                break;
            }
        }
        labels.into_iter().map(|(label, _)| label).collect()
    }

    /// The rows of the shown groups in `groups`, each with the rows of its
    /// shown children under it, and one row that sums up those not shown
    /// and the groups inlined into them. `short_names` has what the rows
    /// under each group call it (see [`Groups::short_names`]).
    fn rows(
        &self,
        shown: &HashSet<usize>,
        groups: &[usize],
        short_names: &[String],
    ) -> Vec<TreeRow> {
        let (visible, hidden): (Vec<usize>, Vec<usize>) =
            groups.iter().partition(|group| shown.contains(group));
        let mut rows: Vec<TreeRow> = visible
            .iter()
            .map(|&group| self.tree_row(shown, group, short_names))
            .collect();
        rows.extend(self.more(&hidden));
        rows
    }

    /// The row of `group`, with the rows of its shown children under it.
    fn tree_row(&self, shown: &HashSet<usize>, group: usize, short_names: &[String]) -> TreeRow {
        let row = &self.list[group];
        let cells = vec![row.bytes.to_string(), row.copies.to_string()];
        TreeRow {
            short_name: short_names[group].clone(),
            ..TreeRow::new(
                cells,
                row.name,
                self.rows(shown, &row.children, short_names),
            )
        }
    }

    /// The groups in `groups` and all the groups inlined into them, each
    /// before the groups inlined into it.
    fn with_inlined(&self, groups: &[usize]) -> Vec<usize> {
        let mut all = Vec::new();
        // The groups still to list, the next one last. A loop rather than
        // recursion, since copies can nest deeper than the stack would hold.
        let mut next: Vec<usize> = groups.iter().rev().copied().collect();
        while let Some(group) = next.pop() {
            all.push(group);
            next.extend(self.list[group].children.iter().rev());
        }
        all
    }

    /// The row that sums up `hidden` groups and the groups inlined into
    /// them, if there are any.
    fn more(&self, hidden: &[usize]) -> Option<TreeRow> {
        if hidden.is_empty() {
            return None;
        }
        // A group's bytes include those inlined into it, but its copies
        // don't.
        let bytes = super::total(hidden.iter().map(|&group| self.list[group].bytes));
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
        let short_names = groups.short_names("f");
        let rows = groups.rows(&groups.largest(1), &groups.top, &short_names);
        assert_eq!(lines(&rows, 0), ["big 200 1", "4 more 105 5"]);
        // With parent picked too, leaf would be left out on its own, so it
        // is shown instead of "1 more".
        let rows = groups.rows(&groups.largest(2), &groups.top, &short_names);
        assert_eq!(
            lines(&rows, 0),
            ["big 200 1", "parent 100 1", "  2 more 90 3", "leaf 5 1"]
        );
    }

    #[test]
    fn a_function_inlined_in_several_places_says_where() {
        let copies = [
            copy("app::run", 10, None),
            copy("core::Iter::next", 5, Some(0)),
            copy("app::step", 10, None),
            copy("core::Iter::next", 5, Some(2)),
            copy("core::Zip::next", 1, Some(3)),
            copy("core::Iter::next", 5, None),
        ];
        let groups = Groups::new(&copies, 0);
        // Each by the shortest name that no other function has: `next`
        // would be either `Iter::next` or `Zip::next`.
        let short_names = groups.short_names("main");
        let names: Vec<(&str, &str)> = groups
            .list
            .iter()
            .map(|group| group.name)
            .zip(short_names.iter().map(String::as_str))
            .collect();
        assert_eq!(
            names,
            [
                ("app::run", "run"),
                ("core::Iter::next", "Iter::next in run"),
                ("app::step", "step"),
                ("core::Iter::next", "Iter::next in step"),
                ("core::Zip::next", "Zip::next"),
                ("core::Iter::next", "Iter::next in main"),
            ]
        );
    }

    #[test]
    fn each_copy_with_rows_under_it_has_a_name_of_its_own() {
        let copies = [
            copy("fmt::padding", 10, None),
            copy("core::Range::next", 5, Some(0)),
            copy("core::Range::spec_next", 4, Some(1)),
            copy("core::lt", 1, Some(2)),
            copy("core::Range::step", 5, Some(0)),
            copy("core::Range::spec_next", 4, Some(4)),
            copy("core::lt", 1, Some(5)),
            copy("fmt::write", 10, None),
            copy("core::Range::next", 5, Some(7)),
            copy("core::Range::step", 5, Some(7)),
            // The function inlined into itself.
            copy("app::walk", 3, None),
            copy("app::helper", 2, Some(10)),
            copy("app::mix", 1, Some(11)),
            copy("app::helper", 2, None),
            copy("app::mix", 1, Some(13)),
        ];
        let groups = Groups::new(&copies, 0);
        // Each place is the nearest group above whose function is inlined
        // there only, unless that leaves two groups with rows under them
        // called the same, as the two `spec_next` in `padding` would be.
        // Rows under no group show no name, so theirs may repeat.
        assert_eq!(
            groups.short_names("app::walk"),
            [
                "padding",
                "next in padding",
                "spec_next in next in padding",
                "lt in padding",
                "step in padding",
                "spec_next in step in padding",
                "lt in padding",
                "write",
                "next in write",
                "step in write",
                "walk in walk",
                "helper in walk in walk",
                "mix in walk",
                "helper in walk",
                "mix in walk",
            ]
        );

        // Another function's `run` is not the function's own.
        let copies = [
            copy("b::run", 10, None),
            copy("x::f", 2, Some(0)),
            copy("x::f", 2, None),
        ];
        let groups = Groups::new(&copies, 0);
        assert_eq!(
            groups.short_names("a::run"),
            ["b::run", "f in b::run", "f in a::run"]
        );
    }

    #[test]
    fn a_summary_of_copies_nested_deeply_takes_one_pass() {
        // Each copy inlined into the one before, 100,000 deep: summing them
        // up takes neither a frame of the stack for each level nor time
        // that grows with its square.
        let depth: usize = 100_000;
        let copies: Vec<InlinedCopy> = (0..depth)
            .map(|index| copy("f", 1, index.checked_sub(1)))
            .collect();
        let groups = Groups::new(&copies, 0);
        let row = groups.more(&groups.top).unwrap();
        assert_eq!(lines(&[row], 0), [format!("{depth} more 1 {depth}")]);
    }
}
