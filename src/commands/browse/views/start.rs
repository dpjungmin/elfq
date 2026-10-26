//! The start page, which browse opens when it is given no SYMBOL: what the
//! file is and what elfq found in it, with the blocks of memory that its
//! sections occupy, as the overview says them, then where the program
//! starts and the bytes that each start reaches, as `elfq FILE reach`
//! lists them, from the same code (`reach::starts`). It lists every start,
//! and under them is that command, ready to paste, with `--all` when the
//! command needs it to list them all. Enter opens the selected start's
//! page. `printed.rs` draws it.

use super::printed::{Lines, Printed, table_lines};
use crate::commands::browse::session::Session;
use crate::commands::{overview, reach};

/// The start page of the file that `session` browses.
pub fn page(session: &Session) -> Printed {
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
    let starts = reach::starts(binary, &session.graph, &session.names);
    let (answer, brief) = (starts.answer(path, true), starts.answer(path, false));
    // `reach` lists only the starts that reach the most, and names only
    // a few functions that elfq can't read, unless with --all.
    let cut = brief.written() != answer.written();
    let command = session.command("reach", None, cut);
    let brief = brief.notes;
    Printed::of_reach(answer, command)
        .with_context(overview)
        .with_brief(brief)
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
        for index in 0..binary.items.len() {
            let node = Node::Item(index);
            if overview::is_own_function(&binary, index) && session.graph.root(node).is_none() {
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
        let all = String::from_utf8(starts.answer(&path, true).written()).unwrap();
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
