//! `elfq FILE why SYMBOL`: the chains of references, from where the program
//! starts, that keep SYMBOL in the file.
//!
//! Every item that uses SYMBOL keeps it there, unless nothing reaches that
//! item or it is there only because of SYMBOL. The text shows the shortest
//! chain to each of the nearest ten items that keep SYMBOL, or with `--all`
//! to every one, merged into one tree, whose rows name their user where
//! chains branch. The JSON lists the shortest chain to every item that
//! keeps SYMBOL, one by one.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::symbol::Names;
use super::{Kept, TreeAnswer, json, text};
use crate::binary::{Binary, FileKind};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph, Node, Reference, Root, ShortestPaths};
use crate::render::{Align, TreeRow, TreeShape};

/// How many items that keep SYMBOL get their chain shown without `--all`.
/// An item that would be left out on its own is shown too.
const SHOWN: usize = 10;

#[derive(Serialize)]
struct WhyJson<'a> {
    symbol: json::Symbol<'a>,
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
    /// The items that use SYMBOL but that only SYMBOL leads to, as when it
    /// calls them and they call it back, so they don't keep it either.
    users_it_keeps: Vec<json::Item<'a>>,
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
/// program starts through each item that keeps SYMBOL. The text shows those
/// through the nearest 10, or with `all`, through every one.
pub fn run(
    binary: &Binary,
    symbol: &str,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    super::check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbol = names.find(symbol)?;
    let node = symbol.node;
    // The name in the answer's first line, which says what SYMBOL gave when
    // it isn't that.
    let lead_name = symbol.lead_name(binary);
    refusal(binary, node)?;
    let graph = Kept::new(Graph::build(binary));
    if let Some(root) = graph.root(node) {
        if format == Format::Json {
            let answer = WhyJson {
                symbol: json::Symbol::new(binary, node),
                reachable: true,
                root: Some(json::root_field(binary, root.kind)),
                chains: Vec::new(),
                unreachable_users: Vec::new(),
                users_it_keeps: Vec::new(),
                on_every_chain: None,
            };
            return json::write(&answer, out);
        }
        writeln!(
            out,
            "{}",
            text::root_sentence(binary, &lead_name, root.kind)
        )?;
        return Ok(());
    }

    let tree = DominatorTree::build(binary, &graph, None);
    if format == Format::Json {
        return write_json(binary, &graph, &tree, node, out);
    }
    let users = Users::new(binary, &graph, &tree, node);
    users.answer(&names, &lead_name, all, format).write(out)
}

/// Writes what `why SYMBOL --json` writes about `node`, which mustn't be a
/// root, given `graph` and its dominator tree.
pub(super) fn write_json(
    binary: &Binary,
    graph: &Graph,
    tree: &DominatorTree,
    node: Node,
    out: &mut dyn Write,
) -> Result<()> {
    let users = Users::new(binary, graph, tree, node);
    let item = |node: Node| json::Item::new(binary, node);
    let answer = WhyJson {
        symbol: json::Symbol::new(binary, node),
        reachable: !users.keeping.is_empty(),
        root: None,
        chains: users
            .keeping
            .iter()
            .map(|&user| {
                chain(binary, graph, &users.paths, Node::Item(user), node)
                    .into_iter()
                    .map(|(node, how)| Step {
                        item: item(node),
                        how,
                    })
                    .collect()
            })
            .collect(),
        unreachable_users: users
            .unreachable
            .iter()
            .map(|&user| item(Node::Item(user)))
            .collect(),
        users_it_keeps: (users.behind.iter())
            .map(|&user| item(Node::Item(user)))
            .collect(),
        on_every_chain: tree.parent(node).map(item),
    };
    json::write(&answer, out)
}

/// Why `why` can't answer about `node`, if it can't: not yet in a shared
/// library.
pub(super) fn refusal(binary: &Binary, node: Node) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq can't yet tell why {} is in a shared library",
            node.name(binary)
        );
    }
    Ok(())
}

