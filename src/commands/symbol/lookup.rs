//! [`Names`]: what each name in the file finds, worked out once, and how
//! to name an item for the user in return.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use super::Symbol;
use super::not_found::Inlined;
use super::similar::{Lowercase, Suggestable};
use crate::binary::{Binary, ItemKind, hex_number, names};
use crate::graph::Node;

/// Every name that finds an item or import, and what each one finds.
/// Working that out walks every name in the file, which takes a while in a
/// large one, so it is done the first time a question needs it, and once:
/// build one `Names` and ask it everything.
///
/// To name an item for the user, pick by where the name goes:
///
/// - [`Names::query_for`]: what to type as SYMBOL to find it, any of its
///   names, or failing those, its address.
/// - [`Names::short_name`]: what a row says after "kept by": never an
///   alias, which no row shows, and failing a shorter form, the whole name.
/// - [`Names::row_name`]: what a row says after "used by": keeps a PLT
///   stub's `@plt`, and failing a shorter form, the address.
pub(in crate::commands) struct Names<'a> {
    pub(super) binary: &'a Binary,
    /// Names of items and imports, and aliases of items.
    full: OnceCell<HashMap<&'a str, Vec<Node>>>,
    /// For each key (see [`names::short_name_key`]), the items that one of
    /// their names files under it, and the short names with the key, worked
    /// out the first time a query that isn't a full name has it.
    filed: OnceCell<HashMap<String, Filed<'a>>>,
    /// Every short name of every item, worked out at once when many items
    /// are to be named (see [`Names::will_name`]).
    all_short: OnceCell<HashMap<String, Vec<Node>>>,
    /// Whether many items are to be named.
    many: Cell<bool>,
    /// The PLT stubs that jump to the file's own functions. Only their short
    /// names that end in `@plt` find them (see [`Names::short_names_of`]),
    /// and they are never suggested, since a name like theirs means the
    /// function.
    own_function_stubs: OnceCell<HashSet<usize>>,
    /// The functions that the compiler inlined, read from the DWARF the
    /// first time a query finds nothing; `None` when it can't be read.
    pub(super) inlined: OnceCell<Option<Inlined>>,
    /// Every name of every item and import in lowercase, made the first
    /// time a question needs it.
    pub(super) lowercase: OnceCell<Lowercase>,
    /// The short names that [`Names::similar`] compares a query with, made
    /// the first time a query finds nothing.
    pub(super) suggestable: OnceCell<Suggestable>,
    /// The row name of each item, by its index, kept the first time
    /// [`Names::row_name`] works it out: `reach` names an item after "used
    /// by" on the row of each item that it uses, and browse on each line
    /// that uses it. Short names are kept by those that ask for them again:
    /// browse's `Session::short_name` and diff's `RowNames`.
    row_names: RefCell<HashMap<usize, String>>,
}

/// The items filed under a key, and their short names with it.
struct Filed<'a> {
    /// The items with a name that may have a short name with the key: a
    /// few more than have one.
    items: Vec<usize>,
    /// The short names with the key, and what each finds.
    nodes: OnceCell<HashMap<String, Vec<Node>>>,
    /// The words of the items' names (see [`names::words`]), and the items
    /// that have each, as indices into `items`, for [`Names::finds_only`].
    words: OnceCell<Words<'a>>,
}

/// The words of some items' names, and the items that have each.
struct Words<'a> {
    with: HashMap<&'a str, Vec<usize>>,
    /// The items with a name that may have a short name with any words
    /// (see [`names::may_join_words`]).
    any: Vec<usize>,
}

