//! Runs the elfq program the way a user would.

use std::process::Stdio;

use crate::support::elf::ElfBytes;
use crate::support::{
    DEEP_SAMPLE, LIBRARY_SAMPLE, Run, SAMPLE, TCM_SAMPLE, assert_output, command, crossing,
    samples, stderr, stdout,
};

#[test]
fn commands_that_need_roots_refuse_a_shared_library() {
    for (args, what) in [
        (&["top"][..], "what keeps the most bytes"),
        (
            &["retained", "process_frame"],
            "what only process_frame leads to",
        ),
        (&["why", "log_msg"], "why log_msg is"),
        (&["--range", "0x1000+4K"], "what keeps the bytes of a range"),
    ] {
        let args = [&[LIBRARY_SAMPLE][..], args].concat();
        assert_output(
            &stderr(&args),
            &format!("error: elfq can't yet tell {what} in a shared library\n"),
        );
    }
}

#[test]
fn tree_draws_the_rows_as_a_tree() {
    let tree = stdout(&[SAMPLE, "top", "--tree"]);
    assert!(
        tree.contains(
            "\
RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     378   271     │  ├─ apply_filter
     107    75     │  │  └─ filter_tap
      32    32     │  │     └─ fir_coeffs
      54    54     │  ├─ decode_header
      21           │  └─ 2 more
     705   370     ├─ report
     162   162     │  ├─ format_u32
      80    16     │  ├─ root
      48    16     │  │  ├─ inner
      32           │  │  │  └─ 2 more
      16    16     │  │  └─ leaf_a
      46    46     │  ├─ count_nodes
      47           │  └─ 6 more
     187    84     ├─ parse_value
     103   103     │  └─ parse_list
     103    51     ├─ log_msg
      36    20     │  ├─ platform_write
      16    16     │  │  └─ write@plt
      16    16     │  └─ strlen@plt
      57    51     ├─ app_tick
       6    ~6     │  └─ .rodata@0x200c
      34   ~34     ├─ .rodata@0x205e
      33    33     ├─ dispatch_event
      32    32     ├─ stats
      31    19     ├─ on_button (through a pointer)
      12           │  └─ 2 more
      25    25     ├─ frame_a
     127           └─ 12 more
     152   ~64  __do_global_dtors_aux
      48   ~48  ├─ deregister_tm_clones
      40        └─ 2 more
      80   ~16  frame_dummy
      64   ~64  └─ register_tm_clones
      64    64  .plt
      32    32  __abi_tag
      17        5 more
"
        ),
        "{tree}"
    );
    // The same rows in the same order, each named the same way.
    let rows = stdout(&[SAMPLE, "top"]);
    let names = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.split(", kept by ").next().unwrap();
                line.replace(['│', '├', '└', '─'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert_eq!(names(&tree), names(&rows));

    // The overview draws its largest items the same way, for the whole
    // file and for a range.
    let tree = stdout(&[SAMPLE, "--tree"]);
    assert!(
        tree.contains(
            "\
RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     453           │  └─ 6 more
    1334           └─ 41 more
     345        13 more
"
        ),
        "{tree}"
    );
    let tree = stdout(&[TCM_SAMPLE, "--range", "0x0..0xe0", "--tree"]);
    assert!(tree.contains("     200    0  └─ main\n"), "{tree}");

    // why's chains too.
    let tree = stdout(&[SAMPLE, "why", "log_msg", "--tree"]);
    assert!(
        tree.contains(
            "\
HOW            ITEM
entry point    _start
address taken  └─ main
call              ├─ app_tick
call              │  └─ log_msg
address taken     ├─ on_button
tail call         │  └─ log_msg
call              ├─ process_frame
call              │  └─ log_msg
call              └─ report
call                 └─ log_msg
"
        ),
        "{tree}"
    );
}

#[test]
fn only_trees_are_drawn() {
    for args in [
        &[SAMPLE, "top", "--tree", "--json"][..],
        &[SAMPLE, "--json", "--tree"],
    ] {
        assert_output(
            &stderr(args),
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        );
    }
    for args in [
        &[SAMPLE, "callers", "log_msg", "--tree"][..],
        &[SAMPLE, "callees", "--tree", "0x1000+4K"],
        &[SAMPLE, "inlined", "--tree"],
        &[SAMPLE, "debug", "items", "--tree"],
    ] {
        assert_output(
            &stderr(args),
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        );
    }
}

#[test]
fn no_line_sums_up_a_single_item() {
    // A row such as "12 more, kept by main" counts every item it sums up,
    // so one that would sum up a single item shows the item instead, and so
    // do lists of names.
    for sample in &samples() {
        let overview = stdout(&[sample]);
        let mut runs = vec![vec![sample.as_str()]];
        // `top` refuses shared libraries.
        if !sample.ends_with(".so") {
            runs.push(vec![sample, "top"]);
        }
        // The ranges in the overview's Try list.
        for (_, range) in overview
            .lines()
            .filter_map(|line| line.split_once(" --range "))
        {
            runs.push(vec![sample, "--range", range]);
            runs.push(vec![sample, "top", "--range", range]);
        }
        // What the item that the Try list asks about reaches.
        for line in overview.lines() {
            if let Some((_, symbol)) = line
                .split_once(" retained ")
                .or_else(|| line.split_once(" callees "))
            {
                runs.push(vec![sample, "reach", symbol]);
                runs.push(vec![sample, "reach", symbol, "--tree"]);
            }
        }
        for args in &runs {
            let text = stdout(args);
            assert!(
                !text.lines().any(|line| line.contains(" 1 more")),
                "{args:?}\n{text}"
            );
        }
    }
}

#[test]
fn long_lists_name_the_first_few() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let choices = stderr(&[sample, "callers", "fmt"]);
    assert!(
        choices.starts_with("error: `fmt` could mean 53 functions;"),
        "{choices}"
    );
    assert_eq!(choices.lines().count(), 12, "{choices}");
    assert!(choices.ends_with("\n  and 43 more\n"), "{choices}");
    let retained = stdout(&[sample, "retained", "main"]);
    assert!(retained.contains(" and 48 more.\n"), "{retained}");
    let inlined = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined"]);
    assert!(inlined.contains("     231  133 more\n"), "{inlined}");
}

#[test]
fn every_tree_is_at_most_nine_levels_deep() {
    // Each command, and the rows whose part of the tree is continued below.
    let trees = [
        (
            &[DEEP_SAMPLE, "top", "--tree"][..],
            &["stage7", "stage15"][..],
        ),
        (
            &[DEEP_SAMPLE, "retained", "stage1", "--tree"],
            &["stage9", "stage17"],
        ),
        (
            &[DEEP_SAMPLE, "why", "chain_table", "--tree"],
            &["chain7", "chain15"],
        ),
        (
            &[DEEP_SAMPLE, "why", "stage_table", "--tree"],
            &["stage7", "stage15"],
        ),
        (
            &[DEEP_SAMPLE, "inlined", "run_steps", "--tree"],
            &["step9", "step17"],
        ),
        (
            &[DEEP_SAMPLE, "reach", "chain1", "--tree"],
            &["chain9", "chain17"],
        ),
        (
            &[
                "testdata/bin/rust-x86_64-nodebug.elf",
                "why",
                "memset",
                "--tree",
            ],
            &[
                "std::panicking::default_hook",
                "rustc_demangle::try_demangle",
                "__rustc::__rust_alloc_zeroed",
            ],
        ),
    ];
    for (args, cuts) in trees {
        let text = stdout(args);
        // The tree is in the last column, which starts where its header
        // does. Each level is 3 columns.
        let header = text
            .lines()
            .find(|line| line.ends_with("ITEM") || line.ends_with("FUNCTION"));
        let column = header.unwrap().rfind(' ').unwrap() + 1;
        let deepest = text
            .lines()
            .filter(|line| line.contains("─ "))
            .map(|line| {
                line.chars()
                    .skip(column)
                    .take_while(|c| "│├└─ ".contains(*c))
                    .count()
            })
            .max();
        assert!(deepest.unwrap() <= 9 * 3, "{args:?}\n{text}");
        // Each part cut from the tree is continued below, in order.
        let cut: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued below)"))
            .map(|line| line.rsplit("─ ").next().unwrap())
            .collect();
        let continued: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued)"))
            .map(str::trim)
            .collect();
        assert_eq!(cut, cuts, "{args:?}\n{text}");
        assert_eq!(continued, cuts, "{args:?}\n{text}");
    }
}

