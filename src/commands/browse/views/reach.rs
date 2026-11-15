//! Tab 4 of an item's page: everything the item calls or uses, at any
//! depth, as `elfq FILE reach SYMBOL` prints it, from the same code
//! (`reach::tab`): its first line, the bytes in each block of memory
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
pub(super) fn tab(session: &Session, index: usize, name: &str) -> Printed {
    let (binary, graph, names) = (session.binary, &session.graph, &session.names);
    let tab = reach::tab(binary, graph, names, index, name);
    let node = Node::Item(index);
    let command = session.command("reach", Some(node), tab.needs_all);
    Printed::of_flat_tab(tab, command).opening(Tab::Reach, node)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::test_support::{assert_has_all_when_needed, tab_command};
    use super::*;
    use crate::binary::Binary;
    use crate::binary::test_samples::samples;
    use crate::cli::Command;
    use crate::commands::checks::readable_item;
    use crate::commands::text::Detail;

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
        // As in each_session, so that tab_command checks the command's file
        // against the path opened here.
        assert_eq!(session.path, path);
        for index in (first..binary.items.len()).step_by(step) {
            let node = Node::Item(index);
            if readable_item(&binary, node).is_err() {
                continue;
            }
            let tab = tab(&session, index, &session.lead_name(node));
            let name = path.display();
            // The command parses as elfq's arguments, after the shell has
            // split it into words, and finds the item in the file.
            let command = tab_command(&session, &tab, Some(node), |parsed| match parsed {
                Command::Reach(args) => args.symbol.as_deref(),
                _ => None,
            });
            let (line, symbol) = (&command.line, command.symbol.unwrap());
            // What it prints, as `reach` runs it, but on the graph that the
            // session has built, with --all and without.
            let output = |detail| {
                let (graph, names) = (&session.graph, &session.names);
                let lead_name = symbol.lead_name(&binary);
                reach::text_output(&binary, graph, names, index, &lead_name, detail)
            };
            let (every, brief) = (output(Detail::All), output(Detail::Brief));
            // The tab has every line it prints with --all.
            let lines: Vec<&str> = every.lines().filter(|line| !line.is_empty()).collect();
            assert_eq!(lines, tab.printed(), "{name}: {line}");
            // The command has --all exactly when it needs it to list every
            // row and every name.
            assert_has_all_when_needed(line, &every, &brief);
            pages += 1;
            cut_pages += usize::from(every != brief);
        }
        (pages, cut_pages)
    }
}
