//! Damaged and unusual files, made by changing the bytes of a sample:
//! headers that don't add up, huge sections, and calls or debug info nested
//! far deeper than compilers make them. elfq must still answer quickly,
//! with what the file does hold. The tests run in a debug build, so
//! arithmetic that overflows fails them too.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use elfq::binary::Binary;
use elfq::graph::{Graph, ReferenceKind};
use object::{Object, ObjectSection, ObjectSymbol};

use crate::support::{assert_output, elfq, load_sample, sample_path};

/// Where a field is in a section header: in a 64-bit file, and in a
/// 32-bit one.
type Field = (usize, usize);

/// `sh_addr`, the section's address.
const ADDRESS: Field = (0x10, 0x0c);
/// `sh_offset`, where the section's bytes are in the file.
const OFFSET: Field = (0x18, 0x10);
/// `sh_size`, the section's size.
const SIZE: Field = (0x20, 0x14);

/// The sample `name`, as bytes to change.
fn sample(name: &str) -> Vec<u8> {
    std::fs::read(sample_path(name)).unwrap()
}

/// Writes `value` over the `width` bytes at `at`. The samples are
/// little-endian.
fn write(data: &mut [u8], at: usize, width: usize, value: u64) {
    data[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

/// Sets `field` in the header of `section` to `value`.
fn set_section_field(data: &mut [u8], section: &str, field: Field, value: u64) {
    let file = object::File::parse(&*data).unwrap();
    let index = file.section_by_name(section).unwrap().index().0;
    let is_64 = file.is_64();
    let number = |at: usize, width: usize| {
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&data[at..at + width]);
        u64::from_le_bytes(bytes) as usize
    };
    // `e_shoff` and `e_shentsize` in the file's header, which say where the
    // section headers are.
    let (headers, header_size) = if is_64 {
        (number(0x28, 8), number(0x3a, 2))
    } else {
        (number(0x20, 4), number(0x2e, 2))
    };
    let (at, width) = if is_64 { (field.0, 8) } else { (field.1, 4) };
    write(data, headers + index * header_size + at, width, value);
}

/// Sets the size that `.symtab` gives `symbol` to `size`, in a 64-bit file.
fn set_symbol_size(data: &mut [u8], symbol: &str, size: u64) {
    let file = object::File::parse(&*data).unwrap();
    let index = file.symbol_by_name(symbol).unwrap().index().0;
    let (table, _) = file
        .section_by_name(".symtab")
        .unwrap()
        .file_range()
        .unwrap();
    // Each symbol takes 24 bytes, and `st_size` is the last 8.
    write(data, table as usize + index * 24 + 16, 8, size);
}

/// Puts `bytes` at the end of the file and points `section` at them.
fn replace_section(data: &mut Vec<u8>, section: &str, bytes: &[u8]) {
    let offset = data.len() as u64;
    data.extend_from_slice(bytes);
    set_section_field(data, section, OFFSET, offset);
    set_section_field(data, section, SIZE, bytes.len() as u64);
}

/// `bytes` after their length in 4 bytes, as DWARF starts a unit or a
/// list of ranges.
fn with_length(bytes: &[u8]) -> Vec<u8> {
    let mut all = (bytes.len() as u32).to_le_bytes().to_vec();
    all.extend(bytes);
    all
}

fn parse(data: &[u8]) -> (Binary, Graph) {
    let binary = Binary::parse(data).unwrap_or_else(|err| panic!("{err:#}"));
    let graph = Graph::build(&binary);
    (binary, graph)
}

/// [`parse`], failing after 10 seconds: reading a huge section one word at
/// a time took minutes in a debug build, with the same answer in the end.
fn parse_quickly(data: Vec<u8>) -> (Binary, Graph) {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        // Sending fails only once the test has stopped waiting.
        let _ = send.send(parse(&data));
    });
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("no answer within 10 seconds")
}

