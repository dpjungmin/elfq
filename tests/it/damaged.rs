//! Damaged and unusual files, made by changing the bytes of a sample:
//! headers that don't add up, huge sections, and calls or debug info nested
//! far deeper than compilers make them. elfq must still answer quickly,
//! with what the file does hold. The tests run in a debug build, so
//! arithmetic that overflows fails them too.

use std::sync::mpsc;
use std::time::Duration;

use elfq::binary::Binary;
use elfq::graph::{Graph, ReferenceKind};
use object::{LittleEndian, elf};

use crate::support::elf::{
    ElfBytes, HeaderField, SectionField, chain_of_calls, for_arm_version, past_the_end,
};
use crate::support::{SAMPLE, TCM_SAMPLE, assert_output, load_sample, run};

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

/// [`parse`], failing after 10 seconds, for files that once took minutes in
/// a debug build, such as a huge section read one word at a time.
fn parse_quickly(data: ElfBytes) -> (Binary, Graph) {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        // Sending fails only once the test has stopped waiting.
        let _ = send.send(parse(&data));
    });
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("no answer within 10 seconds")
}

#[test]
fn the_addend_of_a_pointer_that_a_plt_stub_jumps_through_is_ignored() {
    // glibc's own libc.so.6 gives each of these pointers an address in its
    // stub as the addend, which the dynamic loader ignores. The stubs in
    // `.plt` jump through JUMP_SLOT pointers, listed in `.rela.plt`, and
    // those in `.plt.got` through GLOB_DAT ones, listed in `.rela.dyn`.
    let mut data = ElfBytes::sample("testdata/bin/c-plt.so");
    for table in [".rela.plt", ".rela.dyn"] {
        for relocation in data.relocations_mut(table) {
            let r_type = relocation.r_type(LittleEndian, false);
            if [elf::R_X86_64_JUMP_SLOT, elf::R_X86_64_GLOB_DAT].contains(&r_type) {
                relocation.r_addend.set(LittleEndian, 0x1064);
            }
        }
    }
    let (binary, _) = parse(&data);
    let stubs = load_sample("testdata/bin/c-plt.so").own_function_stubs();
    assert_eq!(stubs.len(), 6);
    assert_eq!(binary.own_function_stubs(), stubs);
}

#[test]
fn a_section_larger_than_the_file_is_read_as_far_as_the_file_goes() {
    // What the exit array lists is still found, along with whatever the
    // rest of the file holds, which the array now seems to run into.
    let mut data = ElfBytes::sample(SAMPLE);
    data.set_section(".fini_array", SectionField::Size, 1 << 40);
    let (_, graph) = parse_quickly(data);
    let roots = Graph::build(&load_sample(SAMPLE)).roots;
    assert!(roots.iter().all(|root| graph.roots.contains(root)));

    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-stripped.elf");
    data.set_section(".isr_vector", SectionField::Size, 0xa400_0040);
    let (binary, _) = parse_quickly(data);
    let vector_table = load_sample("testdata/bin/c-cortex-m4-stripped.elf").vector_table;
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
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4.elf");
    data.set_section(".bss", SectionField::Size, 0xffff_0000);
    let (_, graph) = parse_quickly(data);
    assert_eq!(
        pointers(graph),
        pointers(Graph::build(&load_sample("testdata/bin/c-cortex-m4.elf")))
    );
}

#[test]
fn words_are_read_from_the_section_that_holds_each() {
    // Sections can overlap, as GNU ld's OVERLAY makes them. Here
    // `.isr_vector` keeps only the vector table's first 8 bytes, and
    // `.ARM.exidx`, later in the table, is moved to hold all 64 of them,
    // from the same bytes of the file. The handlers listed past the first 8
    // bytes come from `.ARM.exidx`.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4.elf");
    data.set_section(".isr_vector", SectionField::Size, 8)
        .set_section(".ARM.exidx", SectionField::Address, 0x0800_0000)
        .set_section(".ARM.exidx", SectionField::Offset, 0x1000)
        .set_section(".ARM.exidx", SectionField::Size, 0x40);
    let in_table = |graph: Graph| {
        let references = graph.references.into_iter();
        references
            .filter(|reference| reference.kind == ReferenceKind::Pointer)
            .filter(|reference| (0x0800_0000..0x0800_0040).contains(&reference.site))
            .collect::<Vec<_>>()
    };
    let (_, graph) = parse(&data);
    let table = in_table(Graph::build(&load_sample("testdata/bin/c-cortex-m4.elf")));
    assert_eq!(table.len(), 10);
    assert_eq!(in_table(graph), table);
}

