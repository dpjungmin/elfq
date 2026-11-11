//! `callers` and `callees`: what uses SYMBOL and what it uses.
//!
//! The tests check them as text and as JSON, and with ranges of memory,
//! patterns or several SYMBOLs, or with `--from` or `--to`, what crosses
//! the edge of what those pick. The two commands share their table and
//! their ranges, so many of these tests check both.

use serde_json::{Value, json};

use elfq::binary::{ItemKind, Source};

use crate::support::{
    FAR_SAMPLES, LIBRARY_SAMPLE, SAMPLE, TCM_SAMPLE, assert_output, crossing, elfq_json,
    load_sample, names, run, stderr, stdout,
};

#[test]
fn callers_lists_the_largest_callers() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let callers = stdout(&[sample, "callers", "_Unwind_Resume"]);
    let lines: Vec<&str> = callers.lines().collect();
    assert_eq!(lines[0], "_Unwind_Resume@plt is used by 100 functions:");
    assert_eq!(lines[2], "HOW   AT                  CALLER");
    // 30 callers, then a row that counts the rest.
    assert_eq!(lines.len(), 34, "{callers}");
    assert!(lines[3..33].iter().all(|line| line.starts_with("call  ")));
    assert_eq!(lines[33], "                          70 more");
    let all = stdout(&[sample, "callers", "_Unwind_Resume", "--all"]);
    assert_eq!(all.lines().count(), 103, "{all}");
    // JSON has every caller either way.
    let json = stdout(&[sample, "callers", "_Unwind_Resume", "--json"]);
    let json: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["callers"].as_array().unwrap().len(), 100);
}

#[test]
fn callees_lists_the_largest_callees_in_the_order_of_the_code() {
    // The 30 largest of 54, in the order the code first uses them. Of the
    // 8-byte objects, those whose names sort first are kept, and the
    // imports that std::rt::lang_start_internal calls, such as poll and
    // abort, count as 0 bytes. "24 more" counts items, not rows.
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callees",
            "std::rt::lang_start_internal",
        ]),
        "\
std::rt::lang_start_internal uses 54 items:

HOW            AT                  CALLEE
uses           0x3a17f, 0x3a19e    <std::thread::id::ThreadId>::new::COUNTER
uses           0x3a1ca             .rodata@0x71c0
uses           0x3a249 and 3 more  .rodata@0x6044
uses           0x3a31b             .rodata@0x79e8
call           0x3a525, 0x3a5a1    std::sys::pal::unix::stack_overflow::imp::make_handler
call           0x3a53b, 0x3a5b8    std::sys::pal::unix::stack_overflow::thread_info::set_current_info
address taken  0x3a54a, 0x3a5c4    std::sys::pal::unix::stack_overflow::imp::signal_handler
uses           0x3a5ea             std::sys::args::unix::imp::ARGC.0
call           0x3a654             <std::thread::id::ThreadId>::new::exhausted
uses           0x3a674             .data.rel.ro@0x52e80
uses           0x3a67b             .data.rel.ro@0x53628
call           0x3a68b             <std::sys::sync::once::futex::Once>::call
uses           0x3a693             .rodata@0x89fd
call           0x3a6a3             <std::sys::stdio::unix::Stderr as std::io::Write>::write_fmt
call           0x3a6b6             core::ptr::drop_glue::<core::result::Result<(), std::io::error::Error>>
call           0x3a6bb             std::process::abort
uses           0x3a6c1             .rodata@0x87ef
call           0x3a6cd             core::panicking::panic_nounwind
uses           0x3a6d3             .data.rel.ro@0x538f8
uses           0x3a6e0             .data.rel.ro@0x538e0
call           0x3a6f5             core::panicking::assert_failed::<i32, i32>
uses           0x3a700             .rodata@0x80a8
uses           0x3a707             .data.rel.ro@0x53970
call           0x3a713             core::panicking::panic
uses           0x3a71e             .data.rel.ro@0x53910
call           0x3a725             core::panicking::panic_const::panic_const_rem_by_zero
call           0x3a733 and 2 more  __rustc::__rust_panic_cleanup
uses           0x3a73f, 0x3a7dd    std::panicking::panic_count::GLOBAL_PANIC_COUNT
call           0x3a7cd             std::rt::lang_start_internal::{closure#0}::{closure#0}::{closure#1}
call           0x3a802             std::rt::handle_rt_panic::<isize>
                                   24 more

Also 7 calls and 1 jump through pointers, at 0x3a1fa, 0x3a206, 0x3a329 and 5 more, which elfq can't follow.
",
    );
}

#[test]
fn callers_lists_each_caller_and_how_it_calls() {
    assert_output(
        &stdout(&[SAMPLE, "callers", "log_msg"]),
        "\
log_msg is used by 5 functions:

HOW        AT              CALLER
call       0x16e7          app_tick
call       0x1aaf          debug_dump
tail call  0x1661          on_button
call       0x18ee, 0x1908  process_frame
call       0x1a72          report
",
    );
}

#[test]
fn callers_in_a_shared_library_include_calls_through_its_stubs() {
    assert_output(
        &stdout(&[LIBRARY_SAMPLE, "callers", "log_msg"]),
        "\
log_msg is used by 5 functions:

HOW        AT              CALLER
call       0x179f          app_tick
call       0x1b5f          debug_dump
tail call  0x1711          on_button
call       0x199e, 0x19b8  process_frame
call       0x1b22          report

This includes calls through log_msg@plt.
",
    );
    // The stub itself is still there to ask about.
    assert_output(
        &stdout(&[LIBRARY_SAMPLE, "callees", "log_msg@plt"]),
        "\
log_msg@plt uses 1 function:

HOW        AT      CALLEE
tail call  0x1144  log_msg
",
    );
    // The same goes for an 8-byte stub in `.plt.got`, and for an ifunc's.
    const PLT: &str = "testdata/bin/c-plt.so";
    assert_output(
        &stdout(&[PLT, "callers", "negate"]),
        "\
negate is used by 2 functions:

HOW            AT      CALLER
address taken  0x1210  pick
call           0x126a  run

This includes calls through negate@plt.
Calls to negate through a pointer aren't listed, since elfq can't see them.
",
    );
    assert_output(
        &stdout(&[PLT, "callers", "sum_to"]),
        "\
sum_to is used by 1 function:

HOW   AT      CALLER
call  0x1273  run

This includes calls through sum_to@plt.
",
    );
    // Code that only takes the address of an ifunc's stub takes the
    // ifunc's address, and calls nothing through the stub.
    assert_output(
        &stdout(&[PLT, "callers", "square"]),
        "\
square is used by 1 function:

HOW            AT      CALLER
address taken  0x1240  pick_square

Calls to square through a pointer aren't listed, since elfq can't see them.
",
    );
}

#[test]
fn callees_also_reports_what_it_cannot_follow() {
    assert_output(
        &stdout(&[SAMPLE, "callees", "dispatch_event"]),
        "\
dispatch_event uses 1 data object:

HOW   AT      CALLEE
uses  0x169b  handlers

Also 1 jump through a pointer, at 0x16ad, which elfq can't follow.
",
    );
    const RUST_M4: &str = "testdata/bin/rust-cortex-m4.elf";
    assert!(
        stdout(&[RUST_M4, "callees", "<core::fmt::Formatter>::pad_integral"]).ends_with(
            "\nAlso 5 calls and 1 jump through pointers, at 0x8001004, 0x80010c4, 0x80010d6 \
             and 3 more, which elfq can't follow.\n"
        )
    );
    // With nothing else to list, the sentence leads.
    assert_output(
        &stdout(&[
            RUST_M4,
            "callees",
            "<core::fmt::Formatter>::pad_integral::write_prefix",
        ]),
        "<core::fmt::Formatter>::pad_integral::write_prefix makes 1 call and 1 jump through \
         pointers, at 0x8001744 and 0x8001762, which elfq can't follow.\n",
    );
}

#[test]
fn code_uses_data_whether_it_reads_it_or_loads_its_address() {
    // lang_start_internal reads CLEANUP in one place and loads its address
    // in another: both are one row.
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "std::rt::cleanup::CLEANUP",
        ]),
        "\
std::rt::cleanup::CLEANUP is used by 1 function:

HOW   AT                CALLER
uses  0x3a60a, 0x3a66d  std::rt::lang_start_internal
",
    );
    // Arm code loads a variable's address before it reads or writes it,
    // and that is "uses" too.
    let stats = stdout(&["testdata/bin/c-cortex-m4.elf", "callers", "stats"]);
    assert!(
        stats.contains("\nuses  0x8000332  process_frame\n"),
        "{stats}"
    );
}

