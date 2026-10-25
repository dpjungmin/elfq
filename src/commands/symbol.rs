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

use std::cell::{Cell, OnceCell};
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};

use super::text;
use crate::binary::{
    AddressRange, Binary, ItemKind, can_be_shorter, demangle, ends_with_short_name,
    for_each_short_name, has_short_name, hex_number, may_join_words, range_hint, short_name_key,
    short_name_keys, short_names, short_names_with_key, words,
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
                "`{query}` isn't a range; give each range as its own argument: {}",
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
        bail!("`{query}` isn't a range: {}", range_hint(query));
    }
    Ok(())
}

/// Whether `query` is meant as a range of memory rather than a symbol: it
/// starts with a digit, as no name does, and has `..` or `+` in it.
pub fn is_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && (query.contains("..") || query.contains('+'))
}

/// Finds the item or import that `query` names, as `Names::find` does.
/// The names are freed before the answer, so that what the command builds
/// next can take their memory.
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
    /// For each key (see [`short_name_key`]), the items that one of their
    /// names files under it, and the short names with the key, worked out
    /// the first time a query that isn't a full name has it.
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
    inlined: OnceCell<Option<Inlined>>,
    /// Every name of every item and import in lowercase, made the first
    /// time a question needs it.
    lowercase: OnceCell<Lowercase>,
    /// The short names that [`Names::similar`] compares a query with, made
    /// the first time a query finds nothing.
    suggestable: OnceCell<Suggestable>,
}

/// The copies of functions that the compiler inlined, by function.
struct Inlined {
    copies: Vec<InlinedCopy>,
    /// Each function's name, with its copies, as indices into `copies`.
    functions: Vec<(String, Vec<usize>)>,
    /// The function with each name, as an index into `functions`.
    by_name: HashMap<String, usize>,
    /// The functions that each key files (see [`short_name_key`]).
    filed: HashMap<String, Vec<usize>>,
}

/// Every name of every item and import in lowercase, one after another,
/// for what looks for names that have some text in any case: the go-to
/// box, and [`Names::similar`].
struct Lowercase {
    text: String,
    /// Where each name ends in `text`.
    ends: Vec<usize>,
    /// Where the names of each item, by its index, and then of each import
    /// start in `ends`, and after the last, where they end.
    starts: Vec<usize>,
    /// Whether every name is ASCII. Then each character is a byte, and a
    /// part of a name in lowercase is that part of it in lowercase.
    ascii: bool,
}

/// The short names, in lowercase, of every item and import that
/// [`Names::similar`] can suggest, so that each query needn't work them out
/// again. Their names are in [`Lowercase`]. Most short names are parts of
/// those names, and are kept as where they are in them.
struct Suggestable {
    /// Each item or import, with where its short names start in `ends` and
    /// in `parts`.
    nodes: Vec<(Node, usize, usize)>,
    /// The short names that aren't kept as parts, one after another.
    text: String,
    /// Where each of those ends in `text`.
    ends: Vec<usize>,
    /// The short names that are parts of names: the name, by where it is in
    /// [`Lowercase`]'s `ends`, where the short name starts in it, and its
    /// length.
    parts: Vec<(u32, u32, u32)>,
}

/// Text to look for in names. `str::contains` is quick for text of up to
/// 32 bytes, so longer text is looked for by its first 32 bytes first.
pub(super) struct Needle<'t> {
    text: &'t str,
    start: &'t str,
}

/// The items filed under a key, and their short names with it.
struct Filed<'a> {
    /// The items with a name that may have a short name with the key: a
    /// few more than have one.
    items: Vec<usize>,
    /// The short names with the key, and what each finds.
    nodes: OnceCell<HashMap<String, Vec<Node>>>,
    /// The words of the items' names (see [`words`]), and the items that
    /// have each, as indices into `items`, for [`Names::finds_only`].
    words: OnceCell<Words<'a>>,
}

/// The words of some items' names, and the items that have each.
struct Words<'a> {
    with: HashMap<&'a str, Vec<usize>>,
    /// The items with a name that may have a short name with any words
    /// (see [`may_join_words`]).
    any: Vec<usize>,
}

