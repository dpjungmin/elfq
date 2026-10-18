//! `elfq FILE why SYMBOL`: the chains of references, from where the program
//! starts, that keep SYMBOL in the file.
//!
//! Every item that uses SYMBOL keeps it there, unless nothing reaches that
//! item or it is there only because of SYMBOL. The text shows the shortest
//! chain to each of the nearest ten items that keep SYMBOL, merged into one
//! tree. The JSON lists the shortest chain to every item that keeps SYMBOL,
//! one by one.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{json, text};
use crate::binary::{Binary, FileKind};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph, Node, Reference, Root, ShortestPaths};
use crate::render::{Align, Table, TreeRow};

/// How many items that keep SYMBOL get their chain shown, at most.
const SHOWN: usize = 10;

#[derive(Serialize)]
struct WhyJson<'a> {
    symbol: json::Item<'a>,
    /// Whether anything reaches SYMBOL from where the program starts.
    reachable: bool,
    /// Why SYMBOL is itself where the program starts, in the words of
    /// [`json::root_field`], such as `entry point` or `listed in the vector
    /// table`, or `null` when it isn't. When it is, nothing else needs to
    /// keep it, and `chains` is empty.
    root: Option<String>,
    /// The shortest chain from where the program starts, through each item
    /// that keeps SYMBOL, to SYMBOL. The shortest chains come first.
    chains: Vec<Vec<Step<'a>>>,
    /// The items that use SYMBOL but that nothing reaches.
    unreachable_users: Vec<json::Item<'a>>,
    /// The closest item that every chain goes through, toward whose
    /// retained size SYMBOL counts, or `null` when no single item is.
    on_every_chain: Option<json::Item<'a>>,
}

/// One item on a chain: one row of the text's tree.
#[derive(Serialize)]
struct Step<'a> {
    item: json::Item<'a>,
    /// For the first item, why the program starts there, such as `entry
    /// point`; for the others, how the item before uses this one, in the
    /// words of [`json::how_field`], such as `call` or `call, address taken`.
    how: String,
}