/// Whether `text` sums up what it leaves out: a row such as "6 more, kept by
/// main" or "70 more", or a list in a sentence such as "a, b and 48 more.".
/// The AT column keeps its cut, as in "0x3a249 and 3 more  .rodata@0x6044".
fn sums_up(text: &str) -> bool {
    text.lines()
        .any(|line| line.contains(" more") && !line.contains(" more  "))
}

#[test]
fn all_shows_every_item() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/rust-cortex-m4.elf";
    for args in [
        vec![sample, "top"],
        vec![sample, "retained", "main"],
        vec![firmware, "inlined"],
        vec![sample, "callers", "memcpy"],
        vec![sample, "callees", "std::rt::lang_start_internal"],
    ] {
        let largest = stdout(&args);
        let all = stdout(&[&args[..], &["--all"]].concat());
        assert!(sums_up(&largest), "{args:?}\n{largest}");
        assert!(!sums_up(&all), "{args:?}\n{all}");
        assert!(all.lines().count() > largest.lines().count());
    }
    // A range check lists every crossing either way, and --all names every
    // function in the sentence under the table.
    let range = [sample, "callers", "0x20000+0x10000"];
    let largest = crossing(&range);
    let all = crossing(&[&range[..], &["--all"]].concat());
    assert!(largest.contains(" and 9 more aren't listed"), "{largest}");
    assert!(!sums_up(&all), "{all}");
    assert_eq!(all.lines().count(), largest.lines().count());
    // why draws the chains through the nearest 10 users, or every one.
    let chains = |why: &str| why.lines().filter(|line| line.ends_with(" memcpy")).count();
    let why = stdout(&[sample, "why", "memcpy"]);
    assert!(
        why.starts_with(
            "memcpy is used by 57 functions; these are the chains through the nearest 10:\n"
        ),
        "{why}"
    );
    assert_eq!(chains(&why), 10, "{why}");
    let why = stdout(&[sample, "why", "memcpy", "--all"]);
    assert!(
        why.starts_with("memcpy is used by 57 functions, through these chains:\n"),
        "{why}"
    );
    assert_eq!(chains(&why), 57, "{why}");
    // A tree of fewer than 30 items is the same either way.
    assert_output(
        &stdout(&[SAMPLE, "retained", "process_frame", "--all"]),
        &stdout(&[SAMPLE, "retained", "process_frame"]),
    );
}

