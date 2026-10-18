//! The elfq program: parses arguments, runs one command on a thread with a
//! large stack, and reports errors. Everything else lives in the library
//! (src/lib.rs).

use std::ffi::OsString;
use std::io::{self, BufWriter, ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;
use elfq::cli::{self, Cli};
use elfq::commands::Outcome;

/// The stack for the thread that runs elfq. Trees are built, drawn and
/// written as JSON recursively, as deep as the file nests them: the retained
/// tree as deep as the longest chain of calls, and the tree of inlined
/// copies as deep as the copies nest. The main thread's stack, usually
/// 8 MiB, runs out at a chain of some 10,000 calls, and of fewer in a debug
/// build; this one holds 32 times as many. The thread's memory grows only as
/// far as its stack is used, but all of its addresses are set aside from the
/// start.
const STACK_SIZE: usize = 256 << 20;

fn main() -> ExitCode {
    // Under a limit on address space, as `ulimit -v` sets, the stack's
    // addresses count in full, and would leave that much less for the
    // file's data.
    if address_space_is_limited() {
        return run();
    }
    let thread = std::thread::Builder::new()
        .name("elfq".to_string())
        .stack_size(STACK_SIZE)
        .spawn(run);
    match thread {
        Ok(thread) => thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        // When no thread can start, try with the main thread's stack.
        Err(_) => run(),
    }
}

/// Whether a limit on address space is set, as `ulimit -v` sets it. Linux
/// lists it in /proc/self/limits; elsewhere, elfq takes it that there is
/// none.
fn address_space_is_limited() -> bool {
    let Ok(limits) = std::fs::read_to_string("/proc/self/limits") else {
        return false;
    };
    // As in "Max address space  unlimited  unlimited  bytes", where the
    // first value is the limit that applies.
    limits.lines().any(|line| {
        line.strip_prefix("Max address space")
            .and_then(|values| values.split_whitespace().next())
            .is_some_and(|limit| limit != "unlimited")
    })
}

/// Parses the arguments, runs the command and returns the exit status.
fn run() -> ExitCode {
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
    let mut out = BufWriter::new(Stdout::default());
    let result = elfq::commands::run(&cli, &mut out).and_then(|outcome| {
        out.flush()?;
        Ok(outcome)
    });
    // As with diff: 0 for an answer, 1 for one that a script may want to
    // act on, and 2 for an error, as for clap's errors in the arguments.
    match result {
        Ok(Outcome::Answered) => ExitCode::SUCCESS,
        Ok(Outcome::Crossed) => ExitCode::from(1),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// Standard output, which drops what is written to it once the reader has
/// stopped reading, as `head` does after its first lines. Then the command
/// still finishes, and elfq's exit status still says what the answer was.
#[derive(Default)]
struct Stdout {
    closed: bool,
}

impl Write for Stdout {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.closed {
            return Ok(bytes.len());
        }
        match io::stdout().write(bytes) {
            Err(err) if err.kind() == ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(bytes.len())
            }
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match io::stdout().flush() {
            Err(err) if err.kind() == ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(())
            }
            result => result,
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
    // Lossy, since an argument may not be UTF-8 (`std::env::args()` would
    // panic on one), and the hint only shows the arguments.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
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
