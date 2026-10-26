//! The file being browsed, and what the pages ask about it.
//!
//! The file is loaded, and its reference graph built, once, before the
//! screen switches, on elfq's own thread with its large stack, as for
//! every command. Pages then ask the commands' own code for their answers,
//! such as `callers::answer` for "used by N", so that a page says what the
//! commands print. What keeps each item, which the pages' headers say as
//! `retained` and `why` do, takes a while to work out in a large program,
//! so it is worked out once, after the first page is drawn.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use anyhow::Result;

use super::views::PageCode;
use crate::binary::{Binary, FileKind, ItemKind, MemoryBlock, memory_blocks};
use crate::commands::callers::{self, Answer};
use crate::commands::retained_tree::RetainedTree;
use crate::commands::symbol::Names;
use crate::commands::{reference_graph, text};
use crate::graph::{DominatorTree, Graph, Node, ShortestPaths, VeneerTarget, Veneers};

/// A loaded file, with what every page needs.
pub struct Session<'a> {
    /// The path as the user typed it.
    pub(super) path: PathBuf,
    pub(super) binary: &'a Binary,
    pub(super) graph: Graph,
    /// What SYMBOL finds, and the short names that rows use.
    pub(super) names: Names<'a>,
    /// Where each veneer, or PLT stub for one of the file's own functions,
    /// goes, by the index of its item.
    pub(super) veneers: HashMap<usize, VeneerTarget>,
    /// The blocks of memory that the sections occupy, as the overview
    /// lists them.
    pub(super) blocks: Vec<MemoryBlock>,
    /// The name that rows give each item, by its index, worked out the
    /// first time a row names it: a page names an item on every line that
    /// uses it.
    row_names: RefCell<HashMap<usize, String>>,
    /// What rows call each item or import after "kept by" or "used by",
    /// worked out the first time a header names it.
    short_names: RefCell<HashMap<Node, String>>,
    /// Each item's code as its page shows it, by the item's index, shared
    /// by every page open on the item, so that opening it again, as from
    /// go to, needn't work it out again. It is freed with the last of
    /// those pages.
    page_codes: RefCell<HashMap<usize, Weak<PageCode>>>,
    /// What keeps each item, once worked out (see
    /// [`Session::work_out_keepers`]).
    keepers: Option<Keepers>,
}

/// What keeps each item, and the shortest chain to each from where the
/// program starts, as `retained` and `why` find them.
pub struct Keepers {
    /// Which item keeps which: the dominator tree that `retained` builds,
    /// without a range.
    pub tree: DominatorTree,
    /// The shortest chain from where the program starts to each item.
    pub paths: ShortestPaths,
}

