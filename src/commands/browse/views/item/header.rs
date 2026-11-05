//! The header of an item's page.
//!
//! It says where the item is and its facts, and, once the session has
//! worked it out, what keeps it, as `retained` and `why` say.

use std::collections::BTreeSet;

use ratatui::style::Stylize;
use ratatui::text::Line;

use super::super::{cut_left, fit_name, keep_lines, name_fitter, whereabouts, wrap};
use crate::binary::Source;
use crate::commands::browse::session::Session;
use crate::commands::{text, why};
use crate::graph::Node;

/// The narrowest that the header makes the name after "kept by".
///
/// It shortens the name to keep the facts on one line, but to no fewer
/// columns than this.
const KEEPER_WIDTH: usize = 16;

/// What the header of an item's page says, and its lines as last laid out.
pub(super) struct Header {
    /// The item or import.
    node: Node,
    /// The item's full name, which the first line starts with.
    name: String,
    /// The rest of the first line: what SYMBOL said, then where it is.
    ///
    /// An example: " (also named NMI_Handler)  function at …".
    after_name: String,
    /// The second line: "used by 2 · uses 3".
    facts: String,
    /// What keeps the item, once the session has worked it out.
    ///
    /// It is there only where `retained` or `why` answers about the item.
    keeping: Option<Keeping>,
    /// The lines, and the columns and the most lines they were made for.
    lines: Option<((usize, usize), Vec<Line<'static>>)>,
}

impl Header {
    /// The header of `node`, an item or an import that `used_by` items use.
    ///
    /// It names the node as `header_name`, which may say what SYMBOL gave,
    /// as in "Default_Handler (also named NMI_Handler)" (see the names in
    /// `session.rs`).
    pub(super) fn new(session: &Session, node: Node, header_name: &str, used_by: usize) -> Header {
        let name = node.name(session.binary);
        let (name, note) = match header_name.strip_prefix(name) {
            Some(note) => (name, note),
            None => (header_name, ""),
        };
        let (place, facts) = match node {
            Node::Item(index) => (
                item_place(session, index),
                item_facts(session, index, used_by),
            ),
            Node::Import(_) => (
                "imported from a shared library: its code isn't in this file".to_string(),
                used_by_text(used_by),
            ),
        };
        Header {
            node,
            name: name.to_string(),
            after_name: format!("{note}  {place}"),
            facts,
            keeping: None,
            lines: None,
        }
    }

    /// Adds what keeps the item, once the session has worked it out.
    ///
    /// It adds it only where `retained` or `why` answers about the item, as
    /// those commands say it.
    pub(super) fn add_keeping(&mut self, session: &Session, retained: bool, why: bool) {
        if self.keeping.is_none() {
            self.keeping = Keeping::new(session, self.node, retained, why);
            self.lines = None;
        }
    }

    /// The header, wrapped to `width` columns, in at most `most` lines.
    ///
    /// The lines are made once for each size of screen. The page keeps room
    /// under them for the tab line and two lines of the tab.
    ///
    /// 1. The header wraps, so that nothing is cut, when it fits.
    /// 2. Otherwise the line with the chain from where the program starts
    ///    goes first, since tab 5 shows the chain too.
    /// 3. Then the item's name is shortened as the title line's is, but to
    ///    no less than half a line.
    /// 4. What is still too long is cut, ending in "…".
    ///
    /// The chain is always one line, which leaves out names at its start, so
    /// that its end shows (see [`Keeping::chain_line`]).
    pub(super) fn lines(&mut self, width: usize, most: usize) -> Vec<Line<'static>> {
        if let Some((size, lines)) = &self.lines
            && *size == (width, most)
        {
            return lines.clone();
        }
        let (name, rest) = (&self.name, &self.after_name);
        let identity = |name: &str| wrap(&format!("{name}{rest}"), width);
        let mut facts = self.facts.clone();
        let mut chain = None;
        if let Some(keeping) = &self.keeping {
            let more = keeping.facts(&facts, width);
            facts += &more;
            chain = keeping.chain_line(width);
        }
        let facts = wrap(&facts, width);
        let mut first = identity(name);
        // Rule 2: the chain isn't counted here, so it goes before the name
        // is shortened.
        if first.len() + facts.len() > most {
            let lines = most.saturating_sub(facts.len()).max(1);
            // Rule 3: the widest shortened name that leaves the line short
            // enough, which is no wider than the lines, but no narrower
            // than half a line, after which rule 4 cuts the rest instead.
            let fit = name_fitter(name);
            let length = name.chars().count();
            let (mut fits, mut wider) = (0, length.min(lines * width));
            while fits + 1 < wider {
                let middle = (fits + wider) / 2;
                if identity(&fit(middle)).len() <= lines {
                    fits = middle;
                } else {
                    wider = middle;
                }
            }
            let fits = fits.max(length.min(width / 2)).max(1);
            first = keep_lines(identity(&fit(fits)), lines, width);
        }
        // Rule 4.
        let facts = keep_lines(facts, most.saturating_sub(first.len()), width);
        // Rules 1 and 2: the chain shows only where a line is left for it.
        let chain = chain.filter(|_| first.len() + facts.len() < most);
        let first = first.into_iter().map(|line| Line::raw(line).bold());
        let below = facts.into_iter().chain(chain);
        let below = below.map(|line| Line::raw(line).dim());
        let lines: Vec<Line<'static>> = first.chain(below).collect();
        self.lines = Some(((width, most), lines.clone()));
        lines
    }
}