/// The shortest chain from where the program starts to `node`, through
/// the nearest item that keeps it: the first chain that `why SYMBOL --json`
/// lists, of the items on it, `node` last. `paths` are the shortest chains
/// from where the program starts ([`ShortestPaths::from_roots`]), so that
/// one search serves every item. `None` for a root, and for an item that
/// nothing reaches.
///
/// `why` searches without passing through `node`, but a shortest chain to
/// `node` can't pass through it anyway: the items right before it on one
/// are the nearest that keep it, which `why` lists first, and the chains to
/// them are the same in both searches. Of those, `why` takes the name that
/// sorts first, then the first item.
#[cfg(feature = "tui")]
pub(super) fn first_chain(
    binary: &Binary,
    graph: &Graph,
    paths: &ShortestPaths,
    node: Node,
) -> Option<Vec<Node>> {
    if graph.root(node).is_some() {
        return None;
    }
    let before = paths.distance(node)?.checked_sub(1)?;
    let user = graph
        .references_to(node)
        .map(|reference| reference.from)
        .filter(|&user| paths.distance(Node::Item(user)) == Some(before))
        .min_by_key(|&user| (Node::Item(user).name(binary), user))?;
    let path = paths.path_to(Node::Item(user))?;
    let start = path.first().map_or(user, |reference| reference.from);
    let mut chain = vec![Node::Item(start)];
    chain.extend(path.iter().map(|reference| reference.to));
    chain.push(node);
    Some(chain)
}

/// Says that no item that every chain to the item called `name` goes
/// through keeps it.
pub(super) fn no_single_keeper(name: &str) -> String {
    format!("Removing any single item wouldn't remove {name}.")
}

/// The items that use SYMBOL, and the chains from where the program starts
/// to those that keep it, in a file's reference graph.
struct Users<'u, 'a> {
    binary: &'a Binary,
    graph: &'u Graph,
    /// The graph's dominator tree.
    tree: &'u DominatorTree,
    /// SYMBOL's item or import.
    node: Node,
    /// The shortest chains from where the program starts that don't pass
    /// through SYMBOL.
    paths: ShortestPaths,
    /// The items that keep SYMBOL: those that use it and that a chain
    /// reaches. Those with the shortest chains come first, then by name.
    keeping: Vec<usize>,
    /// The items that use SYMBOL but that nothing reaches.
    unreachable: Vec<usize>,
    /// The items that use SYMBOL but that only SYMBOL leads to, as when it
    /// calls them and they call it back.
    behind: Vec<usize>,
}

impl<'u, 'a> Users<'u, 'a> {
    /// The items that use `node` in `graph`, whose dominator tree is
    /// `tree`. `node` mustn't be a root.
    fn new(
        binary: &'a Binary,
        graph: &'u Graph,
        tree: &'u DominatorTree,
        node: Node,
    ) -> Users<'u, 'a> {
        let paths = ShortestPaths::avoiding(binary, graph, node);
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
        Users {
            binary,
            graph,
            tree,
            node,
            paths,
            keeping,
            unreachable,
            behind,
        }
    }