#[test]
fn words_come_from_a_section_that_starts_inside_another() {
    // `.ARM.exidx` is moved to hold the vector table's 64 bytes, and
    // `.isr_vector`, listed before it, to start halfway through them and
    // hold bytes of `.text`. From there on the words are `.isr_vector`'s,
    // as when each is read on its own.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-stripped.elf");
    data.set_section(".ARM.exidx", SectionField::Address, 0x0800_0000)
        .set_section(".ARM.exidx", SectionField::Offset, 0x1000)
        .set_section(".ARM.exidx", SectionField::Size, 0x40)
        .set_section(".isr_vector", SectionField::Address, 0x0800_0020)
        .set_section(".isr_vector", SectionField::Offset, 0x1040)
        .set_section(".isr_vector", SectionField::Size, 0x20);
    let (binary, _) = parse(&data);
    let one_at_a_time: Vec<(u64, u64)> = (0x0800_0000..0x0800_0040)
        .step_by(4)
        .map(|address| (address, binary.pointer_at(address).unwrap()))
        .collect();
    let words: Vec<(u64, u64)> = binary.words(0x0800_0000, 0x0800_0040).collect();
    assert_eq!(words, one_at_a_time);
    let table = load_sample("testdata/bin/c-cortex-m4-stripped.elf");
    assert_ne!(Some(words[8].1), table.pointer_at(0x0800_0020));
}

#[test]
fn the_vector_table_ends_where_the_file_holds_no_bytes() {
    // `.isr_vector` now covers 8 bytes in the middle of the table, which
    // the file doesn't hold, and `.rodata` the whole table. The table ends
    // before them, though the file holds the entries after them.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-stripped.elf");
    data.set_section(".isr_vector", SectionField::Address, 0x0800_0010)
        .set_section(".isr_vector", SectionField::Offset, 0x10_0000)
        .set_section(".isr_vector", SectionField::Size, 8)
        .set_section(".rodata", SectionField::Address, 0x0800_0000)
        .set_section(".rodata", SectionField::Offset, 0x1000)
        .set_section(".rodata", SectionField::Size, 0x40);
    let (binary, _) = parse(&data);
    let table = binary.vector_table.unwrap();
    assert_eq!((table.address, table.size), (0x0800_0000, 0x10));
    let handlers: Vec<(u64, u64)> = (table.handlers.iter())
        .map(|handler| (handler.exception, handler.address))
        .collect();
    assert_eq!(
        handlers,
        [(1, 0x0800_05a0), (2, 0x0800_059c), (3, 0x0800_059c)]
    );
}

#[test]
fn the_search_for_the_vector_table_skips_a_huge_bss() {
    // With an entry point that no vector table lists, elfq looks for one
    // in every section, `.bss` too, and finds none. It then finds the table
    // at the start of `.isr_vector`, by how the table begins.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4.elf");
    data.set(HeaderField::EntryPoint, 0x0800_0001);
    data.set_section(".bss", SectionField::Size, 0xffff_0000);
    let (binary, _) = parse_quickly(data);
    let table = binary.vector_table.unwrap();
    assert_eq!(table.address, 0x0800_0000);
    assert_eq!(table.reset_handler(), Some(0x0800_05a0));
}

