//! The start page, which browse opens when it is given no SYMBOL.
//!
//! It shows what the file is and what elfq found in it, with the blocks
//! of memory that its sections occupy, as the overview says them, then
//! where the program starts and the bytes that each start reaches, as
//! `elfq FILE reach` lists them, from the same code (`reach::starts`). It
//! lists every start, and under them is that command, ready to paste,
//! with `--all` when the command needs it to list them all. Enter opens
//! the selected start's page. `printed.rs` draws it.
//!
//! In a large program, working out where it starts takes a while.
//! Without SYMBOL, browse works it out before it takes the screen. When
//! SYMBOL finds several items or none, the go-to box opens over it, and
//! it is worked out the first time it is on the screen without the box,
//! after a draw that says so (see `App::pending`).

use ratatui::Frame;
use ratatui::layout::Rect;

use super::printed::{Lines, Printed, table_lines};
use super::{Cursor, Go};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::{overview, reach};

/// The page that lists where the program starts.
pub(in crate::commands::browse) struct StartPage {
    /// What the page shows, or `None` until it is worked out.
    printed: Option<Box<Printed>>,
}

impl StartPage {
    /// The start page, before it is worked out.
    pub(in crate::commands::browse) fn new() -> StartPage {
        StartPage { printed: None }
    }

    /// Whether it is worked out.
    pub(in crate::commands::browse) fn is_worked_out(&self) -> bool {
        self.printed.is_some()
    }

    /// Works it out, if it isn't yet, for the file that `session` browses.
    pub(super) fn work_out(&mut self, session: &Session) {
        if self.printed.is_none() {
            self.printed = Some(Box::new(page(session)));
        }
    }

    /// Draws what it shows, or nothing until that is worked out.
    ///
    /// Until then, the status line says what is being worked out.
    pub(super) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(printed) = &mut self.printed {
            printed.draw(frame, area);
        }
    }

    /// Does what `action` asks.
    ///
    /// The tabs and u have nothing to show here, and say where they do.
    pub(super) fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        match action {
            // A start's page has the tabs, which the start page hasn't.
            Action::Tab(_) => Some(Go::Say(
                match self.printed.as_ref()?.has_links() {
                    true => "Enter opens the selected start's page, which has the tabs",
                    false => "the tabs are on an item's page; g goes to one",
                }
                .to_string(),
            )),
            Action::Keeper => Some(Go::Say(
                "u opens the item that keeps an item, from that item's page".to_string(),
            )),
            // The rest act on the rows, as on any printed page.
            other => self.printed.as_mut()?.handle(session, other),
        }
    }

    /// What the selected row opens, in full.
    pub(super) fn preview(&self, session: &Session) -> Option<String> {
        self.printed.as_ref()?.preview(session)
    }

    /// Whether some row opens a start's page.
    pub(super) fn has_links(&self) -> bool {
        (self.printed.as_ref()).is_some_and(|printed| printed.has_links())
    }

    /// The command under the page, `reach` without SYMBOL.
    pub(super) fn command(&self) -> Option<&str> {
        self.printed.as_ref().map(|printed| printed.command())
    }

    /// The cursor over the rows, which find moves.
    pub(super) fn cursor(&mut self) -> Option<&mut Cursor> {
        self.printed.as_mut().map(|printed| printed.cursor())
    }

    /// The rows that have `lowercase`, ignoring case.
    pub(super) fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        (self.printed.as_ref()).map_or_else(Vec::new, |printed| printed.rows_with(lowercase))
    }
}

/// What the start page of the file that `session` browses shows.
fn page(session: &Session) -> Printed {
    let binary = session.binary;
    let path = &session.path;
    let mut overview = vec![
        Lines::Words(overview::file_text(path, binary)),
        Lines::Words(overview::found_text(binary)),
    ];
    overview.extend(overview::found_notes(binary).into_iter().map(Lines::Words));
    if let Some(memory) = overview::memory_table(binary, &session.blocks) {
        overview.push(Lines::Table(table_lines(&memory)));
    }
    let tab = reach::starts(binary, &session.graph, &session.names).tab(path);
    let command = session.command("reach", None, tab.needs_all);
    Printed::of_flat(tab.answer, command)
        .with_context(overview)
        .with_brief(tab.brief)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::super::{samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::Cli;
    use crate::commands;
    use crate::graph::{Node, Root, RootKind};

    /// What elfq prints for `words`, a command line, as it runs it, with the
    /// empty lines left out.
    fn printed(words: &[String]) -> Vec<String> {
        let cli = Cli::try_parse_from(words).unwrap_or_else(|error| panic!("{words:?}: {error}"));
        let mut out = Vec::new();
        commands::run(&cli, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        out.lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_start_page_shows_what_the_overview_and_reach_print() {
        let mut pages = 0;
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let start = page(&session);
            // The command under it parses as elfq's arguments, after the
            // shell has split it into words, and names the file.
            let line = start.command().strip_prefix("$ ").unwrap();
            let words = shell_words(line);
            assert_eq!(words[0], "elfq", "{line}");
            let cli = Cli::try_parse_from(&words).unwrap();
            assert_eq!(cli.file, path, "{line}");
            // The page has the overview's lines up to its blocks of memory,
            // then every line of the command's answer, with --all.
            let overview = printed(&words[..2]);
            let mut head: Vec<String> = overview
                .iter()
                .take_while(|line| !line.starts_with("RETAINED") && *line != "Try:")
                .cloned()
                .collect();
            let all = printed(&words);
            head.extend(all.iter().cloned());
            assert_eq!(start.printed(), head, "{line}");
            // The command has --all exactly when it needs it to list every
            // start.
            let without: Vec<String> = words
                .iter()
                .filter(|&word| word != "--all")
                .cloned()
                .collect();
            let cut = printed(&without) != all;
            assert_eq!(line.ends_with(" --all"), cut, "{line}");
            pages += 1;
        }
        // No sample has more starts than `reach` lists without --all, or
        // more than a few that elfq can't read: the next test has them.
        assert!(pages > 40, "{pages}");
    }

    #[test]
    fn a_page_of_more_starts_than_reach_lists_lists_them_all() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-deep.elf");
        let binary = Binary::load(&path).unwrap();
        let mut session = Session::new(&path, &binary).unwrap();
        // As if the unwinder started each of the file's own functions.
        for (index, item) in binary.items.iter().enumerate() {
            let node = Node::Item(index);
            if item.is_own_function() && session.graph.root(node).is_none() {
                let kind = RootKind::Personality;
                session.graph.roots.push(Root { node, kind });
            }
        }
        session.graph.roots.sort_by_key(|root| root.node);
        let starts = reach::starts(&binary, &session.graph, &session.names);
        assert!(starts.starts.len() > 30, "{}", starts.starts.len());
        // Every start has a row, and the command under them has --all,
        // since `reach` would list fewer.
        let page = page(&session);
        let all = String::from_utf8(starts.tab(&path).answer.written()).unwrap();
        let lines: Vec<String> = (all.lines())
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        assert!(page.printed().ends_with(&lines));
        assert_eq!(page.names().len(), starts.starts.len());
        assert!(
            page.command().ends_with(" reach --all"),
            "{}",
            page.command()
        );
    }
}
