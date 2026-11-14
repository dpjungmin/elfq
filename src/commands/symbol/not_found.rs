//! The error for SYMBOL that finds several items or none: what to type for
//! each of several, and in a command that takes a set, such as `retained`,
//! the pattern that takes them all, when it takes just those (see
//! [`Names::choices`]); or why none has the name, as when the file has no
//! symbol table or the compiler inlined every copy of the function. For a
//! name that finds nothing, such as a module, type or class typed without
//! `*`, the error offers first the pattern for what is in it, as
//! `'core::fmt::*'` for `core::fmt`, where the command can take what that
//! matches ([`Takes`]). For a pattern that matches no item, the error
//! offers a pattern like it that matches some, as when only case keeps it
//! from matching, and for one that matches several in a command that takes
//! one item, such as `why`, it lists them as it lists what a name could
//! mean.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::lookup::add_index;
use super::{LookupOrMatch, Names, Pattern, PatternMatches};
use crate::binary::names;
use crate::commands::checks::found_functions;
use crate::commands::text::{self, Detail};
use crate::debuginfo::{self, CopiesToName, InlinedCopy};
use crate::graph::Node;

/// Where the compiler inlined every copy of a function, as the DWARF says.
pub(in crate::commands) struct InlinedEverywhere<'n> {
    /// The function's name, as the DWARF gives it.
    name: &'n str,
    /// How many copies of it there are.
    copies: usize,
    /// The functions that hold them, as indices into the items, those that
    /// hold the most first, then by name.
    pub(in crate::commands) into: Vec<usize>,
}

/// What a command takes for SYMBOL, which decides whether the error for a
/// name that finds nothing offers the pattern for what is in a module of
/// that name (see [`Names::module_pattern`]), and how the error for a name
/// that could mean several items ends: whether it says to use one
/// ([`USE_ONE`]) or one or more ([`USE_ONE_OR_MORE`]), and whether it
/// offers the pattern that takes them all (see [`Names::one_or_error`]).
#[derive(Clone, Copy)]
pub(in crate::commands) enum Takes {
    /// One item, as `why`, `reach`, `inlined` and `browse` take: the
    /// pattern for a module is offered only when it matches one item, the
    /// item it would find, and a name that could mean several says to use
    /// one, with no pattern for all (see [`Names::could_mean`]).
    One,
    /// A set, as `retained`, `callers` and `callees` take, of every item
    /// that a pattern matches: the pattern for a module is offered when it
    /// matches any, and a name that could mean several items says to use
    /// one or more, with the pattern that takes just those, when there is
    /// one (see [`Names::choices`]).
    Set,
}

/// The copies of functions that the compiler inlined, by function.
pub(super) struct Inlined {
    copies: Vec<InlinedCopy>,
    /// Each function once, in the order of its first copy.
    functions: Vec<InlinedFunction>,
    /// The function with each name, as an index into `functions`.
    by_name: HashMap<Arc<str>, usize>,
    /// The functions that each key files (see [`names::short_name_key`]).
    filed: HashMap<String, FiledFunctions>,
    /// The names of the functions that the compiler kept part of out of
    /// line, by the names of the items it kept: GCC names them after the
    /// function, as `checksum.part.0`, or in C++, `checksum(char const*)
    /// [clone .part.0]`. Each name before a dot is among them.
    kept_out_of_line: HashSet<String>,
}

/// A function that the compiler inlined, with its copies.
struct InlinedFunction {
    /// Its name, which its copies share.
    name: Arc<str>,
    /// Its copies, as indices into [`Inlined`]'s `copies`.
    copies: Vec<usize>,
}

/// The inlined functions that one key files, and once worked out, those
/// with each short name with that key: diff looks up thousands of names.
#[derive(Default)]
struct FiledFunctions {
    functions: Vec<usize>,
    by_short_name: OnceCell<HashMap<String, Vec<usize>>>,
}

