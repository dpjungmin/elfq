//! What several test modules share: the sample files, and running elfq the
//! way a user would.

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
