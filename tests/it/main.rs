//! The integration tests, built as one test program.
//!
//! That way they link against elfq once. Each module tests one part of
//! elfq on the sample binaries in testdata/bin: `commands` holds each
//! command's tests, in a file named after it, `library` calls the library
//! rather than running the program, and `support` holds what the modules
//! share.

mod cli;
mod commands;
mod damaged;
mod docs;
mod json;
mod library;
mod names;
mod support;
mod tables;
