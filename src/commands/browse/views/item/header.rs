//! The header of an item's page: where the item is and its facts, and,
//! once the session has worked it out, what keeps it, as `retained` and
//! `why` say.

use std::collections::BTreeSet;

use super::super::{cut_left, fit_name, whereabouts};
use crate::binary::Source;
use crate::commands::browse::session::Session;
use crate::commands::{text, why};
use crate::graph::Node;

/// The fewest columns that the header shortens the name of the item that
/// keeps this one to, to keep its facts on one line.
const KEEPER_WIDTH: usize = 16;

/// The facts about item `index` that its header gives: where it is, and
/// on the next line, what makes it a root, how many items use it,
/// `used_by`, and how many it uses.
pub(super) fn header(session: &Session, index: usize, used_by: usize) -> (String, String) {
    let binary = session.binary;
    let item = &binary.items[index];
    let mut place = whereabouts(item, &binary.sections[item.section].name);
    // Which block of memory, when there are several, as in firmware.
    if session.blocks.len() > 1
        && let Some(block) = session.block_of(item.address)
    {
        place += &format!(" ({})", text::range_argument(session.blocks[block].range));
    }
    if let Some(load) = binary.load_address(item) {
        place += &format!(", copied there from {load:#x}");
    }
    let mut facts = Vec::new();
    if item.source == Source::Reached {
        facts.push("found by following the code".to_string());
    }
    let node = Node::Item(index);
    if let Some(root) = session.graph.root(node) {
        facts.push(format!("root: {}", text::root_column(binary, root.kind)));
    }
    facts.push(used_by_text(used_by));
    if item.size > 0 {
        let uses: BTreeSet<Node> = session
            .graph
            .references_from(index)
            .iter()
            .map(|reference| reference.to)
            .collect();
        facts.push(format!("uses {}", uses.len()));
    }
    let unresolved = session.graph.unresolved_in(index);
    if !unresolved.is_empty() {
        facts.push(format!(
            "{} elfq can't follow",
            text::calls_and_jumps(unresolved)
        ));
    }
    (place, facts.join(" · "))
}

/// "used by 5", or "used by nothing".
pub(super) fn used_by_text(used_by: usize) -> String {
    match used_by {
        0 => "used by nothing".to_string(),
        users => format!("used by {users}"),
    }
}

/// What the header says about what keeps an item, as `retained` and `why`
/// say it.
pub(super) struct Keeping {
    /// What removing the item would save, where `retained` answers about it
    /// and something reaches the item.
    retained: Option<u64>,
    /// The name of the item that keeps it, if one does, as `retained`'s
    /// rows name it after "kept by".
    kept_by: Option<String>,
    /// Whether nothing reaches the item from where the program starts.
    unreached: bool,
    /// The names on the shortest chain to the item from where the program
    /// starts, as the first that `why SYMBOL --json` lists, each as rows
    /// name it after "used by": empty for a root, which the facts already
    /// say, for an item that nothing reaches, and where `why` refuses the
    /// item.
    chain: Vec<String>,
}

impl Keeping {
    /// What the header says about what keeps `node`, if the session has
    /// worked out what keeps each item and `retained` or `why` answers
    /// about it, as `retained` and `why` say.
    pub(super) fn new(session: &Session, node: Node, retained: bool, why: bool) -> Option<Keeping> {
        let keepers = session.keepers()?;
        if !retained && !why {
            return None;
        }
        let name = |node: Node| session.short_name(node);
        let unreached = !keepers.tree.is_reachable(node);
        let chain = match why {
            true => why::first_chain(session.binary, &session.graph, &keepers.paths, node),
            false => None,
        };
        Some(Keeping {
            retained: (retained && !unreached).then(|| keepers.tree.retained(node)),
            kept_by: (retained.then(|| keepers.tree.parent(node)).flatten()).map(name),
            unreached,
            chain: chain.unwrap_or_default().into_iter().map(name).collect(),
        })
    }

    /// The facts it adds to the header's second line after `before`, the
    /// facts there already, on a line `width` columns wide: " · retained
    /// 1072, kept by __crc32_veneer", or " · nothing reaches it". The name
    /// of the item that keeps it is shortened to fit what is left of the
    /// line, so that the facts stay on one, or where less than
    /// [`KEEPER_WIDTH`] columns are left, to fit a line of its own.
    pub(super) fn facts(&self, before: &str, width: usize) -> String {
        if self.unreached {
            return " · nothing reaches it".to_string();
        }
        let Some(retained) = self.retained else {
            return String::new();
        };
        let facts = format!(" · retained {retained}");
        let Some(name) = &self.kept_by else {
            return facts;
        };
        let facts = facts + ", kept by ";
        let used = before.chars().count() + facts.chars().count();
        let room = match width.saturating_sub(used) {
            left if left >= KEEPER_WIDTH => left,
            _ => width,
        };
        facts + &fit_name(name, room)
    }