/// Saves `data` as `name` in a folder for the tests' files, and returns its
/// path.
fn save(name: &str, data: &[u8]) -> String {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, data).unwrap();
    path.to_str().unwrap().to_string()
}

/// Runs elfq with `args` on `data`, saved as `name`, and returns what it
/// printed to stdout and stderr. It must have answered, with exit status 0.
fn run(name: &str, data: &[u8], args: &[&str]) -> (String, String) {
    let output = elfq(&[&[save(name, data).as_str()], args].concat());
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    let (stdout, stderr) = (text(output.stdout), text(output.stderr));
    assert!(output.status.success(), "{name}: {stderr}");
    (stdout, stderr)
}

#[test]
fn a_section_larger_than_the_file_is_read_as_far_as_the_file_goes() {
    // What the exit array lists is still found, along with whatever the
    // rest of the file holds, which the array now seems to run into.
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".fini_array", SIZE, 1 << 40);
    let (_, graph) = parse_quickly(data);
    let roots = Graph::build(&load_sample("c-x86_64.elf")).roots;
    assert!(roots.iter().all(|root| graph.roots.contains(root)));

    let mut data = sample("c-cortex-m4-stripped.elf");
    set_section_field(&mut data, ".isr_vector", SIZE, 0xa400_0040);
    let (binary, _) = parse_quickly(data);
    let vector_table = load_sample("c-cortex-m4-stripped.elf").vector_table;
    assert_eq!(binary.vector_table, vector_table);
}

#[test]
fn a_huge_bss_holds_no_pointers() {
    // `.bss` takes no space in the file, however large it is, so there are
    // no pointers in it to read.
    let pointers = |graph: Graph| {
        let references = graph.references.into_iter();
        references
            .filter(|reference| reference.kind == ReferenceKind::Pointer)
            .collect::<Vec<_>>()
    };
    let mut data = sample("c-cortex-m4.elf");
    set_section_field(&mut data, ".bss", SIZE, 0xffff_0000);
    let (_, graph) = parse_quickly(data);
    assert_eq!(
        pointers(graph),
        pointers(Graph::build(&load_sample("c-cortex-m4.elf")))
    );
}

#[test]
fn words_are_read_from_the_section_that_holds_each() {
    // Sections can overlap, as GNU ld's OVERLAY makes them. Here
    // `.isr_vector` keeps only the vector table's first 8 bytes, and
    // `.ARM.exidx`, later in the table, is moved to hold all 64 of them,
    // from the same bytes of the file. The handlers listed past the first 8
    // bytes come from `.ARM.exidx`.
    let mut data = sample("c-cortex-m4.elf");
    set_section_field(&mut data, ".isr_vector", SIZE, 8);
    set_section_field(&mut data, ".ARM.exidx", ADDRESS, 0x0800_0000);
    set_section_field(&mut data, ".ARM.exidx", OFFSET, 0x1000);
    set_section_field(&mut data, ".ARM.exidx", SIZE, 0x40);
    let in_table = |graph: Graph| {
        let references = graph.references.into_iter();
        references
            .filter(|reference| reference.kind == ReferenceKind::Pointer)
            .filter(|reference| (0x0800_0000..0x0800_0040).contains(&reference.site))
            .collect::<Vec<_>>()
    };
    let (_, graph) = parse(&data);
    let table = in_table(Graph::build(&load_sample("c-cortex-m4.elf")));
    assert_eq!(table.len(), 10);
    assert_eq!(in_table(graph), table);
}

