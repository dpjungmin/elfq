//! `diff`: what changed between two builds of a program.
//!
//! It compares them block of memory by block. The tests check its text,
//! its JSON, its exit status and the builds it refuses, and at the end,
//! its answers on builds made by changing a sample's bytes with
//! `ElfBytes`.

use std::collections::BTreeMap;

use object::elf;
use serde_json::{Value, json};

use crate::support::elf::{
    ElfBytes, HeaderField, SectionField, past_the_end, without_section_headers,
};
use crate::support::{
    Exit, LIBRARY_SAMPLE, MOVED_SAMPLE, Run, SAMPLE, TCM_EDITED_SAMPLE, TCM_INLINED_SAMPLE,
    TCM_MOVED_SAMPLE, TCM_OCRAM_SAMPLE, TCM_SAMPLE, TCM_SDRAM_SAMPLE, assert_output, elfq_json,
    run, samples, stderr,
};

/// Runs `elfq OLD diff NEW` with `options`.
#[track_caller]
fn diff(old: &str, new: &str, options: &[&str]) -> Run {
    run(&[&[old, "diff", new], options].concat())
}

#[test]
fn firmware_shows_each_block_of_memory_and_the_items_that_changed() {
    // filter_tap left the ITCM through a new veneer, and crc32 there grew
    // by a call to log_msg, through another, with its message in .rodata.
    // So apply_filter calls filter_tap in flash, and crc32 log_msg, and
    // filter_tap no longer uses fir_coeffs from the ITCM.
    let text = diff(TCM_SAMPLE, TCM_MOVED_SAMPLE, &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-moved.elf: 1 moved, 3 new, 1 grew.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   208     -16  .itcm -16
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000eec  3804  3820     +16  load image of .itcm -16, .rodata +16

OLD  NEW  FROM  TO          MOVED
 52   52  0x40  0x30000188  filter_tap (.itcm to .text), kept by __filter_tap_veneer

CHANGE  OLD  NEW  ADDRESS     ITEM
   +16   48   64  0x6c        crc32, kept by __crc32_veneer
    +8         8  0xb0        __filter_tap_veneer (new), kept by apply_filter
    +8         8  0xb8        __log_msg_veneer (new), kept by crc32
   +16       ~16  0x30000de0  .rodata@0x30000de0 (new), kept by crc32

Calls between blocks, new or gone:

STATUS  ADDRESS     SIZE  HOW              AT    CALLEE
new     0x30000188    52  call via veneer  0x36  filter_tap, used by apply_filter
new     0x300002bc    26  call via veneer  0x9a  log_msg, used by crc32

Other uses between blocks: 1 new and 1 gone; --json lists them.

61 other items are the same.

Try:
  elfq testdata/bin/c-cortex-m7-moved.elf why __filter_tap_veneer
  elfq testdata/bin/c-cortex-m7-moved.elf startup
",
    );
    // --all lists the same calls: every one is listed already.
    let calls = |text: &str| -> String {
        let (_, calls) = text.split_once("new or gone:\n").unwrap();
        calls.split_once("\n\n").unwrap().1.to_string()
    };
    let all = diff(TCM_SAMPLE, TCM_MOVED_SAMPLE, &["--all"]).printed(1);
    assert_eq!(calls(&all), calls(&text));
    // The other way round, the calls are gone, and the uses of data swap.
    let back = diff(TCM_MOVED_SAMPLE, TCM_SAMPLE, &[]).printed(1);
    assert!(
        back.contains(
            "\ngone    0x30000188    52  call via veneer  0x36  filter_tap, used by apply_filter\n\
             gone    0x300002bc    26  call via veneer  0x9a  log_msg, used by crc32\n\
             \n\
             Other uses between blocks: 1 new and 1 gone; --json lists them.\n"
        ),
        "{back}"
    );
}

#[test]
fn a_build_without_symbols_is_compared_by_its_bytes() {
    // The memory is the same as with symbols, but nothing names the items.
    let text = diff(
        "testdata/bin/c-cortex-m7-stripped.elf",
        "testdata/bin/c-cortex-m7-moved-stripped.elf",
        &[],
    )
    .printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-cortex-m7-stripped.elf to testdata/bin/c-cortex-m7-moved-stripped.elf: the bytes differ in 2 of 4 blocks.
testdata/bin/c-cortex-m7-stripped.elf has no symbol table, so diff compares only bytes, not functions and data.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   208     -16  .itcm -16
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000eec  3804  3820     +16  load image of .itcm -16, .rodata +16

Sections that differ: .itcm, .isr_vector, .text, .rodata and .ARM.exidx.

Try:
  elfq testdata/bin/c-cortex-m7-moved-stripped.elf startup
",
    );
    // Only the new build without: it is the one named.
    let text = diff(
        TCM_SAMPLE,
        "testdata/bin/c-cortex-m7-moved-stripped.elf",
        &[],
    )
    .printed(1);
    assert!(
        text.contains(
            "\ntestdata/bin/c-cortex-m7-moved-stripped.elf has no symbol table, so diff \
             compares only bytes, not functions and data.\n"
        ),
        "{text}"
    );
}

