//! Tab 3 of an item's page: what calls or uses the item, in the rows and
//! notes that `elfq FILE callers SYMBOL` prints, from the same code
//! (`callers::text`). The tab lists every row, where the command lists the
//! 30 largest callers, and under the rows is that command, ready to paste,
//! with `--all` when the command needs it to list them all. Enter opens
//! the selected caller's page on its callers, with its code at the call.
//! `printed.rs` draws it.

use super::item::Tab;
use super::printed::Printed;
use crate::commands::browse::session::Session;
use crate::commands::flat_answer::FlatAnswer;
use crate::commands::uses;
use crate::graph::Node;

/// The callers of `node`, from `answer`, the answer that `callers SYMBOL
/// --all` gives for it (see [`Session::callers_text`]): HOW, AT and
/// CALLER, each row opening the caller at the first place where it uses
/// `node`.
pub(super) fn tab(session: &Session, node: Node, mut answer: FlatAnswer) -> Printed {
    // `callers SYMBOL` lists the rows of the 30 largest callers, unless
    // with --all, and a last row counts the rest.
    let needs_all = uses::table_needs_all(answer.node_count());
    // The page's header counts the callers, so the tab leaves out the
    // first line, which counts them too, unless it is all there is to say.
    if !answer.rows.is_empty() {
        answer.lead.clear();
    }
    let command = session.command("callers", Some(node), needs_all);
    Printed::of_flat(answer, command).opening(Tab::Callers, node)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::fit_name;
    use super::super::names::alike_in_a_window;
    use super::super::test_support::{assert_has_all_when_needed, each_session, tab_command};
    use super::*;
    use crate::binary::test_samples::load_sample;
    use crate::cli::{Command, Format};
    use crate::commands::callers;
    use crate::commands::symbol::Symbol;
    use crate::commands::text::Detail;

    /// What `command` gives as SYMBOL, when it is `callers` with one
    /// SYMBOL: the tests' own match on the command under the tab, for
    /// `tab_command`.
    fn query(command: &Command) -> Option<&str> {
        match command {
            Command::Callers(args) => match &args.uses.symbols[..] {
                [query] => Some(query),
                _ => None,
            },
            _ => None,
        }
    }

    /// What `callers` prints for `symbol`, as elfq runs it, but on the
    /// graph that `session` has already built, and what it prints without
    /// `--all`.
    fn run(session: &Session, symbol: &Symbol) -> (String, String) {
        let output = |detail| {
            let mut out = Vec::new();
            let (binary, graph) = (session.binary, &session.graph);
            callers::callers(binary, graph, symbol, detail, Format::Text, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        (output(Detail::All), output(Detail::Brief))
    }

    #[test]
    fn the_callers_tab_lists_what_its_command_prints() {
        // Pages, and those whose command needs --all.
        let (mut pages, mut cut_pages) = (0, 0);
        each_session(|path, binary, session| {
            // Every item, and every import that has a page of its own.
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes = items.chain(imports.filter(|&node| session.page_node(node) == node));
            for node in nodes {
                let tab = tab(&session, node, session.callers_text(node));
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words, and finds the item in the file.
                let command = tab_command(&session, &tab, Some(node), query);
                let line = &command.line;
                let (every, brief) = run(&session, command.symbol.as_ref().unwrap());
                // Its rows and notes are the tab's, with --all, after its
                // first line, which says what the tab says in place of the
                // rows when nothing uses the item.
                let (lead, rest) = every.split_once('\n').unwrap();
                let rest: Vec<&str> = rest.lines().filter(|line| !line.is_empty()).collect();
                let mut printed = tab.printed();
                if tab.names().is_empty() {
                    assert!(lead.starts_with("Nothing in the file calls or uses "));
                    assert!(printed.remove(0).starts_with("Nothing in the file calls"));
                }
                assert_eq!(rest, printed, "{name}: {line}");
                // The command has --all exactly when it needs it to list
                // every row.
                assert_has_all_when_needed(line, &every, &brief);
                pages += 1;
                cut_pages += usize::from(every != brief);
            }
        });
        assert!(pages > 4500 && cut_pages > 0, "{pages} {cut_pages}");
    }

    #[test]
    fn callers_that_would_read_the_same_read_differently_on_the_screen() {
        // The pages where some callers on the screen would read the same
        // if each name were shortened by itself.
        let mut pages = 0;
        each_session(|_, binary, session| {
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            let nodes = items.chain(imports.filter(|&node| session.page_node(node) == node));
            for node in nodes {
                let tab = tab(&session, node, session.callers_text(node));
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
        });
        assert!(pages > 100, "{pages}");
    }

    #[test]
    fn the_command_quotes_the_file_as_typed() {
        let binary = load_sample("testdata/bin/c-x86_64.elf");
        let typed = Path::new("my samples/it's c-x86_64.elf");
        let session = Session::new(typed, &binary).unwrap();
        let node = session.names.find("log_msg").unwrap().node;
        let tab = tab(&session, node, session.callers_text(node));
        let command = tab_command(&session, &tab, Some(node), query);
        // The test's point: the command names the file as typed, which
        // tab_command checks only against the session's path.
        assert_eq!(command.cli.file, typed, "{}", command.line);
    }
}