#[test]
fn words_come_from_a_section_that_starts_inside_another() {
    // `.ARM.exidx` is moved to hold the vector table's 64 bytes, and
    // `.isr_vector`, listed before it, to start halfway through them and
    // hold bytes of `.text`. From there on the words are `.isr_vector`'s,
    // as when each is read on its own.
    let mut data = sample("c-cortex-m4-stripped.elf");
    set_section_field(&mut data, ".ARM.exidx", ADDRESS, 0x0800_0000);
    set_section_field(&mut data, ".ARM.exidx", OFFSET, 0x1000);
    set_section_field(&mut data, ".ARM.exidx", SIZE, 0x40);
    set_section_field(&mut data, ".isr_vector", ADDRESS, 0x0800_0020);
    set_section_field(&mut data, ".isr_vector", OFFSET, 0x1040);
    set_section_field(&mut data, ".isr_vector", SIZE, 0x20);
    let (binary, _) = parse(&data);
    let one_at_a_time: Vec<(u64, u64)> = (0x0800_0000..0x0800_0040)
        .step_by(4)
        .map(|address| (address, binary.pointer_at(address).unwrap()))
        .collect();
    let words: Vec<(u64, u64)> = binary.words(0x0800_0000, 0x0800_0040).collect();
    assert_eq!(words, one_at_a_time);
    let table = load_sample("c-cortex-m4-stripped.elf");
    assert_ne!(Some(words[8].1), table.pointer_at(0x0800_0020));
}

#[test]
fn the_vector_table_ends_where_the_file_holds_no_bytes() {
    // `.isr_vector` now covers 8 bytes in the middle of the table, which
    // the file doesn't hold, and `.ARM.exidx` the whole table. The table
    // ends before them, though the file holds the entries after them.
    let mut data = sample("c-cortex-m4-stripped.elf");
    set_section_field(&mut data, ".isr_vector", ADDRESS, 0x0800_0010);
    set_section_field(&mut data, ".isr_vector", OFFSET, 0x10_0000);
    set_section_field(&mut data, ".isr_vector", SIZE, 8);
    set_section_field(&mut data, ".ARM.exidx", ADDRESS, 0x0800_0000);
    set_section_field(&mut data, ".ARM.exidx", OFFSET, 0x1000);
    set_section_field(&mut data, ".ARM.exidx", SIZE, 0x40);
    let (binary, _) = parse(&data);
    let table = binary.vector_table.unwrap();
    assert_eq!((table.address, table.size), (0x0800_0000, 0x10));
    assert_eq!(table.handlers, [0x0800_05a0, 0x0800_059c, 0x0800_059c]);
}

#[test]
fn the_search_for_the_vector_table_skips_a_huge_bss() {
    // With an entry point that no vector table lists, elfq looks for one
    // in every section, `.bss` too, and finds none.
    let mut data = sample("c-cortex-m4.elf");
    // `e_entry`, in the file's header.
    write(&mut data, 0x18, 4, 0x0800_0001);
    set_section_field(&mut data, ".bss", SIZE, 0xffff_0000);
    let (binary, _) = parse_quickly(data);
    assert_eq!(binary.vector_table, None);
}

#[test]
fn a_section_that_would_end_past_the_last_address_is_ignored() {
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".fini_array", SIZE, u64::MAX - 15);
    let (_, stderr) = run("fini-array-size.elf", &data, &["top", "--all"]);
    assert_output(
        &stderr,
        "warning: ignoring section .fini_array: it runs past the last address\n",
    );
}

#[test]
fn a_section_that_ends_at_the_last_address_is_kept() {
    // Its last byte is at the last address, so its end is one past it,
    // which a u64 can't hold: the section loses that byte instead.
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".fini_array", ADDRESS, u64::MAX - 7);
    let (_, stderr) = run("fini-array-at-the-top.elf", &data, &["top", "--all"]);
    assert_output(&stderr, "");
    let (binary, _) = parse(&data);
    let fini = binary.sections.iter().find(|s| s.name == ".fini_array");
    let fini = fini.unwrap();
    assert!(fini.allocated && fini.size == 7);
}

#[test]
fn a_section_that_isnt_allocated_keeps_its_addresses() {
    // elfq uses no addresses from it, so they can't be damaged, and the
    // inlined copies its debug info lists are still found.
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".debug_info", ADDRESS, u64::MAX - 15);
    let (stdout, stderr) = run("debug-info-address.elf", &data, &["inlined"]);
    assert_output(&stderr, "");
    let (expected, _) = run("c-x86_64.elf", &sample("c-x86_64.elf"), &["inlined"]);
    assert_output(&stdout, &expected);
}

