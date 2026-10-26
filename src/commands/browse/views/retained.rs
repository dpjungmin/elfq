//! Tab 6 of an item's page: what removing the item would save, as `elfq
//! FILE retained SYMBOL` prints it, from the same code
//! (`retained::answers`): the tree of the items that only it leads to,
//! and the notes. The tab lists every item, where the command lists the 30
//! largest, and under them is that command, ready to paste, with `--all`
//! when the command needs it to list them all. In a file with several
//! blocks of memory, a table under the notes gives the bytes that removing
//! the item would save in each, as `retained SYMBOL --range BLOCK` counts
//! them: what would leave ITCM or flash. Enter opens the selected row's
//! item on what removing it would save. `printed.rs` draws it.

use super::item::Tab;
use super::printed::{Lines, Printed, note_name, table_lines};
use crate::commands::browse::session::Session;
use crate::commands::retained::{self, SymbolInTree};
use crate::commands::{json, overview, text, total};
use crate::graph::{DominatorTree, Node};
use crate::render::{Align, Table};

/// What removing the item at `index` would save, as `retained SYMBOL`
/// says it, which names it `name` (see [`Session::lead_name`]), once the
/// session has worked out what keeps each item.
pub fn tab(session: &Session, index: usize, name: &str) -> Option<Printed> {
    let retained = session.retained_tree()?;
    let symbol = SymbolInTree {
        retained: &retained,
        index,
        lead_name: name,
    };
    let answer = retained::answers(&symbol, &note_name);
    let node = Node::Item(index);
    let command = session.command("retained", Some(node), answer.cut);
    let printed = (Printed::of_tab(answer, command))
        .with_below(by_block(session, &retained.tree, node))
        .opening(Tab::Retained, node);
    Some(printed)
}