    /// What `why SYMBOL` writes as text, whose first line names SYMBOL
    /// `lead_name` (see [`Symbol::lead_name`]), with `names` for the short
    /// names of the rows: the shortest chains through the nearest 10 items
    /// that keep SYMBOL, or with `all`, through every one, in the shape
    /// that `format` asks for.
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    fn answer(&self, names: &Names<'a>, lead_name: &str, all: bool, format: Format) -> TreeAnswer {
        let (binary, graph, node) = (self.binary, self.graph, self.node);
        let names_of = |users: &[usize]| -> Vec<&str> {
            users
                .iter()
                .map(|&user| binary.items[user].name.as_str())
                .collect()
        };
        let unreachable = names_of(&self.unreachable);
        let behind = names_of(&self.behind);
        let keeping = &self.keeping;
        if keeping.is_empty() {
            return TreeAnswer::sentences(unreachable_lines(lead_name, &unreachable, all));
        }
        let room = if all { usize::MAX } else { SHOWN };
        let shown = &keeping[..text::shown_count(keeping.len(), room)];
        let chains = Chains::new(binary, graph, names, &self.paths, node, shown);
        // A chain branches only now and then, so a row names its user only
        // where it doesn't come right after it.
        let shape = match format {
            Format::Tree => TreeShape::Drawn,
            _ => TreeShape::Chains { words: "used by" },
        };

        let name = node.name(binary);
        let mut notes = Vec::new();
        if !unreachable.is_empty() {
            let (verb, pronoun) = verb_and_pronoun(&unreachable);
            notes.push(format!(
                "{} {verb} {name} too, but nothing reaches {pronoun}.",
                text::names_text(&unreachable, all),
            ));
        }
        if !behind.is_empty() {
            let (verb, pronoun) = verb_and_pronoun(&behind);
            notes.push(format!(
                "{} {verb} {name} too, but only {name} leads to {pronoun}.",
                text::names_text(&behind, all),
            ));
        }
        notes.push(match self.tree.parent(node) {
            Some(parent) => format!("Removing {} would remove {name} too.", parent.name(binary)),
            None => no_single_keeper(name),
        });
        TreeAnswer {
            lead: vec![lead(binary, lead_name, keeping, shown.len())],
            columns: vec![("HOW", Align::Left), ("ITEM", Align::Left)],
            rows: chains.rows(shape),
            shape,
            notes,
        }
    }
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

/// Says that nothing reaches SYMBOL, called `name` in this first line of
/// the answer, nor the items that use it: as many as [`text::names_text`]
/// names, or with `all`, every one.
fn unreachable_lines(name: &str, users: &[&str], all: bool) -> Vec<String> {
    let mut lines = vec![text::nothing_reaches(name)];
    if !users.is_empty() {
        let (verb, pronoun) = verb_and_pronoun(users);
        lines.push(format!(
            "{} {verb} it, but nothing reaches {pronoun} either.",
            text::names_text(users, all),
        ));
    }
    lines
}

/// The first line of the answer: what keeps SYMBOL, called `name` here,
/// through the chains to the nearest `shown` of the items that keep it.
fn lead(binary: &Binary, name: &str, keeping: &[usize], shown: usize) -> String {
    if let [user] = keeping {
        return format!(
            "{name} is used by {}, through this chain:",
            binary.items[*user].name
        );
    }
    let nodes: Vec<Node> = keeping.iter().map(|&user| Node::Item(user)).collect();
    let users = text::count_text(binary, &nodes);
    if shown == keeping.len() {
        format!("{name} is used by {users}, through these chains:")
    } else {
        format!("{name} is used by {users}; these are the chains through the nearest {shown}:")
    }
}

/// The chains to the items that keep SYMBOL, merged into a tree: each item
/// hangs under the item that uses it on the way from a root, and SYMBOL
/// under each item that keeps it.
struct Chains<'n, 'a> {
    binary: &'a Binary,
    graph: &'n Graph,
    /// The names that find each item, for the short names of the rows.
    names: &'n Names<'a>,
    /// The roots that the chains start from, and why each is one.
    roots: Vec<Root>,
    /// The items each item leads to, in the tree.
    children: BTreeMap<Node, Vec<Node>>,
}

impl<'n, 'a> Chains<'n, 'a> {
    fn new(
        binary: &'a Binary,
        graph: &'n Graph,
        names: &'n Names<'a>,
        paths: &ShortestPaths,
        symbol: Node,
        keeping: &[usize],
    ) -> Chains<'n, 'a> {
        let mut chains = Chains {
            binary,
            graph,
            names,
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

    /// The tree's rows, for a table that shows them in `shape`: each root,
    /// and under it the chains that start there.
    fn rows(&self, shape: TreeShape) -> Vec<TreeRow> {
        self.roots
            .iter()
            .map(|root| self.row(text::root_column(self.binary, root.kind), root.node, shape))
            .collect()
    }

    /// The row of `node`, whose HOW column says `how`, with the rows of its
    /// children under it. As rows, those call it by its short name.
    fn row(&self, how: String, node: Node, shape: TreeShape) -> TreeRow {
        let children = self.children_rows(node, shape);
        let name = node.name(self.binary);
        // Finding a short name takes a while, so only rows that show it
        // find it.
        let short_name = if children.is_empty() || shape == TreeShape::Drawn {
            name.to_string()
        } else {
            self.names.short_name(node)
        };
        TreeRow {
            short_name,
            ..TreeRow::new(vec![how], name, children)
        }
    }

    /// The rows of `node`'s children, each with the rows of its own.
    fn children_rows(&self, node: Node, shape: TreeShape) -> Vec<TreeRow> {
        let Some(children) = self.children.get(&node) else {
            return Vec::new();
        };
        children
            .iter()
            .map(|&child| {
                let how = hows_between(self.graph, node, child, |r| {
                    text::how_column(self.binary, r.to, r.kind)
                });
                self.row(how, child, shape)
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
