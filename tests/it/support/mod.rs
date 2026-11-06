//! What several test modules share.
//!
//! It holds the sample files, running elfq the way a user would, and
//! checking whole outputs; and in [`elf`], changing a sample's bytes.

pub(crate) mod elf;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clap::Parser;
use serde_json::Value;

use elfq::binary::Binary;
use elfq::cli::Cli;
use elfq::commands::{Outcome, symbol};
use elfq::graph::Node;
use object::{Architecture, Object};

/// The C sample for x86-64 Linux, which most tests run on.
pub(crate) const SAMPLE: &str = "testdata/bin/c-x86_64.elf";
/// Firmware with code and data in several kinds of memory.
pub(crate) const TCM_SAMPLE: &str = "testdata/bin/c-cortex-m7.elf";
/// Calls and inlined code nested 20 deep.
pub(crate) const DEEP_SAMPLE: &str = "testdata/bin/c-deep.elf";
/// The C sample for x86-64 Linux, built as a shared library.
pub(crate) const LIBRARY_SAMPLE: &str = "testdata/bin/c-x86_64.so";
/// [`SAMPLE`] with one change, for diff: crc32 logs an empty frame.
pub(crate) const MOVED_SAMPLE: &str = "testdata/bin/c-x86_64-moved.elf";
/// [`TCM_SAMPLE`] with the same change, and filter_tap out of ITCM.
pub(crate) const TCM_MOVED_SAMPLE: &str = "testdata/bin/c-cortex-m7-moved.elf";
/// [`TCM_SAMPLE`] with DTCM linked at 0x80000000.
pub(crate) const TCM_SDRAM_SAMPLE: &str = "testdata/bin/c-cortex-m7-sdram.elf";
/// [`TCM_SAMPLE`] with tx_head in OCRAM, which the reset handler doesn't
/// zero.
pub(crate) const TCM_OCRAM_SAMPLE: &str = "testdata/bin/c-cortex-m7-ocram.elf";
/// [`TCM_SAMPLE`] with decode_header inlined into process_frame.
pub(crate) const TCM_INLINED_SAMPLE: &str = "testdata/bin/c-cortex-m7-inlined.elf";
/// [`TCM_SAMPLE`] with two constants changed: one that decode_header
/// compares with, and one of fir_coeffs.
pub(crate) const TCM_EDITED_SAMPLE: &str = "testdata/bin/c-cortex-m7-edited.elf";
/// [`TCM_SAMPLE`] linked with `main` as its entry point, so that no word of
/// its vector table holds the entry point.
pub(crate) const TCM_ENTRY_MAIN_SAMPLE: &str = "testdata/bin/c-cortex-m7-entry-main.elf";

/// The C sample, built for each instruction set that elfq reads.
///
/// testdata/README.md gives their answers.
pub(crate) const C_SAMPLES: [&str; 2] = [SAMPLE, "testdata/bin/c-cortex-m4.elf"];

/// The builds of c/far.c with symbols, which call through veneers.
///
/// Their calls between RAM, flash and ROM go through the veneers that GNU
/// ld and LLD add for Cortex-M, all but two of LLD's (see
/// `testdata/README.md`).
pub(crate) const FAR_SAMPLES: [&str; 8] = [
    "testdata/bin/far-gnu-m4-nodebug.elf",
    "testdata/bin/far-gnu-m4-pure-nodebug.elf",
    "testdata/bin/far-gnu-m4-pic-nodebug.elf",
    "testdata/bin/far-gnu-m0-nodebug.elf",
    "testdata/bin/far-lld-m4-nodebug.elf",
    "testdata/bin/far-lld-m4-pic-nodebug.elf",
    "testdata/bin/far-lld-m0-nodebug.elf",
    "testdata/bin/far-lld-m0-pure-nodebug.elf",
];

