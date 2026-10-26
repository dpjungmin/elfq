//! Tab 2 of an item's page: what the item calls or uses, in the rows and
//! notes that `elfq FILE callees SYMBOL` prints, from the same code
//! (`callees::answer`). The tab lists every row and every site, where the
//! command lists the 30 largest callees and the first few sites that elfq
//! can't follow, and under them is that command, ready to paste, with
//! `--all` when the command needs it to list them all. When the notes
//! don't fit, they are as the command words them without `--all`. Enter
//! opens the selected callee's page on its callees. `printed.rs` draws it.

use super::item::Tab;
use super::printed::{Above, Printed, PrintedRow};
use crate::commands::browse::session::Session;
use crate::commands::{callees, distinct_nodes, text};
use crate::graph::Node;
use crate::render::Align;

/// What the item at `index` calls or uses, as `callees SYMBOL` lists it,
/// which names it `name` (see [`Session::lead_name`]): HOW, AT and
/// CALLEE, each row opening the callee.
pub fn tab(session: &Session, index: usize, name: &str) -> Printed {
    let (binary, graph) = (session.binary, &session.graph);
    let answer = callees::answer(binary, graph, index, name, true);
    let brief = callees::answer(binary, graph, index, name, false).notes;
    // `callees SYMBOL` lists the rows of the 30 largest callees, unless
    // with --all, and a last row counts the rest; and it gives only the
    // first few sites of the calls and jumps that elfq can't follow.
    let count = distinct_nodes(&answer.rows).len();
    let cut = text::shown_count(count, text::shown(false)) < count || brief != answer.notes;
    let node = Node::Item(index);
    let command = session.command("callees", Some(node), cut);
    // What the command says in place of the table, when it has none, is
    // said at the top.
    let (above, notes, brief) = if answer.rows.is_empty() {
        let above = answer.notes.into_iter().map(Above::Words).collect();
        (above, Vec::new(), Vec::new())
    } else {
        (Vec::new(), answer.notes, brief)
    };
    let rows = (answer.rows.into_iter())
        .map(|row| PrintedRow {
            cells: vec![row.how.to_string(), text::at_column(&row.sites)],
            name: row.node.name(binary).to_string(),
            rest: String::new(),
            link: Some((row.node, None)),
        })
        .collect();
    let columns = ["HOW", "AT", "CALLEE"].map(|header| (header.to_string(), Align::Left));
    (Printed::new(above, columns.into(), rows, notes, command))
        .with_brief(brief)
        .opening(Tab::Callees, node)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::{samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};
    use crate::commands::readable_item;

    #[test]
    fn the_callees_tab_lists_what_its_command_prints() {
        // Pages, and those whose command needs --all.
        let (mut pages, mut cut_pages) = (0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            // Every item whose code `callees` reads.
            for index in 0..binary.items.len() {
                let node = Node::Item(index);
                if readable_item(&binary, node).is_err() {
                    continue;
                }
                let tab = tab(&session, index, &session.lead_name(node));
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words, and finds the item in the file.
                let line = tab.command().strip_prefix("$ ").unwrap();
                let words = shell_words(line);
                assert_eq!(words[0], "elfq", "{line}");
                let cli = Cli::try_parse_from(&words).unwrap_or_else(|e| panic!("{line}: {e}"));
                assert_eq!(cli.file, path, "{line}");
                let Some(Command::Callees(args)) = cli.command else {
                    panic!("{line} is not callees");
                };
                let [query] = &args.uses.symbols[..] else {
                    panic!("{line} has no one SYMBOL");
                };
                let symbol = session.names.find(query).unwrap();
                assert_eq!(symbol.node, node, "{line}");
                // What it prints, as `callees` runs it, but on the graph
                // that the session has built, with --all and without.
                let output = |all| {
                    let mut out = Vec::new();
                    let (graph, lead) = (&session.graph, symbol.lead_name(&binary));
                    callees::callees(&binary, graph, index, &lead, all, Format::Text, &mut out)
                        .unwrap();
                    String::from_utf8(out).unwrap()
                };
                let (all, cut) = (output(true), output(false));
                // The tab has its lines with --all, but for the first,
                // which counts the rows, when there are rows.
                let mut lines: Vec<&str> = all.lines().filter(|line| !line.is_empty()).collect();
                if !tab.names().is_empty() {
                    assert!(lines.remove(0).contains(" uses "), "{name}: {line}");
                }
                assert_eq!(lines, tab.printed(), "{name}: {line}");
                // The command has --all exactly when it needs it to list
                // every row and every site.
                assert_eq!(line.ends_with(" --all"), all != cut, "{name}: {line}");
                pages += 1;
                cut_pages += usize::from(all != cut);
            }
        }
        assert!(pages > 4000 && cut_pages > 0, "{pages} {cut_pages}");
    }
}
