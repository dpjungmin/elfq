//! The elfq program: parses arguments, runs one command and reports errors.
//! Everything else lives in the library (src/lib.rs).

use std::ffi::OsString;
use std::io::{self, BufWriter, ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;
use elfq::cli::{self, Cli};

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(args()) {
        Ok(cli) => cli,
        Err(err) => {
            // `use_stderr` is false for --help and --version, which aren't errors.
            if err.use_stderr()
                && let Some(hint) = file_first_hint()
            {
                eprintln!("{hint}");
                return ExitCode::from(2);
            }
            err.exit();
        }
    };
    let mut out = BufWriter::new(io::stdout().lock());
    let result = elfq::commands::run(&cli, &mut out).and_then(|()| Ok(out.flush()?));
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

/// The arguments, with `elfq help top` read as `elfq top --help`, since
/// `help` is the first thing many people try, and `elfq help` as
/// `elfq --help`. Anything else after `help`, such as a misspelled command,
/// is left for clap to report.
fn args() -> Vec<OsString> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let commands = cli::command_names();
    let is_command = |arg: &OsString| commands.iter().any(|command| arg == command.as_str());
    if args.get(1).is_some_and(|arg| arg == "help") && args.get(2).is_none_or(is_command) {
        args.remove(1);
        args.insert(args.len().min(2), "--help".into());
    }
    args
}

/// A hint for people used to tools like `git`, who type the command before
/// the file: `elfq callers app.elf log_msg`.
fn file_first_hint() -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first()?;
    if !cli::command_names().contains(command) {
        return None;
    }
    let file = args
        .get(1)
        .filter(|arg| !arg.starts_with('-'))
        .map_or("FILE", String::as_str);
    let rest: Vec<&str> = args[2.min(args.len())..]
        .iter()
        .map(String::as_str)
        .collect();
    let example = format!("elfq {file} {command} {}", rest.join(" "));
    Some(format!(
        "error: the file comes before the command, as in: {}",
        example.trim_end()
    ))
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == ErrorKind::BrokenPipe)
    })
}
