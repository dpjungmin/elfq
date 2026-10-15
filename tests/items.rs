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
fn stripped_firmware_functions_are_found_by_following_the_code() {
    // No symbols and no unwind table, so elfq follows the code from the
    // entry point and the vector table, and the addresses stored in data.
    // Each function starts where the twin with symbols has one, and runs to
    // the next. Strings in the Cortex-M7 sample's data look like addresses
    // in its ITCM, which starts at 0, but start nothing.
    for (sample, found) in [
        ("c-cortex-m4", 25),
        ("c-cortex-m7", 29),
        ("rust-cortex-m4", 34),
    ] {
        let named = load(&format!("{sample}-nodebug.elf"));
        let stripped = load(&format!("{sample}-stripped.elf"));
        assert_eq!(functions(&stripped).len(), found, "{sample}");
        for function in functions(&stripped) {
            let twin = functions(&named)
                .into_iter()
                .find(|twin| twin.address == function.address)
                .unwrap_or_else(|| panic!("{sample}: no function starts at {}", function.name));
            assert!(function.size >= twin.size, "{sample}: {}", twin.name);
            assert!(function.size_estimated, "{sample}: {}", twin.name);
            if function.address != stripped.entry {
                let name = format!("fn_{:#x}", function.address);
                assert_eq!((&function.name, function.source), (&name, Source::Reached));
            }
        }
    }
    let binary = load("c-cortex-m4-stripped.elf");
    let entry = item(&binary, "entry");
    assert_eq!((entry.address, entry.size), (0x0800_05a0, 76));
    assert_eq!(entry.source, Source::EntryPoint);
}

#[test]
fn the_vector_table_is_found_by_its_shape() {
    // Stripping keeps the table, so its handlers can be checked against the
    // functions that the copy with symbols names.
    for (sample, address) in [
        ("c-cortex-m4", 0x0800_0000),
        ("c-cortex-m7", 0x3000_0000),
        ("rust-cortex-m4", 0x0800_0000),
    ] {
        let named = load(&format!("{sample}-nodebug.elf"));
        let stripped = load(&format!("{sample}-stripped.elf"));
        let table = stripped.vector_table.as_ref().unwrap();
        // 16 words: the stack pointer and the Cortex-M exceptions. The walk
        // stops at the end of the table's section, before the code.
        assert_eq!((table.address, table.size), (address, 0x40), "{sample}");
        assert_eq!(table.handlers[0], stripped.entry, "{sample}");
        for &handler in &table.handlers {
            assert!(
                functions(&named).iter().any(|f| f.address == handler),
                "{sample}: no function starts at {handler:#x}"
            );
        }
        assert_eq!(named.vector_table.as_ref(), Some(table), "{sample}");
    }
    // The Cortex-M7 sample's SysTick handler is in ITCM, at address 0.
    let m7 = load("c-cortex-m7-stripped.elf");
    assert_eq!(m7.vector_table.unwrap().handlers.last(), Some(&0));
    assert_eq!(load("c-x86_64.elf").vector_table, None);
}

#[test]
fn reserved_vector_table_entries_can_hold_other_words() {
    // NXP's LPC parts want the checksum of the first seven words in entry
    // 7, which the architecture reserves. The table goes on past it.
    let sample = "c-cortex-m4-stripped.elf";
    let binary = load(sample);
    let table = binary.vector_table.clone().unwrap();
    let words: Vec<u32> = (0..7)
        .map(|index| binary.pointer_at(table.address + index * 4).unwrap() as u32)
        .collect();
    let mut data = std::fs::read(path(&format!("testdata/bin/{sample}"))).unwrap();
    let first_words: Vec<u8> = words[..2]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
    let offset = data
        .windows(8)
        .position(|bytes| bytes == first_words)
        .unwrap();
    let checksum = words.iter().fold(0u32, |sum, word| sum.wrapping_sub(*word));
    data[offset + 28..offset + 32].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(Binary::parse(&data).unwrap().vector_table, Some(table));
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