#[test]
fn one_block_of_memory_gets_a_line() {
    // crc32 grew by 16 bytes, which moved the code after it, and the
    // addresses and unwind info that point to it. process_frame keeps its
    // size, but keeps the frame in r12 rather than r8 across its call to
    // crc32: GCC knew the old crc32 left r8 alone, and the new one calls
    // log_msg.
    let text = diff(SAMPLE, MOVED_SAMPLE, &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-x86_64.elf to testdata/bin/c-x86_64-moved.elf: 1 new, 2 grew, 1 shrank, 1 edited.
The memory stays at 15728 bytes (.text +16, .eh_frame +8).

CHANGE  OLD  NEW  ADDRESS  ITEM
   +20   59   79  0x1610   crc32, kept by process_frame
   +13       ~13  0x2004   .rodata@0x2004 (new), kept by crc32
   -13  ~34  ~21  0x206b   .rodata@0x206b, kept by main
    +3   19   22  0x1660   on_button, kept by main
     0  532  532  0x1710   process_frame (edited), kept by main

64 other items are the same.
",
    );
    // The other way round, the changes are the other way round, and what
    // was new is gone.
    let back = diff(MOVED_SAMPLE, SAMPLE, &[]).printed(1);
    assert!(
        back.contains("The memory stays at 15728 bytes (.text -16, .eh_frame -8).\n"),
        "{back}"
    );
    assert!(
        back.contains("\n   -13  ~13       0x2004   .rodata@0x2004 (gone), kept by crc32\n"),
        "{back}"
    );
}

#[test]
fn data_moved_to_another_memory_is_a_gone_and_a_new_block() {
    // DTCM linked at 0x80000000: every item of .data and .bss moved by one
    // offset, and the code and flash hold their addresses. Only the vector
    // table is edited: its first word, the initial stack pointer, went from
    // the end of DTCM at 0x20040000 to 0x80040000, where no section ends.
    let text = diff(TCM_SAMPLE, TCM_SDRAM_SAMPLE, &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-sdram.elf: 12 moved, 1 edited.

MEMORY                          OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0          224   224       0  bytes differ
0x20000000..0x20000450 (gone)  1104     -   -1104  .data .bss
0x20240000..0x20241000         4096  4096       0
0x30000000..0x30000edc         3804  3804       0  bytes differ
0x80000000..0x80000450 (new)      -  1104   +1104  .data .bss

 OLD   NEW  FROM        TO          MOVED
1104  1104  0x20000000  0x80000000  all 12 items of .data and .bss

CHANGE  OLD  NEW  ADDRESS     ITEM
     0   64   64  0x30000000  vector_table (edited)

No call between blocks is new or gone.
Other uses between blocks: 17 new and 17 gone; --json lists them.

50 other items are the same.

Try:
  elfq testdata/bin/c-cortex-m7-sdram.elf startup
",
    );
    // --all lists each, largest first.
    let all = diff(TCM_SAMPLE, TCM_SDRAM_SAMPLE, &["--all"]).printed(1);
    let (_, moved) = all.split_once("TO          MOVED\n").unwrap();
    let (moved, _) = moved.split_once("\n\n").unwrap();
    let rows: Vec<&str> = moved.lines().collect();
    assert_eq!(rows.len(), 12, "{all}");
    assert_eq!(
        rows[0],
        "1024  1024  0x20000000  0x80000000  crc_table, kept by main"
    );
}

#[test]
fn an_item_moved_to_another_block_asks_for_startup() {
    // tx_head moved from .bss to .ocram_bss, which the reset handler
    // doesn't zero, while no load image changed: the item that moved is
    // why diff asks for startup.
    let text = diff(TCM_SAMPLE, TCM_OCRAM_SAMPLE, &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-ocram.elf: 1 moved.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   224       0
0x20000000..0x20000450  1104  1100      -4  .bss -4
0x20240000..0x20241004  4096  4100      +4  .ocram_bss +4
0x30000000..0x30000edc  3804  3804       0  bytes differ

OLD  NEW  FROM        TO          MOVED
  4    4  0x2000044c  0x20241000  tx_head (.bss to .ocram_bss), kept by platform_write

No call between blocks is new or gone.
Other uses between blocks: 1 new and 1 gone; --json lists them.

62 other items are the same.

Try:
  elfq testdata/bin/c-cortex-m7-ocram.elf startup
",
    );
}

#[test]
fn an_item_that_changed_but_kept_its_size_is_edited() {
    // The number in decode_header's movw and the last of fir_coeffs
    // changed, and nothing moved.
    let text = diff(TCM_SAMPLE, TCM_EDITED_SAMPLE, &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-edited.elf: 2 edited.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   224       0
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000edc  3804  3804       0  bytes differ

CHANGE  OLD  NEW  ADDRESS     ITEM
     0   40   40  0x30000160  decode_header (edited), kept by process_frame
     0   32   32  0x30000eb4  fir_coeffs (edited), kept by filter_tap

No call between blocks is new or gone.

61 other items are the same.
",
    );
}

#[test]
fn a_shared_library_has_no_kept_by() {
    let text = diff(LIBRARY_SAMPLE, "testdata/bin/c-x86_64-moved.so", &[]).printed(1);
    assert_output(
        &text,
        "\
From testdata/bin/c-x86_64.so to testdata/bin/c-x86_64-moved.so: 1 new, 1 grew, 1 shrank, 1 edited.
The memory stays at 15936 bytes (.text +16, .eh_frame +8).
elfq can't yet tell what keeps items in a shared library, so rows don't say.

CHANGE  OLD  NEW  ADDRESS  ITEM
   +20   59   79  0x16c0   crc32
   +13       ~13  0x2000   .rodata@0x2000 (new)
   -13  ~38  ~25  0x2067   .rodata@0x2067
     0  532  532  0x17c0   process_frame (edited)

69 other items are the same.
",
    );
}