#[test]
fn callees_of_ranges_lists_what_leaves_them() {
    // ITCM and DTCM together: only what is in neither, at the call that
    // goes through the linker's veneer.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callees", "0x0+256K", "0x20000000..0x20040000"]),
        "\
0x00000000..0x00040000 (.itcm) and 0x20000000..0x20040000 (.data and .bss) use 3 items outside them:

ADDRESS     SIZE  HOW                   AT    CALLEE
0x3000087c   308  call via veneer       0xbc  memcpy, used by apply_filter
0x300002e4    60  tail call via veneer  0x8   app_tick, used by SysTick_Handler
0x30000eb4    32  uses                  0x56  fir_coeffs, used by filter_tap
",
    );
    // The same without symbols, where the veneers are known by their code.
    assert_output(
        &crossing(&[
            "testdata/bin/c-cortex-m7-stripped.elf",
            "callees",
            "0x0+256K",
            "0x20000000+256K",
        ]),
        "\
0x00000000..0x00040000 (.itcm) and 0x20000000..0x20040000 (.data and .bss) use 3 items outside them:

ADDRESS     SIZE  HOW                   AT    CALLEE
0x3000087c  ~308  call via veneer       0xbc  fn_0x3000087c, used by fn_0x74
0x300002e4   ~60  tail call via veneer  0x8   fn_0x300002e4, used by fn_0x0
0x30000eb4   ~32  uses                  0x56  .rodata@0x30000eb4, used by fn_0x40
",
    );
    // ITCM alone: DTCM is outside it too.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callees", "0x0+256K"]),
        "\
0x00000000..0x00040000 (.itcm) uses 5 items outside it:

ADDRESS     SIZE  HOW                   AT    CALLEE
0x20000000  1024  uses                  0x18  crc_table, used by crc32
0x3000087c   308  call via veneer       0xbc  memcpy, used by apply_filter
0x300002e4    60  tail call via veneer  0x8   app_tick, used by SysTick_Handler
0x30000eb4    32  uses                  0x56  fir_coeffs, used by filter_tap
0x20000444     4  uses                  0x0   systick_count, used by SysTick_Handler
",
    );
    // Data that holds no pointers uses nothing: exit status 0.
    assert_output(
        &stdout(&[TCM_SAMPLE, "callees", "0x20000000+256K"]),
        "0x20000000..0x20040000 (.data and .bss) uses nothing outside it.\n",
    );
}

#[test]
fn callees_of_ranges_names_who_calls_through_a_veneer() {
    // Code in RAM calls flash and a routine in the chip's ROM, through
    // veneers that GNU ld adds, and through a register for a long call.
    // elfq can't tell whether the ROM has a function where the tail call
    // goes, so it is a jump.
    assert_output(
        &crossing(&["testdata/bin/far-gnu-m4-nodebug.elf", "callees", "0x0+64K"]),
        "\
0x00000000..0x00010000 (.ram) uses 4 items outside it; elfq can't follow 1 call and 1 jump:

ADDRESS     SIZE  HOW                   AT    CALLEE
0x1fff0000        call via veneer       0x46  (outside any function), used by ram_calls_rom
0x1fff0000        jump via veneer       0x60  (outside any function), used by ram_tails_rom
0x30000100    16  call                  0x74  flash_far, used by ram_long_calls_flash
0x300000b8    16  tail call via veneer  0x34  flash_tail, used by ram_tails_flash
0x300000a8    16  call via veneer       0x1a  flash_work, used by ram_calls_flash
0x20000000     4  uses                  0x0   counter, used by ram_leaf
0x20000000     4  uses                  0x10  counter, used by ram_calls_flash
0x20000000     4  uses                  0x2c  counter, used by ram_tails_flash
0x20000000     4  uses                  0x3e  counter, used by ram_calls_rom
0x20000000     4  uses                  0x58  counter, used by ram_tails_rom
0x20000000     4  uses                  0x68  counter, used by ram_long_calls_flash
",
    );
    // From flash, through the veneers that LLD adds for Cortex-M0, which
    // jump through the stack.
    assert_output(
        &crossing(&["testdata/bin/far-lld-m0-nodebug.elf", "callers", "0x0+64K"]),
        "\
0x00000000..0x00010000 (.ram) is used by 2 functions outside it:

ADDRESS     SIZE  HOW              AT          CALLER
0x30000124    32  call via veneer  0x30000126  Reset_Handler, uses ram_calls_flash
0x30000124    32  call via veneer  0x3000012a  Reset_Handler, uses ram_tails_flash
0x30000124    32  call via veneer  0x3000012e  Reset_Handler, uses ram_calls_rom
0x30000124    32  call via veneer  0x30000132  Reset_Handler, uses ram_tails_rom
0x30000124    32  call via veneer  0x30000136  Reset_Handler, uses ram_long_calls_flash
0x300000dc    28  call via veneer  0x300000e6  flash_calls_ram, uses ram_leaf
",
    );
}

#[test]
fn callees_of_ranges_lists_first_what_elfq_cannot_follow() {
    let flash = crossing(&[TCM_SAMPLE, "callees", "0x30000000+16M"]);
    let lines: Vec<&str> = flash.lines().take(6).collect();
    assert_eq!(
        lines,
        [
            "0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx) uses 14 items \
             outside it; elfq can't follow 1 jump:",
            "",
            "ADDRESS     SIZE  HOW              AT                      CALLEE",
            "                  jump             0x300002da              (through a pointer), used by dispatch_event",
            "0x20240000  4096  uses             0x30000674, 0x30000696  tx_buffer, used by platform_write",
            "0x74          92  call via veneer  0x30000386, 0x30000454  apply_filter, used by process_frame",
        ]
    );
    // An item that several items use has a row for each of them, and every
    // row names both.
    assert!(
        flash.contains(
            "\n0x20000400    32  uses             0x3000038a              stats, used by process_frame\n\
             0x20000400    32  uses             0x300004de              stats, used by report\n"
        ),
        "{flash}"
    );
    // glibc's _init has no size, so its code can't be read.
    assert_output(
        &crossing(&[SAMPLE, "callees", "0x1000+4"]),
        "\
In the code elfq can read, 0x00001000..0x00001004 (.init) uses nothing outside it.
elfq doesn't know where _init ends, so it can't read its code.
",
    );
}

#[test]
fn callers_of_ranges_lists_what_uses_them_from_outside() {
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "0x0+256K"]),
        "\
0x00000000..0x00040000 (.itcm) is used by 2 items outside it:

ADDRESS     SIZE  HOW              AT                      CALLER
0x30000320   376  call via veneer  0x3000034c              process_frame, uses crc32
0x30000320   376  call via veneer  0x30000386, 0x30000454  process_frame, uses apply_filter
0x30000000    64  pointer          0x3000003c              vector_table, uses SysTick_Handler
",
    );
    // Code outside may call a function through a pointer to it.
    assert_output(
        &crossing(&[SAMPLE, "callers", "0x1260+0x20"]),
        "\
0x00001260..0x00001280 (.text) is used by 1 function outside it:

ADDRESS  SIZE  HOW            AT      CALLER
0x10a0    195  address taken  0x10a8  main, uses on_timer
0x10a0    195  address taken  0x10c7  main, uses on_data

Calls through a pointer to on_timer and on_data aren't listed, since elfq can't see them.
",
    );
    assert_eq!(
        crossing(&[TCM_SAMPLE, "callers", "0x20240000+4K"])
            .lines()
            .next(),
        Some("0x20240000..0x20241000 (.ocram_bss) is used by 1 function outside it:"),
    );
    // Nothing is outside all the memory, but the handlers' addresses are
    // taken, and could be passed on.
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callers",
            "0x0+16M",
            "0x20000000+16M",
            "0x30000000+16M",
        ]),
        "Nothing outside 0x00000000..0x01000000 (.itcm), 0x20000000..0x21000000 (.data, .bss \
         and .ocram_bss) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and \
         .ARM.exidx) uses them directly.\n\
         Calls through a pointer to on_timer, on_data and on_button aren't listed, since elfq \
         can't see them.\n",
    );
}

#[test]
fn from_and_to_list_only_what_is_in_other_ranges() {
    // What in ITCM uses DTCM.
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callers",
            "0x20000000+256K",
            "--from",
            "0x0+256K",
        ]),
        "\
0x20000000..0x20040000 (.data and .bss) is used by 2 functions in 0x00000000..0x00040000 (.itcm):