    /// The header's line with the chain, to fit `width` columns: "from a
    /// root: Reset_Handler › main › process_frame". Each name is shortened
    /// to fit half of the line, and where the chain is still too long, the
    /// names at its start are left out, so that its end shows: "from a
    /// root: … › process_frame › __crc32_veneer › crc32".
    pub(super) fn chain_line(&self, width: usize) -> Option<String> {
        if self.chain.is_empty() {
            return None;
        }
        let lead = "from a root: ";
        let room = width.saturating_sub(lead.chars().count()).max(20);
        let names: Vec<String> = (self.chain.iter())
            .map(|name| fit_name(name, (room / 2).max(20)))
            .collect();
        let chain = |from: usize| match from {
            0 => names.join(" › "),
            _ => format!("… › {}", names[from..].join(" › ")),
        };
        // The fewest names left out that let it fit, or all but the last,
        // which is cut from the left where even that doesn't fit.
        let from = (0..names.len())
            .find(|&from| chain(from).chars().count() <= room)
            .unwrap_or(names.len() - 1);
        Some(format!("{lead}{}", cut_left(&chain(from), room)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::super::printed::table_lines;
    use super::super::super::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::commands::json;
    use crate::commands::retained;
    use crate::commands::retained_tree::RetainedTree;
    use crate::graph::DominatorTree;
    use crate::render::{Align, Table, TreeShape};

    #[test]
    fn the_header_says_what_retained_and_why_say() {
        // Items whose header names what keeps them, roots, items with a
        // chain, and items that nothing reaches.
        let (mut kept, mut roots, mut chains, mut unreached) = (0, 0, 0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let mut session = Session::new(&path, &binary).unwrap();
            session.work_out_keepers();
            let graph = &session.graph;
            // A shared library, about whose items `retained` and `why`
            // don't answer, needn't know what keeps them.
            let Some(keepers) = session.keepers() else {
                assert!(retained::refusal(&binary, Node::Item(0)).is_err());
                assert!(why::refusal(&binary, Node::Item(0)).is_err());
                continue;
            };
            // What the commands write, as they run them, but on the graph
            // that the session has built.
            let retained_tree = RetainedTree::new(&binary, graph, None);
            let tree = DominatorTree::build(&binary, graph, None);
            let json = |write: &dyn Fn(&mut Vec<u8>) -> anyhow::Result<()>| {
                let mut out = Vec::new();
                write(&mut out).unwrap();
                serde_json::from_slice::<serde_json::Value>(&out).unwrap()
            };
            let item = |node: Node| serde_json::to_value(json::Item::new(&binary, node)).unwrap();
            let items = (0..binary.items.len()).map(Node::Item);
            for node in items.chain((0..binary.imports.len()).map(Node::Import)) {
                let shown = format!("{}: {}", path.display(), node.name(&binary));
                let retained_index = retained::refusal(&binary, node).ok();
                let why_answers = why::refusal(&binary, node).is_ok();
                let keeping = Keeping::new(&session, node, retained_index.is_some(), why_answers);
                let Some(keeping) = keeping else {
                    assert!(retained_index.is_none() && !why_answers, "{shown}");
                    continue;
                };
                // R is what `retained SYMBOL --json` says removing it would
                // save, where it answers.
                if let Some(index) = retained_index {
                    let symbol = retained::SymbolInTree {
                        retained: &retained_tree,
                        index,
                        lead_name: "",
                    };
                    let answer = json(&|out| retained::write_json(&symbol, out));
                    let reachable = answer["reachable"].as_bool().unwrap();
                    assert_eq!(keeping.unreached, !reachable, "{shown}");
                    let saved = reachable.then(|| answer["retained"].as_u64().unwrap());
                    assert_eq!(keeping.retained, saved, "{shown}");
                }
                if graph.root(node).is_some() {
                    assert!(keeping.kept_by.is_none() && keeping.chain.is_empty());
                    roots += 1;
                    continue;
                }
                // K is the item on every chain that `why SYMBOL --json`
                // names, and the chain its first.
                let answer = json(&|out| why::write_json(&binary, graph, &tree, node, out));
                let reachable = answer["reachable"].as_bool().unwrap();
                assert_eq!(keeping.unreached, !reachable, "{shown}");
                unreached += usize::from(keeping.unreached);
                let parent = keepers.tree.parent(node);
                let on_every_chain = parent.map_or(serde_json::Value::Null, item);
                assert_eq!(answer["on_every_chain"], on_every_chain, "{shown}");
                if retained_index.is_some() {
                    assert_eq!(keeping.kept_by.is_some(), parent.is_some(), "{shown}");
                }
                if let (Some(index), Some(Node::Item(parent))) = (retained_index, parent) {
                    // As `retained`'s own rows name it after "kept by": the
                    // row of this item, which comes right after K's in the
                    // text of the tree under K, after the columns' names.
                    let shape = TreeShape::Rows { words: "kept by" };
                    let with_rows = HashSet::from([parent, index]);
                    let mut table = Table::new(&[
                        ("RETAINED", Align::Right),
                        ("OWN", Align::Right),
                        ("ITEM", Align::Left),
                    ]);
                    table.tree(&[retained_tree.tree_row(&with_rows, parent, shape)], shape);
                    let row = &table_lines(&table)[2];
                    let kept_by = format!(", kept by {}", keeping.kept_by.as_ref().unwrap());
                    assert!(
                        row.contains(binary.items[index].name.as_str()),
                        "{shown}: {row}"
                    );
                    assert!(row.ends_with(&kept_by), "{shown}: {row}");
                    kept += 1;
                }
                let first = why::first_chain(&binary, graph, &keepers.paths, node);
                let first = first.unwrap_or_default();
                let steps: Vec<_> = (answer["chains"].get(0).and_then(|c| c.as_array()))
                    .map_or(Vec::new(), |steps| {
                        steps.iter().map(|s| s["item"].clone()).collect()
                    });
                let nodes: Vec<_> = first.iter().map(|&node| item(node)).collect();
                assert_eq!(nodes, steps, "{shown}");
                // Each named as why's rows name the row they hang under.
                let names: Vec<String> = (first.into_iter())
                    .map(|node| session.names.short_name(node))
                    .collect();
                assert_eq!(keeping.chain, names, "{shown}");
                chains += usize::from(!keeping.chain.is_empty());
            }
        }
        assert!(
            kept > 4000 && roots > 150 && chains > 4000 && unreached > 100,
            "{kept} {roots} {chains} {unreached}"
        );
    }
}
