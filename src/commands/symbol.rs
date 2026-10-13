//! Working out what the user means by SYMBOL on the command line.
//!
//! SYMBOL can be any of these, tried in order:
//!
//! 1. an address inside an item, such as `0x1700`;
//! 2. the name of an item or an import, or one of an item's aliases, which
//!    include the symbol of a demangled name. An import's name finds its PLT
//!    stub if it has one, since the stub is what code calls;
//! 3. a short name: the end of an item's path, as in `process_frame` for
//!    `app::process_frame`, or `Gain::run` for
//!    `<app::Gain as app::Stage>::run` (see [`short_names`]).

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::binary::{Binary, demangle, short_names};
use crate::graph::Node;

/// Finds the item or import that `query` names. When it could mean several,
/// or none, the error lists names to try instead.
pub fn find(binary: &Binary, query: &str) -> Result<Node> {
    if let Some(hex) = query.strip_prefix("0x") {
        let address = u64::from_str_radix(hex, 16)
            .with_context(|| format!("`{query}` is not a hexadecimal address"))?;
        return match binary.item_at(address) {
            Some(index) => Ok(Node::Item(index)),
            None => bail!("no function or data object contains address {address:#x}"),
        };
    }

    let names = Names::new(binary);
    match names.nodes(query) {
        [node] => Ok(*node),
        [] => {
            let similar = names.similar(query);
            if similar.is_empty() {
                bail!("no function or data object is named `{query}`");
            }
            bail!(
                "no function or data object is named `{query}`; did you mean {}?",
                similar.join(", ")
            );
        }
        nodes => bail!("{}", names.choices(query, nodes)),
    }
}

/// Every name that finds an item or import, and what each one finds.
pub(super) struct Names<'a> {
    binary: &'a Binary,
    /// Names of items and imports, and aliases of items.
    full: HashMap<&'a str, Vec<Node>>,
    /// Short names of items.
    short: HashMap<String, Vec<Node>>,
}

