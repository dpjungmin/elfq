//! elfq answers questions about an ELF binary's call graph and size.
//!
//! docs/design.md explains how elfq works, and CONTRIBUTING.md says where
//! things live and the rules the code keeps. The library exists for the
//! elfq program and its tests, so its API may change in any release, and
//! `arch` and `render` are private to it. Each module uses only the ones
//! after it in this list:
//!
//! - [`commands`]: one module per command, plus the ones they share. A
//!   command looks up what the user asks about, asks `graph` or
//!   `debuginfo`, and writes the answer.
//! - [`cli`]: command-line arguments.
//! - `render`: tables and trees, for text output.
//! - [`debuginfo`]: reads DWARF, when the file has it, for what the compiler
//!   inlined.
//! - [`graph`]: connects items by the references between them, and works
//!   out the roots, what is exclusive to what, the shortest chains to each
//!   item, the references that cross the boundary of ranges of memory and
//!   where a linker's veneers jump to.
//! - [`binary`]: reads an ELF file and finds its functions and data objects.
//! - `arch`: finds the addresses that machine code calls or uses.

pub(crate) mod arch;
pub mod binary;
pub mod cli;
pub mod commands;
pub mod debuginfo;
pub mod graph;
pub(crate) mod render;
