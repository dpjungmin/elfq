use object::{Architecture, SectionKind};

use super::*;
use crate::binary::{Relocation, Section, Source};
use crate::commands::diff::pairing::PairedBy;
use crate::graph::{Graph, Reference};

/// The sections of a build: code, constants and variables, each stored
/// in the file at its address.
const SECTIONS: [(&str, u64, u64); 3] = [
    (".text", 0x100, 0x100),
    (".rodata", 0x400, 0x100),
    (".data", 0x800, 0x100),
];

/// A Cortex-M build with `sections` and `items`, each a name, an
/// address and a size, in address order: a function in `.text`, and
/// data elsewhere. Its bytes are 0 but for `bytes`, each at an address,
/// and its graph has `calls`, from and to items by name, where a
/// veneer's call is its jump.
fn build(
    sections: &[(&str, u64, u64)],
    items: &[(&str, u64, u64)],
    bytes: &[(u64, &[u8])],
    calls: &[(&str, &str)],
) -> Build {
    let sections: Vec<Section> = sections
        .iter()
        .map(|&(name, address, size)| Section {
            name: name.to_string(),
            address,
            size,
            kind: SectionKind::Data,
            allocated: true,
            file_range: Some((address, size)),
            entry_size: 0,
            load_address: None,
        })
        .collect();
    let items: Vec<Item> = items
        .iter()
        .map(|&(name, address, size)| {
            let section = sections.iter().position(|s| s.contains(address)).unwrap();
            Item {
                name: name.to_string(),
                aliases: Vec::new(),
                kind: [ItemKind::Function, ItemKind::Data][usize::from(section > 0)],
                address,
                size,
                size_estimated: false,
                section,
                source: Source::SymbolTable,
            }
        })
        .collect();
    let mut data = vec![0; 0x900];
    for &(address, bytes) in bytes {
        data[address as usize..][..bytes.len()].copy_from_slice(bytes);
    }
    let index = |name: &str| items.iter().position(|item| item.name == name).unwrap();
    let mut graph = Graph::default();
    for &(from, to) in calls {
        graph.references.push(Reference {
            from: index(from),
            to: Node::Item(index(to)),
            kind: match from.ends_with("_veneer") {
                true => ReferenceKind::TailCall,
                false => ReferenceKind::Call,
            },
            site: items[index(from)].address,
        });
    }
    graph.references.sort_by_key(|r| (r.from, r.site));
    let mut binary = Binary::for_tests(sections, items, data);
    binary.architecture = Architecture::Arm;
    binary.pointer_size = 4;
    binary.thumb = true;
    Build {
        binary,
        graph,
        tree: None,
    }
}

/// `build` as an x86-64 build.
fn x86(mut build: Build) -> Build {
    build.binary.architecture = Architecture::X86_64;
    build.binary.pointer_size = 8;
    build.binary.thumb = false;
    build
}

/// Whether `name` is edited from `old` to `new`, whose items pair by
/// name.
fn edited(old: &Build, new: &Build, name: &str) -> bool {
    let partners = |from: &Build, to: &Build| -> Vec<Option<usize>> {
        let items = &to.binary.items;
        from.binary
            .items
            .iter()
            .map(|item| items.iter().position(|other| other.name == item.name))
            .collect()
    };
    let pairing = Pairing {
        old_to_new: partners(old, new)
            .into_iter()
            .map(|partner| partner.map(|partner| (partner, PairedBy::Name)))
            .collect(),
        new_to_old: partners(new, old),
    };
    let index = |build: &Build| {
        let items = &build.binary.items;
        items.iter().position(|item| item.name == name).unwrap()
    };
    Edits::new(old, new, &pairing).edited(index(old), index(new))
}

/// Whether `table`, a word at 0x400 that holds `old` in the old build
/// and `new` in the new one, is edited. The functions f and g swap
/// places, f at 0x100 and g at 0x110 in the old build; `gone` and
/// `new_item` are only in one build, at 0x480 and 0x490; and text that
/// no item holds moved: "hello" from 0x4c0 to 0x4e0, "hi!" at 0x4d0
/// became "hi?" at 0x4f0, and "okay", which "then" follows with no NUL
/// between, as Rust's strings do, is at 0x4d8 and then 0x4f8, before
/// "now". The new build also has "hello" in .data.
fn table_edited(old: u32, new: u32) -> bool {
    let old_bytes = old.to_le_bytes();
    let new_bytes = new.to_le_bytes();
    let old = build(
        &SECTIONS,
        &[
            ("f", 0x100, 0x10),
            ("g", 0x110, 0x10),
            ("table", 0x400, 4),
            ("gone", 0x480, 8),
        ],
        &[
            (0x400, &old_bytes),
            (0x4c0, b"hello\0"),
            (0x4d0, b"hi!"),
            (0x4d8, b"okaythen"),
        ],
        &[],
    );
    let new = build(
        &SECTIONS,
        &[
            ("g", 0x100, 0x10),
            ("f", 0x110, 0x10),
            ("table", 0x400, 4),
            ("new_item", 0x490, 8),
        ],
        &[
            (0x400, &new_bytes),
            (0x4e0, b"hello\0"),
            (0x4f0, b"hi?"),
            (0x4f8, b"okaynow"),
            (0x880, b"hello"),
        ],
        &[],
    );
    edited(&old, &new, "table")
}

