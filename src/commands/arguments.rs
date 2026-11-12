//! The checks on which arguments go together. Most are made before the file
//! is loaded, so that a mistake is reported at once, even for a large file
//! ([`check_before_loading`]). `--range` with `callers` or `callees` waits
//! for the file, since which command to suggest in its place depends on
//! what the file holds ([`check_after_loading`]).
//!
//! Each option that only some commands take has one match with an arm for
//! every command, so that the compiler asks about a new one. An arm that
//! lets an option through to a command that doesn't take it says where the
//! command refuses it.
//!
//! clap refuses an argument that a command doesn't take before elfq sees
//! the others, and [`unexpected_argument_hints`] words what elfq adds under
//! the first line of that error: for the commands that take one SYMBOL at
//! most ([`TAKE_ONE_SYMBOL_AT_MOST`]), that the arguments are files here,
//! which the shell may have put in place of a pattern, and for `top`, which
//! takes none, the command that takes a pattern given to it.

use std::io::Write;

use anyhow::{Result, bail};

use super::checks::{check_code_is_readable, item_in_file};
use super::kept::freeing;
use super::range_check::Direction;
use super::symbol::{FilesHere, Pattern, is_here};
use super::text::{CommandLine, Detail};
use super::{Outcome, callees, callers, symbol, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::{Cli, Command, DebugCommand, Format, ReachArgs, UsesArgs};

/// The error for `--range` with a command that doesn't take it.
const RANGE_REFUSED: &str = "only the overview, top and retained take --range";

/// The error for `--json`, `--tree` or `--range` with `browse`, which shows
/// its pages on the screen, and prints no answer for them to change.
const BROWSE_REFUSED: &str = "browse takes no --json, --tree or --range";

/// The commands that take one SYMBOL at most, and `top`, which takes none,
/// though a pattern may be given to it to see what those items keep. clap
/// refuses an argument after the one that they take before the file is
/// read, so the line that says that the arguments are files here, which
/// the shell may have put in place of a pattern, goes under clap's error
/// (see [`unexpected_argument_hints`]), where `retained`, `callers` and
/// `callees`, which take several, give it under their own. clap refuses an
/// argument after those that `diff`, `startup` and `debug` take too, but
/// none of the three is listed: `diff` and `startup` take no SYMBOL, so a
/// pattern isn't given to them, and of the hidden `debug` commands,
/// `items` and `walk` take none, and `matches`, which takes patterns,
/// takes any number. A unit test checks that every other command that
/// clap refuses one more argument for is listed.
const TAKE_ONE_SYMBOL_AT_MOST: [&str; 5] = ["why", "reach", "inlined", "browse", "top"];

/// The lines that elfq adds under the first line of clap's error for
/// `unexpected`, an argument that `command` doesn't take, given
/// `arguments`, all those after the command, in the commands that take one
/// SYMBOL at most ([`TAKE_ONE_SYMBOL_AT_MOST`]). For `top`, which takes
/// none, first the command that takes the first of `arguments` that is a
/// pattern and not a range, wherever clap's error stops, or failing that,
/// the pattern that the shell may have put files in place of (see
/// [`top_takes_no_symbol`]).
/// Then, when `unexpected` is a file or a folder here, as the shell may
/// have put either in place of a pattern, the line that says which of
/// `arguments` are files here (see [`FilesHere::hint`]). The file isn't
/// read yet, so that line names a file whose name an item has too. None
/// for an unexpected option, such as a misspelled `--json`, nor for the
/// other commands.
pub fn unexpected_argument_hints(
    command: &str,
    arguments: &[String],
    unexpected: &str,
) -> Vec<String> {
    if !TAKE_ONE_SYMBOL_AT_MOST.contains(&command) || unexpected.starts_with('-') {
        return Vec::new();
    }
    let files = is_here(unexpected).then(|| FilesHere::among(arguments));
    let mut lines = Vec::new();
    if command == "top" {
        let pattern = (arguments.iter())
            .find(|argument| !symbol::is_range(argument) && Pattern::parse(argument).is_some())
            .cloned()
            .or_else(|| files.as_ref()?.pattern());
        lines.extend(pattern.map(|pattern| top_takes_no_symbol(&pattern)));
    }
    lines.extend(files.and_then(|files| files.hint()));
    lines
}

/// The line under clap's error for `pattern`, given to `top`, which takes
/// no SYMBOL: the command that lists the items that it matches, "top takes
/// no SYMBOL; retained 'core::fmt::*' lists those items, with what each
/// keeps."
fn top_takes_no_symbol(pattern: &str) -> String {
    format!(
        "top takes no SYMBOL; retained {} lists those items, with what each keeps.",
        text::shell_quote(pattern)
    )
}

/// Refuses arguments that don't go together, before the file is loaded, so
/// that a mistake is reported at once, even for a large file.
pub(super) fn check_before_loading(cli: &Cli) -> Result<()> {
    // Before the others, so that browse gets the same error for each.
    if let Some(Command::Browse(_)) = &cli.command
        && (cli.json || cli.tree || cli.range.is_some())
    {
        bail!("{BROWSE_REFUSED}");
    }
    if cli.tree {
        check_tree(cli)?;
    }
    if let Some(uses) = cli.command.as_ref().and_then(UsesQuery::of) {
        check_uses(cli, &uses)?;
    }
    if let Some(Command::Reach(args)) = &cli.command {
        check_reach(cli, args)?;
    }
    if cli.range.is_some() {
        check_range(cli)?;
    }
    if cli.format() == Format::Json {
        check_json(cli)?;
    }
    Ok(())
}

/// Refuses `--range` with `callers` or `callees`, which
/// [`check_before_loading`] leaves until elfq can tell which command to
/// suggest in its place.
pub(super) fn check_after_loading(cli: &Cli, binary: &Binary) -> Result<()> {
    if let Some(range) = cli.range
        && let Some(uses) = cli.command.as_ref().and_then(UsesQuery::of)
    {
        refuse_range(cli, binary, &uses, range)?;
    }
    Ok(())
}

/// Refuses `--tree` where there is no tree to draw: with `--json`, and for
/// commands that print a list.
fn check_tree(cli: &Cli) -> Result<()> {
    if cli.json {
        bail!("--tree draws trees in the text, so it doesn't go with --json");
    }
    match &cli.command {
        None | Some(Command::Top(_) | Command::Retained(_) | Command::Why(_)) => Ok(()),
        // Without SYMBOL, reach refuses it once the file is loaded (see
        // `reach::run_starts`), so that the command to type instead can
        // name a function in the file.
        Some(Command::Reach(_)) => Ok(()),
        Some(Command::Inlined(args)) if args.symbol.is_some() => Ok(()),
        Some(Command::Browse(_)) => bail!("{BROWSE_REFUSED}"),
        Some(
            Command::Inlined(_)
            | Command::Callers(_)
            | Command::Callees(_)
            | Command::Diff(_)
            | Command::Startup(_)
            | Command::Debug(_),
        ) => {
            bail!(
                "only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree"
            )
        }
    }
}

/// Refuses `--range` for the commands that don't take it.
fn check_range(cli: &Cli) -> Result<()> {
    match &cli.command {
        None | Some(Command::Top(_) | Command::Retained(_)) => Ok(()),
        // Refused once the file is loaded (see `check_after_loading`).
        Some(Command::Callers(_) | Command::Callees(_)) => Ok(()),
        // With SYMBOL, `check_reach` has refused it, naming `--to`. Without,
        // reach refuses it once the file is loaded (see
        // `reach::run_starts`), so that the command to type instead can
        // name a function in the file.
        Some(Command::Reach(_)) => Ok(()),
        Some(Command::Browse(_)) => bail!("{BROWSE_REFUSED}"),
        Some(
            Command::Why(_)
            | Command::Inlined(_)
            | Command::Diff(_)
            | Command::Startup(_)
            | Command::Debug(_),
        ) => bail!("{RANGE_REFUSED}"),
    }
}

/// Refuses `--json` for the commands that print only text.
fn check_json(cli: &Cli) -> Result<()> {
    match &cli.command {
        None
        | Some(
            Command::Top(_)
            | Command::Retained(_)
            | Command::Why(_)
            | Command::Inlined(_)
            | Command::Callers(_)
            | Command::Callees(_)
            | Command::Reach(_)
            | Command::Diff(_)
            | Command::Startup(_),
        ) => Ok(()),
        Some(Command::Browse(_)) => bail!("{BROWSE_REFUSED}"),
        // It prints JSON lines, with `--json` or without.
        Some(Command::Debug(DebugCommand::Matches(_))) => Ok(()),
        // The message is older than `debug matches`, and stays as it was so
        // that the output of these two doesn't change.
        Some(Command::Debug(DebugCommand::Items | DebugCommand::Walk)) => {
            bail!("the debug commands print only text")
        }
    }
}

/// The `run` of `callers` and `callees`, which takes the file, SYMBOL or
/// what is given in its place, the ranges of `--from` or `--to`, how much
/// to show, the format and the output.
type UsesRun =
    fn(&Binary, &[String], &[AddressRange], Detail, Format, &mut dyn Write) -> Result<Outcome>;

/// What `callers` or `callees` was given.
struct UsesQuery<'a> {
    /// Which command it is: `callers` asks what crosses into SYMBOL or the
    /// ranges, `callees` what crosses out.
    direction: Direction,
    /// The command's `run`.
    run: UsesRun,
    /// How the command takes a SYMBOL among others.
    item_in_set: symbol::ItemInSet,
    args: &'a UsesArgs,
    /// The ranges of `--from` or `--to`.
    far_side: &'a [AddressRange],
    /// Whether the other command's option was given by mistake: `--to` for
    /// `callers`.
    misplaced: bool,
}

