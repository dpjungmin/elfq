use object::SectionKind;

use super::*;
use crate::binary::Section;
use crate::graph::{Graph, Reference};

/// An item for [`binary`]: a function in `.text` when `kind` is one,
/// else data, in `.rodata` with `bytes`, or with `bss`, in `.bss`, of
/// `bytes`' size.
struct Spec {
    name: &'static str,
    aliases: &'static [&'static str],
    kind: ItemKind,
    source: Source,
    bss: bool,
    bytes: &'static [u8],
}

fn function(name: &'static str, bytes: &'static [u8]) -> Spec {
    Spec {
        name,
        aliases: &[],
        kind: ItemKind::Function,
        source: Source::SymbolTable,
        bss: false,
        bytes,
    }
}

fn data(name: &'static str, bytes: &'static [u8]) -> Spec {
    Spec {
        kind: ItemKind::Data,
        ..function(name, bytes)
    }
}

/// Data whose name elfq made from its address, as for a string.
fn unnamed(name: &'static str, bytes: &'static [u8]) -> Spec {
    Spec {
        source: Source::Reference,
        ..data(name, bytes)
    }
}

/// A file whose `.text` at 0x1000, `.rodata` at 0x2000 and `.bss` at
/// 0x3000 hold the items, one after another.
fn binary(specs: &[Spec]) -> Binary {
    let section = |name: &str, address, kind| Section {
        name: name.to_string(),
        address,
        size: 0,
        kind,
        allocated: true,
        file_range: None,
        entry_size: 0,
        load_address: None,
    };
    let mut sections = vec![
        section("", 0, SectionKind::Unknown),
        section(".text", 0x1000, SectionKind::Text),
        section(".rodata", 0x2000, SectionKind::ReadOnlyData),
        section(".bss", 0x3000, SectionKind::UninitializedData),
    ];
    let mut contents: [Vec<u8>; 3] = Default::default();
    let mut items = Vec::new();
    for spec in specs {
        let at = match (spec.kind, spec.bss) {
            (ItemKind::Function, _) => 1,
            (_, false) => 2,
            (_, true) => 3,
        };
        items.push(Item {
            name: spec.name.to_string(),
            aliases: spec.aliases.iter().map(|alias| alias.to_string()).collect(),
            kind: spec.kind,
            address: sections[at].address + contents[at - 1].len() as u64,
            size: spec.bytes.len() as u64,
            size_estimated: false,
            section: at,
            source: spec.source,
        });
        contents[at - 1].extend_from_slice(spec.bytes);
    }
    let mut file = Vec::new();
    for (at, bytes) in contents.into_iter().enumerate() {
        let section = &mut sections[at + 1];
        section.size = bytes.len() as u64;
        if at < 2 {
            section.file_range = Some((file.len() as u64, bytes.len() as u64));
            file.extend(bytes);
        }
    }
    items.sort_by_key(|item| item.address);
    Binary::for_tests(sections, items, file)
}

/// Each item of `old` by name, with the name of its partner in `new`
/// and the rule that paired them, using `references` in both.
fn pairs(
    old: &[Spec],
    new: &[Spec],
    references: &[(usize, usize)],
) -> Vec<(String, Option<(String, PairedBy)>)> {
    let graph = |binary: &Binary| Graph {
        references: references
            .iter()
            .map(|&(from, to)| Reference {
                from,
                to: Node::Item(to),
                kind: ReferenceKind::Address,
                site: binary.items[from].address,
            })
            .collect(),
        ..Graph::default()
    };
    let build = |specs| {
        let binary = binary(specs);
        Build {
            graph: graph(&binary),
            binary,
            tree: None,
        }
    };
    let (old, new) = (build(old), build(new));
    let pairing = pair(&old, &new);
    old.binary
        .items
        .iter()
        .zip(pairing.old_to_new)
        .map(|(item, partner)| {
            let partner = partner.map(|(index, rule)| (new.binary.items[index].name.clone(), rule));
            (item.name.clone(), partner)
        })
        .collect()
}

fn paired(old: &str, new: &str, rule: PairedBy) -> (String, Option<(String, PairedBy)>) {
    (old.to_string(), Some((new.to_string(), rule)))
}

fn unpaired(old: &str) -> (String, Option<(String, PairedBy)>) {
    (old.to_string(), None)
}

#[test]
fn rule_1_pairs_by_name_only_one_item_of_each_build_and_of_one_kind() {
    // Two static functions named `twice` pair with neither, and a
    // function never pairs with data.
    assert_eq!(
        pairs(
            &[
                function("crc32", b"\x01\x02\x03\x04\x05"),
                function("twice", b"\x11\x12\x13\x14\x15"),
                function("twice", b"\x21\x22\x23\x24\x25"),
                function("table", b"\x31\x32\x33\x34\x35"),
            ],
            &[
                function("crc32", b"\x41\x42\x43\x44\x45\x46"),
                function("twice", b"\x51\x52\x53\x54\x55"),
                data("table", b"\x61\x62\x63\x64\x65"),
            ],
            &[],
        ),
        [
            paired("crc32", "crc32", PairedBy::Name),
            unpaired("twice"),
            unpaired("twice"),
            unpaired("table"),
        ]
    );
}

