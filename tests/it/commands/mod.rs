//! The tests of each command, in a file named after it.
//!
//! They check what the elfq program prints for the command, its errors and
//! its exit statuses.

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