impl UsesQuery<'_> {
    /// What `command` was given, when it is `callers` or `callees`.
    fn of(command: &Command) -> Option<UsesQuery<'_>> {
        match command {
            Command::Callers(args) => Some(UsesQuery {
                direction: Direction::In,
                run: callers::run,
                item_in_set: callers::item_in_set,
                args: &args.uses,
                far_side: &args.from,
                misplaced: !args.misplaced_to.is_empty(),
            }),
            Command::Callees(args) => Some(UsesQuery {
                direction: Direction::Out,
                run: callees::run,
                item_in_set: item_in_file,
                args: &args.uses,
                far_side: &args.to,
                misplaced: !args.misplaced_from.is_empty(),
            }),
            _ => None,
        }
    }
}

/// Refuses what `callers` or `callees` doesn't take: the other command's
/// option, and no SYMBOL.
fn check_uses(cli: &Cli, uses: &UsesQuery) -> Result<()> {
    let name = uses.direction.command();
    if uses.misplaced {
        let other = uses.direction.other();
        bail!(
            "{} goes with {}; {name} takes {} to list only the {name} in a range",
            other.option(),
            other.command(),
            uses.direction.option()
        )
    }
    if cli.range.is_some() {
        // Which command can take its place depends on what the file holds:
        // see `refuse_range`.
        return Ok(());
    }
    if uses.args.symbols.is_empty() {
        bail!("{name} needs a SYMBOL, or patterns or ranges, such as 'uart_*' or 0x0+256K")
    }
    Ok(())
}