#[test]
fn rule_2_pairs_by_another_of_their_names() {
    // A weak alias that became the name, as when the default handler
    // is replaced.
    assert_eq!(
        pairs(
            &[Spec {
                aliases: &["SysTick_Handler"],
                ..function("Default_Handler", b"\xfe\xe7")
            }],
            &[Spec {
                aliases: &["Default_Handler"],
                ..function("SysTick_Handler", b"\x70\x47")
            }],
            &[],
        ),
        [paired("Default_Handler", "SysTick_Handler", PairedBy::Name)]
    );
}

#[test]
fn rule_2_pairs_neither_when_the_names_of_one_item_lead_to_two() {
    // Identical code folding gave a stub the names of the old stub and
    // of a function that does the same: neither pairs by name, whichever
    // build is the old one, and the stubs pair by their bytes.
    let stub = Spec {
        aliases: &["Error::cause"],
        ..function("Stub::cause", b"\x31\xc0\xc3")
    };
    let parse = function("ParseError::cause", b"\x01\x02\x03\x04\x05");
    let folded = || Spec {
        aliases: &["Error::cause", "ParseError::cause"],
        ..function("Box::cause", b"\x31\xc0\xc3")
    };
    assert_eq!(
        pairs(&[stub, parse], &[folded()], &[]),
        [
            paired("Stub::cause", "Box::cause", PairedBy::Bytes),
            unpaired("ParseError::cause")
        ]
    );
    let stub = Spec {
        aliases: &["Error::cause"],
        ..function("Stub::cause", b"\x31\xc0\xc3")
    };
    let parse = function("ParseError::cause", b"\x01\x02\x03\x04\x05");
    assert_eq!(
        pairs(&[folded()], &[stub, parse], &[]),
        [paired("Box::cause", "Stub::cause", PairedBy::Bytes)]
    );
}

#[test]
fn rule_3_pairs_a_compiler_copy_of_a_function_with_it() {
    assert_eq!(
        pairs(
            &[
                function("dev_read", b"\x01\x02\x03\x04\x05"),
                function("scale.llvm.123", b"\x11\x12\x13\x14\x15"),
                function("std::sort(int*) [clone .isra.0]", b"\x21\x22\x23\x24\x25"),
            ],
            &[
                function("dev_read.constprop.0", b"\x41\x42\x43\x44"),
                function("scale.llvm.456", b"\x51\x52\x53\x54"),
                function("std::sort(int*)", b"\x61\x62\x63\x64"),
            ],
            &[],
        ),
        [
            paired("dev_read", "dev_read.constprop.0", PairedBy::Name),
            paired("scale.llvm.123", "scale.llvm.456", PairedBy::Name),
            paired(
                "std::sort(int*) [clone .isra.0]",
                "std::sort(int*)",
                PairedBy::Name
            ),
        ]
    );
}

#[test]
fn rule_4_pairs_by_bytes_but_a_made_name_pairs_nothing() {
    // The code is the same once the padding after it is left out.
    // `.rodata@0x2000` is in both, but the name is elfq's, from its
    // address, and the bytes differ. So is `.str.1051`, which LLVM
    // numbered, as libLLVM 20 and 22 show.
    assert_eq!(
        pairs(
            &[
                Spec {
                    source: Source::EhFrame,
                    ..function("fn_0x1000", b"\x01\x02\x03\x04\x05\x90\x90")
                },
                unnamed(".rodata@0x2000", b"\x11\x12\x13\x14\x15"),
                data(".str.1051.llvm.27", b"N1UnitV1\0"),
            ],
            &[
                Spec {
                    source: Source::EhFrame,
                    ..function("fn_0x1000", b"\x01\x02\x03\x04\x05\x00\x00")
                },
                unnamed(".rodata@0x2000", b"\x21\x22\x23\x24\x25"),
                data(".str.1051.llvm.84", b"M3UnitFST\0"),
            ],
            &[],
        ),
        [
            paired("fn_0x1000", "fn_0x1000", PairedBy::Bytes),
            unpaired(".rodata@0x2000"),
            unpaired(".str.1051.llvm.27"),
        ]
    );
}

#[test]
fn rule_5_pairs_data_by_its_first_string() {
    assert_eq!(
        pairs(
            &[unnamed(".rodata@0x2000", b"usage: %s\0old\0")],
            &[unnamed(".rodata@0x2000", b"usage: %s\0newer\0")],
            &[],
        ),
        [paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Bytes)]
    );
}

#[test]
fn rule_6_pairs_a_few_bytes_of_one_size() {
    // Too few bytes for rule 4, and no string.
    assert_eq!(
        pairs(
            &[unnamed(".rodata@0x2000", b"\x01\x00\x00\x00")],
            &[unnamed(".rodata@0x2000", b"\x01\x00\x00\x00")],
            &[],
        ),
        [paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Bytes)]
    );
}