#[test]
fn a_pointer_into_partners_at_the_same_offset_is_the_same() {
    // Case (a): f's address, with the Thumb bit, and 4 bytes into it.
    assert!(!table_edited(0x101, 0x111));
    assert!(!table_edited(0x104, 0x114));
    assert!(!table_edited(0x111, 0x101));
    // f's Thumb bit is lost.
    assert!(table_edited(0x101, 0x110));
}

#[test]
fn a_pointer_just_past_the_end_of_partners_is_the_same() {
    // Case (a): the end of f, which is where g starts in the old build
    // and where no item is in the new one, as for the end of an array.
    assert!(!table_edited(0x110, 0x120));
}

#[test]
fn a_number_that_changed_is_an_edit_where_it_reads_as_an_address_too() {
    // A coefficient, 5 to 7, and a table's 0x104 to 0x116: f's 4th byte
    // and 6th.
    assert!(table_edited(5, 7));
    assert!(table_edited(0x104, 0x116));
}

#[test]
fn pointers_into_items_that_only_one_build_has_are_the_same() {
    // Case (b): such items have rows of their own, but a pointer into a
    // partner that now points into one of them is an edit.
    assert!(!table_edited(0x480, 0x490));
    assert!(table_edited(0x484, 0x101));
    assert!(table_edited(0x101, 0x494));
}

#[test]
fn a_pointer_to_the_same_text_is_the_same() {
    // Case (c): text that no item holds, as a Rust string's. A whole
    // string, or 3 characters of one, are the same text, but not 2 that
    // go on otherwise, nor text in another section.
    assert!(!table_edited(0x4c0, 0x4e0));
    assert!(!table_edited(0x4c2, 0x4e2));
    assert!(!table_edited(0x4c3, 0x4e3));
    assert!(!table_edited(0x4d8, 0x4f8));
    assert!(table_edited(0x4d0, 0x4f0));
    assert!(table_edited(0x4c0, 0x880));
}

#[test]
fn a_pointer_to_the_same_end_of_a_section_is_the_same() {
    // Case (d): the end of .data, which shrank, as a linker script's
    // _edata.
    let data_ending_at = |end: u64| {
        let mut sections = SECTIONS;
        sections[2].2 = end - 0x800;
        let bytes = (end as u32).to_le_bytes();
        build(&sections, &[("table", 0x400, 4)], &[(0x400, &bytes)], &[])
    };
    let (old, new) = (data_ending_at(0x900), data_ending_at(0x880));
    assert!(!edited(&old, &new, "table"));
    let other = build(
        &SECTIONS,
        &[("table", 0x400, 4)],
        &[(0x400, &[0x80, 8, 0, 0])],
        &[],
    );
    assert!(edited(&old, &other, "table"));
}

#[test]
fn a_pointer_that_the_loader_writes_is_compared_as_it_writes_it() {
    // limit_ptr is 0 in the file, as LLD leaves it, and the loader
    // writes &limits[1], then &limits[2]; and f's address, which moved.
    let pointer = |to: u64| {
        let items = [
            ("f", 0x100, 0x10),
            ("g", 0x110, 0x10),
            ("limits", 0x400, 16),
            ("limit_ptr", 0x800, 8),
        ];
        let mut build = x86(build(&SECTIONS, &items, &[], &[]));
        build.binary.relocations = vec![Relocation {
            address: 0x800,
            target: RelocationTarget::Address(to),
        }];
        build
    };
    assert!(edited(&pointer(0x404), &pointer(0x408), "limit_ptr"));
    assert!(!edited(&pointer(0x404), &pointer(0x404), "limit_ptr"));
    let (mut old, new) = (pointer(0x100), pointer(0x110));
    old.binary.items.swap(0, 1);
    (old.binary.items[0].address, old.binary.items[1].address) = (0x100, 0x110);
    assert!(!edited(&old, &new, "limit_ptr"));
}

