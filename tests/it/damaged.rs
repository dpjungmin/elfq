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
use object::{Object, ObjectSection, ObjectSymbol, elf};

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

/// Where the header of `section` is in the file, and whether the file is a
/// 64-bit one.
fn section_header(data: &[u8], section: &str) -> (usize, bool) {
    let file = object::File::parse(data).unwrap();
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
    (headers + index * header_size, is_64)
}

/// Sets `field` in the header of `section` to `value`.
fn set_section_field(data: &mut [u8], section: &str, field: Field, value: u64) {
    let (header, is_64) = section_header(data, section);
    let (at, width) = if is_64 { (field.0, 8) } else { (field.1, 4) };
    write(data, header + at, width, value);
}

/// Sets the size that `.symtab` gives `symbol` to `size`.
fn set_symbol_size(data: &mut [u8], symbol: &str, size: u64) {
    let file = object::File::parse(&*data).unwrap();
    let index = file.symbol_by_name(symbol).unwrap().index().0;
    let (table, _) = file
        .section_by_name(".symtab")
        .unwrap()
        .file_range()
        .unwrap();
    // In a 64-bit file each symbol takes 24 bytes, and `st_size` is the
    // last 8; in a 32-bit one, 16 bytes, with `st_size` the 4 after
    // `st_name` and `st_value`.
    let (entry_size, at, width) = if file.is_64() {
        (24, 16, 8)
    } else {
        (16, 8, 4)
    };
    write(data, table as usize + index * entry_size + at, width, size);
}

/// Puts `bytes` at the end of the file and points `section` at them.
fn replace_section(data: &mut Vec<u8>, section: &str, bytes: &[u8]) {
    let offset = data.len() as u64;
    data.extend_from_slice(bytes);
    set_section_field(data, section, OFFSET, offset);
    set_section_field(data, section, SIZE, bytes.len() as u64);
}

/// The sample `name`, with build attributes that say only that its code is
/// for version `version` of the Arm architecture (Tag_CPU_arch), on a
/// Cortex-M core; or with an empty `.ARM.attributes`, which says nothing,
/// when `version` is `None`.
pub fn for_arm_version(name: &str, version: Option<u8>) -> Vec<u8> {
    let mut data = sample(name);
    let mut section = Vec::new();
    if let Some(version) = version {
        // Version `A`, then the `aeabi` vendor's attributes, then those of
        // the whole file (tag 1): Tag_CPU_arch (6) and
        // Tag_CPU_arch_profile (7), each with its length first.
        let file = [1, 9, 0, 0, 0, 6, version, 7, b'M'];
        section.push(b'A');
        section.extend((file.len() as u32 + 10).to_le_bytes());
        section.extend(b"aeabi\0");
        section.extend(file);
    }
    replace_section(&mut data, ".ARM.attributes", &section);
    data
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

/// [`parse`], failing after 10 seconds, for files that once took minutes in
/// a debug build, such as a huge section read one word at a time.
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
pub fn save(name: &str, data: &[u8]) -> String {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, data).unwrap();
    path.to_str().unwrap().to_string()
}