impl<'a> Session<'a> {
    /// Builds what the pages need for `binary`, loaded from `path`, or says
    /// why elfq can't read its code, as every command that reads code does.
    pub fn new(path: &Path, binary: &'a Binary) -> Result<Session<'a>> {
        let graph = reference_graph(binary)?;
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
            row_names: RefCell::new(HashMap::new()),
            short_names: RefCell::new(HashMap::new()),
            page_codes: RefCell::new(HashMap::new()),
            keepers: None,
        })
    }

    /// What keeps each item, once worked out.
    pub(super) fn keepers(&self) -> Option<&Keepers> {
        self.keepers.as_ref()
    }

    /// Whether the session has yet to work out what keeps each item, which
    /// it needn't in a shared library, where `retained` and `why` answer
    /// about no item.
    pub(super) fn needs_keepers(&self) -> bool {
        self.keepers.is_none() && self.binary.kind != FileKind::SharedLibrary
    }

    /// The tree of retained sizes that `retained SYMBOL` lists, without a
    /// range, from what keeps each item, once the session has worked it
    /// out, and with the session's names.
    pub(super) fn retained_tree(&self) -> Option<RetainedTree<'_, 'a>> {
        let keepers = self.keepers()?;
        let (binary, graph, names) = (self.binary, &self.graph, &self.names);
        Some(RetainedTree::borrowing(binary, graph, &keepers.tree, names))
    }

    /// Works out what keeps each item, and the shortest chain to each from
    /// where the program starts, once. In a large program, this takes a
    /// while, which the status line says.
    pub(super) fn work_out_keepers(&mut self) {
        if self.needs_keepers() {
            self.keepers = Some(Keepers {
                tree: DominatorTree::build(self.binary, &self.graph, None),
                paths: ShortestPaths::from_roots(self.binary, &self.graph),
            });
        }
    }

    /// What uses `node`, as `callers SYMBOL` lists it: its rows, with every
    /// caller, and the notes under them.
    pub(super) fn callers(&self, node: Node) -> Answer {
        callers::answer(self.binary, &self.graph, node)
    }

    /// `node`'s name as the first line of a command's answer about it
    /// gives it, when the command is given the shortest name that finds it,
    /// as the command under a tab is: "sniprintf (also named snprintf)".
    pub(super) fn lead_name(&self, node: Node) -> String {
        match self.names.find(&self.names.query_for(node)) {
            Ok(symbol) => symbol.lead_name(self.binary),
            Err(_) => node.name(self.binary).to_string(),
        }
    }

    /// The command that prints what a page shows about `node`, or without
    /// one, about the file, ready to paste into a shell, as in `$ elfq
    /// testdata/bin/cpp-x86_64.elf callers 'log(unsigned int)'`: the file
    /// as the user typed it, and the shortest name that finds `node` as
    /// SYMBOL, both quoted for the shell, then `--all` when `all` asks for
    /// it.
    pub(super) fn command(&self, command: &str, node: Option<Node>, all: bool) -> String {
        let symbol = match node {
            Some(node) => format!(" {}", text::shell_quote(&self.names.query_for(node))),
            None => String::new(),
        };
        let all = if all { " --all" } else { "" };
        format!(
            "$ elfq {} {command}{symbol}{all}",
            text::shell_quote(&self.path.display().to_string()),
        )
    }

    /// The node whose page stands for `node`: for an import, its PLT stub
    /// when it has one, since the import's name finds the stub as SYMBOL,
    /// in every command. So code that uses the import through the global
    /// offset table, as code built with `-fno-plt` calls it, leads to the
    /// same page as code that calls the stub. An import without a stub
    /// has a page of its own.
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

    /// The name that rows give item `index`: the shortest that finds only
    /// it as SYMBOL, as in "crc_table, kept by crc32".
    pub(super) fn row_name(&self, index: usize) -> String {
        if let Some(name) = self.row_names.borrow().get(&index) {
            return name.clone();
        }
        let name = self.names.row_name(index);
        self.row_names.borrow_mut().insert(index, name.clone());
        name
    }

    /// What the rows of `retained` and `why` call `node` after "kept by"
    /// or "used by", to name the row they hang under, as in "crc_table,
    /// kept by crc32": the shortest form of its name that finds only it,
    /// or failing that, its whole name, where [`Session::row_name`] gives
    /// an address.
    pub(super) fn short_name(&self, node: Node) -> String {
        if let Some(name) = self.short_names.borrow().get(&node) {
            return name.clone();
        }
        let name = self.names.short_name(node);
        self.short_names.borrow_mut().insert(node, name.clone());
        name
    }

    /// Item `index`'s code, as its page shows it.
    pub(super) fn page_code(&self, index: usize) -> Rc<PageCode> {
        if let Some(code) = self.page_codes.borrow().get(&index).and_then(Weak::upgrade) {
            return code;
        }
        let code = Rc::new(PageCode::new(self, index));
        self.page_codes
            .borrow_mut()
            .insert(index, Rc::downgrade(&code));
        code
    }

    /// The file's name, without its folders, for the title line.
    pub(super) fn file_name(&self) -> String {
        match self.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.path.display().to_string(),
        }
    }
}
