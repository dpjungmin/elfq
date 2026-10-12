//! The shapes of the JSON that commands write for `--format json`.
//!
//! A command builds one value from these and its own shapes, and writes it
//! with [`write`]. Every key is always there, `null` when it has no value,
//! so that scripts can count on the shape. Addresses are hex strings, as
//! elfq prints them and SYMBOL takes them; sizes are numbers.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::binary::Binary;
use crate::graph::Node;

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
    /// Whether elfq estimated the size, as it does for data without a
    /// symbol.
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
    /// As in the HOW column: `call`, `tail call`, `address taken`, `access`
    /// or `pointer`.
    pub how: &'static str,
    /// The addresses of the instructions or pointers.
    pub sites: Vec<String>,
}

pub(super) fn uses<'a>(binary: &'a Binary, rows: &[super::Row]) -> Vec<Use<'a>> {
    rows.iter()
        .map(|row| Use {
            item: Item::new(binary, row.node),
            how: row.how,
            sites: row.sites.iter().map(|&site| address(site)).collect(),
        })
        .collect()
}

pub(super) fn address(address: u64) -> String {
    format!("{address:#x}")
}