impl<'a> Names<'a> {
    pub(super) fn new(binary: &'a Binary) -> Names<'a> {
        let mut full: HashMap<&str, Vec<Node>> = HashMap::new();
        let mut short: HashMap<String, Vec<Node>> = HashMap::new();
        let add = |nodes: &mut Vec<Node>, node| {
            if !nodes.contains(&node) {
                nodes.push(node);
            }
        };
        for (index, item) in binary.items.iter().enumerate() {
            for name in item.names() {
                add(full.entry(name).or_default(), Node::Item(index));
                for short_name in short_names(name) {
                    add(short.entry(short_name).or_default(), Node::Item(index));
                }
            }
        }
        for (index, import) in binary.imports.iter().enumerate() {
            let stub = full
                .get(format!("{import}@plt").as_str())
                .and_then(|nodes| nodes.first().copied());
            let node = stub.unwrap_or(Node::Import(index));
            add(full.entry(import).or_default(), node);
        }
        Names {
            binary,
            full,
            short,
        }
    }

    /// What `query` finds: the items and imports with that name, or if
    /// there are none, the items with that short name.
    fn nodes(&self, query: &str) -> &[Node] {
        self.full
            .get(query)
            .or_else(|| self.short.get(query))
            .map_or(&[], Vec::as_slice)
    }

    /// The shortest thing to type that finds only `node`: a short name, a
    /// name other than a mangled symbol, or failing those, an item's
    /// address.
    pub(super) fn query_for(&self, node: Node) -> String {
        let (names, fallback): (Vec<&str>, String) = match node {
            Node::Item(index) => {
                let item = &self.binary.items[index];
                (item.names().collect(), format!("{:#x}", item.address))
            }
            Node::Import(index) => {
                let import = &self.binary.imports[index];
                (vec![import], import.clone())
            }
        };
        let mut queries: Vec<String> = names
            .into_iter()
            .filter(|name| demangle(name).is_none())
            .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
            .collect();
        queries.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        queries
            .into_iter()
            .find(|query| self.nodes(query) == [node])
            .unwrap_or(fallback)
    }

    /// The message for a query that finds several things: what to type for
    /// each, next to its full name.
    fn choices(&self, query: &str, nodes: &[Node]) -> String {
        const SHOWN: usize = 10;
        let shown = super::shown_count(nodes.len(), SHOWN);
        let rows: Vec<(String, &str)> = nodes[..shown]
            .iter()
            .map(|&node| {
                let query = super::shell_quote(&self.query_for(node));
                (query, node.name(self.binary))
            })
            .collect();
        let width = rows
            .iter()
            .map(|(query, _)| query.chars().count())
            .max()
            .unwrap_or(0);

        let mut message = format!(
            "`{query}` could mean {}; use one of the names on the left instead:",
            super::count_text(self.binary, nodes)
        );
        for (query, name) in rows {
            if query == name {
                message += &format!("\n  {query}");
            } else {
                message += &format!("\n  {query:width$}  {name}");
            }
        }
        if shown < nodes.len() {
            message += &format!("\n  and {} more", nodes.len() - shown);
        }
        message
    }

    /// Up to five things to type for items or imports whose names contain
    /// `query`, ignoring case, or differ from it by a typo or two.
    fn similar(&self, query: &str) -> Vec<String> {
        let query = query.to_lowercase();
        let typos = (query.chars().count() / 3).min(2);
        let distance = |name: &str| {
            let name = name.to_lowercase();
            if name.contains(&query) {
                return Some(0);
            }
            if name.chars().count().abs_diff(query.chars().count()) > typos {
                return None;
            }
            Some(edit_distance(&name, &query)).filter(|&distance| distance <= typos)
        };
        let closest = |names: Vec<&str>| {
            names
                .into_iter()
                .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
                .filter_map(|name| distance(&name))
                .min()
        };

        let items = (0..self.binary.items.len()).map(|index| {
            let item = &self.binary.items[index];
            (
                Node::Item(index),
                item.name.as_str(),
                item.names().collect(),
            )
        });
        let imports = (0..self.binary.imports.len()).map(|index| {
            let import = self.binary.imports[index].as_str();
            (Node::Import(index), import, vec![import])
        });
        // Closest first, then shortest.
        let mut similar: Vec<(usize, usize, &str, Node)> = items
            .chain(imports)
            .filter_map(|(node, name, names)| Some((closest(names)?, name.len(), name, node)))
            .collect();
        similar.sort();

        let mut suggestions: Vec<String> = Vec::new();
        for (distance, _, _, node) in similar {
            let suggestion = self.query_for(node);
            // Skip a name that has `query` only in a part that the
            // suggestion leaves out, such as a generic argument.
            if distance == 0 && !suggestion.to_lowercase().contains(&query) {
                continue;
            }
            let suggestion = super::shell_quote(&suggestion);
            if !suggestions.contains(&suggestion) {
                suggestions.push(suggestion);
            }
            if suggestions.len() == 5 {
                break;
            }
        }
        suggestions
    }
}

/// The number of typos that turn `a` into `b`, where a typo is a character
/// inserted, deleted or replaced, or two neighbors swapped (the optimal
/// string alignment distance).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // distance[i][j] is the distance between the first i characters of `a`
    // and the first j of `b`.
    let mut distance = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in distance.iter_mut().enumerate() {
        row[0] = i;
    }
    distance[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let replace = distance[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = replace
                .min(distance[i - 1][j] + 1)
                .min(distance[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(distance[i - 2][j - 2] + 1);
            }
            distance[i][j] = best;
        }
    }
    distance[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_typos() {
        assert_eq!(edit_distance("process_frame", "process_frame"), 0);
        assert_eq!(edit_distance("process_frame", "proces_frame"), 1);
        assert_eq!(edit_distance("log_msg", "log_mgs"), 1);
        assert_eq!(edit_distance("write", "wirte"), 1);
        assert_eq!(edit_distance("log_msg", "lgo_mgs"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