#[test]
fn a_section_that_runs_past_the_end_of_the_file_keeps_its_addresses() {
    // Only where its bytes are is damaged, so the section is still there,
    // and the file holds none of its bytes.
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".plt", OFFSET, u64::MAX - 15);
    let (_, stderr) = run("plt-offset.elf", &data, &["top", "--all"]);
    assert_output(
        &stderr,
        "warning: section .plt runs past the end of the file\n",
    );
    let (binary, _) = parse(&data);
    let plt = binary.sections.iter().find(|s| s.name == ".plt").unwrap();
    assert!(plt.allocated && plt.size > 0);
    assert_eq!(binary.bytes(plt.address, 1), None);
}

#[test]
fn a_function_whose_size_would_end_past_the_last_address_gets_another() {
    let mut data = sample("c-x86_64.elf");
    set_symbol_size(&mut data, "main", u64::MAX);
    let (binary, _) = parse(&data);
    let main = binary
        .items
        .iter()
        .find(|item| item.name == "main")
        .unwrap();
    // The unwind table gives it, as it does when a symbol has no size.
    assert_eq!((main.size, main.size_estimated), (195, false));
}

#[test]
fn a_data_object_whose_size_would_end_past_the_last_address_is_dropped() {
    // As one without a size is: what code refers to there becomes unnamed
    // data, sized up to the next item.
    let mut data = sample("c-x86_64.elf");
    set_symbol_size(&mut data, "handlers", u64::MAX);
    let (binary, _) = parse(&data);
    assert!(binary.items.iter().all(|item| item.name != "handlers"));
    let unnamed = binary.items.iter().find(|item| item.address == 0x4070);
    let unnamed = unnamed.unwrap();
    assert_eq!(
        (unnamed.name.as_str(), unnamed.size, unnamed.size_estimated),
        (".bss@0x4070", 24, true)
    );
}

#[test]
fn sizes_that_add_up_past_the_largest_number_stop_there() {
    // Functions that overlap, each half as large as the address space.
    let mut data = sample("c-x86_64.elf");
    for function in ["on_timer", "on_data", "crc32"] {
        set_symbol_size(&mut data, function, 1 << 63);
    }
    let (stdout, _) = run("huge-functions.elf", &data, &[]);
    let total = stdout.lines().nth(1).unwrap();
    assert!(
        total.contains(&format!(", {} bytes of", u64::MAX)),
        "{total}"
    );
    run("huge-functions.elf", &data, &["top", "--all"]);
    run("huge-functions.elf", &data, &["inlined"]);
}

#[test]
fn a_64_bit_file_marked_as_arm_is_not_read_as_thumb() {
    // Thumb code only comes in 32-bit files, whatever the entry point says.
    let mut data = sample("c-x86_64.elf");
    // `e_machine` and `e_entry`, in the file's header: EM_ARM, and an odd
    // entry point, as Thumb code has.
    write(&mut data, 0x12, 2, 40);
    let entry = load_sample("c-x86_64.elf").entry;
    write(&mut data, 0x18, 8, entry | 1);
    let (binary, _) = parse(&data);
    assert!(!binary.thumb);
}

