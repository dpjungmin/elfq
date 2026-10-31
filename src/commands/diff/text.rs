//! diff's answer as text: a line that says whether the builds load the same
//! memory, and what changed; for several blocks of memory, the MEMORY
//! table, or for one, a line on how its size changed; the MOVED and CHANGE
//! tables of the items that changed; for several blocks, the calls between
//! blocks that are new or gone, and a line that counts the other uses; and
//! when no row says why the memory differs, a line that names the sections
//! that differ.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;

use super::blocks::{Block, SectionChange};
use super::crossings::Crossing;
use super::pairing::{self, is_made_name, same_but_numbers};
use super::rows::{Change, ChangeRow, MovedRow, Status};
use super::{Answer, Build, Compared};
use crate::binary::{FileKind, Item};
use crate::commands::json::address;
use crate::commands::overview::sections_text;
use crate::commands::ranges::{self, Direction};
use crate::commands::symbol::Names;
use crate::commands::text::{
    cannot_read_code, named, names_text, plural, range_argument, range_start, shell_quote,
    shown_count, signed, size_text,
};
use crate::commands::total;
use crate::graph::Node;
use crate::render::{Align, Table};

/// How many sections a cell of SECTIONS, or the line on a block's size,
/// names before "and 5 more", unless `--all` asks for every one.
const CHANGES_NAMED: usize = 3;

pub(super) fn write(answer: &Answer, all: bool, out: &mut dyn Write) -> Result<()> {
    let (old_path, new_path) = (answer.old_path.display(), answer.new_path.display());
    if answer.same_memory {
        writeln!(
            out,
            "{old_path} and {new_path} hold the same bytes at the same addresses."
        )?;
        return Ok(());
    }
    writeln!(out, "From {old_path} to {new_path}: {}", lead(answer))?;
    if answer.items.is_none() {
        let path = match answer.old.has_symbols {
            true => &new_path,
            false => &old_path,
        };
        writeln!(
            out,
            "{path} has no symbol table, so diff compares only bytes, not functions and data."
        )?;
    }
    let blocks = &answer.blocks;
    let table = match &blocks[..] {
        // One block that both builds have gets a line, as the overview
        // gives one block no table.
        [block] if !block.old.is_empty() && !block.new.is_empty() => {
            writeln!(out, "{}", size_line(answer, block, all))?;
            false
        }
        [] => false,
        _ => true,
    };
    let Some(compared) = &answer.items else {
        if table {
            writeln!(out)?;
            memory_table(answer, all).write(out)?;
            writeln!(out)?;
        }
        writeln!(out, "{}", sections_line(answer, all))?;
        return write_try(answer, None, out);
    };
    let items = &compared.items;
    if !items.changes.is_empty()
        && let Some(reason) = no_keepers(compared)
    {
        writeln!(out, "{reason}")?;
    }
    if table {
        writeln!(out)?;
        memory_table(answer, all).write(out)?;
    }
    let heads = items.heads();
    let moved = items.moved_rows(all);
    let changes = items.change_rows(&heads, blocks.len(), all);
    compared.old_names.will_name(moved.len() + changes.len());
    compared.new_names.will_name(moved.len() + changes.len());
    let rows = Rows::new(compared, &changes, all);
    if !moved.is_empty() {
        writeln!(out)?;
        rows.moved_table(&moved).write(out)?;
    }
    if !changes.is_empty() {
        writeln!(out)?;
        rows.change_table(answer, &changes).write(out)?;
    }
    if blocks.len() > 1 {
        writeln!(out)?;
        write_crossings(compared, out)?;
    }
    let mut closing = Vec::new();
    if items.unchanged > 0 {
        closing.push(format!(
            "{} the same.",
            plural(items.unchanged, "other item is", "other items are")
        ));
    }
    if items.changes.is_empty() {
        // No row says which bytes differ.
        closing.push(sections_line(answer, all));
    }
    if !closing.is_empty() {
        writeln!(out)?;
        for line in closing {
            writeln!(out, "{line}")?;
        }
    }
    write_try(answer, try_why(compared, &heads), out)
}

