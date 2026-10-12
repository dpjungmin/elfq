//! Item discovery, checked against the sample binaries in testdata/bin.
//! testdata/README.md describes what each sample contains.

use std::path::{Path, PathBuf};

use elfq::binary::{Binary, Item, ItemKind, Source};

fn path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn load(name: &str) -> Binary {
    Binary::load(&path(&format!("testdata/bin/{name}"))).unwrap()
}

/// The item that has `name` as its name or as an alias.
fn item<'a>(binary: &'a Binary, name: &str) -> &'a Item {
    binary
        .items
        .iter()
        .find(|item| item.name == name || item.aliases.iter().any(|alias| alias == name))
        .unwrap_or_else(|| panic!("no item named {name}"))
}

fn functions(binary: &Binary) -> Vec<&Item> {
    binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .collect()
}

#[test]
fn symbols_give_names_addresses_and_sizes() {
    let binary = load("c-x86_64.elf");
    let function = item(&binary, "process_frame");
    assert_eq!(function.kind, ItemKind::Function);
    assert_eq!((function.address, function.size), (0x1700, 532));
    assert_eq!(function.source, Source::SymbolTable);

    let table = item(&binary, "crc_table");
    assert_eq!((table.kind, table.size), (ItemKind::Data, 1024));
}

#[test]
fn debug_info_does_not_change_the_items() {
    for sample in ["c-x86_64", "c-cortex-m4", "rust-cortex-m4"] {
        let full = load(&format!("{sample}.elf"));
        let nodebug = load(&format!("{sample}-nodebug.elf"));
        assert_eq!(full.items, nodebug.items, "{sample}");
    }
}

#[test]
fn stripped_x86_64_files_get_functions_from_the_unwind_table() {
    for (sample, expected) in [("c-x86_64", 24), ("rust-x86_64", 531)] {
        let named = load(&format!("{sample}-nodebug.elf"));
        let stripped = load(&format!("{sample}-stripped.elf"));
        let found = functions(&stripped);
        assert_eq!(found.len(), expected, "{sample}");
        // Each one matches a function the symbols describe, start and size.
        for function in found {
            assert_eq!(function.source, Source::EhFrame);
            assert!(
                functions(&named)
                    .iter()
                    .any(|f| (f.address, f.size) == (function.address, function.size)),
                "{sample}: {} has no matching symbol",
                function.name
            );
        }
    }
    let stripped = load("c-x86_64-stripped.elf");
    let process_frame = item(&stripped, "fn_0x1700");
    assert_eq!(process_frame.size, 532);
}

#[test]
fn stripped_firmware_has_only_its_entry_point_so_far() {
    // No symbols and no unwind table. Scanning the vector table and the code
    // will find more; until then the entry point is all there is.
    let binary = load("c-cortex-m4-stripped.elf");
    let [entry] = &binary.items[..] else {
        panic!("expected one item, found {}", binary.items.len());
    };
    assert_eq!((entry.name.as_str(), entry.address), ("entry", 0x0800_05a0));
    assert_eq!((entry.source, entry.size), (Source::EntryPoint, 0));
}

#[test]
fn thumb_addresses_are_even() {
    // Arm sets bit 0 in Thumb function symbols and in the entry point.
    let binary = load("c-cortex-m4.elf");
    assert_eq!(item(&binary, "process_frame").address, 0x0800_02c8);
    assert_eq!(binary.entry, 0x0800_05a0);
    assert_eq!(item(&binary, "Reset_Handler").address, binary.entry);
}

#[test]
fn symbols_at_one_address_are_aliases_of_one_item() {
    let binary = load("c-cortex-m4.elf");
    let handler = item(&binary, "NMI_Handler");
    // The strong symbol names the item; the weak aliases point at it.
    assert_eq!(handler.name, "Default_Handler");
    assert_eq!(handler.aliases.len(), 8);
    assert_eq!(handler.address, item(&binary, "PendSV_Handler").address);

    // Identical code folding: two Display impls share one function.
    let binary = load("rust-cortex-m4.elf");
    let fmt = binary
        .items
        .iter()
        .find(|i| i.address == 0x0800_176c)
        .unwrap();
    assert_eq!(fmt.aliases.len(), 1);
}

#[test]
fn merged_globals_become_separate_items() {
    // LLVM places several statics in one `.L_MergedGlobals` block.
    let binary = load("rust-cortex-m4.elf");
    assert!(binary.items.iter().all(|i| i.name != ".L_MergedGlobals"));
    let frames_bad = binary
        .items
        .iter()
        .find(|i| i.name.contains("FRAMES_BAD"))
        .unwrap();
    assert_eq!(frames_bad.size, 4);
}

#[test]
fn sizes_missing_from_the_symbol_table_are_estimated() {
    let binary = load("c-x86_64.elf");
    let function = item(&binary, "deregister_tm_clones");
    assert_eq!((function.size, function.size_estimated), (48, true));
}

#[test]
fn rejects_files_that_are_not_elf() {
    let err = Binary::load(&path("Cargo.toml")).unwrap_err();
    assert!(format!("{err:#}").ends_with("not an ELF file"), "{err:#}");
}

#[test]
fn rejects_object_files() {
    // A 64-byte ELF header for an x86-64 relocatable object with no sections.
    let mut header = vec![0u8; 64];
    header[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    header[16] = 1; // e_type = ET_REL
    header[18] = 0x3e; // e_machine = x86-64
    header[20] = 1; // e_version
    header[52] = 64; // e_ehsize
    let err = Binary::parse(&header).unwrap_err();
    assert!(err.to_string().contains("object file"), "{err:#}");
}
