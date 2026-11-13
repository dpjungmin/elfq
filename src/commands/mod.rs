//! One module per command, and what they share. This file runs the command
//! that the arguments name. The shared modules are libraries that commands
//! call: `arguments.rs` checks which arguments go together, and words the
//! lines that `main.rs` adds under clap's error for one that a command
//! doesn't take, `checks.rs` the things a command needs before it answers,
//! `symbol/` finds what SYMBOL names, `retained_tree.rs` builds the trees
//! of retained sizes, `uses.rs` holds the table of `callers` and `callees`,
//! `range_check/` answers them for ranges of memory or with `--from` or
//! `--to`, `flat_answer.rs` holds the answers whose table has one row to a
//! line, and `tree_answer.rs` those whose table holds a tree, and how a
//! tree is cut down to its largest rows, and `kept.rs` leaves what a
//! command builds for the system to free. How elfq words things, and
//! which rows of a long list it keeps, are in `text.rs`, and the shapes
//! of its JSON in `json.rs`.
//!
//! A command calls no other command, except to ask whether that one would
//! answer, for a Try line, which ends an answer with a command to run next,
//! or for a suggestion in an error. `browse/` asks the six commands it shows
//! for their answers: the overview, `reach`, `callers`, `callees`, `why` and
//! `retained`.

mod arguments;
#[cfg(feature = "tui")]
pub mod browse;
mod callees;
mod callers;
mod checks;
mod debug_items;
mod debug_matches;
mod diff;
mod flat_answer;
mod inlined;
mod json;
mod kept;
mod overview;
mod range_check;
mod reach;
mod retained;
mod retained_tree;
mod startup;
pub mod symbol;
mod text;
mod top;
mod tree_answer;
mod uses;
mod why;

use std::io::Write;

use anyhow::Result;
#[cfg(not(feature = "tui"))]
use anyhow::bail;

pub use arguments::unexpected_argument_hints;
use kept::Kept;
pub use kept::end_after_answer;
use text::Detail;

use crate::binary::{Binary, distinct_ranges};
use crate::cli::{BrowseArgs, Cli, Command, DebugCommand};

/// What an answer means for elfq's exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exit status 0.
    Answered,
    /// Something crosses the boundary of the ranges that `callers` or
    /// `callees` asked about, or may, or `reach --to` finds something in
    /// its ranges, or may: exit status 1, so that a script can check that
    /// code in a range stays in it.
    Crossed,
    /// `diff` finds that the two builds load different memory: exit status
    /// 1, so that a script can check that a change leaves a build's memory
    /// as it was.
    Differs,
    /// `startup` finds a problem in what the reset code does before
    /// `main`, or code it can't follow that may be one: exit status 1, so
    /// that a script can check that a build still starts.
    Problems,
}

/// Runs the command in `cli`, writing its answer to `out`.
pub fn run(cli: &Cli, out: &mut dyn Write) -> Result<Outcome> {
    arguments::check_before_loading(cli)?;
    let format = cli.format();
    let range = cli.range;
    let detail = Detail::from_flag(cli.all());
    // The browser takes over the terminal, so it loads the file itself,
    // and prints the warnings once it gives the terminal back.
    if let Some(Command::Browse(args)) = &cli.command {
        return run_browse(cli, args);
    }
    // diff reads two files, and needs only their headers and the bytes of
    // their sections.
    if let Some(Command::Diff(args)) = &cli.command {
        return diff::run(&cli.file, &args.new, detail, format, out);
    }
    // startup refuses what isn't Cortex-M firmware from the file's headers,
    // before it loads the rest.
    if let Some(Command::Startup(_)) = &cli.command {
        startup::check_header(&cli.file)?;
    }
    let binary = Kept::new(Binary::load(&cli.file)?);
    write_warnings(&binary);
    arguments::check_after_loading(cli, &binary)?;
    match &cli.command {
        None => overview::run(&cli.file, &binary, range, format, out),
        Some(Command::Top(_)) => top::run(&binary, range, detail, format, out),
        Some(Command::Retained(args)) => {
            retained::run(&binary, &args.symbols, range, detail, format, out)
        }
        Some(Command::Why(args)) => why::run(&binary, &args.symbol, detail, format, out),
        Some(Command::Inlined(args)) => inlined::run(
            &cli.file,
            &binary,
            args.symbol.as_deref(),
            detail,
            format,
            out,
        ),
        // In callers, callees and reach, a range given twice to `--from` or
        // `--to` is named once, as one given twice in place of SYMBOL is.
        Some(Command::Callers(args)) => callers::run(
            &binary,
            &args.uses.symbols,
            &distinct_ranges(args.from.iter().copied()),
            detail,
            format,
            out,
        ),
        Some(Command::Callees(args)) => callees::run(
            &binary,
            &args.uses.symbols,
            &distinct_ranges(args.to.iter().copied()),
            detail,
            format,
            out,
        ),
        Some(Command::Reach(args)) => {
            let to = distinct_ranges(args.to.iter().copied());
            match &args.symbol {
                Some(symbol) => reach::run(&binary, symbol, &to, detail, format, out),
                None => reach::run_starts(&cli.file, &binary, &to, range, detail, format, out),
            }
        }
        Some(Command::Startup(_)) => startup::run(&cli.file, &binary, detail, format, out),
        Some(Command::Debug(DebugCommand::Items)) => debug_items::run(&binary, out),
        Some(Command::Debug(DebugCommand::Walk)) => startup::debug::run(&cli.file, &binary, out),
        Some(Command::Debug(DebugCommand::Matches(args))) => {
            debug_matches::run(&binary, &args.patterns, out)
        }
        // These two run before the file is loaded, above.
        Some(Command::Browse(_)) => unreachable!("browse has run above"),
        Some(Command::Diff(_)) => unreachable!("diff has run above"),
    }
}

/// Prints the file's warnings to stderr.
fn write_warnings(binary: &Binary) {
    for warning in &binary.warnings {
        // Not `eprintln!`, which panics when nothing reads standard error
        // any more: the answer still counts.
        let _ = writeln!(std::io::stderr(), "warning: {warning}");
    }
}

/// Runs `elfq FILE browse`, in a build with the browser.
#[cfg(feature = "tui")]
fn run_browse(cli: &Cli, args: &BrowseArgs) -> Result<Outcome> {
    browse::run(&cli.file, args.symbol.as_deref())?;
    Ok(Outcome::Answered)
}

/// Says that this build has no browser.
#[cfg(not(feature = "tui"))]
fn run_browse(_: &Cli, _: &BrowseArgs) -> Result<Outcome> {
    bail!("this elfq was built without browse (the tui feature)")
}