#[test]
fn x86_jump_table_entries_are_distances_from_the_table() {
    // A table at 0x404 of 3 entries, each a distance from it into f,
    // which moved from 0x100 to 0x120, while the table didn't.
    let table = |f: u64, offsets: [u64; 3]| {
        let entries: Vec<u8> = offsets
            .iter()
            .flat_map(|offset| ((f + offset).wrapping_sub(0x404) as u32).to_le_bytes())
            .collect();
        let items = [("f", f, 0x40), ("table", 0x404, 12)];
        x86(build(&SECTIONS, &items, &[(0x404, &entries)], &[]))
    };
    let old = table(0x100, [0x10, 0x20, 0x30]);
    assert!(!edited(&old, &table(0x120, [0x10, 0x20, 0x30]), "table"));
    // A case that now goes elsewhere in f.
    assert!(edited(&old, &table(0x120, [0x10, 0x28, 0x30]), "table"));
}

#[test]
fn words_are_pointers_where_they_fit_and_entries_elsewhere() {
    let ranges = |address, len, pointer, entries| -> Vec<std::ops::Range<usize>> {
        words(address, len, pointer, entries).collect()
    };
    // x86-64 data at 0x1004: an entry up to the first 8-byte boundary,
    // then pointers, and an entry where no pointer fits.
    assert_eq!(ranges(0x1004, 24, 8, true), [0..4, 4..12, 12..20, 20..24]);
    // Thumb code at 0x1002: words from the first 4-byte boundary, and
    // none in the 2 bytes left.
    assert_eq!(ranges(0x1002, 12, 4, false), [2..6, 6..10]);
    assert_eq!(ranges(0x1002, 5, 4, false), []);
}

/// Thumb's `bl` at `from` to `to`.
fn bl(from: u64, to: u64) -> [u8; 4] {
    let offset = to.wrapping_sub(from + 4) as u32;
    let s = (offset >> 24) & 1;
    let j1 = !((offset >> 23) ^ s) & 1;
    let j2 = !((offset >> 22) ^ s) & 1;
    let first = 0xf000 | (s << 10) | ((offset >> 12) & 0x3ff);
    let second = 0xd000 | (j1 << 13) | (j2 << 11) | ((offset >> 1) & 0x7ff);
    halfwords(first, second)
}

/// Thumb's `movw r0, #value`, or with `top`, `movt r0, #value`.
fn mov(top: bool, value: u32) -> [u8; 4] {
    let first =
        [0xf240, 0xf2c0][usize::from(top)] | ((value >> 12) & 0xf) | (((value >> 11) & 1) << 10);
    halfwords(first, (((value >> 8) & 7) << 12) | (value & 0xff))
}

fn halfwords(first: u32, second: u32) -> [u8; 4] {
    let [a, b] = (first as u16).to_le_bytes();
    let [c, d] = (second as u16).to_le_bytes();
    [a, b, c, d]
}

/// A build where f, at 0x100, calls g at `g` and puts `number` in r0
/// with `movw`, and with `movt`, and `var` is at `var`.
fn code(g: u64, number: u32, movt: bool, var: u64) -> Build {
    let mut code = bl(0x100, g).to_vec();
    code.extend(mov(false, number & 0xffff));
    if movt {
        code.extend(mov(true, number >> 16));
    }
    let items = [("f", 0x100, 0x10), ("g", g, 0x10), ("var", var, 4)];
    build(&SECTIONS, &items, &[(0x100, &code)], &[("f", "g")])
}

#[test]
fn thumb_code_is_the_same_where_only_distances_and_addresses_changed() {
    // g and var moved: the distance in bl to g and the address of var
    // that movw and movt put together changed.
    let old = code(0x120, 0x800, true, 0x800);
    assert!(!edited(&old, &code(0x130, 0x810, true, 0x810), "f"));
}

#[test]
fn the_new_build_is_read_as_thumb_when_the_old_one_is() {
    // As when the new build lost the section that says its code is
    // Thumb.
    let old = code(0x120, 0x800, true, 0x800);
    let mut new = code(0x130, 0x810, true, 0x810);
    new.binary.thumb = false;
    assert!(!edited(&old, &new, "f"));
}

#[test]
fn a_number_that_movw_and_movt_put_together_is_an_edit_when_it_changed() {
    // The number that f compares a header with, 0xe1f0 to 0xe1f2.
    let magic = |number| code(0x120, number, true, 0x800);
    assert!(edited(&magic(0xe1f0), &magic(0xe1f2), "f"));
}

#[test]
fn movw_alone_puts_no_address_together() {
    let alone = |number| code(0x120, number, false, number.into());
    assert!(edited(&alone(0x800), &alone(0x810), "f"));
}