impl<'a> Names<'a> {
    /// Where the compiler inlined every copy of the function that `query`
    /// names, as the DWARF says: the reason that no item has the name.
    /// `None` without DWARF, when `query` ends the names of several inlined
    /// functions, or when the compiler kept part of the function out of
    /// line, as `checksum.part.0` for `checksum`.
    pub(in crate::commands) fn inlined_everywhere(
        &self,
        query: &str,
    ) -> Option<InlinedEverywhere<'_>> {
        if !self.binary.has_dwarf {
            return None;
        }
        let inlined = self.inlined()?;
        let InlinedFunction { name, copies } = &inlined.functions[inlined.only_function(query)?];
        if inlined.kept_out_of_line.contains(name.as_ref()) {
            return None;
        }
        // The functions that hold the copies, those with the most first.
        let mut into: BTreeMap<usize, usize> = BTreeMap::new();
        for &copy in copies {
            *into.entry(inlined.copies[copy].into).or_default() += 1;
        }
        let mut into: Vec<(usize, usize)> = into.into_iter().collect();
        into.sort_by_key(|&(index, count)| (Reverse(count), &self.binary.items[index].name));
        Some(InlinedEverywhere {
            name,
            copies: copies.len(),
            into: into.into_iter().map(|(index, _)| index).collect(),
        })
    }

    /// The reason that `query` finds nothing when the compiler inlined
    /// every copy of the function it names (see
    /// [`Names::inlined_everywhere`]): where, and the `inlined` command
    /// that lists the copies.
    pub(super) fn inlined_reason(&self, query: &str) -> Option<String> {
        let InlinedEverywhere { name, copies, into } = self.inlined_everywhere(query)?;
        let names: Vec<String> = into
            .iter()
            .map(|&index| self.short_name(Node::Item(index)))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        // `inlined` lists the largest of what a function holds, which can
        // leave this one out without `--all`.
        let first = into[0];
        let all = match crate::commands::inlined::shows(&self.inlined()?.copies, first, name) {
            true => "",
            false => " --all",
        };
        let command = format!(
            "`inlined {}{all}`",
            text::shell_quote(&self.query_for(Node::Item(first)))
        );
        let copies = match copies {
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
                text::names_text(&names, Detail::Brief),
                names[0]
            ),
        })
    }

    /// The copies of functions that the compiler inlined, as the DWARF
    /// lists them, read once.
    fn inlined(&self) -> Option<&Inlined> {
        let inlined = self.inlined.get_or_init(|| {
            let copies = debuginfo::inlined_copies(self.binary, CopiesToName::All).ok()?;
            // A function is often inlined in many places: each is filed
            // once.
            let mut functions: Vec<InlinedFunction> = Vec::new();
            let mut by_name: HashMap<Arc<str>, usize> = HashMap::new();
            for (index, copy) in copies.iter().enumerate() {
                let function = *by_name.entry(copy.name.clone()).or_insert_with(|| {
                    functions.push(InlinedFunction {
                        name: copy.name.clone(),
                        copies: Vec::new(),
                    });
                    functions.len() - 1
                });
                functions[function].copies.push(index);
            }
            let mut filed: HashMap<String, FiledFunctions> = HashMap::new();
            for (function, InlinedFunction { name, .. }) in functions.iter().enumerate() {
                for key in names::short_name_keys(name) {
                    filed.entry(key).or_default().functions.push(function);
                }
            }
            let mut kept_out_of_line = HashSet::new();
            for item in &self.binary.items {
                let name = &item.name;
                for (at, _) in name
                    .match_indices('.')
                    .chain(name.match_indices(" [clone ."))
                {
                    kept_out_of_line.insert(name[..at].to_string());
                }
            }
            Some(Inlined {
                copies,
                functions,
                by_name,
                filed,
                kept_out_of_line,
            })
        });
        inlined.as_ref()
    }

    /// How elfq names items in a file without a symbol table, with
    /// examples, or why it found no functions in it: the likely reason that
    /// no item has a name. `None` in a file with a symbol table.
    pub(super) fn unnamed(&self) -> Option<String> {
        if self.binary.has_symbols {
            return None;
        }
        if !found_functions(self.binary) {
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

    /// The error for `query` when it finds nothing, in a command that
    /// `takes` one item or a set: no item or import has the name, then why,
    /// when elfq can tell (see [`Names::unnamed`] and
    /// [`Names::inlined_reason`]), and what it may mean: first the pattern
    /// for what is in a module, type or class of that name, when the
    /// command can take what it matches (see [`Names::module_pattern`]),
    /// then names like it.
    pub(in crate::commands) fn nothing_named(&self, query: &str, takes: Takes) -> String {
        let may_mean: Vec<String> = (self.module_pattern(query, takes).into_iter())
            .chain(self.similar(query))
            .collect();
        self.not_found(query, &may_mean)
    }

    /// The pattern for what is in the module, type or class that `query`
    /// would name, as the error for a name that finds nothing offers it,
    /// with how many items it matches: "'core::fmt::*' (73 functions)" for
    /// `core::fmt`, or for `app::Gain::`, whose `::` at the end is left
    /// out, "'app::Gain::*' (4 items)". A command that `takes` a set gets
    /// it when it matches any item, and one that takes one item only when
    /// it matches one, the item it would find, since elsewhere it would
    /// point where the command can't go. `None` when it matches no item, or
    /// for a name of only colons, whose `::*` would match every item.
    fn module_pattern(&self, query: &str, takes: Takes) -> Option<String> {
        if query.chars().all(|c| c == ':') {
            return None;
        }
        let module = query.strip_suffix("::").unwrap_or(query);
        let pattern = format!("{module}::*");
        let matched = self.items_matched(&pattern).map(Node::Item);
        let nodes: Vec<Node> = match takes {
            Takes::Set => matched.collect(),
            // A second match rules it out, so the walk stops there.
            Takes::One => matched.take(2).collect(),
        };
        let can_take = match takes {
            Takes::Set => !nodes.is_empty(),
            Takes::One => nodes.len() == 1,
        };
        can_take.then(|| offered(&pattern, &text::count_text(self.binary, &nodes)))
    }

    /// The error for `pattern` when it matches no item: as for a name that
    /// finds nothing, but what it may mean is first a pattern like it that
    /// matches items (see [`Names::pattern_fix`]); or else the imports that
    /// it matches, which a pattern doesn't take, since their code isn't in
    /// the file, though `why` and `callers` take their names as SYMBOL; or
    /// else names like it, as for a C++ name typed with a space before a
    /// pointer's `*`, which makes it a pattern.
    pub(in crate::commands) fn nothing_matches(&self, pattern: &Pattern) -> String {
        let typed = pattern.typed();
        let may_mean = match self.pattern_fix(pattern) {
            Some(fix) => vec![fix],
            None => {
                let imports = self.imports_matching(pattern);
                match imports[..] {
                    [] => self.similar(typed),
                    _ => imports,
                }
            }
        };
        self.not_found(typed, &may_mean)
    }

    /// The error for `query` when nothing has the name or matches it: that
    /// no item or import has the name, then why, when elfq can tell, and
    /// what to type instead, `may_mean`, if there is any.
    fn not_found(&self, query: &str, may_mean: &[String]) -> String {
        let mut message = format!("no function or data object is named `{query}`");
        if let Some(reason) = self.unnamed().or_else(|| self.inlined_reason(query)) {
            message += &format!("; {reason}");
        }
        if !may_mean.is_empty() {
            message += &format!("; did you mean {}?", may_mean.join(", "));
        }
        message
    }

    /// A pattern like `pattern`, which matches no item, that matches some,
    /// as the error offers it, with how many it matches. The first that
    /// matches of: the pattern without quotes around it, which the shell
    /// takes off but a script may leave on, as "on_* (3 functions), without
    /// the quotes"; in the file's case (see [`Names::in_files_case`]), as
    /// "'uart_*' (7 items)"; without a leading `::`; and with `*` in front,
    /// unless it starts with `*` or `::`. `None` when none of these matches.
    fn pattern_fix(&self, pattern: &Pattern) -> Option<String> {
        let typed = pattern.typed();
        let unquoted = ['\'', '"']
            .into_iter()
            .find_map(|quote| typed.strip_prefix(quote)?.strip_suffix(quote));
        if let Some(unquoted) = unquoted
            && let Some(count) = self.matched_count(unquoted)
        {
            return Some(format!("{unquoted} ({count}), without the quotes"));
        }
        let with_star = !typed.starts_with('*') && !typed.starts_with("::");
        let fixes = [
            self.in_files_case(pattern),
            typed.strip_prefix("::").map(str::to_string),
            with_star.then(|| format!("*{typed}")),
        ];
        fixes.into_iter().flatten().find_map(|fix| {
            let count = self.matched_count(&fix)?;
            Some(offered(&fix, &count))
        })
    }

    /// How many items `typed`, as a pattern, matches, as the error counts
    /// them: "7 items" or "3 functions". `None` when it isn't a pattern, or
    /// matches none.
    fn matched_count(&self, typed: &str) -> Option<String> {
        let nodes: Vec<Node> = self.items_matched(typed).map(Node::Item).collect();
        (!nodes.is_empty()).then(|| text::count_text(self.binary, &nodes))
    }

    /// The imports whose names `pattern` matches, the first five of them,
    /// as the error names them.
    fn imports_matching(&self, pattern: &Pattern) -> Vec<String> {
        const NAMED: usize = 5;
        (self.binary.imports.iter())
            .filter(|import| pattern.matches_name(import))
            .take(NAMED)
            .map(|import| text::shell_quote(import))
            .collect()
    }

    /// The error for `query` when it finds `nodes`, several things, as the
    /// commands that take one item, such as `why`, give it: that it could
    /// mean each, then `instruction`, what to do instead, such as
    /// [`USE_ONE`], and what to type for each (see [`Names::choice_lines`]).
    /// The commands that take a set give it through [`Names::choices`].
    pub(super) fn could_mean(&self, query: &str, nodes: &[Node], instruction: &str) -> String {
        let first_line = format!(
            "`{query}` could mean {}; {instruction}:",
            text::count_text(self.binary, nodes)
        );
        self.with_choice_lines(first_line, nodes)
    }

    /// The error for `query` when it finds `nodes`, several things, in a
    /// command that takes a set, as `retained`, `callers` and `callees` do:
    /// the error of [`Names::could_mean`], which says to use one or more
    /// of them ([`USE_ONE_OR_MORE`]) and ends with the pattern that takes
    /// them all, when there is one (see [`Names::pattern_for_all`]): "use
    /// one or more on the left instead, or 'checksum*' for all 3".
    pub(in crate::commands) fn choices(&self, query: &str, nodes: &[Node]) -> String {
        let instruction = match self.pattern_for_all(query, nodes) {
            Some(pattern) => format!("{USE_ONE_OR_MORE}, or {pattern} for all {}", nodes.len()),
            None => USE_ONE_OR_MORE.to_string(),
        };
        self.could_mean(query, nodes, &instruction)
    }

    /// The pattern that takes all of `nodes`, the things that `query` could
    /// mean, and nothing else, quoted for the shell: `query` with a `*`
    /// after it, as 'checksum*' for `checksum`, when typing that as SYMBOL
    /// would take just those items. `None` when it would take more, as
    /// 'run*' takes `run_with_cstr_allocating` besides the functions named
    /// `run`, or fewer, as when an import is among `nodes`, since a pattern
    /// takes only items, or when it isn't read as a pattern, as when an
    /// item has that name.
    fn pattern_for_all(&self, query: &str, nodes: &[Node]) -> Option<String> {
        let typed = format!("{query}*");
        let Ok(LookupOrMatch::Pattern(matched)) = self.look_up_or_match(&typed) else {
            return None;
        };
        let mut taken: Vec<Node> = (matched.matches.iter())
            .map(|found| Node::Item(found.item))
            .collect();
        let mut listed = nodes.to_vec();
        taken.sort_unstable();
        listed.sort_unstable();
        (taken == listed).then(|| text::shell_quote(&typed))
    }

    /// The error for `matched`, a pattern that matches several items, in a
    /// command that takes one item, such as `why`: how many it matches, and
    /// what to type for each, in address order, as the error for a name
    /// that could mean several lists them (see [`Names::could_mean`]).
    pub(super) fn matches_several(&self, matched: &PatternMatches) -> String {
        let nodes: Vec<Node> = (matched.matches.iter())
            .map(|found| Node::Item(found.item))
            .collect();
        let first_line = format!(
            "`{}` matches {}; {USE_ONE}:",
            matched.pattern.typed(),
            text::count_text(self.binary, &nodes)
        );
        self.with_choice_lines(first_line, &nodes)
    }

    /// An error that lists `nodes`: `first_line`, then the lines that say
    /// what to type for each (see [`Names::choice_lines`]), indented under
    /// it.
    fn with_choice_lines(&self, first_line: String, nodes: &[Node]) -> String {
        let mut message = first_line;
        for line in self.choice_lines(nodes) {
            message += &format!("\n  {line}");
        }
        message
    }

    /// The lines of an error that list `nodes`, things that SYMBOL could
    /// mean: what to type for each, next to its full name, one to a line,
    /// for the first ten, then how many more there are. An item whose
    /// shortest name is longer than 40 characters gets its address
    /// instead, so that the names on the right line up close by.
    fn choice_lines(&self, nodes: &[Node]) -> Vec<String> {
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
        let mut lines: Vec<String> = (rows.into_iter())
            .map(|(query, name)| match query == name {
                true => query,
                false => format!("{query:width$}  {name}"),
            })
            .collect();
        if shown < nodes.len() {
            lines.push(format!("and {} more", nodes.len() - shown));
        }
        lines
    }
}

/// What the error for SYMBOL that finds several things says to do in a
/// command that takes one item ([`Takes::One`]), and the error for a
/// pattern that matches several items there. Browse's go-to box takes it
/// off the error's first line, since it lists the things in its rows.
pub(in crate::commands) const USE_ONE: &str = "use one on the left instead";

/// What the error for SYMBOL that finds several things says to do in a
/// command that takes a set ([`Takes::Set`]), as `retained`, `callers` and
/// `callees` do: such a command takes several SYMBOLs, so the user can
/// give one or more of the things. [`Names::choices`] words that error
/// for every such command.
const USE_ONE_OR_MORE: &str = "use one or more on the left instead";

/// `pattern` as the error offers it, quoted for the shell, with `count`,
/// how many items it matches: "'uart_*' (7 items)".
fn offered(pattern: &str, count: &str) -> String {
    format!("{} ({count})", text::shell_quote(pattern))
}

impl Inlined {
    /// The function that `query` names, as its name or a short name, when
    /// it names only one.
    fn only_function(&self, query: &str) -> Option<usize> {
        let mut found: Vec<usize> = self.by_name.get(query).copied().into_iter().collect();
        if let Some(key) = names::short_name_key(query)
            && let Some(filed) = self.filed.get(key)
        {
            let by_short_name = filed.by_short_name.get_or_init(|| {
                let mut by_short_name: HashMap<String, Vec<usize>> = HashMap::new();
                for &function in &filed.functions {
                    for short_name in
                        names::short_names_with_key(&self.functions[function].name, key)
                    {
                        add_index(by_short_name.entry(short_name).or_default(), function);
                    }
                }
                by_short_name
            });
            for &function in by_short_name.get(query).into_iter().flatten() {
                if !found.contains(&function) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::symbol::{Lookup, fixtures};

    #[test]
    fn a_name_that_finds_nothing_offers_the_pattern_for_what_is_in_it() {
        let binary = fixtures::functions(&[
            "app::Gain::run",
            "app::Gain::new",
            "app::Config::load",
            "main",
        ]);
        let names = Names::new(&binary);
        let offers = |query: &str, takes: Takes| names.module_pattern(query, takes);
        let offer = |pattern: &str| Some(pattern.to_string());
        // A command that takes a set gets it when it matches any item.
        assert_eq!(
            offers("app::Gain", Takes::Set),
            offer("'app::Gain::*' (2 functions)")
        );
        assert_eq!(offers("app", Takes::Set), offer("'app::*' (3 functions)"));
        // One that takes one item, only when it matches one.
        assert_eq!(offers("app::Gain", Takes::One), None);
        let config = offer("'app::Config::*' (1 function)");
        assert_eq!(offers("app::Config", Takes::One), config);
        // A `::` at the end is left out.
        assert_eq!(offers("app::Config::", Takes::One), config);
        assert_eq!(offers("app::Config::", Takes::Set), config);
        // None that matches nothing, nor for a name of only colons, whose
        // `::*` would match every item.
        for query in ["nosuch", "main", "", "::", ":::"] {
            for takes in [Takes::One, Takes::Set] {
                assert_eq!(offers(query, takes), None, "{query}");
            }
        }
        // The error offers it before the names like it.
        assert_eq!(
            names.nothing_named("app::Config", Takes::One),
            "no function or data object is named `app::Config`; did you mean 'app::Config::*' (1 \
             function), app::Config::load?"
        );
    }

    #[test]
    fn a_pattern_that_matches_several_items_lists_them_in_address_order() {
        let error = |functions: &[String], typed: &str| {
            let binary = fixtures::functions(functions);
            let names = Names::new(&binary);
            let pattern = Pattern::parse(typed).unwrap();
            let matches = names.matching(&pattern);
            names.matches_several(&PatternMatches { pattern, matches })
        };
        let functions = ["on_timer", "on_data", "main", "on_button"].map(String::from);
        assert_eq!(
            error(&functions, "on_*"),
            "`on_*` matches 3 functions; use one on the left instead:\n  on_timer\n  on_data\n  \
             on_button"
        );
        // As for a name that could mean several, the first ten, then how
        // many more.
        let functions: Vec<String> = (1..=12).map(|number| format!("irq{number}")).collect();
        let error = error(&functions, "irq*");
        let lines: Vec<&str> = error.lines().collect();
        assert_eq!(
            lines[0],
            "`irq*` matches 12 functions; use one on the left instead:"
        );
        assert_eq!(lines[1..3], ["  irq1", "  irq2"]);
        assert_eq!(lines[10..], ["  irq10", "  and 2 more"]);
    }

    #[test]
    fn a_set_command_offers_the_pattern_for_all_that_a_name_could_mean() {
        let mut binary = fixtures::functions(&[
            "app::checksum::<u8>",
            "app::checksum::<u16>",
            "app::send(char*)",
            "drv::send(char*)",
            "app::run",
            "drv::run",
            "drv::run_all",
            "log",
            "main",
        ]);
        binary.imports.push("log".to_string());
        let names = Names::new(&binary);
        // The first line of the error for a name that could mean several,
        // in a command that `takes` one item or a set.
        let first_line = |query: &str, takes: Takes| {
            let several = Lookup::Several(names.nodes(query));
            let error = names.one_or_error(query, several, takes).unwrap_err();
            error.to_string().lines().next().unwrap().to_string()
        };
        let in_set = |query: &str| first_line(query, Takes::Set);
        // It says to use one or more, and the name with a `*` after it
        // takes just what it could mean, even with a pointer's `*` in it.
        assert_eq!(
            in_set("checksum"),
            "`checksum` could mean 2 functions; use one or more on the left instead, or \
             'checksum*' for all 2:"
        );
        assert_eq!(
            in_set("send(char*)"),
            "`send(char*)` could mean 2 functions; use one or more on the left instead, or \
             'send(char*)*' for all 2:"
        );
        // Not when it takes more, as 'run*' takes `drv::run_all`, or fewer,
        // as when an import is among them, which a pattern never takes.
        assert_eq!(
            in_set("run"),
            "`run` could mean 2 functions; use one or more on the left instead:"
        );
        assert_eq!(
            in_set("log"),
            "`log` could mean 2 items; use one or more on the left instead:"
        );
        // A command that takes one item says to use one, and offers none.
        assert_eq!(
            first_line("checksum", Takes::One),
            "`checksum` could mean 2 functions; use one on the left instead:"
        );
    }
}
