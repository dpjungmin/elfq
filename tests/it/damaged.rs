//! Damaged and unusual files, made by changing the bytes of a sample:
//! headers that don't add up, huge sections, and calls or debug info nested
//! far deeper than compilers make them. elfq must still answer quickly,
//! with what the file does hold. The tests run in a debug build, so
//! arithmetic that overflows fails them too.

use std::sync::mpsc;
use std::time::Duration;

use elfq::binary::Binary;
use elfq::graph::{Graph, ReferenceKind};
use object::{Object, ObjectSection};

use crate::support::{load_sample, sample_path};

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
