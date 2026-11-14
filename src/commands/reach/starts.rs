//! `elfq FILE reach` without SYMBOL: where the program starts, and the
//! bytes of code and data that each start reaches, as text and as JSON.
//! Browse's start page lists them too.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::binary::{
    Binary, FileKind, MemoryBlock, STARTUP_ARRAYS, exception_name, is_reserved, memory_blocks,
};
#[cfg(feature = "tui")]
use crate::commands::flat_answer::FlatTab;
use crate::commands::flat_answer::{FlatAnswer, FlatRow};
use crate::commands::symbol::Names;
use crate::commands::text::Detail;
use crate::commands::{json, text};
use crate::graph::{Graph, Node, Reached, RootKind, listed_in};
use crate::render::Align;

/// What `elfq FILE reach` lists without SYMBOL: where the program starts,
/// and what each start reaches.
#[derive(Serialize)]
struct StartsJson<'a> {
    /// Each start, in the text's order (see [`Starts::find`]).
    starts: Vec<StartJson<'a>>,
    /// The functions where the program starts whose end elfq doesn't know,
    /// so it can't read their code, such as `_init`.
    unreadable: Vec<json::Item<'a>>,
}

#[derive(Serialize)]
struct StartJson<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Why it is a root, as `root` in `why --json` says it, or `null` for
    /// `main`.
    root: Option<String>,
    /// The exceptions that it handles, as the WHY column names them, but
    /// every one.
    exceptions: Vec<String>,
    /// As `reach START --json` counts them.
    code_bytes: u64,
    data_bytes: u64,
    /// Every block of memory that the overview's `memory` lists, with the
    /// bytes in it that the start and what it reaches take.
    blocks: Vec<BlockBytesJson>,
    /// Whether elfq can follow everything that the start reaches, as in
    /// `reach START --json`.
    complete: bool,
}

#[derive(Serialize)]
struct BlockBytesJson {
    #[serde(flatten)]
    range: json::Range,
    code_bytes: u64,
    data_bytes: u64,
}

/// Where the program starts, and what each start reaches.
pub(in crate::commands) struct Starts<'a> {
    binary: &'a Binary,
    /// The starts, in the order of [`Starts::find`].
    pub(in crate::commands) starts: Vec<Start>,
    /// The roots that are functions whose end elfq doesn't know, such as
    /// `_init`, as indexes into [`Binary::items`]: elfq can't tell what
    /// they reach.
    unreadable: Vec<usize>,
    /// The file's blocks of memory, as the overview lists them.
    blocks: Vec<MemoryBlock>,
}

/// One place where the program starts, and what it reaches.
pub(in crate::commands) struct Start {
    /// The function, as an index into [`Binary::items`].
    index: usize,
    /// Why it is a root, or `None` for `main`.
    root: Option<RootKind>,
    /// The entries of the vector table that list it, in the table's order,
    /// counting the stack pointer's as 0.
    exceptions: Vec<u64>,
    /// What it reaches, added up.
    reached: Reached,
}

/// A row of the table of starts.
enum StartRow {
    /// A start, as an index into [`Starts::starts`].
    Start(usize),
    /// How many of the starts above, all roots of this kind, the table
    /// leaves out.
    More(RootKind, usize),
}