#[test]
fn a_word_in_a_literal_pool_is_compared_as_a_word_but_not_as_a_use() {
    // f calls code at 0x1f0, where no item is, then loads the end of
    // ring from the pool after it, where `after` starts in the old
    // build and `extra`, a new item, in the new one, so the graph says
    // that f uses another item; or a number.
    let loading = |word: u32, items: &[(&str, u64, u64)]| {
        let mut code = bl(0x100, 0x1f0).to_vec();
        code.extend([0x00, 0x48, 0x00, 0xbf]);
        code.extend(word.to_le_bytes());
        let mut build = build(&SECTIONS, items, &[(0x100, &code)], &[]);
        if let Some(used) = items.iter().position(|item| item.1 == u64::from(word)) {
            build.graph.references.push(Reference {
                from: 0,
                to: Node::Item(used),
                kind: ReferenceKind::Address,
                site: 0x104,
            });
        }
        build
    };
    let old = [("f", 0x100, 12), ("ring", 0x800, 8), ("after", 0x808, 4)];
    let new = [
        ("f", 0x100, 12),
        ("ring", 0x810, 8),
        ("extra", 0x818, 4),
        ("after", 0x81c, 4),
    ];
    assert!(!edited(&loading(0x808, &old), &loading(0x818, &new), "f"));
    assert!(edited(&loading(0x808, &old), &loading(5, &old), "f"));
}

#[test]
fn partners_with_the_same_code_are_the_same() {
    // f's bl leads to 0x110 in both builds, where g is in the old one
    // and h in the new one. Only partners whose bytes differ are
    // compared.
    let calling = |items: &[(&str, u64, u64)], to: &str| {
        build(
            &SECTIONS,
            items,
            &[(0x100, &bl(0x100, 0x110))],
            &[("f", to)],
        )
    };
    let old = [("f", 0x100, 8), ("g", 0x110, 0x10), ("h", 0x120, 0x10)];
    let new = [("f", 0x100, 8), ("h", 0x110, 0x10), ("g", 0x120, 0x10)];
    assert!(!edited(&calling(&old, "g"), &calling(&new, "h"), "f"));
}

#[test]
fn x86_code_is_the_same_where_only_distances_changed() {
    // f calls g, and puts the address of var in eax.
    let code = |g: u64, var: u32| {
        let mut code = vec![0xe8];
        code.extend((g.wrapping_sub(0x105) as u32).to_le_bytes());
        code.push(0xb8);
        code.extend(var.to_le_bytes());
        code.push(0xc3);
        let items = [("f", 0x100, 11), ("g", g, 0x10), ("var", var.into(), 4)];
        x86(build(&SECTIONS, &items, &[(0x100, &code)], &[("f", "g")]))
    };
    let old = code(0x120, 0x800);
    assert!(!edited(&old, &code(0x130, 0x800), "f"));
    // An address in an instruction is compared as a number, as any.
    assert!(edited(&old, &code(0x120, 0x810), "f"));
}

#[test]
fn code_that_calls_other_items_is_edited() {
    let functions = [
        ("f", 0x100, 0x10),
        ("g", 0x110, 0x10),
        ("h", 0x120, 0x10),
        ("__g_veneer", 0x130, 8),
    ];
    // f's bl, and ldr.w pc, [pc, #-0], as GNU ld's veneer.
    let calling = |to: &str, calls: &[(&str, &str)]| {
        let at = functions.iter().find(|item| item.0 == to).unwrap().1;
        let bytes: &[(u64, &[u8])] = &[(0x100, &bl(0x100, at)), (0x130, &[0x5f, 0xf8, 0x00, 0xf0])];
        build(&SECTIONS, &functions, bytes, calls)
    };
    let old = calling("g", &[("f", "g")]);
    assert!(edited(&old, &calling("h", &[("f", "h")]), "f"));
    // Through a veneer, f still calls g.
    let through = calling("__g_veneer", &[("f", "__g_veneer"), ("__g_veneer", "g")]);
    assert!(!edited(&old, &through, "f"));
}

#[test]
fn a_call_from_an_item_that_only_one_build_has_to_another_doesnt_count() {
    // f calls gone in the old build and new in the new one, each with
    // a row of its own.
    let calling = |to: &str, at: u64| {
        let items = [("f", 0x100, 0x10), (to, at, 0x10)];
        build(&SECTIONS, &items, &[(0x100, &bl(0x100, at))], &[("f", to)])
    };
    assert!(!edited(
        &calling("gone", 0x110),
        &calling("new", 0x120),
        "f"
    ));
}

#[test]
fn imports_are_compared_by_name() {
    // f's bytes are the same, but the global offset table entry it
    // reads holds another import, which only one build has.
    let importing = |name: &str| {
        let mut build = x86(build(&SECTIONS, &[("f", 0x100, 0x10)], &[], &[]));
        build.binary.imports = vec![name.to_string()];
        build.graph.references.push(Reference {
            from: 0,
            to: Node::Import(0),
            kind: ReferenceKind::Call,
            site: 0x100,
        });
        build
    };
    assert!(edited(&importing("unlink"), &importing("rmdir"), "f"));
    assert!(!edited(&importing("unlink"), &importing("unlink"), "f"));
}
