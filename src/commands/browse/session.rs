//! The file being browsed, and what the pages ask about it.
//!
//! The file is loaded, and its reference graph built, once, before the
//! screen switches, on elfq's own thread with its large stack, as for
//! every command. Pages then ask the commands' own code for their answers,
//! such as `callers::text` for "used by N", so that a page says what the
//! commands print. What keeps each item, which the pages' headers say as
//! `retained` and `why` do, takes a while to work out in a large program,
//! so it is worked out once, after the first page is drawn.
//!
//! # Names
//!
//! The pages name an item or import in six ways, each in its own places:
//!
//! - Its full name, from `Node::name`, as in `app::log(unsigned int)`: in
//!   the name column of a command's rows, which `names.rs` fits to the
//!   column, in the link previews on the status line, and in an import's
//!   title. Tab 3's first line has it, as in "Nothing in the file calls or
//!   uses vfiprintf.", which shows only when nothing does (see
//!   [`Session::callers_text`]). So does what u says where no one item
//!   keeps the item, as in "Removing any single item wouldn't remove
//!   vfiprintf."
//! - Its row name, from [`Names::row_name`], as in `frames` or
//!   `0x30000de8`: in the notes on tab 1's lines, as in `uses
//!   frames+0x8`, in what tab 1 says when it has no lines, and in an
//!   item's title. `reach` names an item so after "used by". It is the
//!   shortest name that finds only the item as SYMBOL, keeping a PLT
//!   stub's `@plt`, or failing that, the item's address.
//! - Its short name, from [`Session::short_name`], as in `crc32`: after
//!   "kept by" and "used by", in the rows of `retained` and `why` and in
//!   the header. It is the shortest form of its name that finds only it,
//!   or failing that, its whole name, so it is never an address.
//! - The header's name, on an item's page: its full name, then the note of
//!   the SYMBOL that the user typed (`Symbol::note`), as in
//!   `Default_Handler (also named NMI_Handler)`. The page gets it as
//!   `header_name`, from `Symbol::lead_name` or the go-to box's
//!   `PickerRow::lead`, and a page that a link opens gets the full name
//!   alone. `Header.name` holds the full name, and `Header.after_name`
//!   starts with the note. It differs from the lead name only in whose
//!   note it has: the SYMBOL that the user typed, not the query.
//! - Its lead name, from [`Session::lead_name`], as in `sniprintf (also
//!   named snprintf)`: in the first line of the answer under tabs 2, 4, 5
//!   and 6, and in what u says of a root or of an item that nothing
//!   reaches. It is what the command says, given the query as SYMBOL.
//! - Its query, from `Names::query_for`, as in `log(unsigned int)`: as
//!   SYMBOL, quoted for the shell, in the command under a tab (see
//!   [`Session::command`]). It is the shortest thing to type that finds
//!   only it, which can be another of its names, or an item's address.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use anyhow::Result;

use super::views::Listing;
use crate::binary::{Binary, FileKind, ItemKind, MemoryBlock, memory_blocks};
use crate::commands::callers;
use crate::commands::checks::check_code_is_readable;
use crate::commands::flat_answer::FlatAnswer;
use crate::commands::retained_tree::RetainedTree;
use crate::commands::symbol::Names;
use crate::commands::text::{CommandLine, Detail};
use crate::graph::{DominatorTree, Graph, Node, ShortestPaths, VeneerTarget, Veneers};

/// A loaded file, with what every page needs.
pub struct Session<'a> {
    /// The path as the user typed it.
    pub(super) path: PathBuf,
    pub(super) binary: &'a Binary,
    pub(super) graph: Graph,
    /// What SYMBOL finds, and the short names that rows use.
    pub(super) names: Names<'a>,
    /// Where each veneer goes, by the index of its item.
    ///
    /// A PLT stub for one of the file's own functions counts as a veneer.
    pub(super) veneers: HashMap<usize, VeneerTarget>,
    /// The blocks of memory that the sections occupy.
    ///
    /// They are as the overview lists them.
    pub(super) blocks: Vec<MemoryBlock>,
    /// The short name of each item or import, once a header names it.
    short_names: RefCell<HashMap<Node, String>>,
    /// Each item's listing, as tab 1 of its page shows it, by its index.
    ///
    /// Every page open on the item shares it, so that opening it again, as
    /// from go to, needn't work it out again. It is freed with the last of
    /// those pages.
    listings: RefCell<HashMap<usize, Weak<Listing>>>,
    /// What keeps each item, once worked out.
    ///
    /// [`Session::work_out_keepers`] works it out.
    keepers: Option<Keepers>,
}

/// What keeps each item, and the shortest chain to each.
///
/// The chains start where the program starts. `retained` and `why` find
/// the same.
pub(super) struct Keepers {
    /// Which item keeps which.
    ///
    /// It is the dominator tree that `retained` builds, without a range.
    pub(super) tree: DominatorTree,
    /// The shortest chain from where the program starts to each item.
    pub(super) paths: ShortestPaths,
}