#[test]
fn all_names_every_change() {
    // A program and its shared library: many sections changed size, and
    // the rows say what keeps an item in neither build.
    let text = diff(SAMPLE, LIBRARY_SAMPLE, &[]).printed(1);
    assert!(
        text.contains(
            "\nThe memory went from 15728 to 15936 bytes (.dynsym +192, .rela.plt +168, .plt \
             +112 and 15 more).\nelfq can't yet tell what keeps items in a shared library, so \
             rows don't say.\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\n    -4    4       0x2000   _IO_stdin_used (gone)\n"),
        "{text}"
    );
    let all = diff(SAMPLE, LIBRARY_SAMPLE, &["--all"]).printed(1);
    assert!(
        all.contains(
            "\nThe memory went from 15728 to 15936 bytes (.note.gnu.property -16, .gnu.hash +36, \
             .interp -28, .dynsym +192, .note.ABI-tag -32, .dynstr +66, .gnu.version +16, \
             .gnu.version_r -16, .rela.dyn -24, .rela.plt +168, .plt +112, .plt.sec +112, .text \
             -48, .eh_frame_hdr -8, .eh_frame -24, .dynamic -48, .got -56, .got.plt +104).\n"
        ),
        "{all}"
    );
    // With --all, nothing folds: the two objects without a name that the
    // text folds into one row each have one.
    assert!(
        text.contains("\n   +12                     2 data objects without a name changed size\n"),
        "{text}"
    );
    assert!(
        all.contains("\n    +8  ~24  ~32  0x4060   .data@0x4060\n"),
        "{all}"
    );
    // Without a symbol table, no row says why the memory differs, and the
    // last line names the sections that differ: 10, or every one.
    let stripped = "testdata/bin/c-x86_64-stripped.elf";
    let text = diff(stripped, LIBRARY_SAMPLE, &[]).printed(1);
    assert!(
        text.ends_with(
            "\nSections that differ: .note.gnu.property, .note.gnu.build-id, .gnu.hash, .interp, \
             .dynsym, .note.ABI-tag, .dynstr, .gnu.version, .gnu.version_r, .rela.dyn and 18 \
             more.\n"
        ),
        "{text}"
    );
    let all = diff(stripped, LIBRARY_SAMPLE, &["--all"]).printed(1);
    assert!(
        all.ends_with(
            "\nSections that differ: .note.gnu.property, .note.gnu.build-id, .gnu.hash, .interp, \
             .dynsym, .note.ABI-tag, .dynstr, .gnu.version, .gnu.version_r, .rela.dyn, .rela.plt, \
             .init, .plt, .plt.got, .plt.sec, .text, .fini, .rodata, .eh_frame_hdr, .eh_frame, \
             .init_array, .fini_array, .data.rel.ro, .dynamic, .got, .got.plt, .data and .bss.\n"
        ),
        "{all}"
    );
    // Two programs for other chips: blocks that only one has name what is
    // in them, as the overview does, whole.
    let firmware = diff("testdata/bin/c-cortex-m4.elf", TCM_SAMPLE, &[]).printed(1);
    assert!(
        firmware.contains(
            "0x08000000..0x08000e0c (gone)  3596     -   -3596  .isr_vector .text .rodata \
             .ARM.exidx, load image of .data\n"
        ),
        "{firmware}"
    );
    assert!(
        firmware.contains(
            "0x30000000..0x30000edc (new)      -  3804   +3804  .isr_vector .text .rodata \
             .ARM.exidx, load images of .itcm .data\n"
        ),
        "{firmware}"
    );
}

#[test]
fn a_gone_function_says_where_the_compiler_inlined_it() {
    // decode_header, inlined into process_frame, its only caller: so the
    // row doesn't say that process_frame kept it too. process_frame, which
    // holds its code now, kept its size, and is edited.
    let text = diff(TCM_SAMPLE, TCM_INLINED_SAMPLE, &[]).printed(1);
    assert!(
        text.contains(
            "\nCHANGE  OLD  NEW  ADDRESS     ITEM\n   \
             -40   40       0x30000160  decode_header (gone, inlined into process_frame)\n     \
             0  376  376  0x300002f8  process_frame (edited), kept by main\n\n"
        ),
        "{text}"
    );
}

#[test]
fn the_change_table_shows_the_largest_rows_of_every_block() {
    // A program with newlib's strtod in place of the sample's code: more
    // rows than the table has room for, in two blocks.
    let text = diff(
        "testdata/bin/c-cortex-m4.elf",
        "testdata/bin/c-strtod.elf",
        &[],
    )
    .printed(1);
    let (_, table) = text.split_once("ADDRESS     ITEM\n").unwrap();
    let (table, _) = table.split_once("\n\n").unwrap();
    let rows: Vec<&str> = table.lines().collect();
    // 30 rows, and a row for the rest of each block, with how many items
    // they stand for and their sum.
    assert_eq!(rows.len(), 32, "{text}");
    assert_eq!(
        rows[0],
        "+16744        16744  0x8001074   strtod (new, with 114 new items it keeps), kept by Reset_Handler"
    );
    assert_eq!(
        rows[26],
        "  +460                           30 more in 0x08000000"
    );
    assert_eq!(
        rows[27],
        " +4096         4096  0x200001d0  heap.0 (new), kept by _sbrk"
    );
    assert_eq!(
        rows[31],
        "   -24                           18 more in 0x20000000"
    );
    // The other way round, strtod is gone, and OLD gives its bytes and
    // those of the gone items it kept, so that NEW less OLD is CHANGE.
    let back = diff(
        "testdata/bin/c-strtod.elf",
        "testdata/bin/c-cortex-m4.elf",
        &[],
    )
    .printed(1);
    assert!(
        back.contains(
            "\n-16744  16744        0x8001074   strtod (gone, with 114 gone items it kept), kept \
             by Reset_Handler\n"
        ),
        "{back}"
    );
    // Each block's rows add up to what its items' bytes changed by.
    let answer = diff(
        "testdata/bin/c-cortex-m4.elf",
        "testdata/bin/c-strtod.elf",
        &["--json"],
    )
    .json(1);
    let change = |rows: &[&str]| -> i64 {
        rows.iter()
            .map(|row| {
                row.split_whitespace()
                    .next()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
            })
            .sum()
    };
    assert_eq!(change(&rows[..27]), answer["blocks"][0]["item_bytes"]);
    assert_eq!(change(&rows[27..]), answer["blocks"][1]["item_bytes"]);
}

