//! What the commands share in their errors and help.
//!
//! The tests check the arguments and files that the commands refuse,
//! errors on stderr with exit status 2 and what to try instead, the exit
//! status when what reads the answer stops early, and help before or
//! after a command. Each command's own errors are tested in its file in
//! commands/.

use std::process::Stdio;

use object::elf;

use crate::support::elf::{ElfBytes, SectionField};
use crate::support::{
    LIBRARY_SAMPLE, Run, SAMPLE, TCM_SAMPLE, assert_output, command, stderr, stdout,
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