ADDRESS  SIZE  HOW   AT    CALLER
0x10       48  uses  0x18  crc32, uses crc_table
0x0        16  uses  0x0   SysTick_Handler, uses systick_count
",
    );
    // What ITCM uses in flash, which leaves out DTCM.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x30000000+16M"]),
        "\
0x00000000..0x00040000 (.itcm) uses 3 items in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx):

ADDRESS     SIZE  HOW                   AT    CALLEE
0x3000087c   308  call via veneer       0xbc  memcpy, used by apply_filter
0x300002e4    60  tail call via veneer  0x8   app_tick, used by SysTick_Handler
0x30000eb4    32  uses                  0x56  fir_coeffs, used by filter_tap
",
    );
    // Each option takes one range, and can be given again.
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "--from",
            "0x30000000+16M",
            "--from",
            "0x20000000+256K",
        ]),
        "\
0x00000000..0x00040000 (.itcm) is used by 2 items in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx) and 0x20000000..0x20040000 (.data and .bss):

ADDRESS     SIZE  HOW              AT                      CALLER
0x30000320   376  call via veneer  0x3000034c              process_frame, uses crc32
0x30000320   376  call via veneer  0x30000386, 0x30000454  process_frame, uses apply_filter
0x30000000    64  pointer          0x3000003c              vector_table, uses SysTick_Handler
",
    );
    // Nothing there: exit status 0.
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "callers",
            "0x20000000+256K",
            "--from",
            "0x20240000+512K",
        ]),
        "Nothing in 0x20240000..0x202c0000 (.ocram_bss) uses 0x20000000..0x20040000 (.data and \
         .bss).\n",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x20240000+512K"]),
        "0x00000000..0x00040000 (.itcm) uses nothing in 0x20240000..0x202c0000 (.ocram_bss).\n",
    );
}

#[test]
fn from_and_to_keep_what_elfq_cannot_follow_that_may_be_there() {
    // A call through a pointer may go to a function whose address is taken
    // or stored, as on_timer's and on_data's are.
    assert_output(
        &crossing(&[SAMPLE, "callees", "0x1180+0x80", "--to", "0x1260+0x20"]),
        "\
0x00001180..0x00001200 (.text) makes 2 jumps that elfq can't follow, which may go to 0x00001260..0x00001280 (.text):

ADDRESS  SIZE  HOW   AT      CALLEE
               jump  0x11bf  (through a pointer), used by deregister_tm_clones
               jump  0x1200  (through a pointer), used by register_tm_clones
",
    );
    assert_output(
        &crossing(&[SAMPLE, "callers", "0x1260+0x20", "--from", "0x1180+0x80"]),
        "\
Nothing in 0x00001180..0x00001200 (.text) uses 0x00001260..0x00001280 (.text) directly.
Calls through a pointer to on_timer and on_data aren't listed, since elfq can't see them.
",
    );
    // Nor when every such function in the --to ranges is in the ranges
    // asked about too.
    assert_output(
        &stdout(&[
            SAMPLE,
            "callees",
            "0x1180..0x1280",
            "--to",
            "0x1180..0x1300",
        ]),
        "0x00001180..0x00001280 (.text) uses nothing in 0x00001180..0x00001300 (.text).\n",
    );
    // Code that elfq can't read, as glibc's _init, may use the ranges in
    // any way, and both sides say so.
    assert_output(
        &crossing(&[SAMPLE, "callers", "0x1260+0x20", "--from", "0x1000+1"]),
        "\
In the code elfq can read, nothing in 0x00001000..0x00001001 (.init) uses 0x00001260..0x00001280 (.text) directly.
Calls through a pointer to on_timer and on_data aren't listed, since elfq can't see them.
elfq doesn't know where _init ends, so it can't read its code.
",
    );
    assert_output(
        &crossing(&[SAMPLE, "callees", "0x1000+1", "--to", "0x1260+0x20"]),
        "\
In the code elfq can read, 0x00001000..0x00001001 (.init) uses nothing in 0x00001260..0x00001280 (.text).
elfq doesn't know where _init ends, so it can't read its code.
",
    );
    let plt = "testdata/bin/c-plt.so";
    for args in [
        &[plt, "callers", "0x11f0+4", "--from", "0x1000+1", "--json"],
        &[plt, "callees", "0x1000+1", "--to", "0x11f0+4", "--json"],
    ] {
        let json = crossing(args);
        assert!(
            json.contains("],\"unreadable\":[\n{\"name\":\"_init\",")
                && json.ends_with("],\"complete\":false}\n"),
            "{json}"
        );
    }
    // Not to data, nor to functions whose addresses only the vector table
    // holds; and code that makes no such call doesn't make one.
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "callees",
            "0x300002cc+24",
            "--to",
            "0x20240000+4K",
        ]),
        "0x300002cc..0x300002e4 (.text) uses nothing in 0x20240000..0x20241000 (.ocram_bss).\n",
    );
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callees",
            "0x30000000+0x2e0",
            "--to",
            "0x0+256K",
        ]),
        "\
0x30000000..0x300002e0 (.isr_vector and .text) uses 1 function in 0x00000000..0x00040000 (.itcm):

ADDRESS  SIZE  HOW      AT          CALLEE
0x0        16  pointer  0x3000003c  SysTick_Handler, used by vector_table
",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "callers", "0x30000000+16M", "--from", "0x10+48"]),
        "Nothing in 0x00000010..0x00000040 (.itcm) uses 0x30000000..0x31000000 (.isr_vector, \
         .text, .rodata and .ARM.exidx).\n",
    );
    // A call to an address outside every function goes there, as to a
    // routine in the chip's ROM, which --to can name though the file has
    // nothing there, when code in it calls there.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    assert_output(
        &crossing(&[far, "callees", "0x0+64K", "--to", "0x1fff0000+64K"]),
        "\
0x00000000..0x00010000 (.ram) makes 1 call and 1 jump that elfq can't follow, which go to 0x1fff0000..0x20000000:

ADDRESS     SIZE  HOW              AT    CALLEE
0x1fff0000        call via veneer  0x46  (outside any function), used by ram_calls_rom
0x1fff0000        jump via veneer  0x60  (outside any function), used by ram_tails_rom
",
    );
    assert_output(
        &crossing(&[far, "callees", "0x3c+28", "--to", "0x1fff0000+64K"]),
        "\
0x0000003c..0x00000058 (.ram) makes 1 call that elfq can't follow, which goes to 0x1fff0000..0x20000000:

ADDRESS     SIZE  HOW              AT    CALLEE
0x1fff0000        call via veneer  0x46  (outside any function), used by ram_calls_rom
",
    );
    assert_output(
        &stdout(&[far, "callees", "0x0..0x3c", "--to", "0x1fff0000+64K"]),
        "0x00000000..0x0000003c (.ram) uses nothing in 0x1fff0000..0x20000000.\n",
    );
    // callers lists the same calls from the other side, and a range that
    // holds only such code needs no --from.
    assert_output(
        &crossing(&[far, "callers", "0x1fff0000+64K"]),
        "\
0x1fff0000..0x20000000 is used by 3 functions outside it:

ADDRESS     SIZE  HOW              AT          CALLER
0x3c          28  call via veneer  0x46        ram_calls_rom, uses 0x1fff0000 (outside any function)
0x58          16  jump via veneer  0x60        ram_tails_rom, uses 0x1fff0000 (outside any function)
0x300000e4    28  call via veneer  0x300000ee  flash_calls_rom, uses 0x1fff0000 (outside any function)
",
    );
    assert_output(
        &crossing(&[
            far,
            "callers",
            "0x1fff0000..0x20000004",
            "--from",
            "0x300000e4..0x30000100",
        ]),
        "\
0x1fff0000..0x20000004 (.bss) is used by 1 function in 0x300000e4..0x30000100 (.text):

ADDRESS     SIZE  HOW              AT          CALLER
0x300000e4    28  call via veneer  0x300000ee  flash_calls_rom, uses 0x1fff0000 (outside any function)
0x300000e4    28  uses             0x300000e6  flash_calls_rom, uses counter
",
    );
    // The veneer's own jump, when only it is in the --from ranges.
    assert_output(
        &crossing(&[
            far,
            "callers",
            "0x1fff0000..0x20000004",
            "--from",
            "0x30000138+8",
        ]),
        "\
0x1fff0000..0x20000004 (.bss) is used by 1 function in 0x30000138..0x30000140 (.text):

ADDRESS     SIZE  HOW   AT          CALLER
0x30000138     8  jump  0x30000138  __rom_routine_veneer, uses 0x1fff0000 (outside any function)
",
    );
    assert_output(
        &crossing(&[far, "callees", "0x0+64K", "--to", "0x30000000+16M"]),
        "\
0x00000000..0x00010000 (.ram) uses 3 functions in 0x30000000..0x31000000 (.isr_vector and .text):

ADDRESS     SIZE  HOW                   AT    CALLEE
0x30000100    16  call                  0x74  flash_far, used by ram_long_calls_flash
0x300000b8    16  tail call via veneer  0x34  flash_tail, used by ram_tails_flash
0x300000a8    16  call via veneer       0x1a  flash_work, used by ram_calls_flash
",
    );
}

