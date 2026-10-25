//! Tab 4 of an item's page: everything the item calls or uses, at any
//! depth, as `elfq FILE reach SYMBOL` prints it, from the same code
//! (`reach::answer`): its first line, the bytes in each block of memory
//! when there are several, the rows, and the notes. The tab lists every
//! row, where the command lists the 30 largest items, and under them is
//! that command, ready to paste, with `--all` when the command needs it to
//! list them all. When the notes don't fit, as one that names every
//! function that calls through pointers, they are as the command words
//! them without `--all`. Enter opens the selected row's item on what it
//! reaches: for a call or jump that elfq can't follow, the item that makes
//! it. `printed.rs` draws it.

use super::item::Tab;
use super::printed::Printed;
use crate::commands::browse::session::Session;
use crate::commands::reach;
use crate::graph::Node;

/// What the item at `index` reaches, as `reach SYMBOL` lists it, which
/// names it `name` (see [`Session::lead_name`]).
pub fn tab(session: &Session, index: usize, name: &str) -> Printed {
    let (binary, graph, names) = (session.binary, &session.graph, &session.names);
    let (answer, brief) = reach::answer(binary, graph, names, index, name.to_string());
    // `reach SYMBOL` lists the 30 items that take the most bytes, and
    // names only a few functions that elfq can't follow, unless with
    // --all.
    let cut = brief.written() != answer.written();
    let node = Node::Item(index);
    let command = session.command("reach", Some(node), cut);
    (Printed::of_reach(answer, command))
        .with_brief(brief.notes)
        .opening(Tab::Reach, node)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::super::{samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};
    use crate::commands::readable_item;

    #[test]
    fn the_reach_tab_lists_what_its_command_prints() {
        // Pages, and those whose command needs --all.
        let (mut pages, mut cut_pages) = (0, 0);
        for path in samples() {
            // Reach takes a while on the items of a large program, so four
            // threads each check every fourth item, each with its own
            // session, which can't be shared.
            let threads = 4;
            std::thread::scope(|scope| {
                let path = &path;
                let checks: Vec<_> = (0..threads)
                    .map(|first| scope.spawn(move || check_pages(path, first, threads)))
                    .collect();
                for check in checks {
                    let (checked, cut) = check.join().unwrap();
                    pages += checked;
                    cut_pages += cut;
                }
            });
        }
        assert!(pages > 4000 && cut_pages > 50, "{pages} {cut_pages}");
    }

    /// Checks the reach tab of every `step`th item from `first` in the
    /// file at `path`, of those whose code `reach` reads, and counts the
    /// pages and those whose command needs --all.
    fn check_pages(path: &Path, first: usize, step: usize) -> (usize, usize) {
        let (mut pages, mut cut_pages) = (0, 0);
        let binary = Binary::load(path).unwrap();
        let session = Session::new(path, &binary).unwrap();
        for index in (first..binary.items.len()).step_by(step) {
            let node = Node::Item(index);
            if readable_item(&binary, node).is_err() {
                continue;
            }
            let tab = tab(&session, index, &session.lead_name(node));
            let name = path.display();
            // The command parses as elfq's arguments, after the shell has
            // split it into words, and finds the item in the file.
            let line = tab.command().strip_prefix("$ ").unwrap();
            let words = shell_words(line);
            assert_eq!(words[0], "elfq", "{line}");
            let cli = Cli::try_parse_from(&words).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(cli.file, path, "{line}");
            let Some(Command::Reach(args)) = cli.command else {
                panic!("{line} is not reach");
            };
            let symbol = session.names.find(args.symbol.as_ref().unwrap()).unwrap();
            assert_eq!(symbol.node, node, "{line}");
            // What it prints, as `reach` runs it, but on the graph that the
            // session has built, with --all and without.
            let output = |all| {
                let mut out = Vec::new();
                let (graph, names) = (&session.graph, &session.names);
                let text = Format::Text;
                reach::reach(
                    &binary,
                    graph,
                    names,
                    &symbol,
                    index,
                    &[],
                    all,
                    text,
                    &mut out,
                )
                .unwrap();
                String::from_utf8(out).unwrap()
            };
            let (all, cut) = (output(true), output(false));
            // The tab has every line it prints with --all.
            let lines: Vec<&str> = all.lines().filter(|line| !line.is_empty()).collect();
            assert_eq!(lines, tab.printed(), "{name}: {line}");
            // The command has --all exactly when it needs it to list every
            // row and every name.
            assert_eq!(line.ends_with(" --all"), all != cut, "{name}: {line}");
            pages += 1;
            cut_pages += usize::from(all != cut);
        }
        (pages, cut_pages)
    }
}
