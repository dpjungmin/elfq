//! Command-line arguments. Each command has a variant here and a module in
//! `commands/`.
//!
//! The file comes first and the question after it, so that a command sits
//! next to the symbol it asks about: `elfq app.elf callers log_msg`.
//!
//! The prose that `--help` prints after the options is in `help.rs`, so
//! that this file reads as the command line: `Cli`, `Command` and each
//! command's arguments.

mod help;

#[cfg(feature = "tui")]
pub(crate) use help::HOW_WORDS;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::binary::AddressRange;
use help::{
    AT, BROWSE, CALLEES, CALLEES_RANGES, CALLERS, CALLERS_RANGES, DIFF, EXAMPLES, HOW_NOTES,
    INLINED, PATTERNS, REACH, RETAINED, RETAINED_REACH, RETAINED_SET, ROW_MARKS, STARTUP, TOP, WHY,
    how_table,
};

// The whole command line. No `///` comment here: clap would show one of more
// than one paragraph at the top of `elfq --help`, in place of the
// description that `about` takes from Cargo.toml.
#[derive(Debug, Parser)]
#[command(
    name = "elfq",
    version,
    about,
    arg_required_else_help = true,
    disable_help_subcommand = true,
    after_help = EXAMPLES
)]
pub struct Cli {
    /// The ELF file: an executable, shared library or firmware image
    pub(crate) file: PathBuf,
    /// What to ask about FILE. Without one, elfq prints an overview.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
    /// Print the answer as JSON, for scripts
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Draw the overview, top, retained, why, reach SYMBOL and inlined
    /// SYMBOL as trees
    #[arg(long, global = true)]
    pub(crate) tree: bool,
    /// Count only the bytes in one range of memory, such as ITCM: START..END
    /// or START+SIZE, like 0x20000000+256K. When the file's sections and load
    /// images occupy several ranges, as in firmware, the overview lists them.
    /// For the overview, top and retained.
    #[arg(long, global = true, value_name = "RANGE", value_parser = one_range)]
    pub(crate) range: Option<AddressRange>,
}

/// Reads `--range`, which takes one range: given several, separated by
/// commas, it says so, with the first as the example.
fn one_range(text: &str) -> Result<AddressRange, String> {
    if let Some((first, _)) = text.split_once(',')
        && first.parse::<AddressRange>().is_ok()
    {
        return Err(format!("--range takes one range, such as {}", first.trim()));
    }
    text.parse()
}

/// Reads one `--from`.
fn from_range(text: &str) -> Result<AddressRange, String> {
    one_range_each("--from", text)
}

/// Reads one `--to`.
fn to_range(text: &str) -> Result<AddressRange, String> {
    one_range_each("--to", text)
}

/// Reads one range for `option`, `--from` or `--to`, which take one each:
/// given several, separated by commas, it shows them given apart.
fn one_range_each(option: &str, text: &str) -> Result<AddressRange, String> {
    if !text.contains(',') {
        return text.parse();
    }
    let parts: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    for part in &parts {
        part.parse::<AddressRange>()?;
    }
    match parts[..] {
        [] => text.parse(),
        [_] => Err(format!("{option} takes one range; leave out the comma")),
        _ => {
            let apart: Vec<String> = parts
                .iter()
                .map(|part| format!("{option} {part}"))
                .collect();
            Err(format!(
                "{option} takes one range; give it again for each: {}",
                apart.join(" ")
            ))
        }
    }
}

impl Cli {
    /// How to print the answer. `--json` and `--tree` don't go together,
    /// which `commands::run` checks.
    pub(crate) fn format(&self) -> Format {
        if self.json {
            Format::Json
        } else if self.tree {
            Format::Tree
        } else {
            Format::Text
        }
    }
}

/// How a command prints its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// Tables of one row per line, for people and for tools such as grep
    Text,
    /// The same, but with trees drawn with branches, with `--tree`
    Tree,
    /// JSON, for scripts, with `--json`
    Json,
}

