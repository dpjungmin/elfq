//! Command-line arguments. Each command has a variant here and a module in
//! `commands/`.
//!
//! The file comes first and the question after it, so that a command sits
//! next to the symbol it asks about: `elfq app.elf callers log_msg`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::binary::AddressRange;

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
    pub file: PathBuf,
    /// What to ask about FILE. Without one, elfq prints an overview.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Print the answer as JSON, for scripts
    #[arg(long, global = true)]
    pub json: bool,
    /// Draw the overview, top, retained, why and inlined SYMBOL as trees
    #[arg(long, global = true)]
    pub tree: bool,
    /// Count only the bytes in one range of memory, such as ITCM: START..END
    /// or START+SIZE, like 0x20000000+256K. The overview lists the ranges
    /// that the file's sections and load images occupy. For the overview,
    /// top and retained.
    #[arg(long, global = true, value_name = "RANGE", value_parser = one_range)]
    pub range: Option<AddressRange>,
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

impl Cli {
    /// How to print the answer. `--json` and `--tree` don't go together,
    /// which `commands::run` checks.
    pub fn format(&self) -> Format {
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
pub enum Format {
    /// Tables of one row per line, for people and for tools such as grep
    Text,
    /// The same, but with trees drawn with branches, with `--tree`
    Tree,
    /// JSON, for scripts, with `--json`
    Json,
}

const EXAMPLES: &str = "\
Examples:
  elfq app.elf                            Overview of app.elf
  elfq app.elf top                        What keeps the most bytes in app.elf
  elfq app.elf top --tree                 The same, drawn as a tree
  elfq app.elf top --range 0x0+256K       The same, counting only the first 256 KiB
  elfq app.elf retained process_frame     What removing process_frame would save
  elfq app.elf why crc_table              What keeps crc_table in the file
  elfq app.elf inlined                    Which inlined functions take the most bytes
  elfq app.elf inlined main               The inlined code in main
  elfq app.elf callers log_msg            What calls or uses log_msg
  elfq app.elf callees main               What main calls or uses
  elfq app.elf callees 0x0+256K           What the first 256 KiB uses outside it

The overview's table is the start of the one top shows; elfq help top
explains it.";

// One variant per command. Each variant's `///` line is its line in
// `elfq --help`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the items that keep the most bytes in the file
    #[command(after_help = TOP)]
    Top(TopArgs),
    /// Show what removing SYMBOL would save
    #[command(after_help = RETAINED)]
    Retained(RetainedArgs),
    /// Show the chains of calls and references that keep SYMBOL in the file
    #[command(after_help = format!("{WHY}\n\n{HOW}"))]
    Why(WhyArgs),
    /// List the functions the compiler inlined, by the bytes their copies take
    #[command(after_help = INLINED)]
    Inlined(InlinedArgs),
    /// List what calls or uses SYMBOL
    #[command(
        override_usage = "elfq <FILE> callers [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callers [OPTIONS] <RANGE>...",
        after_help = format!("{HOW}\n\n{CALLERS}\n\n{CALLERS_RANGES}")
    )]
    Callers(UsesArgs),
    /// List what SYMBOL calls or uses
    #[command(
        override_usage = "elfq <FILE> callees [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callees [OPTIONS] <RANGE>...",
        after_help = format!("{HOW}\n\n{CALLEES}\n\n{CALLEES_RANGES}")
    )]
    Callees(UsesArgs),
    /// Show elfq's own view of the file (for developing elfq)
    #[command(subcommand, hide = true)]
    Debug(DebugCommand),
}

const HOW: &str = "\
The HOW column says how one item uses the other:
  call           a direct call, or a call through a register that the
                 code loaded with the target's address, as a long call does
  tail call      a jump to the start of a function, which then returns to
                 the caller's caller
  uses           code reads or writes data, or loads its address
  address taken  code loads a function's address, as when passing a
                 function pointer, or an import's
  pointer        data holds a pointer to it, as in a table of functions

elfq can't tell where a call through a pointer set at run time goes; the
address taken and pointer rows show where such pointers come from.";

const CALLERS: &str = "\
The table lists the 30 largest callers by their own size, in order of name,
and a last row such as \"70 more\" counts the rest; --all lists every one.";

const CALLEES: &str = "\
The table lists the 30 largest callees by their own size, in the order
SYMBOL first uses them, and a last row such as \"70 more\" counts the rest;
--all lists every one.";