#[test]
fn why_shows_the_chains_that_keep_an_item() {
    assert_output(
        &stdout(&[SAMPLE, "why", "log_msg"]),
        "\
log_msg is used by 4 functions, through these chains:

HOW            ITEM
entry point    _start
address taken  main
call           app_tick
call           log_msg
address taken  on_button, used by main
tail call      log_msg
call           process_frame, used by main
call           log_msg
call           report, used by main
call           log_msg

debug_dump uses log_msg too, but nothing reaches debug_dump.

Removing main would remove log_msg too.
",
    );
    // parse_list calls parse_value back, so it doesn't keep it.
    let why = stdout(&[SAMPLE, "why", "parse_value"]);
    assert!(
        why.contains(
            "\nparse_list uses parse_value too, but only parse_value leads to parse_list.\n"
        ),
        "{why}"
    );
    // With exactly 11 users, every chain is shown, since a summary never
    // stands for a single item.
    let why = stdout(&["testdata/bin/far-gnu-m4-nodebug.elf", "why", "counter"]);
    assert!(
        why.starts_with("counter is used by 11 functions, through these chains:\n"),
        "{why}"
    );
    assert_eq!(
        why.lines()
            .filter(|line| line.ends_with(" counter"))
            .count(),
        11
    );
    // With more, it shows the chains through the nearest ten.
    let why = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "why",
        ".rodata@0x5633",
    ]);
    assert!(
        why.starts_with(
            ".rodata@0x5633 is used by 36 data objects; these are the chains through the \
             nearest 10:\n"
        ),
        "{why}"
    );
}

#[test]
fn why_on_firmware_can_start_from_an_interrupt() {
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m4.elf", "why", "app_tick"]),
        "\
app_tick is used by 2 functions, through these chains:

HOW           ITEM
entry point   Reset_Handler
call          main
call          app_tick
vector table  SysTick_Handler
tail call     app_tick

Removing any single item wouldn't remove app_tick.
",
    );
}

