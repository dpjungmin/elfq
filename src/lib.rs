//! elfq answers questions about an ELF binary's call graph and size.
//!
//! The code is organized as a pipeline; docs/design.md describes all of it.
//! So far it has:
//!
//! - [`binary`]: reads an ELF file and finds its functions and data objects.
//! - [`arch`]: finds the addresses that machine code calls or uses.
//! - [`graph`]: connects items by the references between them.
//! - [`commands`]: one module per subcommand, connecting a question to output.
//! - [`render`]: text output.
//! - [`cli`]: command-line arguments.

pub mod arch;
pub mod binary;
pub mod cli;
pub mod commands;
pub mod graph;
pub mod render;
