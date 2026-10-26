//! Tab 1 of a data object's page: what elfq reads in its bytes. It lists
//! only rows that carry elfq's links, and strings, and no hex dump, which
//! objdump prints:
//!
//! - A vector table lists every entry, named by its exception as Arm's
//!   manuals name it for the file's architecture, with the word it holds,
//!   and for a handler, a note that links to it: `pointer Reset_Handler`.
//!   So does each part of a table that is split into several data
//!   objects, as cortex-m-rt splits it.
//! - Other data lists each word that holds a pointer to an item, the
//!   pointers that `callees SYMBOL` lists, with the note and link that a
//!   pointer among a function's code has. Where the dynamic loader writes
//!   the pointer, as in a shared library, the value is what the loader
//!   writes: an address, or for an import, which has none in this file,
//!   "imported". When the file holds something else there, such as 0, a
//!   footnote says so, since the objdump command under the rows prints it.
//! - An object that holds strings and nothing else lists each one, as an
//!   assembler writes it: `.asciz "tick\n"`.
//!
//! Under them is the objdump command that prints the object's bytes, ready
//! to paste, unless its section takes no room in the file, as `.bss`
//! doesn't.

use super::{CodeLine, Notes, PageCode, strings, widest};
use crate::binary::{Binary, ItemKind, RelocationTarget, exception_name};
use crate::commands::browse::session::Session;
use crate::commands::text;
use crate::graph::Reference;

/// Data object `index`'s contents, as tab 1 of its page shows them.
pub(super) fn page_code(session: &Session, index: usize) -> PageCode {
    let binary = session.binary;
    let item = &binary.items[index];
    let section = &binary.sections[item.section];
    let name = session.row_name(index);
    let mut code = PageCode {
        kind: ItemKind::Data,
        lines: Vec::new(),
        column: Some("VALUE".to_string()),
        widest: (0, 0),
        empty: None,
        footnote: None,
        command: None,
    };
    // Every row is read from the object's bytes, so it has none.
    if section.file_range.is_none() {
        code.empty = Some(format!(
            "{name} has no bytes in the file: {} takes no room in it.",
            section.name
        ));
        return code;
    }
    let notes = Notes::new(session, index);
    let table = (binary.vector_table.as_ref())
        .filter(|table| (table.address..table.address + table.size).contains(&item.address));
    if let Some(table) = table {
        let (column, lines) = entries(&notes, table.address);
        code.column = Some(column);
        code.lines = lines;
    } else {
        code.lines = pointers(&notes);
    }
    if code.lines.is_empty() {
        code.lines = (strings(session, index).into_iter())
            .map(|(address, text)| plain_line(address, format!(".asciz {text}")))
            .collect();
    }
    if code.lines.is_empty() {
        code.empty = Some(format!(
            "{name} holds no pointers that elfq follows, and isn't made of strings that end in \
             a 0 byte; the objdump command below prints its bytes."
        ));
    }
    if (code.lines.iter()).any(|line| file_holds_other(binary, line.address)) {
        code.footnote = Some(
            "Where the dynamic loader writes a pointer, VALUE is what it writes; objdump prints \
             what the file holds there, such as 0."
                .to_string(),
        );
    }
    code.command = Some(format!(
        "$ objdump -s -j {} --start-address={:#x} --stop-address={:#x} {}",
        text::shell_quote(&section.name),
        item.address,
        item.address + item.size,
        text::shell_quote(&session.path.display().to_string()),
    ));
    code.widest = widest(&code.lines);
    code
}