impl<'a> Names<'a> {
    pub(in crate::commands) fn new(binary: &'a Binary) -> Names<'a> {
        Names {
            binary,
            full: OnceCell::new(),
            filed: OnceCell::new(),
            all_short: OnceCell::new(),
            many: Cell::new(false),
            own_function_stubs: OnceCell::new(),
            inlined: OnceCell::new(),
            lowercase: OnceCell::new(),
            suggestable: OnceCell::new(),
            row_names: RefCell::new(HashMap::new()),
        }
    }

    /// `main` if there is one, or else the function where the program
    /// starts, the vector table's reset handler or the entry point, since
    /// every reader knows where those fit; otherwise the largest function.
    /// The overview's Try lines and `reach` without SYMBOL start here.
    pub(in crate::commands) fn starting_point(&self) -> Option<usize> {
        let binary = self.binary;
        // `main` as SYMBOL would find it, so also `app::main` in Rust.
        let main = match self.nodes("main") {
            [Node::Item(index)] => Some(*index),
            _ => None,
        };
        let reset = binary
            .reset_handler()
            .and_then(|address| binary.item_at(address));
        let entry = binary.item_at(binary.entry);
        let largest = (0..binary.items.len())
            .filter(|&index| binary.items[index].is_own_function())
            .max_by_key(|&index| binary.items[index].size);
        [main, reset, entry]
            .into_iter()
            .flatten()
            .find(|&index| binary.items[index].is_own_function())
            .or(largest)
    }

    /// Finds the item or import that `query` names. When it could mean
    /// several, or none, the error lists names to try instead.
    pub(in crate::commands) fn find(&self, query: &str) -> Result<Symbol> {
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
            &[node] => Ok(self.found(node, query)),
            [] => {
                let mut message = format!("no function or data object is named `{query}`");
                if let Some(reason) = self.unnamed().or_else(|| self.inlined_reason(query)) {
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

    /// What `query` finds when it finds one item or import, as
    /// [`Names::find`] finds it, or `None`. Unlike `find`, it doesn't work
    /// out why `query` finds several or none, which can take seconds in a
    /// large file.
    #[cfg(feature = "tui")]
    pub(in crate::commands) fn find_one(&self, query: &str) -> Option<Symbol> {
        // An address finds its item without looking up a name.
        if hex_number(query).is_some() {
            return self.find(query).ok();
        }
        match self.nodes(query) {
            &[node] => Some(self.found(node, query)),
            _ => None,
        }
    }

    /// The answer for `query` when it finds only `node`.
    fn found(&self, node: Node, query: &str) -> Symbol {
        Symbol {
            node,
            note: self
                .other_name(node, query)
                .map(|name| format!("also named {name}")),
        }
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
        let gives =
            |name: &str| name == query || names::short_names(name).iter().any(|s| s == query);
        if gives(&item.name) {
            return None;
        }
        let alias = readable(item.aliases.iter().find(|alias| gives(alias))?);
        (alias != item.name).then_some(alias)
    }

    /// What `query` finds: the items and imports with that name, or if
    /// there are none, the items with that short name.
    pub(in crate::commands) fn nodes(&self, query: &str) -> &[Node] {
        if let Some(nodes) = self.full().get(query) {
            return nodes;
        }
        if self.many.get() {
            return self.all_short().get(query).map_or(&[], Vec::as_slice);
        }
        // Working out every short name of every item takes a while in a
        // large file: only those of the items filed under `query`'s key
        // are worked out, once.
        let key = names::short_name_key(query);
        let Some(filed) = key.and_then(|key| self.filed().get(key)) else {
            return &[];
        };
        let short = filed.nodes.get_or_init(|| {
            let mut short: HashMap<String, Vec<Node>> = HashMap::new();
            for &index in &filed.items {
                for short_name in self.short_names_of(index, key) {
                    add(short.entry(short_name).or_default(), Node::Item(index));
                }
            }
            short
        });
        short.get(query).map_or(&[], Vec::as_slice)
    }

    /// Whether `query` finds only `node`, as [`Names::nodes`] would say.
    /// When many items are filed under its key, it works out the short
    /// names of only those whose names have every word of `query`, until
    /// it finds one that isn't `node`. That is quicker than working out all
    /// of theirs, as for `fmt`, which thousands of Rust functions end with.
    pub(super) fn finds_only(&self, query: &str, node: Node) -> bool {
        const MANY: usize = 100;
        if self.full().contains_key(query) || self.many.get() {
            return self.nodes(query) == [node];
        }
        let key = names::short_name_key(query);
        let Some(filed) = key.and_then(|key| self.filed().get(key)) else {
            return false;
        };
        if filed.items.len() <= MANY || filed.nodes.get().is_some() {
            return self.nodes(query) == [node];
        }
        let by_word = filed.words.get_or_init(|| {
            let mut by_word = Words {
                with: HashMap::new(),
                any: Vec::new(),
            };
            for (at, &index) in filed.items.iter().enumerate() {
                // A name that can't be shorter has no short names, as a
                // mangled symbol has none.
                let item_names = self.binary.items[index]
                    .names()
                    .filter(|name| names::can_be_shorter(name));
                for name in item_names {
                    if names::may_join_words(name) {
                        add_index(&mut by_word.any, at);
                    }
                    for word in names::words(name) {
                        add_index(by_word.with.entry(word).or_default(), at);
                    }
                }
            }
            by_word
        });
        // The items whose names have the word of `query` that the fewest
        // have, or every item when it has no words.
        let query_words: Vec<&str> = names::words(query).collect();
        let all: Vec<usize>;
        let with = match query_words
            .iter()
            .map(|word| by_word.with.get(word).map_or(&[][..], Vec::as_slice))
            .min_by_key(|with| with.len())
        {
            Some(with) => with,
            None => {
                all = (0..filed.items.len()).collect();
                &all
            }
        };
        // Whether item `index` has `query` as a short name, as
        // [`Names::short_names_of`] gives them.
        let has = |index: usize| {
            let item_names = || self.binary.items[index].names();
            let may_have = |name: &str| {
                names::may_join_words(name) || query_words.iter().all(|word| name.contains(word))
            };
            if self.own_function_stubs().contains(&index) && !query.ends_with("@plt")
                || !item_names().any(may_have)
            {
                return false;
            }
            item_names().any(|name| {
                names::ends_with_short_name(name, query) || names::has_short_name(name, query)
            })
        };
        let mut found = false;
        for &at in with.iter().chain(&by_word.any) {
            let index = filed.items[at];
            if has(index) {
                if node != Node::Item(index) {
                    return false;
                }
                found = true;
            }
        }
        found
    }

    /// Says that about `count` items are to be named, as `top --all` names
    /// thousands. When that is many of the file's items, every short name
    /// is worked out at once, the first time one is needed after this,
    /// which is quicker than key by key.
    pub(in crate::commands) fn will_name(&self, count: usize) {
        if count > self.binary.items.len() / 4 {
            self.many.set(true);
        }
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
                let nodes = full.entry(import).or_default();
                // A stub can have the import's name already.
                if !nodes.contains(&node) {
                    nodes.push(node);
                }
            }
            full
        })
    }

    /// The PLT stubs that jump to the file's own functions.
    pub(super) fn own_function_stubs(&self) -> &HashSet<usize> {
        self.own_function_stubs
            .get_or_init(|| self.binary.own_function_stubs().into_keys().collect())
    }

    /// Every short name of every item, and what each finds.
    fn all_short(&self) -> &HashMap<String, Vec<Node>> {
        self.all_short.get_or_init(|| {
            let mut short: HashMap<String, Vec<Node>> = HashMap::new();
            for index in 0..self.binary.items.len() {
                for short_name in self.short_names_of(index, None) {
                    add(short.entry(short_name).or_default(), Node::Item(index));
                }
            }
            short
        })
    }

    /// The items filed under each key.
    fn filed(&self) -> &HashMap<String, Filed<'a>> {
        self.filed.get_or_init(|| {
            let mut filed: HashMap<String, Filed> = HashMap::new();
            for (index, item) in self.binary.items.iter().enumerate() {
                for key in item.names().flat_map(names::short_name_keys) {
                    let items = &mut filed
                        .entry(key)
                        .or_insert_with(|| Filed {
                            items: Vec::new(),
                            nodes: OnceCell::new(),
                            words: OnceCell::new(),
                        })
                        .items;
                    if items.last() != Some(&index) {
                        items.push(index);
                    }
                }
            }
            filed
        })
    }