#[test]
fn from_and_to_see_a_veneer_on_the_far_side() {
    // A call goes into the --to ranges at a veneer there, which jumps on
    // elsewhere; callers asks the same from the other side.
    let tcm = |args: &[&str]| crossing(&[&[TCM_SAMPLE][..], args].concat());
    assert_output(
        &tcm(&["callees", "0x0..0xd0", "--to", "0xd0+256K"]),
        "\
0x00000000..0x000000d0 (.itcm) uses 2 functions in 0x000000d0..0x000400d0 (.itcm):

ADDRESS  SIZE  HOW        AT    CALLEE
0xd0        8  tail call  0x8   __app_tick_veneer, used by SysTick_Handler
0xd8        8  call       0xbc  __memcpy_veneer, used by apply_filter
",
    );
    assert_output(
        &tcm(&["callers", "0xd0+256K", "--from", "0x0..0xd0"]),
        "\
0x000000d0..0x000400d0 (.itcm) is used by 2 functions in 0x00000000..0x000000d0 (.itcm):

ADDRESS  SIZE  HOW        AT    CALLER
0x74       92  call       0xbc  apply_filter, uses __memcpy_veneer
0x0        16  tail call  0x8   SysTick_Handler, uses __app_tick_veneer
",
    );
    // The veneers' jumps, when only they are in the --from ranges.
    assert_output(
        &tcm(&["callers", "0x30000000+16M", "--from", "0xd0..0xe0"]),
        "\
0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx) is used by 2 functions in 0x000000d0..0x000000e0 (.itcm):

ADDRESS  SIZE  HOW        AT    CALLER
0xd0        8  tail call  0xd0  __app_tick_veneer, uses app_tick
0xd8        8  tail call  0xd8  __memcpy_veneer, uses memcpy
",
    );
    assert_output(
        &tcm(&["callees", "0xd0..0xe0", "--to", "0x30000000+16M"]),
        "\
0x000000d0..0x000000e0 (.itcm) uses 2 functions in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx):

ADDRESS     SIZE  HOW        AT    CALLEE
0x3000087c   308  tail call  0xd8  memcpy, used by __memcpy_veneer
0x300002e4    60  tail call  0xd0  app_tick, used by __app_tick_veneer
",
    );
    // A veneer in both is neither on the far side, nor used there.
    assert_output(
        &tcm(&["callees", "0x0..0xe0", "--to", "0x0..0x20000010"]),
        "\
0x00000000..0x000000e0 (.itcm) uses 1 data object in 0x00000000..0x20000010 (.itcm and .data):

ADDRESS     SIZE  HOW   AT    CALLEE
0x20000000  1024  uses  0x18  crc_table, used by crc32
",
    );
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "0x300009b0..0x300009c0",
            "--from",
            "0x0..0x20000010",
            "--from",
            "0x300009b0..0x30001000",
        ]),
        "Nothing in 0x00000000..0x20000010 (.itcm and .data) and 0x300009b0..0x30001000 (.text, \
         .rodata and .ARM.exidx) uses 0x00000000..0x00040000 (.itcm) and \
         0x300009b0..0x300009c0 (.text).\n",
    );
    // A call to a veneer that jumps to the chip's ROM.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    let calls_rom = crossing(&[
        far,
        "callees",
        "0x30000000..0x30000130",
        "--to",
        "0x30000130..0x30000168",
    ]);
    assert!(
        calls_rom.contains(
            "\n0x30000138     8  call  0x300000ee  __rom_routine_veneer, used by flash_calls_rom\n"
        ),
        "{calls_rom}"
    );
}

#[test]
fn from_and_to_keep_their_rules_for_code_outside_any_function() {
    // Both sides list ram_tails_rom's jump to the ROM, at its veneer.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    assert_output(
        &crossing(&[far, "callers", "0x1fff0000+64K", "--from", "0x58..0x98"]),
        "\
0x1fff0000..0x20000000 is used by 1 function in 0x00000058..0x00000098 (.ram):

ADDRESS  SIZE  HOW              AT    CALLER
0x58       16  jump via veneer  0x60  ram_tails_rom, uses 0x1fff0000 (outside any function)
",
    );
    assert_output(
        &crossing(&[far, "callees", "0x58..0x98", "--to", "0x1fff0000+64K"]),
        "\
0x00000058..0x00000098 (.ram) makes 1 jump that elfq can't follow, which goes to 0x1fff0000..0x20000000:

ADDRESS     SIZE  HOW              AT    CALLEE
0x1fff0000        jump via veneer  0x60  (outside any function), used by ram_tails_rom
",
    );
    // callees asks about code it can read, so not about the ROM alone.
    assert_output(
        &stderr(&[far, "callees", "0x1fff0000+64K"]),
        "error: the file has nothing in 0x1fff0000..0x20000000; the overview lists its ranges\n",
    );
    // A --to range must hold something outside the ranges asked about, and
    // code there that the file calls counts only when it is outside them.
    assert_output(
        &stderr(&[
            far,
            "callees",
            "0x1fff0000..0x20000004",
            "--to",
            "0x1fff0000..0x20000004",
        ]),
        "error: --to 0x1fff0000..0x20000004 holds nothing outside 0x1fff0000..0x20000004, and \
         callees lists only what it uses outside it\n",
    );
    assert_output(
        &stderr(&[
            far,
            "callers",
            "0x0+64K",
            "0x1fff0000..0x1fff0004",
            "--from",
            "0x1fff0000..0x20000004",
        ]),
        "error: --from 0x1fff0000..0x20000004 and 0x1fff0000..0x1fff0004 both hold the code at \
         0x1fff0000; give ranges that don't share it\n",
    );
    // A tail call between two functions of the ranges isn't listed, nor a
    // call through a pointer from code in them: only app_tick is outside.
    assert_output(
        &crossing(&[
            SAMPLE,
            "callers",
            "0x1260..0x16c0",
            "--from",
            "0x1260..0x1700",
        ]),
        "\
0x00001260..0x000016c0 (.text) is used by 1 function in 0x00001260..0x00001700 (.text):

ADDRESS  SIZE  HOW        AT              CALLER
0x16c0     51  tail call  0x16dc, 0x16f1  app_tick, uses dispatch_event
0x16c0     51  call       0x16e7          app_tick, uses log_msg
",
    );
}

#[test]
fn from_and_to_see_through_a_plt_stub() {
    // A shared library calls its own functions through PLT stubs, as
    // `callers log_msg` says.
    let library = "testdata/bin/c-x86_64.so";
    assert_output(
        &crossing(&[library, "callers", "log_msg", "--from", "0x1180..0x2000"]),
        "\
log_msg is used by 5 functions in 0x00001180..0x00002000 (.text and .fini):

ADDRESS  SIZE  HOW                AT              CALLER
0x17b0    532  call via PLT       0x199e, 0x19b8  process_frame, uses log_msg
0x19d0    370  call via PLT       0x1b22          report, uses log_msg
0x1770     62  call via PLT       0x179f          app_tick, uses log_msg
0x1b50     29  call via PLT       0x1b5f          debug_dump, uses log_msg
0x1700     22  tail call via PLT  0x1711          on_button, uses log_msg

elfq doesn't know where _fini ends, so it can't read its code.
",
    );
    // When the caller and the function are on one side, the stub's jump
    // crosses back.
    assert!(
        crossing(&[library, "callers", "0x1180..0x2000"])
            .contains("\n0x1140     16  tail call  0x1144  log_msg@plt, uses log_msg\n")
    );
    let plt = "testdata/bin/c-plt.so";
    assert_output(
        &crossing(&[plt, "callers", "twice", "--from", "0x1250+62"]),
        "\
twice is used by 1 function in 0x00001250..0x0000128e (.text):

ADDRESS  SIZE  HOW           AT      CALLER
0x1250     62  call via PLT  0x1258  run, uses twice
",
    );
    assert_output(
        &crossing(&[plt, "callees", "run", "--to", "0x11f0+4"]),
        "\
run uses 1 function in 0x000011f0..0x000011f4 (.text):

ADDRESS  SIZE  HOW           AT      CALLEE
0x11f0      4  call via PLT  0x1258  twice, used by run
",
    );
    // A lazy stub's jump to the PLT's header is its own.
    assert_output(
        &crossing(&[plt, "callers", "0x1020+16", "--from", "0x1030+16"]),
        "\
0x00001020..0x00001030 (.plt) is used by 1 function in 0x00001030..0x00001040 (.plt):

ADDRESS  SIZE  HOW        AT      CALLER
0x1030     16  tail call  0x103b  twice@plt, uses .plt
",
    );
}