/// Refuses what `reach` doesn't take: `--from`, ranges in place of SYMBOL,
/// and `--range`, each with what to type instead. Without SYMBOL, `--to`,
/// `--range` and `--tree` wait for the file to load, so that the command to
/// type instead can name a function in it (see `reach::run_starts`).
fn check_reach(cli: &Cli, args: &ReachArgs) -> Result<()> {
    if !args.misplaced_from.is_empty() {
        bail!("--from goes with callers; reach takes --to to list only what it reaches in a range")
    }
    let Some(query) = &args.symbol else {
        return Ok(());
    };
    if symbol::is_range(query) {
        // What a range uses outside it, or in other ranges, is a range
        // check, which callees has.
        let callees = match query.parse::<AddressRange>() {
            Ok(range) => {
                let what = if args.to.is_empty() {
                    "what a range uses outside it"
                } else {
                    "what a range uses in the --to ranges"
                };
                let command = CommandLine::new(&cli.file, "callees")
                    .ranges("", [range])
                    .ranges("--to", args.to.iter().copied())
                    .flag("--json", cli.json);
                format!("; for {what}: {command}")
            }
            Err(_) => String::new(),
        };
        bail!("reach takes one SYMBOL, not ranges{callees}")
    }
    if let Some(range) = cli.range {
        let command = CommandLine::new(&cli.file, "reach")
            .word(query)
            .ranges("--to", args.to.iter().copied().chain([range]))
            .flag("--all", args.all)
            .flag("--tree", cli.tree)
            .flag("--json", cli.json);
        bail!("reach takes --to, not --range, to list only what it reaches in a range: {command}")
    }
    Ok(())
}

