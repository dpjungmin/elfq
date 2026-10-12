//! `elfq FILE exclusive SYMBOL`: the items that only SYMBOL leads to.
//!
//! [`Exclusive`] also serves `retained`, which shows the same items as a
//! tree.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use super::tree::RetainedTree;
use crate::binary::{Binary, FileKind, ItemKind};
use crate::cli::Format;
use crate::graph::Node;
use crate::render::{Align, Table};

#[derive(Serialize)]
struct ExclusiveJson<'a> {
    symbol: json::Item<'a>,
    /// Whether anything reaches SYMBOL from where the program starts. When
    /// nothing does, nothing is there only because of it.
    reachable: bool,
    /// The bytes of the items exclusive to SYMBOL, not counting its own.
    bytes: u64,
    /// The items exclusive to SYMBOL, largest first.
    items: Vec<ExclusiveItem<'a>>,
}

#[derive(Serialize)]
struct ExclusiveItem<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Whether nothing calls this function directly, so that it counts for
    /// the code or data that holds its address.
    through_pointer: bool,
}

pub fn run(binary: &Binary, symbol: &str, format: Format, out: &mut dyn Write) -> Result<()> {
    let exclusive = Exclusive::find(binary, symbol)?;
    let name = exclusive.node.name(binary);
    let mut items = exclusive.items();
    items.sort_by_key(|&index| (std::cmp::Reverse(binary.items[index].size), index));
    let bytes: u64 = items.iter().map(|&index| binary.items[index].size).sum();
    if format == Format::Json {
        let answer = ExclusiveJson {
            symbol: json::Item::new(binary, exclusive.node),
            reachable: exclusive.is_reachable(),
            bytes,
            items: items
                .iter()
                .map(|&index| ExclusiveItem {
                    item: json::Item::new(binary, Node::Item(index)),
                    through_pointer: exclusive.retained.through_pointer(index),
                })
                .collect(),
        };
        return json::write(&answer, out);
    }
    if !exclusive.is_reachable() {
        return exclusive.write_unreachable(out);
    }
    if items.is_empty() {
        writeln!(out, "Nothing else is in the file only because of {name}.")?;
        return Ok(());
    }
    let nodes: Vec<Node> = items.iter().map(|&index| Node::Item(index)).collect();
    writeln!(
        out,
        "{name} is the only way to reach {}, {bytes} bytes in all:",
        super::count_text(binary, &nodes)
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
            super::size_text(item),
            item.kind.to_string(),
            exclusive.retained.label(index),
        ]);
    }
    table.write(out)?;
    exclusive.retained.write_pointer_note(&items, out)?;
    Ok(())
}

/// SYMBOL's item, in the dominator tree that says what is exclusive to it.
pub(super) struct Exclusive<'a> {
    pub retained: RetainedTree<'a>,
    /// SYMBOL's item.
    pub node: Node,
}

impl<'a> Exclusive<'a> {
    /// Finds SYMBOL's item and what is exclusive to it.
    pub fn find(binary: &'a Binary, symbol: &str) -> Result<Exclusive<'a>> {
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
        let retained = RetainedTree::build(binary)?;
        Ok(Exclusive { retained, node })
    }

    /// Whether anything reaches SYMBOL from where the program starts.
    pub fn is_reachable(&self) -> bool {
        self.retained.tree.is_reachable(self.node)
    }

    /// Says that nothing reaches SYMBOL, so nothing is there only because of
    /// it.
    pub fn write_unreachable(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(
            out,
            "Nothing reaches {} from where the program starts, as far as elfq can tell, so \
             nothing is there only because of it. It may be dead code, or used in a way elfq \
             can't see.",
            self.node.name(self.retained.binary)
        )?;
        Ok(())
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
