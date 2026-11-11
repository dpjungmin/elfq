//! What the commands share in their errors and help.
//!
//! The tests check the arguments and files that the commands refuse,
//! errors on stderr with exit status 2 and what to try instead, the line
//! that says that the arguments are files here, which the shell may have
//! put in place of a pattern, the exit status when what reads the answer
//! stops early, and help before or after a command. Each command's own
//! errors are tested in its file in commands/.

use std::process::Stdio;

use object::elf;

use crate::support::elf::{ElfBytes, SectionField};
use crate::support::{
    LIBRARY_SAMPLE, Run, SAMPLE, TCM_SAMPLE, TempFolder, assert_output, command, stderr, stdout,
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
        (&["why", "__rust_dealloc", "--json"], 0),
        (&["reach", "main", "--json"], 0),
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
            "error: callers needs a SYMBOL, or patterns or ranges, such as 'uart_*' or 0x0+256K\n",
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
    for command in ["callees", "retained"] {
        assert_output(&stderr(&[TCM_SAMPLE, command, "0x0-0x40000"]), dash);
    }
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
    for command in ["callers", "retained"] {
        assert_output(
            &stderr(&[TCM_SAMPLE, command, "0x0..0x100,0x200..0x300"]),
            "error: `0x0..0x100,0x200..0x300` isn't a range; give each range as its own \
             argument: 0x0..0x100 0x200..0x300\n",
        );
    }
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
fn a_name_that_could_mean_several_gets_the_pattern_for_all_where_a_set_is_taken() {
    let rust = "testdata/bin/rust-cortex-m4.elf";
    let checksums = "
  'checksum::<u8>'   elfq_sample::checksum::<u8>
  'checksum::<u32>'  elfq_sample::checksum::<u32>
  'checksum::<u16>'  elfq_sample::checksum::<u16>
";
    // retained, callers and callees take every item that a pattern
    // matches, so the first line ends with the one that takes the three.
    for (command, instruction) in [
        ("retained", "use one or more on the left instead"),
        ("callers", "use one on the left instead"),
        ("callees", "use one on the left instead"),
    ] {
        assert_output(
            &stderr(&[rust, command, "checksum"]),
            &format!(
                "error: `checksum` could mean 3 functions; {instruction}, or 'checksum*' for all \
                 3:{checksums}"
            ),
        );
    }
    // why, reach and inlined take one item, so they never offer it.
    for command in ["why", "reach", "inlined"] {
        assert_output(
            &stderr(&[rust, command, "checksum"]),
            &format!(
                "error: `checksum` could mean 3 functions; use one on the left instead:{checksums}"
            ),
        );
    }
    // A name with a pointer's `*` in it gets the pattern too.
    assert_output(
        &stderr(&[
            "testdata/bin/cpp-x86_64.elf",
            "callees",
            "run(short*, unsigned long)",
        ]),
        "\
error: `run(short*, unsigned long)` could mean 2 functions; use one on the left instead, or 'run(short*, unsigned long)*' for all 2:
  Gain::run     app::Gain::run(short*, unsigned long)
  DcBlock::run  app::DcBlock::run(short*, unsigned long)
",
    );
    // When it takes more, the error is as it was: 'run*' takes std's
    // run_with_cstr_allocating too.
    assert_output(
        &stderr(&["testdata/bin/rust-x86_64-nodebug.elf", "retained", "run"]),
        "\
error: `run` could mean 3 functions; use one or more on the left instead:
  Gain::run     <elfq_sample::Gain as elfq_sample::Stage>::run
  DcBlock::run  <elfq_sample::DcBlock as elfq_sample::Stage>::run
  enable::run   std::sys::thread_local::guard::key::enable::run
",
    );
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
        &stderr(&[&path, "callers", "append"]),
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
    let error = stderr(&[&path, "callers", "append"]);
    assert!(
        error.starts_with(
            "error: no function or data object is named `append`; the file has no symbol table"
        ),
        "{error}"
    );
}

#[test]
fn a_module_typed_without_a_star_gets_its_pattern_where_the_command_takes_it() {
    let sample = "testdata/bin/cpp-x86_64.elf";
    let named = |name: &str, may_mean: &str| {
        format!("error: no function or data object is named `{name}`; did you mean {may_mean}?\n")
    };
    let like_gain = "'vtable for app::Gain', 'typeinfo for app::Gain', 'typeinfo name for \
                     app::Gain', app::Gain::run";
    let config = "'app::Config::*' (1 function), app::config, app::Config::load_defaults";
    // retained, callers and callees take every item that a pattern
    // matches, so they offer it whenever it matches any, first.
    for command in ["retained", "callers", "callees"] {
        assert_output(
            &stderr(&[sample, command, "app::Gain"]),
            &named(
                "app::Gain",
                &format!("'app::Gain::*' (4 items), {like_gain}"),
            ),
        );
        assert_output(
            &stderr(&[sample, command, "app::Config"]),
            &named("app::Config", config),
        );
    }
    // why, reach and inlined take one item, so they offer it only when it
    // matches one, the item it would find.
    for command in ["why", "reach", "inlined"] {
        assert_output(
            &stderr(&[sample, command, "app::Gain"]),
            &named("app::Gain", like_gain),
        );
        assert_output(
            &stderr(&[sample, command, "app::Config"]),
            &named("app::Config", config),
        );
    }
    // A `::` at the end is left out, and a name of only colons gets no
    // pattern, since `::*` would match every item.
    assert_output(
        &stderr(&[sample, "retained", "vtable for app::"]),
        &named(
            "vtable for app::",
            "'vtable for app::*' (2 data objects), 'vtable for app::Gain', 'vtable for \
             app::DcBlock'",
        ),
    );
    assert_output(
        &stderr(&[sample, "callers", "::"]),
        &named(
            "::",
            "app::config, app::handlers, app::frames_ok, app::frames_bad, app::timer_ticks",
        ),
    );
    // Any other name gets one, as a Rust impl block does.
    assert_output(
        &stderr(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "reach",
            "<u32 as core::fmt::Display>",
        ]),
        &named(
            "<u32 as core::fmt::Display>",
            "'<u32 as core::fmt::Display>::*' (1 function), '<u32 as core::fmt::Display>::fmt'",
        ),
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

/// The line under an error for `uart_driver.c` and `uart_hal.c`, which the
/// shell puts in place of `uart_*` where they are files.
const UART_FILES_HERE: &str = "uart_driver.c and uart_hal.c are files here: the shell may have \
                               put them in place of a pattern. Put a pattern in single quotes, as \
                               in 'uart_*', so that the shell passes it as typed.";

/// The line under an error for `name`, alone a file here.
fn a_file_here(name: &str) -> String {
    format!(
        "{name} is a file here: the shell may have put it in place of a pattern. Put a pattern \
         in single quotes, so that the shell passes it as typed."
    )
}

/// The error for `name`, which names no item in the samples.
fn not_named(name: &str) -> String {
    format!("no function or data object is named `{name}`")
}

#[test]
fn files_that_the_shell_put_in_place_of_a_pattern_are_pointed_out() {
    let uart = ["uart_driver.c", "uart_hal.c"];
    let here = TempFolder::new("shell-put-files", &uart, &[]);
    let error = |args: &[&str]| here.run(TCM_SAMPLE, &[args, &uart].concat()).error();
    // why, reach, inlined and browse take one SYMBOL at most, so clap
    // refuses the second name before the file is read, and the line goes
    // under the first line of clap's error.
    for (command, symbol) in [
        ("why", "<SYMBOL>"),
        ("reach", "[SYMBOL]"),
        ("inlined", "[SYMBOL]"),
        ("browse", "[SYMBOL]"),
    ] {
        assert_output(
            &error(&[command]),
            &format!(
                "error: unexpected argument 'uart_hal.c' found\n{UART_FILES_HERE}\n\nUsage: elfq \
                 <FILE> {command} [OPTIONS] {symbol}\n\nFor more information, try '--help'.\n"
            ),
        );
    }
    // retained lists every argument that it can't use, then the line.
    assert_output(
        &error(&["retained"]),
        &format!(
            "error: elfq can't use 2 of the 2 SYMBOLs:\n  {}\n  {}\n{UART_FILES_HERE}\n",
            not_named("uart_driver.c"),
            not_named("uart_hal.c")
        ),
    );
    // callers and callees stop at the first.
    for command in ["callers", "callees"] {
        assert_output(
            &error(&[command]),
            &format!("error: {}\n{UART_FILES_HERE}\n", not_named("uart_driver.c")),
        );
    }
    // The tests run in the crate's root, where these aren't files.
    for command in ["why", "retained", "callers"] {
        let elsewhere = stderr(&[&[TCM_SAMPLE, command], &uart[..]].concat());
        assert!(!elsewhere.contains("files here"), "{elsewhere}");
    }
}

#[test]
fn the_line_shows_a_pattern_where_the_files_names_start_alike() {
    let here = TempFolder::new(
        "names-alike",
        &[
            "uart_hal.c",
            "main.c",
            "on_button.c",
            "über_a.c",
            "über_b.c",
        ],
        &[],
    );
    let error = |args: &[&str]| here.run(TCM_SAMPLE, args).error();
    // One file's name says nothing of where the `*` was.
    assert_output(
        &error(&["why", "uart_hal.c"]),
        &format!(
            "error: {}\n{}\n",
            not_named("uart_hal.c"),
            a_file_here("uart_hal.c")
        ),
    );
    // The line follows the names like it, which come first.
    assert_output(
        &error(&["retained", "on_button.c"]),
        &format!(
            "error: {}; did you mean on_button?\n{}\n",
            not_named("on_button.c"),
            a_file_here("on_button.c")
        ),
    );
    // Names that start with nothing in common get no example either.
    let unalike = error(&["retained", "main.c", "on_button.c"]);
    assert!(
        unalike.ends_with(
            "\nmain.c and on_button.c are files here: the shell may have put them in place of a \
             pattern. Put a pattern in single quotes, so that the shell passes it as typed.\n"
        ),
        "{unalike}"
    );
    // A name outside ASCII is quoted, as for the shell, and the pattern
    // ends after a whole character.
    let quoted = error(&["retained", "über_a.c", "über_b.c"]);
    assert!(
        quoted.ends_with(
            "\n'über_a.c' and 'über_b.c' are files here: the shell may have put them in place of \
             a pattern. Put a pattern in single quotes, as in 'über_*', so that the shell passes \
             it as typed.\n"
        ),
        "{quoted}"
    );
}

#[test]
fn the_line_names_ten_files_then_how_many_more_even_with_all() {
    // In the order that the shell puts them in.
    let mut irqs: Vec<String> = (1..=12).map(|number| format!("irq{number}.c")).collect();
    irqs.sort();
    let irqs: Vec<&str> = irqs.iter().map(String::as_str).collect();
    let here = TempFolder::new("ten-files", &irqs, &[]);
    let line = "\nirq1.c, irq10.c, irq11.c, irq12.c, irq2.c, irq3.c, irq4.c, irq5.c, irq6.c, \
                irq7.c and 2 more are files here: the shell may have put them in place of a \
                pattern. Put a pattern in single quotes, as in 'irq*', so that the shell passes \
                it as typed.\n";
    let listed = here
        .run(TCM_SAMPLE, &[&["retained"], &irqs[..]].concat())
        .error();
    assert!(
        listed.ends_with(&format!("\n  and 2 more{line}")),
        "{listed}"
    );
    // --all lists every argument that retained can't use, but what matters
    // in the line is that they are files.
    let every = (here.run(TCM_SAMPLE, &[&["retained", "--all"], &irqs[..]].concat())).error();
    assert!(
        every.ends_with(&format!("\n  {}{line}", not_named("irq9.c"))),
        "{every}"
    );
}

#[test]
fn only_files_in_this_folder_count() {
    let here = TempFolder::new(
        "only-files-here",
        &["uart_hal.c", "drivers/uart_hal.c"],
        &["build", "drivers"],
    );
    let error = |args: &[&str]| here.run(TCM_SAMPLE, args).error();
    // A folder's name is more often typed as is, and a path isn't what a
    // `*` alone puts in place of a pattern.
    assert_output(
        &error(&["retained", "build"]),
        &format!("error: {}\n", not_named("build")),
    );
    assert_output(
        &error(&["retained", "drivers/uart_hal.c", "./uart_hal.c"]),
        &format!(
            "error: elfq can't use 2 of the 2 SYMBOLs:\n  {}\n  {}\n",
            not_named("drivers/uart_hal.c"),
            not_named("./uart_hal.c")
        ),
    );
    // Where clap refuses a folder, as the shell may put one in place of a
    // pattern, the line names the files, and only when there are some.
    let usage = "\nUsage: elfq <FILE> why [OPTIONS] <SYMBOL>\n\nFor more information, try \
                 '--help'.\n";
    assert_output(
        &error(&["why", "uart_hal.c", "build"]),
        &format!(
            "error: unexpected argument 'build' found\n{}\n{usage}",
            a_file_here("uart_hal.c")
        ),
    );
    assert_output(
        &error(&["why", "x", "build"]),
        &format!("error: unexpected argument 'build' found\n{usage}"),
    );
    assert_output(
        &error(&["why", "main", "drivers/uart_hal.c"]),
        &format!("error: unexpected argument 'drivers/uart_hal.c' found\n{usage}"),
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_file_counts_as_a_file() {
    use std::os::unix::fs::symlink;
    let here = TempFolder::new("symlinks", &["uart_hal.c"], &["build"]);
    symlink("uart_hal.c", here.path().join("link.c")).unwrap();
    symlink("build", here.path().join("linked_build")).unwrap();
    symlink("nowhere.c", here.path().join("gone.c")).unwrap();
    assert_output(
        &(here.run(
            TCM_SAMPLE,
            &["retained", "link.c", "linked_build", "gone.c"],
        ))
        .error(),
        &format!(
            "error: elfq can't use 3 of the 3 SYMBOLs:\n  {}\n  {}\n  {}\n{}\n",
            not_named("link.c"),
            not_named("linked_build"),
            not_named("gone.c"),
            a_file_here("link.c")
        ),
    );
}

#[test]
fn the_line_follows_any_error_for_the_first_argument_that_callers_cant_use() {
    let here = TempFolder::new(
        "first-error",
        &["log", "log.c", "__libc_start_main", "main.c"],
        &[],
    );
    // The shell puts every file in place of one pattern, so when the first
    // argument that callers can't use is a file here, the line follows its
    // error, whatever it is: here, the error for a name that could mean
    // several items, whose first line this test leaves to the tests of that
    // error.
    let several = (here.run("testdata/bin/cpp-x86_64.elf", &["callers", "log", "log.c"])).error();
    assert!(
        several.starts_with("error: `log` could mean 2 functions; use one on the left instead"),
        "{several}"
    );
    assert!(
        several.ends_with(
            "\n  'log(char const*)'   app::log(char const*)\n  'log(unsigned int)'  \
             app::log(unsigned int)\nlog and log.c are files here: the shell may have put them \
             in place of a pattern. Put a pattern in single quotes, as in 'log*', so that the \
             shell passes it as typed.\n"
        ),
        "{several}"
    );
    // Or the error for an import, whose code isn't in the file.
    assert_output(
        &(here.run(SAMPLE, &["callees", "__libc_start_main", "main.c"])).error(),
        "error: __libc_start_main is imported from a shared library, so its code isn't in this \
         file\n__libc_start_main and main.c are files here: the shell may have put them in place \
         of a pattern. Put a pattern in single quotes, so that the shell passes it as typed.\n",
    );
    // why takes one item, so the line follows only an error for a name that
    // finds nothing: `log` is a file here, but names two items.
    let several = here
        .run("testdata/bin/cpp-x86_64.elf", &["why", "log"])
        .error();
    assert!(!several.contains("file here"), "{several}");
}

#[test]
fn a_file_here_that_finds_its_item_gets_no_line() {
    // `main` is a file here, but it names an item, so it was typed as
    // meant, and the error is for `zzq_none` alone, as in the crate's
    // root, where `main` isn't a file.
    let here = TempFolder::new("found-files", &["main"], &[]);
    for args in [
        &["callers", "main", "zzq_none"][..],
        &["callers", "zzq_none", "main"],
        &["retained", "main", "zzq_none"],
        &["retained", "zzq_none", "main"],
    ] {
        let error = here.run(TCM_SAMPLE, args).error();
        assert!(!error.contains("file here"), "{error}");
        assert_output(&error, &stderr(&[&[TCM_SAMPLE], args].concat()));
    }
}

#[test]
fn clap_s_error_names_files_whose_names_items_have_too() {
    // The file isn't read before clap's error, so `main` counts, which
    // names an item.
    let here = TempFolder::new("clap-names-files", &["main", "main.c"], &[]);
    let error = here.run(TCM_SAMPLE, &["why", "main", "main.c"]).error();
    assert!(
        error.starts_with(
            "error: unexpected argument 'main.c' found\nmain and main.c are files here: the shell \
             may have put them in place of a pattern. Put a pattern in single quotes, as in \
             'main*', so that the shell passes it as typed.\n\nUsage:"
        ),
        "{error}"
    );
}

#[test]
fn an_unexpected_option_gets_no_line() {
    let here = TempFolder::new("unexpected-option", &["uart_hal.c"], &[]);
    // As in the crate's root, where uart_hal.c isn't a file.
    for args in [
        &["why", "uart_hal.c", "--jsn"][..],
        &["top", "--jsn", "uart_hal.c"],
        &["top", "--jsn", "uart_*"],
    ] {
        assert_output(
            &here.run(TCM_SAMPLE, args).error(),
            &stderr(&[&[TCM_SAMPLE], args].concat()),
        );
    }
}

#[test]
fn an_argument_before_the_command_gets_no_line() {
    // clap refuses it in the file's place, and its error is the top
    // level's, even when a file here or a pattern comes after the command.
    let top_level = "\n\nUsage: elfq [OPTIONS] <FILE> [COMMAND]\n\nFor more information, try \
                     '--help'.\n";
    let here = TempFolder::new("before-the-command", &["on_button.c", "on_timer.c"], &[]);
    assert_output(
        &(here.run(TCM_SAMPLE, &["on_button.c", "why", "on_timer.c"])).error(),
        &format!("error: unexpected argument 'on_button.c' found{top_level}"),
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "x.elf", "top", "core::fmt::*"]),
        &format!("error: unexpected argument 'x.elf' found{top_level}"),
    );
}

#[test]
fn the_lines_under_clap_s_error_keep_its_colours() {
    // CLICOLOR_FORCE colours clap's errors where stderr isn't a terminal.
    let output = command()
        .args([TCM_SAMPLE, "top", "core::fmt::*"])
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    let error = Run::new("elfq top 'core::fmt::*' in colour", output).error();
    assert!(
        error.starts_with(
            "\x1b[1m\x1b[31merror:\x1b[0m unexpected argument '\x1b[33mcore::fmt::*\x1b[0m' \
             found\ntop takes no SYMBOL; retained 'core::fmt::*' lists those items, with what \
             each keeps.\n\n\x1b[1m\x1b[4mUsage:\x1b[0m"
        ),
        "{error:?}"
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
fn the_help_says_which_commands_take_patterns() {
    for (command, other_side) in [("callers", "--from"), ("callees", "--to")] {
        let help = stdout(&[SAMPLE, command, "--help"]);
        for line in [
            format!(
                "Usage: elfq <FILE> {command} [OPTIONS] <SYMBOL>\n       \
                 elfq <FILE> {command} [OPTIONS] <SYMBOL|PATTERN|RANGE>...\n"
            ),
            "Patterns, such as 'uart_*', and several SYMBOLs answer the same way, for\n"
                .to_string(),
            format!(
                "Exit status, with patterns, ranges, several SYMBOLs or {other_side}: 1 if the\n"
            ),
        ] {
            assert!(help.contains(&line), "{line}\nisn't in:\n{help}");
        }
        // What a pattern takes ends the page.
        assert!(
            help.ends_with("Quote a pattern, so that the shell passes it as typed.\n"),
            "{help}"
        );
    }
    // The other commands that take SYMBOL take a pattern that matches one
    // item, and browse lists several in its go-to box.
    for command in ["why", "reach", "inlined"] {
        let help = stdout(&[SAMPLE, command, "--help"]);
        let line = " or an address such as 0x1700. Or a pattern such as 'on_b*', where * stands \
                    for any text, that matches one item";
        let line = match command {
            "inlined" => line.replace(" such as 0x1700", " in it"),
            _ => line.to_string(),
        };
        assert!(help.contains(&line), "{line}\nisn't in:\n{help}");
    }
    let help = stdout(&[SAMPLE, "browse", "--help"]);
    let line = " or an address in it. Or a pattern such as 'on_*', where * stands for any text: one \
                that matches several lists them in the go-to box\n";
    assert!(help.contains(line), "{line}\nisn't in:\n{help}");
    let help = stdout(&["--help"]);
    let paragraph = "\n\nSYMBOL can be a pattern such as 'uart_*', where * stands for any text:\n\
                     retained, callers and callees take every item it matches, and the other\n\
                     commands one item it matches. Quote it, so that the shell passes it as\n\
                     typed.\n\n";
    assert!(help.contains(paragraph), "{paragraph}\nisn't in:\n{help}");
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