#[test]
fn from_and_to_take_symbol_as_the_range_it_occupies() {
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "crc32", "--from", "0x30000000+16M"]),
        "\
crc32 is used by 1 function in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx):

ADDRESS     SIZE  HOW              AT          CALLER
0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32
",
    );
    assert_output(
        &crossing(&[TCM_SAMPLE, "callees", "process_frame", "--to", "0x0+256K"]),
        "\
process_frame uses 2 functions in 0x00000000..0x00040000 (.itcm):

ADDRESS  SIZE  HOW              AT                      CALLEE
0x74       92  call via veneer  0x30000386, 0x30000454  apply_filter, used by process_frame
0x10       48  call via veneer  0x3000034c              crc32, used by process_frame
",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "callers", "crc32", "--from", "0x0+256K"]),
        "Nothing in 0x00000000..0x00040000 (.itcm) uses crc32.\n",
    );
    // A function of unknown size counts as its first byte.
    let init = crossing(&[SAMPLE, "callers", "_init", "--from", "0x0+16M"]);
    assert!(
        init.starts_with("In the code elfq can read, nothing in 0x00000000..0x01000000 (.interp, ")
            && init.ends_with(
                ") uses _init.\nelfq doesn't know where _fini ends, so it can't read its code.\n"
            ),
        "{init}"
    );
    // An alias names its item.
    assert!(
        crossing(&[
            TCM_SAMPLE,
            "callers",
            "NMI_Handler",
            "--from",
            "0x30000000+16M"
        ])
        .starts_with("Default_Handler (also named NMI_Handler) is used by 1 data object in "),
    );
}

#[test]
fn ranges_in_place_of_symbol_must_be_ranges_with_something_in_them() {
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "0x40000000+4K"]),
        "error: the file has nothing in 0x40000000..0x40001000; the overview lists its ranges\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x100..0x0"]),
        "error: `0x100..0x0` isn't a range: the range must end after it starts\n",
    );
    // --range is for sizes; ranges to check go in place of SYMBOL.
    for args in [
        &[TCM_SAMPLE, "callees", "--range", "0+1K"],
        &[TCM_SAMPLE, "--range", "0+1K", "callees"],
    ] {
        assert_output(
            &stderr(args),
            "error: callees takes ranges in place of SYMBOL, not with --range: \
             elfq testdata/bin/c-cortex-m7.elf callees 0x00000000..0x00000400\n",
        );
    }
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "--range", "0+1K", "--json"]),
        "error: callees takes ranges in place of SYMBOL, not with --range: \
         elfq testdata/bin/c-cortex-m7.elf callees 0x00000000..0x00000400 --json\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "--range",
            "0x20000000+256K",
        ]),
        "error: callers takes ranges in place of SYMBOL, not with --range: \
         elfq testdata/bin/c-cortex-m7.elf callers 0x0+256K 0x20000000..0x20040000; to list \
         only the callers in 0x20000000..0x20040000: elfq testdata/bin/c-cortex-m7.elf callers \
         0x0+256K --from 0x20000000..0x20040000\n",
    );
    // Each command only when it answers: not with --from when --range is in
    // a range given, where nothing is outside them, or shares an item with
    // them, as app_tick; and neither when the file has nothing there.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0+256K", "--range", "0x0+1K"]),
        "error: callers takes ranges in place of SYMBOL, not with --range: \
         elfq testdata/bin/c-cortex-m7.elf callers 0x0+256K 0x00000000..0x00000400\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x30000000+0x400",
            "--range",
            "0x30000300+0x400",
        ]),
        "error: callers takes ranges in place of SYMBOL, not with --range: \
         elfq testdata/bin/c-cortex-m7.elf callers 0x30000000+0x400 0x30000300..0x30000700\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "--range",
            "0x40000000+16",
        ]),
        "error: only the overview, top and retained take --range\n",
    );
    // Keeping the --from given.
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "--range",
            "0x20000000+256K",
            "--from",
            "0x0+256K",
        ]),
        "error: callers takes ranges in place of SYMBOL, not with --range: \
         elfq testdata/bin/c-cortex-m7.elf callers 0x20000000..0x20040000 --from \
         0x00000000..0x00040000\n",
    );
    // With SYMBOL, --from or --to is what --range can mean, when that
    // answers: not for an import, nor for code that elfq can't read, nor
    // for a range in SYMBOL, nor where the file has nothing.
    for args in [
        &[SAMPLE, "callers", "__libc_start_main", "--range", "0+16M"][..],
        &[TCM_SAMPLE, "callers", "crc32", "--range", "0x10+48"],
        &[
            TCM_SAMPLE,
            "callees",
            "process_frame",
            "--range",
            "0x30000320+16",
        ],
        &[TCM_SAMPLE, "callers", "crc32", "--range", "0x40000000+16"],
        // Nor without SYMBOL.
        &[TCM_SAMPLE, "callers", "--range", "0x40000000+16"],
        &[
            TCM_SAMPLE, "callers", "--range", "0x0+256K", "--from", "0x0+1K",
        ],
        &[
            TCM_SAMPLE,
            "callees",
            "--range",
            "0x30000000+16M",
            "--to",
            "0x30000320+376",
        ],
    ] {
        assert_output(
            &stderr(args),
            "error: only the overview, top and retained take --range\n",
        );
    }
    // When no command answers, and the one given fails without --range
    // too, its error comes first.
    for (args, error) in [
        (
            &[SAMPLE, "callees", "_init", "--range", "0+16M"][..],
            "error: elfq doesn't know where _init ends, so it can't read its code\n",
        ),
        (
            &[
                TCM_SAMPLE,
                "callers",
                "0x4000000+256K",
                "--range",
                "0x30000000+16M",
            ],
            "error: the file has nothing in 0x04000000..0x04040000; the overview lists its \
             ranges\n",
        ),
        (
            &[
                TCM_SAMPLE,
                "callees",
                "process_frame",
                "--to",
                "0x40000000+16",
                "--range",
                "0x0+256K",
            ],
            "error: --to 0x40000000..0x40000010 holds no functions or data, and no code in the \
             file calls or jumps there\n",
        ),
    ] {
        assert_output(&stderr(args), error);
    }
    // A mistyped name comes first.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "crc32x", "--range", "0+1K"]),
        "error: no function or data object is named `crc32x`; did you mean crc32?\n",
    );
    // As with one SYMBOL, for several, or patterns.
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "crc32",
            "apply_filter",
            "--range",
            "0+1K",
        ]),
        "error: callers takes --from, not --range, to list only the callers in a range: \
         elfq testdata/bin/c-cortex-m7.elf callers crc32 apply_filter --from \
         0x00000000..0x00000400\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "on_*", "--range", "0+1K"]),
        "error: callers takes --from, not --range, to list only the callers in a range: \
         elfq testdata/bin/c-cortex-m7.elf callers 'on_*' --from 0x00000000..0x00000400\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "crc32", "--range", "0+1K"]),
        "error: callees takes --to, not --range, to list only the callees in a range: \
         elfq testdata/bin/c-cortex-m7.elf callees crc32 --to 0x00000000..0x00000400\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "crc32",
            "--from",
            "0x30000000+16M",
            "--range",
            "0+1K",
        ]),
        "error: callers takes --from, not --range, to list only the callers in a range: \
         elfq testdata/bin/c-cortex-m7.elf callers crc32 --from 0x30000000..0x31000000 --from \
         0x00000000..0x00000400\n",
    );
    // The ranges of --from and --to must each hold something outside the
    // ranges asked about, or they are most likely mistyped.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0+256K", "--from", "0x40000000+4K"]),
        "error: the file has nothing in 0x40000000..0x40001000; the overview lists its ranges\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x0+1K"]),
        "error: --to 0x00000000..0x00000400 holds nothing outside 0x00000000..0x00040000, and \
         callees lists only what it uses outside it\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callees",
            "0x0+256K",
            "0x20000000+256K",
            "--to",
            "0x10+16",
        ]),
        "error: --to 0x00000010..0x00000020 holds nothing outside 0x00000000..0x00040000 and \
         0x20000000..0x20040000, and callees lists only what they use outside them\n",
    );
    // --to can name a ROM only when code calls there.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x1fff0000+64K"]),
        "error: --to 0x1fff0000..0x20000000 holds no functions or data, and no code in the \
         file calls or jumps there\n",
    );
    // An item in both would be on both sides, unless everything in the
    // ranges asked about is in the others too.
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x30000498..0x300005b2",
            "--from",
            "0x30000570..0x300005f6",
        ]),
        "error: --from 0x30000570..0x300005f6 and 0x30000498..0x300005b2 both hold main; give \
         ranges that don't share it\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callees",
            "0x30000668+96",
            "0x2000044a+2",
            "--to",
            "0x20000444..0x2000044a",
        ]),
        "error: --to 0x20000444..0x2000044a and 0x2000044a..0x2000044c both hold tx_tail; give \
         ranges that don't share it\n",
    );
    // Each range on its own, though another is fine.
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "0x20000000+256K",
            "--from",
            "0x20240000+512K",
            "--from",
            "0x20000000+1K",
        ]),
        "error: --from 0x20000000..0x20000400 holds nothing outside 0x00000000..0x00040000 and \
         0x20000000..0x20040000, and callers lists only what uses them from outside\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "crc32", "--from", "0x10+16"]),
        "error: --from 0x00000010..0x00000020 holds nothing outside crc32, and callers lists \
         only what uses it from outside\n",
    );
    // An import has no range in the file, and callees can't read code
    // whose end elfq doesn't know.
    assert_output(
        &stderr(&[SAMPLE, "callers", "__libc_start_main", "--from", "0x0+16M"]),
        "error: __libc_start_main is imported from a shared library, so it has no range in \
         this file for --from\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "callees", "_init", "--to", "0x0+16M"]),
        "error: elfq doesn't know where _init ends, so it can't read its code\n",
    );
    // Each option takes one range.
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "0x0+256K",
            "--from",
            "0x0+1K,0x20000000+1K",
        ]),
        "error: invalid value '0x0+1K,0x20000000+1K' for '--from <RANGE>': --from takes one \
         range; give it again for each: --from 0x0+1K --from 0x20000000+1K\n\n\
         For more information, try '--help'.\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x0+1K,"]),
        "error: invalid value '0x0+1K,' for '--to <RANGE>': --to takes one range; leave out \
         the comma\n\n\
         For more information, try '--help'.\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers"]),
        "error: callers needs a SYMBOL, or patterns or ranges, such as 'uart_*' or 0x0+256K\n",
    );
}

