//! The tests of each command, in a file named after it.
//!
//! They check what the elfq program prints for the command, its errors and
//! its exit statuses. A file named after two or three commands holds those
//! that share their table or their tree. What all the commands share, such
//! as the shape of their JSON, their tables, names and errors, is tested
//! in the modules beside this one.

#[cfg(feature = "tui")]
mod browse;
mod callers_and_callees;
mod debug;
mod diff;
mod inlined;
mod overview_top_and_retained;
mod reach;
mod startup;
mod why;