/// Runs elfq with `args` on `data`, saved as `name`, and returns the error
/// it printed, with exit status 2.
fn error(name: &str, data: &[u8], args: &[&str]) -> String {
    let output = elfq(&[&[save(name, data).as_str()], args].concat());
    assert_eq!(output.status.code(), Some(2), "{name}");
    String::from_utf8(output.stderr).unwrap()
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
fn the_addend_of_a_pointer_that_a_plt_stub_jumps_through_is_ignored() {
    // glibc's own libc.so.6 gives each of these pointers an address in its
    // stub as the addend, which the dynamic loader ignores. The stubs in
    // `.plt` jump through JUMP_SLOT pointers, listed in `.rela.plt`, and
    // those in `.plt.got` through GLOB_DAT ones, listed in `.rela.dyn`.
    let mut data = sample("c-plt.so");
    let file = object::File::parse(&*data).unwrap();
    let tables = [".rela.plt", ".rela.dyn"]
        .map(|name| file.section_by_name(name).unwrap().file_range().unwrap());
    for (table, size) in tables {
        // Each entry takes 24 bytes: `r_offset`, then `r_info`, whose low
        // 32 bits are the type, then `r_addend`.
        for entry in (table as usize..(table + size) as usize).step_by(24) {
            let r_type = data[entry + 8..entry + 12].try_into().unwrap();
            let r_type = elf::RelocationType(u32::from_le_bytes(r_type));
            if [elf::R_X86_64_JUMP_SLOT, elf::R_X86_64_GLOB_DAT].contains(&r_type) {
                write(&mut data, entry + 16, 8, 0x1064);
            }
        }
    }
    let (binary, _) = parse(&data);
    let stubs = load_sample("c-plt.so").own_function_stubs();
    assert_eq!(stubs.len(), 6);
    assert_eq!(binary.own_function_stubs(), stubs);
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
    assert_eq!(table.exceptions, [1, 2, 3]);
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

/// The x86-64 sample, with the bytes of each of `sections` past the end
/// of the file.
pub fn past_the_end(sections: &[&str]) -> Vec<u8> {
    let mut data = sample("c-x86_64.elf");
    for section in sections {
        set_section_field(&mut data, section, OFFSET, u64::MAX - 15);
    }
    data
}

#[test]
fn a_section_that_runs_past_the_end_of_the_file_keeps_its_addresses() {
    // Only where its bytes are is damaged, so the section is still there,
    // and the file holds none of its bytes.
    let data = past_the_end(&[".plt"]);
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
fn diff_names_the_file_that_a_warning_is_about() {
    let path = save("plt-offset-diff.elf", &past_the_end(&[".plt"]));
    let old = sample_path("c-x86_64.elf");
    let output = elfq(&[old.to_str().unwrap(), "diff", &path]);
    assert_eq!(output.status.code(), Some(1));
    assert_output(
        &String::from_utf8(output.stderr).unwrap(),
        &format!("warning: {path}: section .plt runs past the end of the file\n"),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.ends_with("Sections that differ: .plt.\n"),
        "{stdout}"
    );
}

/// Runs `elfq SAMPLE diff NEW`, with `data` saved as `name` for NEW, and
/// returns its exit status, and what it printed and printed as an error.
fn diff(sample: &str, name: &str, data: &[u8], options: &[&str]) -> (i32, String, String) {
    let old = sample_path(sample);
    let output = elfq(&[&[old.to_str().unwrap(), "diff", &save(name, data)], options].concat());
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    (
        output.status.code().unwrap(),
        text(output.stdout),
        text(output.stderr),
    )
}

/// Where the bytes of `section` are in the file.
fn file_range(data: &[u8], section: &str) -> (usize, usize) {
    let file = object::File::parse(data).unwrap();
    let (offset, size) = file.section_by_name(section).unwrap().file_range().unwrap();
    (offset as usize, size as usize)
}

#[test]
fn diff_names_a_section_whose_bytes_alone_differ() {
    // Another build ID, as after a change only in debug info: the memory
    // keeps its size, and the last line says where it differs.
    let mut data = sample("c-x86_64.elf");
    let (offset, size) = file_range(&data, ".note.gnu.build-id");
    data[offset + size - 1] ^= 1;
    let (status, text, _) = diff("c-x86_64.elf", "build-id.elf", &data, &[]);
    assert_eq!(status, 1);
    assert!(
        text.ends_with(
            "\nThe memory stays at 15728 bytes.\nSections that differ: .note.gnu.build-id.\n"
        ),
        "{text}"
    );
    let (_, json, _) = diff("c-x86_64.elf", "build-id.elf", &data, &["--json"]);
    assert!(
        json.contains("\"sections_that_differ\":[\n\".note.gnu.build-id\"\n],"),
        "{json}"
    );
    // In firmware, the block's row says only that bytes differ.
    let mut data = sample("c-cortex-m7.elf");
    let (offset, _) = file_range(&data, ".rodata");
    data[offset] ^= 1;
    let (status, text, _) = diff("c-cortex-m7.elf", "rodata-byte.elf", &data, &[]);
    assert_eq!(status, 1);
    assert!(
        text.contains("\n0x30000000..0x30000edc  3804  3804       0  bytes differ\n"),
        "{text}"
    );
    assert!(
        text.ends_with("\n\nSections that differ: .rodata.\n"),
        "{text}"
    );
}

#[test]
fn diff_counts_the_bytes_that_sections_overlap_on_once() {
    // .rodata, grown by 8 bytes, holds the bytes of .ARM.exidx after it
    // too, at the same addresses.
    let mut data = sample("c-cortex-m7.elf");
    set_section_field(&mut data, ".rodata", SIZE, 0xf4 + 8);
    let (status, text, _) = diff("c-cortex-m7.elf", "rodata-over-exidx.elf", &data, &[]);
    assert_eq!(status, 0, "{text}");
    assert!(text.ends_with(" hold the same bytes at the same addresses.\n"));
}

/// The offset of each section header in a 32-bit file, with the section's
/// `sh_flags`.
fn section_headers_32(data: &[u8]) -> Vec<(usize, u32)> {
    let number = |at: usize, width: usize| {
        let mut bytes = [0; 4];
        bytes[..width].copy_from_slice(&data[at..at + width]);
        u32::from_le_bytes(bytes) as usize
    };
    // `e_shoff`, `e_shentsize` and `e_shnum`.
    let (headers, size, count) = (number(0x20, 4), number(0x2e, 2), number(0x30, 2));
    (0..count)
        .map(|index| headers + index * size)
        .map(|header| (header, number(header + 8, 4) as u32))
        .collect()
}

/// `SHF_ALLOC`, set in the flags of a section that the program loads.
const ALLOCATED: u32 = 0x2;

#[test]
fn diff_refuses_a_separate_debug_file() {
    // objcopy --only-keep-debug keeps the header of each section that the
    // program loads, but makes it take no room in the file, as .bss does.
    let mut data = sample("c-cortex-m7.elf");
    for (header, flags) in section_headers_32(&data) {
        if flags & ALLOCATED != 0 {
            // `sh_type`, as SHT_NOBITS.
            write(&mut data, header + 4, 4, 8);
        }
    }
    let path = save("c-cortex-m7.debug", &data);
    let (status, _, error) = diff("c-cortex-m7.elf", "c-cortex-m7.debug", &data, &[]);
    assert_eq!(status, 2);
    assert_output(
        &error,
        &format!(
            "error: {path} stores none of its code, as a separate debug file doesn't; diff \
             needs the program itself\n"
        ),
    );
}

#[test]
fn diff_refuses_a_file_that_puts_nothing_in_memory() {
    let mut data = sample("c-cortex-m7.elf");
    for (header, flags) in section_headers_32(&data) {
        write(&mut data, header + 8, 4, u64::from(flags & !ALLOCATED));
    }
    let path = save("c-cortex-m7-unallocated.elf", &data);
    let (status, _, error) = diff("c-cortex-m7.elf", "c-cortex-m7-unallocated.elf", &data, &[]);
    assert_eq!(status, 2);
    assert_output(
        &error,
        &format!("error: {path} puts nothing in memory, so diff has nothing to compare\n"),
    );
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
fn a_function_of_unknown_size_is_named_where_its_load_image_is() {
    // The last function in .itcm, which flash stores from 0x30000118 for
    // the startup code to copy, without a size.
    let mut data = sample("c-cortex-m7-nodebug.elf");
    set_symbol_size(&mut data, "__memcpy_veneer", 0);
    let path = save("memcpy-veneer-unsized.elf", &data);
    let output = elfq(&[&path, "top", "--range", "0x30000118+8"]);
    assert_eq!(output.status.code(), Some(2));
    assert_output(
        &String::from_utf8(output.stderr).unwrap(),
        "error: elfq doesn't know where __memcpy_veneer ends, so it can't count the bytes in \
         0x30000118..0x30000120 (the load image of .itcm)\n",
    );
}

#[test]
fn reach_names_what_it_reaches_but_cant_read() {
    // apply_filter calls memcpy through __memcpy_veneer, which has no size
    // now, so memcpy isn't there.
    let mut data = sample("c-cortex-m7-nodebug.elf");
    set_symbol_size(&mut data, "__memcpy_veneer", 0);
    let (stdout, _) = run(
        "reach-unsized-veneer.elf",
        &data,
        &["reach", "apply_filter"],
    );
    assert_output(
        &stdout,
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
    let (json, _) = run(
        "reach-unsized-veneer.elf",
        &data,
        &["reach", "apply_filter", "--json"],
    );
    let reach: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
    assert_eq!(reach["complete"], false);
    // With --to, what elfq can't read may reach the ranges, so the exit
    // status is 1, even when nothing is listed.
    let path = save("reach-unsized-veneer.elf", &data);
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
        let output = elfq(&[&path, "reach", "apply_filter", "--to", to]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_output(&String::from_utf8(output.stdout).unwrap(), expected);
        let output = elfq(&[&path, "reach", "apply_filter", "--to", to, "--json"]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let reach: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
        assert_eq!(reach["complete"], false);
    }
}

#[test]
fn the_overview_suggests_reach_for_a_handler_that_runs_in_normal_operation() {
    // The overview of c-cortex-m4-nodebug.elf with `handler` in the given
    // entries of the vector table, counted from the stack pointer's.
    let overview = |handler: &str, entries: &[usize]| {
        let mut data = sample("c-cortex-m4-nodebug.elf");
        let file = object::File::parse(&*data).unwrap();
        let (table, _) = file
            .section_by_name(".isr_vector")
            .unwrap()
            .file_range()
            .unwrap();
        let address = file.symbol_by_name(handler).unwrap().address();
        for &entry in entries {
            write(&mut data, table as usize + entry * 4, 4, address | 1);
        }
        run(&format!("{handler}-in-the-table.elf"), &data, &[]).0
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
    let mut data = sample("c-cortex-m7-nodebug.elf");
    set_section_field(&mut data, ".ocram_bss", SIZE, 0);
    let (stdout, _) = run("reach-no-block.elf", &data, &["reach", "platform_write"]);
    assert_output(
        &stdout,
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
    let (json, _) = run(
        "reach-no-block.elf",
        &data,
        &["reach", "platform_write", "--json"],
    );
    let reach: serde_json::Value = serde_json::from_str(&json).unwrap();
    let tx_buffer = &reach["items"][2];
    assert_eq!(tx_buffer["name"], "tx_buffer");
    assert_eq!(tx_buffer["block"], serde_json::Value::Null);
}

#[test]
fn sizes_that_add_up_past_the_largest_number_stop_there() {
    // Functions that overlap, each half as large as the address space.
    // Each is the last in its section, since one that runs on past the
    // start of another in its section would end there.
    let mut data = sample("c-x86_64.elf");
    for function in ["_init", "platform_write", "_fini"] {
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
fn dwarf_that_cant_be_read_says_so() {
    let mut data = sample("c-x86_64.elf");
    set_section_field(&mut data, ".debug_info", SIZE, 0x10);
    let path = save("short-dwarf.elf", &data);
    let output = elfq(&[&path, "inlined"]);
    assert_eq!(output.status.code(), Some(2));
    assert_output(
        &String::from_utf8(output.stderr).unwrap(),
        &format!("error: cannot read the DWARF debug info in {path}: unexpected end of input\n"),
    );
    // SYMBOL is looked up before the DWARF is read, which can take a while.
    let output = elfq(&[&path, "inlined", "mian"]);
    assert_eq!(output.status.code(), Some(2));
    assert_output(
        &String::from_utf8(output.stderr).unwrap(),
        "error: no function or data object is named `mian`; did you mean main?\n",
    );
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
    // The copies all have one name, so the rows name each by where it is,
    // in at most 16 names.
    let place = "(unnamed) in ".repeat(15);
    assert!(
        stdout.contains(&format!("  99970 more, inlined into {place}main\n")),
        "{stdout}"
    );
    // JSON has every copy, as a row under the one before.
    let (json, _) = run(
        "deep-inlined-copies.elf",
        &data,
        &["inlined", "main", "--json"],
    );
    let tree: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = tree["inlined"].as_array().unwrap();
    assert_eq!(rows.len(), depth);
    for (id, row) in rows.iter().enumerate().skip(1) {
        assert_eq!(row["parent"], id - 1);
    }
}

/// Stripped firmware whose code is `count` Thumb functions, in place of
/// `.text`, that each call the one before. It starts at the last. The first
/// function takes 2 bytes and the others 8, since elfq leaves out the
/// padding after the first.
pub fn chain_of_calls(count: usize) -> Vec<u8> {
    // bx lr, then three nops as padding.
    let mut code: Vec<u16> = vec![0x4770, 0xbf00, 0xbf00, 0xbf00];
    for _ in 1..count {
        // push {lr}; bl to the function before; pop {pc}
        code.extend([0xb500, 0xf7ff, 0xfff9, 0xbd00]);
    }
    let code: Vec<u8> = code.iter().flat_map(|half| half.to_le_bytes()).collect();

    let mut data = sample("c-cortex-m4-stripped.elf");
    let address = 0x0810_0000;
    replace_section(&mut data, ".text", &code);
    set_section_field(&mut data, ".text", ADDRESS, address);
    // `e_entry`, in the file's header: the last function, as Thumb code.
    let last = address + 8 * (count as u64 - 1);
    write(&mut data, 0x18, 4, last | 1);
    data
}

#[test]
fn a_long_chain_of_calls_is_read() {
    // The retained tree is as deep as the chain: deeper than the main
    // thread's stack holds in a debug build.
    let data = chain_of_calls(10_000);
    let (stdout, _) = run("long-chain.elf", &data, &[]);
    let total = stdout.lines().nth(1).unwrap();
    assert_eq!(total, "10000 functions, 79994 bytes of code");
    run("long-chain.elf", &data, &["top", "--all"]);
    // JSON has the whole chain, as rows that each hang under the one
    // before.
    let (json, _) = run("long-chain.elf", &data, &["top", "--json"]);
    let top: serde_json::Value = serde_json::from_str(&json).unwrap();
    let items = top["items"].as_array().unwrap();
    assert_eq!(items.len(), 10_000);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
    // So does reach's, from the last function.
    let (json, _) = run("long-chain.elf", &data, &["reach", "0x8113878", "--json"]);
    let reach: serde_json::Value = serde_json::from_str(&json).unwrap();
    let items = reach["items"].as_array().unwrap();
    assert_eq!(items.len(), 10_000);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
    run("long-chain.elf", &data, &["reach", "0x8113878", "--all"]);
    run("long-chain.elf", &data, &["reach", "0x8113878", "--tree"]);
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
    let mut data = sample("c-x86_64.elf");
    let file = object::File::parse(&*data).unwrap();
    let (start, size) = file
        .section_by_name(".strtab")
        .unwrap()
        .file_range()
        .unwrap();
    let (start, end) = (start as usize, (start + size) as usize);
    let at = start
        + data[start..end]
            .windows(15)
            .position(|name| name == b"\0process_frame\0")
            .unwrap();
    data[at + 1..at + 14].copy_from_slice(b"append.part.0");
    assert_output(
        &error("kept-in-part.elf", &data, &["callers", "append"]),
        "error: no function or data object is named `append`; did you mean append.part.0?\n",
    );
}

#[test]
fn a_file_with_dwarf_but_no_symbol_table_says_so() {
    // Without names, elfq can't tell a function that the compiler inlined
    // everywhere from one it named by address.
    let mut data = sample("c-x86_64.elf");
    // `sh_type`, which says what a section holds, takes the 4 bytes after
    // its name in either size of file: 1 is `SHT_PROGBITS`, plain bytes.
    let (header, _) = section_header(&data, ".symtab");
    write(&mut data, header + 4, 4, 1);
    let error = error("no-symbol-table.elf", &data, &["callers", "append"]);
    assert!(
        error.starts_with(
            "error: no function or data object is named `append`; the file has no symbol table"
        ),
        "{error}"
    );
}

#[test]
fn reach_names_the_exceptions_that_the_architecture_has() {
    // Default_Handler's exceptions, as `reach --json` names them, in
    // `data`, a build of c-cortex-m7.elf, where it is in entries 2 to 6, 11,
    // 12 and 14 of the vector table.
    let exceptions = |name: &str, data: &[u8]| {
        let (json, _) = run(name, data, &["reach", "--json"]);
        let reach: serde_json::Value = serde_json::from_str(&json).unwrap();
        let starts = reach["starts"].as_array().unwrap();
        let handler = starts
            .iter()
            .find(|start| start["name"] == "Default_Handler")
            .unwrap();
        handler["exceptions"].clone()
    };
    // In the reserved entries too, which the processor never takes, so
    // they aren't named or counted.
    let mut data = sample("c-cortex-m7.elf");
    let file = object::File::parse(&*data).unwrap();
    let (table, _) = file
        .section_by_name(".isr_vector")
        .unwrap()
        .file_range()
        .unwrap();
    let address = file.symbol_by_name("Default_Handler").unwrap().address();
    for entry in [7, 8, 9, 10, 13] {
        write(&mut data, table as usize + entry * 4, 4, address | 1);
    }
    let name = "reserved-entries.elf";
    assert_eq!(
        exceptions(name, &data),
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
    let (stdout, _) = run(name, &data, &["reach"]);
    assert!(
        stdout.contains("  NMI, HardFault and 6 more  Default_Handler\n"),
        "{stdout}"
    );
    // Armv6S-M, as on a Cortex-M0, has no MemManage, BusFault, UsageFault
    // or DebugMonitor: their entries are reserved.
    let data = for_arm_version("c-cortex-m7.elf", Some(12));
    assert_eq!(
        exceptions("armv6s-m.elf", &data),
        serde_json::json!(["NMI", "HardFault", "SVCall", "PendSV"])
    );
}