/// The entries of the vector table that starts at `table` which the data
/// object holds, each named by its exception, counted from the table's
/// start, with the header of their column: "ENTRY", lined up over the
/// names, then "VALUE".
fn entries(notes: &Notes, table: u64) -> (String, Vec<CodeLine>) {
    let binary = notes.session.binary;
    let item = &binary.items[notes.index];
    let words: Vec<(u64, u64)> = binary
        .words(item.address, item.address + item.size)
        .collect();
    let names: Vec<String> = (words.iter())
        .map(|&(address, _)| {
            let number = (address - table) / binary.pointer_size;
            exception_name(number, binary.arm_architecture)
        })
        .collect();
    let width = (names.iter().map(|name| name.chars().count()))
        .chain(["ENTRY".len()])
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (&(address, _), name) in words.iter().zip(&names) {
        let value = format!("{name:<width$}  {}", value(binary, address));
        match notes.references.get(&address) {
            Some(references) => {
                let pointer = |reference: &&Reference| pointer(notes, reference, value.clone());
                lines.extend(references.iter().map(pointer));
            }
            None => lines.push(plain_line(address, value)),
        }
    }
    (format!("{:<width$}  VALUE", "ENTRY"), lines)
}

/// The words in the data object that hold a pointer to an item, in order.
fn pointers(notes: &Notes) -> Vec<CodeLine> {
    let binary = notes.session.binary;
    (notes.session.graph.references_from(notes.index).iter())
        .map(|reference| pointer(notes, reference, value(binary, reference.site)))
        .collect()
}

/// The line of `reference`, a pointer stored in the data object, which
/// shows `value`: its note names what it points to, which it links to, as
/// the note of a pointer among a function's code does. A pointer can
/// point inside an item, as to an element of an array: `pointer
/// frames+0x8`.
fn pointer(notes: &Notes, reference: &Reference, value: String) -> CodeLine {
    let binary = notes.session.binary;
    let (name, via, link) = notes.target(reference);
    let how = text::how_via(binary, reference.to, reference.kind, via);
    CodeLine {
        address: reference.site,
        instruction: value,
        note: format!("{how} {name}"),
        elsewhere: notes.elsewhere(link),
        link: Some(link),
    }
}

/// A line that shows `value` at `address`, and links nowhere.
fn plain_line(address: u64, value: String) -> CodeLine {
    CodeLine {
        address,
        instruction: value,
        note: String::new(),
        elsewhere: None,
        link: None,
    }
}

/// What the program finds in the pointer-sized word at `address`: what
/// the dynamic loader writes there, if it writes something, or else what
/// the file holds.
pub(super) fn stored_at(binary: &Binary, address: u64) -> Option<RelocationTarget> {
    match binary.relocation_at(address) {
        Some(relocation) => Some(relocation.target),
        None => binary.pointer_at(address).map(RelocationTarget::Address),
    }
}

/// The pointer-sized word at `address`, in hex with all its digits, as the
/// program finds it (see `stored_at`). A pointer to an import has no
/// address in this file, so it is "imported".
fn value(binary: &Binary, address: u64) -> String {
    let digits = 2 + 2 * binary.pointer_size as usize;
    match stored_at(binary, address) {
        Some(RelocationTarget::Address(word)) => format!("{word:#0digits$x}"),
        Some(RelocationTarget::Import(_)) => "imported".to_string(),
        None => String::new(),
    }
}

