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
//! When SYMBOL gives another of an item's names, or an address inside it,
//! the answer's first line says so (see [`Symbol::lead_name`]).
//!
//! For `callers` and `callees`, SYMBOL can also be one or more ranges of
//! memory instead (see [`subject`]).

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};

use super::text;
use crate::binary::{
    AddressRange, Binary, ItemKind, demangle, example_range, hex_number, short_names,
};
use crate::debuginfo::{self, InlinedCopy};
use crate::graph::Node;

/// The item or import that SYMBOL finds.
#[derive(Debug)]
pub struct Symbol {
    pub node: Node,
    /// What the answer's first line adds to the item's name when SYMBOL
    /// gave something other than that name: another of its names, as in
    /// "also named snprintf", or an address inside it, as in "which holds
    /// 0x30000290".
    pub note: Option<String>,
}

impl Symbol {
    /// The item's name as the answer's first line gives it, with the note
    /// if there is one: `sniprintf (also named snprintf)`.
    pub fn lead_name(&self, binary: &Binary) -> String {
        let name = self.node.name(binary);
        match &self.note {
            Some(note) => format!("{name} ({note})"),
            None => name.to_string(),
        }
    }
}

/// What `callers` and `callees` ask about.
pub enum Subject {
    /// One item or import.
    Symbol(Symbol),
    /// Everything in some ranges of memory, together.
    Ranges(Vec<AddressRange>),
}

/// Works out what `queries` ask about: one SYMBOL (see [`find`]), or one or
/// more ranges of memory.
pub fn subject(binary: &Binary, queries: &[String]) -> Result<Subject> {
    if let [query] = queries
        && range(query).is_none()
    {
        check_not_a_range(query)?;
        return Ok(Subject::Symbol(find(binary, query)?));
    }
    let ranges = queries
        .iter()
        .map(|query| match range(query) {
            Some(range) => range,
            None => {
                check_not_a_range(query)?;
                bail!("`{query}` isn't a range; give one SYMBOL, or ranges such as 0x0+256K")
            }
        })
        .collect::<Result<_>>()?;
    Ok(Subject::Ranges(ranges))
}

/// The range that `query` gives, if it gives one (see [`is_range`]):
/// `START..END` or `START+SIZE`, as `--range` takes it. Ranges joined by
/// commas are an error that says to give each on its own.
fn range(query: &str) -> Option<Result<AddressRange>> {
    is_range(query).then(|| {
        if !query.contains(',') {
            return query
                .parse()
                .map_err(|error| anyhow!("`{query}` isn't a range: {error}"));
        }
        let parts: Vec<&str> = query
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        for part in &parts {
            if let Err(error) = part.parse::<AddressRange>() {
                bail!("`{part}` isn't a range: {error}");
            }
        }
        match parts[..] {
            [_] => bail!("`{query}` isn't a range; leave out the comma"),
            _ => bail!(
                "`{query}` isn't a range; give each range as an argument of its own: {}",
                parts.join(" ")
            ),
        }
    })
}

/// Refuses `query` when it is a range typed with `-` or `:` between its
/// ends, as in `0x0-0x40000`, which neither a range nor SYMBOL can be:
/// no name starts with a digit, and no address has `-` or `:` in it.
fn check_not_a_range(query: &str) -> Result<()> {
    if query.starts_with(|c: char| c.is_ascii_digit()) && query.contains(['-', ':']) {
        bail!(
            "`{query}` isn't a range or an address; a range is START..END or START+SIZE, such \
             as {}",
            example_range(query)
        );
    }
    Ok(())
}

/// Whether `query` is meant as a range of memory rather than a symbol: it
/// starts with a digit, as no name does, and has `..` or `+` in it.
pub fn is_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && (query.contains("..") || query.contains('+'))
}