#[test]
fn builds_that_load_the_same_memory_exit_0() {
    // Symbols, debug info and where the file keeps its bytes don't count.
    // rust-x86_64's .tbss shares its addresses with .fini_array, .init_array
    // and .data.rel.ro.
    for (old, new) in [
        (
            "testdata/bin/c-cortex-m7-nodebug.elf",
            "testdata/bin/c-cortex-m7-stripped.elf",
        ),
        (TCM_SAMPLE, "testdata/bin/c-cortex-m7-stripped.elf"),
        (
            "testdata/bin/c-cortex-m7-moved-nodebug.elf",
            "testdata/bin/c-cortex-m7-moved-stripped.elf",
        ),
        (
            "testdata/bin/c-x86_64-nodebug.elf",
            "testdata/bin/c-x86_64-stripped.elf",
        ),
        (MOVED_SAMPLE, "testdata/bin/c-x86_64-moved-stripped.elf"),
        (
            "testdata/bin/rust-x86_64-nodebug.elf",
            "testdata/bin/rust-x86_64-stripped.elf",
        ),
        (LIBRARY_SAMPLE, LIBRARY_SAMPLE),
    ] {
        let text = diff(old, new, &[]).printed(0);
        assert_output(
            &text,
            &format!("{old} and {new} hold the same bytes at the same addresses.\n"),
        );
    }
    // Each file holds the same memory as itself.
    for sample in samples() {
        diff(&sample, &sample, &[]).printed(0);
    }
}

#[test]
fn builds_that_load_other_memory_exit_1() {
    for (old, new) in [
        (TCM_SAMPLE, TCM_MOVED_SAMPLE),
        (TCM_MOVED_SAMPLE, TCM_SAMPLE),
        (TCM_SAMPLE, TCM_SDRAM_SAMPLE),
        (SAMPLE, MOVED_SAMPLE),
        ("testdata/bin/c-x86_64.so", "testdata/bin/c-x86_64-moved.so"),
        (
            "testdata/bin/c-x86_64-nodebug.elf",
            "testdata/bin/c-x86_64-moved-nodebug.elf",
        ),
        (
            "testdata/bin/c-cortex-m7-stripped.elf",
            "testdata/bin/c-cortex-m7-moved-stripped.elf",
        ),
        (TCM_SAMPLE, TCM_INLINED_SAMPLE),
    ] {
        diff(old, new, &[]).printed(1);
        diff(old, new, &["--json"]).json(1);
    }
}

#[test]
fn diff_needs_two_builds_of_one_program_with_section_headers() {
    assert_output(
        &stderr(&[TCM_SAMPLE, "diff", SAMPLE]),
        "error: testdata/bin/c-cortex-m7.elf is 32-bit Arm and testdata/bin/c-x86_64.elf is \
         x86-64; diff compares two builds of one program\n",
    );
    // The firmware, with no section headers.
    let path = without_section_headers(TCM_SAMPLE).save("c-cortex-m7-no-section-headers.elf");
    let path = path.as_str();
    assert_output(
        &stderr(&[TCM_SAMPLE, "diff", path]),
        &format!("error: {path} has no section headers, which diff needs to find its memory\n"),
    );
    assert_output(
        &stderr(&[path, "diff", TCM_SAMPLE, "--json"]),
        &format!("error: {path} has no section headers, which diff needs to find its memory\n"),
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "diff", "Cargo.toml"]),
        "error: cannot load Cargo.toml: not an ELF file\n",
    );
    assert_output(
        &stderr(&["missing.elf", "diff", TCM_SAMPLE]),
        "error: cannot read missing.elf: No such file or directory (os error 2)\n",
    );
}

/// Runs `elfq OLD diff NEW --json`.
#[track_caller]
fn diff_json(old: &str, new: &str) -> Run {
    run(&[old, "diff", new, "--json"])
}