    /// The short names of item `index` (see [`names::short_names`]), or
    /// with a key, only those that have it. A stub for one of the file's
    /// own functions keeps only those that end in `@plt`:
    /// `app::log(unsigned int)@plt` is also `log(unsigned int)@plt`, but
    /// not `log`, which means the function.
    fn short_names_of(&self, index: usize, key: Option<&str>) -> Vec<String> {
        let own_function_stub = self.own_function_stubs().contains(&index);
        self.binary.items[index]
            .names()
            .flat_map(|name| match key {
                Some(key) => names::short_names_with_key(name, key),
                None => names::short_names(name),
            })
            .filter(|short_name| !own_function_stub || short_name.ends_with("@plt"))
            .collect()
    }

    /// The shortest thing to type that finds only `node`: a short name, a
    /// name other than a mangled symbol, or failing those, an item's
    /// address.
    pub(in crate::commands) fn query_for(&self, node: Node) -> String {
        self.shortest_name(node, self.names_of(node), |_| true)
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
    pub(in crate::commands) fn short_name(&self, node: Node) -> String {
        let name = node.name(self.binary);
        self.shortest_name(node, vec![name], |_| true)
            .unwrap_or_else(|| name.to_string())
    }

    /// What a row calls the item at `index` after "used by", so that it
    /// works as SYMBOL: the shortest form of its name that finds only the
    /// item, as [`Names::short_name`] picks it but keeping a PLT stub's
    /// `@plt`, or else its address, as when two functions share a name.
    /// It is worked out once for each item.
    pub(in crate::commands) fn row_name(&self, index: usize) -> String {
        if let Some(row_name) = self.row_names.borrow().get(&index) {
            return row_name.clone();
        }
        let node = Node::Item(index);
        let name = node.name(self.binary);
        // Without `@plt`, a stub's name means what the stub passes calls
        // to, which reads as another item: `strlen, used by strlen`.
        let stub = name.ends_with("@plt");
        let row_name = self
            .shortest_name(node, vec![name], |query| !stub || query.ends_with("@plt"))
            .unwrap_or_else(|| format!("{:#x}", self.binary.items[index].address));
        self.row_names.borrow_mut().insert(index, row_name.clone());
        row_name
    }

    /// The shortest of `whole_names` and their short names that finds only
    /// `node` and that `keep` keeps, leaving out mangled symbols, if one
    /// does.
    fn shortest_name(
        &self,
        node: Node,
        whole_names: Vec<&str>,
        keep: impl Fn(&str) -> bool,
    ) -> Option<String> {
        // A stub for one of the file's own functions has the function's
        // mangled names as aliases, with `@plt` added.
        let mut queries: Vec<String> = whole_names
            .into_iter()
            .filter(|name| names::demangle(name.strip_suffix("@plt").unwrap_or(name)).is_none())
            .flat_map(|name| {
                names::short_names(name)
                    .into_iter()
                    .chain([name.to_string()])
            })
            .filter(|query| keep(query))
            .collect();
        queries.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        queries
            .into_iter()
            .find(|query| self.finds_only(query, node))
    }

    /// Where `node`'s names are in [`Lowercase`]'s `starts`.
    pub(super) fn position(&self, node: Node) -> usize {
        match node {
            Node::Item(index) => index,
            Node::Import(index) => self.binary.items.len() + index,
        }
    }

    /// The names of `node`: an item's name and aliases, or an import's
    /// name.
    pub(super) fn names_of(&self, node: Node) -> Vec<&'a str> {
        match node {
            Node::Item(index) => self.binary.items[index].names().collect(),
            Node::Import(index) => vec![&self.binary.imports[index]],
        }
    }
}

