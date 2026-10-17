//! The integration tests, built as one test program so that they link
//! against elfq once. Each module tests one part of elfq on the sample
//! binaries in testdata/bin, except `support`, which holds what they share.

mod cli;
mod docs;
mod graph;
mod inlined;
mod items;
mod json;
mod retained;
mod support;
mod symbol;
mod why;
