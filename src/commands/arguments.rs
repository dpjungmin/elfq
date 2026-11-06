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

use std::io::Write;

use anyhow::{Result, bail};

use super::checks::check_code_is_readable;
use super::kept::freeing;
use super::range_check::Direction;
use super::text::CommandLine;
use super::{Outcome, callees, callers, symbol, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::{Cli, Command, DebugCommand, Format, ReachArgs, UsesArgs};

/// The error for `--range` with a command that doesn't take it.
const RANGE_REFUSED: &str = "only the overview, top and retained take --range";

/// The error for `--json`, `--tree` or `--range` with `browse`, which shows
/// its pages on the screen, and prints no answer for them to change.
const BROWSE_REFUSED: &str = "browse takes no --json, --tree or --range";

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
/// ranges, the ranges of `--from` or `--to`, whether to list all, the format
/// and the output.
type UsesRun =
    fn(&Binary, &[String], &[AddressRange], bool, Format, &mut dyn Write) -> Result<Outcome>;

/// What `callers` or `callees` was given.
struct UsesQuery<'a> {
    /// Which command it is: `callers` asks what crosses into SYMBOL or the
    /// ranges, `callees` what crosses out.
    direction: Direction,
    /// The command's `run`.
    run: UsesRun,
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
                args: &args.uses,
                far_side: &args.from,
                misplaced: !args.misplaced_to.is_empty(),
            }),
            Command::Callees(args) => Some(UsesQuery {
                direction: Direction::Out,
                run: callees::run,
                args: &args.uses,
                far_side: &args.to,
                misplaced: !args.misplaced_from.is_empty(),
            }),
            _ => None,
        }
    }
}

/// Refuses what `callers` or `callees` doesn't take: the other command's
/// option, no SYMBOL, and with `--range`, ranges mixed with a SYMBOL.
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
    let symbols = &uses.args.symbols;
    if cli.range.is_some() {
        if symbols.len() > 1
            && let Some(query) = symbols.iter().find(|query| !symbol::is_range(query))
        {
            bail!("`{query}` isn't a range; give one SYMBOL, or ranges such as 0x0+256K")
        }
        // Which command can take its place depends on what the file holds:
        // see `refuse_range`.
        return Ok(());
    }
    if symbols.is_empty() {
        bail!("{name} needs a SYMBOL, or ranges such as 0x0+256K")
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
        freeing(|| symbol::subject(binary, symbols))?;
    }
    let answer = |symbols: &[String], far_side: &[AddressRange]| {
        let sink = &mut std::io::sink();
        freeing(|| (uses.run)(binary, symbols, far_side, false, Format::Json, sink))
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