/// `name` demangled, if it is a mangled symbol, as an alias can be: with
/// `@plt` after it for one of a stub's.
fn readable(name: &str) -> String {
    let (symbol, plt) = match name.strip_suffix("@plt") {
        Some(symbol) => (symbol, "@plt"),
        None => (name, ""),
    };
    names::demangle(symbol).map_or_else(|| name.to_string(), |readable| readable + plt)
}

/// Adds `index` to a list of indices in order, unless it is the last.
pub(super) fn add_index(indices: &mut Vec<usize>, index: usize) {
    if indices.last() != Some(&index) {
        indices.push(index);
    }
}

/// Adds `node` to what a name finds, unless it is there already. Items are
/// added in order, with all their names, so that one already there is the
/// last.
fn add(nodes: &mut Vec<Node>, node: Node) {
    if nodes.last() != Some(&node) {
        nodes.push(node);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The sample `name` in testdata/bin.
    fn sample(name: &str) -> Binary {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/bin")
            .join(name);
        Binary::load(&path).unwrap()
    }

    /// The index of the item named `name`.
    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    #[test]
    fn a_row_name_is_worked_out_once() {
        let binary = sample("c-x86_64.elf");
        let names = Names::new(&binary);
        let stub = index(&binary, "strlen@plt");
        assert_eq!(names.row_name(stub), "strlen@plt");
        assert_eq!(names.row_names.borrow()[&stub], "strlen@plt");
        // Asked again, it gives the name it kept, without working it out.
        names
            .row_names
            .borrow_mut()
            .insert(stub, "kept".to_string());
        assert_eq!(names.row_name(stub), "kept");
    }
}
