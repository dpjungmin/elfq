//! Command-line arguments. Each command has a variant here and a module in
//! `commands/`.
//!
//! The file comes first and the question after it, so that a command sits
//! next to the symbol it asks about: `elfq app.elf callers log_msg`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
}

const EXAMPLES: &str = "\
Examples:
  elfq app.elf                    Overview of app.elf
  elfq app.elf callers log_msg    What calls or uses log_msg
  elfq app.elf callees main       What main calls or uses";

#[derive(Debug, Subcommand)]
pub enum Command {
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
  pointer        data holds a pointer to it, as in a table of functions";

#[derive(Debug, Args)]
pub struct SymbolArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
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
