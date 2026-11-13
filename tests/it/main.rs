//! The integration tests, built as one test program.
//!
//! That way they link against elfq once. They test elfq on the sample
//! binaries in testdata/bin. `commands` holds each command's tests, in a
//! file named after it, and `across_commands` the tests of what the
//! commands share; `damaged` tests elfq, both by running it and by calling
//! the library, on files made by changing a sample's bytes; `docs` runs
//! the examples in the docs, checks the paths that CONTRIBUTING.md and
//! design.md cite, and checks that `scripts/same-output.sh` runs every
//! command; `library` calls the library rather than running the program;
//! and `support` holds what the modules share.

#![allow(clippy::too_many_lines, reason = "tests list cases or pin outputs")]

mod across_commands;
mod commands;
mod damaged;
mod docs;
mod library;
mod support;