#[test]
fn diff_has_each_block_and_its_sections() {
    let answer = diff_json(TCM_SAMPLE, TCM_MOVED_SAMPLE).json(1);
    let blocks = answer["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 4);
    // The sections that differ are named in address order, those whose
    // size or bytes differ, and .ARM.exidx, which moved.
    let mut head = answer.clone();
    head.as_object_mut().unwrap().remove("blocks");
    head.as_object_mut().unwrap().remove("items");
    head.as_object_mut().unwrap().remove("crossings");
    assert_eq!(
        head,
        json!({
            "old": TCM_SAMPLE,
            "new": TCM_MOVED_SAMPLE,
            "same_bytes": false,
            "items_compared": true,
            "old_bytes": 7835,
            "new_bytes": 7883,
            "unchanged": 61,
            "sections_that_differ": [".itcm", ".isr_vector", ".text", ".rodata", ".ARM.exidx"],
        })
    );
    // The ITCM's items lost 20 bytes: filter_tap's 52 left, crc32 grew by
    // 16, and two veneers came in; the padding after them grew by 4.
    assert_eq!(
        blocks[0],
        json!({
            "start": "0x0",
            "end": "0xe0",
            "old_size": 224,
            "new_size": 208,
            "change": -16,
            "same_bytes": false,
            "item_bytes": -20,
            "section_bytes": 0,
            "padding_bytes": 4,
            "sections": [{
                "name": ".itcm",
                "load_image": false,
                "holds_items": true,
                "old_address": "0x0",
                "old_size": 224,
                "new_address": "0x0",
                "new_size": 208
            }]
        })
    );
    assert_eq!(blocks[1]["same_bytes"], true);
    // .rodata grew, so .ARM.exidx after it moved, but kept its size.
    assert_eq!(
        blocks[3]["sections"][5],
        json!({
            "name": ".ARM.exidx",
            "load_image": false,
            "holds_items": false,
            "old_address": "0x30000ed4",
            "old_size": 8,
            "new_address": "0x30000ee4",
            "new_size": 8
        })
    );
    // A block that only one build has.
    let answer = diff_json(TCM_SAMPLE, TCM_SDRAM_SAMPLE).json(1);
    let gone = &answer["blocks"][1];
    assert_eq!(
        [
            &gone["start"],
            &gone["old_size"],
            &gone["new_size"],
            &gone["change"]
        ],
        [
            &json!("0x20000000"),
            &json!(1104),
            &Value::Null,
            &json!(-1104)
        ]
    );
    assert_eq!(gone["sections"][0]["new_address"], Value::Null);
    // The same memory has its blocks too, and each row is a line.
    let stripped = "testdata/bin/c-cortex-m7-stripped.elf";
    let text = diff_json(TCM_SAMPLE, stripped).printed(0);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "{text}");
    assert_eq!(
        lines[0],
        r#"{"old":"testdata/bin/c-cortex-m7.elf","new":"testdata/bin/c-cortex-m7-stripped.elf","#
            .to_string()
            + r#""same_bytes":true,"items_compared":false,"old_bytes":null,"new_bytes":null,"#
            + r#""unchanged":null,"sections_that_differ":[],"blocks":["#
    );
    for line in &lines[1..5] {
        let row: Value = serde_json::from_str(line.trim_end_matches(',')).unwrap();
        assert_eq!(row["same_bytes"], true, "{line}");
    }
    assert_eq!(lines[5], "],\"items\":[],\"crossings\":[]}");
}

#[test]
fn diff_has_each_item_that_changed() {
    let answer = diff_json(TCM_SAMPLE, TCM_MOVED_SAMPLE).json(1);
    let items = answer["items"].as_array().unwrap();
    // Largest change first, and of the same change, by name.
    let names: Vec<&str> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            ".rodata@0x30000de0",
            "crc32",
            "__filter_tap_veneer",
            "__log_msg_veneer",
            "filter_tap"
        ]
    );
    assert_eq!(
        items[1],
        json!({
            "name": "crc32",
            "was": null,
            "kind": "function",
            "status": "grew",
            "paired_by": "name",
            "change": 16,
            "old_address": "0x10",
            "old_size": 48,
            "old_section": ".itcm",
            "old_block": "0x0",
            "old_kept_by": {"name": "__crc32_veneer", "address": "0x300009b0"},
            "old_retained": 1072,
            "new_address": "0x6c",
            "new_size": 64,
            "new_section": ".itcm",
            "new_block": "0x0",
            "new_kept_by": {"name": "__crc32_veneer", "address": "0x300009b8"},
            "new_retained": 1112,
            "size_estimated": false,
            "inlined_into": null
        })
    );
    assert_eq!(items[4]["status"], "moved");
    assert_eq!(items[4]["new_block"], "0x30000000");
    // A gone function says where the new build's DWARF says the compiler
    // inlined it.
    let answer = diff_json(TCM_SAMPLE, TCM_INLINED_SAMPLE).json(1);
    let gone = &answer["items"][0];
    assert_eq!(gone["name"], "decode_header");
    assert_eq!(gone["status"], "gone");
    assert_eq!(gone["new_address"], Value::Null);
    assert_eq!(gone["old_kept_by"]["name"], "process_frame");
    assert_eq!(gone["inlined_into"], json!(["process_frame"]));
    // An edited item keeps its size, and `unchanged` counts only the items
    // that are the same.
    let answer = diff_json(TCM_SAMPLE, TCM_EDITED_SAMPLE).json(1);
    let edited: Vec<(&str, &str, i64)> = answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let text = |key: &str| item[key].as_str().unwrap();
            (
                text("name"),
                text("status"),
                item["change"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        edited,
        [("decode_header", "edited", 0), ("fir_coeffs", "edited", 0)]
    );
    assert_eq!(answer["unchanged"], 61);
}

