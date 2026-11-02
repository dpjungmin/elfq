//! The integration tests, built as one test program.
//!
//! That way they link against elfq once. Each module tests one part of
//! elfq on the sample binaries in testdata/bin; `library` calls the
//! library rather than running the program, and `support` holds what the
//! modules share.

#[cfg(feature = "tui")]
mod browse;
mod cli;
mod damaged;
mod docs;
mod json;
mod library;
mod reach;
mod startup;
mod support;
