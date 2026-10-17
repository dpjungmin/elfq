//! What several test modules share: the sample files, running elfq the way
//! a user would, and checking whole outputs.

use std::path::Path;
use std::process::{Command, Output};

use elfq::binary::Binary;
use elfq::commands::symbol;
use elfq::graph::Node;

/// The C sample for x86-64 Linux, which most tests run on.
pub const SAMPLE: &str = "testdata/bin/c-x86_64.elf";
/// Firmware with code and data in several kinds of memory.
pub const TCM_SAMPLE: &str = "testdata/bin/c-cortex-m7.elf";
/// Calls and inlined code nested 20 deep.
pub const DEEP_SAMPLE: &str = "testdata/bin/c-deep.elf";

/// Loads the sample `name` from testdata/bin.
pub fn load_sample(name: &str) -> Binary {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/bin")
        .join(name);
    Binary::load(&path).unwrap_or_else(|err| panic!("{name}: {err:#}"))
}

/// What elfq finds for `name` as a SYMBOL.
pub fn node(binary: &Binary, name: &str) -> Node {
    symbol::find(binary, name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// The elfq program, set to run in the crate's root so that sample paths
/// such as [`SAMPLE`] work.
pub fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_elfq"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

/// Runs elfq with `args` and returns what it did.
pub fn elfq(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

/// Checks that elfq printed `expected`. When it didn't, the failure names
/// the first line that differs and then prints the whole output as it is,
/// ready to paste into the test.
#[track_caller]
pub fn assert_output(actual: &str, expected: &str) {
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
