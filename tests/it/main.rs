//! The integration tests, built as one test program.
//!
//! That way they link against elfq once. They test elfq on the sample
//! binaries in testdata/bin. `commands` holds each command's tests, in a
//! file named after it; `json`, `tables`, `names` and `errors_and_help`
//! test what the commands share; `damaged` tests elfq, both by running it
//! and by calling the library, on files made by changing a sample's bytes;
//! `docs` runs the examples in the docs; `library` calls the library
//! rather than running the program; and `support` holds what the modules
//! share.

mod commands;
mod damaged;
mod docs;
mod errors_and_help;
mod json;
mod library;
mod names;
mod support;
mod tables;
