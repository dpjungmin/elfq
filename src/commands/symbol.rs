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
//!
//! For `callers` and `callees`, SYMBOL can also be one or more ranges of
//! memory instead (see [`subject`]).

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};

use super::text;
use crate::binary::{AddressRange, Binary, demangle, hex_number, short_names};
use crate::graph::Node;

/// What `callers` and `callees` ask about.
pub enum Subject {
    /// One item or import.
    Node(Node),
    /// Everything in some ranges of memory, together.
    Ranges(Vec<AddressRange>),
}

/// Works out what `queries` ask about: one SYMBOL (see [`find`]), or one or
/// more ranges of memory.
pub fn subject(binary: &Binary, queries: &[String]) -> Result<Subject> {
    if let [query] = queries
        && range(query).is_none()
    {
        return Ok(Subject::Node(find(binary, query)?));
    }
    let ranges = queries
        .iter()
        .map(|query| match range(query) {
            Some(range) => range,
            None => bail!("`{query}` isn't a range; give one SYMBOL, or ranges such as 0x0+256K"),
        })
        .collect::<Result<_>>()?;
    Ok(Subject::Ranges(ranges))
}

/// The range that `query` gives, if it gives one (see [`is_range`]):
/// `START..END` or `START+SIZE`, as `--range` takes it.
fn range(query: &str) -> Option<Result<AddressRange>> {
    is_range(query).then(|| {
        query
            .parse()
            .map_err(|error| anyhow!("`{query}` isn't a range: {error}"))
    })
}

/// Whether `query` is meant as a range of memory rather than a symbol: it
/// starts with a digit, as no name does, and has `..` or `+` in it.
pub fn is_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && (query.contains("..") || query.contains('+'))
}

/// Finds the item or import that `query` names, as `Names::find` does.
pub fn find(binary: &Binary, query: &str) -> Result<Node> {
    Names::new(binary).find(query)
}

/// Every name that finds an item or import, and what each one finds.
/// Working that out walks every name in the file, which takes a while in a
/// large one, so it is done the first time a question needs it, and once:
/// build one `Names` and ask it everything.
pub(super) struct Names<'a> {
    binary: &'a Binary,
    /// Names of items and imports, and aliases of items.
    full: OnceCell<HashMap<&'a str, Vec<Node>>>,
    /// Short names of items, built the first time a query isn't a full name.
    short: OnceCell<HashMap<String, Vec<Node>>>,
    /// The PLT stubs that jump to the file's own functions. Only their short
    /// names that end in `@plt` find them (see [`Names::short_names_of`]),
    /// and they are never suggested, since a name like theirs means the
    /// function.
    own_function_stubs: OnceCell<HashSet<usize>>,
}

impl<'a> Names<'a> {
    pub(super) fn new(binary: &'a Binary) -> Names<'a> {
        Names {
            binary,
            full: OnceCell::new(),
            short: OnceCell::new(),
            own_function_stubs: OnceCell::new(),
        }
    }

    /// Finds the item or import that `query` names. When it could mean
    /// several, or none, the error lists names to try instead.
    pub(super) fn find(&self, query: &str) -> Result<Node> {
        if let Some(address) = hex_number(query) {
            let address =
                address.with_context(|| format!("`{query}` is not a hexadecimal address"))?;
            return match self.binary.item_at(address) {
                Some(index) => Ok(Node::Item(index)),
                None => bail!("no function or data object contains address {address:#x}"),
            };
        }

        match self.nodes(query) {
            [node] => Ok(*node),
            [] => {
                let similar = self.similar(query);
                if similar.is_empty() {
                    bail!("no function or data object is named `{query}`");
                }
                bail!(
                    "no function or data object is named `{query}`; did you mean {}?",
                    similar.join(", ")
                );
            }
            nodes => bail!("{}", self.choices(query, nodes)),
        }
    }

    /// What `query` finds: the items and imports with that name, or if
    /// there are none, the items with that short name.
    pub(super) fn nodes(&self, query: &str) -> &[Node] {
        self.full()
            .get(query)
            .or_else(|| self.short().get(query))
            .map_or(&[], Vec::as_slice)
    }

    /// The names of items and imports, and aliases of items, and what each
    /// one finds.
    fn full(&self) -> &HashMap<&'a str, Vec<Node>> {
        self.full.get_or_init(|| {
            let mut full: HashMap<&str, Vec<Node>> = HashMap::new();
            for (index, item) in self.binary.items.iter().enumerate() {
                for name in item.names() {
                    add(full.entry(name).or_default(), Node::Item(index));
                }
            }
            for (index, import) in self.binary.imports.iter().enumerate() {
                let stub = full
                    .get(format!("{import}@plt").as_str())
                    .and_then(|nodes| nodes.first().copied());
                let node = stub.unwrap_or(Node::Import(index));
                add(full.entry(import).or_default(), node);
            }
            full
        })
    }