/// Finds the item or import that `query` names, as `Names::find` does.
pub fn find(binary: &Binary, query: &str) -> Result<Symbol> {
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
    pub(super) fn find(&self, query: &str) -> Result<Symbol> {
        if let Some(address) = hex_number(query) {
            let address =
                address.with_context(|| format!("`{query}` is not a hexadecimal address"))?;
            let Some(index) = self.binary.item_at(address) else {
                bail!("no function or data object contains address {address:#x}");
            };
            // The item's start needs no note: it is what the item's name
            // stands for. So is a Thumb function's start with bit 0 set, as
            // symbols, pointers and the entry point give it.
            let item = &self.binary.items[index];
            let thumb_start =
                self.binary.thumb && item.kind == ItemKind::Function && address == item.address | 1;
            let note = (address != item.address && !thumb_start)
                .then(|| format!("which holds {address:#x}"));
            return Ok(Symbol {
                node: Node::Item(index),
                note,
            });
        }

        match self.nodes(query) {
            &[node] => Ok(Symbol {
                node,
                note: self
                    .other_name(node, query)
                    .map(|name| format!("also named {name}")),
            }),
            [] => {
                let mut message = format!("no function or data object is named `{query}`");
                if let Some(reason) = self.unnamed().or_else(|| self.inlined_everywhere(query)) {
                    message += &format!("; {reason}");
                }
                let similar = self.similar(query);
                if !similar.is_empty() {
                    message += &format!("; did you mean {}?", similar.join(", "));
                }
                bail!("{message}");
            }
            nodes => bail!("{}", self.choices(query, nodes)),
        }
    }

    /// Where the compiler inlined every copy of the function that `query`
    /// names, as the DWARF says: the reason that no item has the name.
    /// `None` without DWARF, when `query` ends the names of several inlined
    /// functions, or when the compiler kept part of the function out of
    /// line, as `checksum.part.0` for `checksum`.
    fn inlined_everywhere(&self, query: &str) -> Option<String> {
        if !self.binary.has_dwarf {
            return None;
        }
        let all_copies = debuginfo::inlined_copies(self.binary).ok()?;
        let copies: Vec<&InlinedCopy> = all_copies
            .iter()
            .filter(|copy| copy.name == query || short_names(&copy.name).iter().any(|s| s == query))
            .collect();
        let name = &copies.first()?.name;
        if copies.iter().any(|copy| &copy.name != name) {
            return None;
        }
        // GCC names what it keeps out of line after the function, as
        // `checksum.part.0`, or in C++, `checksum(char const*) [clone
        // .part.0]`.
        let kept = self.binary.items.iter().any(|item| {
            item.name
                .strip_prefix(name.as_str())
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with(" [clone ."))
        });
        if kept {
            return None;
        }
        // The functions that hold the copies, those with the most first.
        let mut into: BTreeMap<usize, usize> = BTreeMap::new();
        for copy in &copies {
            *into.entry(copy.into).or_default() += 1;
        }
        let mut into: Vec<(usize, Node)> = into
            .into_iter()
            .map(|(index, count)| (count, Node::Item(index)))
            .collect();
        into.sort_by_key(|&(count, node)| (Reverse(count), node.name(self.binary)));
        let names: Vec<String> = into
            .iter()
            .map(|&(_, node)| self.short_name(node))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        // `inlined` lists the largest of what a function holds, which can
        // leave this one out without `--all`.
        let (_, first) = into[0];
        let all = match first {
            Node::Item(index) if !super::inlined::shows(&all_copies, index, name) => " --all",
            _ => "",
        };
        let command = format!(
            "`inlined {}{all}`",
            text::shell_quote(&self.query_for(first))
        );
        let copies = match copies.len() {
            1 => "its one copy".to_string(),
            2 => "both copies of it".to_string(),
            count => format!("all {count} copies of it"),
        };
        Some(match names[..] {
            [function] => {
                format!("the compiler inlined {copies} into {function}, which {command} lists")
            }
            _ => format!(
                "the compiler inlined {copies} into {}; {command} lists those in {}",
                text::names_text(&names, false),
                names[0]
            ),
        })
    }

    /// How elfq names items in a file without a symbol table, with
    /// examples, or why it found no functions in it: the likely reason that
    /// no item has a name. `None` in a file with a symbol table.
    fn unnamed(&self) -> Option<String> {
        if self.binary.has_symbols {
            return None;
        }
        if !super::found_functions(self.binary) {
            return Some(
                "the file has no symbols or unwind table, so elfq can't find its functions yet"
                    .to_string(),
            );
        }
        let names = match text::unnamed_items(self.binary)? {
            (function, Some(data)) => {
                format!(
                    "functions and data by address, like {} and {}",
                    function.name, data.name
                )
            }
            (function, None) => format!("functions by address, like {}", function.name),
        };
        Some(format!(
            "the file has no symbol table, so elfq names its {names}"
        ))
    }

    /// The other name of `node` that `query` gives, if it gives one rather
    /// than the item's name: an alias, or the alias that a short name gives.
    /// A mangled symbol stands for its readable name, which is no other name
    /// when it is the item's.
    fn other_name(&self, node: Node, query: &str) -> Option<String> {
        let Node::Item(index) = node else {
            return None;
        };
        let item = &self.binary.items[index];
        let gives = |name: &str| name == query || short_names(name).iter().any(|s| s == query);
        if gives(&item.name) {
            return None;
        }
        let alias = readable(item.aliases.iter().find(|alias| gives(alias))?);
        (alias != item.name).then_some(alias)
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
    /// each, next to its full name. An item whose shortest name is longer
    /// than 40 characters gets its address instead, so that the names on the
    /// right line up close by.
    fn choices(&self, query: &str, nodes: &[Node]) -> String {
        const SHOWN: usize = 10;
        const LONGEST: usize = 40;
        let shown = text::shown_count(nodes.len(), SHOWN);
        let rows: Vec<(String, &str)> = nodes[..shown]
            .iter()
            .map(|&node| {
                let query = match (self.query_for(node), node) {
                    (query, Node::Item(index)) if query.chars().count() > LONGEST => {
                        format!("{:#x}", self.binary.items[index].address)
                    }
                    (query, _) => text::shell_quote(&query),
                };
                (query, node.name(self.binary))
            })
            .collect();
        let width = rows
            .iter()
            .map(|(query, _)| query.chars().count())
            .max()
            .unwrap_or(0);

        let is_address = |query: &str| hex_number(query).is_some();
        let names = rows.iter().any(|(query, _)| !is_address(query));
        let addresses = rows.iter().any(|(query, _)| is_address(query));
        let what = match (names, addresses) {
            (true, true) => "names or addresses",
            (false, true) => "addresses",
            _ => "names",
        };
        let mut message = format!(
            "`{query}` could mean {}; use one of the {what} on the left instead:",
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
        // How close the closest of `names` and their short names comes, and
        // those of them to suggest: the fewest typos from `query`, and of
        // those, the ones that contain it. So `SV_Handler` gets
        // `SVC_Handler`, not `PendSV_Handler`, and `iprintf` gets
        // `fiprintf`, not `fprintf`.
        let closest = |names: Vec<&str>| {
            let close: Vec<(usize, String)> = names
                .into_iter()
                .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
                .filter_map(|name| Some((distance(&name)?, name)))
                .collect();
            let nearest = close.iter().map(|(distance, _)| *distance).min()?;
            let rank = |(distance, name): &(usize, String)| {
                let typos_from = edit_distance(&name.to_lowercase(), &query).min(typos + 1);
                (typos_from, *distance > 0)
            };
            let best = close.iter().map(rank).min()?;
            let closest: Vec<String> = close
                .iter()
                .filter(|name| rank(name) == best)
                .map(|(_, name)| name.clone())
                .collect();
            Some((nearest, closest))
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
        let mut similar: Vec<(usize, usize, &str, Node, Vec<String>)> = items
            .chain(imports)
            .filter_map(|(node, name, names)| {
                let (distance, closest) = closest(names)?;
                Some((distance, name.len(), name, node, closest))
            })
            .collect();
        similar.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));

        let mut suggestions: Vec<String> = Vec::new();
        for (distance, _, _, node, closest) in similar {
            // The shortest of the names that came closest, which needn't be
            // the item's shortest name: `sniprintf` for `sniprin`, though
            // its alias `snprintf` is shorter. Failing that, when none of them
            // finds only this item, or they are all mangled symbols, the
            // shortest thing to type that does, but not one that has `query`
            // only in a part that it leaves out, such as a generic argument.
            let suggestion = closest
                .into_iter()
                .filter(|name| demangle(name).is_none() && self.nodes(name) == [node])
                .min_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)))
                .or_else(|| {
                    let query_for = self.query_for(node);
                    (distance > 0 || query_for.to_lowercase().contains(&query)).then_some(query_for)
                });
            let Some(suggestion) = suggestion else {
                continue;
            };
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

/// `name` demangled, if it is a mangled symbol, as an alias can be: with
/// `@plt` after it for one of a stub's.
fn readable(name: &str) -> String {
    let (symbol, plt) = match name.strip_suffix("@plt") {
        Some(symbol) => (symbol, "@plt"),
        None => (name, ""),
    };
    demangle(symbol).map_or_else(|| name.to_string(), |readable| readable + plt)
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