#[test]
fn a_pattern_answers_as_ranges_do() {
    // Every row, what crosses the edge of the items it matches, and the
    // exit status 1 when something does.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "on_*"]),
        "The 3 items named on_* are used by 1 function outside them:\n\
         \n\
         ADDRESS     SIZE  HOW            AT          CALLER\n\
         0x30000570   132  address taken  0x30000572  main, uses on_timer\n\
         0x30000570   132  address taken  0x3000057a  main, uses on_button\n\
         0x30000570   132  address taken  0x30000582  main, uses on_data\n\
         \n\
         Calls through a pointer to on_timer, on_data and on_button aren't listed, since \
         elfq can't see them.\n",
    );
    // Even when it matches one item, so that the answer keeps its shape
    // when a build adds or drops a match; its name alone keeps the answer
    // about the item itself.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "crc3*"]),
        "crc32 (the one item named crc3*) is used by 1 function outside it:\n\
         \n\
         ADDRESS     SIZE  HOW              AT          CALLER\n\
         0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32\n",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "callers", "crc32"]),
        "crc32 is used by 1 function:\n\
         \n\
         HOW        AT          CALLER\n\
         tail call  0x300009b0  __crc32_veneer\n",
    );
}

#[test]
fn several_symbols_answer_together_with_ranges_and_patterns() {
    assert_output(
        &crossing(&[TCM_SAMPLE, "callees", "on_timer", "on_data"]),
        "on_timer and on_data use 2 data objects outside them:\n\
         \n\
         ADDRESS     SIZE  HOW   AT          CALLEE\n\
         0x2000042c     4  uses  0x30000150  data_events, used by on_data\n\
         0x20000434     4  uses  0x30000140  timer_ticks, used by on_timer\n",
    );
    // An item given twice counts once, and so does a range.
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "crc32", "crc32"]),
        "crc32 is used by 1 function outside it:\n\
         \n\
         ADDRESS     SIZE  HOW              AT          CALLER\n\
         0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32\n",
    );
    assert_output(
        &crossing(&[TCM_SAMPLE, "callers", "0x0+16", "crc32", "0x0+16"]),
        "SysTick_Handler and crc32 are used by 2 items outside them:\n\
         \n\
         ADDRESS     SIZE  HOW              AT          CALLER\n\
         0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32\n\
         0x30000000    64  pointer          0x3000003c  vector_table, uses SysTick_Handler\n",
    );
    // The first line names each part, in the order given, up to three.
    let leaves = crossing(&[TCM_SAMPLE, "callers", "leaf_*", "0x0..0x40000"]);
    assert!(
        leaves.starts_with(
            "The 3 items named leaf_* and the 6 items in 0x00000000..0x00040000 (.itcm) are \
             used by 4 items outside them:\n"
        ),
        "{leaves}"
    );
    let six = crossing(&[TCM_SAMPLE, "callers", "on_*", "crc32", "report", "main"]);
    assert_output(
        &six,
        "These 6 items are used by 2 functions outside them:\n\
         \n\
         ADDRESS     SIZE  HOW              AT          CALLER\n\
         0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32\n\
         0x300005f8   112  call             0x30000642  Reset_Handler, uses main\n\
         \n\
         Calls through a pointer to on_timer, on_data and on_button aren't listed, since \
         elfq can't see them.\n",
    );
}

#[test]
fn a_set_names_a_range_of_code_outside_every_item_by_its_addresses() {
    // The far sample calls a routine in the chip's ROM, at 0x1fff0000,
    // where no item is, so no item's name can name that range.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    let first_line = |args: &[&str]| {
        let answer = crossing(&[&[far, "callers"][..], args].concat());
        answer.lines().next().unwrap_or_default().to_string()
    };
    assert_eq!(
        first_line(&["ram_calls_rom", "0x1fff0000+64K"]),
        "ram_calls_rom and 0x1fff0000..0x20000000 are used by 3 functions outside them:"
    );
    assert_eq!(
        first_line(&["flash_*", "0x1fff0000+64K"]),
        "The 5 items named flash_* and 0x1fff0000..0x20000000 are used by 7 functions \
         outside them:"
    );
    // The errors name it the same way.
    assert_output(
        &stderr(&[
            far,
            "callers",
            "0x1fff0000+64K",
            "ram_leaf",
            "--from",
            "0x0+16",
        ]),
        "error: --from 0x00000000..0x00000010 holds nothing outside 0x1fff0000..0x20000000 \
         and ram_leaf, and callers lists only what uses them from outside\n",
    );
}

#[test]
fn a_set_notes_how_its_patterns_matched_first() {
    const SOFTFLOAT: &str = "testdata/bin/c-softfloat-nodebug.elf";
    assert_output(
        &crossing(&[SOFTFLOAT, "callers", "*df2"]),
        "The 4 items named *df2 are used by 1 function outside them:\n\
         \n\
         ADDRESS    SIZE  HOW   AT         CALLER\n\
         0x8000490    16  call  0x8000492  __aeabi_cdcmpeq, uses __cmpdf2\n\
         \n\
         __aeabi_f2d matches only through another of its names, __extendsfdf2.\n",
    );
    // With nothing to list, the note follows the first line.
    assert_output(
        &stdout(&[SOFTFLOAT, "callers", "__extendsf*", "__gedf*"]),
        "Nothing outside the 2 items named __extendsf* or __gedf* uses them.\n\
         __aeabi_f2d matches only through another of its names, __extendsfdf2.\n",
    );
}

