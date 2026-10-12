//! One module per subcommand. A command loads the binary, asks the library a
//! question and renders the answer; the analysis itself never lives here.

mod debug_items;

use std::io::Write;

use anyhow::Result;

use crate::cli::{Command, DebugCommand};

/// Runs `command`, writing its answer to `out`.
pub fn run(command: &Command, out: &mut dyn Write) -> Result<()> {
    match command {
        Command::Debug(DebugCommand::Items(args)) => debug_items::run(args, out),
    }
}