#[test]
fn stripped_cortex_m_firmware_is_thumb_though_its_entry_point_is_even() {
    // Linked with no entry point, firmware's entry point is where `.text`
    // starts, an even address: here on_timer's, as ld sets it. Stripped,
    // it has no symbols either, but its build attributes name a Cortex-M
    // core.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m7-entry-main-stripped.elf");
    data.set(HeaderField::EntryPoint, 0x3000_0140);
    let (binary, _) = parse(&data);
    assert!(binary.thumb);
    let table = binary.vector_table.unwrap();
    assert_eq!(table.reset_handler(), Some(0x3000_05f8));
}

#[test]
fn an_entry_point_in_the_vector_table_is_no_function() {
    // Linked with no entry point, firmware whose `.text` begins with the
    // vector table has its entry point there. The table is data all the
    // same: no function starts there, and nothing reads it as code.
    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-cmsis-stripped.elf");
    data.set(HeaderField::EntryPoint, 0x0800_0000);
    let (binary, _) = parse(&data);
    let table = binary.vector_table.clone().unwrap();
    assert_eq!(
        (table.address, table.reset_handler()),
        (0x0800_0000, Some(0x0800_05a0))
    );
    let item = &binary.items[binary.item_at(0x0800_0000).unwrap()];
    assert_eq!(
        (item.name.as_str(), item.size),
        (".text@0x8000000", table.size)
    );
    let path = data.save("entry-in-table.elf");
    let stdout = run(&[&path, "reach"]).printed(0);
    assert!(!stdout.contains("entry point"), "{stdout}");
    assert!(stdout.contains("Reset "), "{stdout}");
    // It is a root as the vector table, not as the entry point.
    let stdout = run(&[&path, "why", ".text@0x8000000"]).printed(0);
    assert_output(
        &stdout,
        ".text@0x8000000 holds the vector table, where the processor finds its handlers.\n",
    );
}

#[test]
fn a_section_that_would_end_past_the_last_address_is_ignored() {
    let path = ElfBytes::sample(SAMPLE)
        .set_section(".fini_array", SectionField::Size, u64::MAX - 15)
        .save("fini-array-size.elf");
    let top = run(&[&path, "top", "--all"]).exited(0);
    assert_output(
        &top.stderr,
        "warning: ignoring section .fini_array: it runs past the last address\n",
    );
}

#[test]
fn a_section_that_ends_at_the_last_address_is_kept() {
    // Its last byte is at the last address, so its end is one past it,
    // which a u64 can't hold: the section loses that byte instead.
    let mut data = ElfBytes::sample(SAMPLE);
    data.set_section(".fini_array", SectionField::Address, u64::MAX - 7);
    // elfq doesn't warn about it.
    run(&[&data.save("fini-array-at-the-top.elf"), "top", "--all"]).printed(0);
    let (binary, _) = parse(&data);
    let fini = binary.sections.iter().find(|s| s.name == ".fini_array");
    let fini = fini.unwrap();
    assert!(fini.allocated && fini.size == 7);
}

#[test]
fn a_section_that_isnt_allocated_keeps_its_addresses() {
    // elfq uses no addresses from it, so they can't be damaged, and the
    // inlined copies its debug info lists are still found.
    let path = ElfBytes::sample(SAMPLE)
        .set_section(".debug_info", SectionField::Address, u64::MAX - 15)
        .save("debug-info-address.elf");
    let damaged = run(&[&path, "inlined"]).printed(0);
    assert_output(&damaged, &run(&[SAMPLE, "inlined"]).printed(0));
}

#[test]
fn a_section_that_runs_past_the_end_of_the_file_keeps_its_addresses() {
    // Only where its bytes are is damaged, so the section is still there,
    // and the file holds none of its bytes.
    let data = past_the_end(&[".plt"]);
    let top = run(&[&data.save("plt-offset.elf"), "top", "--all"]).exited(0);
    assert_output(
        &top.stderr,
        "warning: section .plt runs past the end of the file\n",
    );
    let (binary, _) = parse(&data);
    let plt = binary.sections.iter().find(|s| s.name == ".plt").unwrap();
    assert!(plt.allocated && plt.size > 0);
    assert_eq!(binary.bytes(plt.address, 1), None);
}