/// The bytes that removing `node` would save in each block of memory, as
/// `retained SYMBOL --range BLOCK` counts them, with the block's sections
/// as the overview lists them: none in a file with one block, or where
/// nothing reaches `node`, which `retained` says.
///
/// A range changes only the sizes in the dominator tree, not which item
/// keeps which, so they are the bytes in the block of `node` and of
/// everything only it leads to, load images too.
fn by_block(session: &Session, tree: &DominatorTree, node: Node) -> Vec<Lines> {
    let binary = session.binary;
    if session.blocks.len() < 2 || !tree.is_reachable(node) {
        return Vec::new();
    }
    let mut kept = tree.exclusive(node);
    kept.push(node);
    let items: Vec<_> = (kept.into_iter())
        .filter_map(|node| match node {
            Node::Item(index) => Some(&binary.items[index]),
            Node::Import(_) => None,
        })
        .collect();
    let mut table = Table::new(&[
        ("MEMORY", Align::Left),
        ("RETAINED", Align::Right),
        ("SECTIONS", Align::Left),
    ]);
    for block in &session.blocks {
        let range = Some(block.range);
        let bytes = total(items.iter().map(|item| binary.bytes_in(item, range)));
        let sections = json::section_names(binary, &block.sections);
        let load_images = json::section_names(binary, &block.load_images);
        table.row(vec![
            text::range_argument(block.range),
            bytes.to_string(),
            overview::sections_text(&sections, &load_images),
        ]);
    }
    vec![
        Lines::Words("Keeps by block, as retained --range MEMORY counts it:".to_string()),
        Lines::Table(table_lines(&table)),
    ]
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::{samples, shell_words};
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};
    use crate::commands::retained_tree::RetainedTree;

    #[test]
    fn the_retained_tab_lists_what_its_command_prints() {
        // Pages, those whose command needs --all, and those with the bytes
        // by block.
        let (mut pages, mut cut_pages, mut by_block) = (0, 0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let mut session = Session::new(&path, &binary).unwrap();
            session.work_out_keepers();
            let graph = &session.graph;
            // The trees that the command builds, without a range and with
            // each block's.
            let whole = RetainedTree::new(&binary, graph, None);
            let blocks: Vec<RetainedTree> = (session.blocks.iter())
                .map(|block| RetainedTree::new(&binary, graph, Some(block.range)))
                .collect();
            for index in 0..binary.items.len() {
                let node = Node::Item(index);
                if retained::refusal(&binary, node).is_err() {
                    continue;
                }
                let tab = tab(&session, index, &session.lead_name(node)).unwrap();
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words, and finds the item in the file.
                let line = tab.command().strip_prefix("$ ").unwrap();
                let words = shell_words(line);
                assert_eq!(words[0], "elfq", "{line}");
                let cli = Cli::try_parse_from(&words).unwrap_or_else(|e| panic!("{line}: {e}"));
                assert_eq!(cli.file, path, "{line}");
                let Some(Command::Retained(args)) = cli.command else {
                    panic!("{line} is not retained");
                };
                let symbol = session.names.find(&args.symbol).unwrap();
                assert_eq!(symbol.node, node, "{line}");
                // What it prints, as `retained` runs it, but on the graph
                // that the session has built, with --all and without.
                let lead_name = symbol.lead_name(&binary);
                let output = |all| {
                    let symbol = SymbolInTree {
                        retained: &whole,
                        index,
                        lead_name: &lead_name,
                    };
                    String::from_utf8(retained::answer(&symbol, all, Format::Text).written())
                        .unwrap()
                };
                let (all, cut) = (output(true), output(false));
                // The tab has every line it prints with --all.
                let lines: Vec<&str> = all.lines().filter(|line| !line.is_empty()).collect();
                assert_eq!(lines, tab.printed(), "{name}: {line}");
                // The command has --all exactly when it needs it to list
                // every row and name.
                assert_eq!(line.ends_with(" --all"), all != cut, "{name}: {line}");
                pages += 1;
                cut_pages += usize::from(all != cut);

                // Under the notes, what removing it would save in each
                // block, as `retained SYMBOL --range BLOCK` says, where
                // there are several and something reaches it.
                let below = tab.below();
                if blocks.len() < 2 || !whole.tree.is_reachable(node) {
                    assert!(below.is_empty(), "{name}: {line}");
                    continue;
                }
                assert_eq!(below.len(), 2 + blocks.len(), "{name}: {line}");
                for (row, block) in below[2..].iter().zip(&blocks) {
                    let cells: Vec<&str> = row.split_whitespace().collect();
                    let range = format!("--range={}", cells[0]);
                    let cli =
                        Cli::try_parse_from(words.iter().map(String::as_str).chain([&*range]));
                    assert_eq!(cli.unwrap().range, block.range, "{name}: {row}");
                    let bytes = block.tree.retained(node).to_string();
                    assert_eq!(cells[1], bytes, "{name}: {line} {row}");
                }
                by_block += 1;
            }
        }
        assert!(
            pages > 4500 && cut_pages > 150 && by_block > 1000,
            "{pages} {cut_pages} {by_block}"
        );
    }

    #[test]
    fn the_answer_keeps_its_first_line_on_a_small_screen() {
        let path = samples()
            .into_iter()
            .find(|path| path.ends_with("c-cortex-m7.elf"))
            .unwrap();
        let binary = Binary::load(&path).unwrap();
        let mut session = Session::new(&path, &binary).unwrap();
        session.work_out_keepers();
        for name in ["main", "process_frame", "crc32"] {
            let Node::Item(index) = session.names.find(name).unwrap().node else {
                panic!("{name} is not an item");
            };
            let mut tab = tab(&session, index, name).unwrap();
            for (width, height) in (6..=24).flat_map(|height| [(80, height), (100, height)]) {
                let screen = tab.drawn((width, height));
                let lines: Vec<&str> = screen.lines().collect();
                let lead = format!("Removing {name} would save");
                assert!(lines[0].starts_with(&lead), "{width}x{height}:\n{screen}");
                // The bytes by block are whole, or what is left of them
                // ends in "…".
                let by_block = lines
                    .iter()
                    .position(|line| line.starts_with("Keeps by block"));
                if let Some(at) = by_block {
                    let cut = lines[at].ends_with('…');
                    let whole = lines.len() > at + 5 && lines[at + 5].contains("0x30000000");
                    assert!(cut || whole, "{width}x{height}:\n{screen}");
                }
            }
        }
    }
}
