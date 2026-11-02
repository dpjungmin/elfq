use object::{Architecture, Endianness};

use super::*;

/// A Thumb executable whose entry point is `entry`, with `sections`:
/// each a name, an address, a kind and the words it holds, which the
/// file stores one section after another.
fn firmware(entry: u64, sections: &[(&str, u64, SectionKind, Vec<u32>)]) -> Binary {
    let mut data = Vec::new();
    let sections = (sections.iter())
        .map(|(name, address, kind, words)| {
            let offset = data.len() as u64;
            data.extend(words.iter().flat_map(|word| word.to_le_bytes()));
            let size = data.len() as u64 - offset;
            Section {
                name: name.to_string(),
                address: *address,
                size,
                kind: *kind,
                allocated: true,
                file_range: Some((offset, size)),
                entry_size: 0,
                load_address: None,
            }
        })
        .collect();
    Binary {
        architecture: Architecture::Arm,
        pointer_size: 4,
        kind: FileKind::Executable,
        entry,
        thumb: true,
        endianness: Endianness::Little,
        has_symbols: false,
        has_dwarf: false,
        sections,
        items: Vec::new(),
        imports: Vec::new(),
        relocations: Vec::new(),
        used_by_unwinder: Vec::new(),
        merged_variables: Vec::new(),
        vector_table: None,
        arm_architecture: None,
        data_in_code: Vec::new(),
        warnings: Vec::new(),
        code_references: std::collections::HashMap::new(),
        data,
    }
}

/// The handler of `exception`, whose code starts at `address`.
fn handler(exception: u64, address: u64) -> Handler {
    Handler { exception, address }
}

#[test]
fn a_short_table_in_code_ends_at_its_last_handler() {
    // The stack pointer, then the reset handler and five fault handlers,
    // all at 0x1030. Five words of code follow, where entries 7 to 10
    // would be reserved, and then the reset handler's code.
    let mut words = vec![0x2000_1000_u32];
    words.extend([0x1031; 6]);
    words.extend([0xbf00_bf00; 5]);
    words.push(0xe7fe_e7fe);
    let binary = firmware(0x1030, &[(".text", 0x1000, SectionKind::Text, words)]);
    let table = VectorTable {
        address: 0x1000,
        size: 28,
        handlers: (1..=6)
            .map(|exception| handler(exception, 0x1030))
            .collect(),
    };
    assert_eq!(find(&binary), Some(table));
}

