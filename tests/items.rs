//! Item discovery, checked against the sample binaries in testdata/bin.
//! testdata/README.md describes what each sample contains.

use std::path::{Path, PathBuf};

use elfq::binary::{Binary, Item, ItemKind, RelocationTarget, Source};

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
    for sample in ["c-x86_64", "c-cortex-m4", "c-cortex-m7", "rust-cortex-m4"] {
        let full = load(&format!("{sample}.elf"));
        let nodebug = load(&format!("{sample}-nodebug.elf"));
        assert_eq!(full.items, nodebug.items, "{sample}");
    }
}

#[test]
fn stripped_x86_64_files_get_functions_from_the_unwind_table() {
    for (sample, expected) in [("c-x86_64", 21), ("rust-x86_64", 531)] {
        let named = load(&format!("{sample}-nodebug.elf"));
        let stripped = load(&format!("{sample}-stripped.elf"));
        let found: Vec<&Item> = functions(&stripped)
            .into_iter()
            .filter(|function| function.source == Source::EhFrame)
            .collect();
        assert_eq!(found.len(), expected, "{sample}");
        // Each one matches a function the symbols describe, start and size.
        for function in found {
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
fn plt_stubs_are_named_after_the_imports_they_call() {
    // Both PLT layouts: GCC's, with `.plt.sec` and `.plt.got`, and lld's
    // single `.plt` (Rust).
    for sample in ["c-x86_64.elf", "c-x86_64-stripped.elf"] {
        let binary = load(sample);
        let stubs: Vec<(&str, u64, u64)> = binary
            .items
            .iter()
            .filter(|item| item.source == Source::Plt)
            .map(|item| (item.name.as_str(), item.address, item.size))
            .collect();
        assert_eq!(
            stubs,
            [
                (".plt", 0x1020, 64),
                ("__cxa_finalize@plt", 0x1060, 16),
                ("write@plt", 0x1070, 16),
                ("strlen@plt", 0x1080, 16),
                ("__stack_chk_fail@plt", 0x1090, 16),
            ],
            "{sample}"
        );
    }
    let binary = load("rust-x86_64-stripped.elf");
    assert_eq!(item(&binary, "_Unwind_Resume@plt").address, 0x51cb0);
}

#[test]
fn startup_and_exit_arrays_are_data_items() {
    let binary = load("c-x86_64-stripped.elf");
    let init = item(&binary, ".init_array");
    assert_eq!(
        (init.kind, init.address, init.size),
        (ItemKind::Data, 0x3d80, 8)
    );
    assert_eq!(init.source, Source::Section);
}

#[test]
fn dynamic_relocations_give_stored_pointers_and_imports() {
    let binary = load("c-x86_64.elf");
    // `.init_array` holds a pointer to `frame_dummy`.
    let relocation = binary.relocation_at(0x3d80).unwrap();
    assert_eq!(
        relocation.target,
        RelocationTarget::Address(item(&binary, "frame_dummy").address)
    );
    let mut imports = binary.imports.clone();
    imports.sort();
    assert_eq!(
        imports,
        [
            "_ITM_deregisterTMCloneTable",
            "_ITM_registerTMCloneTable",
            "__cxa_finalize",
            "__gmon_start__",
            "__libc_start_main",
            "__stack_chk_fail",
            "strlen",
            "write",
        ]
    );
}

#[test]
fn stripped_firmware_has_only_its_entry_point_so_far() {
    // No symbols and no unwind table. Scanning the vector table and the code
    // will find more; until then the entry point is all there is.
    for (sample, address) in [
        ("c-cortex-m4-stripped.elf", 0x0800_05a0),
        ("c-cortex-m7-stripped.elf", 0x3000_05f8),
    ] {
        let binary = load(sample);
        let [entry] = &binary.items[..] else {
            panic!("{sample}: expected one item, found {}", binary.items.len());
        };
        assert_eq!((entry.name.as_str(), entry.address), ("entry", address));
        assert_eq!((entry.source, entry.size), (Source::EntryPoint, 0));
    }
}

#[test]
fn data_without_symbols_starts_where_something_refers_to_it() {
    let binary = load("c-x86_64.elf");
    // A string literal, which runs to the next one: "bad header\n" and its
    // terminating NUL.
    let string = item(&binary, ".rodata@0x2012");
    assert_eq!(
        (
            string.kind,
            string.address,
            string.size,
            string.size_estimated
        ),
        (ItemKind::Data, 0x2012, 12, true)
    );
    assert_eq!(string.source, Source::Reference);
    // The global offset table gets no items: elfq follows its pointers.
    assert!(binary.items.iter().all(|item| {
        let section = &binary.sections[item.section].name;
        section != ".got" && section != ".got.plt"
    }));
}

#[test]
fn thumb_addresses_are_even() {
    // Arm sets bit 0 in Thumb function symbols and in the entry point.
    let binary = load("c-cortex-m4.elf");
    assert!(binary.thumb);
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

    // Identical code folding: two Display impls share one function. Each
    // one's mangled symbol is an alias too.
    let binary = load("rust-cortex-m4.elf");
    let fmt = binary
        .items
        .iter()
        .find(|i| i.address == 0x0800_176c)
        .unwrap();
    assert_eq!(fmt.name, "<u32 as core::fmt::Display>::fmt");
    assert_eq!(fmt.aliases.len(), 3);
    assert!(fmt.aliases[1..].contains(&"<usize as core::fmt::Display>::fmt".to_string()));
    assert_eq!(
        fmt.aliases.iter().filter(|a| a.starts_with("_R")).count(),
        2
    );
}

#[test]
fn rust_names_are_demangled() {
    let binary = load("rust-x86_64-nodebug.elf");
    let process_frame = item(&binary, "elfq_sample::process_frame");
    assert_eq!(
        process_frame.aliases,
        ["_RNvCsjtrlrMDykj5_11elfq_sample13process_frame"]
    );
    let run = item(&binary, "<elfq_sample::Gain as elfq_sample::Stage>::run");
    assert_eq!(run.address, 0x15080);
    // Each copy of a generic function has its own name.
    for name in ["checksum::<u8>", "checksum::<u16>", "checksum::<u32>"] {
        item(&binary, &format!("elfq_sample::{name}"));
    }
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
