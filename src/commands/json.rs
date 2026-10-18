//! The shapes of the JSON that commands write for `--json`.
//!
//! A command builds one value from these and its own shapes, and writes it
//! with [`write()`]. Every key is always there, `null` when it has no value,
//! so that scripts can count on the shape. Addresses are hex strings, as
//! elfq prints them and SYMBOL takes them; sizes are numbers.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::binary::{AddressRange, Binary, MemoryBlock};
use crate::graph::{Node, ReferenceKind, RootKind};

/// Writes `value` as indented JSON.
pub(super) fn write(value: &impl Serialize, out: &mut dyn Write) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

/// An item or an import.
#[derive(Serialize)]
pub(super) struct Item<'a> {
    pub name: &'a str,
    /// `function`, `data` or `import`.
    pub kind: String,
    /// Where it starts, or `null` for an import, whose code is in another
    /// file.
    pub address: Option<String>,
    /// Its size in bytes, or `null` when elfq doesn't know it.
    pub size: Option<u64>,
    /// Whether elfq estimated the size, as it does when the file doesn't
    /// record it: for data without a symbol, and for functions whose symbol
    /// has no size or that elfq found without one.
    pub size_estimated: bool,
}

impl<'a> Item<'a> {
    pub fn new(binary: &'a Binary, node: Node) -> Item<'a> {
        match node {
            Node::Item(index) => {
                let item = &binary.items[index];
                Item {
                    name: &item.name,
                    kind: item.kind.to_string(),
                    address: Some(address(item.address)),
                    size: (item.size > 0).then_some(item.size),
                    size_estimated: item.size_estimated,
                }
            }
            Node::Import(_) => Item {
                name: node.name(binary),
                kind: "import".to_string(),
                address: None,
                size: None,
                size_estimated: false,
            },
        }
    }
}

/// How one item uses another: one row of `callers` or `callees`.
#[derive(Serialize)]
pub(super) struct Use<'a> {
    /// The item on the other end: the caller, or the callee.
    pub item: Item<'a>,
    /// The [kind of reference](how_field).
    pub how: &'static str,
    /// The addresses of the instructions or pointers.
    pub sites: Vec<String>,
}

pub(super) fn uses<'a>(binary: &'a Binary, rows: &[super::Row]) -> Vec<Use<'a>> {
    rows.iter()
        .map(|row| Use {
            item: Item::new(binary, row.node),
            how: row.how,
            sites: addresses(&row.sites),
        })
        .collect()
}

/// A kind of reference in words, for the `how` fields of JSON: `call`,
/// `tail call`, `address taken`, `access` or `pointer`. Unlike the HOW
/// column, which says "uses" for both, JSON tells reading or writing data
/// from loading its address.
pub(super) fn how_field(kind: ReferenceKind) -> &'static str {
    match kind {
        ReferenceKind::Call => "call",
        ReferenceKind::TailCall => "tail call",
        ReferenceKind::Address => "address taken",
        ReferenceKind::Access => "access",
        ReferenceKind::Pointer => "pointer",
    }
}

/// Why a root is one, for the `root` field of `why`'s JSON and the first
/// step of each chain, in the words the HOW column used before it was
/// shortened, since scripts may count on them.
pub(super) fn root_field(binary: &Binary, kind: RootKind) -> String {
    match kind {
        RootKind::Entry => "entry point".to_string(),
        RootKind::InSection(section) => format!("in {}", binary.sections[section].name),
        RootKind::ListedIn(section) => format!("listed in {}", binary.sections[section].name),
        RootKind::PltHeader => "used by the dynamic loader".to_string(),
        RootKind::Personality => "used by the unwinder".to_string(),
        RootKind::VectorTable => "vector table".to_string(),
        RootKind::Handler => "listed in the vector table".to_string(),
    }
}

/// The addresses from `start` up to, but not including, `end`.
#[derive(Serialize)]
pub(super) struct Range {
    pub start: String,
    pub end: String,
}

impl Range {
    pub fn new(range: AddressRange) -> Range {
        Range {
            start: address(range.start),
            end: address(range.end),
        }
    }
}

/// A block of addresses that sections occupy, such as one memory.
#[derive(Serialize)]
pub(super) struct Block<'a> {
    #[serde(flatten)]
    pub range: Range,
    pub size: u64,
    /// The sections in it, in address order.
    pub sections: Vec<&'a str>,
}

impl<'a> Block<'a> {
    pub fn new(binary: &'a Binary, block: &MemoryBlock) -> Block<'a> {
        Block {
            range: Range::new(block.range),
            size: block.range.end - block.range.start,
            sections: block
                .sections
                .iter()
                .map(|&index| binary.sections[index].name.as_str())
                .collect(),
        }
    }
}

pub(super) fn address(address: u64) -> String {
    format!("{address:#x}")
}

pub(super) fn addresses(addresses: &[u64]) -> Vec<String> {
    addresses.iter().map(|&a| address(a)).collect()
}
