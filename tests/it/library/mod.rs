//! The library, called as the commands call it, rather than the program.
//!
//! The tests call it as with `Binary::load` and `Graph::build`, and check
//! what each part finds in the samples against the answers in
//! testdata/README.md.

mod graph;
mod inlined;
mod items;
mod retained;
mod symbol;
mod why;