impl<'a> Session<'a> {
    /// Builds what the pages need for `binary`, loaded from `path`.
    ///
    /// Where elfq can't read its code, it says why, as every command that
    /// reads code does.
    pub fn new(path: &Path, binary: &'a Binary) -> Result<Session<'a>> {
        check_code_is_readable(binary)?;
        let graph = Graph::build(binary);
        let veneers = {
            let finder = Veneers::new(binary, &graph);
            (0..binary.items.len())
                .filter(|&index| binary.items[index].kind == ItemKind::Function)
                .filter_map(|index| Some((index, finder.target(index)?)))
                .collect()
        };
        Ok(Session {
            path: path.to_owned(),
            binary,
            graph,
            names: Names::new(binary),
            veneers,
            blocks: memory_blocks(&binary.sections),
            short_names: RefCell::new(HashMap::new()),
            listings: RefCell::new(HashMap::new()),
            keepers: None,
        })
    }

    /// What keeps each item, once worked out.
    pub(super) fn keepers(&self) -> Option<&Keepers> {
        self.keepers.as_ref()
    }

    /// Whether the session has yet to work out what keeps each item.
    ///
    /// It needn't in a shared library, where `retained` and `why` answer
    /// about no item.
    pub(super) fn needs_keepers(&self) -> bool {
        self.keepers.is_none() && self.binary.kind != FileKind::SharedLibrary
    }

    /// The tree of retained sizes that `retained SYMBOL` lists.
    ///
    /// It is built without a range, from `keepers`, what keeps each item,
    /// and with the session's names.
    pub(super) fn retained_tree<'s>(&'s self, keepers: &'s Keepers) -> RetainedTree<'s, 'a> {
        let (binary, graph, names) = (self.binary, &self.graph, &self.names);
        RetainedTree::borrowing(binary, graph, &keepers.tree, names)
    }

    /// Works out what keeps each item, and the shortest chain to each, once.
    ///
    /// The chains start where the program starts. In a large program, this
    /// takes a while, which the status line says.
    pub(super) fn work_out_keepers(&mut self) {
        if self.needs_keepers() {
            self.keepers = Some(Keepers {
                tree: DominatorTree::build(self.binary, &self.graph, None),
                paths: ShortestPaths::from_roots(self.binary, &self.graph),
            });
        }
    }

    /// What uses `node`, as `callers SYMBOL --all` lists it.
    ///
    /// Its first line has `node`'s full name, which the page's header starts
    /// with.
    pub(super) fn callers_text(&self, node: Node) -> FlatAnswer {
        let name = node.name(self.binary);
        callers::text(self.binary, &self.graph, node, name, Detail::All)
    }

    /// `node`'s name as the answer under tabs 2, 4, 5 and 6 gives it.
    ///
    /// The command under the tab is given `node`'s query as SYMBOL, and its
    /// first line says what SYMBOL gave: "sniprintf (also named snprintf)".
    /// See the names in the module doc.
    pub(super) fn lead_name(&self, node: Node) -> String {
        match self.names.find(&self.names.query_for(node)) {
            Ok(symbol) => symbol.lead_name(self.binary),
            Err(_) => node.name(self.binary).to_string(),
        }
    }

    /// The command that prints what a page shows, to paste into a shell.
    ///
    /// It is about `node`, or without one, about the file, as in `$ elfq
    /// testdata/bin/cpp-x86_64.elf callers 'log(unsigned int)'`. It names
    /// the file as the user typed it, and `node` by its query as SYMBOL,
    /// both quoted for the shell, then `--all` when `needs_all` says that
    /// the command needs it to print all that the page shows.
    pub(super) fn command(&self, command: &str, node: Option<Node>, needs_all: bool) -> String {
        let mut line = CommandLine::new(&self.path, command);
        if let Some(node) = node {
            line = line.word(&self.names.query_for(node));
        }
        format!("$ {}", line.flag("--all", needs_all))
    }

    /// The node whose page stands for `node`.
    ///
    /// For an import, that is its PLT stub when it has one, since the
    /// import's name finds the stub as SYMBOL, in every command. So code
    /// that uses the import through the global offset table, as code built
    /// with `-fno-plt` calls it, leads to the same page as code that calls
    /// the stub. An import without a stub has a page of its own.
    pub(super) fn page_node(&self, node: Node) -> Node {
        match node {
            Node::Import(index) => match self.names.nodes(&self.binary.imports[index]) {
                &[found] => found,
                _ => node,
            },
            Node::Item(_) => node,
        }
    }

    /// The memory block that holds `address`, as an index into `blocks`.
    pub(super) fn block_of(&self, address: u64) -> Option<usize> {
        self.blocks
            .iter()
            .position(|block| block.range.contains(address))
    }

    /// The name of `node`'s page in the title line's trail.
    ///
    /// It is an item's row name, and an import's full name (see the names
    /// in the module doc). The status line names the page so while a tab of
    /// it is worked out, as in "working out what main reaches…".
    pub(super) fn title(&self, node: Node) -> String {
        match node {
            Node::Item(index) => self.names.row_name(index),
            Node::Import(_) => node.name(self.binary).to_string(),
        }
    }

    /// The short name of `node` (see the names in the module doc).
    ///
    /// The rows of `retained` and `why` call `node` so after "kept by" or
    /// "used by", to name the row they hang under: `crc32` in "crc_table,
    /// kept by crc32". It is the shortest form of its name that finds only
    /// it, or failing that, its whole name, where [`Names::row_name`]
    /// gives an address.
    pub(super) fn short_name(&self, node: Node) -> String {
        if let Some(name) = self.short_names.borrow().get(&node) {
            return name.clone();
        }
        let name = self.names.short_name(node);
        self.short_names.borrow_mut().insert(node, name.clone());
        name
    }

    /// Item `index`'s listing, as tab 1 of its page shows it.
    pub(super) fn listing(&self, index: usize) -> Rc<Listing> {
        if let Some(listing) = self.listings.borrow().get(&index).and_then(Weak::upgrade) {
            return listing;
        }
        let listing = Rc::new(Listing::of_item(self, index));
        self.listings
            .borrow_mut()
            .insert(index, Rc::downgrade(&listing));
        listing
    }

    /// The file's name, without its folders, for the title line.
    pub(super) fn file_name(&self) -> String {
        match self.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.path.display().to_string(),
        }
    }
}
