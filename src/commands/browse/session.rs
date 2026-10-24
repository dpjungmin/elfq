//! The file being browsed, and what the pages ask about it.
//!
//! The file is loaded, and its reference graph built, once, before the
//! screen switches, on elfq's own thread with its large stack, as for
//! every command. Pages then ask the commands' own code for their answers,
//! such as `callers::answer` for "used by N", so that a page says what the
//! commands print.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::binary::{Binary, ItemKind, MemoryBlock, memory_blocks};
use crate::commands::symbol::Names;
use crate::commands::{callers, distinct_nodes, reference_graph};
use crate::graph::{Graph, Node, VeneerTarget, Veneers};

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
        })
    }

    /// How many items use `node`: the callers that `callers SYMBOL` lists.
    pub(super) fn used_by(&self, node: Node) -> usize {
        let answer = callers::answer(self.binary, &self.graph, node);
        distinct_nodes(&answer.rows).len()
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
        self.names.row_name(index)
    }

    /// The file's name, without its folders, for the title line.
    pub(super) fn file_name(&self) -> String {
        match self.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.path.display().to_string(),
        }
    }
}
