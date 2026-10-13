//! `elfq FILE exclusive SYMBOL`: the items that only SYMBOL leads to.
//!
//! [`Exclusive`] also serves `retained`, which shows the same items as a
//! tree. With `--range`, both count only the bytes in the range.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use super::tree::RetainedTree;
use crate::binary::{AddressRange, Binary, FileKind, ItemKind};
use crate::cli::Format;
use crate::graph::{Node, Unresolved};
use crate::render::{Align, Table};

#[derive(Serialize)]
struct ExclusiveJson<'a> {
    symbol: json::Item<'a>,
    /// Whether anything reaches SYMBOL from where the program starts. When
    /// nothing does, nothing is there only because of it.
    reachable: bool,
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// The bytes of the items exclusive to SYMBOL, not counting its own.
    bytes: u64,
    /// The items exclusive to SYMBOL, largest first. With a range, only
    /// those with bytes in it.
    items: Vec<ExclusiveItem<'a>>,
    /// How many calls and jumps in SYMBOL and those items go through
    /// pointers set at run time, which elfq can't follow. A function called
    /// that way counts for the code or data that holds its address.
    indirect_calls: usize,
}

#[derive(Serialize)]
struct ExclusiveItem<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Whether nothing calls this function directly, so that it counts for
    /// the code or data that holds its address.
    through_pointer: bool,
}

pub fn run(
    binary: &Binary,
    symbol: &str,
    range: Option<AddressRange>,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let exclusive = Exclusive::find(binary, symbol, range)?;
    let retained = &exclusive.retained;
    let name = exclusive.node.name(binary);
    let mut items = exclusive.counted_items();
    items.sort_by_key(|&index| (std::cmp::Reverse(retained.own(index)), index));
    let bytes: u64 = items.iter().map(|&index| retained.own(index)).sum();
    if format == Format::Json {
        let answer = ExclusiveJson {
            symbol: json::Item::new(binary, exclusive.node),
            reachable: exclusive.is_reachable(),
            range: range.map(json::Range::new),
            bytes,
            items: items
                .iter()
                .map(|&index| ExclusiveItem {
                    item: json::Item::new(binary, Node::Item(index)),
                    through_pointer: exclusive.retained.through_pointer(index),
                })
                .collect(),
            indirect_calls: exclusive.indirect_calls().len(),
        };
        return json::write(&answer, out);
    }
    if !exclusive.is_reachable() {
        return exclusive.write_unreachable(out);
    }
    if items.is_empty() {
        match range {
            Some(range) => writeln!(
                out,
                "Nothing in {} is in the file only because of {name}.",
                super::range_text(binary, range)
            )?,
            None => writeln!(out, "Nothing else is in the file only because of {name}.")?,
        }
        return exclusive.write_indirect_note(out);
    }
    let nodes: Vec<Node> = items.iter().map(|&index| Node::Item(index)).collect();
    let place = match range {
        Some(range) => format!("in {}", super::range_text(binary, range)),
        None => "in all".to_string(),
    };
    writeln!(
        out,
        "{name} is the only way to reach {}, {} {place}:",
        super::count_text(binary, &nodes),
        super::bytes_text(bytes)
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("SIZE", Align::Right),
        ("KIND", Align::Left),
        ("ITEM", Align::Left),
    ]);
    for &index in &items {
        let item = &binary.items[index];
        table.row(vec![
            retained.own_text(index),
            item.kind.to_string(),
            retained.label(index),
        ]);
    }
    table.write(out)?;
    exclusive.write_indirect_note(out)
}

/// SYMBOL's item, in the dominator tree that says what is exclusive to it.
pub(super) struct Exclusive<'a> {
    pub retained: RetainedTree<'a>,
    /// SYMBOL's item.
    pub node: Node,
}

