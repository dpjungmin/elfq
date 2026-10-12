//! Command-line arguments. Each command has a variant here and a module in
//! `commands/`.
//!
//! The file comes first and the question after it, so that a command sits
//! next to the symbol it asks about: `elfq app.elf callers log_msg`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

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
    pub file: PathBuf,
    /// What to ask about FILE. Without one, elfq prints an overview.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// How to print the answer
    #[arg(long, global = true, value_enum, default_value_t = Format::Text)]
    pub format: Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Tables and trees, for people
    Text,
    /// JSON, for scripts
    Json,
}

const EXAMPLES: &str = "\
Examples:
  elfq app.elf                            Overview of app.elf
  elfq app.elf top                        What keeps the most bytes in app.elf
  elfq app.elf retained process_frame     What process_frame keeps in the file
  elfq app.elf exclusive process_frame    What only process_frame leads to
  elfq app.elf why crc_table              What keeps crc_table in the file
  elfq app.elf inlined                    Which inlined functions take the most bytes
  elfq app.elf callers log_msg            What calls or uses log_msg
  elfq app.elf callees main               What main calls or uses";

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the items that keep the most bytes in the file, as a tree
    #[command(after_help = TOP)]
    Top,
    /// Show what SYMBOL keeps in the file, as a tree, and how many bytes
    #[command(after_help = EXCLUSIVE)]
    Retained(SymbolArgs),
    /// List the items that only SYMBOL leads to
    #[command(after_help = EXCLUSIVE)]
    Exclusive(SymbolArgs),
    /// Show the chains of calls and references that keep SYMBOL in the file
    #[command(after_help = format!("{WHY}\n\n{HOW}"))]
    Why(SymbolArgs),
    /// List the functions the compiler inlined, by the bytes their copies take
    #[command(after_help = INLINED)]
    Inlined(InlinedArgs),
    /// List what calls or uses SYMBOL
    #[command(after_help = HOW)]
    Callers(SymbolArgs),
    /// List what SYMBOL calls or uses
    #[command(after_help = HOW)]
    Callees(SymbolArgs),
    /// Show elfq's own view of the file (for developing elfq)
    #[command(subcommand, hide = true)]
    Debug(DebugCommand),
}

const HOW: &str = "\
The HOW column says how one item uses the other:
  call           a direct call
  tail call      a jump to the start of a function, which then returns to
                 the caller's caller
  address taken  code computes or loads the address, as when passing a
                 pointer (Arm code also does this to use a variable)
  access         code reads or writes it
  pointer        data holds a pointer to it, as in a table of functions

Calls through a pointer set at run time don't show, since elfq can't tell
where they go; the address taken and pointer rows show where such pointers
come from.";

const WHY: &str = "\
The tree starts where the program does: at the entry point, a function the
C runtime runs at startup, or a handler in the vector table. Each item in
it is used by the one above it, and the chains end at SYMBOL. Only the
shortest chain through each item that uses SYMBOL is shown.";

const TOP: &str = "\
An item's retained size is its own size plus the sizes of the items that
only it leads to, from where the program starts: what removing it would
save. Each item hangs under the closest item that every path to it goes
through, and the items at the top are where the program starts, or are
reached from more than one of those places. The tree shows the 30 largest
items; lines such as \"└─ 6 more\" sum up the rest.

A function that is only called through pointers counts for the code or data
that holds its address. Sizes marked ~ are estimates.";

const INLINED: &str = "\
The compiler inlines a function by putting a copy of its code into the
function that calls it. Only DWARF debug info says where it did that, so
this command needs a file built with debug info, such as with -g.

In the list, a function's bytes leave out the functions inlined into its
copies in turn, which have rows of their own. In the tree that --in shows,
they include the rows below. A copy of 0 bytes is one the compiler merged
into the code around it.";

const EXCLUSIVE: &str = "\
An item is exclusive to SYMBOL when every path to it, from where the program
starts, goes through SYMBOL: without SYMBOL it would be dead code. SYMBOL's
retained size is its own size plus the sizes of everything exclusive to it,
which is what removing SYMBOL would save.

A function that is only called through pointers counts for the code or data
that holds its address. Sizes marked ~ are estimates.";

#[derive(Debug, Args)]
pub struct SymbolArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
}

#[derive(Debug, Args)]
pub struct InlinedArgs {
    /// Show the inlined code in one function instead, as a tree. SYMBOL is
    /// its name, the end of its name, or an address in it
    #[arg(long = "in", value_name = "SYMBOL")]
    pub within: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// List the functions and data objects elfq found, and where it found each
    Items,
}

/// The names of the commands, for spotting a command typed before the file.
pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .collect()
}
