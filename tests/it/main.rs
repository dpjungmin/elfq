//! The integration tests, built as one test program so that they link
//! against elfq once. Each module tests one part of elfq on the sample
//! binaries in testdata/bin.

mod cli;
mod graph;
mod inlined;
mod items;
mod json;
mod retained;
mod symbol;
mod why;