/// Answers `elfq FILE why SYMBOL`: the shortest chains from where the
/// program starts through each item that keeps SYMBOL.
pub fn run(binary: &Binary, symbol: &str, format: Format, out: &mut dyn Write) -> Result<()> {
    super::check_code_is_readable(binary)?;
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq doesn't know yet which functions a shared library offers to other files, \
             so it can't tell why {name} is in it"
        );
    }
    let graph = Graph::build(binary);
    if let Some(root) = graph.root(node) {
        if format == Format::Json {
            let answer = WhyJson {
                symbol: json::Item::new(binary, node),
                reachable: true,
                root: Some(json::root_field(binary, root.kind)),
                chains: Vec::new(),
                unreachable_users: Vec::new(),
                on_every_chain: None,
            };
            return json::write(&answer, out);
        }
        writeln!(out, "{}", text::root_sentence(binary, root))?;
        return Ok(());
    }

    let paths = ShortestPaths::avoiding(binary, &graph, node);
    let tree = DominatorTree::build(binary, &graph, None);
    // References are sorted by the item that makes them, so each user
    // appears once after `dedup`.
    let mut users: Vec<usize> = graph.references_to(node).map(|r| r.from).collect();
    users.dedup();
    let (mut keeping, others): (Vec<usize>, Vec<usize>) = users
        .into_iter()
        .partition(|&user| paths.is_reached(Node::Item(user)));
    // The other users don't keep SYMBOL: nothing reaches them, or only
    // SYMBOL leads to them, as when it calls them and they call it back.
    // SYMBOL using itself goes unsaid.
    let (unreachable, behind): (Vec<usize>, Vec<usize>) = others
        .into_iter()
        .filter(|&user| Node::Item(user) != node)
        .partition(|&user| !tree.is_reachable(Node::Item(user)));
    // Those with the shortest chains first, then by name.
    keeping.sort_by_key(|&user| {
        let user_node = Node::Item(user);
        (paths.distance(user_node), user_node.name(binary))
    });
    if format == Format::Json {
        let item = |node: Node| json::Item::new(binary, node);
        let answer = WhyJson {
            symbol: item(node),
            reachable: !keeping.is_empty(),
            root: None,
            chains: keeping
                .iter()
                .map(|&user| {
                    chain(binary, &graph, &paths, Node::Item(user), node)
                        .into_iter()
                        .map(|(node, how)| Step {
                            item: item(node),
                            how,
                        })
                        .collect()
                })
                .collect(),
            unreachable_users: unreachable
                .iter()
                .map(|&user| item(Node::Item(user)))
                .collect(),
            on_every_chain: tree.parent(node).map(item),
        };
        return json::write(&answer, out);
    }

    let names = |users: &[usize]| -> Vec<&str> {
        users
            .iter()
            .map(|&user| binary.items[user].name.as_str())
            .collect()
    };
    let unreachable = names(&unreachable);
    let behind = names(&behind);
    if keeping.is_empty() {
        return write_unreachable(name, &unreachable, out);
    }
    writeln!(out, "{}", lead(binary, node, &keeping))?;
    writeln!(out)?;
    let shown = &keeping[..keeping.len().min(SHOWN)];
    let chains = Chains::new(binary, &graph, &paths, node, shown);
    let mut table = Table::new(&[("HOW", Align::Left), ("ITEM", Align::Left)]);
    table.tree(&chains.rows());
    table.write(out)?;

    if !unreachable.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(&unreachable);
        writeln!(out)?;
        writeln!(
            out,
            "{} {verb} {name} too, but nothing reaches {pronoun}.",
            text::names_text(&unreachable),
        )?;
    }
    if !behind.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(&behind);
        writeln!(out)?;
        writeln!(
            out,
            "{} {verb} {name} too, but only {name} leads to {pronoun}.",
            text::names_text(&behind),
        )?;
    }
    writeln!(out)?;
    match tree.parent(node) {
        Some(parent) => writeln!(
            out,
            "Removing {} would remove {name} too.",
            parent.name(binary)
        )?,
        None => writeln!(out, "Removing any single item wouldn't remove {name}.")?,
    }
    Ok(())
}

/// The shortest chain from where the program starts, through `user`, to
/// SYMBOL, for `--json`: each item on it, with how the one before uses it,
/// or for the first, why the program starts there, in JSON's words
/// ([`json::how_field`] and [`json::root_field`]).
fn chain(
    binary: &Binary,
    graph: &Graph,
    paths: &ShortestPaths,
    user: Node,
    symbol: Node,
) -> Vec<(Node, String)> {
    let path = paths.path_to(user).unwrap_or_default();
    let start = path.first().map_or(user, |r| Node::Item(r.from));
    let root = graph.root(start);
    let mut chain = vec![(
        start,
        root.map_or(String::new(), |root| json::root_field(binary, root.kind)),
    )];
    let json_how =
        |from: Node, to: Node| hows_between(graph, from, to, |r| json::how_field(r.kind));
    for reference in &path {
        chain.push((
            reference.to,
            json_how(Node::Item(reference.from), reference.to),
        ));
    }
    chain.push((symbol, json_how(user, symbol)));
    chain
}

/// Says that nothing reaches SYMBOL, nor the items that use it.
fn write_unreachable(name: &str, users: &[&str], out: &mut dyn Write) -> Result<()> {
    writeln!(out, "{}", text::nothing_reaches(name))?;
    if !users.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(users);
        writeln!(
            out,
            "{} {verb} it, but nothing reaches {pronoun} either.",
            text::names_text(users),
        )?;
    }
    Ok(())
}