impl<'a> Exclusive<'a> {
    /// Finds SYMBOL's item and what is exclusive to it, counting only the
    /// bytes in `range`, if there is one.
    pub fn find(
        binary: &'a Binary,
        symbol: &str,
        range: Option<AddressRange>,
    ) -> Result<Exclusive<'a>> {
        super::check_code_is_readable(binary)?;
        let node = super::symbol::find(binary, symbol)?;
        let name = node.name(binary);
        let Node::Item(index) = node else {
            bail!("{name} is imported from a shared library, so its code isn't in this file");
        };
        let item = &binary.items[index];
        if item.kind == ItemKind::Function && item.size == 0 {
            bail!("elfq doesn't know where {name} ends, so it can't read its code");
        }
        if binary.kind == FileKind::SharedLibrary {
            bail!(
                "elfq doesn't know yet which functions a shared library offers to other files, \
                 so it can't tell what only {name} leads to"
            );
        }
        let retained = RetainedTree::build(binary, range)?;
        Ok(Exclusive { retained, node })
    }

    /// Whether anything reaches SYMBOL from where the program starts.
    pub fn is_reachable(&self) -> bool {
        self.retained.tree.is_reachable(self.node)
    }

    /// Says that nothing reaches SYMBOL, so it is dead code, or used in a way
    /// elfq can't see.
    pub fn write_unreachable(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(
            out,
            "{}",
            super::nothing_reaches(self.node.name(self.retained.binary))
        )?;
        Ok(())
    }

    /// The calls and jumps through pointers set at run time in SYMBOL and
    /// the items exclusive to it.
    pub fn indirect_calls(&self) -> Vec<Unresolved> {
        let Node::Item(index) = self.node else {
            return Vec::new();
        };
        let graph = &self.retained.graph;
        std::iter::once(index)
            .chain(self.items())
            .flat_map(|item| graph.unresolved_in(item))
            .filter(|unresolved| unresolved.is_indirect())
            .copied()
            .collect()
    }

    /// Says that what SYMBOL's code calls through pointers may count for
    /// other items, if it calls anything that way.
    pub fn write_indirect_note(&self, out: &mut dyn Write) -> Result<()> {
        let indirect = self.indirect_calls();
        if indirect.is_empty() {
            return Ok(());
        }
        let name = self.node.name(self.retained.binary);
        let in_symbol = indirect.iter().any(|u| Node::Item(u.from) == self.node);
        let in_items = indirect.iter().any(|u| Node::Item(u.from) != self.node);
        let calls = indirect.iter().any(|u| u.is_call);
        let jumps = indirect.iter().any(|u| !u.is_call);
        // The verb after one subject, and after two.
        let (one, two) = match (calls, jumps) {
            (true, true) => ("calls and jumps", "call and jump"),
            (true, false) => ("calls", "call"),
            (false, _) => ("jumps", "jump"),
        };
        let what = match (in_symbol, in_items) {
            (true, false) => format!("{name} also {one}"),
            (false, _) => format!("Code that only {name} leads to also {one}"),
            (true, true) => format!("{name} and the code that only it leads to also {two}"),
        };
        let (through, reach) = if indirect.len() == 1 {
            ("a pointer", "that reaches")
        } else {
            ("pointers", "they reach")
        };
        writeln!(out)?;
        writeln!(
            out,
            "{what} through {through}, which elfq can't follow, so what {reach} may count \
             elsewhere."
        )?;
        Ok(())
    }

    /// The items exclusive to SYMBOL that sizes count: all of them, or with
    /// a range, those with bytes in it. In no particular order.
    pub fn counted_items(&self) -> Vec<usize> {
        let retained = &self.retained;
        self.items()
            .into_iter()
            .filter(|&index| retained.range.is_none() || retained.own(index) > 0)
            .collect()
    }

    /// The items exclusive to SYMBOL's item, in no particular order.
    /// Imports are left out: their code isn't in the file.
    pub fn items(&self) -> Vec<usize> {
        self.retained
            .tree
            .exclusive(self.node)
            .into_iter()
            .filter_map(|node| match node {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect()
    }
}