#[test]
fn why_explains_where_the_program_starts() {
    // A root of each kind, and for startup code, when the program runs it.
    let rust = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/c-cortex-m4.elf";
    for (file, symbol, sentence) in [
        (
            SAMPLE,
            "_start",
            "_start is the entry point, where the program starts.",
        ),
        (
            SAMPLE,
            "_init",
            "_init is in .init, which the program uses when it starts.",
        ),
        (
            SAMPLE,
            "_fini",
            "_fini is in .fini, which the program uses when it exits.",
        ),
        (
            SAMPLE,
            "frame_dummy",
            "frame_dummy is listed in .init_array, so the program runs it when it starts.",
        ),
        (
            SAMPLE,
            "__do_global_dtors_aux",
            "__do_global_dtors_aux is listed in .fini_array, so the program runs it when it \
             exits.",
        ),
        (
            SAMPLE,
            "__abi_tag",
            "__abi_tag is a note in .note.ABI-tag, which the system reads when it loads the \
             program.",
        ),
        (
            SAMPLE,
            ".plt",
            ".plt is the PLT header, which the dynamic loader uses to find imports.",
        ),
        (
            rust,
            "rust_eh_personality",
            "rust_eh_personality is a personality routine, which the unwinder calls on a panic \
             or exception.",
        ),
        (
            firmware,
            "vector_table",
            "vector_table holds the vector table, where the processor finds its handlers.",
        ),
        (
            firmware,
            "SysTick_Handler",
            "SysTick_Handler is in the vector table: the processor runs it on an interrupt or \
             exception.",
        ),
    ] {
        assert_output(&stdout(&[file, "why", symbol]), &format!("{sentence}\n"));
    }
}

#[test]
fn why_says_when_nothing_reaches_an_item() {
    assert_output(
        &stdout(&[SAMPLE, "why", "debug_dump"]),
        "Nothing reaches debug_dump: the program never uses it, or elfq can't see how.\n",
    );
    // The string that only `debug_dump` prints.
    assert_output(
        &stdout(&[SAMPLE, "why", ".rodata@0x2056"]),
        "Nothing reaches .rodata@0x2056: the program never uses it, or elfq can't see how.\n\
         debug_dump uses it, but nothing reaches debug_dump either.\n",
    );
}

#[test]
fn inlined_ranks_the_functions_the_compiler_copied() {
    assert_output(
        &stdout(&[SAMPLE, "inlined"]),
        "\
Inlined copies of 5 functions take 420 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
  164       1  update_stats
  150       6  append
   74       1  unpack_samples
   32       1  saturate
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "process_frame"]),
        "\
Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:

BYTES  COPIES  FUNCTION
  164       1  update_stats
   74       1  unpack_samples
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    // The six calls to `append` in `report` make one row.
    assert_output(
        &stdout(&[SAMPLE, "inlined", "report"]),
        "\
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "main"]),
        "The debug info lists no inlined code in main.\n",
    );
}

#[test]
fn inlined_shows_copies_inside_copies() {
    let rows = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined", "memcpy"]);
    assert!(
        rows.starts_with(
            "\
Of compiler_builtins::mem::memcpy's 730 bytes, 712 are inlined copies of 17 functions:

BYTES  COPIES  FUNCTION
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words, inlined into copy_forward
   74       1  compiler_builtins::mem::impls::load_aligned_partial, inlined into copy_forward_misaligned_words
"
        ),
        "{rows}"
    );
    // A function inlined in several places names where its copies are.
    let rows = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "process_frame",
    ]);
    for row in [
        "   14       1  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         update_stats\n",
        "   42       3  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         process_frame\n",
    ] {
        assert!(rows.contains(row), "{rows}");
    }
    let tree = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "memcpy",
        "--tree",
    ]);
    assert!(
        tree.contains(
            "\
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  ├─ compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words
   74       1  │  ├─ compiler_builtins::mem::impls::load_aligned_partial
"
        ),
        "{tree}"
    );
}

#[test]
fn inlined_needs_dwarf() {
    assert_output(
        &stderr(&["testdata/bin/c-x86_64-nodebug.elf", "inlined"]),
        "error: testdata/bin/c-x86_64-nodebug.elf has no DWARF debug info; build it with debug \
         info and don't strip it\n",
    );
}

#[test]
fn the_first_line_says_what_symbol_gave_when_it_isnt_the_name() {
    let first_line = |args: &[&str]| stdout(args).lines().next().unwrap().to_string();
    let m4 = "testdata/bin/c-cortex-m4.elf";
    assert_eq!(
        first_line(&[m4, "retained", "NMI_Handler"]),
        "Removing Default_Handler (also named NMI_Handler) would save only its own 2 bytes."
    );
    assert_eq!(
        first_line(&[m4, "why", "HardFault_Handler"]),
        "Default_Handler (also named HardFault_Handler) is in the vector table: the processor \
         runs it on an interrupt or exception."
    );
    assert_eq!(
        first_line(&[m4, "callees", "SVC_Handler"]),
        "Default_Handler (also named SVC_Handler) doesn't call or use anything."
    );
    assert_eq!(
        first_line(&[TCM_SAMPLE, "callers", "0x30000290"]),
        "log_msg (which holds 0x30000290) is used by 4 functions:"
    );
    assert_eq!(
        first_line(&[SAMPLE, "why", "0x1aa5"]),
        "Nothing reaches debug_dump (which holds 0x1aa5): the program never uses it, or elfq \
         can't see how."
    );
    // A name with a note can't take "'s".
    assert_eq!(
        first_line(&[SAMPLE, "inlined", "0x1710"]),
        "Of the 532 bytes of process_frame (which holds 0x1710), 238 are inlined copies of 3 \
         functions:"
    );
    assert_eq!(
        first_line(&[SAMPLE, "inlined", "0x1700"]),
        "Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:"
    );
}