    /// The PLT stubs that jump to the file's own functions.
    fn own_function_stubs(&self) -> &HashSet<usize> {
        self.own_function_stubs
            .get_or_init(|| self.binary.own_function_stubs().into_keys().collect())
    }

    /// The short names of items, and what each one finds.
    fn short(&self) -> &HashMap<String, Vec<Node>> {
        self.short.get_or_init(|| {
            let mut short: HashMap<String, Vec<Node>> = HashMap::new();
            for index in 0..self.binary.items.len() {
                for short_name in self.short_names_of(index) {
                    add(short.entry(short_name).or_default(), Node::Item(index));
                }
            }
            short
        })
    }

    /// The short names of item `index` (see [`short_names`]). A stub for one
    /// of the file's own functions keeps only those that end in `@plt`:
    /// `app::log(unsigned int)@plt` is also `log(unsigned int)@plt`, but not
    /// `log`, which means the function.
    fn short_names_of(&self, index: usize) -> Vec<String> {
        let own_function_stub = self.own_function_stubs().contains(&index);
        self.binary.items[index]
            .names()
            .flat_map(short_names)
            .filter(|short_name| !own_function_stub || short_name.ends_with("@plt"))
            .collect()
    }

    /// The shortest thing to type that finds only `node`: a short name, a
    /// name other than a mangled symbol, or failing those, an item's
    /// address.
    pub(super) fn query_for(&self, node: Node) -> String {
        let names: Vec<&str> = match node {
            Node::Item(index) => self.binary.items[index].names().collect(),
            Node::Import(index) => vec![&self.binary.imports[index]],
        };
        self.shortest_name(node, names)
            .unwrap_or_else(|| match node {
                Node::Item(index) => format!("{:#x}", self.binary.items[index].address),
                Node::Import(_) => node.name(self.binary).to_string(),
            })
    }

    /// The shortest form of the name that `node`'s row shows that finds
    /// only `node`, or failing that, that whole name: what a row says to
    /// name the row it hangs under, such as `crc32` in "crc_table, kept by
    /// crc32". Unlike [`Names::query_for`], it never picks an alias, which
    /// no row shows.
    pub(super) fn short_name(&self, node: Node) -> String {
        let name = node.name(self.binary);
        self.shortest_name(node, vec![name])
            .unwrap_or_else(|| name.to_string())
    }

    /// The shortest of `names` and their short names that finds only
    /// `node`, leaving out mangled symbols, if one does.
    fn shortest_name(&self, node: Node, names: Vec<&str>) -> Option<String> {
        // A stub for one of the file's own functions has the function's
        // mangled names as aliases, with `@plt` added.
        let mut queries: Vec<String> = names
            .into_iter()
            .filter(|name| demangle(name.strip_suffix("@plt").unwrap_or(name)).is_none())
            .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
            .collect();
        queries.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        queries
            .into_iter()
            .find(|query| self.nodes(query) == [node])
    }

    /// The message for a query that finds several things: what to type for
    /// each, next to its full name.
    fn choices(&self, query: &str, nodes: &[Node]) -> String {
        const SHOWN: usize = 10;
        let shown = text::shown_count(nodes.len(), SHOWN);
        let rows: Vec<(String, &str)> = nodes[..shown]
            .iter()
            .map(|&node| {
                let query = text::shell_quote(&self.query_for(node));
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
            text::count_text(self.binary, nodes)
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

        // A stub for one of the file's own functions is left out: its
        // function is the one to suggest.
        let items = (0..self.binary.items.len())
            .filter(|index| !self.own_function_stubs().contains(index))
            .map(|index| {
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
            let suggestion = text::shell_quote(&suggestion);
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

/// Adds `node` to what a name finds, unless it is there already.
fn add(nodes: &mut Vec<Node>, node: Node) {
    if !nodes.contains(&node) {
        nodes.push(node);
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
    fn a_query_that_starts_with_a_digit_can_be_a_range() {
        let range = |query| range(query).map(|range| range.map_err(|error| error.to_string()));
        let itcm = AddressRange {
            start: 0,
            end: 0x40000,
        };
        assert_eq!(range("0x0+256K"), Some(Ok(itcm)));
        assert_eq!(range("0..0x40000"), Some(Ok(itcm)));
        assert_eq!(
            range("0x40000..0x0"),
            Some(Err(
                "`0x40000..0x0` isn't a range: the range must end after it starts".to_string()
            ))
        );
        // An address, and names, even with `+` in them.
        assert_eq!(range("0x1700"), None);
        assert_eq!(range("operator+"), None);
        assert_eq!(range("Vec<T>::operator+="), None);
    }

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