#[test]
fn inlined_copies_that_add_up_past_the_largest_number_stop_there() {
    // Two inlined copies of one function, whose two ranges each cover
    // nearly the whole address space. Abbreviation 1 is an inlined copy
    // without children, with its ranges at an offset in
    // `.debug_rnglists`, and 2 a compile unit with children and no
    // attributes.
    let abbrev = [1, 0x1d, 0, 0x55, 0x17, 0, 0, 2, 0x11, 1, 0, 0, 0];
    // DWARF 5, a compile unit, 8-byte addresses, abbreviations at offset
    // 0. Then the compile unit, the two copies with their ranges at offset
    // 12, past the list's length and header, and the end of the unit's
    // children.
    let mut unit = vec![5, 0, 1, 8, 0, 0, 0, 0, 2];
    for _ in 0..2 {
        unit.push(1);
        unit.extend(12u32.to_le_bytes());
    }
    unit.push(0);
    // DWARF 5, 8-byte addresses, no segments and no offsets. Then each
    // range as its start and end (DW_RLE_start_end), and the end of the
    // list.
    let mut ranges = vec![5, 0, 8, 0, 0, 0, 0, 0];
    for _ in 0..2 {
        ranges.push(6);
        ranges.extend(0x1000u64.to_le_bytes());
        ranges.extend(0xffff_ffff_ffff_fff0u64.to_le_bytes());
    }
    ranges.push(0);

    let mut data = sample("c-x86_64.elf");
    replace_section(&mut data, ".debug_info", &with_length(&unit));
    replace_section(&mut data, ".debug_abbrev", &abbrev);
    replace_section(&mut data, ".debug_rnglists", &with_length(&ranges));
    let (stdout, _) = run("huge-inlined-copies.elf", &data, &["inlined"]);
    assert_output(
        &stdout,
        "\
Inlined copies of 1 function take 18446744073709551615 of the 2557 bytes of code:

               BYTES  COPIES  FUNCTION
18446744073709551615       2  (unnamed)
",
    );
    let (stdout, _) = run("huge-inlined-copies.elf", &data, &["inlined", "_init"]);
    assert_output(
        &stdout,
        "\
18446744073709551615 bytes of _init's code are inlined copies of 1 function:

               BYTES  COPIES  FUNCTION
18446744073709551615       2  (unnamed)
",
    );
}

#[test]
fn an_inlined_copy_that_would_end_past_the_last_address_has_no_bytes() {
    // Its DW_AT_high_pc gives a size that, added to its DW_AT_low_pc, ends
    // past the last address. Abbreviation 1 is an inlined copy without
    // children, with a DW_AT_low_pc address and a DW_AT_high_pc size of 8
    // bytes, and 2 a compile unit with children and no attributes.
    let abbrev = [
        1, 0x1d, 0, 0x11, 0x01, 0x12, 0x07, 0, 0, 2, 0x11, 1, 0, 0, 0,
    ];
    let items = load_sample("c-x86_64.elf").items;
    let main = items.iter().find(|item| item.name == "main").unwrap();
    // DWARF 4, abbreviations at offset 0, 8-byte addresses. Then the
    // compile unit, the copy in main, and the end of the unit's children.
    let mut unit = vec![4, 0, 0, 0, 0, 0, 8, 2, 1];
    unit.extend(main.address.to_le_bytes());
    unit.extend(0xffff_ffff_ffff_ff00u64.to_le_bytes());
    unit.push(0);

    let mut data = sample("c-x86_64.elf");
    replace_section(&mut data, ".debug_info", &with_length(&unit));
    replace_section(&mut data, ".debug_abbrev", &abbrev);
    // As in a release build, the range is dropped, and the copy is found at
    // its DW_AT_low_pc, without bytes.
    let (stdout, _) = run("huge-high-pc.elf", &data, &["inlined"]);
    assert_output(
        &stdout,
        "\
Inlined copies of 1 function take 0 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
    0       1  (unnamed)

A copy of 0 bytes is one the compiler merged into the code around it, so the debug info gives it no bytes of its own.
",
    );
}

