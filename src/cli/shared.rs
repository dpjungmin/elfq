//! The shared options, `--json`, `--tree` and `--range`, as each command
//! declares them, in the `args` of its variant of `Command`, so that its
//! help lists only those that it takes, each worded for it.
//!
//! `Cli` holds the three, for the overview and for options given before a
//! command's name. They are global so that, once clap has read the
//! arguments, it copies the value of a command's own option with the same
//! id (`Arg::new`'s, which is the field's name on `Cli`) into `Cli`'s, and
//! so that the debug commands, which declare none, get a copy: clap gives
//! a command `Cli`'s copy only when it declares no option with that id. So
//! every other command declares all three: those that it takes as
//! [`json`], [`tree`] and [`range`] build them, and those that it refuses
//! through [`refused`]. The overview's help for `--json`, and the start of
//! its help for `--range`, are the commands' too: [`JSON_HELP`] and
//! [`RANGE_HELP`].
//!
//! `arguments.rs` decides which commands take which option, but for
//! `reach` without SYMBOL, which refuses `--tree` and `--range` once the
//! file is loaded (`reach::run_starts`), as its arms there say. The test
//! `each_command_s_help_lists_the_shared_options_that_it_takes`, in
//! `tests/it/across_commands/errors_and_help.rs`, checks each command's
//! help against what the command does with each option.
//!
//! A new shared option takes a field on `Cli`, with `global = true`; a
//! builder here; an entry in every variant's `args`, taken or refused; a
//! match in `arguments.rs`; and an entry in that test's `SHARED_OPTIONS`.

use clap::{Arg, ArgAction};

use super::one_range;

/// What `--json` does, as the overview and each command that takes it say.
pub(super) const JSON_HELP: &str = "Print the answer as JSON, for scripts";

/// What `--range` does, as the commands that take it say. The overview's
/// help adds which commands take it.
pub(super) const RANGE_HELP: &str = "Count only the bytes in one range of memory, such as \
                                     ITCM: START..END or START+SIZE, like 0x20000000+256K. \
                                     When the file's sections and load images occupy several \
                                     ranges, as in firmware, the overview lists them";

/// `--json`, as the commands that take it list it.
pub(super) fn json() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help(JSON_HELP)
}

/// `--tree`. A command that takes it gives it a help line that says what
/// it draws, as `tree().help("Draw the chains as a tree")`.
pub(super) fn tree() -> Arg {
    Arg::new("tree").long("tree").action(ArgAction::SetTrue)
}

/// `--range`, as the commands that take it list it.
pub(super) fn range() -> Arg {
    Arg::new("range")
        .long("range")
        .value_name("RANGE")
        .value_parser(one_range)
        .help(RANGE_HELP)
}

/// `option`, for a command that refuses it: elfq reads it, so that
/// `commands/arguments.rs` refuses it with its own error, which says which
/// commands take it or what to type instead, but the command's help leaves
/// it out.
pub(super) fn refused(option: Arg) -> Arg {
    option.hide(true)
}

#[cfg(test)]
mod tests {
    use clap::{Arg, Command, CommandFactory};

    use crate::cli::Cli;

    /// The ids of the shared options: `Cli`'s global ones.
    fn shared_ids(cli: &Command) -> Vec<&str> {
        cli.get_arguments()
            .filter(|option| option.is_global_set())
            .map(|option| option.get_id().as_str())
            .collect()
    }

    /// How elfq reads an option: all that the overview's option and a
    /// command's own must agree on, so that it means the same before the
    /// command's name and after it, with the value's name only for an
    /// option that takes a value. Their help may differ, and so may
    /// whether it is hidden.
    fn how_read(option: &Arg) -> String {
        let action = option.get_action();
        format!(
            "{:?} {action:?} {:?} {:?} {:?}",
            option.get_long(),
            action.takes_values().then(|| option.get_value_names()),
            option.get_num_args(),
            option.get_value_parser().type_id()
        )
    }

    #[test]
    fn every_command_declares_the_shared_options() {
        // Not built, so that the commands don't yet have `Cli`'s copies.
        let cli = Cli::command();
        let ids = shared_ids(&cli);
        // The debug commands declare none (see `Command::Debug`).
        for command in cli
            .get_subcommands()
            .filter(|command| command.get_name() != "debug")
        {
            for id in &ids {
                assert!(
                    command.get_arguments().any(|option| option.get_id() == *id),
                    "{} doesn't declare --{id}: give it {id}() or refused({id}()) in its \
                     variant's args, in cli/mod.rs",
                    command.get_name()
                );
            }
        }
    }

    #[test]
    fn every_command_reads_the_shared_options_as_the_overview_does() {
        let mut cli = Cli::command();
        cli.build();
        for id in shared_ids(&cli) {
            let find = |command: &Command| {
                let found = command.get_arguments().find(|arg| arg.get_id() == id);
                how_read(found.unwrap_or_else(|| panic!("{}: no {id}", command.get_name())))
            };
            let overview = find(&cli);
            for command in cli.get_subcommands() {
                assert_eq!(find(command), overview, "{} --{id}", command.get_name());
            }
        }
    }
}
