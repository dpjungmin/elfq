//! [`Names`]: what each name in the file finds, worked out once, and how
//! to name an item for the user in return.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use super::not_found::{Inlined, USE_ONE};
use super::similar::{Lowercase, Suggestable};
use super::{LookupOrMatch, Symbol};
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

/// What a query finds, before an error words why it finds no one item or
/// import (see [`Names::look_up`]).
pub(in crate::commands) enum Lookup<'n> {
    /// The one item or import that it finds.
    One(Symbol),
    /// No item or import has the name.
    NoName,
    /// Several items or imports have it, in the order that the error lists
    /// them.
    Several(&'n [Node]),
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
    /// How many queries with the key [`Names::finds_only`] has answered
    /// from `words`, word by word: after a few, it works out `nodes` once
    /// and answers from those instead.
    word_by_word: Cell<u32>,
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

    /// Finds the item or import that `query` names, or when it finds nothing
    /// as a name, the one item that it matches as a pattern, such as `on_b*`
    /// (see [`Names::look_up_or_match`]), as the commands that take one
    /// item, such as `why`, read SYMBOL. When it could mean several, or
    /// none, the error lists what to type instead.
    pub(in crate::commands) fn find(&self, query: &str) -> Result<Symbol> {
        match self.look_up_or_match(query)? {
            LookupOrMatch::Name(lookup) => self.one_or_error(query, lookup),
            LookupOrMatch::Pattern(matched) => match matched.only_item() {
                Some(symbol) => Ok(symbol),
                None => bail!("{}", self.matches_several(&matched)),
            },
            LookupOrMatch::NoMatch(pattern) => bail!("{}", self.nothing_matches(&pattern)),
        }
    }

    /// The one item or import that `query` finds, from `lookup`, what it
    /// finds as a name (see [`Names::look_up`]), or the error that
    /// [`Names::find`] gives when a name finds several or none.
    pub(in crate::commands) fn one_or_error(&self, query: &str, lookup: Lookup) -> Result<Symbol> {
        match lookup {
            Lookup::One(symbol) => Ok(symbol),
            Lookup::NoName => bail!("{}", self.nothing_named(query)),
            Lookup::Several(nodes) => bail!("{}", self.choices(query, nodes, USE_ONE)),
        }
    }

    /// What `query` finds as a name or an address, without reading it as a
    /// pattern (see [`Names::look_up_or_match`]), and before an error words
    /// why it finds no one item or import, so that a command can word that
    /// its own way, as `retained` does. An address that no item holds, or
    /// that isn't one, is an error already. Unlike `find`, it doesn't work
    /// out why a name finds several or none, which can take seconds in a
    /// large file.
    pub(in crate::commands) fn look_up(&self, query: &str) -> Result<Lookup<'_>> {
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
            return Ok(Lookup::One(Symbol {
                node: Node::Item(index),
                note,
            }));
        }
        Ok(match self.nodes(query) {
            &[node] => Lookup::One(self.found(node, query)),
            [] => Lookup::NoName,
            nodes => Lookup::Several(nodes),
        })
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
    /// of theirs, as for `fmt`, which thousands of Rust functions end with,
    /// unless the key is asked about again and again: after a few queries
    /// with one key, it works out all of theirs once, as `nodes` does, and
    /// answers from those.
    pub(super) fn finds_only(&self, query: &str, node: Node) -> bool {
        const MANY: usize = 100;
        // Word by word is quicker when an answer asks about a key a few
        // times, but one that names many items, as `why --all` does in a
        // large Rust program, can ask about one key for each of them, and
        // each query checks the names with its words again. So after this
        // many queries with one key, its short names are worked out once.
        // It counts queries, not items named. Naming an item asks its short
        // names, shortest first, until one finds only it: `new`, then
        // `App::new`. For a method of a generic type, as anyhow's
        // `<ContextError<C, E> as core::fmt::Debug>::fmt`, more than eight
        // of those have the key `fmt`. So an answer that names many items
        // with a key works out its short names within the first few.
        const MOST_WORD_BY_WORD: u32 = 8;
        if self.full().contains_key(query) || self.many.get() {
            return self.nodes(query) == [node];
        }
        let key = names::short_name_key(query);
        let Some(filed) = key.and_then(|key| self.filed().get(key)) else {
            return false;
        };
        let word_by_word = filed.word_by_word.get();
        if filed.items.len() <= MANY
            || filed.nodes.get().is_some()
            || word_by_word == MOST_WORD_BY_WORD
        {
            return self.nodes(query) == [node];
        }
        filed.word_by_word.set(word_by_word + 1);
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
            let may_have = |name: &str| names::may_have_words(name, &query_words);
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

    /// Whether [`Names::will_name`] was told that many items are to be
    /// named, for the tests of the answers that tell it.
    #[cfg(test)]
    pub(in crate::commands) fn names_many(&self) -> bool {
        self.many.get()
    }

    /// The names of items and imports, and aliases of items, and what each
    /// one finds.
    fn full(&self) -> &HashMap<&'a str, Vec<Node>> {
        self.full.get_or_init(|| {
            // Sized for every name at once, since growing hashes the names
            // in it again.
            let names: usize = (self.binary.items.iter())
                .map(|item| item.names().count())
                .sum();
            let mut full: HashMap<&str, Vec<Node>> =
                HashMap::with_capacity(names + self.binary.imports.len());
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
                            word_by_word: Cell::new(0),
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
            .filter(|name| !names::is_mangled(name))
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
    use crate::commands::symbol::fixtures;

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

    #[test]
    fn find_takes_the_one_item_that_a_pattern_matches() {
        let binary =
            fixtures::functions(&["on_button", "on_timer", "on_data", "operator*", "main"]);
        let names = Names::new(&binary);
        let find = |query: &str| match names.find(query) {
            Ok(symbol) => Ok((symbol.node, symbol.lead_name(&binary))),
            Err(error) => Err(error.to_string()),
        };
        let found = |index: usize, lead: &str| Ok((Node::Item(index), lead.to_string()));
        // The first line says that the pattern found the item.
        assert_eq!(
            find("on_b*"),
            found(0, "on_button (the one item named on_b*)")
        );
        // Several are an error that lists them, in address order.
        assert_eq!(
            find("on_*").unwrap_err(),
            "`on_*` matches 3 functions; use one on the left instead:\n  on_button\n  on_timer\n  \
             on_data"
        );
        // None gets the error for a name, with a pattern like it that
        // matches.
        assert_eq!(
            find("On_*").unwrap_err(),
            "no function or data object is named `On_*`; did you mean 'on_*' (3 functions)?"
        );
        // A name comes first, even with a `*` in it, and a name or an
        // address finds what it found before.
        assert_eq!(find("operator*"), found(3, "operator*"));
        assert_eq!(find("on_data"), found(2, "on_data"));
        assert_eq!(find("0x1004"), found(0, "on_button (which holds 0x1004)"));
    }

    #[test]
    fn telling_names_that_many_items_are_to_be_named_changes_no_name() {
        for file in [
            "c-x86_64.elf",
            "c-x86_64.so",
            "cpp-x86_64.so",
            "rust-x86_64-nodebug.elf",
        ] {
            let binary = sample(file);
            // One works out the short names filed under each key that it is
            // asked about, and the other every short name at once.
            let key_by_key = Names::new(&binary);
            let at_once = Names::new(&binary);
            at_once.will_name(usize::MAX);
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            for node in items.chain(imports) {
                let query_for = key_by_key.query_for(node);
                assert_eq!(at_once.query_for(node), query_for, "{file}: {node:?}");
                let short_name = key_by_key.short_name(node);
                assert_eq!(at_once.short_name(node), short_name, "{file}: {node:?}");
                if let Node::Item(index) = node {
                    let row_name = key_by_key.row_name(index);
                    assert_eq!(at_once.row_name(index), row_name, "{file}: {node:?}");
                }
            }
            // Each worked them out its own way.
            assert!(key_by_key.all_short.get().is_none() && at_once.filed.get().is_none());
        }
    }

    /// An executable with 135 functions filed under the key `fmt`, more
    /// than the 100 above which [`Names::finds_only`] goes word by word:
    /// mostly the `Debug` and `Display` methods of Rust types, with a few
    /// that have generic arguments, two that share a name, and a PLT stub
    /// for one of them. It has one import.
    fn fmt_methods() -> Binary {
        let mut names: Vec<String> = (0..120)
            .map(|number| format!("<app::Frame{number} as core::fmt::Debug>::fmt"))
            .collect();
        for number in 0..4 {
            names.push(format!("<app::Frame{number} as core::fmt::Display>::fmt"));
            // Short names may leave out generic arguments.
            names.push(format!(
                "<Vec<app::Frame{number}> as core::fmt::Debug>::fmt"
            ));
            // And leaving out these joins two words.
            names.push(format!("app::Frame<u{number}>Ref::fmt"));
        }
        // Two functions with one name.
        names.extend(["app::log::fmt".to_string(), "app::log::fmt".to_string()]);
        // A stub that jumps to `<app::Frame10 as core::fmt::Debug>::fmt`.
        names.push("<app::Frame10 as core::fmt::Debug>::fmt@plt".to_string());
        let mut binary = fixtures::functions(&names);
        binary.imports.push("write".to_string());
        binary
    }

    #[test]
    fn finds_only_answers_alike_word_by_word_and_from_a_keys_short_names() {
        let binary = fmt_methods();
        // Names finds the function that a stub jumps to from the stub's
        // bytes, which `fmt_methods` doesn't make, so each Names here is
        // told which item is the stub.
        let stub = index(&binary, "<app::Frame10 as core::fmt::Debug>::fmt@plt");
        let new_names = || {
            let names = Names::new(&binary);
            names.own_function_stubs.set(HashSet::from([stub])).unwrap();
            names
        };
        // Each short name with the key `fmt` that isn't a whole name, asked
        // of the item that has it, of another and of the import.
        let names = new_names();
        let mut questions: Vec<(String, Node)> = Vec::new();
        for (index, item) in binary.items.iter().enumerate() {
            for query in names::short_names(&item.name) {
                if names::short_name_key(&query) == Some("fmt")
                    && !names.full().contains_key(&*query)
                {
                    let next = Node::Item((index + 1) % binary.items.len());
                    for node in [Node::Item(index), next, Node::Import(0)] {
                        questions.push((query.clone(), node));
                    }
                }
            }
        }
        // Asked of a Names that has been asked nothing, each goes word by
        // word.
        let word_by_word: Vec<bool> = (questions.iter())
            .map(|(query, node)| {
                let names = new_names();
                let finds_only = names.finds_only(query, *node);
                let filed = &names.filed()["fmt"];
                assert!(filed.words.get().is_some() && filed.nodes.get().is_none());
                finds_only
            })
            .collect();
        // Asked of one Names, all but the first few are answered from every
        // short name with the key, worked out once.
        let from_short_names: Vec<bool> = (questions.iter())
            .map(|(query, node)| names.finds_only(query, *node))
            .collect();
        assert!(names.filed()["fmt"].nodes.get().is_some());
        assert_eq!(from_short_names, word_by_word);
        let nodes: Vec<bool> = (questions.iter())
            .map(|(query, node)| names.nodes(query) == [*node])
            .collect();
        assert_eq!(word_by_word, nodes);
        // Some get yes, and the others no.
        assert!(word_by_word.contains(&true) && word_by_word.contains(&false));
    }
}