const CALLEES_RANGES: &str = "\
With ranges of memory in place of SYMBOL, such as 0x0+256K for an ITCM,
elfq lists only what the code and data in them use outside them. Each row
names the item outside first, as in \"memcpy, used by apply_filter\", and
ADDRESS and SIZE are that item's. Every crossing has a row, and --all makes
the sentences name everything they would cut short. Calls and jumps that
elfq can't follow come first, as \"(through a pointer)\", or as \"(outside any
function)\" when they go to an address no function holds. A call through a
veneer, which a linker adds when the target is too far away for the call
instruction, is listed where it calls the veneer, as \"call via veneer\".
elfq knows the veneers that GNU ld and LLD add for Cortex-M.

Exit status: 0 if nothing leaves the ranges, 1 if something does or might,
and 2 for an error.";

const CALLERS_RANGES: &str = "\
With ranges of memory in place of SYMBOL, such as 0x0+256K for an ITCM,
elfq lists only the code and data outside them that use what is in them.
Each row names the item outside first, as in \"process_frame, uses crc32\",
and ADDRESS and SIZE are that item's. Every crossing has a row, and --all
makes the sentences name everything they would cut short. A call through a
veneer, which a linker adds when the target is too far away for the call
instruction, is listed where it calls the veneer, as \"call via veneer\".
elfq knows the veneers that GNU ld and LLD add for Cortex-M.

Exit status: 0 if nothing outside uses the ranges, 1 if something does or
might through a pointer, and 2 for an error.";

const WHY: &str = "\
The chains start where the program does, as HOW says: at the entry point, a
function the C runtime runs at startup, or a handler in the vector table.
Every other row is used by the row above it, or where chains branch, by the
item it names, as in \"process_frame, used by main\", and the chains end at
SYMBOL. Only the shortest chain through each item that uses SYMBOL is
shown, for the 10 nearest, or for every one with --all, and --tree draws
the chains as a tree. Items that nothing reaches, or that only SYMBOL leads
to, don't keep it, and are named below the chains.

The last line names the closest item that every chain goes through, if
there is one: SYMBOL counts toward that item's retained size.";

const TOP: &str = "\
An item's retained size is its own size plus the sizes of the items that
only it leads to, from where the program starts: what removing it would
save. A row such as \"crc_table, kept by crc32\" names the closest item that
every path to it goes through. The rows that name none are where the
program starts, or are reached from more than one of those places. The
table shows the 30 largest items, or every item with --all; rows such as
\"6 more, kept by main\" sum up the rest, and --tree draws the rows as a
tree.

Nothing calls a function marked \"(through a pointer)\" directly, so it counts
for the code or data that holds its address. With --range, a row marked
\"(load image)\" counts the copy of an item that the file stores in the
range for startup code to copy where the program uses it, and one marked
\"(with its load image)\" counts both copies. Sizes marked ~ are estimates.";

const INLINED: &str = "\
The compiler inlines a function by putting a copy of its code into the
function that calls it. Only DWARF debug info says where it did that, so
this command needs a file built with debug info, such as with -g.

In the list, a function's bytes leave out the functions inlined into its
copies in turn, which have rows of their own. For SYMBOL, a row's bytes
include the functions inlined into it, whose rows name it, as in
\"atomic_add, inlined into fetch_add\", or where fetch_add is inlined in
several places, \"atomic_add, inlined into fetch_add in update_stats\";
--tree draws the rows as a tree. A copy of 0 bytes is one the compiler
merged into the code around it.";

const RETAINED: &str = "\
SYMBOL's retained size is its own size plus the sizes of the items that only
it leads to, from where the program starts: what removing it would save. A
row such as \"crc_table, kept by crc32\" names the closest item that every
path to it goes through. The table shows the 30 largest items, or every
item with --all; rows such as \"6 more, kept by crc32\" sum up the rest, and
--tree draws the rows as a tree.

Nothing calls a function marked \"(through a pointer)\" directly, so it counts
for the code or data that holds its address. With --range, a row marked
\"(load image)\" counts the copy of an item that the file stores in the
range for startup code to copy where the program uses it, and one marked
\"(with its load image)\" counts both copies. Sizes marked ~ are estimates.";

#[derive(Debug, Args)]
pub struct TopArgs {
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct RetainedArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct WhyArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
    /// Show the chains through every item that keeps SYMBOL, not only the
    /// 10 nearest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct UsesArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or one or more ranges of memory, START..END or START+SIZE, to list
    /// only what crosses their edge
    // Not required here: commands::check_arguments says what is missing,
    // and with --range, to give the range here instead.
    #[arg(value_name = "SYMBOL|RANGE")]
    pub symbols: Vec<String>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct InlinedArgs {
    /// Show the inlined code in one function instead: its name, the end of
    /// its name, or an address in it
    pub symbol: Option<String>,
    /// Show every function, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// List the functions and data objects elfq found, and where it found each
    Items,
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