impl<'a> Starts<'a> {
    /// Finds where the program starts, as `elfq FILE reach` lists it:
    /// `main`, but not a shared library's; then each root that is a
    /// function whose code elfq can read, other than a PLT's header: the
    /// entry point; the handlers in the vector table, in the order of their
    /// first entries; the functions that the startup and exit arrays list,
    /// `.preinit_array`, `.init_array` and `.fini_array` in that order,
    /// each in its array's order; the functions in `.init` and `.fini`, by
    /// section; then the rest, such as personality routines, by address.
    /// Each comes with what it reaches, as `reach START` counts it.
    pub(in crate::commands) fn find(
        binary: &'a Binary,
        graph: &Graph,
        names: &Names,
    ) -> Starts<'a> {
        let blocks = memory_blocks(&binary.sections);
        let is_start = |index: usize| binary.items[index].is_own_function();
        let table = binary.vector_table.as_ref();
        let exceptions = |index: usize| -> Vec<u64> {
            let address = binary.items[index].address;
            table.map_or_else(Vec::new, |table| table.exceptions_of(address).collect())
        };
        let mut roots: Vec<(usize, RootKind)> = Vec::new();
        let mut unreadable = Vec::new();
        let mut arrays = Vec::new();
        for root in &graph.roots {
            let Node::Item(index) = root.node else {
                continue;
            };
            let item = &binary.items[index];
            if is_start(index) {
                roots.push((index, root.kind));
            } else if item.end_unknown() {
                unreadable.push(index);
            }
            if let RootKind::ListedIn(array) = root.kind
                && !arrays.contains(&array)
            {
                arrays.push(array);
            }
        }
        // Where each array lists each function, from its first entry.
        let mut listed: HashMap<(usize, Node), usize> = HashMap::new();
        for array in arrays {
            for (position, node) in listed_in(binary, array).into_iter().enumerate() {
                listed.entry((array, node)).or_insert(position);
            }
        }
        // A root's key is its kind's place, then for an array its place
        // in STARTUP_ARRAYS, then the section (the array, or the one that
        // the root is in), then the function's place in its array or a
        // handler's first entry. The roots come by node, so by address: a
        // stable sort keeps that order among roots of the same kind that
        // no array lists. Each root's key is worked out once, not on each
        // comparison, since a handler's reads the vector table.
        roots.sort_by_cached_key(|&(index, kind)| match kind {
            RootKind::Entry => (0, 0, 0, 0),
            RootKind::Handler => (1, 0, 0, exceptions(index).first().copied().unwrap_or(0)),
            RootKind::ListedIn(array) => {
                let name = binary.sections[array].name.as_str();
                let rank = STARTUP_ARRAYS.iter().position(|&startup| startup == name);
                let position = listed.get(&(array, Node::Item(index))).copied();
                (
                    2,
                    rank.unwrap_or(usize::MAX),
                    array,
                    position.unwrap_or(0) as u64,
                )
            }
            RootKind::InSection(section) => (3, 0, section, 0),
            RootKind::PltHeader | RootKind::Personality | RootKind::VectorTable => (4, 0, 0, 0),
        });
        // A main that is a root too is listed as one. A shared library's
        // isn't looked up: in one that has no main, looking it up would
        // first file every item under its short names' keys.
        let main = if binary.kind == FileKind::SharedLibrary {
            None
        } else {
            match names.nodes("main") {
                [Node::Item(index)]
                    if is_start(*index) && graph.root(Node::Item(*index)).is_none() =>
                {
                    Some((*index, None))
                }
                _ => None,
            }
        };
        let starts = main
            .into_iter()
            .chain(roots.into_iter().map(|(index, kind)| (index, Some(kind))))
            .map(|(index, root)| Start {
                index,
                root,
                exceptions: exceptions(index),
                reached: Reached::find(binary, graph, index, &blocks),
            })
            .collect();
        Starts {
            binary,
            starts,
            unreadable,
            blocks,
        }
    }

    /// The answer as browse's start page shows it, for the file at `path`:
    /// with every start and name, as with `--all`, the notes as the
    /// command writes them without `--all`, and whether the command needs
    /// `--all` to print it all.
    #[cfg(feature = "tui")]
    pub(in crate::commands) fn tab(&self, path: &Path) -> FlatTab {
        FlatTab::new(self.text(path, Detail::All), self.text(path, Detail::Brief))
    }

    /// The answer's text: the starts as a table, then what elfq can't
    /// follow or read, and for a shared library, that its exported
    /// functions aren't counted. Each row is about its start.
    pub(super) fn text(&self, path: &Path, detail: Detail) -> FlatAnswer {
        let file = path.display();
        let rows = self.rows(detail);
        let mut answer = FlatAnswer::default();
        if !self.starts.is_empty() {
            let each = if self.several_blocks() {
                " in each block of memory"
            } else {
                ""
            };
            answer.lead.push(format!(
                "Where {file} starts, and the bytes of code and data that each reaches{each}:"
            ));
            answer.columns = self.columns();
            answer.rows = (rows.iter())
                .map(|row| match *row {
                    StartRow::Start(start) => self.start_row(&self.starts[start]),
                    StartRow::More(kind, count) => self.more_row(kind, count),
                })
                .collect();
        } else if self.unreadable.is_empty() {
            answer
                .lead
                .push(format!("elfq found no place where {file} starts."));
        }
        answer.notes = self.notes(path, &rows, detail);
        answer
    }

    /// Whether the file has several blocks of memory, so that the table
    /// gives the bytes that each start reaches in each, in place of CODE
    /// and DATA.
    fn several_blocks(&self) -> bool {
        self.blocks.len() > 1
    }

    /// The table's columns: the bytes that a start reaches, in each block
    /// of memory or as CODE and DATA, then WHY and START.
    fn columns(&self) -> Vec<(String, Align)> {
        let mut columns: Vec<(String, Align)> = if self.several_blocks() {
            self.blocks
                .iter()
                .map(|block| (text::range_start(block.range), Align::Right))
                .collect()
        } else {
            vec![
                ("CODE".to_string(), Align::Right),
                ("DATA".to_string(), Align::Right),
            ]
        };
        columns.push(("WHY".to_string(), Align::Left));
        columns.push(("START".to_string(), Align::Left));
        columns
    }

    /// The row of `start`: the bytes it reaches, why it is a start, and its
    /// name, with "(may reach more)" after it when elfq can't follow or
    /// read all that it reaches.
    fn start_row(&self, start: &Start) -> FlatRow {
        let binary = self.binary;
        let reached = &start.reached;
        let mut cells: Vec<String> = if self.several_blocks() {
            (reached.blocks.iter())
                .map(|block| block.total().to_string())
                .collect()
        } else {
            vec![
                reached.bytes.code.to_string(),
                reached.bytes.data.to_string(),
            ]
        };
        cells.push(start.why(binary));
        let rest = if reached.complete() {
            ""
        } else {
            " (may reach more)"
        };
        FlatRow::new(
            cells,
            binary.items[start.index].name.clone(),
            Some(Node::Item(start.index)),
        )
        .with_rest(rest.to_string())
    }

    /// The row such as "650 more" that counts the `count` starts, all roots
    /// of `kind`, that the table leaves out, with the WHY of their kind.
    fn more_row(&self, kind: RootKind, count: usize) -> FlatRow {
        // Empty under the columns of bytes, all but WHY and START.
        let mut cells = vec![String::new(); self.columns().len() - 2];
        cells.push(text::root_column(self.binary, kind));
        FlatRow::new(cells, format!("{count} more"), None)
    }

    /// The notes under the table: what the starts marked (may reach more)
    /// among `rows` reach, the roots whose end elfq doesn't know, and for
    /// a shared library, that its exported functions aren't counted.
    fn notes(&self, path: &Path, rows: &[StartRow], detail: Detail) -> Vec<String> {
        let mut notes = Vec::new();
        // The marks in the table, of the starts that it shows.
        let shown: Vec<&Reached> = rows
            .iter()
            .filter_map(|row| match *row {
                StartRow::Start(start) => Some(&self.starts[start].reached),
                StartRow::More(..) => None,
            })
            .collect();
        let unfollowed = shown.iter().any(|reached| reached.unfollowed);
        let unreadable = shown.iter().any(|reached| reached.unreadable);
        let marked = match (unfollowed, unreadable) {
            (true, false) => Some("calls or jumps that elfq can't follow"),
            (false, true) => Some("code that elfq can't read"),
            (true, true) => {
                Some("calls or jumps that elfq can't follow, or code that it can't read")
            }
            (false, false) => None,
        };
        if let Some(marked) = marked {
            notes.push(format!(
                "The starts marked (may reach more) reach {marked}; reach START names them."
            ));
        }
        if !self.unreadable.is_empty() {
            notes.push(self.unreadable_text(path, detail));
        }
        if self.binary.kind == FileKind::SharedLibrary {
            notes.push(
                "elfq doesn't count a shared library's exported functions as starting points yet."
                    .to_string(),
            );
        }
        notes
    }

    /// The rows of the table: every start with [`Detail::All`]; otherwise
    /// `main`, the entry point and the handlers, and of the other starts,
    /// those that reach the most bytes, up to [`text::shown`] rows in all,
    /// with a row such as "650 more" after the starts of each kind that it
    /// leaves out. Of starts that reach as many bytes, the first is kept.
    fn rows(&self, detail: Detail) -> Vec<StartRow> {
        let is_other =
            |start: &Start| !matches!(start.root, None | Some(RootKind::Entry | RootKind::Handler));
        let mut others: Vec<usize> = (0..self.starts.len())
            .filter(|&start| is_other(&self.starts[start]))
            .collect();
        let room = text::shown(detail).saturating_sub(self.starts.len() - others.len());
        others.sort_by_key(|&start| Reverse(self.starts[start].reached.bytes.total()));
        let left_out: HashSet<usize> = others[text::shown_count(others.len(), room)..]
            .iter()
            .copied()
            .collect();
        let mut rows = Vec::new();
        let mut first = 0;
        for same_kind in self.starts.chunk_by(|a, b| a.root == b.root) {
            let starts = first..first + same_kind.len();
            first += same_kind.len();
            let hidden = starts
                .clone()
                .filter(|start| left_out.contains(start))
                .count();
            // A row for one start takes no more room than "1 more" would.
            let hide = |start: &usize| hidden > 1 && left_out.contains(start);
            rows.extend(starts.filter(|start| !hide(start)).map(StartRow::Start));
            if let Some(root) = same_kind[0].root
                && hidden > 1
            {
                rows.push(StartRow::More(root, hidden));
            }
        }
        rows
    }

    /// Names the roots whose end elfq doesn't know, which the table leaves
    /// out, as "_init and _fini are starts too, but elfq doesn't know where
    /// they end, ...", or when they are the only roots, "_start is where
    /// FILE starts, ...".
    fn unreadable_text(&self, path: &Path, detail: Detail) -> String {
        let binary = self.binary;
        let names: Vec<&str> = self
            .unreadable
            .iter()
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        let one = names.len() == 1;
        let what = match (self.starts.is_empty(), one) {
            (true, true) => format!("is where {} starts", path.display()),
            (true, false) => format!("are where {} starts", path.display()),
            (false, true) => "is a start too".to_string(),
            (false, false) => "are starts too".to_string(),
        };
        let (it, ends, reaches) = if one {
            ("it", "ends", "reaches")
        } else {
            ("they", "end", "reach")
        };
        format!(
            "{} {what}, but elfq doesn't know where {it} {ends}, so it can't count what {it} \
             {reaches}.",
            text::names_text(&names, detail)
        )
    }

    pub(super) fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |index| json::Item::new(binary, Node::Item(index));
        let answer = StartsJson {
            starts: self
                .starts
                .iter()
                .map(|start| StartJson {
                    item: item(start.index),
                    root: start.root.map(|kind| json::root_field(binary, kind)),
                    exceptions: start.exception_names(binary),
                    code_bytes: start.reached.bytes.code,
                    data_bytes: start.reached.bytes.data,
                    blocks: self
                        .blocks
                        .iter()
                        .zip(&start.reached.blocks)
                        .map(|(block, bytes)| BlockBytesJson {
                            range: json::Range::new(block.range),
                            code_bytes: bytes.code,
                            data_bytes: bytes.data,
                        })
                        .collect(),
                    complete: start.reached.complete(),
                })
                .collect(),
            unreadable: self.unreadable.iter().map(|&index| item(index)).collect(),
        };
        json::write(&answer, out)
    }
}