/// Fails with the error for `--range` with `callers` or `callees` (see
/// [`range_option_error`]), suggesting only the commands that answer for
/// `binary`: elfq runs them to find out, which costs time only on this
/// mistake. When none does, and the command fails without `--range` too,
/// that error comes first, since no form of the command answers until it
/// is fixed.
fn refuse_range(cli: &Cli, binary: &Binary, uses: &UsesQuery, range: AddressRange) -> Result<()> {
    check_code_is_readable(binary)?;
    let symbols = &uses.args.symbols;
    let ranges = symbols.iter().all(|query| symbol::is_range(query));
    if !ranges {
        // A mistyped name is the first thing to fix.
        freeing(|| symbol::subject(binary, symbols, uses.item_in_set))?;
    }
    let answer = |symbols: &[String], far_side: &[AddressRange]| {
        let sink = &mut std::io::sink();
        freeing(|| (uses.run)(binary, symbols, far_side, Detail::Brief, Format::Json, sink))
    };
    let added = ranges
        && answer(
            &[&symbols[..], &[range.to_string()]].concat(),
            uses.far_side,
        )
        .is_ok();
    let limited =
        !symbols.is_empty() && answer(symbols, &[uses.far_side, &[range]].concat()).is_ok();
    let answers = match (added, limited) {
        (false, false) => {
            if !symbols.is_empty() {
                answer(symbols, uses.far_side)?;
            }
            bail!("{RANGE_REFUSED}");
        }
        (false, true) => InPlaceOfRange::Limited,
        (true, false) => InPlaceOfRange::Added,
        (true, true) => InPlaceOfRange::Both,
    };
    bail!("{}", range_option_error(cli, uses, range, answers))
}

/// The forms of `callers` or `callees` that answer in place of one with
/// `--range`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InPlaceOfRange {
    /// The one that lists only the uses in the range, with `--from` or
    /// `--to`.
    Limited,
    /// The one that adds the range to the ranges in place of SYMBOL.
    Added,
    /// Both.
    Both,
}