#[test]
fn rust_names_are_readable_and_short_ones_work() {
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "process_frame",
        ]),
        "\
elfq_sample::process_frame is used by 1 function:

HOW   AT                  CALLER
call  0x14f12 and 2 more  elfq_sample::app_main
",
    );
    // A row names the row it hangs under by the shortest form of its name
    // that finds it.
    let top = stdout(&["testdata/bin/rust-cortex-m4.elf", "top"]);
    assert!(
        top.contains("\n    5840   736  elfq_sample::process_frame, kept by app_main\n"),
        "{top}"
    );
    let why = stdout(&["testdata/bin/rust-cortex-m4.elf", "why", "log_msg"]);
    assert!(
        why.contains("\ncall          elfq_sample::process_frame, used by app_main\n"),
        "{why}"
    );
    // Two functions have this name, and only an address finds each one, so
    // rows use the whole name.
    let all = stdout(&["testdata/bin/rust-x86_64-nodebug.elf", "top", "--all"]);
    assert!(
        all.contains(", kept by core::ptr::drop_glue::<alloc::string::String>\n"),
        "{all}"
    );
    // `DebugSet::entry` is an alias of this function, and shorter, but no
    // row shows it.
    let retained = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "retained",
        "DebugList::entry",
    ]);
    assert!(
        retained.contains("  .rodata@0x963e, kept by DebugList::entry\n"),
        "{retained}"
    );
}

#[test]
fn cpp_names_are_readable_and_short_ones_work() {
    const CPP: &str = "testdata/bin/cpp-x86_64.elf";
    // A virtual function is reached through its class's vtable.
    assert_output(
        &stdout(&[CPP, "why", "Gain::run"]),
        "\
app::Gain::run(short*, unsigned long) is used by vtable for app::Gain, through this chain:

HOW            ITEM
entry point    _start
address taken  main
uses           vtable for app::Gain
pointer        app::Gain::run(short*, unsigned long)

Removing vtable for app::Gain would remove app::Gain::run(short*, unsigned long) too.
",
    );
    // Overloads share a name, so their parameters tell them apart.
    assert_output(
        &stderr(&[CPP, "callers", "log"]),
        "\
error: `log` could mean 2 functions; use one on the left instead:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)
",
    );
    // An operator, and imports from the C++ runtime.
    assert_output(
        &stdout(&[CPP, "callers", "operator<"]),
        "\
app::operator<(app::Reading const&, app::Reading const&) is used by 1 function:

HOW   AT      CALLER
call  0x156f  app::sort_readings(app::Reading*, unsigned long)
",
    );
    assert_output(
        &stdout(&[CPP, "callees", "typeinfo for Gain"]),
        "\
typeinfo for app::Gain uses 3 items:

HOW      AT      CALLEE
pointer  0x3d78  vtable for __cxxabiv1::__si_class_type_info
pointer  0x3d80  typeinfo name for app::Gain
pointer  0x3d88  typeinfo for app::Stage
",
    );
    // Rows name the row they hang under by a short name, prefix and all.
    let top = stdout(&[CPP, "top"]);
    assert!(
        top.contains(" (through a pointer), kept by vtable for DcBlock\n"),
        "{top}"
    );
}

#[test]
fn data_without_a_symbol_is_named_by_section_and_address() {
    // The vtable through which `app_main` reaches `Gain::run`.
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "Gain::run"
        ]),
        "\
<elfq_sample::Gain as elfq_sample::Stage>::run is used by 1 data object:

HOW      AT       CALLER
pointer  0x52dc0  .data.rel.ro@0x52da8

Calls to <elfq_sample::Gain as elfq_sample::Stage>::run through a pointer aren't listed, since elfq can't see them.
"
    );
    let overview = stdout(&["testdata/bin/c-x86_64-stripped.elf"]);
    assert!(
        overview.contains(
            "Items without symbols are named by address, like fn_0x10a0 and .rodata@0x2004.\n"
        ),
        "{overview}"
    );
}