/// Whether the file holds another word at `address` than the one the
/// dynamic loader writes there, such as 0, which is what objdump prints.
fn file_holds_other(binary: &Binary, address: u64) -> bool {
    match binary
        .relocation_at(address)
        .map(|relocation| relocation.target)
    {
        Some(RelocationTarget::Address(target)) => binary.pointer_at(address) != Some(target),
        Some(RelocationTarget::Import(_)) => true,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{samples, shell_words};
    use super::super::{Link, quoted};
    use super::*;
    use crate::cli::Format;
    use crate::commands::{callees, json};

    #[test]
    fn the_pointers_are_those_that_callees_lists() {
        // Data objects with pointers, vector tables, strings, tables of
        // strings, and with no bytes in the file.
        let (mut pointers, mut tables, mut strings, mut string_tables, mut nothing) =
            (0, 0, 0, 0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let item_json = |node| serde_json::to_value(json::Item::new(&binary, node)).unwrap();
            for index in 0..binary.items.len() {
                let item = &binary.items[index];
                if item.kind != ItemKind::Data {
                    continue;
                }
                let shown = format!("{}: {}", path.display(), item.name);
                let contents = page_code(&session, index);
                // Each row that links names a pointer and what it points
                // to, as `callees SYMBOL --json` names its sites and
                // callees, run on the graph that the session has built.
                let mut rows: Vec<(String, serde_json::Value)> = Vec::new();
                for line in &contents.lines {
                    if let Some(Link::Node { node, .. }) = line.link {
                        assert!(line.note.starts_with("pointer "), "{shown}: {}", line.note);
                        rows.push((json::address(line.address), item_json(node)));
                    }
                }
                let mut out = Vec::new();
                let graph = &session.graph;
                callees::callees(&binary, graph, index, "", false, Format::Json, &mut out).unwrap();
                let answer: serde_json::Value = serde_json::from_slice(&out).unwrap();
                let mut listed = Vec::new();
                for callee in answer["callees"].as_array().unwrap() {
                    assert_eq!(callee["how"], "pointer", "{shown}");
                    for site in callee["sites"].as_array().unwrap() {
                        let site = site.as_str().unwrap().to_string();
                        listed.push((site, callee["item"].clone()));
                    }
                }
                rows.sort_by_key(|(site, item)| (site.clone(), item.to_string()));
                listed.sort_by_key(|(site, item)| (site.clone(), item.to_string()));
                assert_eq!(rows, listed, "{shown}");
                pointers += usize::from(!rows.is_empty());
                // The command under them prints the object's bytes, from
                // the file as typed, and only where the file has them.
                let section = &binary.sections[item.section];
                match &contents.command {
                    Some(command) => {
                        let words = shell_words(command.strip_prefix("$ ").unwrap());
                        let range = [item.address, item.address + item.size];
                        let [start, stop] = range.map(|address| format!("{address:#x}"));
                        let path = path.display().to_string();
                        let wanted = [
                            "objdump",
                            "-s",
                            "-j",
                            &section.name,
                            &format!("--start-address={start}"),
                            &format!("--stop-address={stop}"),
                            &path,
                        ];
                        assert_eq!(words, wanted, "{shown}");
                    }
                    None => {
                        assert!(section.file_range.is_none(), "{shown}");
                        nothing += 1;
                    }
                }
                // Rows or a sentence, never a blank tab.
                assert!(
                    contents.lines.is_empty() != contents.empty.is_none(),
                    "{shown}"
                );
                tables += usize::from(contents.column.as_deref() != Some("VALUE"));
                // Strings, each in the object, after the one before, and
                // each the text from its address to the next 0 byte, which
                // comes after another 0 byte or the object's start.
                let string = |line: &&CodeLine| line.instruction.starts_with(".asciz \"");
                let at: Vec<u64> = (contents.lines.iter().filter(string))
                    .map(|line| line.address)
                    .collect();
                assert!(at.is_sorted_by(|a, b| a < b), "{shown}");
                assert!(at.iter().all(|&address| item.contains(address)), "{shown}");
                let bytes = binary.bytes(item.address, item.size);
                for line in contents.lines.iter().filter(string) {
                    let start = (line.address - item.address) as usize;
                    let text = &bytes.unwrap()[start..];
                    let text = &text[..text.iter().position(|&byte| byte == 0).unwrap()];
                    let before = start.checked_sub(1).map(|before| bytes.unwrap()[before]);
                    assert!(
                        matches!(before, None | Some(0)),
                        "{shown}: {}",
                        line.instruction
                    );
                    assert_eq!(
                        line.instruction,
                        format!(".asciz {}", quoted(text)),
                        "{shown}"
                    );
                }
                strings += usize::from(!at.is_empty());
                string_tables += usize::from(at.len() > 1);
            }
        }
        assert!(
            pointers > 500 && tables > 25 && strings > 300 && string_tables > 2 && nothing > 200,
            "{pointers} {tables} {strings} {string_tables} {nothing}"
        );
    }
}
