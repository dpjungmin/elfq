//! Command-line arguments. Each subcommand has a variant here and a module in
//! `commands/`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "elfq", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show elfq's own view of a binary (for development)
    #[command(subcommand)]
    Debug(DebugCommand),
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// List the functions and data objects elfq found, and where it found each
    Items(ItemsArgs),
}

#[derive(Debug, Args)]
pub struct ItemsArgs {
    /// The ELF file to inspect
    pub file: PathBuf,
}
