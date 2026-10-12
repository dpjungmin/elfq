//! `elfq FILE why SYMBOL`: the chains of references, from where the program
//! starts, that keep SYMBOL in the file.
//!
//! Every item that uses SYMBOL keeps it there, unless nothing reaches that
//! item or it is there only because of SYMBOL. The answer shows the
//! shortest chain to each item that keeps SYMBOL, merged into one tree.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use crate::binary::{Binary, FileKind};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph, Node, Root, RootKind, ShortestPaths};
use crate::render::{Align, Table, TreeRow};

/// How many items that keep SYMBOL get their chain shown, at most.
const SHOWN: usize = 10;

#[derive(Serialize)]
struct WhyJson<'a> {
    symbol: json::Item<'a>,
    /// Whether anything reaches SYMBOL from where the program starts.
    reachable: bool,
    /// Why SYMBOL is itself where the program starts, as in the HOW column,
    /// or `null` when it isn't. When it is, nothing else needs to keep it,
    /// and `chains` is empty.
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
    /// As in the HOW column: for the first item, why the program starts
    /// there, such as `entry point`; for the others, how the item before
    /// uses this one, such as `call` or `call, address taken`.
    how: String,
}

pub fn run(
    path: &Path,
    binary: &Binary,
    symbol: &str,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let graph = super::reference_graph(binary)?;
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq doesn't know yet which functions a shared library offers to other files, \
             so it can't tell why {name} is in it"
        );
    }
    if let Some(root) = graph.roots.iter().find(|root| root.node == node) {
        if format == Format::Json {
            let answer = WhyJson {
                symbol: json::Item::new(binary, node),
                reachable: true,
                root: Some(root_how(binary, root.kind)),
                chains: Vec::new(),
                unreachable_users: Vec::new(),
                on_every_chain: None,
            };
            return json::write(&answer, out);
        }
        writeln!(out, "{}", root_sentence(binary, root))?;
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
    let unreachable: Vec<usize> = others
        .into_iter()
        .filter(|&user| !tree.is_reachable(Node::Item(user)))
        .collect();
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

    let unreachable: Vec<&str> = unreachable
        .iter()
        .map(|&user| binary.items[user].name.as_str())
        .collect();
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

    if keeping.len() > SHOWN {
        writeln!(out)?;
        writeln!(
            out,
            "To list everything that uses {name}:\n  elfq {} callers {}",
            super::shell_quote(&path.display().to_string()),
            super::shell_quote(symbol)
        )?;
    }
    if !unreachable.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(&unreachable);
        writeln!(out)?;
        writeln!(
            out,
            "{} {verb} {name} too, but nothing reaches {pronoun}.",
            super::names_text(&unreachable),
        )?;
    }
    writeln!(out)?;
    writeln!(out, "{}", on_every_chain(binary, &tree, node))?;
    Ok(())
}

/// The shortest chain from where the program starts, through `user`, to
/// SYMBOL: each item on it, with how the one before uses it, or for the
/// first, why the program starts there.
fn chain(
    binary: &Binary,
    graph: &Graph,
    paths: &ShortestPaths,
    user: Node,
    symbol: Node,
) -> Vec<(Node, String)> {
    let path = paths.path_to(user).unwrap_or_default();
    let start = path.first().map_or(user, |r| Node::Item(r.from));
    let root = graph.roots.iter().find(|root| root.node == start);
    let mut chain = vec![(
        start,
        root.map_or(String::new(), |root| root_how(binary, root.kind)),
    )];
    for reference in &path {
        chain.push((
            reference.to,
            how(graph, Node::Item(reference.from), reference.to),
        ));
    }
    chain.push((symbol, how(graph, user, symbol)));
    chain
}

/// Says that nothing reaches SYMBOL, nor the items that use it.
fn write_unreachable(name: &str, users: &[&str], out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "Nothing reaches {name} from where the program starts, as far as elfq can tell. It may \
         be dead code, or used in a way elfq can't see."
    )?;
    if !users.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(users);
        writeln!(
            out,
            "{} {verb} it, but nothing reaches {pronoun} either.",
            super::names_text(users),
        )?;
    }
    Ok(())
}

