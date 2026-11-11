//! The elfq program: parses arguments, runs one command on a thread with a
//! large stack, and reports errors. Everything else lives in the library
//! (src/lib.rs).

use std::ffi::OsString;
use std::io::{self, BufWriter, ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;
use clap::builder::StyledStr;
use clap::error::{ContextKind, ContextValue, ErrorFormatter};
use elfq::STACK_SIZE;
use elfq::cli::{self, Cli};
use elfq::commands::{Outcome, unexpected_argument_hints};

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
                report(&hint);
                return ExitCode::from(2);
            }
            exit_with_clap_error(err);
        }
    };
    let mut out = BufWriter::new(Stdout::default());
    // elfq ends once the command has answered.
    elfq::commands::end_after_answer();
    let result = elfq::commands::run(&cli, &mut out).and_then(|outcome| {
        out.flush()?;
        Ok(outcome)
    });
    // As with diff(1): 0 for an answer, 1 for one that a script may want to
    // act on, and 2 for an error, as for clap's errors in the arguments.
    match result {
        Ok(Outcome::Answered) => ExitCode::SUCCESS,
        Ok(Outcome::Crossed | Outcome::Differs | Outcome::Problems) => ExitCode::from(1),
        Err(err) => {
            report(&format!("error: {err:#}"));
            ExitCode::from(2)
        }
    }
}

/// Writes `message` to standard error, as `eprintln!` does, but goes on
/// when nothing reads it any more, as after `2>&1 | head -1`, where
/// `eprintln!` would panic, and elfq would exit with 101 instead of 2.
fn report(message: &str) {
    let _ = writeln!(io::stderr(), "{message}");
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

/// The arguments after the program's name, as text. Lossy, since an
/// argument may not be UTF-8 (`std::env::args()` would panic on one), and
/// the hints only show the arguments or look for files with their names.
fn args_as_text() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// A hint for people used to tools like `git`, who type the command before
/// the file: `elfq callers app.elf log_msg`.
fn file_first_hint() -> Option<String> {
    let args = args_as_text();
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

/// Prints clap's error `err` and exits, as `err.exit()` does, with the
/// lines that elfq adds under its first line, when it has some (see
/// [`with_hints`]).
fn exit_with_clap_error(mut err: clap::Error) -> ! {
    let Some(text) = with_hints(&err) else {
        err.exit();
    };
    err.insert(ContextKind::Custom, ContextValue::StyledStr(text));
    err.apply::<Rendered>().exit()
}

/// The text of clap's error `err`, with the lines that elfq adds under its
/// first line for an argument after the command that `why`, `reach`,
/// `inlined`, `browse` or `top` doesn't take, in clap's "unexpected
/// argument" error alone (see [`unexpected_argument_hints`]): "error:
/// unexpected argument 'uart_hal.c' found", then "uart_driver.c and
/// uart_hal.c are files here: ...". The text keeps clap's colours, which
/// clap takes out where it wouldn't colour its own errors. `None` when elfq
/// adds nothing.
fn with_hints(err: &clap::Error) -> Option<StyledStr> {
    if err.kind() != clap::error::ErrorKind::UnknownArgument {
        return None;
    }
    let Some(ContextValue::String(unexpected)) = err.get(ContextKind::InvalidArg) else {
        return None;
    };
    // The command comes after the file: `file_first_hint` has answered
    // when it comes first.
    let args = args_as_text();
    let command_names = cli::command_names();
    let command = args.iter().position(|arg| command_names.contains(arg))?;
    // clap refused an argument before the command, such as a second file,
    // and its error is the top level's.
    if args[..command].contains(unexpected) {
        return None;
    }
    let lines = unexpected_argument_hints(&args[command], &args[command + 1..], unexpected);
    if lines.is_empty() {
        return None;
    }
    let text = err.render().ansi().to_string();
    let (first_line, rest) = text.split_once('\n')?;
    Some(StyledStr::from(format!(
        "{first_line}\n{}\n{rest}",
        lines.join("\n")
    )))
}

/// Formats a clap error whose text [`with_hints`] has already worked out,
/// which it holds as its [`ContextKind::Custom`], so that clap prints that
/// text as it prints its own errors: to standard error, in colour only
/// where it would colour its own.
struct Rendered;

impl ErrorFormatter for Rendered {
    fn format_error(error: &clap::error::Error<Rendered>) -> StyledStr {
        match error.get(ContextKind::Custom) {
            Some(ContextValue::StyledStr(text)) => text.clone(),
            // `exit_with_clap_error` always gives it one.
            _ => StyledStr::new(),
        }
    }
}