/// Where item `index` is, as the header's first line says it.
///
/// That is "function at 0x1080, 16 bytes, in .plt.sec", then its block
/// of memory, when there are several, and where it is copied from, if it
/// is.
fn item_place(session: &Session, index: usize) -> String {
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
    place
}

/// The facts about item `index` that the header's second line gives.
///
/// They are what makes it a root, how many items use it, `used_by`, and
/// how many it uses.
fn item_facts(session: &Session, index: usize, used_by: usize) -> String {
    let binary = session.binary;
    let item = &binary.items[index];
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
    facts.join(" · ")
}

/// "used by 5", or "used by nothing".
fn used_by_text(used_by: usize) -> String {
    match used_by {
        0 => "used by nothing".to_string(),
        users => format!("used by {users}"),
    }
}

/// What the header says about what keeps an item.
///
/// It says it as `retained` and `why` do.
struct Keeping {
    /// What removing the item would save.
    ///
    /// It is there where `retained` answers about the item and something
    /// reaches it.
    retained: Option<u64>,
    /// The name of the item that keeps it, if one does.
    ///
    /// It is the name that `retained`'s rows give it after "kept by".
    kept_by: Option<String>,
    /// Whether nothing reaches the item from where the program starts.
    unreached: bool,
    /// The names on the shortest chain from where the program starts to it.
    ///
    /// That is the first chain that `why SYMBOL --json` lists, each name as
    /// rows give it after "used by". It is empty for a root, which the facts
    /// already say, for an item that nothing reaches, and where `why`
    /// refuses the item.
    chain: Vec<String>,
}

impl Keeping {
    /// What the header says about what keeps `node`, if anything.
    ///
    /// It says something once the session has worked out what keeps each
    /// item, where `retained` or `why` answers about `node`, as they say it.
    fn new(session: &Session, node: Node, retained: bool, why: bool) -> Option<Keeping> {
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

    /// The facts it adds after `before`, the header's second line so far.
    ///
    /// They are " · retained 1072, kept by __crc32_veneer", or " · nothing
    /// reaches it", on a line `width` columns wide. The name of the item
    /// that keeps it is shortened to fit what is left of the line, so that
    /// the facts stay on one. Where less than [`KEEPER_WIDTH`] columns are
    /// left, it is shortened to fit a line of its own.
    fn facts(&self, before: &str, width: usize) -> String {
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

    /// The header's line with the chain, to fit `width` columns.
    ///
    /// An example: "from a root: Reset_Handler › main › process_frame". Each
    /// name is shortened to fit half of the line. Where the chain is still
    /// too long, the names at its start are left out, so that its end shows:
    /// "from a root: … › process_frame › __crc32_veneer › crc32".
    fn chain_line(&self, width: usize) -> Option<String> {
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
    use std::collections::{HashMap, HashSet};

    use super::super::super::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::cli::Format;
    use crate::commands::json;
    use crate::commands::retained;
    use crate::commands::retained_tree::RetainedTree;
    use crate::graph::DominatorTree;

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
                    let left_out = [];
                    let answer =
                        json(&|out| retained::write_json(&retained_tree, &[index], &left_out, out));
                    let reachable = answer["symbols"][0]["reachable"].as_bool().unwrap();
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
                    let with_rows = HashSet::from([parent, index]);
                    let tops = [Node::Item(parent)];
                    let table =
                        retained_tree.table(&with_rows, &tops, &HashMap::new(), Format::Text);
                    let mut out = Vec::new();
                    table.write(&mut out).unwrap();
                    let out = String::from_utf8(out).unwrap();
                    let row = out.lines().nth(2).unwrap();
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