/// The first line of the answer: what keeps SYMBOL.
fn lead(binary: &Binary, symbol: Node, keeping: &[usize]) -> String {
    let name = symbol.name(binary);
    if let [user] = keeping {
        return format!(
            "{name} is used by {}, through this chain:",
            binary.items[*user].name
        );
    }
    let nodes: Vec<Node> = keeping.iter().map(|&user| Node::Item(user)).collect();
    let users = text::count_text(binary, &nodes);
    if keeping.len() <= SHOWN {
        format!("{name} is used by {users}, through these chains:")
    } else {
        format!("{name} is used by {users}; these are the chains through the nearest {SHOWN}:")
    }
}

/// The chains to the items that keep SYMBOL, merged into a tree: each item
/// hangs under the item that uses it on the way from a root, and SYMBOL
/// under each item that keeps it.
struct Chains<'a> {
    binary: &'a Binary,
    graph: &'a Graph,
    /// The roots that the chains start from, and why each is one.
    roots: Vec<Root>,
    /// The items each item leads to, in the tree.
    children: BTreeMap<Node, Vec<Node>>,
}

impl<'a> Chains<'a> {
    fn new(
        binary: &'a Binary,
        graph: &'a Graph,
        paths: &ShortestPaths,
        symbol: Node,
        keeping: &[usize],
    ) -> Chains<'a> {
        let mut chains = Chains {
            binary,
            graph,
            roots: Vec::new(),
            children: BTreeMap::new(),
        };
        for &user in keeping {
            let user = Node::Item(user);
            let path = paths.path_to(user).unwrap_or_default();
            let start = path.first().map_or(user, |r| Node::Item(r.from));
            if let Some(&root) = graph.root(start)
                && !chains.roots.contains(&root)
            {
                chains.roots.push(root);
            }
            for reference in &path {
                chains.add(Node::Item(reference.from), reference.to);
            }
            chains.add(user, symbol);
        }
        // SYMBOL first, then by name.
        let name = |node: &Node| (*node != symbol, node.name(binary));
        chains
            .roots
            .sort_by(|a, b| name(&a.node).cmp(&name(&b.node)));
        for children in chains.children.values_mut() {
            children.sort_by(|a, b| name(a).cmp(&name(b)));
        }
        chains
    }

    fn add(&mut self, parent: Node, child: Node) {
        let children = self.children.entry(parent).or_default();
        if !children.contains(&child) {
            children.push(child);
        }
    }

    /// The tree's rows: each root, and under it the chains that start
    /// there.
    fn rows(&self) -> Vec<TreeRow> {
        self.roots
            .iter()
            .map(|root| {
                TreeRow::new(
                    vec![text::root_column(self.binary, root.kind)],
                    root.node.name(self.binary),
                    self.children_rows(root.node),
                )
            })
            .collect()
    }

    /// The rows of `node`'s children, each with the rows of its own.
    fn children_rows(&self, node: Node) -> Vec<TreeRow> {
        let Some(children) = self.children.get(&node) else {
            return Vec::new();
        };
        children
            .iter()
            .map(|&child| {
                TreeRow::new(
                    vec![hows_between(self.graph, node, child, |r| {
                        text::how_column(self.binary, r.to, r.kind)
                    })],
                    child.name(self.binary),
                    self.children_rows(child),
                )
            })
            .collect()
    }
}

/// How `from` uses `to`, in words such as "call", or "call, address
/// taken", with `word` saying how each reference uses it.
fn hows_between(
    graph: &Graph,
    from: Node,
    to: Node,
    word: impl Fn(&Reference) -> &'static str,
) -> String {
    let Node::Item(from) = from else {
        return String::new();
    };
    let mut hows: Vec<&str> = Vec::new();
    for reference in graph.references_from(from) {
        let how = word(reference);
        if reference.to == to && !hows.contains(&how) {
            hows.push(how);
        }
    }
    hows.join(", ")
}

/// "uses" and the item's name for one item, or "use" and "them" for several.
fn verb_and_pronoun<'a>(names: &[&'a str]) -> (&'static str, &'a str) {
    match names {
        [one] => ("uses", one),
        _ => ("use", "them"),
    }
}