/// Where the sample at `path` is, from wherever the test runs. Tests name a
/// sample by its path relative to the crate, as [`SAMPLE`] and
/// [`samples`] give it, since that is what elfq is given as FILE.
pub(crate) fn sample_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// Every sample file, as a path relative to the crate.
pub(crate) fn samples() -> Vec<String> {
    let bin = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/bin");
    let mut samples: Vec<String> = std::fs::read_dir(bin)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".elf") || name.ends_with(".so"))
        .map(|name| format!("testdata/bin/{name}"))
        .collect();
    samples.sort();
    assert!(samples.len() > 10);
    samples
}

/// Every sample whose code is for `architecture`.
///
/// The machine in a sample's ELF header says what its code is for. They
/// come in the order of [`samples`].
pub(crate) fn samples_for(architecture: Architecture) -> Vec<String> {
    let samples: Vec<String> = samples()
        .into_iter()
        .filter(|path| {
            let data = std::fs::read(sample_path(path)).unwrap();
            let file = object::File::parse(&*data).unwrap_or_else(|err| panic!("{path}: {err}"));
            file.architecture() == architecture
        })
        .collect();
    assert!(!samples.is_empty(), "{architecture:?}");
    samples
}

/// Loads the sample at `path`, relative to the crate, as [`SAMPLE`].
pub(crate) fn load_sample(path: &str) -> Binary {
    Binary::load(&sample_path(path)).unwrap_or_else(|err| panic!("{path}: {err:#}"))
}

/// What elfq finds for `name` as a SYMBOL.
pub(crate) fn node(binary: &Binary, name: &str) -> Node {
    symbol::find(binary, name)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
        .node
}

/// The elfq program, set to run in the crate's root.
///
/// There, sample paths such as [`SAMPLE`] work. [`run`] runs it; a test
/// that needs more, such as a pipe for its input, starts it itself and
/// reads what it did with [`Run::new`].
pub(crate) fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_elfq"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

/// Runs elfq with `args` and returns the [`Run`], for the test to check.
#[track_caller]
pub(crate) fn run(args: &[&str]) -> Run {
    let output = command().args(args).output().unwrap();
    Run::new(&format!("elfq {}", args.join(" ")), output)
}

/// One run of elfq: its exit status and what it printed.
///
/// Tests read it with [`Run::printed`], [`Run::json`] or [`Run::error`],
/// which check the exit status and that elfq printed nothing on the other
/// stream, so that a warning or an answer that a test doesn't expect fails
/// it. A test that expects something on both, such as an answer with a
/// warning, checks the status with [`Run::exited`] and reads both itself.
/// A `Run` that nothing reads checks nothing, so the compiler warns about
/// one.
#[must_use = "read it with printed, json, error or exited, which check it"]
pub(crate) struct Run {
    /// What ran, for the message of a check that fails.
    command: String,
    /// The exit status.
    status: i32,
    /// What elfq printed on stdout.
    pub(crate) stdout: String,
    /// What elfq printed on stderr.
    pub(crate) stderr: String,
}

/// The exit status that a test expects elfq to end with.
#[derive(Clone, Copy)]
pub(crate) enum Exit {
    /// This status.
    ///
    /// It is 0 for an answer, 1 for an answer that a script may act on, as
    /// when something crosses the edge of a range or two builds differ,
    /// and 2 for an error.
    Status(i32),
    /// 0 or 1: an answer, whether or not it holds something to act on.
    ///
    /// It is for a test that runs many commands, or one on many files, and
    /// doesn't check which of the two each gives.
    ZeroOrOne,
}

impl Exit {
    /// Whether elfq exiting with `status` is what the test expects.
    pub(crate) fn allows(self, status: i32) -> bool {
        match self {
            Exit::Status(expected) => status == expected,
            Exit::ZeroOrOne => status == 0 || status == 1,
        }
    }
}

impl From<i32> for Exit {
    fn from(status: i32) -> Exit {
        Exit::Status(status)
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Exit::Status(status) => write!(f, "{status}"),
            Exit::ZeroOrOne => write!(f, "0 or 1"),
        }
    }
}