/// The error for `--range` with `callers` or `callees`, which don't take
/// it, with the commands to type that `answers` names, the forms that
/// answer: with ranges in place of SYMBOL, or none, the one that adds
/// `range` to them, and the one that lists only the uses in `range`, with
/// `--from` or `--to`. Keeps what `uses` has of those.
fn range_option_error(
    cli: &Cli,
    uses: &UsesQuery,
    range: AddressRange,
    answers: InPlaceOfRange,
) -> String {
    let name = uses.direction.command();
    let option = uses.direction.option();
    let symbols = &uses.args.symbols;
    let kept = uses.far_side.iter().copied();
    let limited_command = CommandLine::new(&cli.file, name)
        .words(symbols)
        .ranges(option, kept.clone().chain([range]))
        .flag("--json", cli.json);
    // Only ranges in place of SYMBOL, or none, can have `range` added.
    if answers == InPlaceOfRange::Limited {
        return format!(
            "{name} takes {option}, not --range, to list only the {name} in a range: \
             {limited_command}"
        );
    }
    let added_command = CommandLine::new(&cli.file, name)
        .words(symbols)
        .ranges("", [range])
        .ranges(option, kept)
        .flag("--json", cli.json);
    let mut error =
        format!("{name} takes ranges in place of SYMBOL, not with --range: {added_command}");
    // With `--from` or `--to` given, the command that adds `range` to them
    // lists the uses in theirs too.
    if answers == InPlaceOfRange::Both && !symbols.is_empty() && uses.far_side.is_empty() {
        let range = text::range_argument(range);
        error += &format!("; to list only the {name} in {range}: {limited_command}");
    }
    error
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use clap::error::ErrorKind;

    use crate::cli;

    use super::*;

    /// What [`unexpected_argument_hints`] adds for these arguments.
    fn hints(command: &str, arguments: &[&str], unexpected: &str) -> Vec<String> {
        let arguments: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        unexpected_argument_hints(command, &arguments, unexpected)
    }

    #[test]
    fn the_commands_listed_refuse_a_second_symbol_before_the_file_is_read() {
        for command in TAKE_ONE_SYMBOL_AT_MOST {
            assert!(
                cli::command_names().contains(&command.to_string()),
                "{command}"
            );
            let error = Cli::try_parse_from(["elfq", "app.elf", command, "a", "b"]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{command}");
        }
    }

    /// Arguments that `command` takes, so that one more is one too many for
    /// a command that takes one SYMBOL at most. A new command fails the
    /// test below until it has an arm here.
    fn sample_arguments(command: &str) -> &'static [&'static str] {
        match command {
            "top" | "startup" => &[],
            "retained" | "why" | "inlined" | "callers" | "callees" | "reach" | "browse" => &["a"],
            "diff" => &["new.elf"],
            "debug" => &["items"],
            _ => panic!("give {command} sample arguments"),
        }
    }

    #[test]
    fn every_command_that_takes_one_symbol_at_most_is_listed() {
        // Those that clap refuses one more argument for too, but that take
        // no SYMBOL (see TAKE_ONE_SYMBOL_AT_MOST).
        let take_no_symbol = ["diff", "startup", "debug"];
        for command in cli::command_names() {
            let command = command.as_str();
            let arguments = [
                &["elfq", "app.elf", command],
                sample_arguments(command),
                &["b"],
            ];
            let refused = match Cli::try_parse_from(arguments.concat()) {
                Ok(_) => false,
                Err(error) => {
                    assert_eq!(
                        error.kind(),
                        ErrorKind::UnknownArgument,
                        "{command}: clap refused its sample arguments for another reason: \
                         fix its arm in sample_arguments"
                    );
                    true
                }
            };
            let listed =
                TAKE_ONE_SYMBOL_AT_MOST.contains(&command) || take_no_symbol.contains(&command);
            assert!(
                listed || !refused,
                "{command}: clap refuses an argument after those it takes: add it to \
                 TAKE_ONE_SYMBOL_AT_MOST, or if it takes no SYMBOL, to take_no_symbol, and say \
                 why in the constant's doc"
            );
            assert!(
                refused || !listed,
                "{command} takes another argument: take it out of the list it is in"
            );
        }
    }

    #[test]
    fn top_names_the_command_that_takes_the_first_pattern_given() {
        let takes = |pattern: &str| {
            vec![format!(
                "top takes no SYMBOL; retained '{pattern}' lists those items, with what each \
                 keeps."
            )]
        };
        assert_eq!(
            hints("top", &["core::fmt::*"], "core::fmt::*"),
            takes("core::fmt::*")
        );
        // Wherever clap's error stops.
        assert_eq!(
            hints("top", &["--json", "x", "on_*", "uart_*"], "x"),
            takes("on_*")
        );
        // Not a name, nor one whose only `*` is a pointer's, nor any after
        // an unexpected option, which is more likely misspelled.
        assert_eq!(hints("top", &["main"], "main"), Vec::<String>::new());
        assert_eq!(
            hints("top", &["f(char*)"], "f(char*)"),
            Vec::<String>::new()
        );
        assert_eq!(
            hints("top", &["--jsn", "on_*"], "--jsn"),
            Vec::<String>::new()
        );
        // Nor a range, which SYMBOL reads as one.
        assert_eq!(
            hints("top", &["0x8000000+*"], "0x8000000+*"),
            Vec::<String>::new()
        );
        // Only top names a command: why takes a pattern in place of
        // SYMBOL, and diff takes none.
        assert_eq!(hints("why", &["x", "on_*"], "on_*"), Vec::<String>::new());
        assert_eq!(
            hints("diff", &["new.elf", "on_*"], "on_*"),
            Vec::<String>::new()
        );
    }
}