#[test]
fn a_reader_that_stops_early_keeps_the_exit_status() {
    // As in `elfq ... | head`, in a shell with `set -o pipefail`. Each
    // answer is larger than a pipe holds, so elfq writes after the reader
    // has gone.
    for (args, code) in [
        (&["callees", "0x1000+0x40000"][..], 1),
        (&["callees", "0x1000+0x40000", "--json"], 1),
        (&["reach", "main", "--to", "0x1000+0x40000"], 1),
        (&["reach", "main", "--to", "0x1000+0x40000", "--json"], 1),
        (&["top", "--all"], 0),
    ] {
        let sample = "testdata/bin/rust-x86_64-nodebug.elf";
        let mut child = command()
            .arg(sample)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        let command = format!("elfq {sample} {}", args.join(" "));
        Run::new(&command, output).printed(code);
    }
}

#[test]
fn an_error_keeps_its_exit_status_when_nothing_reads_it() {
    // As in `elfq ... 2>&1 | head -0`: the error is written after the
    // reader has gone.
    for args in [
        &[SAMPLE, "callers", "nothing_has_this_name"][..],
        &["callers", SAMPLE, "log_msg"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(writer)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn arguments_that_dont_go_together_are_refused_before_the_file_is_read() {
    // So that a mistake costs no time on a large file. This file doesn't
    // exist.
    for (args, error) in [
        (
            &["missing.elf", "why", "main", "--range", "0+1K"][..],
            "error: only the overview, top and retained take --range\n",
        ),
        (
            &["missing.elf", "callers"],
            "error: callers needs a SYMBOL, or ranges such as 0x0+256K\n",
        ),
        (
            &["missing.elf", "callers", "log_msg", "--to", "0x0+1K"],
            "error: --to goes with callees; callers takes --from to list only the callers in a \
             range\n",
        ),
        (
            &["missing.elf", "callees", "0x0+1K", "--from", "0x0+1K"],
            "error: --from goes with callers; callees takes --to to list only the callees in a \
             range\n",
        ),
        (
            &["missing.elf", "debug", "items", "--json"],
            "error: the debug commands print only text\n",
        ),
        (
            &["missing.elf", "browse", "--json"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "browse", "main", "--tree"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "browse", "--range", "0+1K"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "top", "--tree", "--json"],
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        ),
        (
            &["missing.elf", "inlined", "--tree"],
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        ),
        (
            &["missing.elf", "diff", "missing-too.elf", "--tree"],
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        ),
        (
            &["missing.elf", "diff", "missing-too.elf", "--range", "0+1K"],
            "error: only the overview, top and retained take --range\n",
        ),
        (
            &["missing.elf", "reach", "main", "--tree", "--json"],
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        ),
        (
            &["missing.elf", "reach", "main", "--range", "0+1K"],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x00000000..0x00000400\n",
        ),
        (
            &[
                "missing.elf",
                "reach",
                "main",
                "--to",
                "0x20000000+1K",
                "--range",
                "0+1K",
                "--json",
            ],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x20000000..0x20000400 --to 0x00000000..0x00000400 \
             --json\n",
        ),
        (
            &[
                "missing.elf",
                "reach",
                "main",
                "--range",
                "0+1K",
                "--all",
                "--tree",
            ],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x00000000..0x00000400 --all --tree\n",
        ),
        (
            &["missing.elf", "reach", "main", "--from", "0x0+1K"],
            "error: --from goes with callers; reach takes --to to list only what it reaches in a \
             range\n",
        ),
        // What a range uses is what callees lists for it.
        (
            &["missing.elf", "reach", "0x0+1K", "--json"],
            "error: reach takes one SYMBOL, not ranges; for what a range uses outside it: elfq \
             missing.elf callees 0x00000000..0x00000400 --json\n",
        ),
        (
            &["missing.elf", "reach", "0x0+1K", "--to", "0x20000000+1K"],
            "error: reach takes one SYMBOL, not ranges; for what a range uses in the --to ranges: \
             elfq missing.elf callees 0x00000000..0x00000400 --to 0x20000000..0x20000400\n",
        ),
        (
            &["missing.elf", "reach", "0x0..zz"],
            "error: reach takes one SYMBOL, not ranges\n",
        ),
    ] {
        assert_output(&stderr(args), error);
    }
}

#[test]
fn debug_items_lists_each_item() {
    let output = stdout(&[SAMPLE, "debug", "items"]);
    let line = output
        .lines()
        .find(|line| line.ends_with(" process_frame"))
        .unwrap();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields,
        ["0x1700", "532", "function", "symtab", "process_frame"]
    );
}

#[test]
fn debug_walk_lists_what_the_code_run_from_reset_does() {
    let output = stdout(&[TCM_SAMPLE, "debug", "walk"]);
    let mut lines = output.lines();
    assert_eq!(
        lines.next().unwrap(),
        "from Reset_Handler (0x300005f8), stack pointer 0x20040000, main 0x30000570"
    );
    // The calls of the reset handler's own code: the copies of .itcm and
    // .data, the zeroing of .bss, and main, with what memset left in the
    // registers: its first argument in r0.
    let calls: Vec<&str> = lines
        .clone()
        .filter(|line| line.contains(" 0  call "))
        .collect();
    assert_eq!(
        calls,
        [
            "0x3000060e      0  call memcpy (0x3000087c) (r0=0x0 r1=0x30000040 r2=0xe0 r3=0xdf)",
            "0x30000626      0  call memcpy (0x3000087c) (r0=0x20000000 r1=0x300009c0 r2=0x420 r3=0x41f)",
            "0x3000063e      0  call memset (0x300007d8) (r0=0x20000420 r1=0x0 r2=0x30 r3=0x2f)",
            "0x30000642      0  call main (0x30000570) (r0=0x20000420 r1=0x0 r2=? r3=?), left by a call: r0",
        ]
    );
    assert_eq!(
        lines.last().unwrap(),
        "0x30000642         ends: reaches main"
    );
    // Only Thumb code.
    assert_output(
        &stderr(&[SAMPLE, "debug", "walk"]),
        "error: elfq walks only Thumb code, which Cortex-M processors run\n",
    );
}

#[test]
fn a_file_whose_code_elfq_cant_read_says_so() {
    // The x86-64 sample, marked as AArch64 code, which elfq can't read yet.
    let path = ElfBytes::sample(SAMPLE)
        .mark_code_unreadable()
        .save("aarch64.elf");
    let path = path.as_str();

    assert_output(
        &stdout(&[path]),
        &format!(
            "\
{path}: Aarch64 position-independent executable, with symbols and DWARF debug info
30 functions and 23 data objects, 3873 bytes of code and data
elfq can't yet read Aarch64 code to tell what calls what.
"
        ),
    );
    // Even when SYMBOL names nothing, since no SYMBOL would help.
    for args in [
        &[path, "top"][..],
        &[path, "retained", "no_such_function"],
        &[path, "why", "no_such_function"],
        &[path, "callers", "no_such_function"],
        &[path, "callees", "no_such_function"],
        &[path, "reach", "no_such_function"],
        &[path, "reach"],
        &[
            path,
            "callers",
            "no_such_function",
            "--range",
            "0x1000+0x100",
        ],
        &[path, "callers", "--range", "0x1000+0x100"],
    ] {
        assert_output(
            &stderr(args),
            "error: elfq can't yet read Aarch64 code to tell what calls what\n",
        );
    }
}

#[test]
fn errors_go_to_stderr_with_exit_code_2() {
    assert_output(
        &stderr(&["Cargo.toml"]),
        "error: cannot load Cargo.toml: not an ELF file\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "callers", "proces_frame"]),
        "error: no function or data object is named `proces_frame`; did you mean process_frame?\n",
    );
}

#[test]
fn errors_say_what_to_try() {
    // A range typed with `-` or `:` between its ends.
    let dash = "error: `0x0-0x40000` isn't a range: did you mean 0x0..0x40000?\n";
    assert_output(&stderr(&[TCM_SAMPLE, "callees", "0x0-0x40000"]), dash);
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100", "0x0-0x40000"]),
        dash,
    );
    let error = stderr(&[TCM_SAMPLE, "top", "--range", "0x0:0x40000"]);
    assert!(
        error.starts_with(
            "error: invalid value '0x0:0x40000' for '--range <RANGE>': did you mean \
             0x0..0x40000?\n"
        ),
        "{error}"
    );
    // A second number smaller than the first is a size.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x20000000:256K"]),
        "error: `0x20000000:256K` isn't a range: did you mean 0x20000000+256K?\n",
    );
    // Ends that make no range get the forms a range takes.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x40000-0x0"]),
        "error: `0x40000-0x0` isn't a range: expected START..END or START+SIZE, such as \
         0x0+256K\n",
    );
    // Several ranges joined by commas.
    let error = stderr(&[TCM_SAMPLE, "top", "--range", "0x0..0x100,0x200..0x300"]);
    assert!(
        error.starts_with(
            "error: invalid value '0x0..0x100,0x200..0x300' for '--range <RANGE>': --range takes \
             one range, such as 0x0..0x100\n"
        ),
        "{error}"
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,0x200..0x300"]),
        "error: `0x0..0x100,0x200..0x300` isn't a range; give each range as its own argument: \
         0x0..0x100 0x200..0x300\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,", "0x200..0x300"]),
        "error: `0x0..0x100,` isn't a range; leave out the comma\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,0x200"]),
        "error: `0x200` isn't a range: expected START..END or START+SIZE, such as 0x0+256K\n",
    );

    // A file without a symbol table names its items by address.
    assert_output(
        &stderr(&["testdata/bin/c-cortex-m4-stripped.elf", "callers", "main"]),
        "error: no function or data object is named `main`; the file has no symbol table, so \
         elfq names its functions and data by address, like fn_0x8000040 and .rodata@0x80008f0\n",
    );

    // A function that the compiler inlined everywhere has no item.
    assert_output(
        &stderr(&[SAMPLE, "callers", "append"]),
        "error: no function or data object is named `append`; the compiler inlined all 6 copies \
         of it into report, which `inlined report` lists\n",
    );
    assert_output(
        &stderr(&[
            "testdata/bin/rust-cortex-m4.elf",
            "inlined",
            "copy_forward_bytes",
        ]),
        "error: no function or data object is named `copy_forward_bytes`; the compiler inlined \
         both copies of it into memcpy, which `inlined memcpy` lists\n",
    );
    assert_output(
        &stderr(&[
            "testdata/bin/rust-cortex-m4.elf",
            "why",
            "atomic_add::<u32, u32>",
        ]),
        "error: no function or data object is named `atomic_add::<u32, u32>`; the compiler \
         inlined all 9 copies of it into process_frame, app_main, on_button, on_data, on_timer \
         and sys_tick; `inlined process_frame` lists those in process_frame\n",
    );
    // Suggestions still follow, as the name may not be the one meant.
    assert_output(
        &stderr(&["testdata/bin/c-deep.elf", "why", "printf"]),
        "error: no function or data object is named `printf`; the compiler inlined its one copy \
         into main, which `inlined main` lists; did you mean __printf_chk?\n",
    );
}