// One variant per command. Each variant's `///` line is its line in
// `elfq --help`.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Show the items that keep the most bytes in the file
    #[command(after_help = format!("{TOP}\n\n{ROW_MARKS}"))]
    Top(TopArgs),
    /// Show what removing SYMBOL, or several at once, would save
    #[command(after_help = format!("{RETAINED}\n\n{RETAINED_SET}\n\n{PATTERNS}\n\n{ROW_MARKS}\n\n{RETAINED_REACH}"))]
    Retained(RetainedArgs),
    /// Show the chains of calls and references that keep SYMBOL in the file
    #[command(after_help = format!("{WHY}\n\n{}\n\n{HOW_NOTES}", how_table()))]
    Why(WhyArgs),
    /// List the functions the compiler inlined, by the bytes their copies take
    #[command(after_help = INLINED)]
    Inlined(InlinedArgs),
    /// List what calls or uses SYMBOL
    #[command(
        override_usage = "elfq <FILE> callers [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callers [OPTIONS] <SYMBOL|PATTERN|RANGE>...",
        after_help = format!("{}\n\n{HOW_NOTES}\n\n{AT}\n\n{CALLERS}\n\n{CALLERS_RANGES}\n\n{PATTERNS}", how_table())
    )]
    Callers(CallersArgs),
    /// List what SYMBOL calls or uses
    #[command(
        override_usage = "elfq <FILE> callees [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callees [OPTIONS] <SYMBOL|PATTERN|RANGE>...",
        after_help = format!("{}\n\n{HOW_NOTES}\n\n{AT}\n\n{CALLEES}\n\n{CALLEES_RANGES}\n\n{PATTERNS}", how_table())
    )]
    Callees(CalleesArgs),
    /// List everything SYMBOL calls or uses, at any depth, by memory block
    #[command(after_help = format!("{REACH}\n\n{}\n\n{HOW_NOTES}", how_table()))]
    Reach(ReachArgs),
    /// Read a function's code in the terminal, and follow what it calls, reaches and is used by
    #[command(after_help = format!("{BROWSE}\n\n{}", how_table()))]
    Browse(BrowseArgs),
    /// Show what changed from FILE to NEW, by block of memory
    #[command(after_help = DIFF)]
    Diff(DiffArgs),
    /// Check that the reset code copies and zeroes memory before it uses it
    #[command(after_help = STARTUP)]
    Startup(StartupArgs),
    /// Show elfq's own view of the file (for developing elfq)
    #[command(subcommand, hide = true)]
    Debug(DebugCommand),
}

#[derive(Debug, Args)]
pub(crate) struct TopArgs {
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RetainedArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or a pattern such as 'uart_*', where * stands for any text, for every
    /// item it matches. Or a range of memory, START..END or START+SIZE, for
    /// every item with a byte in it. Give several to ask what removing them
    /// all at once would save
    #[arg(required = true, value_name = "SYMBOL|PATTERN|RANGE")]
    pub(crate) symbols: Vec<String>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WhyArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or a pattern such as 'on_b*', where * stands for any text, that
    /// matches one item
    pub(crate) symbol: String,
    /// Show the chains through every item that keeps SYMBOL, not only the
    /// 10 nearest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct UsesArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or several SYMBOLs, patterns such as 'uart_*', where * stands for any
    /// text, or ranges of memory, START..END or START+SIZE, to list only
    /// what crosses the edge of what they pick
    // Not required here: commands/arguments.rs says what is missing, and
    // with --range, what to type instead.
    #[arg(value_name = "SYMBOL|PATTERN|RANGE")]
    pub(crate) symbols: Vec<String>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct CallersArgs {
    #[command(flatten)]
    pub(crate) uses: UsesArgs,
    /// List only the callers in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = from_range)]
    pub(crate) from: Vec<AddressRange>,
    // Hidden, so that commands/arguments.rs can say that --to goes with
    // callees, in place of clap's error.
    #[arg(long = "to", hide = true, value_name = "RANGE")]
    pub(crate) misplaced_to: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CalleesArgs {
    #[command(flatten)]
    pub(crate) uses: UsesArgs,
    /// List only the callees in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = to_range)]
    pub(crate) to: Vec<AddressRange>,
    // As `CallersArgs::misplaced_to`.
    #[arg(long = "from", hide = true, value_name = "RANGE")]
    pub(crate) misplaced_from: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReachArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or a pattern such as 'on_b*', where * stands for any text, that
    /// matches one item. Without it, reach lists where the program starts,
    /// and what each start reaches
    pub(crate) symbol: Option<String>,
    /// List only what SYMBOL reaches in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = to_range)]
    pub(crate) to: Vec<AddressRange>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub(crate) all: bool,
    // As `CallersArgs::misplaced_to`.
    #[arg(long = "from", hide = true, value_name = "RANGE")]
    pub(crate) misplaced_from: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct BrowseArgs {
    /// The function, data object or import to open first: its name, the
    /// end of its name, or an address in it. Or a pattern such as 'on_*',
    /// where * stands for any text: one that matches several lists them in
    /// the go-to box
    pub(crate) symbol: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct DiffArgs {
    /// The other build of the program, to compare with FILE
    #[arg(value_name = "NEW")]
    pub(crate) new: PathBuf,
    /// Show every item and section that changed, not only the largest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct StartupArgs {
    /// Show every step, not only the first 30, and name every function in
    /// sentences
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InlinedArgs {
    /// Show the inlined code in one function instead: its name, the end of
    /// its name, or an address in it. Or a pattern such as 'on_b*', where
    /// * stands for any text, that matches one item
    pub(crate) symbol: Option<String>,
    /// Show every function, not only the 30 largest
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DebugCommand {
    /// List the functions and data objects elfq found, and where it found each
    Items,
    /// List what the code run from reset does before main: calls, stores and loops
    Walk,
    /// List the items that each pattern matches, with how, as JSON
    // Hidden from `debug --help` too, unlike the two above, so that adding
    // it changed no help text.
    #[command(hide = true)]
    Matches(MatchesArgs),
}

#[derive(Debug, Args)]
pub(crate) struct MatchesArgs {
    /// A pattern such as 'uart_*', where * stands for any text
    #[arg(required = true, value_name = "PATTERN")]
    pub(crate) patterns: Vec<String>,
}

/// The names of the commands, for reading `elfq help COMMAND` and for
/// spotting a command typed before the file.
pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .collect()
}