/// The first line of the answer: how many items keep SYMBOL.
fn lead(binary: &Binary, symbol: Node, keeping: &[usize]) -> String {
    let name = symbol.name(binary);
    // An import's code is in another file.
    let is_here = match symbol {
        Node::Item(_) => "is in the file",
        Node::Import(_) => "is imported",
    };
    if let [user] = keeping {
        return format!(
            "{name} {is_here} because {} uses it. The shortest chain to it, from where the \
             program starts:",
            binary.items[*user].name
        );
    }
    let nodes: Vec<Node> = keeping.iter().map(|&user| Node::Item(user)).collect();
    let through = if keeping.len() <= SHOWN {
        "each".to_string()
    } else {
        format!("the nearest {SHOWN}")
    };
    format!(
        "{name} {is_here} because {} use it. The shortest chain through {through}, from where \
         the program starts:",
        super::count_text(binary, &nodes)
    )
}

/// The last line of the answer: the closest item that every chain to
/// SYMBOL goes through, which is the item it counts toward in `retained`.
fn on_every_chain(binary: &Binary, tree: &DominatorTree, symbol: Node) -> String {
    let name = symbol.name(binary);
    // Imports have no bytes in the file to count.
    let counts = matches!(symbol, Node::Item(_));
    match tree.parent(symbol) {
        Some(parent) if counts => format!(
            "Every chain to {name} goes through {parent}, so it counts toward {parent}'s retained \
             size.",
            parent = parent.name(binary)
        ),
        Some(parent) => format!(
            "Every chain to {name} goes through {}.",
            parent.name(binary)
        ),
        None if counts => format!(
            "No single item is on every chain to {name}, so it doesn't count toward any one \
             item's retained size."
        ),
        None => format!("No single item is on every chain to {name}."),
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
            if let Some(&root) = graph.roots.iter().find(|root| root.node == start)
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
                    vec![root_how(self.binary, root.kind)],
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
                    vec![how(self.graph, node, child)],
                    child.name(self.binary),
                    self.children_rows(child),
                )
            })
            .collect()
    }
}

/// How `from` uses `to`, in words: "call", or "call, address taken".
fn how(graph: &Graph, from: Node, to: Node) -> String {
    let Node::Item(from) = from else {
        return String::new();
    };
    let mut hows: Vec<&str> = Vec::new();
    for reference in graph.references_from(from) {
        let how = super::how(reference);
        if reference.to == to && !hows.contains(&how) {
            hows.push(how);
        }
    }
    hows.join(", ")
}

/// Why a root is one, for the HOW column.
fn root_how(binary: &Binary, kind: RootKind) -> String {
    match kind {
        RootKind::Entry => "entry point".to_string(),
        RootKind::InSection(section) => format!("in {}", binary.sections[section].name),
        RootKind::ListedIn(section) => format!("listed in {}", binary.sections[section].name),
        RootKind::PltHeader => "used by the dynamic loader".to_string(),
        RootKind::Personality => "used by the unwinder".to_string(),
        RootKind::VectorTable => "vector table".to_string(),
        RootKind::Handler => "listed in the vector table".to_string(),
    }
}

/// Why a root is one, as a sentence.
pub(super) fn root_sentence(binary: &Binary, root: &Root) -> String {
    let name = root.node.name(binary);
    // Whether the program uses a startup section when it starts or exits.
    let when = |section: usize| {
        let section = &binary.sections[section].name;
        if section.contains("fini") || section.contains("dtors") {
            "exits"
        } else {
            "starts"
        }
    };
    match root.kind {
        RootKind::Entry => format!("{name} is the entry point, where the program starts."),
        RootKind::InSection(section)
            if binary.sections[section].kind == object::SectionKind::Note =>
        {
            format!(
                "{name} is a note in {}, which the system reads when it loads the program.",
                binary.sections[section].name
            )
        }
        RootKind::InSection(section) => format!(
            "{name} is in {}, which the program uses when it {}.",
            binary.sections[section].name,
            when(section)
        ),
        RootKind::ListedIn(section) => format!(
            "{name} is listed in {}, so the program runs it when it {}.",
            binary.sections[section].name,
            when(section)
        ),
        RootKind::PltHeader => format!(
            "{name} is the header of the procedure linkage table, which the dynamic loader uses \
             to find imported functions."
        ),
        RootKind::Personality => format!(
            "{name} is a personality routine, which the unwinder calls while it handles a panic \
             or an exception."
        ),
        RootKind::VectorTable => format!(
            "{name} holds the vector table, from which the processor takes the addresses of its \
             handlers."
        ),
        RootKind::Handler => format!(
            "{name} is listed in the vector table, so the processor runs it for an exception or \
             an interrupt."
        ),
    }
}

/// "uses" and the item's name for one item, or "use" and "them" for several.
fn verb_and_pronoun<'a>(names: &[&'a str]) -> (&'static str, &'a str) {
    match names {
        [one] => ("uses", one),
        _ => ("use", "them"),
    }
}