/// Firmware whose entry point, 0x1050, isn't its reset handler, 0x1040,
/// as when a linker script names `main` as the entry point. `.isr` holds
/// the vector table; `.rodata`, which the file lists first and whose
/// address comes first, begins like a table but for a stack pointer
/// that isn't a multiple of 4, as an array of Thumb addresses would.
fn entry_point_is_main(kind: FileKind) -> Binary {
    let mut binary = firmware(
        0x1050,
        &[
            (
                ".rodata",
                0x800,
                SectionKind::ReadOnlyData,
                vec![0x1041, 0x1041, 0x1041, 0x1041],
            ),
            (
                ".isr",
                0x900,
                SectionKind::ReadOnlyData,
                vec![0x2000_1000, 0x1041, 0x1045, 0x1045],
            ),
            (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
        ],
    );
    binary.kind = kind;
    binary
}

#[test]
fn a_table_that_does_not_hold_the_entry_point_is_at_a_section_start() {
    let table = VectorTable {
        address: 0x900,
        size: 16,
        handlers: vec![handler(1, 0x1040), handler(2, 0x1044), handler(3, 0x1044)],
    };
    let binary = entry_point_is_main(FileKind::Executable);
    assert_eq!(find(&binary), Some(table));
    assert_eq!(find(&binary).unwrap().reset_handler(), Some(0x1040));
}

#[test]
fn only_executables_linked_at_one_address_have_a_table_found_by_how_it_begins() {
    for kind in [
        FileKind::PositionIndependentExecutable,
        FileKind::SharedLibrary,
    ] {
        assert_eq!(find(&entry_point_is_main(kind)), None);
    }
}

#[test]
fn a_table_can_begin_a_section_of_code() {
    // As in pico-sdk, `.text` begins with the table, and the entry
    // point is code after it.
    let mut text = vec![0x2000_1000, 0x1015, 0x1019, 0x1019];
    text.extend([0xbf00_bf00; 8]);
    let binary = firmware(0x1010, &[(".text", 0x1000, SectionKind::Text, text)]);
    let table = find(&binary).unwrap();
    assert_eq!(
        (table.address, table.reset_handler()),
        (0x1000, Some(0x1014))
    );
}

#[test]
fn an_unwind_table_does_not_hold_the_reset_entry() {
    // Linked by LLD with no entry point, firmware's entry point is 0,
    // and `.ARM.exidx` can begin with an entry that says a function
    // can't be unwound: an offset, then 1, the Thumb address of 0.
    let mut text = vec![0x2000_1000, 0x41, 0x45, 0x45];
    text.extend([0xbf00_bf00; 16]);
    let binary = firmware(
        0,
        &[
            (".text", 0, SectionKind::Text, text),
            (
                ".ARM.exidx",
                0x100,
                SectionKind::Unknown,
                vec![0x7fff_ff5c, 1],
            ),
        ],
    );
    let table = find(&binary).unwrap();
    assert_eq!((table.address, table.reset_handler()), (0, Some(0x40)));
}

#[test]
fn a_table_of_handlers_and_their_arguments_does_not_pass_for_one() {
    // Built to start after a boot loader, Zephyr firmware begins with
    // 0s and then the vector table. `_sw_isr_table`, after it, holds
    // each interrupt's argument and handler: for an interrupt that
    // isn't used, 0 and a handler that reports it. The first
    // interrupt's argument can be 0 or point to RAM.
    let mut rom_start = vec![0; 4];
    rom_start.extend([0x2000_1000, 0x1041, 0x1045, 0x1045]);
    for argument in [0, 0x2000_0000] {
        let sections = |kind| {
            [
                ("rom_start", 0x800, kind, rom_start.clone()),
                (
                    "sw_isr_table",
                    0x900,
                    SectionKind::ReadOnlyData,
                    vec![argument, 0x1049, 0, 0x1049],
                ),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ]
        };
        // In data, the word that holds the entry point is the reset
        // entry.
        let binary = firmware(0x1040, &sections(SectionKind::ReadOnlyData));
        assert_eq!(find(&binary).map(|table| table.address), Some(0x810));
        // In code, that word isn't read, and elfq finds no table rather
        // than a wrong one.
        let binary = firmware(0x1040, &sections(SectionKind::Text));
        assert_eq!(find(&binary), None);
    }
}

#[test]
fn a_table_can_begin_read_only_data_that_holds_strings() {
    // LLD marks an output section as one of strings when any section it
    // holds is, so `.rodata` that begins with the table and holds
    // string literals is one.
    let rodata = vec![0x2000_1000, 0x1041, 0x1045, 0x1045, 0x6c6c_6568, 0x6f];
    let sections = [
        (".rodata", 0x800, SectionKind::ReadOnlyString, rodata),
        (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
    ];
    // The entry point is the reset handler, or `main`.
    for entry in [0x1040, 0x1050] {
        let table = find(&firmware(entry, &sections)).unwrap();
        assert_eq!(
            (table.address, table.reset_handler()),
            (0x800, Some(0x1040))
        );
    }
}

#[test]
fn a_word_that_points_to_a_constant_is_not_the_stack_pointer() {
    // `ops` holds a pointer to a driver's configuration in `.rodata`,
    // then its functions. Rust firmware's `.rodata` holds strings too,
    // and is a section of strings.
    for kind in [SectionKind::ReadOnlyData, SectionKind::ReadOnlyString] {
        let sections = [
            (
                "ops",
                0x800,
                SectionKind::ReadOnlyData,
                vec![0x884, 0x1041, 0x1045, 0x1045],
            ),
            (".rodata", 0x880, kind, vec![1, 2, 3, 4]),
            (
                ".isr",
                0x900,
                SectionKind::ReadOnlyData,
                vec![0x2000_1000, 0x1041, 0x1045, 0x1045],
            ),
            (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
        ];
        // Where the second words of `ops` and `.isr` both hold the
        // entry point, and where none does.
        for entry in [0x1040, 0x1050] {
            let table = find(&firmware(entry, &sections));
            assert_eq!(table.map(|table| table.address), Some(0x900));
        }
    }
}

#[test]
fn a_section_starts_a_table_only_if_its_first_four_words_could() {
    // `.isr` ends where `.text` starts, and `.text` holds the address
    // of code, so that the words after a short `.isr` would do for
    // NMI's and HardFault's.
    let starts = |words: Vec<u32>| {
        let isr = 0x1000 - 4 * words.len() as u64;
        let sections = [
            (".isr", isr, SectionKind::ReadOnlyData, words),
            (".text", 0x1000, SectionKind::Text, vec![0x1041; 32]),
        ];
        find(&firmware(0x1050, &sections)).is_some()
    };
    assert!(starts(vec![0x2000_1000, 0x1041, 0x1041, 0x1041]));
    // The stack pointer isn't a multiple of 4, is 0, or points into
    // code, not into RAM.
    assert!(!starts(vec![0x2000_1002, 0x1041, 0x1041, 0x1041]));
    assert!(!starts(vec![0, 0x1041, 0x1041, 0x1041]));
    assert!(!starts(vec![0x1004, 0x1041, 0x1041, 0x1041]));
    // The reset, NMI or HardFault entry is 0, an even address, or odd
    // but outside code: in `.isr`.
    assert!(!starts(vec![0x2000_1000, 0, 0x1041, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x1040, 0x1041, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x0ff1, 0x1041, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x1040, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x0ff1, 0x1041]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041, 0]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041, 0x1040]));
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041, 0x0ff1]));
    // Too short to hold four words.
    assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041]));
}

