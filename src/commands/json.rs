//! The shapes of the JSON that commands write for `--json`.
//!
//! A command builds one value from these and its own shapes, and writes it
//! with [`write()`], one row of the answer per line. A list whose rows
//! would take much memory held all at once, such as the chains of `why`,
//! is a [`Rows`], whose rows are made as they are written. Every key is
//! always there, `null` when it has no value, so that scripts can count on
//! the shape. Addresses are hex strings, as elfq prints them and SYMBOL
//! takes them; sizes are numbers.

use std::cell::RefCell;
use std::io::{self, Write};

use anyhow::Result;
use serde::{Serialize, Serializer};
use serde_json::ser::Formatter;

use super::symbol::{self, MatchedBy, Pattern};
use super::{text, uses};
use crate::binary::{AddressRange, Binary, MemoryBlock};
use crate::graph::{self, Node, ReferenceKind, RootKind};

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

/// A list of an answer's rows that are made one at a time, as [`write()`]
/// writes them, so that the command never holds them all: for a list whose
/// rows would take much memory held all at once, as the chains of `why`
/// would, one for each item that keeps SYMBOL, each with every item from
/// where the program starts to SYMBOL. A list whose rows take little next
/// to the rest of the command stays a `Vec`, which reads more plainly, even
/// when it grows with the program, as `top`'s items do. A `Rows` is written
/// as a list of the rows that the iterator it is made from gives. A row is
/// made after the lines before it are written, so making one mustn't fail:
/// every error comes before the answer's first line. Writing the list uses
/// its rows up, so it can be written only once.
pub(super) struct Rows<'r, T> {
    /// The rows still to make. `Serialize` borrows what it writes only to
    /// read it, so the cell lets writing the list take the rows.
    rows: RefCell<Box<dyn Iterator<Item = T> + 'r>>,
}

impl<'r, T> Rows<'r, T> {
    pub(super) fn new(rows: impl Iterator<Item = T> + 'r) -> Rows<'r, T> {
        Rows {
            rows: RefCell::new(Box::new(rows)),
        }
    }
}

impl<T: Serialize> Serialize for Rows<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(&mut *self.rows.borrow_mut())
    }
}

/// How many objects and lists hold a row: the answer's object, and the list
/// of rows in it.
const ROW_DEPTH: usize = 2;

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
        if self.depth == ROW_DEPTH {
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
        if self.depth == ROW_DEPTH {
            self.has_rows = true;
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.depth == ROW_DEPTH && self.has_rows {
            writer.write_all(b"\n")?;
        }
        self.depth -= 1;
        writer.write_all(b"]")
    }
}

/// An item or an import.
#[derive(Serialize)]
pub(super) struct Item<'a> {
    name: &'a str,
    /// `function`, `data` or `import`.
    kind: String,
    /// The section that holds it, such as `.text` or `.bss`, or `null` for
    /// an import.
    section: Option<&'a str>,
    /// Where it starts, or `null` for an import, whose code is in another
    /// file.
    address: Option<String>,
    /// Its size in bytes, or `null` when elfq doesn't know it.
    size: Option<u64>,
    /// Whether elfq estimated the size, as it does when the file doesn't
    /// record it: for data that elfq finds only because something refers to
    /// it, and for functions whose size no symbol, unwind table or PLT
    /// gives, such as those elfq finds by following the code in stripped
    /// firmware.
    size_estimated: bool,
}

impl<'a> Item<'a> {
    pub(super) fn new(binary: &'a Binary, node: Node) -> Item<'a> {
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

/// SYMBOL's item or import, the subject of an answer, with its other
/// names.
#[derive(Serialize)]
pub(super) struct Symbol<'a> {
    #[serde(flatten)]
    item: Item<'a>,
    /// The item's other names: the symbol that `name` is demangled from,
    /// and other symbols at its address, such as weak aliases, each mangled
    /// one also demangled. Empty for an import.
    aliases: Vec<&'a str>,
}

impl<'a> Symbol<'a> {
    pub(super) fn new(binary: &'a Binary, node: Node) -> Symbol<'a> {
        let aliases = match node {
            Node::Item(index) => binary.items[index]
                .aliases
                .iter()
                .map(String::as_str)
                .collect(),
            Node::Import(_) => Vec::new(),
        };
        Symbol {
            item: Item::new(binary, node),
            aliases,
        }
    }
}

/// An item that a pattern matches, and how, as `retained --json` lists it
/// under `matches`, and `debug matches` lists it.
#[derive(Serialize)]
pub(super) struct Match<'a> {
    /// The pattern as typed.
    pattern: &'a str,
    /// The item, with the keys of every item row.
    #[serde(flatten)]
    item: Item<'a>,
    /// `name`, `end` or `alias`: how the pattern matched (see
    /// [`MatchedBy`]).
    matched_by: MatchedBy,
    /// The text of the item's names that the pattern matched (see
    /// [`symbol::Match`]).
    matched_as: &'a str,
}

impl<'a> Match<'a> {
    pub(super) fn new(
        binary: &'a Binary,
        pattern: &'a Pattern,
        found: &'a symbol::Match,
    ) -> Match<'a> {
        Match {
            pattern: pattern.typed(),
            item: Item::new(binary, Node::Item(found.item)),
            matched_by: found.matched_by,
            matched_as: &found.matched_as,
        }
    }
}

/// The items at `indexes` in [`Binary::items`].
pub(super) fn items<'a>(binary: &'a Binary, indexes: &[usize]) -> Vec<Item<'a>> {
    indexes
        .iter()
        .map(|&index| Item::new(binary, Node::Item(index)))
        .collect()
}

