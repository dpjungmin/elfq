//! Tab 5 of an item's page: why the item is in the file, as `elfq FILE why
//! SYMBOL` prints it, from the same code (`why::answers`): the shortest
//! chains from where the program starts through the items that keep it,
//! merged into one tree, and the notes, the last of which names the item
//! on every chain. The tab shows the chain through every item that keeps
//! it, where the command shows those through the nearest 10, and under
//! them is that command, ready to paste, with `--all` when the command
//! needs it to show them all. Enter opens the selected row's item on why
//! it is there. `printed.rs` draws it.

use super::item::Tab;
use super::printed::{Printed, note_name};
use crate::commands::browse::session::Session;
use crate::commands::why;
use crate::graph::Node;

/// Why `node` is in the file, as `why SYMBOL` says it, which names it
/// `name` (see [`Session::lead_name`]), once the session has worked out
/// what keeps each item.
pub fn tab(session: &Session, node: Node, name: &str) -> Option<Printed> {
    let keepers = session.keepers()?;
    let (binary, graph, names) = (session.binary, &session.graph, &session.names);
    let answer = why::answers(binary, graph, names, &keepers.tree, node, name, &note_name);
    let command = session.command("why", Some(node), answer.cut);
    Some(Printed::of_tab(answer, command).opening(Tab::Why, node))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::{samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};

    #[test]
    fn the_why_tab_shows_what_its_command_prints() {
        // Pages, those whose command needs --all, and roots.
        let (mut pages, mut cut_pages, mut roots) = (0, 0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let mut session = Session::new(&path, &binary).unwrap();
            session.work_out_keepers();
            let items = (0..binary.items.len()).map(Node::Item);
            for node in items.chain((0..binary.imports.len()).map(Node::Import)) {
                // An import with a PLT stub has the stub's page.
                if session.page_node(node) != node {
                    continue;
                }
                let Some(tab) = tab(&session, node, &session.lead_name(node)) else {
                    // `why` answers about no item of a shared library.
                    assert!(why::refusal(&binary, node).is_err());
                    continue;
                };
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words, and finds the item in the file.
                let line = tab.command().strip_prefix("$ ").unwrap();
                let words = shell_words(line);
                assert_eq!(words[0], "elfq", "{line}");
                let cli = Cli::try_parse_from(&words).unwrap_or_else(|e| panic!("{line}: {e}"));
                assert_eq!(cli.file, path, "{line}");
                let Some(Command::Why(args)) = cli.command else {
                    panic!("{line} is not why");
                };
                let symbol = session.names.find(&args.symbol).unwrap();
                assert_eq!(symbol.node, node, "{line}");
                // What it prints, as `why` runs it, but on the graph that
                // the session has built, with --all and without.
                let output = |all| {
                    let mut out = Vec::new();
                    let (graph, names, text) = (&session.graph, &session.names, Format::Text);
                    why::why(&binary, graph, names, &symbol, all, text, &mut out).unwrap();
                    String::from_utf8(out).unwrap()
                };
                let (all, cut) = (output(true), output(false));
                // The tab has every line it prints with --all.
                let lines: Vec<&str> = all.lines().filter(|line| !line.is_empty()).collect();
                assert_eq!(lines, tab.printed(), "{name}: {line}");
                // The command has --all exactly when it needs it to show
                // every chain and name.
                assert_eq!(line.ends_with(" --all"), all != cut, "{name}: {line}");
                pages += 1;
                cut_pages += usize::from(all != cut);
                roots += usize::from(session.graph.root(node).is_some());
            }
        }
        assert!(
            pages > 4500 && cut_pages > 50 && roots > 150,
            "{pages} {cut_pages} {roots}"
        );
    }
}