#[test]
fn from_and_to_take_a_set() {
    // What in flash, outside memset and crc32, uses them.
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callers",
            "memset",
            "crc32",
            "--from",
            "0x30000000+16M",
        ]),
        "memset and crc32 are used by 2 functions in 0x30000000..0x31000000 (.isr_vector, \
         .text, .rodata and .ARM.exidx):\n\
         \n\
         ADDRESS     SIZE  HOW              AT          CALLER\n\
         0x30000320   376  call via veneer  0x3000034c  process_frame, uses crc32\n\
         0x300005f8   112  call             0x3000063e  Reset_Handler, uses memset\n",
    );
    // The ranges may hold some of the set's items, which stay inside, as
    // one SYMBOL's item does: process_frame is in flash, so its call to
    // crc32 isn't listed, but the veneer's jump is.
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callers",
            "crc32",
            "process_frame",
            "--from",
            "0x30000000+16M",
        ]),
        "crc32 and process_frame are used by 2 functions in 0x30000000..0x31000000 \
         (.isr_vector, .text, .rodata and .ARM.exidx):\n\
         \n\
         ADDRESS     SIZE  HOW        AT                     CALLER\n\
         0x30000570   132  call       0x30000596 and 2 more  main, uses process_frame\n\
         0x300009b0     8  tail call  0x300009b0             __crc32_veneer, uses crc32\n",
    );
    assert_output(
        &crossing(&[
            TCM_SAMPLE,
            "callees",
            "crc32",
            "process_frame",
            "--to",
            "0x0+256K",
        ]),
        "crc32 and process_frame use 1 function in 0x00000000..0x00040000 (.itcm):\n\
         \n\
         ADDRESS  SIZE  HOW              AT                      CALLEE\n\
         0x74       92  call via veneer  0x30000386, 0x30000454  apply_filter, used by \
         process_frame\n",
    );
    // But each range must hold something outside the set, and a range
    // given in the set may not share an item with them.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "on_*", "--from", "0x30000140+32"]),
        "error: --from 0x30000140..0x30000160 holds nothing outside the 3 items named on_*, \
         and callers lists only what uses them from outside\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "on_*", "--to", "0x30000140+32"]),
        "error: --to 0x30000140..0x30000160 holds nothing outside the 3 items named on_*, \
         and callees lists only what they use outside them\n",
    );
    assert_output(
        &stderr(&[
            TCM_SAMPLE,
            "callers",
            "process_frame",
            "0x0+16",
            "--from",
            "0x0+256K",
        ]),
        "error: --from 0x00000000..0x00040000 and 0x00000000..0x00000010 both hold \
         SysTick_Handler; give ranges that don't share it\n",
    );
}

#[test]
fn a_set_stops_at_the_first_argument_it_cant_read() {
    // A set needs each item's range, which an import has none of in the
    // file. callees can't answer for one alone either, so it says why.
    assert_output(
        &stderr(&[SAMPLE, "callers", "main", "__libc_start_main"]),
        "error: __libc_start_main is imported from a shared library, so it has no range in \
         this file to put with the others; give it alone\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "callees", "log_*", "__libc_start_main"]),
        "error: __libc_start_main is imported from a shared library, so its code isn't in \
         this file\n",
    );
    assert!(
        stdout(&[SAMPLE, "callers", "__libc_start_main"])
            .starts_with("__libc_start_main is used by 1 function:\n"),
    );
    // The first argument that it can't read gives the error.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "nosuch_*", "0x100..0x0"]),
        "error: no function or data object is named `nosuch_*`\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x100..0x0", "nosuch"]),
        "error: `0x100..0x0` isn't a range: the range must end after it starts\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "crc32", "0x40000000+16"]),
        "error: the file has nothing in 0x40000000..0x40000010; the overview lists its \
         ranges\n",
    );
    // A range is checked for what it holds only after every name and
    // pattern is found, so the name's error comes first here.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x40000000+16", "nosuch"]),
        "error: no function or data object is named `nosuch`\n",
    );
    let error = stderr(&[
        "testdata/bin/rust-cortex-m4.elf",
        "callers",
        "checksum",
        "crc32",
    ]);
    assert!(
        error.starts_with(
            "error: `checksum` could mean 3 functions; use one on the left instead, or \
             'checksum*' for all 3:\n"
        ),
        "{error}"
    );
    assert_output(
        &stderr(&[SAMPLE, "callees"]),
        "error: callees needs a SYMBOL, or patterns or ranges, such as 'uart_*' or 0x0+256K\n",
    );
}

#[test]
fn a_set_lists_its_symbols_and_matches_in_json() {
    // `ranges` lists only the ranges typed, each once, `symbols` the
    // SYMBOLs, and `matches` what each pattern matched, as in retained.
    let text = run(&[
        TCM_SAMPLE,
        "callees",
        "on_*",
        "crc32",
        "0x0+16",
        "0x0+16",
        "crc32",
        "--to",
        "0x20000000+256K",
        "--json",
    ])
    .printed(1);
    assert_eq!(
        keys_in_order(&text),
        [
            "ranges",
            "symbols",
            "matches",
            "to",
            "callees",
            "unresolved",
            "unreadable",
            "complete"
        ]
    );
    let callees: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        callees["ranges"],
        json!([{ "start": "0x0", "end": "0x10" }])
    );
    assert_eq!(callees["symbols"][0]["aliases"], json!([]));
    assert_eq!(names(&callees["symbols"]), ["crc32"]);
    assert_eq!(
        callees["matches"][0],
        json!({
            "pattern": "on_*",
            "name": "on_timer",
            "kind": "function",
            "section": ".text",
            "address": "0x30000140",
            "size": 16,
            "size_estimated": false,
            "matched_by": "name",
            "matched_as": "on_timer"
        })
    );
    assert_eq!(
        names(&callees["matches"]),
        ["on_timer", "on_data", "on_button"]
    );
    // Only what was given: no `matches` without a pattern, no `symbols`
    // without a SYMBOL.
    let text = run(&[
        TCM_SAMPLE,
        "callers",
        "memset",
        "crc32",
        "--from",
        "0x30000000+16M",
        "--json",
    ])
    .printed(1);
    assert_eq!(
        keys_in_order(&text),
        [
            "ranges",
            "symbols",
            "from",
            "callers",
            "unresolved",
            "unreadable",
            "through_pointer",
            "complete"
        ]
    );
    let text = run(&[TCM_SAMPLE, "callers", "on_*", "--json"]).printed(1);
    assert_eq!(keys_in_order(&text)[..3], ["ranges", "matches", "from"]);
    // Ranges alone and one SYMBOL keep their JSON.
    for args in [
        &["0x0+16", "0x10+4"][..],
        &["crc32", "--from", "0x30000000+16M"],
    ] {
        let text = run(&[&[TCM_SAMPLE, "callers", "--json"], args].concat()).printed(1);
        assert_eq!(keys_in_order(&text)[..2], ["ranges", "from"], "{text}");
    }
}

/// The keys of an answer's JSON, `text` as elfq printed it, in the order
/// printed. Each is where `"key":` is first printed, so the answer's rows
/// mustn't have a key of the same name.
fn keys_in_order(text: &str) -> Vec<String> {
    let answer: serde_json::Map<String, Value> = serde_json::from_str(text).unwrap();
    let mut keys: Vec<(usize, &String)> = (answer.keys())
        .map(|key| (text.find(&format!("\"{key}\":")).unwrap(), key))
        .collect();
    keys.sort();
    keys.into_iter().map(|(_, key)| key.clone()).collect()
}

#[test]
fn a_shared_library_has_the_same_callers_as_the_program() {
    // Each caller and how it calls, and whether that is every caller.
    let callers = |sample: &str, function: &str| {
        let answer = elfq_json(&[sample, "callers", function]);
        let callers: Vec<(Value, Value)> = answer["callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|caller| (caller["item"]["name"].clone(), caller["how"].clone()))
            .collect();
        (callers, answer["complete"].clone())
    };
    let library = load_sample(LIBRARY_SAMPLE);
    let functions = library
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function && item.source == Source::SymbolTable);
    let mut checked = 0;
    for function in functions {
        // In the program, `_start` hands `main` to the C library.
        if function.name == "main" {
            continue;
        }
        assert_eq!(
            callers(LIBRARY_SAMPLE, &function.name),
            callers(SAMPLE, &function.name),
            "{}",
            function.name
        );
        checked += 1;
    }
    assert!(checked > 20, "{checked}");
}

