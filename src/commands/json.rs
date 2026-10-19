//! The shapes of the JSON that commands write for `--json`.
//!
//! A command builds one value from these and its own shapes, and writes it
//! with [`write()`], one row of the answer per line. Every key is always
//! there, `null` when it has no value, so that scripts can count on the
//! shape. Addresses are hex strings, as elfq prints them and SYMBOL takes
//! them; sizes are numbers.

use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;
use serde_json::ser::Formatter;

use crate::binary::{AddressRange, Binary, MemoryBlock};
use crate::graph::{Node, ReferenceKind, RootKind};

/// Writes `value`, an answer's object, as JSON with one row per line: each
/// element of a list that the object holds, such as an item of `top` or a
/// chain of `why`, goes on a line of its own, so that `grep` and `head` work
/// on the rows as they do on the text's. The rest has no spaces or line
/// breaks.
pub(super) fn write(value: &impl Serialize, out: &mut dyn Write) -> Result<()> {
    let mut serializer = serde_json::Serializer::with_formatter(&mut *out, RowPerLine::default());
    value.serialize(&mut serializer)?;
    writeln!(out)?;
    Ok(())
}

/// How many objects and lists hold a row: the answer's object, and the list
/// of rows in it.
const ROWS: usize = 2;

/// Lays out JSON for [`write()`].
#[derive(Default)]
struct RowPerLine {
    /// How many objects and lists hold the next value.
    depth: usize,
    /// Whether the list of rows being written has a row yet.
    has_rows: bool,
}

impl Formatter for RowPerLine {
    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        writer.write_all(b"{")
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        writer.write_all(b"}")
    }

    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        if self.depth == ROWS {
            self.has_rows = false;
        }
        writer.write_all(b"[")
    }

    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if !first {
            writer.write_all(b",")?;
        }
        if self.depth == ROWS {
            self.has_rows = true;
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.depth == ROWS && self.has_rows {
            writer.write_all(b"\n")?;
        }
        self.depth -= 1;
        writer.write_all(b"]")
    }
}

/// An item or an import.
#[derive(Serialize)]
pub(super) struct Item<'a> {
    pub name: &'a str,
    /// `function`, `data` or `import`.
    pub kind: String,
    /// The section that holds it, such as `.text` or `.bss`, or `null` for
    /// an import.
    pub section: Option<&'a str>,
    /// Where it starts, or `null` for an import, whose code is in another
    /// file.
    pub address: Option<String>,
    /// Its size in bytes, or `null` when elfq doesn't know it.
    pub size: Option<u64>,
    /// Whether elfq estimated the size, as it does when the file doesn't
    /// record it: for data that elfq finds only because something refers to
    /// it, and for functions whose size no symbol, unwind table or PLT
    /// gives, such as those elfq finds by following the code in stripped
    /// firmware.
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
                    section: Some(&binary.sections[item.section].name),
                    address: Some(address(item.address)),
                    size: (item.size > 0).then_some(item.size),
                    size_estimated: item.size_estimated,
                }
            }
            Node::Import(_) => Item {
                name: node.name(binary),
                kind: "import".to_string(),
                section: None,
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

/// A block of addresses that sections and load images occupy, such as one
/// memory.
#[derive(Serialize)]
pub(super) struct Block<'a> {
    #[serde(flatten)]
    pub range: Range,
    pub size: u64,
    /// The sections in it, in address order.
    pub sections: Vec<&'a str>,
    /// The sections whose load images are in it, in the order of those
    /// addresses: the bytes that the file stores there for the program to
    /// copy to the section before it uses them, as firmware's startup code
    /// copies `.data` from flash to RAM.
    pub load_images: Vec<&'a str>,
}

impl<'a> Block<'a> {
    pub fn new(binary: &'a Binary, block: &MemoryBlock) -> Block<'a> {
        Block {
            range: Range::new(block.range),
            size: block.range.end - block.range.start,
            sections: section_names(binary, &block.sections),
            load_images: section_names(binary, &block.load_images),
        }
    }
}

/// The names of the sections at `indexes` in [`Binary::sections`].
pub(super) fn section_names<'a>(binary: &'a Binary, indexes: &[usize]) -> Vec<&'a str> {
    indexes
        .iter()
        .map(|&index| binary.sections[index].name.as_str())
        .collect()
}

pub(super) fn address(address: u64) -> String {
    format!("{address:#x}")
}

pub(super) fn addresses(addresses: &[u64]) -> Vec<String> {
    addresses.iter().map(|&a| address(a)).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// An answer with each kind of value that the layout tells apart.
    #[derive(Serialize)]
    struct Answer {
        name: &'static str,
        rows: Vec<Value>,
        empty: Vec<Value>,
        more: Vec<Value>,
        last: Value,
    }

    #[test]
    fn each_row_of_a_list_in_the_answer_is_a_line() {
        // Each row of a list in the answer is a line. Lists further in, in a
        // row or in another object, stay on their line, brackets in a
        // string are only text, and an empty list takes no line, even after
        // one that has rows.
        let answer = Answer {
            name: "a [b],\n{c}",
            rows: vec![
                serde_json::json!({"sites": ["0x1", "0x2"]}),
                serde_json::json!([1, [2]]),
            ],
            empty: Vec::new(),
            more: vec![serde_json::json!(3)],
            last: serde_json::json!({"list": [4]}),
        };
        let mut out = Vec::new();
        write(&answer, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"{"name":"a [b],\n{c}","rows":[
{"sites":["0x1","0x2"]},
[1,[2]]
],"empty":[],"more":[
3
],"last":{"list":[4]}}
"#
        );
    }
}
