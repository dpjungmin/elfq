//! Tab 3 of an item's page: what calls or uses the item, in the rows and
//! notes that `elfq FILE callers SYMBOL` prints, from the same code
//! (`callers::answer`). The tab lists every row, where the command lists
//! the 30 largest callers, and under the rows is that command, ready to
//! paste, with `--all` when the command needs it to list them all. Enter
//! opens the selected caller's page on its callers, with its code at the
//! call. `printed.rs` draws it.

use super::item::Tab;
use super::printed::Printed;
use crate::commands::browse::session::Session;
use crate::commands::flat_answer::FlatAnswer;
use crate::commands::uses;
use crate::graph::Node;

/// The callers of `node`, from `answer`, the answer that `callers SYMBOL
/// --all` gives for it (see [`Session::callers`]): HOW, AT and CALLER,
/// each row opening the caller at the first place where it uses `node`.
pub(super) fn tab(session: &Session, node: Node, mut answer: FlatAnswer) -> Printed {
    // `callers SYMBOL` lists the rows of the 30 largest callers, unless
    // with --all, and a last row counts the rest.
    let cut = uses::table_needs_all(answer.node_count());
    // The page's header counts the callers, so the tab leaves out the
    // first line, which counts them too, unless it is all there is to say.
    if !answer.rows.is_empty() {
        answer.lead.clear();
    }
    let command = session.command("callers", Some(node), cut);
    Printed::of_flat(answer, command).opening(Tab::Callers, node)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::super::names::alike_in_a_window;
    use super::super::{fit_name, samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};
    use crate::commands::callers;

    /// Runs `words`, a `callers` command line, as elfq does, but on the
    /// graph that `session` has already built, and returns what it prints,
    /// and what it prints without `--all`. The command must ask about
    /// `node`, in the file that `session` browses.
    fn run(session: &Session, words: &[String], node: Node) -> (String, String) {
        let cli = Cli::try_parse_from(words).unwrap_or_else(|error| panic!("{words:?}: {error}"));
        assert_eq!(cli.file, session.path, "{words:?}");
        let Some(Command::Callers(args)) = cli.command else {
            panic!("{words:?} is not callers");
        };
        let [query] = &args.uses.symbols[..] else {
            panic!("{words:?} has no one SYMBOL");
        };
        let symbol = session.names.find(query).unwrap();
        assert_eq!(symbol.node, node, "{words:?}");
        let output = |all| {
            let mut out = Vec::new();
            let (binary, graph) = (session.binary, &session.graph);
            callers::callers(binary, graph, &symbol, all, Format::Text, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        (output(true), output(false))
    }

    #[test]
    fn the_callers_tab_lists_what_its_command_prints() {
        // Pages, and those whose command needs --all.
        let (mut pages, mut cut_pages) = (0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            // Every item, and every import that has a page of its own.
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes = items.chain(imports.filter(|&node| session.page_node(node) == node));
            for node in nodes {
                let tab = tab(&session, node, session.callers(node));
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words.
                let line = tab.command().strip_prefix("$ ").unwrap();
                let words = shell_words(line);
                assert_eq!(words[0], "elfq", "{line}");
                let (all, cut) = run(&session, &words, node);
                // Its rows and notes are the tab's, with --all, after its
                // first line, which says what the tab says in place of the
                // rows when nothing uses the item.
                let (lead, rest) = all.split_once('\n').unwrap();
                let rest: Vec<&str> = rest.lines().filter(|line| !line.is_empty()).collect();
                let mut printed = tab.printed();
                if tab.names().is_empty() {
                    assert!(lead.starts_with("Nothing in the file calls or uses "));
                    assert!(printed.remove(0).starts_with("Nothing in the file calls"));
                }
                assert_eq!(rest, printed, "{name}: {line}");
                // The command has --all exactly when it needs it to list
                // every row.
                assert_eq!(line.ends_with(" --all"), all != cut, "{name}: {line}");
                pages += 1;
                cut_pages += usize::from(all != cut);
            }
        }
        assert!(pages > 4500 && cut_pages > 0, "{pages} {cut_pages}");
    }

    #[test]
    fn callers_that_would_read_the_same_read_differently_on_the_screen() {
        // The pages where some callers on the screen would read the same
        // if each name were shortened by itself.
        let mut pages = 0;
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes = items.chain(imports.filter(|&node| session.page_node(node) == node));
            for node in nodes {
                let tab = tab(&session, node, session.callers(node));
                let names = tab.names();
                for width in [60, 80, 100] {
                    let room = tab.name_room(width);
                    if let Some((one, other, both)) = alike_in_a_window(&names, room) {
                        panic!("{one} and {other} both read {both} at {width}");
                    }
                    let mut alone: Vec<String> =
                        names.iter().map(|name| fit_name(name, room)).collect();
                    alone.sort();
                    alone.dedup();
                    let mut distinct = names.clone();
                    distinct.sort();
                    distinct.dedup();
                    pages += usize::from(alone.len() < distinct.len());
                }
            }
        }
        assert!(pages > 100, "{pages}");
    }

    #[test]
    fn the_command_quotes_the_file_as_typed() {
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&real).unwrap();
        let typed = Path::new("my samples/it's c-x86_64.elf");
        let session = Session::new(typed, &binary).unwrap();
        let node = session.names.find("log_msg").unwrap().node;
        let tab = tab(&session, node, session.callers(node));
        let words = shell_words(tab.command().strip_prefix("$ ").unwrap());
        let cli = Cli::try_parse_from(&words).unwrap_or_else(|error| panic!("{words:?}: {error}"));
        assert_eq!(cli.file, typed, "{}", tab.command());
    }
}