#[test]
fn a_command_typed_before_the_file_gets_a_hint() {
    assert_output(
        &stderr(&["callers", SAMPLE, "log_msg"]),
        "error: the file comes before the command, as in: \
         elfq testdata/bin/c-x86_64.elf callers log_msg\n",
    );
    // diff's two files go on either side of it.
    assert_output(
        &stderr(&["diff", "a.elf", "b.elf"]),
        "error: the file comes before the command, as in: elfq a.elf diff b.elf\n",
    );
}

#[test]
fn help_comes_before_a_command_too() {
    let help = stdout(&["help", "top"]);
    assert!(
        help.starts_with("Show the items that keep the most bytes in the file\n"),
        "{help}"
    );
    assert_output(&help, &stdout(&[SAMPLE, "top", "--help"]));
    assert_output(&stdout(&["help"]), &stdout(&["--help"]));
    assert_output(&stdout(&["help", "top", "--json"]), &help);
    // A misspelled command gets the same error as after a file.
    assert!(
        stderr(&["help", "retaind"]).contains("a similar subcommand exists: 'retained'"),
        "{}",
        stderr(&["help", "retaind"])
    );
}

#[test]
fn help_before_a_command_is_that_commands_help() {
    // Hidden commands too: `elfq help debug` is `elfq FILE debug --help`.
    for command in elfq::cli::command_names() {
        assert_output(
            &stdout(&["help", &command]),
            &stdout(&[SAMPLE, &command, "--help"]),
        );
    }
}

#[test]
#[cfg(unix)]
fn an_argument_that_is_not_utf8_is_an_error() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    for args in [[SAMPLE, "retained"], ["retained", SAMPLE]] {
        let output = command()
            .args(args)
            .arg(OsStr::from_bytes(b"\xff"))
            .output()
            .unwrap();
        let command = format!("elfq {} \\xff", args.join(" "));
        let error = Run::new(&command, output).error();
        assert!(error.starts_with("error: "), "{error}");
    }
}

#[test]
fn no_arguments_prints_help() {
    let help = stderr(&[]);
    assert!(
        help.contains("Usage: elfq [OPTIONS] <FILE> [COMMAND]"),
        "{help}"
    );
}
