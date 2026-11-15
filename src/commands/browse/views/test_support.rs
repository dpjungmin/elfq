//! What the tests of browse's tabs and pages share, built only for tests.
//!
//! Each tab's test checks, on every page of every sample, that the command
//! under the tab prints what the tab shows. `each_session` gives it each
//! sample, opened as browse opens it. `tab_command` reads the command under
//! the tab as a shell and elfq read it, and checks that it names the file
//! and that its SYMBOL finds the page's item; the test's own match on its
//! `Command` gives that SYMBOL. `assert_has_all_when_needed` checks that
//! the command has `--all` exactly when it needs it. `screen_lines` reads
//! a screen that a test draws.
//!
//! `shell_words`, which `tab_command` uses, splits a command line as a
//! shell does; a test of a command that isn't elfq's, as contents.rs's test
//! of the objdump command under a data object, calls it itself. The
//! integration tests have a fuller `shell_words` in tests/it/support, which
//! a unit test can't use: it is built into their own program.

use std::path::Path;

use clap::Parser;
use ratatui::buffer::Buffer;

use super::printed::Printed;
use crate::binary::Binary;
use crate::binary::test_samples::samples;
use crate::cli::{Cli, Command};
use crate::commands::browse::session::Session;
use crate::commands::symbol::Symbol;
use crate::graph::Node;

/// Runs `check` on each sample, with its path, the file loaded from it, and
/// a new session that browses the file. It checks first that the session's
/// path is the sample's, so that `tab_command`, which checks a command's
/// file against the session's path, checks it against the path the test
/// opened.
pub(in crate::commands::browse) fn each_session(
    mut check: impl for<'a> FnMut(&Path, &'a Binary, Session<'a>),
) {
    for path in samples() {
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        assert_eq!(session.path, path);
        check(&path, &binary, session);
    }
}

/// The command under a tab or page, as a test reads it.
pub(super) struct TabCommand {
    /// The command line, without the `$ ` before it.
    pub(super) line: String,
    /// The words that a shell splits it into.
    pub(super) words: Vec<String>,
    /// The words, parsed as elfq's arguments.
    pub(super) cli: Cli,
    /// The item or import that the command's SYMBOL finds: Some exactly
    /// when `tab_command` is given a node, and None under the start page,
    /// whose command gives no SYMBOL.
    pub(super) symbol: Option<Symbol>,
}

/// The command under `printed`, a tab or page of the file that `session`
/// browses, once it is checked: a shell splits it into words that parse as
/// elfq's arguments, for that file.
///
/// `query_of` is the test's own match on the `Command` of its tab: it
/// gives the command's query, what it gives as SYMBOL, or None where it
/// gives none or is another command. With `node`, the query must find that
/// node; without, as under the start page, there must be none. A failed
/// check points at the test's own line.
#[track_caller]
pub(super) fn tab_command(
    session: &Session,
    printed: &Printed,
    node: Option<Node>,
    query_of: impl FnOnce(&Command) -> Option<&str>,
) -> TabCommand {
    let line = printed.command().strip_prefix("$ ").unwrap().to_string();
    let words = shell_words(&line);
    assert_eq!(words[0], "elfq", "{line}");
    // A match, since a panic in a closure, as in `unwrap_or_else`, would
    // point here rather than at the test.
    let cli = match Cli::try_parse_from(&words) {
        Ok(cli) => cli,
        Err(error) => panic!("{line}: {error}"),
    };
    assert_eq!(cli.file, session.path, "{line}");
    let symbol = match (node, cli.command.as_ref().and_then(query_of)) {
        (Some(node), Some(query)) => {
            let symbol = session.names.find(query).unwrap();
            assert_eq!(symbol.node, node, "{line}");
            Some(symbol)
        }
        (None, None) => None,
        (Some(_), None) => panic!("{line} isn't the tab's command, or has no one SYMBOL"),
        (None, Some(query)) => panic!("{line} gives {query} as SYMBOL"),
    };
    TabCommand {
        line,
        words,
        cli,
        symbol,
    }
}

/// Checks that `line`, the command under a tab or page, has `--all` exactly
/// when it needs it to print all that the tab shows: when what it prints
/// with `--all`, `every`, isn't what it prints without, `brief`. A failure
/// points at the test's own line.
#[track_caller]
pub(super) fn assert_has_all_when_needed<T: PartialEq>(line: &str, every: &T, brief: &T) {
    assert_eq!(line.ends_with(" --all"), every != brief, "{line}");
}

/// The lines of `buffer`, a screen drawn on ratatui's `TestBackend`,
/// without the spaces at their ends.
pub(in crate::commands::browse) fn screen_lines(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;
    (0..area.height)
        .map(|row| {
            let line: String = (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect();
            line.trim_end().to_string()
        })
        .collect()
}

/// `line` split into words as a shell splits it, for the words that
/// `text::shell_quote` writes: plain, or in single quotes, with `'\''`
/// for a quote.
pub(super) fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    words
}

mod tests {
    use super::*;
    use crate::commands::text;

    #[test]
    fn shell_words_undo_shell_quote() {
        for text in [
            "log_msg",
            "log(unsigned int)",
            "vtable for Gain",
            "<&'a T>",
            "",
        ] {
            let line = format!("elfq {} --all", text::shell_quote(text));
            assert_eq!(shell_words(&line), ["elfq", text, "--all"], "{line}");
        }
    }
}
