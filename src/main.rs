//! The elfq program: parses arguments, runs one command and reports errors.
//! Everything else lives in the library (src/lib.rs).

use std::io::{self, BufWriter, ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;
use elfq::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = BufWriter::new(io::stdout().lock());
    let result = elfq::commands::run(&cli.command, &mut out).and_then(|()| Ok(out.flush()?));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The reader stopped early, as in `elfq ... | head`. Not an error.
        Err(err) if is_broken_pipe(&err) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == ErrorKind::BrokenPipe)
    })
}