#[test]
fn diff_has_each_use_between_blocks_that_is_new_or_gone() {
    let answer = diff_json(TCM_SAMPLE, TCM_MOVED_SAMPLE).json(1);
    let crossings = answer["crossings"].as_array().unwrap();
    // Two calls from the ITCM to flash are new, through veneers, and so is
    // crc32's use of its new message; filter_tap, now in flash, no longer
    // uses fir_coeffs from the ITCM.
    let rows: Vec<[&str; 4]> = crossings
        .iter()
        .map(|row| {
            ["status", "how"]
                .map(|key| row[key].as_str().unwrap())
                .into_iter()
                .chain(["caller", "item"].map(|key| row[key]["name"].as_str().unwrap()))
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
        .collect();
    assert_eq!(
        rows,
        [
            ["new", "call", "apply_filter", "filter_tap"],
            ["new", "call", "crc32", "log_msg"],
            ["new", "address taken", "crc32", ".rodata@0x30000de0"],
            ["gone", "address taken", "filter_tap", "fir_coeffs"],
        ]
    );
    // Each is a row of `callees BLOCK --json` for its caller's block, in
    // the build that has it.
    for row in crossings {
        let build = match row["status"].as_str() {
            Some("new") => TCM_MOVED_SAMPLE,
            _ => TCM_SAMPLE,
        };
        let caller = hex(&row["caller"]["address"]);
        let block = answer["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| hex(&block["start"]) <= caller && caller < hex(&block["end"]))
            .unwrap();
        let range = format!(
            "{}..{}",
            block["start"].as_str().unwrap(),
            block["end"].as_str().unwrap()
        );
        let callees = run(&[build, "callees", &range, "--json"]).json(1);
        let mut listed = row.clone();
        listed.as_object_mut().unwrap().remove("status");
        assert!(
            callees["callees"].as_array().unwrap().contains(&listed),
            "{row}"
        );
    }
    // The docs' check for CI fails on the new calls, but passes when only
    // data moved, and when calls are only gone.
    assert!(!passes_ci_check(&answer));
    assert!(passes_ci_check(
        &diff_json(TCM_SAMPLE, TCM_SDRAM_SAMPLE).json(1)
    ));
    assert!(passes_ci_check(
        &diff_json(TCM_MOVED_SAMPLE, TCM_SAMPLE).json(1)
    ));
    // None in one block, nor without a symbol table.
    let stripped = "testdata/bin/c-cortex-m7-moved-stripped.elf";
    for (old, new) in [
        (SAMPLE, MOVED_SAMPLE),
        (LIBRARY_SAMPLE, "testdata/bin/c-x86_64-moved.so"),
        (TCM_SAMPLE, stripped),
    ] {
        assert_eq!(
            diff_json(old, new).json(1)["crossings"],
            json!([]),
            "{old} {new}"
        );
    }
    // So without a symbol table, the check fails, though it finds no new
    // call, unless the memory is the same.
    assert!(!passes_ci_check(&diff_json(TCM_SAMPLE, stripped).json(1)));
    let same = "testdata/bin/c-cortex-m7-stripped.elf";
    assert!(passes_ci_check(&diff_json(TCM_SAMPLE, same).json(0)));
}

/// Whether the check for CI in the docs passes, as jq works it out:
/// `(.same_bytes or .items_compared) and ([.crossings[] | select(.status==
/// "new" and (.how=="call" or .how=="tail call"))] | length == 0)`.
fn passes_ci_check(answer: &Value) -> bool {
    let new_call =
        |row: &Value| row["status"] == "new" && (row["how"] == "call" || row["how"] == "tail call");
    (answer["same_bytes"] == true || answer["items_compared"] == true)
        && !answer["crossings"].as_array().unwrap().iter().any(new_call)
}

/// Pairs of builds to compare, of every kind that diff sees.
const DIFF_PAIRS: [(&str, &str); 11] = [
    (TCM_SAMPLE, TCM_MOVED_SAMPLE),
    (TCM_SAMPLE, TCM_SDRAM_SAMPLE),
    (TCM_SAMPLE, TCM_INLINED_SAMPLE),
    (TCM_SAMPLE, TCM_EDITED_SAMPLE),
    (SAMPLE, MOVED_SAMPLE),
    (LIBRARY_SAMPLE, "testdata/bin/c-x86_64-moved.so"),
    (SAMPLE, LIBRARY_SAMPLE),
    ("testdata/bin/c-cortex-m4.elf", TCM_SAMPLE),
    ("testdata/bin/c-cortex-m4.elf", "testdata/bin/c-strtod.elf"),
    (
        "testdata/bin/c-cortex-m4.elf",
        "testdata/bin/rust-cortex-m4.elf",
    ),
    (SAMPLE, "testdata/bin/cpp-x86_64.elf"),
];

#[test]
fn diff_blocks_add_up() {
    for (old, new) in DIFF_PAIRS
        .iter()
        .flat_map(|&(old, new)| [(old, new), (new, old)])
    {
        let answer = diff_json(old, new).json(1);
        // What each item changed its block's items' bytes by: a moved item
        // leaves its old block and comes into its new one.
        let mut credits: BTreeMap<&str, i64> = BTreeMap::new();
        for item in answer["items"].as_array().unwrap() {
            let size = |side: &str| item[format!("{side}_size")].as_i64().unwrap_or(0);
            for (side, sign) in [("old", -1), ("new", 1)] {
                if let Some(block) = item[format!("{side}_block")].as_str() {
                    *credits.entry(block).or_default() += sign * size(side);
                }
            }
        }
        for block in answer["blocks"].as_array().unwrap() {
            let part = |key: &str| block[key].as_i64().unwrap();
            let start = block["start"].as_str().unwrap();
            assert_eq!(
                part("change"),
                part("item_bytes") + part("section_bytes") + part("padding_bytes"),
                "{old} {new} {start}"
            );
            assert_eq!(
                part("item_bytes"),
                credits.get(start).copied().unwrap_or(0),
                "{old} {new} {start}"
            );
        }
    }
}

#[test]
fn diff_says_what_keeps_an_item_as_top_does() {
    for (old, new) in DIFF_PAIRS {
        let answer = diff_json(old, new).json(1);
        if old.ends_with(".so") || new.ends_with(".so") {
            // With a shared library on either side, neither build has a
            // tree.
            for item in answer["items"].as_array().unwrap() {
                assert_eq!(item["old_kept_by"], Value::Null, "{old} {new}");
                assert_eq!(item["new_kept_by"], Value::Null, "{old} {new}");
                assert_eq!(item["new_retained"], Value::Null, "{old} {new}");
            }
            continue;
        }
        for (side, path) in [("old", old), ("new", new)] {
            let top = elfq_json(&[path, "top", "--all"]);
            let rows = top["items"].as_array().unwrap();
            let by_address: BTreeMap<&str, &Value> = rows
                .iter()
                .map(|row| (row["address"].as_str().unwrap(), row))
                .collect();
            for item in answer["items"].as_array().unwrap() {
                let Some(address) = item[format!("{side}_address")].as_str() else {
                    continue;
                };
                let kept_by = &item[format!("{side}_kept_by")];
                let retained = &item[format!("{side}_retained")];
                match by_address.get(address) {
                    // Something reaches it: its parent in top's tree keeps it.
                    Some(row) => {
                        let parent = match row["parent"].as_u64() {
                            Some(id) => {
                                let parent = &rows[id as usize];
                                json!({"name": parent["name"], "address": parent["address"]})
                            }
                            None => Value::Null,
                        };
                        assert_eq!(kept_by, &parent, "{path} {address}");
                        assert_eq!(retained, &row["retained"], "{path} {address}");
                    }
                    None => {
                        assert_eq!(kept_by, &Value::Null, "{path} {address}");
                        assert_eq!(retained, &Value::Null, "{path} {address}");
                    }
                }
            }
        }
    }
}

#[test]
fn diff_has_the_blocks_that_the_overview_lists() {
    // The blocks are worked out over both builds, so one can hold several
    // of one build's, but no build's block is split.
    for (old, new) in [
        (TCM_SAMPLE, TCM_MOVED_SAMPLE),
        (TCM_SAMPLE, TCM_SDRAM_SAMPLE),
        ("testdata/bin/c-cortex-m4.elf", TCM_SAMPLE),
        (SAMPLE, MOVED_SAMPLE),
        (SAMPLE, LIBRARY_SAMPLE),
        (
            "testdata/bin/rust-x86_64-nodebug.elf",
            "testdata/bin/rust-x86_64-stripped.elf",
        ),
        (
            "testdata/bin/far-gnu-m4-nodebug.elf",
            "testdata/bin/far-lld-m4-nodebug.elf",
        ),
    ] {
        // Builds that load the same memory exit with 0, and others with 1.
        let answer = diff_json(old, new).json(Exit::ZeroOrOne);
        for (path, side) in [(old, "old"), (new, "new")] {
            let overview = elfq_json(&[path]);
            let mut overview_blocks = overview["memory"].as_array().unwrap().iter().peekable();
            for block in answer["blocks"].as_array().unwrap() {
                let (start, end) = (hex(&block["start"]), hex(&block["end"]));
                // This build's blocks in this one.
                let mut inside = Vec::new();
                while let Some(next) = overview_blocks.next_if(|next| hex(&next["start"]) < end) {
                    assert!(hex(&next["start"]) >= start && hex(&next["end"]) <= end);
                    inside.push(next);
                }
                let size = &block[format!("{side}_size")];
                let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
                    assert_eq!(size, &Value::Null, "{old} {new} {block}");
                    continue;
                };
                let span = hex(&last["end"]) - hex(&first["start"]);
                assert_eq!(size, &json!(span), "{old} {new} {block}");
                // The same sections and load images, in the same order, but
                // .tbss, which isn't in memory at its addresses.
                let listed = |key: &str| -> Vec<&str> {
                    inside
                        .iter()
                        .flat_map(|block| block[key].as_array().unwrap())
                        .map(|name| name.as_str().unwrap())
                        .filter(|&name| name != ".tbss")
                        .collect()
                };
                let in_block = |load_image: bool| -> Vec<&str> {
                    let address = format!("{side}_address");
                    let mut sections: Vec<(u64, &str)> = block["sections"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|section| section["load_image"] == load_image)
                        .filter(|section| !section[&address].is_null())
                        .map(|section| (hex(&section[&address]), section["name"].as_str().unwrap()))
                        .collect();
                    sections.sort_by_key(|&(address, _)| address);
                    sections.into_iter().map(|(_, name)| name).collect()
                };
                assert_eq!(in_block(false), listed("sections"), "{old} {new} {block}");
                assert_eq!(in_block(true), listed("load_images"), "{old} {new} {block}");
            }
            assert!(overview_blocks.next().is_none(), "{path}");
        }
    }
}

/// An address that the JSON writes in hex, such as "0x20000000".
fn hex(address: &Value) -> u64 {
    let address = address.as_str().unwrap();
    u64::from_str_radix(address.trim_start_matches("0x"), 16).unwrap()
}

// The tests below run diff on builds made by changing a sample's
// bytes with `ElfBytes`, against the sample itself.

#[test]
fn diff_names_the_file_that_a_warning_is_about() {
    let path = past_the_end(&[".plt"]).save("plt-offset-diff.elf");
    let compared = diff(SAMPLE, &path, &[]).exited(1);
    assert_output(
        &compared.stderr,
        &format!("warning: {path}: section .plt runs past the end of the file\n"),
    );
    // The new build has no bytes for .plt, so the item that elfq makes of
    // it is gone, and the function that the unwind table finds there new.
    let stdout = compared.stdout;
    assert!(
        stdout.contains(
            "\n   -64   64       0x1020   .plt (gone)\n   \
             +64        64  0x1020   fn_0x1020 (new), which nothing reaches\n"
        ),
        "{stdout}"
    );
}

#[test]
fn diff_names_a_section_whose_bytes_alone_differ() {
    // Another build ID, as after a change only in debug info: no item
    // changed, and the last line says where the bytes differ.
    let mut data = ElfBytes::sample(SAMPLE);
    let build_id = data.section_bytes_mut(".note.gnu.build-id");
    *build_id.last_mut().unwrap() ^= 1;
    let path = data.save("build-id.elf");
    let text = diff(SAMPLE, &path, &[]).printed(1);
    assert!(
        text.ends_with(
            ": no item changed size or block, but bytes differ.\n\
             The memory stays at 15728 bytes.\n\
             \n\
             68 other items are the same.\n\
             Sections that differ: .note.gnu.build-id.\n"
        ),
        "{text}"
    );
    let json = diff(SAMPLE, &path, &["--json"]).printed(1);
    assert!(
        json.contains("\"sections_that_differ\":[\n\".note.gnu.build-id\"\n],"),
        "{json}"
    );
    // In firmware, the block's row says only that bytes differ. A byte of
    // the unwind table, which holds no item, edits none.
    let mut data = ElfBytes::sample(TCM_SAMPLE);
    data.section_bytes_mut(".ARM.exidx")[0] ^= 1;
    let text = diff(TCM_SAMPLE, &data.save("exidx-byte.elf"), &[]).printed(1);
    assert!(
        text.contains("\n0x30000000..0x30000edc  3804  3804       0  bytes differ\n"),
        "{text}"
    );
    assert!(
        text.ends_with(
            "\n\n63 other items are the same.\n\
             Sections that differ: .ARM.exidx.\n"
        ),
        "{text}"
    );
    // In a shared library, with no row, nothing says that rows can't say
    // what keeps items.
    let mut data = ElfBytes::sample(LIBRARY_SAMPLE);
    data.section_bytes_mut(".eh_frame_hdr")[0x10] ^= 1;
    let text = diff(LIBRARY_SAMPLE, &data.save("eh-frame-hdr-byte.so"), &[]).printed(1);
    assert!(
        text.ends_with(
            ": no item changed size or block, but bytes differ.\n\
             The memory stays at 15936 bytes.\n\
             \n\
             72 other items are the same.\n\
             Sections that differ: .eh_frame_hdr.\n"
        ),
        "{text}"
    );
}

#[test]
fn diff_says_where_a_gone_function_was_inlined_only_when_its_name_finds_nothing() {
    // The build where decode_header is inlined, with its variable
    // systick_count named decode_header: a gone function's name that finds
    // an item names no function that is only inlined.
    let path = ElfBytes::sample(TCM_INLINED_SAMPLE)
        .rename_symbol("systick_count", "decode_header")
        .save("renamed.elf");
    let text = diff(TCM_SAMPLE, &path, &[]).printed(1);
    assert!(
        text.contains("  decode_header (gone), kept by process_frame\n"),
        "{text}"
    );
}

#[test]
fn diff_says_when_only_blocks_differ() {
    // .ARM.exidx, which holds no item, at an address of its own: a new
    // block, and the old one ends sooner, but every item is where it was.
    let path = ElfBytes::sample(TCM_SAMPLE)
        .set_section(".ARM.exidx", SectionField::Address, 0x6000_0000)
        .save("exidx-elsewhere.elf");
    let text = diff(TCM_SAMPLE, &path, &[]).printed(1);
    assert!(
        text.contains(": no item changed, but the blocks of memory differ.\n"),
        "{text}"
    );
    assert!(
        text.contains("\n0x60000000..0x60000008 (new)     -     8      +8  .ARM.exidx\n"),
        "{text}"
    );
    assert!(
        text.ends_with("\nSections that differ: .ARM.exidx.\n"),
        "{text}"
    );
}

#[test]
fn diff_counts_the_bytes_that_sections_overlap_on_once() {
    // .rodata, grown by 8 bytes, holds the bytes of .ARM.exidx after it
    // too, at the same addresses.
    let path = ElfBytes::sample(TCM_SAMPLE)
        .set_section(".rodata", SectionField::Size, 0xf4 + 8)
        .save("rodata-over-exidx.elf");
    let text = diff(TCM_SAMPLE, &path, &[]).printed(0);
    assert!(text.ends_with(" hold the same bytes at the same addresses.\n"));
}

#[test]
fn diff_refuses_a_separate_debug_file() {
    // objcopy --only-keep-debug keeps the header of each section that the
    // program loads, but makes it take no room in the file, as .bss does.
    let mut data = ElfBytes::sample(TCM_SAMPLE);
    for index in 0..data.header(HeaderField::SectionHeaderCount) as usize {
        if data.section_at(index, SectionField::Flags) & elf::SHF_ALLOC.0 != 0 {
            data.set_section_at(index, SectionField::Type, elf::SHT_NOBITS);
        }
    }
    let path = data.save("c-cortex-m7.debug");
    assert_output(
        &diff(TCM_SAMPLE, &path, &[]).error(),
        &format!(
            "error: {path} stores none of its code, as a separate debug file doesn't; diff \
             needs the program itself\n"
        ),
    );
}

#[test]
fn diff_refuses_a_file_that_puts_nothing_in_memory() {
    let mut data = ElfBytes::sample(TCM_SAMPLE);
    for index in 0..data.header(HeaderField::SectionHeaderCount) as usize {
        let flags = data.section_at(index, SectionField::Flags);
        data.set_section_at(index, SectionField::Flags, flags & !elf::SHF_ALLOC.0);
    }
    let path = data.save("c-cortex-m7-unallocated.elf");
    assert_output(
        &diff(TCM_SAMPLE, &path, &[]).error(),
        &format!("error: {path} puts nothing in memory, so diff has nothing to compare\n"),
    );
}