/// A call or jump that elfq can't follow.
#[derive(Serialize)]
pub(super) struct Unresolved {
    /// The instruction's address.
    site: String,
    /// `call` or `jump`.
    how: &'static str,
    /// Where it goes, when the code says so but no item is there, or
    /// `null` when only the running program knows.
    target: Option<String>,
}

impl Unresolved {
    pub(super) fn new(unresolved: &graph::Unresolved) -> Unresolved {
        Unresolved {
            site: address(unresolved.site),
            how: text::call_or_jump(unresolved),
            target: unresolved.target.map(address),
        }
    }
}

/// How one item uses another: one row of `callers` or `callees`.
#[derive(Serialize)]
pub(super) struct Use<'a> {
    /// The item on the other end: the caller, or the callee.
    item: Item<'a>,
    /// The [kind of reference](how_field).
    how: &'static str,
    /// The addresses of the instructions or pointers.
    sites: Vec<String>,
}

pub(super) fn uses<'a>(binary: &'a Binary, rows: &[uses::Row]) -> Vec<Use<'a>> {
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
    start: String,
    end: String,
}

impl Range {
    pub(super) fn new(range: AddressRange) -> Range {
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
    range: Range,
    size: u64,
    /// The sections in it, in address order.
    sections: Vec<&'a str>,
    /// The sections whose load images are in it, in the order of those
    /// addresses: the bytes that the file stores there for the program to
    /// copy to the section before it uses them, as firmware's startup code
    /// copies `.data` from flash to RAM.
    load_images: Vec<&'a str>,
}

impl<'a> Block<'a> {
    pub(super) fn new(binary: &'a Binary, block: &MemoryBlock) -> Block<'a> {
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

    /// The answer above, with its lists of rows made as they are written.
    #[derive(Serialize)]
    struct MadeAsWritten {
        name: &'static str,
        rows: Rows<'static, Value>,
        empty: Rows<'static, Value>,
        more: Rows<'static, Value>,
        last: Value,
    }

    /// What [`write()`] writes for `value`.
    fn written(value: &impl Serialize) -> String {
        let mut out = Vec::new();
        write(value, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn rows_made_as_they_are_written_are_written_as_held_ones() {
        // The same bytes, whether the answer holds its rows or makes them as
        // they are written: none, one or several rows, rows that are lists,
        // as the chains of `why` are, or that hold lists and brackets in a
        // string, lists of rows one after another, and rows whose number
        // isn't known until they are made, as a filter gives them.
        let rows = [
            Vec::new(),
            vec![serde_json::json!({"sites": ["0x1"]})],
            vec![
                serde_json::json!([{"item": "a [b],\n{c}", "how": ""}, {"item": "d"}]),
                serde_json::json!([]),
                serde_json::json!(3),
            ],
        ];
        for rows in rows {
            for more in [Vec::new(), rows.clone()] {
                let held = Answer {
                    name: "n",
                    rows: rows.clone(),
                    empty: Vec::new(),
                    more: more.clone(),
                    last: serde_json::json!({"list": [4]}),
                };
                let made = MadeAsWritten {
                    name: "n",
                    rows: Rows::new(rows.clone().into_iter()),
                    empty: Rows::new(Vec::new().into_iter()),
                    more: Rows::new(more.into_iter().filter(|_| true)),
                    last: serde_json::json!({"list": [4]}),
                };
                assert_eq!(written(&made), written(&held), "{rows:?}");
            }
        }
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
        assert_eq!(
            written(&answer),
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