#[test]
fn each_dwarf_unit_is_read_to_the_end_of_its_root() {
    // A unit holds its root entry, the entries below it, and here a byte
    // after them that names no abbreviation. Abbreviation 1 is an inlined
    // copy without children, at the address its DW_AT_low_pc gives, and 2 a
    // compile unit with children and no attributes.
    let abbrev = [1, 0x1d, 0, 0x11, 0x01, 0, 0, 2, 0x11, 1, 0, 0, 0];
    let items = load_sample("c-x86_64.elf").items;
    let main = items.iter().find(|item| item.name == "main").unwrap();
    // The compile unit, a copy in main, and the end of the unit's children.
    let mut entries = vec![2, 1];
    entries.extend(main.address.to_le_bytes());
    entries.push(0);
    let expected = "\
Inlined copies of 1 function take 0 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
    0       1  (unnamed)

A copy of 0 bytes is one the compiler merged into the code around it, so the debug info gives it no bytes of its own.
";
    // DWARF 4, abbreviations at offset 0, 8-byte addresses, then the
    // entries, with the byte after them, or, as gimli also reads it, with a
    // null entry before the root.
    for (name, entries) in [
        ("dwarf-trailing-byte.elf", [&entries[..], &[7]].concat()),
        ("dwarf-leading-null.elf", [&[0], &entries[..]].concat()),
    ] {
        let unit = [&[4, 0, 0, 0, 0, 0, 8], &entries[..]].concat();
        let mut data = sample("c-x86_64.elf");
        replace_section(&mut data, ".debug_info", &with_length(&unit));
        replace_section(&mut data, ".debug_abbrev", &abbrev);
        let (stdout, _) = run(name, &data, &["inlined"]);
        assert_output(&stdout, expected);
    }
}

#[test]
fn dwarf_that_nests_deeply_is_read() {
    // One compile unit, with lexical blocks nested a million deep: far
    // deeper than a walk that recursed would have stack for.
    let depth = 1_000_000;
    // Abbreviation 1 is a lexical block and 2 a compile unit, each with
    // children and no attributes.
    let abbrev = [1, 0x0b, 1, 0, 0, 2, 0x11, 1, 0, 0, 0];
    // DWARF 4, abbreviations at offset 0, 8-byte addresses. Then the
    // compile unit, the blocks, and the end of each one's children.
    let mut unit = vec![4, 0, 0, 0, 0, 0, 8, 2];
    unit.extend(std::iter::repeat_n(1, depth));
    unit.extend(std::iter::repeat_n(0, depth + 1));

    let mut data = sample("c-x86_64.elf");
    replace_section(&mut data, ".debug_info", &with_length(&unit));
    replace_section(&mut data, ".debug_abbrev", &abbrev);
    let (stdout, _) = run("deep-dwarf.elf", &data, &["inlined"]);
    assert_output(&stdout, "The debug info lists no inlined code.\n");
}

#[test]
fn inlined_copies_that_nest_deeply_are_summed_up() {
    // Copies inlined into main, each into the one before, 100,000 deep:
    // all but the first few are summed up in one row.
    let depth = 100_000;
    let items = load_sample("c-x86_64.elf").items;
    let main = items.iter().find(|item| item.name == "main").unwrap();
    // Abbreviation 1 is an inlined copy with children, at the address its
    // DW_AT_low_pc gives, and 2 a compile unit with children and no
    // attributes.
    let abbrev = [1, 0x1d, 1, 0x11, 0x01, 0, 0, 2, 0x11, 1, 0, 0, 0];
    // DWARF 4, abbreviations at offset 0, 8-byte addresses. Then the
    // compile unit, the copies, and the end of each one's children.
    let mut unit = vec![4, 0, 0, 0, 0, 0, 8, 2];
    for _ in 0..depth {
        unit.push(1);
        unit.extend(main.address.to_le_bytes());
    }
    unit.extend(std::iter::repeat_n(0, depth + 1));

    let mut data = sample("c-x86_64.elf");
    replace_section(&mut data, ".debug_info", &with_length(&unit));
    replace_section(&mut data, ".debug_abbrev", &abbrev);
    let (stdout, _) = run("deep-inlined-copies.elf", &data, &["inlined", "main"]);
    assert!(stdout.contains("└─ 99970 more\n"), "{stdout}");
}