#[test]
fn the_first_section_by_address_that_begins_like_a_table_holds_it() {
    // `.ram_vectors` comes first in the file, but `.isr` by address.
    let ram_vectors = vec![0x2000_1000, 0x1045, 0x1045, 0x1045];
    let isr = vec![0x2000_1000, 0x1041, 0x1045, 0x1045];
    let binary = firmware(
        0x1050,
        &[
            (".ram_vectors", 0xa00, SectionKind::Data, ram_vectors),
            (".isr", 0x900, SectionKind::ReadOnlyData, isr),
            (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
        ],
    );
    assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
}

#[test]
fn only_sections_of_code_or_data_begin_a_table() {
    // A Linux program's `.gnu.version`, a list of 16-bit numbers, can
    // begin like a table.
    let versions = vec![0x2000_1000, 0x1041, 0x1041, 0x1041];
    let binary = firmware(
        0x1050,
        &[
            (".gnu.version", 0x800, SectionKind::Unknown, versions),
            (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
        ],
    );
    assert_eq!(find(&binary), None);
}

#[test]
fn sections_that_startup_code_copies_begin_a_table_last() {
    // `.data` comes first by address, but startup code copies it into
    // place, after the processor has read the table at reset. It begins
    // like one: a pointer, then three callbacks.
    let data = vec![0x4018_4000, 0x1041, 0x1045, 0x1045];
    let isr = vec![0x2000_1000, 0x1041, 0x1045, 0x1045];
    let code = vec![0xbf00_bf00; 32];
    let mut binary = firmware(
        0x1050,
        &[
            (".data", 0x700, SectionKind::Data, data.clone()),
            (".isr", 0x900, SectionKind::ReadOnlyData, isr),
            (".text", 0x1000, SectionKind::Text, code.clone()),
        ],
    );
    binary.sections[0].load_address = Some(0x1100);
    assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
    // But when no other section begins like a table, as when a linker
    // script puts the table in RAM, the copy holds it.
    let mut binary = firmware(
        0x1050,
        &[
            (".data", 0x700, SectionKind::Data, data),
            (".text", 0x1000, SectionKind::Text, code),
        ],
    );
    binary.sections[0].load_address = Some(0x1100);
    assert_eq!(find(&binary).map(|table| table.address), Some(0x700));
}

#[test]
fn a_word_that_holds_the_entry_point_where_a_table_starts_holds_its_reset_entry() {
    // `.rodata` begins like a table too, but `.isr`'s reset entry holds
    // the entry point.
    let rodata = vec![0x2000_1000, 0x1045, 0x1045, 0x1045];
    let isr = vec![0x2000_1000, 0x1041, 0, 0x1045];
    let binary = firmware(
        0x1040,
        &[
            (".rodata", 0x800, SectionKind::ReadOnlyData, rodata),
            (".isr", 0x900, SectionKind::ReadOnlyData, isr),
            (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
        ],
    );
    assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
}

#[test]
fn a_word_in_the_middle_of_a_section_holds_the_reset_entry_last() {
    // A pointer to `main`, the entry point, after a pointer to data.
    let rodata = vec![0x1041, 0x1041, 0x800, 0x1051];
    let isr = vec![0x2000_1000, 0x1041, 0x1045, 0x1045];
    let code = vec![0xbf00_bf00; 32];
    let binary = firmware(
        0x1050,
        &[
            (".rodata", 0x800, SectionKind::ReadOnlyData, rodata.clone()),
            (".isr", 0x900, SectionKind::ReadOnlyData, isr),
            (".text", 0x1000, SectionKind::Text, code.clone()),
        ],
    );
    assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
    // When no section begins like a table, the pointer is the reset
    // entry.
    let binary = firmware(
        0x1050,
        &[
            (".rodata", 0x800, SectionKind::ReadOnlyData, rodata),
            (".text", 0x1000, SectionKind::Text, code),
        ],
    );
    assert_eq!(find(&binary).map(|table| table.address), Some(0x808));
}

#[test]
fn the_reset_entry_follows_a_word_that_could_be_the_stack_pointer() {
    // The entry point is NMI's handler, as when a linker script names
    // none and `.text` begins with it. NMI's and HardFault's entries
    // follow odd words, the reset entry's and NMI's, and entry 11
    // follows a reserved entry that holds 0, so none is the reset
    // entry, and the table is where it begins `.isr`.
    let mut isr = vec![0x2000_1000, 0x1045, 0x1041, 0x1041];
    isr.extend([0; 7]);
    isr.push(0x1041);
    let code = vec![0xbf00_bf00; 32];
    let binary = firmware(
        0x1040,
        &[
            (".isr", 0x900, SectionKind::ReadOnlyData, isr.clone()),
            (".text", 0x1000, SectionKind::Text, code.clone()),
        ],
    );
    let table = find(&binary).unwrap();
    assert_eq!(
        (table.address, table.reset_handler()),
        (0x900, Some(0x1044))
    );
    // Where the table doesn't begin its section, elfq finds none,
    // rather than one that starts a word late.
    isr.insert(0, 1);
    let binary = firmware(
        0x1040,
        &[
            (".rodata", 0x8fc, SectionKind::ReadOnlyData, isr),
            (".text", 0x1000, SectionKind::Text, code),
        ],
    );
    assert_eq!(find(&binary), None);
}

#[test]
fn the_reset_handler_is_the_code_its_entry_holds() {
    let table = |handlers| VectorTable {
        address: 0,
        size: 16,
        handlers,
    };
    let reset = table(vec![handler(1, 0x100), handler(3, 0x200)]);
    assert_eq!(reset.reset_handler(), Some(0x100));
    // The reset entry holds no address of code, as when it is a word
    // that holds the address of an entry point outside code.
    let no_reset = table(vec![handler(2, 0x100), handler(3, 0x200)]);
    assert_eq!(no_reset.reset_handler(), None);
}

#[test]
fn a_handler_runs_in_normal_operation_when_an_entry_for_it_is_routine() {
    let table = VectorTable {
        address: 0,
        size: 128,
        handlers: vec![
            handler(1, 0x100),
            handler(2, 0x200),
            handler(3, 0x200),
            handler(4, 0x300),
            handler(12, 0x300),
            handler(11, 0x400),
            handler(6, 0x500),
            handler(20, 0x500),
        ],
    };
    // SVCall, and an interrupt with a fault.
    assert!(table.runs_in_normal_operation(0x400));
    assert!(table.runs_in_normal_operation(0x500));
    // Reset; NMI and HardFault; MemManage and DebugMonitor.
    assert!(!table.runs_in_normal_operation(0x100));
    assert!(!table.runs_in_normal_operation(0x200));
    assert!(!table.runs_in_normal_operation(0x300));
    // No entry lists it.
    assert!(!table.runs_in_normal_operation(0x600));
}

#[test]
fn exceptions_are_named_by_their_entries() {
    let names = |version: Option<u64>| {
        let architecture = version.map(|version| ArmArchitecture {
            version,
            profile: Some(b'M'),
        });
        (0..18)
            .map(|number| exception_name(number, architecture))
            .collect::<Vec<String>>()
    };
    // Armv7E-M, as when the file doesn't say.
    let armv7 = [
        "initial SP",
        "Reset",
        "NMI",
        "HardFault",
        "MemManage",
        "BusFault",
        "UsageFault",
        "reserved",
        "reserved",
        "reserved",
        "reserved",
        "SVCall",
        "DebugMonitor",
        "reserved",
        "PendSV",
        "SysTick",
        "IRQ 0",
        "IRQ 1",
    ];
    assert_eq!(names(Some(13)), armv7);
    assert_eq!(names(None), armv7);
    // Armv6S-M, as on a Cortex-M0, has no faults but HardFault, and no
    // DebugMonitor.
    let mut armv6 = armv7;
    for number in [4, 5, 6, 12] {
        armv6[number] = "reserved";
    }
    assert_eq!(names(Some(12)), armv6);
    // Armv8-M Mainline, as on a Cortex-M33, adds SecureFault.
    let mut armv8 = armv7;
    armv8[7] = "SecureFault";
    assert_eq!(names(Some(17)), armv8);
}