impl Start {
    /// Why it is a start, for the WHY column: "main function"; the
    /// exceptions it handles, as in "NMI, HardFault and 6 more", with
    /// "(entry point)" after them for the handler that is the entry point,
    /// usually the reset handler; or why it is a root, as the HOW column of
    /// `why` says it.
    fn why(&self, binary: &Binary) -> String {
        let names = self.exception_names(binary);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let exceptions = text::column_names(&names);
        match self.root {
            None => "main function".to_string(),
            Some(RootKind::Entry) if names.is_empty() => "entry point".to_string(),
            Some(RootKind::Entry) => format!("{exceptions} (entry point)"),
            Some(RootKind::Handler) => exceptions,
            Some(kind) => text::root_column(binary, kind),
        }
    }

    /// The names of the exceptions it handles, in the vector table's order
    /// (see [`exception_name`]), but not the reserved entries that list it,
    /// since the processor takes none of them: "reserved" only for a
    /// handler that no other entry lists. None for a function that isn't a
    /// handler.
    fn exception_names(&self, binary: &Binary) -> Vec<String> {
        let architecture = binary.arm_architecture;
        let names: Vec<String> = self
            .exceptions
            .iter()
            .filter(|&&number| !is_reserved(number, architecture))
            .map(|&number| exception_name(number, architecture))
            .collect();
        match self.exceptions.first() {
            Some(&number) if names.is_empty() => vec![exception_name(number, architecture)],
            _ => names,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::CodeAndData;
    use crate::graph::Root;

    /// c-x86_64.elf, whose functions the tests below make starts of.
    fn sample() -> Binary {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        Binary::load(&path).unwrap()
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    fn section(binary: &Binary, name: &str) -> usize {
        binary
            .sections
            .iter()
            .position(|section| section.name == name)
            .unwrap()
    }

    /// A start at `name`, a root of `kind` or `main`, that reaches `code`
    /// bytes of code, and calls or jumps that elfq can't follow if
    /// `unfollowed`, or code that it can't read if `unreadable`.
    fn start(
        binary: &Binary,
        name: &str,
        root: Option<RootKind>,
        code: u64,
        (unfollowed, unreadable): (bool, bool),
    ) -> Start {
        Start {
            index: index(binary, name),
            root,
            exceptions: Vec::new(),
            reached: Reached {
                bytes: CodeAndData { code, data: 0 },
                blocks: vec![CodeAndData { code, data: 0 }],
                count: 1,
                unfollowed,
                unreadable,
            },
        }
    }

    fn written(starts: &Starts, detail: Detail) -> String {
        let mut out = Vec::new();
        starts
            .text(Path::new("app.elf"), detail)
            .write(&mut out)
            .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_note_says_why_starts_may_reach_more() {
        let binary = sample();
        let note = |marks: &[(bool, bool)]| {
            let starts = Starts {
                binary: &binary,
                starts: marks
                    .iter()
                    .map(|&marks| start(&binary, "main", None, 10, marks))
                    .collect(),
                unreadable: Vec::new(),
                blocks: memory_blocks(&binary.sections),
            };
            written(&starts, Detail::Brief)
                .lines()
                .last()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            note(&[(true, false)]),
            "The starts marked (may reach more) reach calls or jumps that elfq can't follow; \
             reach START names them."
        );
        assert_eq!(
            note(&[(false, true)]),
            "The starts marked (may reach more) reach code that elfq can't read; reach START \
             names them."
        );
        assert_eq!(
            note(&[(true, false), (false, true)]),
            "The starts marked (may reach more) reach calls or jumps that elfq can't follow, or \
             code that it can't read; reach START names them."
        );
        assert_eq!(note(&[(false, false)]), "  10     0  main function  main");
    }

    #[test]
    fn starts_whose_end_elfq_doesnt_know_are_named_below_the_table() {
        let binary = sample();
        let starts = |starts, unreadable: &[&str]| Starts {
            binary: &binary,
            starts,
            unreadable: unreadable.iter().map(|name| index(&binary, name)).collect(),
            blocks: memory_blocks(&binary.sections),
        };
        let main = || vec![start(&binary, "main", None, 10, (false, false))];
        assert_eq!(
            written(&starts(main(), &["_init", "_fini"]), Detail::Brief),
            "\
Where app.elf starts, and the bytes of code and data that each reaches:

CODE  DATA  WHY            START
  10     0  main function  main

_init and _fini are starts too, but elfq doesn't know where they end, so it can't count what they reach.
"
        );
        assert_eq!(
            written(&starts(main(), &["_init"]), Detail::Brief)
                .lines()
                .last(),
            Some(
                "_init is a start too, but elfq doesn't know where it ends, so it can't count \
                 what it reaches."
            )
        );
        // Without a table, the sentence is the answer.
        assert_eq!(
            written(&starts(Vec::new(), &["_init"]), Detail::Brief),
            "_init is where app.elf starts, but elfq doesn't know where it ends, so it can't \
             count what it reaches.\n"
        );
        assert_eq!(
            written(&starts(Vec::new(), &[]), Detail::Brief),
            "elfq found no place where app.elf starts.\n"
        );
    }

    #[test]
    fn the_table_of_starts_shows_the_starts_that_reach_the_most() {
        let binary = sample();
        let init_array = RootKind::ListedIn(section(&binary, ".init_array"));
        let fini_array = RootKind::ListedIn(section(&binary, ".fini_array"));
        // The entry point, then 31 functions that .init_array lists, each
        // reaching more than the one before, and 2 that .fini_array lists.
        let mut all = vec![start(
            &binary,
            "_start",
            Some(RootKind::Entry),
            1,
            (false, false),
        )];
        for code in 0..31 {
            all.push(start(
                &binary,
                "frame_dummy",
                Some(init_array),
                code,
                (false, false),
            ));
        }
        for code in [100, 0] {
            let name = "__do_global_dtors_aux";
            all.push(start(&binary, name, Some(fini_array), code, (false, false)));
        }
        let starts = Starts {
            binary: &binary,
            starts: all,
            unreadable: Vec::new(),
            blocks: memory_blocks(&binary.sections),
        };
        // The entry point is always shown. Of the other 33 starts, 29 more
        // fit: of .init_array's, the 28 that reach the most, and of
        // .fini_array's, the one that reaches 100 bytes, and the other too,
        // since a row of its own takes no more room than "1 more".
        let rows: Vec<String> = starts
            .rows(Detail::Brief)
            .iter()
            .map(|row| match *row {
                StartRow::Start(start) => starts.starts[start].reached.bytes.code.to_string(),
                StartRow::More(kind, count) => format!("{count} more {kind:?}"),
            })
            .collect();
        let mut expected: Vec<String> = ["1"].into_iter().map(String::from).collect();
        expected.extend((3..31).map(|code: u64| code.to_string()));
        expected.push(format!("3 more {init_array:?}"));
        expected.extend(["100".to_string(), "0".to_string()]);
        assert_eq!(rows, expected);
        assert!(written(&starts, Detail::Brief).contains("  listed in .init_array  3 more\n"));
        assert_eq!(starts.rows(Detail::All).len(), 34);
    }

    #[test]
    fn a_main_that_is_a_root_is_listed_as_one() {
        let binary = sample();
        let mut graph = Graph::build(&binary);
        let names = Names::new(&binary);
        // As if the unwinder called main.
        let main = Node::Item(index(&binary, "main"));
        graph.roots.push(Root {
            node: main,
            kind: RootKind::Personality,
        });
        graph.roots.sort_by_key(|root| root.node);
        let starts = Starts::find(&binary, &graph, &names);
        let listed: Vec<(&str, String)> = starts
            .starts
            .iter()
            .map(|start| (binary.items[start.index].name.as_str(), start.why(&binary)))
            .collect();
        assert_eq!(
            listed,
            [
                ("_start", "entry point".to_string()),
                ("frame_dummy", "listed in .init_array".to_string()),
                ("__do_global_dtors_aux", "listed in .fini_array".to_string()),
                ("main", "used by the unwinder".to_string()),
            ]
        );
    }
}