/// The Try list: `why` with `query`, and `startup` when a change may
/// change what the new build's startup code must do.
fn write_try(answer: &Answer, query: Option<String>, out: &mut dyn Write) -> Result<()> {
    let new_path = shell_quote(&answer.new_path.display().to_string());
    let lines: Vec<String> = [
        query.map(|query| format!("why {query}")),
        answer.try_startup.then(|| "startup".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !lines.is_empty() {
        writeln!(out)?;
        writeln!(out, "Try:")?;
        for line in lines {
            writeln!(out, "  elfq {new_path} {line}")?;
        }
    }
    Ok(())
}

/// The calls between blocks that are new or gone, as `callees BLOCK` lists
/// them, with whether each is new or gone in front, and a line that counts
/// the other uses between blocks that are new or gone, of data and of
/// functions' addresses, of which a relink of data to another memory makes
/// many.
fn write_crossings(compared: &Compared, out: &mut dyn Write) -> Result<()> {
    let crossings = &compared.crossings;
    let calls: Vec<&Crossing> = crossings.iter().filter(|c| c.is_call()).collect();
    let listed = !calls.is_empty();
    if listed {
        writeln!(out, "Calls between blocks, new or gone:")?;
        writeln!(out)?;
        let columns = [("STATUS", Align::Left)]
            .into_iter()
            .chain(ranges::crossings_columns(Direction::Out));
        let mut table = Table::new(&columns.collect::<Vec<_>>());
        for crossing in calls {
            let binary = &crossing.build(&compared.items).binary;
            let callee = Node::Item(crossing.callee);
            let mut cells = vec![crossing.status.word().to_string()];
            cells.extend(ranges::row_cells(
                binary,
                callee,
                &crossing.row,
                Direction::Out,
            ));
            table.row(cells);
        }
        table.write(out)?;
    } else {
        writeln!(out, "No call between blocks is new or gone.")?;
    }
    let uses = |status| {
        let count = crossings
            .iter()
            .filter(|c| !c.is_call() && c.status == status)
            .count();
        (count > 0).then(|| format!("{count} {}", status.word()))
    };
    let uses: Vec<String> = [Status::New, Status::Gone]
        .into_iter()
        .filter_map(uses)
        .collect();
    if !uses.is_empty() {
        if listed {
            writeln!(out)?;
        }
        writeln!(
            out,
            "Other uses between blocks: {}; --json lists them.",
            uses.join(" and ")
        )?;
    }
    Ok(())
}

/// What the first line says after "From OLD to NEW: ": how many items
/// moved, are new or gone, grew, shrank or are edited, as "1 moved, 3 new,
/// 1 grew.", or when diff compared no items, in how many blocks the bytes
/// differ.
fn lead(answer: &Answer) -> String {
    let blocks = &answer.blocks;
    let Some(compared) = &answer.items else {
        let differ = blocks.iter().filter(|block| !block.same_bytes).count();
        if blocks.len() > 1 && differ > 0 {
            return format!(
                "the bytes differ in {differ} of {}.",
                plural(blocks.len(), "block", "blocks")
            );
        }
        // In one block, or only in .tbss, which is in none.
        return "the bytes differ.".to_string();
    };
    let changes = &compared.items.changes;
    let counts: Vec<String> = [
        Status::Moved,
        Status::New,
        Status::Gone,
        Status::Grew,
        Status::Shrank,
        Status::Edited,
    ]
    .into_iter()
    .map(|status| {
        (
            changes
                .iter()
                .filter(|change| change.status == status)
                .count(),
            status,
        )
    })
    .filter(|&(count, _)| count > 0)
    .map(|(count, status)| format!("{count} {}", status.word()))
    .collect();
    if !counts.is_empty() {
        format!("{}.", counts.join(", "))
    } else if blocks
        .iter()
        .any(|block| block.old.is_empty() || block.new.is_empty())
    {
        "no item changed, but the blocks of memory differ.".to_string()
    } else {
        "no item changed size or block, but bytes differ.".to_string()
    }
}

/// The line that names the sections that differ, for when no row says why
/// the memory differs: when no item changed, as when only a build ID
/// differs, or a file has no symbol table. The table and the line on a
/// block's size name the sections that changed size, but not those that
/// changed only their bytes or address, nor `.tbss`, which is in no block.
/// A section whose load image alone moved is named as SECTIONS names a load
/// image.
fn sections_line(answer: &Answer, all: bool) -> String {
    let names: Vec<String> = answer
        .sections_that_differ
        .iter()
        .map(|differs| match differs.only_load_image {
            true => format!("load image of {}", differs.name),
            false => differs.name.to_string(),
        })
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    format!("Sections that differ: {}.", names_text(&names, all))
}

/// Why the rows don't say what keeps items, if they don't: elfq can't
/// tell in a shared library, which either build may be, nor in a file
/// whose code it can't read.
fn no_keepers(compared: &Compared) -> Option<String> {
    let builds = [compared.items.old, compared.items.new];
    if builds
        .iter()
        .any(|build| build.binary.kind == FileKind::SharedLibrary)
    {
        return Some(
            "elfq can't yet tell what keeps items in a shared library, so rows don't say."
                .to_string(),
        );
    }
    let build = builds.into_iter().find(|build| build.tree.is_none())?;
    Some(format!(
        "{}, so rows don't say what keeps items.",
        cannot_read_code(&build.binary)
    ))
}

/// What the rows of MOVED and CHANGE say of the items that changed.
struct Rows<'c, 'a> {
    compared: &'c Compared<'a>,
    /// Where the compiler inlined the gone functions that CHANGE shows (see
    /// [`Items::inlined_into`](super::rows::Items::inlined_into)).
    inlined_into: HashMap<usize, Vec<usize>>,
    old: RowNames<'c, 'a>,
    new: RowNames<'c, 'a>,
    all: bool,
}

/// A build, with the names that rows give its items, each worked out once:
/// `--all` names the same items that keep others, and the same functions
/// that gone functions were inlined into, many times.
struct RowNames<'c, 'a> {
    build: &'a Build,
    names: &'c Names<'a>,
    known: RefCell<HashMap<usize, String>>,
}

impl<'c, 'a> Rows<'c, 'a> {
    /// The rows for `compared`, with the CHANGE table's `changes`.
    fn new(compared: &'c Compared<'a>, changes: &[ChangeRow], all: bool) -> Rows<'c, 'a> {
        let items = &compared.items;
        let gone = changes.iter().filter_map(|row| match row {
            ChangeRow::Item { change, .. } if items.changes[*change].status == Status::Gone => {
                items.changes[*change].old
            }
            _ => None,
        });
        let row_names = |build, names| RowNames {
            build,
            names,
            known: RefCell::default(),
        };
        Rows {
            compared,
            inlined_into: items.inlined_into(&compared.new_names, gone),
            old: row_names(items.old, &compared.old_names),
            new: row_names(items.new, &compared.new_names),
            all,
        }
    }

    /// The MOVED table: a row for each item that moved to another block,
    /// or for all the items of whole sections.
    fn moved_table(&self, rows: &[MovedRow]) -> Table {
        let items = &self.compared.items;
        let mut table = Table::new(&[
            ("OLD", Align::Right),
            ("NEW", Align::Right),
            ("FROM", Align::Left),
            ("TO", Align::Left),
            ("MOVED", Align::Left),
        ]);
        for row in rows {
            // The row's items in each build.
            let (mut old_items, mut new_items): (Vec<&Item>, Vec<&Item>) = (Vec::new(), Vec::new());
            for &at in row.changes() {
                let change = &items.changes[at];
                old_items.extend(change.old.map(|index| &items.old.binary.items[index]));
                new_items.extend(change.new.map(|index| &items.new.binary.items[index]));
            }
            let ([old_size, new_size], text) = match row {
                MovedRow::Item(at) => {
                    let size = |items: &[&Item]| {
                        items
                            .first()
                            .map_or_else(String::new, |item| size_text(item))
                    };
                    (
                        [size(&old_items), size(&new_items)],
                        self.item_text(&items.changes[*at], 0),
                    )
                }
                MovedRow::Sections { changes, sections } => {
                    let sum =
                        |items: &[&Item]| total(items.iter().map(|item| item.size)).to_string();
                    let sections: Vec<&str> = sections.iter().map(String::as_str).collect();
                    (
                        [sum(&old_items), sum(&new_items)],
                        format!(
                            "all {} items of {}",
                            changes.len(),
                            names_text(&sections, false)
                        ),
                    )
                }
            };
            let start = |items: &[&Item]| {
                items
                    .iter()
                    .map(|item| item.address)
                    .min()
                    .map(address)
                    .unwrap_or_default()
            };
            table.row(vec![
                old_size,
                new_size,
                start(&old_items),
                start(&new_items),
                text,
            ]);
        }
        table
    }

    /// The CHANGE table: the items that changed in each block, with the
    /// rows that sum up the rest.
    fn change_table(&self, answer: &Answer, rows: &[ChangeRow]) -> Table {
        let items = &self.compared.items;
        let mut table = Table::new(&[
            ("CHANGE", Align::Right),
            ("OLD", Align::Right),
            ("NEW", Align::Right),
            ("ADDRESS", Align::Left),
            ("ITEM", Align::Left),
        ]);
        for row in rows {
            let cells = match row {
                ChangeRow::Item {
                    change: at,
                    kept,
                    bytes,
                } => {
                    let change = &items.changes[*at];
                    let size = |build: &Build, index: Option<usize>| {
                        index
                            .map_or_else(String::new, |index| size_text(&build.binary.items[index]))
                    };
                    // A new or gone item with what only it keeps counts
                    // their bytes too, so that NEW less OLD is CHANGE.
                    let (old_size, new_size) = match (kept, change.status) {
                        (0, _) => (size(items.old, change.old), size(items.new, change.new)),
                        (_, Status::Gone) => ((-bytes).to_string(), String::new()),
                        _ => (String::new(), bytes.to_string()),
                    };
                    [
                        signed(*bytes),
                        old_size,
                        new_size,
                        address(items.item(change).address),
                        self.item_text(change, *kept),
                    ]
                }
                ChangeRow::Fold { fold, change, .. } => [
                    signed(*change),
                    String::new(),
                    String::new(),
                    String::new(),
                    fold.text(),
                ],
                ChangeRow::More {
                    count,
                    change,
                    block,
                } => {
                    let block = match block {
                        Some(block) if answer.blocks.len() > 1 => {
                            format!(" in {}", range_start(answer.blocks[*block].range))
                        }
                        _ => String::new(),
                    };
                    [
                        signed(*change),
                        String::new(),
                        String::new(),
                        String::new(),
                        format!("{count} more{block}"),
                    ]
                }
            };
            table.row(cells.into());
        }
        table
    }

    /// What a row says of an item that changed: its name, then in
    /// parentheses its old name, the sections it moved between, whether it
    /// is edited, and whether it is new or gone, with the `kept` new or gone
    /// items that only it keeps, or the functions that the compiler inlined
    /// it into; then what keeps it, as "crc_table (gone), kept by crc32".
    fn item_text(&self, change: &Change, kept: usize) -> String {
        let items = &self.compared.items;
        let (old, new) = (&items.old.binary, &items.new.binary);
        let item = items.item(change);
        let mut marks = Vec::new();
        if let (Some(was), Some(old_index), Some(new_index)) =
            (items.was(change), change.old, change.new)
        {
            let (old_item, new_item) = (&old.items[old_index], &new.items[new_index]);
            // A name that elfq made, or that differs only in a number that
            // changes from build to build, says nothing of the item.
            if !is_made_name(old_item, was)
                && !is_made_name(new_item, &new_item.name)
                && !same_but_numbers(old_item, new_item)
            {
                marks.push(format!("was {was}"));
            }
        }
        if let (Status::Moved, Some(old_index), Some(new_index)) =
            (change.status, change.old, change.new)
        {
            let from = pairing::section_name(old, old_index);
            let to = pairing::section_name(new, new_index);
            if from != to {
                marks.push(format!("{from} to {to}"));
            }
        }
        if change.status == Status::Edited {
            marks.push("edited".to_string());
        }
        let mut ending = match (change.status, change.old, change.new) {
            (Status::Gone, Some(index), _) => self.old.kept_by(index),
            (_, _, Some(index)) => self.new.kept_by(index),
            _ => String::new(),
        };
        if matches!(change.status, Status::New | Status::Gone) {
            let mut mark = change.status.word().to_string();
            match (kept, change.status) {
                (0, _) => {}
                (_, Status::New) => {
                    mark += &format!(", with {} it keeps", plural(kept, "new item", "new items"));
                }
                _ => mark += &format!(", with {} it kept", plural(kept, "gone item", "gone items")),
            }
            if let Some(into) = change.old.and_then(|index| self.inlined_into.get(&index)) {
                let names: Vec<String> = into.iter().map(|&index| self.new.name(index)).collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                mark += &format!(", inlined into {}", names_text(&names, self.all));
                // "inlined into process_frame, kept by process_frame" would
                // say it twice.
                let named = &names[..shown_count(names.len(), named(self.all))];
                let keeper = change.old.and_then(|index| self.old.keeper(index));
                if keeper.is_some_and(|keeper| named.contains(&self.old.name(keeper).as_str())) {
                    ending.clear();
                }
            }
            marks.push(mark);
        }
        let marks = match marks.is_empty() {
            true => String::new(),
            false => format!(" ({})", marks.join(", ")),
        };
        format!("{}{marks}{ending}", item.name)
    }
}

impl RowNames<'_, '_> {
    /// What a row calls item `index`, as [`Names::short_name`] gives it.
    fn name(&self, index: usize) -> String {
        if let Some(name) = self.known.borrow().get(&index) {
            return name.clone();
        }
        let name = self.names.short_name(Node::Item(index));
        self.known.borrow_mut().insert(index, name.clone());
        name
    }

    /// The item that keeps item `index`, its parent in the dominator tree,
    /// if one does.
    fn keeper(&self, index: usize) -> Option<usize> {
        match self.build.tree.as_ref()?.parent(Node::Item(index))? {
            Node::Item(parent) => Some(parent),
            Node::Import(_) => None,
        }
    }

    /// How a row ends, saying what keeps item `index`, as `top`'s rows do:
    /// ", kept by main", where one item keeps it; nothing where several of
    /// the places where the program starts do, or where elfq can't tell;
    /// and ", which nothing reaches".
    fn kept_by(&self, index: usize) -> String {
        let Some(tree) = &self.build.tree else {
            return String::new();
        };
        match self.keeper(index) {
            Some(parent) => format!(", kept by {}", self.name(parent)),
            None if !tree.is_reachable(Node::Item(index)) => ", which nothing reaches".to_string(),
            None => String::new(),
        }
    }
}

/// The Try line's question: why the new build has the item that heads the
/// subtree of new items with the most new bytes (see
/// [`Items::heads`](super::rows::Items::heads)), as SYMBOL names it, as the
/// overview's Try line picks an item for `why`: one that something keeps,
/// rather than a place where the program starts, which `why` only names,
/// and with a name of its own. Of subtrees of the same bytes, the one that
/// the table lists first. Not for a shared library, which `why` doesn't
/// answer for yet.
fn try_why(compared: &Compared, heads: &[usize]) -> Option<String> {
    let items = &compared.items;
    let new = items.new;
    let tree = new.tree.as_ref()?;
    let mut bytes = vec![0; items.changes.len()];
    for (at, change) in items.changes.iter().enumerate() {
        if change.status == Status::New {
            bytes[heads[at]] += items.change(change);
        }
    }
    let mut best = None;
    for (at, change) in items.changes.iter().enumerate() {
        let (Status::New, Some(index)) = (change.status, change.new) else {
            continue;
        };
        let (item, node) = (&new.binary.items[index], Node::Item(index));
        if heads[at] != at
            || !tree.is_reachable(node)
            || new.graph.root(node).is_some()
            || is_made_name(item, &item.name)
        {
            continue;
        }
        let rank = (bytes[at], Reverse(&item.name));
        if best.as_ref().is_none_or(|(best, _)| rank > *best) {
            best = Some((rank, index));
        }
    }
    let (_, head) = best?;
    Some(shell_quote(&compared.new_names.query_for(Node::Item(head))))
}

/// "The memory stays at 15728 bytes (.text +16, .eh_frame +8)." or "The
/// memory went from 224 to 208 bytes (.itcm -16)."
fn size_line(answer: &Answer, block: &Block, all: bool) -> String {
    let changes = changes_text(&block.sections(answer.old, answer.new), all);
    let changes = if changes.is_empty() {
        String::new()
    } else {
        format!(" ({changes})")
    };
    let (old, new) = (block.old_size(), block.new_size());
    if old == new {
        format!("The memory stays at {} bytes{changes}.", new.unwrap_or(0))
    } else {
        format!(
            "The memory went from {} to {} bytes{changes}.",
            old.unwrap_or(0),
            new.unwrap_or(0)
        )
    }
}

/// The MEMORY table: a row for each block, with its size in each build and
/// what changed in it.
fn memory_table(answer: &Answer, all: bool) -> Table {
    let mut table = Table::new(&[
        ("MEMORY", Align::Left),
        ("OLD", Align::Right),
        ("NEW", Align::Right),
        ("CHANGE", Align::Right),
        ("SECTIONS", Align::Left),
    ]);
    let size = |size: Option<u64>| size.map_or_else(|| "-".to_string(), |size| size.to_string());
    for block in &answer.blocks {
        let mut memory = range_argument(block.range);
        if block.old.is_empty() {
            memory += " (new)";
        } else if block.new.is_empty() {
            memory += " (gone)";
        }
        table.row(vec![
            memory,
            size(block.old_size()),
            size(block.new_size()),
            signed(block.change()),
            sections_cell(answer, block, all),
        ]);
    }
    table
}

/// What changed in a block: the sections whose size changed, or "bytes
/// differ" when none did but some bytes did, or nothing. A block that only
/// one build has names what is in it, as the overview's table does.
fn sections_cell(answer: &Answer, block: &Block, all: bool) -> String {
    let (memory, places) = match (block.old.is_empty(), block.new.is_empty()) {
        (true, _) => (answer.new, &block.new),
        (_, true) => (answer.old, &block.old),
        _ => return changes_cell(answer, block, all),
    };
    let names = |load_image: bool| -> Vec<&str> {
        places
            .iter()
            .filter(|place| place.load_image == load_image)
            .map(|place| memory.sections[place.section].name.as_str())
            .collect()
    };
    sections_text(&names(false), &names(true))
}

/// The sections whose size changed in a block that both builds have, or
/// "bytes differ", or nothing.
fn changes_cell(answer: &Answer, block: &Block, all: bool) -> String {
    let sections = block.sections(answer.old, answer.new);
    match changes_text(&sections, all) {
        changes if !changes.is_empty() => changes,
        _ if block.same_bytes => String::new(),
        _ => "bytes differ".to_string(),
    }
}

/// The sections whose size changed, each with the change, in address
/// order: those that changed the most, ".itcm -16, .rodata +16", and "and 5
/// more" for the rest, unless `all`.
fn changes_text(sections: &[SectionChange], all: bool) -> String {
    let changed = sections
        .iter()
        .filter(|section| section.change() != 0)
        .count();
    let shown = shown_changes(sections, all);
    let mut text = shown
        .iter()
        .map(|section| format!("{} {}", section_name(section), signed(section.change())))
        .collect::<Vec<_>>()
        .join(", ");
    if shown.len() < changed {
        text += &format!(" and {} more", changed - shown.len());
    }
    text
}

/// The sections whose size changed that [`changes_text`] names, in address
/// order: those that changed the most, or with `all`, every one.
fn shown_changes<'s, 'a>(
    sections: &'s [SectionChange<'a>],
    all: bool,
) -> Vec<&'s SectionChange<'a>> {
    let changed: Vec<&SectionChange> = sections
        .iter()
        .filter(|section| section.change() != 0)
        .collect();
    let shown = shown_count(changed.len(), room(all));
    let mut largest: Vec<usize> = (0..changed.len()).collect();
    largest.sort_by_key(|&index| Reverse(changed[index].change().unsigned_abs()));
    largest.truncate(shown);
    largest.sort_unstable();
    largest.into_iter().map(|index| changed[index]).collect()
}

/// A section's name, or "load image of .data" for its load image.
fn section_name(section: &SectionChange) -> String {
    if section.load_image {
        format!("load image of {}", section.name)
    } else {
        section.name.to_string()
    }
}

/// How many sections to name: [`CHANGES_NAMED`], or with `--all`, every
/// one.
fn room(all: bool) -> usize {
    if all { usize::MAX } else { CHANGES_NAMED }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use object::SectionKind;

    use super::*;
    use crate::binary::{LoadedMemory, Section, same_memory};
    use crate::commands::diff::blocks::{sections_that_differ, shared_blocks};

    /// What diff writes for `old` and `new`, both read from `path`, when
    /// it compares only their bytes, as for a stripped `old`.
    fn text(path: &Path, old: &mut LoadedMemory, new: &LoadedMemory) -> String {
        old.has_symbols = false;
        let old = &*old;
        let same_memory = same_memory(old, new);
        let answer = Answer {
            old_path: path,
            new_path: path,
            old,
            new,
            same_memory,
            blocks: shared_blocks(old, new, same_memory),
            sections_that_differ: sections_that_differ(old, new),
            items: None,
            try_startup: false,
        };
        let mut out = Vec::new();
        write(&answer, false, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
    }

    #[test]
    fn a_change_in_tbss_alone_is_named() {
        // .tbss puts no bytes in memory, so only its name, address and size
        // count, and the line that names the sections says why the exit
        // status is 1.
        let path = Path::new("testdata/bin/rust-x86_64-nodebug.elf");
        let data = read(path);
        let mut old = LoadedMemory::read(&data).unwrap();
        let mut new = LoadedMemory::read(&data).unwrap();
        let tbss = new
            .sections
            .iter()
            .position(|section| section.name == ".tbss")
            .unwrap();
        new.sections[tbss].size += 8;
        let size = shared_blocks(&old, &new, false)[0].new_size().unwrap();
        assert_eq!(
            text(path, &mut old, &new),
            format!(
                "From {0} to {0}: the bytes differ.\n\
                 {0} has no symbol table, so diff compares only bytes, not functions and data.\n\
                 The memory stays at {size} bytes.\n\
                 Sections that differ: .tbss.\n",
                path.display()
            )
        );
    }

    #[test]
    fn a_change_in_tbss_is_named_under_the_table() {
        // Firmware has several blocks, none of which holds .tbss.
        let path = Path::new("testdata/bin/c-cortex-m7.elf");
        let data = read(path);
        let tbss = |size| Section {
            name: ".tbss".to_string(),
            address: 0x2000_0000,
            size,
            kind: SectionKind::UninitializedTls,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        let mut old = LoadedMemory::read(&data).unwrap();
        old.sections.push(tbss(8));
        let mut new = LoadedMemory::read(&data).unwrap();
        new.sections.push(tbss(16));
        assert_eq!(
            text(path, &mut old, &new),
            format!(
                "From {0} to {0}: the bytes differ.\n\
                 {0} has no symbol table, so diff compares only bytes, not functions and data.\n\
                 \n\
                 MEMORY                   OLD   NEW  CHANGE  SECTIONS\n\
                 0x00000000..0x000000e0   224   224       0\n\
                 0x20000000..0x20000450  1104  1104       0\n\
                 0x20240000..0x20241000  4096  4096       0\n\
                 0x30000000..0x30000edc  3804  3804       0\n\
                 \n\
                 Sections that differ: .tbss.\n",
                path.display()
            )
        );
    }
}