impl Run {
    /// What elfq did, from the `output` of a run that a test starts itself.
    ///
    /// The test starts it with [`command`], as for a pipe. `command_line`
    /// says what ran, for the message of a check that fails.
    #[track_caller]
    pub(crate) fn new(command_line: &str, output: Output) -> Run {
        let Some(status) = output.status.code() else {
            panic!("`{command_line}` ended without an exit status: {output:?}");
        };
        Run {
            command: command_line.to_string(),
            status,
            stdout: decode(command_line, "stdout", output.stdout),
            stderr: decode(command_line, "stderr", output.stderr),
        }
    }

    /// What elfq printed on stdout.
    ///
    /// It checks that elfq exited with `status` and printed nothing on
    /// stderr.
    #[track_caller]
    pub(crate) fn printed(self, status: impl Into<Exit>) -> String {
        let status = status.into();
        self.check(
            status.allows(self.status) && self.stderr.is_empty(),
            &format!("exit status {status} and nothing on stderr"),
        );
        self.stdout
    }

    /// What elfq printed on stdout, parsed as JSON.
    ///
    /// It makes the checks of [`Run::printed`].
    #[track_caller]
    pub(crate) fn json(self, status: impl Into<Exit>) -> Value {
        let command = self.command.clone();
        let text = self.printed(status);
        match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => panic!("`{command}` printed no JSON ({err}):\n{text}"),
        }
    }

    /// The error that elfq printed on stderr.
    ///
    /// It checks that elfq exited with 2 and printed nothing on stdout.
    #[track_caller]
    pub(crate) fn error(self) -> String {
        self.check(
            self.status == 2 && self.stdout.is_empty(),
            "exit status 2 and nothing on stdout",
        );
        self.stderr
    }

    /// The run, after checking only that elfq exited with `status`.
    ///
    /// It is for a test that checks both streams itself, as one that
    /// expects a warning on stderr with the answer on stdout.
    #[track_caller]
    pub(crate) fn exited(self, status: impl Into<Exit>) -> Run {
        let status = status.into();
        self.check(status.allows(self.status), &format!("exit status {status}"));
        self
    }

    /// Panics unless the expectation `holds`.
    ///
    /// The message says what the test `expected` and what elfq did.
    #[track_caller]
    fn check(&self, holds: bool, expected: &str) {
        assert!(holds, "expected {expected}, but {self:?}");
    }
}

impl fmt::Debug for Run {
    /// What ran and what it did, a stream to a paragraph.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "`{}` exited with {}", self.command, self.status)?;
        for (name, text) in [("stdout", &self.stdout), ("stderr", &self.stderr)] {
            match text.is_empty() {
                true => write!(f, "\n\nIt printed nothing on {name}.")?,
                false => write!(f, "\n\nIt printed on {name}:\n{text}")?,
            }
        }
        Ok(())
    }
}

/// `bytes`, which `command_line` printed on `stream`, as text.
#[track_caller]
fn decode(command_line: &str, stream: &str, bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => panic!("`{command_line}` printed text that isn't UTF-8 on {stream}: {err}"),
    }
}

/// What elfq printed on stdout with `args`.
///
/// It checks that elfq answered with exit status 0 and printed nothing on
/// stderr: [`Run::printed`] with 0.
#[track_caller]
pub(crate) fn stdout(args: &[&str]) -> String {
    run(args).printed(0)
}

/// The error that elfq printed on stderr with `args`.
///
/// It checks that elfq exited with 2 and printed nothing on stdout:
/// [`Run::error`].
#[track_caller]
pub(crate) fn stderr(args: &[&str]) -> String {
    run(args).error()
}

/// What elfq printed on stdout with `args`, ending with exit status 1.
///
/// It checks that elfq found something that crosses the edge of the
/// ranges it was given, which status 1 says, and printed nothing on
/// stderr: [`Run::printed`] with 1.
#[track_caller]
pub(crate) fn crossing(args: &[&str]) -> String {
    run(args).printed(1)
}

