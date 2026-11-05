//! The error for SYMBOL that finds several items or none: what to type
//! for each of several, or why none has the name, as when the file has no
//! symbol table or the compiler inlined every copy of the function.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::Names;
use super::lookup::add_index;
use crate::binary::names;
use crate::commands::checks::found_functions;
use crate::commands::text;
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

/// The copies of functions that the compiler inlined, by function.
pub(super) struct Inlined {
    copies: Vec<InlinedCopy>,
    /// Each function's name, which its copies share, with its copies, as
    /// indices into `copies`.
    functions: Vec<(Arc<str>, Vec<usize>)>,
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
        let (name, copies) = &inlined.functions[inlined.only_function(query)?];
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
                text::names_text(&names, false),
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
            let mut functions: Vec<(Arc<str>, Vec<usize>)> = Vec::new();
            let mut by_name: HashMap<Arc<str>, usize> = HashMap::new();
            for (index, copy) in copies.iter().enumerate() {
                let function = *by_name.entry(copy.name.clone()).or_insert_with(|| {
                    functions.push((copy.name.clone(), Vec::new()));
                    functions.len() - 1
                });
                functions[function].1.push(index);
            }
            let mut filed: HashMap<String, FiledFunctions> = HashMap::new();
            for (function, (name, _)) in functions.iter().enumerate() {
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

    /// The error for `query` when it finds nothing: no item or import has
    /// the name, then why, when elfq can tell (see [`Names::unnamed`] and
    /// [`Names::inlined_reason`]), and the names that it may mean.
    pub(super) fn nothing_named(&self, query: &str) -> String {
        let mut message = format!("no function or data object is named `{query}`");
        if let Some(reason) = self.unnamed().or_else(|| self.inlined_reason(query)) {
            message += &format!("; {reason}");
        }
        let similar = self.similar(query);
        if !similar.is_empty() {
            message += &format!("; did you mean {}?", similar.join(", "));
        }
        message
    }

    /// The error for `query` when it finds `nodes`, several things: that it
    /// could mean each, then `instruction`, what to do instead, such as
    /// [`USE_ONE`], and what to type for each, next to its full name, one
    /// to a line. An item whose shortest name is longer than 40 characters
    /// gets its address instead, so that the names on the right line up
    /// close by.
    pub(super) fn choices(&self, query: &str, nodes: &[Node], instruction: &str) -> String {
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
            "`{query}` could mean {}; {instruction}:",
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
}

/// What the error for SYMBOL that finds several things says to do. Browse's
/// go-to box takes it off the error's first line, since it lists the things
/// in its rows.
pub(in crate::commands) const USE_ONE: &str = "use one on the left instead";

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
                    for short_name in names::short_names_with_key(&self.functions[function].0, key)
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