#[test]
fn a_function_whose_size_would_end_past_the_last_address_gets_another() {
    let mut data = ElfBytes::sample(SAMPLE);
    data.set_symbol_size("main", u64::MAX);
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
    let mut data = ElfBytes::sample(SAMPLE);
    data.set_symbol_size("handlers", u64::MAX);
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
fn a_function_of_unknown_size_is_named_where_its_load_image_is() {
    // The last function in .itcm, which flash stores from 0x30000118 for
    // the startup code to copy, without a size.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_symbol_size("__memcpy_veneer", 0)
        .save("memcpy-veneer-unsized.elf");
    assert_output(
        &run(&[&path, "top", "--range", "0x30000118+8"]).error(),
        "error: elfq doesn't know where __memcpy_veneer ends, so it can't count the bytes in \
         0x30000118..0x30000120 (the load image of .itcm)\n",
    );
}

#[test]
fn reach_names_what_it_reaches_but_cant_read() {
    // apply_filter calls memcpy through __memcpy_veneer, which has no size
    // now, so memcpy isn't there.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_symbol_size("__memcpy_veneer", 0)
        .save("reach-unsized-veneer.elf");
    assert_output(
        &run(&[&path, "reach", "apply_filter"]).printed(0),
        "\
apply_filter and the 3 items it reaches take 144 bytes of code and 32 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0   144     0  .itcm
0x20000000..0x20000450     0     0
0x20240000..0x20241000     0     0
0x30000000..0x30000edc     0    32  .rodata

ADDRESS     SIZE  HOW   ITEM
0x74          92        apply_filter
0x40          52  call  filter_tap, used by apply_filter
0xd8           ?  call  __memcpy_veneer, used by apply_filter
0x30000eb4    32  uses  fir_coeffs, used by filter_tap

elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
    );
    let reach = run(&[&path, "reach", "apply_filter", "--json"]).json(0);
    assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
    assert_eq!(reach["complete"], false);
    // With --to, what elfq can't read may reach the ranges, so the exit
    // status is 1, even when nothing is listed.
    for (to, expected) in [
        (
            "0x20240000+1M",
            "\
In the code elfq can read, apply_filter reaches nothing in 0x20240000..0x20340000 (.ocram_bss).
elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
        ),
        (
            "0x30000000+16M",
            "\
apply_filter reaches 1 data object in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which takes 0 bytes of code and 32 bytes of data:

ADDRESS     SIZE  HOW   AT    ITEM
0x30000eb4    32  uses  0x56  fir_coeffs, used by filter_tap

elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
        ),
    ] {
        let stdout = run(&[&path, "reach", "apply_filter", "--to", to]).printed(1);
        assert_output(&stdout, expected);
        let reach = run(&[&path, "reach", "apply_filter", "--to", to, "--json"]).json(1);
        assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
        assert_eq!(reach["complete"], false);
    }
}

#[test]
fn the_overview_suggests_reach_for_a_handler_that_runs_in_normal_operation() {
    // The overview of c-cortex-m4-nodebug.elf with `handler` in the given
    // entries of the vector table, counted from the stack pointer's.
    let overview = |handler: &str, entries: &[usize]| {
        let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-nodebug.elf");
        let address = data.symbol_address(handler);
        for &entry in entries {
            data.set_vector_entry(entry, address | 1);
        }
        run(&[&data.save(&format!("{handler}-in-the-table.elf"))]).printed(0)
    };
    // main reaches more code than SysTick_Handler, but runs from these
    // entries only when something has gone wrong or while debugging: as
    // the HardFault handler, as in most firmware; as the handler of every
    // fault and of DebugMonitor, as in Zephyr's; and as the NMI handler.
    for entries in [&[3][..], &[3, 4, 5, 6, 12], &[2]] {
        let stdout = overview("main", entries);
        assert!(
            stdout.contains(" reach SysTick_Handler\n"),
            "{entries:?}: {stdout}"
        );
    }
    let stdout = overview("main", &[3, 15]);
    assert!(stdout.contains(" reach main\n"), "{stdout}");
    // Default_Handler, the SysTick handler now too, reaches nothing else.
    let stdout = overview("Default_Handler", &[15]);
    assert!(!stdout.contains(" reach "), "{stdout}");
}

#[test]
fn reach_lists_an_item_in_no_block_after_the_blocks() {
    // tx_buffer, in a section of size 0 now, so in no block of memory.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_section(".ocram_bss", SectionField::Size, 0)
        .save("reach-no-block.elf");
    assert_output(
        &run(&[&path, "reach", "platform_write"]).printed(0),
        "\
platform_write and the 3 data objects it reaches take 96 bytes of code and 4104 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0     0     0
0x20000000..0x20000450     0     8  .bss
0x30000000..0x30000edc    96     0  .text

ADDRESS     SIZE  HOW   ITEM
0x30000668    96        platform_write
0x2000044c     4  uses  tx_head, used by platform_write
0x20000448     4  uses  tx_tail, used by platform_write
0x20240000  4096  uses  tx_buffer, used by platform_write
",
    );
    let reach = run(&[&path, "reach", "platform_write", "--json"]).json(0);
    let tx_buffer = &reach["items"][2];
    assert_eq!(tx_buffer["name"], "tx_buffer");
    assert_eq!(tx_buffer["block"], serde_json::Value::Null);
}

#[test]
fn sizes_that_add_up_past_the_largest_number_stop_there() {
    // Functions that overlap, each half as large as the address space.
    // Each is the last in its section, since one that runs on past the
    // start of another in its section would end there.
    let mut data = ElfBytes::sample(SAMPLE);
    for function in ["_init", "platform_write", "_fini"] {
        data.set_symbol_size(function, 1 << 63);
    }
    let path = data.save("huge-functions.elf");
    let stdout = run(&[&path]).printed(0);
    let total = stdout.lines().nth(1).unwrap();
    assert!(
        total.contains(&format!(", {} bytes of", u64::MAX)),
        "{total}"
    );
    run(&[&path, "top", "--all"]).printed(0);
    run(&[&path, "inlined"]).printed(0);
}

#[test]
fn a_64_bit_file_marked_as_arm_is_not_read_as_thumb() {
    // Thumb code only comes in 32-bit files, whatever the entry point says.
    // Here the x86-64 sample is marked as Arm code, with an odd entry
    // point, as Thumb code has.
    let entry = load_sample(SAMPLE).entry;
    let mut data = ElfBytes::sample(SAMPLE);
    data.set(HeaderField::Machine, elf::EM_ARM)
        .set(HeaderField::EntryPoint, entry | 1);
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

    let path = ElfBytes::sample(SAMPLE)
        .replace_section(".debug_info", &with_length(&unit))
        .replace_section(".debug_abbrev", &abbrev)
        .replace_section(".debug_rnglists", &with_length(&ranges))
        .save("huge-inlined-copies.elf");
    assert_output(
        &run(&[&path, "inlined"]).printed(0),
        "\
Inlined copies of 1 function take 18446744073709551615 of the 2557 bytes of code:

               BYTES  COPIES  FUNCTION
18446744073709551615       2  (unnamed)
",
    );
    assert_output(
        &run(&[&path, "inlined", "_init"]).printed(0),
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
    let items = load_sample(SAMPLE).items;
    let main = items.iter().find(|item| item.name == "main").unwrap();
    // DWARF 4, abbreviations at offset 0, 8-byte addresses. Then the
    // compile unit, the copy in main, and the end of the unit's children.
    let mut unit = vec![4, 0, 0, 0, 0, 0, 8, 2, 1];
    unit.extend(main.address.to_le_bytes());
    unit.extend(0xffff_ffff_ffff_ff00u64.to_le_bytes());
    unit.push(0);

    let path = ElfBytes::sample(SAMPLE)
        .replace_section(".debug_info", &with_length(&unit))
        .replace_section(".debug_abbrev", &abbrev)
        .save("huge-high-pc.elf");
    // As in a release build, the range is dropped, and the copy is found at
    // its DW_AT_low_pc, without bytes.
    assert_output(
        &run(&[&path, "inlined"]).printed(0),
        "\
Inlined copies of 1 function take 0 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
    0       1  (unnamed)

A copy of 0 bytes is one the compiler merged into the code around it.
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
    let items = load_sample(SAMPLE).items;
    let main = items.iter().find(|item| item.name == "main").unwrap();
    // The compile unit, a copy in main, and the end of the unit's children.
    let mut entries = vec![2, 1];
    entries.extend(main.address.to_le_bytes());
    entries.push(0);
    let expected = "\
Inlined copies of 1 function take 0 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
    0       1  (unnamed)

A copy of 0 bytes is one the compiler merged into the code around it.
";
    // DWARF 4, abbreviations at offset 0, 8-byte addresses, then the
    // entries, with the byte after them, or, as gimli also reads it, with a
    // null entry before the root.
    for (name, entries) in [
        ("dwarf-trailing-byte.elf", [&entries[..], &[7]].concat()),
        ("dwarf-leading-null.elf", [&[0], &entries[..]].concat()),
    ] {
        let unit = [&[4, 0, 0, 0, 0, 0, 8], &entries[..]].concat();
        let path = ElfBytes::sample(SAMPLE)
            .replace_section(".debug_info", &with_length(&unit))
            .replace_section(".debug_abbrev", &abbrev)
            .save(name);
        assert_output(&run(&[&path, "inlined"]).printed(0), expected);
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

    let path = ElfBytes::sample(SAMPLE)
        .replace_section(".debug_info", &with_length(&unit))
        .replace_section(".debug_abbrev", &abbrev)
        .save("deep-dwarf.elf");
    assert_output(
        &run(&[&path, "inlined"]).printed(0),
        "The debug info lists no inlined code.\n",
    );
}

#[test]
fn dwarf_that_cant_be_read_says_so() {
    let path = ElfBytes::sample(SAMPLE)
        .set_section(".debug_info", SectionField::Size, 0x10)
        .save("short-dwarf.elf");
    assert_output(
        &run(&[&path, "inlined"]).error(),
        &format!("error: cannot read the DWARF debug info in {path}: unexpected end of input\n"),
    );
    // SYMBOL is looked up before the DWARF is read, which can take a while.
    assert_output(
        &run(&[&path, "inlined", "mian"]).error(),
        "error: no function or data object is named `mian`; did you mean main?\n",
    );
}

#[test]
fn inlined_copies_that_nest_deeply_are_summed_up() {
    // Copies inlined into main, each into the one before, 100,000 deep:
    // all but the first few are summed up in one row.
    let depth = 100_000;
    let items = load_sample(SAMPLE).items;
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

    let path = ElfBytes::sample(SAMPLE)
        .replace_section(".debug_info", &with_length(&unit))
        .replace_section(".debug_abbrev", &abbrev)
        .save("deep-inlined-copies.elf");
    let stdout = run(&[&path, "inlined", "main"]).printed(0);
    // The copies all have one name, so the rows name each by where it is,
    // in at most 16 names.
    let place = "(unnamed) in ".repeat(15);
    assert!(
        stdout.contains(&format!("  99970 more, inlined into {place}main\n")),
        "{stdout}"
    );
    // JSON has every copy, as a row under the one before.
    let tree = run(&[&path, "inlined", "main", "--json"]).json(0);
    let rows = tree["inlined"].as_array().unwrap();
    assert_eq!(rows.len(), depth);
    for (id, row) in rows.iter().enumerate().skip(1) {
        assert_eq!(row["parent"], id - 1);
    }
}

#[test]
fn a_long_chain_of_calls_is_read() {
    // The retained tree is as deep as the chain: deeper than the main
    // thread's stack holds in a debug build.
    let path = chain_of_calls(10_000).save("long-chain.elf");
    let stdout = run(&[&path]).printed(0);
    let total = stdout.lines().nth(1).unwrap();
    assert_eq!(total, "10000 functions, 79994 bytes of code");
    run(&[&path, "top", "--all"]).printed(0);
    // JSON has the whole chain, as rows that each hang under the one
    // before.
    let top = run(&[&path, "top", "--json"]).json(0);
    let items = top["items"].as_array().unwrap();
    assert_eq!(items.len(), 10_000);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
    // So does reach's, from the last function.
    let reach = run(&[&path, "reach", "0x8113878", "--json"]).json(0);
    let items = reach["items"].as_array().unwrap();
    assert_eq!(items.len(), 10_000);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
    run(&[&path, "reach", "0x8113878", "--all"]).printed(0);
    run(&[&path, "reach", "0x8113878", "--tree"]).printed(0);
}

#[test]
fn the_functions_of_a_long_chain_are_found_quickly() {
    // A function is found only by scanning the one after it, so there is a
    // round per function. Looking at every function again each round took
    // 20 seconds for 20,000 of them in a release build, and looking through
    // every item for each function 12 seconds for 100,000.
    let (binary, _) = parse_quickly(chain_of_calls(100_000));
    let functions = binary.items.iter().filter(|item| item.size == 8).count();
    assert_eq!(functions, 99_999);
}

#[test]
fn a_function_inlined_everywhere_is_told_from_one_kept_in_part() {
    // GCC can inline the start of a function and keep the rest out of line,
    // as `append.part.0`. Then not every copy of `append` was inlined, and
    // the error suggests the part. `process_frame` has as many letters.
    let path = ElfBytes::sample(SAMPLE)
        .rename_symbol("process_frame", "append.part.0")
        .save("kept-in-part.elf");
    assert_output(
        &run(&[&path, "callers", "append"]).error(),
        "error: no function or data object is named `append`; did you mean append.part.0?\n",
    );
}

#[test]
fn a_file_with_dwarf_but_no_symbol_table_says_so() {
    // Without names, elfq can't tell a function that the compiler inlined
    // everywhere from one it named by address.
    // `.symtab` holds plain bytes, as far as its header says.
    let path = ElfBytes::sample(SAMPLE)
        .set_section(".symtab", SectionField::Type, elf::SHT_PROGBITS)
        .save("no-symbol-table.elf");
    let error = run(&[&path, "callers", "append"]).error();
    assert!(
        error.starts_with(
            "error: no function or data object is named `append`; the file has no symbol table"
        ),
        "{error}"
    );
}

#[test]
fn reach_names_the_exceptions_that_the_architecture_has() {
    // Default_Handler's exceptions, as `reach --json` names them, in the
    // file at `path`, a build of c-cortex-m7.elf, where it is in entries 2
    // to 6, 11, 12 and 14 of the vector table.
    let exceptions = |path: &str| {
        let reach = run(&[path, "reach", "--json"]).json(0);
        let starts = reach["starts"].as_array().unwrap();
        let handler = starts
            .iter()
            .find(|start| start["name"] == "Default_Handler")
            .unwrap();
        handler["exceptions"].clone()
    };
    // In the reserved entries too, which the processor never takes, so
    // they aren't named or counted.
    let mut data = ElfBytes::sample(TCM_SAMPLE);
    let address = data.symbol_address("Default_Handler");
    for entry in [7, 8, 9, 10, 13] {
        data.set_vector_entry(entry, address | 1);
    }
    let path = data.save("reserved-entries.elf");
    assert_eq!(
        exceptions(&path),
        serde_json::json!([
            "NMI",
            "HardFault",
            "MemManage",
            "BusFault",
            "UsageFault",
            "SVCall",
            "DebugMonitor",
            "PendSV"
        ])
    );
    let stdout = run(&[&path, "reach"]).printed(0);
    assert!(
        stdout.contains("  NMI, HardFault and 6 more  Default_Handler\n"),
        "{stdout}"
    );
    // Armv6S-M, as on a Cortex-M0, has no MemManage, BusFault, UsageFault
    // or DebugMonitor: their entries are reserved.
    let path = for_arm_version(TCM_SAMPLE, Some(12)).save("armv6s-m.elf");
    assert_eq!(
        exceptions(&path),
        serde_json::json!(["NMI", "HardFault", "SVCall", "PendSV"])
    );
}