#[test]
fn callers_and_callees_list_every_use() {
    let callers = elfq_json(&[SAMPLE, "callers", "log_msg"]);
    assert_eq!(callers["symbol"]["name"], "log_msg");
    assert_eq!(
        names(&callers["callers"]),
        [
            "app_tick",
            "debug_dump",
            "on_button",
            "process_frame",
            "report"
        ]
    );
    assert_eq!(callers["callers"][2]["how"], "tail call");
    assert_eq!(callers["callers"][3]["sites"], json!(["0x18ee", "0x1908"]));
    assert_eq!(callers["complete"], true);
    // Unlike the HOW column, which says "uses" for both, JSON tells reading
    // data apart from loading its address.
    let cleanup = elfq_json(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "callers",
        "std::rt::cleanup::CLEANUP",
    ]);
    assert_eq!(cleanup["callers"][0]["how"], "access");
    assert_eq!(cleanup["callers"][1]["how"], "address taken");
    // Calls through a pointer to on_button don't show.
    assert_eq!(
        elfq_json(&[SAMPLE, "callers", "on_button"])["complete"],
        false
    );

    // The jump through the handler table goes where only the running
    // program knows.
    let callees = elfq_json(&[SAMPLE, "callees", "dispatch_event"]);
    assert_eq!(names(&callees["callees"]), ["handlers"]);
    assert_eq!(
        callees["unresolved"],
        json!([{ "site": "0x16ad", "how": "jump", "target": null }])
    );
    assert_eq!(callees["complete"], false);
}

#[test]
fn ranges_list_what_crosses_their_edge() {
    // Something leaves the range: exit status 1.
    let callees = run(&[TCM_SAMPLE, "callees", "0x0+256K", "--json"]).json(1);
    assert_eq!(
        callees["ranges"],
        json!([{ "start": "0x0", "end": "0x40000" }])
    );
    assert_eq!(
        names(&callees["callees"]),
        [
            "crc_table",
            "memcpy",
            "app_tick",
            "fir_coeffs",
            "systick_count"
        ]
    );
    let memcpy = &callees["callees"][1];
    assert_eq!(memcpy["caller"]["name"], "apply_filter");
    assert_eq!(memcpy["how"], "call");
    assert_eq!(memcpy["veneer"]["name"], "__memcpy_veneer");
    assert_eq!(memcpy["sites"], json!(["0xbc"]));
    assert_eq!(callees["callees"][0]["how"], "address taken");
    assert_eq!(callees["callees"][0]["veneer"], Value::Null);
    assert_eq!(callees["unresolved"], json!([]));
    assert_eq!(callees["unreadable"], json!([]));
    assert_eq!(callees["complete"], true);

    // Data that holds no pointers: nothing leaves, exit status 0.
    let dtcm = elfq_json(&[TCM_SAMPLE, "callees", "0x20000000+256K"]);
    assert_eq!(dtcm["callees"], json!([]));
    assert_eq!(dtcm["complete"], true);

    let callers = run(&[SAMPLE, "callers", "0x1260+0x20", "--json"]).json(1);
    assert_eq!(names(&callers["callers"]), ["main", "main"]);
    assert_eq!(callers["callers"][0]["callee"]["name"], "on_timer");
    assert_eq!(callers["callers"][0]["how"], "address taken");
    assert_eq!(names(&callers["through_pointer"]), ["on_timer", "on_data"]);
    assert_eq!(callers["complete"], false);
    assert_eq!(callers["from"], Value::Null);
    assert_eq!(callees["to"], Value::Null);
}

#[test]
fn from_and_to_list_their_ranges() {
    let callers = run(&[
        TCM_SAMPLE,
        "callers",
        "0x20000000+256K",
        "--from",
        "0x0+256K",
        "--json",
    ])
    .json(1);
    assert_eq!(
        callers["from"],
        json!([{ "start": "0x0", "end": "0x40000" }])
    );
    assert_eq!(names(&callers["callers"]), ["crc32", "SysTick_Handler"]);

    let callees = run(&[
        TCM_SAMPLE,
        "callees",
        "0x0+256K",
        "--to",
        "0x30000000+16M",
        "--to",
        "0x20240000+512K",
        "--json",
    ])
    .json(1);
    assert_eq!(
        callees["to"],
        json!([
            { "start": "0x30000000", "end": "0x31000000" },
            { "start": "0x20240000", "end": "0x202c0000" }
        ])
    );
    assert_eq!(
        names(&callees["callees"]),
        ["memcpy", "app_tick", "fir_coeffs"]
    );

    // callers lists the calls into code that no item holds, as callees
    // does.
    let callers = run(&[
        "testdata/bin/far-gnu-m4-nodebug.elf",
        "callers",
        "0x1fff0000+64K",
        "--from",
        "0x30000000+16M",
        "--json",
    ])
    .json(1);
    let unresolved = &callers["unresolved"][0];
    assert_eq!(unresolved["caller"]["name"], "flash_calls_rom");
    assert_eq!(unresolved["target"], "0x1fff0000");
    assert_eq!(unresolved["veneer"]["name"], "__rom_routine_veneer");
    assert_eq!(callers["callers"], json!([]));
    assert_eq!(callers["complete"], true);
    // And callees --to the ROM, the same call, as complete.
    let callees = run(&[
        "testdata/bin/far-gnu-m4-nodebug.elf",
        "callees",
        "0x30000000+16M",
        "--to",
        "0x1fff0000+64K",
        "--json",
    ])
    .json(1);
    assert_eq!(
        callees["unresolved"][0]["caller"]["name"],
        "flash_calls_rom"
    );
    assert_eq!(callees["unresolved"][0]["target"], "0x1fff0000");
    assert_eq!(callees["complete"], true);
}

/// The rows of a range check's JSON, by address rather than by name, so
/// that a stripped file gives the same: (item, caller, how, veneer, sites),
/// and for what elfq can't follow (target, caller, how, veneer, site).
fn rows_by_address(answer: &Value) -> Vec<[String; 5]> {
    let address = |item: &Value| item["address"].as_str().unwrap_or("").to_string();
    let mut rows: Vec<[String; 5]> = answer["callees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            [
                address(&row["item"]),
                address(&row["caller"]),
                row["how"].to_string(),
                address(&row["veneer"]),
                row["sites"].to_string(),
            ]
        })
        .collect();
    rows.extend(answer["unresolved"].as_array().unwrap().iter().map(|row| {
        [
            row["target"].to_string(),
            address(&row["caller"]),
            row["how"].to_string(),
            address(&row["veneer"]),
            row["site"].to_string(),
        ]
    }));
    rows.sort();
    rows
}

#[test]
fn ranges_follow_calls_through_veneers() {
    // Cortex-M0 has no tail call this far, and code built for it with
    // -mpure-code makes counter's address a byte at a time, which elfq
    // doesn't follow.
    for path in FAR_SAMPLES {
        let callees = run(&[path, "callees", "0x0+64K", "--json"]).json(1);
        let mut rows: Vec<(&str, &str, &str, bool)> = callees["callees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["item"]["name"].as_str().unwrap(),
                    row["caller"]["name"].as_str().unwrap(),
                    row["how"].as_str().unwrap(),
                    !row["veneer"].is_null(),
                )
            })
            .collect();
        rows.retain(|row| row.0 != "counter");
        let tail_call = if path.contains("m0") {
            "call"
        } else {
            "tail call"
        };
        assert_eq!(
            rows,
            [
                ("flash_far", "ram_long_calls_flash", "call", false),
                ("flash_tail", "ram_tails_flash", tail_call, true),
                ("flash_work", "ram_calls_flash", "call", true),
            ],
            "{path}"
        );
        // The ROM routine has no item, so elfq can't follow the calls, and
        // can't tell whether the tail call goes to a function.
        let jump = if path.contains("m0") { "call" } else { "jump" };
        let unresolved: Vec<(&str, &str, &str, bool)> = callees["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["caller"]["name"].as_str().unwrap(),
                    row["how"].as_str().unwrap(),
                    row["target"].as_str().unwrap(),
                    row["veneer"]["name"]
                        .as_str()
                        .unwrap()
                        .contains("rom_routine"),
                )
            })
            .collect();
        assert_eq!(
            unresolved,
            [
                ("ram_calls_rom", "call", "0x1fff0000", true),
                ("ram_tails_rom", jump, "0x1fff0000", true),
            ],
            "{path}"
        );
        assert_eq!(callees["complete"], false);

        // Without symbols, the veneers are known by their code, and the
        // rows are the same.
        let stripped_path = path.replace("-nodebug", "-stripped");
        let stripped = run(&[&stripped_path, "callees", "0x0+64K", "--json"]).json(1);
        assert_eq!(
            rows_by_address(&stripped),
            rows_by_address(&callees),
            "{path}"
        );
    }
}
