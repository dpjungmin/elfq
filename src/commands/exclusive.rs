//! `elfq FILE exclusive SYMBOL`: the items that only SYMBOL leads to.
//!
//! [`Exclusive`] also serves `retained`, which shows the same items as a
//! tree.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};

use crate::binary::{Binary, FileKind, ItemKind};
use crate::graph::{DominatorTree, Graph, Node, ReferenceKind};
use crate::render::{Align, Table};

pub fn run(binary: &Binary, symbol: &str, out: &mut dyn Write) -> Result<()> {
    let Some(exclusive) = Exclusive::find(binary, symbol, out)? else {
        return Ok(());
    };
    let name = exclusive.node.name(binary);
    let mut items = exclusive.items();
    if items.is_empty() {
        writeln!(out, "Nothing else is in the file only because of {name}.")?;
        return Ok(());
    }
    items.sort_by_key(|&index| (std::cmp::Reverse(binary.items[index].size), index));
    let bytes: u64 = items.iter().map(|&index| binary.items[index].size).sum();
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
            exclusive.label(index),
        ]);
    }
    table.write(out)?;
    exclusive.write_pointer_note(&items, out)?;
    Ok(())
}

/// SYMBOL's item, with the graph and dominator tree that say what is
/// exclusive to it.
pub(super) struct Exclusive<'a> {
    pub binary: &'a Binary,
    pub graph: Graph,
    pub tree: DominatorTree,
    /// SYMBOL's item.
    pub node: Node,
    /// The items that something calls directly, rather than only through a
    /// pointer.
    called: HashSet<usize>,
}

impl<'a> Exclusive<'a> {
    /// Finds SYMBOL's item and what is exclusive to it. When nothing reaches
    /// the item, says so in `out` and returns `None`.
    pub fn find(
        binary: &'a Binary,
        symbol: &str,
        out: &mut dyn Write,
    ) -> Result<Option<Exclusive<'a>>> {
        let graph = super::reference_graph(binary)?;
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
        let tree = DominatorTree::build(binary, &graph);
        if !tree.is_reachable(node) {
            writeln!(
                out,
                "Nothing reaches {name} from where the program starts, as far as elfq can tell, \
                 so nothing is there only because of it. It may be dead code, or used in a way \
                 elfq can't see."
            )?;
            return Ok(None);
        }
        let called = graph
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call | ReferenceKind::TailCall))
            .filter_map(|r| match r.to {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect();
        Ok(Some(Exclusive {
            binary,
            graph,
            tree,
            node,
            called,
        }))
    }

    /// The items exclusive to SYMBOL's item, in no particular order.
    /// Imports are left out: their code isn't in the file.
    pub fn items(&self) -> Vec<usize> {
        self.tree
            .exclusive(self.node)
            .into_iter()
            .filter_map(|node| match node {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect()
    }

    /// An item's name, marked when it is a function that is only called
    /// through pointers.
    pub fn label(&self, index: usize) -> String {
        let name = &self.binary.items[index].name;
        if self.through_pointer(index) {
            format!("{name} (through a pointer)")
        } else {
            name.clone()
        }
    }

    /// Explains the marks that [`Exclusive::label`] added to `shown`, if any.
    pub fn write_pointer_note(&self, shown: &[usize], out: &mut dyn Write) -> Result<()> {
        if shown.iter().any(|&index| self.through_pointer(index)) {
            writeln!(out)?;
            writeln!(
                out,
                "Nothing calls the functions marked \"through a pointer\" directly, so elfq \
                 counts them for the code or data that holds their address."
            )?;
        }
        Ok(())
    }

    fn through_pointer(&self, index: usize) -> bool {
        self.binary.items[index].kind == ItemKind::Function
            && Node::Item(index) != self.node
            && !self.called.contains(&index)
    }
}