/// What elfq printed on stdout with `args` and `--json`, parsed.
///
/// It checks that elfq answered with exit status 0 and printed nothing on
/// stderr: [`Run::json`] with 0.
#[track_caller]
pub(crate) fn elfq_json(args: &[&str]) -> Value {
    run(&[args, &["--json"]].concat()).json(0)
}

/// The names of the items in a list of objects that each have an `item`, or
/// are items.
pub(crate) fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.get("item").unwrap_or(entry)["name"].as_str().unwrap())
        .collect()
}

/// The names in the rows of a command's table with `--all`, in order.
///
/// They are without what each says about the row it hangs under, to
/// compare with the rows of the JSON. It checks that elfq answered with
/// exit status 0 and printed nothing on stderr.
#[track_caller]
pub(crate) fn text_rows(args: &[&str]) -> Vec<String> {
    let text = run(&[args, &["--all"]].concat()).printed(0);
    // The table starts after the first blank line, with its header, and
    // the names start where the header's last column does.
    let mut lines = text.lines().skip_while(|line| !line.is_empty()).skip(1);
    let header = lines.next().unwrap();
    let start = header.rfind(' ').unwrap() + 1;
    lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let name = line[start..].split(", kept by ").next().unwrap();
            let name = name.split(", inlined into ").next().unwrap();
            name.trim_end_matches(" (through a pointer)").to_string()
        })
        .collect()
}

/// Splits a command line into its arguments as a shell would, for the
/// quotes that elfq puts around a name such as `'log(char const*)'`, and
/// that a pattern such as `'uart_*'` needs: at runs of whitespace outside
/// quotes, with what `'...'` or `"..."` holds kept as it is, and a `\`
/// outside them keeping the character after it. It knows nothing else of
/// the shell, such as variables, globs, or a `\` inside `"..."`, which the
/// command lines that the tests split don't have.
pub(crate) fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    // The word so far, once a character or a quote starts one, so that
    // `''` is an empty word but whitespace between words is none.
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => word.get_or_insert_default().push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            None if c == '\\' => word.get_or_insert_default().extend(chars.next()),
            None if c.is_whitespace() => words.extend(word.take()),
            None => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

/// Runs `elfq SAMPLE ARGS` in this process, for the sample at `sample`.
///
/// It returns what elfq printed on stdout, with the outcome that sets its
/// exit status. It is much quicker than [`run`], which starts the
/// program, so a test that runs elfq hundreds of times, as on each item of
/// a sample, uses it. It sees less than a user would: an error panics,
/// and a warning goes to the test's own stderr, where nothing checks it.
/// A test of what elfq prints on stderr, or of its exit status as such,
/// uses [`run`].
#[track_caller]
pub(crate) fn run_in_process(sample: &str, args: &[&str]) -> (Outcome, String) {
    let path = sample_path(sample);
    let command_line = [&["elfq", path.to_str().unwrap()], args].concat();
    let cli = Cli::try_parse_from(command_line).unwrap();
    let mut out = Vec::new();
    let outcome = match elfq::commands::run(&cli, &mut out) {
        Ok(outcome) => outcome,
        Err(err) => panic!("{sample} {args:?}: {err:#}"),
    };
    (outcome, String::from_utf8(out).unwrap())
}

/// Checks that elfq printed `expected`. When it didn't, the failure names
/// the first line that differs and then prints the whole output as it is,
/// ready to paste into the test.
#[track_caller]
pub(crate) fn assert_output(actual: &str, expected: &str) {
    if actual == expected {
        return;
    }
    let show = |line: Option<&str>| line.map_or("the end".to_string(), |line| format!("{line:?}"));
    let mut actual_lines = actual.split_inclusive('\n');
    let mut expected_lines = expected.split_inclusive('\n');
    for number in 1.. {
        let (got, wanted) = (actual_lines.next(), expected_lines.next());
        if got != wanted {
            panic!(
                "the output differs from line {number}:\n\
                 expected: {}\n\
                 actual:   {}\n\n\
                 The whole output:\n{actual}",
                show(wanted),
                show(got)
            );
        }
    }
}