#[test]
fn rule_7_pairs_data_by_what_uses_it() {
    // main, paired by name, is the only user of each.
    assert_eq!(
        pairs(
            &[
                function("main", b"\x01\x02\x03\x04\x05"),
                unnamed(".rodata@0x2000", b"\x11\x12\x13\x14\x15"),
            ],
            &[
                function("main", b"\x01\x02\x03\x04\x05\x06"),
                unnamed(".rodata@0x2000", b"\x21\x22\x23\x24\x25"),
            ],
            &[(0, 1)],
        ),
        [
            paired("main", "main", PairedBy::Name),
            paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Uses),
        ]
    );
}

#[test]
fn rules_1_to_3_leave_out_a_name_that_gcc_numbered() {
    // A function with a switch table of its own, added before f, moved
    // the number of f's.
    assert_eq!(
        pairs(
            &[data("CSWTCH.3", b"\x01\x02\x03\x04\x05")],
            &[
                data("CSWTCH.3", b"\x11\x12\x13\x14\x15\x16"),
                data("CSWTCH.5", b"\x01\x02\x03\x04\x05"),
            ],
            &[],
        ),
        [paired("CSWTCH.3", "CSWTCH.5", PairedBy::Bytes)]
    );
}

#[test]
fn rule_7_keeps_the_numbered_variables_of_a_function_apart_by_name() {
    // Two static variables of main, which only main uses, numbered
    // otherwise in the new build.
    let bss = |name, bytes| Spec {
        bss: true,
        ..data(name, bytes)
    };
    assert_eq!(
        pairs(
            &[
                function("main", b"\x01\x02\x03\x04\x05"),
                bss("buf.1", &[0; 16]),
                bss("count.2", &[0; 4]),
            ],
            &[
                function("main", b"\x01\x02\x03\x04\x05\x06"),
                bss("buf.3", &[0; 32]),
                bss("count.4", &[0; 4]),
            ],
            &[(0, 1), (0, 2)],
        ),
        [
            paired("main", "main", PairedBy::Name),
            paired("buf.1", "buf.3", PairedBy::Uses),
            paired("count.2", "count.4", PairedBy::Uses),
        ]
    );
}

#[test]
fn rule_7_pairs_numbered_names_that_one_function_uses_alike_by_their_whole_names() {
    // GCC split a structure that only main uses into its fields, which
    // keep their numbers from build to build.
    let bss = |name, bytes| Spec {
        bss: true,
        ..data(name, bytes)
    };
    assert_eq!(
        pairs(
            &[
                function("main", b"\x01\x02\x03\x04\x05"),
                bss("state.0", &[0; 8]),
                bss("state.1", &[0; 8]),
            ],
            &[
                function("main", b"\x01\x02\x03\x04\x05\x06"),
                bss("state.0", &[0; 8]),
                bss("state.1", &[0; 8]),
            ],
            &[(0, 1), (0, 2)],
        ),
        [
            paired("main", "main", PairedBy::Name),
            paired("state.0", "state.0", PairedBy::Uses),
            paired("state.1", "state.1", PairedBy::Uses),
        ]
    );
}

#[test]
fn rule_7_pairs_a_numbered_name_with_the_name_before_the_number() {
    // A static variable of _sbrk, in place of a global of one name.
    let bss = |name, bytes| Spec {
        bss: true,
        ..data(name, bytes)
    };
    assert_eq!(
        pairs(
            &[
                function("_sbrk", b"\x01\x02\x03\x04\x05"),
                bss("heap_end", &[0; 4])
            ],
            &[
                function("_sbrk", b"\x01\x02\x03\x04\x05\x06"),
                bss("heap_end.0", &[0; 4]),
            ],
            &[(0, 1)],
        ),
        [
            paired("_sbrk", "_sbrk", PairedBy::Name),
            paired("heap_end", "heap_end.0", PairedBy::Uses),
        ]
    );
}

#[test]
fn names_can_differ_only_in_numbers_that_change_with_every_build() {
    let same = |old: Spec, new: Spec| {
        let (old, new) = (binary(&[old]), binary(&[new]));
        same_but_numbers(&old.items[0], &new.items[0])
    };
    for (old, new) in [
        ("scale.llvm.123.cold", "scale.llvm.456.cold"),
        ("assoc_def.llvm.123.cold", "assoc_def.cold"),
        (
            "Table [clone .llvm.123]",
            "Table [clone .llvm] [clone .456]",
        ),
    ] {
        assert!(same(function(old, b""), function(new, b"")), "{old}");
    }
    assert!(same(data("buf.1", b""), data("buf.3", b"")));
    // A copy is another function; and only a C variable's name has
    // GCC's number.
    assert!(!same(
        function("dev_read", b""),
        function("dev_read.constprop.0", b"")
    ));
    assert!(!same(function("f.1", b""), function("f.2", b"")));
}

#[test]
fn rule_8_pairs_the_only_unnamed_object_in_a_section_without_bytes() {
    assert_eq!(
        pairs(
            &[Spec {
                bss: true,
                ..unnamed(".bss@0x3000", &[0; 8])
            }],
            &[Spec {
                bss: true,
                ..unnamed(".bss@0x3000", &[0; 16])
            }],
            &[],
        ),
        [paired(".bss@0x3000", ".bss@0x3000", PairedBy::Place)]
    );
}
