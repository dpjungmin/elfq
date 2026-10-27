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

/// The stack for the thread that runs elfq, and for the one on which `diff`
/// loads its second file. Trees are built, drawn and listed for JSON
/// recursively, as deep as the file nests them: the retained tree as deep
/// as the longest chain of calls, and the tree of inlined copies as deep as
/// the copies nest; C++ names are demangled recursively too. The main
/// thread's stack, usually 8 MiB, runs out at a chain of some 10,000 calls,
/// and of fewer in a debug build; this one holds 32 times as many. The
/// thread's memory grows only as far as its stack is used, but all of its
/// addresses are set aside from the start.
pub const STACK_SIZE: usize = 256 << 20;