impl<'a> Names<'a> {
    pub(super) fn new(binary: &'a Binary) -> Names<'a> {
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
            &[node] => Ok(self.found(node, query)),
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

    /// What `query` finds when it finds one item or import, as
    /// [`Names::find`] finds it, or `None`. Unlike `find`, it doesn't work
    /// out why `query` finds several or none, which can take seconds in a
    /// large file.
    #[cfg(feature = "tui")]
    pub(super) fn find_one(&self, query: &str) -> Option<Symbol> {
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

    /// Where the compiler inlined every copy of the function that `query`
    /// names, as the DWARF says: the reason that no item has the name.
    /// `None` without DWARF, when `query` ends the names of several inlined
    /// functions, or when the compiler kept part of the function out of
    /// line, as `checksum.part.0` for `checksum`.
    fn inlined_everywhere(&self, query: &str) -> Option<String> {
        if !self.binary.has_dwarf {
            return None;
        }
        let inlined = self.inlined()?;
        let all_copies = &inlined.copies;
        let (name, copies) = &inlined.functions[inlined.only_function(query)?];
        let copies: Vec<&InlinedCopy> = copies.iter().map(|&copy| &all_copies[copy]).collect();
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
            Node::Item(index) if !super::inlined::shows(all_copies, index, name) => " --all",
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

    /// The copies of functions that the compiler inlined, as the DWARF
    /// lists them, read once.
    fn inlined(&self) -> Option<&Inlined> {
        let inlined = self.inlined.get_or_init(|| {
            let copies = debuginfo::inlined_copies(self.binary).ok()?;
            // A function is often inlined in many places: each is filed
            // once.
            let mut functions: Vec<(String, Vec<usize>)> = Vec::new();
            let mut by_name: HashMap<String, usize> = HashMap::new();
            for (index, copy) in copies.iter().enumerate() {
                let function = *by_name.entry(copy.name.clone()).or_insert_with(|| {
                    functions.push((copy.name.clone(), Vec::new()));
                    functions.len() - 1
                });
                functions[function].1.push(index);
            }
            let mut filed: HashMap<String, Vec<usize>> = HashMap::new();
            for (function, (name, _)) in functions.iter().enumerate() {
                for key in short_name_keys(name) {
                    filed.entry(key).or_default().push(function);
                }
            }
            Some(Inlined {
                copies,
                functions,
                by_name,
                filed,
            })
        });
        inlined.as_ref()
    }

    /// How elfq names items in a file without a symbol table, with
    /// examples, or why it found no functions in it: the likely reason that
    /// no item has a name. `None` in a file with a symbol table.
    fn unnamed(&self) -> Option<String> {
        if self.binary.has_symbols {
            return None;
        }
        if !super::found_functions(self.binary) {
            return Some(text::NO_FUNCTIONS.to_string());
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
        if let Some(nodes) = self.full().get(query) {
            return nodes;
        }
        if self.many.get() {
            return self.all_short().get(query).map_or(&[], Vec::as_slice);
        }
        // Working out every short name of every item takes a while in a
        // large file: only those of the items filed under `query`'s key
        // are worked out, once.
        let key = short_name_key(query);
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
    fn finds_only(&self, query: &str, node: Node) -> bool {
        const MANY: usize = 100;
        if self.full().contains_key(query) || self.many.get() {
            return self.nodes(query) == [node];
        }
        let key = short_name_key(query);
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
                let names = self.binary.items[index]
                    .names()
                    .filter(|name| can_be_shorter(name));
                for name in names {
                    if may_join_words(name) {
                        add_index(&mut by_word.any, at);
                    }
                    for word in words(name) {
                        add_index(by_word.with.entry(word).or_default(), at);
                    }
                }
            }
            by_word
        });
        // The items whose names have the word of `query` that the fewest
        // have, or every item when it has no words.
        let query_words: Vec<&str> = words(query).collect();
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
            let names = || self.binary.items[index].names();
            let may_have = |name: &str| {
                may_join_words(name) || query_words.iter().all(|word| name.contains(word))
            };
            if self.own_function_stubs().contains(&index) && !query.ends_with("@plt")
                || !names().any(may_have)
            {
                return false;
            }
            names().any(|name| ends_with_short_name(name, query) || has_short_name(name, query))
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
    pub(super) fn will_name(&self, count: usize) {
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
    fn own_function_stubs(&self) -> &HashSet<usize> {
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
                for key in item.names().flat_map(short_name_keys) {
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

    /// The short names of item `index` (see [`short_names`]), or with a
    /// key, only those that have it. A stub for one of the file's own
    /// functions keeps only those that end in `@plt`: `app::log(unsigned
    /// int)@plt` is also `log(unsigned int)@plt`, but not `log`, which means
    /// the function.
    fn short_names_of(&self, index: usize, key: Option<&str>) -> Vec<String> {
        let own_function_stub = self.own_function_stubs().contains(&index);
        self.binary.items[index]
            .names()
            .flat_map(|name| match key {
                Some(key) => short_names_with_key(name, key),
                None => short_names(name),
            })
            .filter(|short_name| !own_function_stub || short_name.ends_with("@plt"))
            .collect()
    }

    /// The shortest thing to type that finds only `node`: a short name, a
    /// name other than a mangled symbol, or failing those, an item's
    /// address.
    pub(super) fn query_for(&self, node: Node) -> String {
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
    pub(super) fn short_name(&self, node: Node) -> String {
        let name = node.name(self.binary);
        self.shortest_name(node, vec![name], |_| true)
            .unwrap_or_else(|| name.to_string())
    }

    /// What a row calls the item at `index` after "used by", so that it
    /// works as SYMBOL: the shortest form of its name that finds only the
    /// item, as [`Names::short_name`] picks it but keeping a PLT stub's
    /// `@plt`, or else its address, as when two functions share a name.
    pub(super) fn row_name(&self, index: usize) -> String {
        let node = Node::Item(index);
        let name = node.name(self.binary);
        // Without `@plt`, a stub's name means what the stub passes calls
        // to, which reads as another item: `strlen, used by strlen`.
        let stub = name.ends_with("@plt");
        self.shortest_name(node, vec![name], |query| !stub || query.ends_with("@plt"))
            .unwrap_or_else(|| format!("{:#x}", self.binary.items[index].address))
    }

    /// The shortest of `names` and their short names that finds only
    /// `node` and that `keep` keeps, leaving out mangled symbols, if one
    /// does.
    fn shortest_name(
        &self,
        node: Node,
        names: Vec<&str>,
        keep: impl Fn(&str) -> bool,
    ) -> Option<String> {
        // A stub for one of the file's own functions has the function's
        // mangled names as aliases, with `@plt` added.
        let mut queries: Vec<String> = names
            .into_iter()
            .filter(|name| demangle(name.strip_suffix("@plt").unwrap_or(name)).is_none())
            .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
            .filter(|query| keep(query))
            .collect();
        queries.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        queries
            .into_iter()
            .find(|query| self.finds_only(query, node))
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
        let mut message = format!(
            "`{query}` could mean {}; use one on the left instead:",
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
        let needle = Needle::new(&query);
        let query_chars: Vec<char> = query.chars().collect();
        let query_length = query_chars.len();
        let typos = (query_length / 3).min(2);
        let lowercase = self.lowercase();
        let length = |name: &str| match lowercase.ascii {
            true => name.len(),
            false => name.chars().count(),
        };
        // The typos between `name`, in lowercase, and `query`, if there are
        // `typos` or fewer. A name whose length differs from the query's by
        // more has more.
        let typos_from = |name: &str| {
            if length(name).abs_diff(query_length) > typos {
                return None;
            }
            if lowercase.ascii && query.is_ascii() {
                typos_within(name.as_bytes(), query.as_bytes(), typos)
            } else {
                let name: Vec<char> = name.chars().collect();
                typos_within(&name, &query_chars, typos)
            }
        };
        // How far `name`, in lowercase, is from `query`, if it is close:
        // 0 when it contains `query`, or else the typos between them.
        let distance = |name: &str| match needle.is_in(name) {
            true => Some(0),
            false => typos_from(name),
        };
        // How close the closest of `node`'s names and their short names
        // comes, and those of them to suggest: the fewest typos from
        // `query`, and of those, the ones that contain it. So `SV_Handler`
        // gets `SVC_Handler`, not `PendSV_Handler`, and `iprintf` gets
        // `fiprintf`, not `fprintf`.
        let closest = |node: Node| {
            let close: Vec<(usize, String)> = self
                .names_of(node)
                .into_iter()
                .flat_map(|name| short_names(name).into_iter().chain([name.to_string()]))
                .filter_map(|name| Some((distance(&name.to_lowercase())?, name)))
                .collect();
            let nearest = close.iter().map(|(distance, _)| *distance).min()?;
            let rank = |(distance, name): &(usize, String)| {
                // More typos than `typos` count the same.
                let typos_from = typos_from(&name.to_lowercase()).unwrap_or(typos + 1);
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

        // Closest first, then shortest. How close each item or import
        // comes is worked out from its names in lowercase, made once; which
        // of its names came closest only for those that the loop below
        // gets to.
        let suggestable = self.suggestable();
        let mut similar: Vec<(usize, usize, &str, Node)> = (0..suggestable.nodes.len())
            .filter_map(|at| {
                let (node, short_names, parts) = suggestable.short_names(at);
                let names = lowercase.names(self.position(node));
                let mut nearest = None;
                for name in names.chain(short_names) {
                    match distance(name) {
                        Some(0) => {
                            nearest = Some(0);
                            break;
                        }
                        Some(distance) => nearest = nearest.min(Some(distance)).or(Some(distance)),
                        None => {}
                    }
                }
                // A part of a name has `query` only if the name has it, so
                // it can only come close by typos.
                if nearest != Some(0) {
                    for &(name, start, length) in parts {
                        if (length as usize).abs_diff(query_length) > typos {
                            continue;
                        }
                        let start = start as usize;
                        let name = &lowercase.name(name as usize)[start..start + length as usize];
                        if let Some(distance) = typos_from(name) {
                            nearest = nearest.min(Some(distance)).or(Some(distance));
                        }
                    }
                }
                let name = node.name(self.binary);
                Some((nearest?, name.len(), name, node))
            })
            .collect();
        similar.sort();

        let mut suggestions: Vec<String> = Vec::new();
        for (distance, _, _, node) in similar {
            let Some((_, closest)) = closest(node) else {
                continue;
            };
            // The shortest of the names that came closest, which needn't be
            // the item's shortest name: `sniprintf` for `sniprin`, though
            // its alias `snprintf` is shorter. Failing that, when none of them
            // finds only this item, or they are all mangled symbols, the
            // shortest thing to type that does, but not one that has `query`
            // only in a part that it leaves out, such as a generic argument.
            let suggestion = closest
                .into_iter()
                .filter(|name| demangle(name).is_none() && self.finds_only(name, node))
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

    /// Whether one of the names of `node`, an item or an import, in
    /// lowercase, has `text`, which is lowercase too.
    #[cfg(feature = "tui")]
    pub(super) fn name_has(&self, node: Node, text: &Needle) -> bool {
        self.lowercase()
            .names(self.position(node))
            .any(|name| text.is_in(name))
    }

    /// Every name of every item and import in lowercase.
    fn lowercase(&self) -> &Lowercase {
        self.lowercase.get_or_init(|| {
            let items = &self.binary.items;
            let imports = &self.binary.imports;
            // The table's size first, so that it is made once, no larger.
            let (mut count, mut length) = (0, 0);
            let names = items.iter().flat_map(|item| item.names());
            for name in names.chain(imports.iter().map(String::as_str)) {
                count += 1;
                length += name.len();
            }
            let mut lowercase = Lowercase {
                text: String::with_capacity(length),
                ends: Vec::with_capacity(count),
                starts: Vec::with_capacity(items.len() + imports.len() + 1),
                ascii: true,
            };
            for item in items {
                lowercase.starts.push(lowercase.ends.len());
                for name in item.names() {
                    lowercase.push(name);
                }
            }
            for import in imports {
                lowercase.starts.push(lowercase.ends.len());
                lowercase.push(import);
            }
            lowercase.starts.push(lowercase.ends.len());
            lowercase
        })
    }

    /// The short names of the items and imports that [`Names::similar`]
    /// can suggest, in lowercase. A stub for one of the file's own
    /// functions is left out: its function is the one to suggest.
    fn suggestable(&self) -> &Suggestable {
        self.suggestable.get_or_init(|| {
            let lowercase = self.lowercase();
            let items = (0..self.binary.items.len())
                .filter(|index| !self.own_function_stubs().contains(index))
                .map(Node::Item);
            let imports = (0..self.binary.imports.len()).map(Node::Import);
            let mut suggestable = Suggestable {
                nodes: Vec::new(),
                text: String::new(),
                ends: Vec::new(),
                parts: Vec::new(),
            };
            for node in items.chain(imports) {
                let (first_end, first_part) = (suggestable.ends.len(), suggestable.parts.len());
                suggestable.nodes.push((node, first_end, first_part));
                let names = lowercase.starts[self.position(node)]..;
                for (at, name) in names.zip(self.names_of(node)) {
                    // Each short name once, where it is a part of `name`
                    // when it can be.
                    for_each_short_name(name, |short_name, start| {
                        if short_name == name {
                            return;
                        }
                        let part = start.filter(|_| lowercase.ascii).and_then(|start| {
                            let number = |at: usize| u32::try_from(at).ok();
                            Some((number(at)?, number(start)?, number(short_name.len())?))
                        });
                        match part {
                            Some(part) => {
                                if !suggestable.parts[first_part..].contains(&part) {
                                    suggestable.parts.push(part);
                                }
                            }
                            None => {
                                let start = suggestable.text.len();
                                match short_name.is_ascii() {
                                    true => suggestable
                                        .text
                                        .extend(short_name.chars().map(|c| c.to_ascii_lowercase())),
                                    false => suggestable.text += &short_name.to_lowercase(),
                                }
                                let text = &suggestable.text;
                                let mut from = match first_end {
                                    0 => 0,
                                    _ => suggestable.ends[first_end - 1],
                                };
                                let seen = suggestable.ends[first_end..].iter().any(|&end| {
                                    let earlier = &text[from..end];
                                    from = end;
                                    earlier == &text[start..]
                                });
                                match seen {
                                    true => suggestable.text.truncate(start),
                                    false => suggestable.ends.push(suggestable.text.len()),
                                }
                            }
                        }
                    });
                }
            }
            suggestable
        })
    }

    /// Where `node`'s names are in [`Lowercase`]'s `starts`.
    fn position(&self, node: Node) -> usize {
        match node {
            Node::Item(index) => index,
            Node::Import(index) => self.binary.items.len() + index,
        }
    }

    /// The names of `node`: an item's name and aliases, or an import's
    /// name.
    fn names_of(&self, node: Node) -> Vec<&'a str> {
        match node {
            Node::Item(index) => self.binary.items[index].names().collect(),
            Node::Import(index) => vec![&self.binary.imports[index]],
        }
    }
}

impl Inlined {
    /// The function that `query` names, as its name or a short name, when
    /// it names only one.
    fn only_function(&self, query: &str) -> Option<usize> {
        let mut found: Vec<usize> = self.by_name.get(query).copied().into_iter().collect();
        if let Some(key) = short_name_key(query)
            && let Some(filed) = self.filed.get(key)
        {
            for &function in filed {
                if found.contains(&function) {
                    continue;
                }
                let (name, _) = &self.functions[function];
                if short_names_with_key(name, key)
                    .iter()
                    .any(|short_name| short_name == query)
                {
                    found.push(function);
                }
            }
        }
        match found[..] {
            [function] => Some(function),
            _ => None,
        }
    }
}

impl Lowercase {
    /// Adds `name` in lowercase.
    fn push(&mut self, name: &str) {
        if name.is_ascii() {
            let start = self.text.len();
            self.text += name;
            self.text[start..].make_ascii_lowercase();
        } else {
            self.text += &name.to_lowercase();
            self.ascii = false;
        }
        self.ends.push(self.text.len());
    }

    /// The name at `at` in `ends`.
    fn name(&self, at: usize) -> &str {
        let start = at.checked_sub(1).map_or(0, |before| self.ends[before]);
        &self.text[start..self.ends[at]]
    }

    /// The names of the item or import at `at` in `starts`.
    fn names(&self, at: usize) -> impl Iterator<Item = &str> {
        (self.starts[at]..self.starts[at + 1]).map(|name| self.name(name))
    }
}

impl Suggestable {
    /// The item or import at `at` in `nodes`, with its short names that
    /// aren't kept as parts, and those that are.
    fn short_names(&self, at: usize) -> (Node, impl Iterator<Item = &str>, &[(u32, u32, u32)]) {
        let (node, first, first_part) = self.nodes[at];
        let (last, last_part) = self.nodes.get(at + 1).map_or(
            (self.ends.len(), self.parts.len()),
            |&(_, first, first_part)| (first, first_part),
        );
        let short_names = (first..last).map(move |name| {
            let start = name.checked_sub(1).map_or(0, |before| self.ends[before]);
            &self.text[start..self.ends[name]]
        });
        (node, short_names, &self.parts[first_part..last_part])
    }
}

impl<'t> Needle<'t> {
    pub(super) fn new(text: &'t str) -> Needle<'t> {
        let mut end = text.len().min(32);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Needle {
            text,
            start: &text[..end],
        }
    }

    /// Whether `name` has the text.
    pub(super) fn is_in(&self, name: &str) -> bool {
        name.contains(self.start)
            && (self.start.len() == self.text.len() || name.contains(self.text))
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

/// Adds `index` to a list of indices in order, unless it is the last.
fn add_index(indices: &mut Vec<usize>, index: usize) {
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

/// The number of typos that turn `a` into `b`, if there are `most` or
/// fewer, where a typo is a character inserted, deleted or replaced, or two
/// neighbors swapped (the optimal string alignment distance).
fn typos_within<T: PartialEq>(a: &[T], b: &[T], most: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > most {
        return None;
    }
    // `row[j]` is the distance between the first i characters of `a` and
    // the first j of `b`, or `over` when it is more than `most`. Only the
    // rows for the two i before are needed, and in each only the j within
    // `most` of i: the others are more than `most`.
    let over = most + 1;
    let mut two_before = vec![over; b.len() + 1];
    let mut before: Vec<usize> = (0..=b.len()).map(|j| j.min(over)).collect();
    let mut row = vec![over; b.len() + 1];
    for i in 1..=a.len() {
        let first = i.saturating_sub(most).max(1);
        let last = (i + most).min(b.len());
        row[0] = i.min(over);
        // The j just outside the band, which the next rows read.
        if first > 1 {
            row[first - 1] = over;
        }
        if last < b.len() {
            row[last + 1] = over;
        }
        let mut least = row[0];
        for j in first..=last {
            let replace = before[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = replace.min(before[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(two_before[j - 2] + 1);
            }
            row[j] = best.min(over);
            least = least.min(row[j]);
        }
        // No row after has a smaller distance than this one's least.
        if least > most {
            return None;
        }
        // The row just made comes before the next, which reuses the oldest.
        std::mem::swap(&mut two_before, &mut before);
        std::mem::swap(&mut before, &mut row);
    }
    Some(before[b.len()]).filter(|&typos| typos <= most)
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
    fn typos_are_counted_up_to_a_most() {
        let typos = |a: &str, b: &str, most| {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            typos_within(&a, &b, most)
        };
        assert_eq!(typos("process_frame", "process_frame", 2), Some(0));
        assert_eq!(typos("process_frame", "proces_frame", 2), Some(1));
        assert_eq!(typos("log_msg", "log_mgs", 1), Some(1));
        assert_eq!(typos("write", "wirte", 2), Some(1));
        assert_eq!(typos("log_msg", "lgo_mgs", 2), Some(2));
        assert_eq!(typos("log_msg", "lgo_mgs", 1), None);
        assert_eq!(typos("", "abc", 3), Some(3));
        assert_eq!(typos("abc", "", 2), None);
        // A swap counts once, but a character can't move further after it.
        assert_eq!(typos("ab", "ba", 1), Some(1));
        assert_eq!(typos("ca", "abc", 3), Some(3));
        assert_eq!(typos("ca", "abc", 2), None);
        assert_eq!(typos("größe", "grösse", 2), Some(2));
        // Typos far apart, and at both ends.
        assert_eq!(typos("xbcdefghiy", "abcdefghij", 2), Some(2));
        assert_eq!(typos("abcdefghij", "bcdefghijk", 2), Some(2));
        assert_eq!(typos("abcdefghij", "cdefghijkl", 3), None);
        assert_eq!(typos("abcdefghij", "cdefghijkl", 4), Some(4));
    }

    #[test]
    fn typos_within_a_most_are_those_of_every_alignment() {
        // Each pair of short strings over a small alphabet, against the
        // distance worked out without a band.
        let full = |a: &[u8], b: &[u8]| {
            let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
            for i in 0..=a.len() {
                for j in 0..=b.len() {
                    table[i][j] = if i == 0 || j == 0 {
                        i + j
                    } else {
                        let mut best = (table[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]))
                            .min(table[i - 1][j] + 1)
                            .min(table[i][j - 1] + 1);
                        if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                            best = best.min(table[i - 2][j - 2] + 1);
                        }
                        best
                    };
                }
            }
            table[a.len()][b.len()]
        };
        let strings: Vec<Vec<u8>> = (0..1093u32)
            .map(|mut number| {
                let mut string = Vec::new();
                while number > 0 {
                    string.push(b"abc"[(number % 3) as usize]);
                    number /= 3;
                }
                string
            })
            .collect();
        for a in &strings {
            for b in strings.iter().step_by(7) {
                let distance = full(a, b);
                for most in 0..4 {
                    let expected = Some(distance).filter(|&distance| distance <= most);
                    assert_eq!(typos_within(a, b, most), expected, "{a:?} {b:?} {most}");
                }
            }
        }
    }
}
