//! The text answer of `reach SYMBOL` without `--to`: its first lines, and
//! the rows of what SYMBOL reaches by block of memory, which browse shows
//! too.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::tree::{Tree, TreeRows};
use super::{Step, Text, columns, reached_words};
use crate::binary::{Binary, ItemKind, total};
use crate::commands::flat_answer::{FlatAnswer, FlatRow};
use crate::commands::{range_check, text};
use crate::graph::{Node, Unfollowed, Unresolved};
use crate::render::{Align, Table};

impl Text<'_, '_> {
    /// The answer as text, without `--to`: what SYMBOL and what it reaches
    /// take, the bytes in each block of memory when there are several, the
    /// rows by block, and the notes on what elfq can't follow or read. With
    /// `--tree`, a tree takes the place of the rows, and the first line
    /// counts the calls and jumps as the tree shows them.
    pub(super) fn answer(&self) -> FlatAnswer {
        let reach = self.reach;
        let binary = reach.binary;
        let name = self.lead_name;
        if reach.reaches_nothing() {
            // Any calls and jumps elfq can't follow are in SYMBOL, through
            // pointers.
            let found: Vec<Unresolved> = reach.cant_follow.iter().map(|cant| cant.found).collect();
            return FlatAnswer {
                lead: nothing_reached_lines(
                    name,
                    reach.self_use,
                    reach.plt_header,
                    &found,
                    self.all,
                ),
                ..FlatAnswer::default()
            };
        }

        let reached = &reach.steps[1..];
        let items: Vec<Node> = reached
            .iter()
            .map(|step| step.node)
            .filter(|node| matches!(node, Node::Item(_)))
            .collect();
        let imports = reached.len() - items.len();
        let bytes = text::code_and_data(reach.bytes(reach.steps.iter()));
        let mut lead = match &items[..] {
            [] => format!("{name} takes {bytes}"),
            _ => format!(
                "{name} and the {} it reaches take {bytes}",
                reached_words(binary, &items)
            ),
        };
        if imports > 0 {
            lead += &format!(", plus {}", text::plural(imports, "import", "imports"));
        }
        let cant_follow = self.counted_cant_follow();
        if !cant_follow.is_empty() {
            lead += &format!("; elfq can't follow {}", text::calls_and_jumps(cant_follow));
        }

        let pointers: Vec<&Unresolved> = reach
            .cant_follow
            .iter()
            .map(|cant| &cant.listed.unresolved)
            .filter(|unresolved| unresolved.target.is_none())
            .collect();
        let mut notes = Vec::new();
        if !pointers.is_empty() {
            notes.push(self.pointer_note(&pointers));
        }
        if !reach.unreadable.is_empty() {
            notes.push(text::unreadable_text(binary, &reach.unreadable, self.all));
        }
        let (rows, tree) = match self.tree {
            true => (Vec::new(), Some(Tree::new(self, TreeRows::Largest).top())),
            false => (self.rows(&reach.outside()), None),
        };
        FlatAnswer {
            lead: vec![format!("{lead}:")],
            memory: (reach.blocks.len() > 1).then(|| self.memory_table()),
            columns: columns(false),
            rows,
            tree,
            notes,
        }
    }

    /// The bytes of code and data in each block of memory.
    fn memory_table(&self) -> Table {
        let reach = self.reach;
        let mut table = Table::new(&[
            ("MEMORY", Align::Left),
            ("CODE", Align::Right),
            ("DATA", Align::Right),
            ("SECTIONS", Align::Left),
        ]);
        let steps: Vec<&Step> = reach.steps.iter().collect();
        for (block, reached) in reach.blocks.iter().zip(reach.blocks_reached(&steps)) {
            table.row(vec![
                text::range_argument(block.range),
                reached.bytes.code.to_string(),
                reached.bytes.data.to_string(),
                reached.sections.join(" "),
            ]);
        }
        table
    }

    /// SYMBOL's row, then a row for each item it reaches, by block,
    /// functions then data objects, each largest first; then the calls and
    /// jumps to code outside every item in `outside`, and the imports.
    /// Unless the answer shows every row, only the 30 largest items are
    /// shown, and a row such as "9 more data objects in
    /// 0x20000000..0x20000450" sums up the rest of a block's.
    fn rows(&self, outside: &[Unfollowed]) -> Vec<FlatRow> {
        let reach = self.reach;
        let reached = &reach.steps[1..];
        let unfollowed = range_check::group_unfollowed(outside);
        if self.all {
            // Each row but SYMBOL's names the item that uses what it lists,
            // and with every row, those can be most of the file's items.
            self.names.will_name(reached.len() + unfollowed.len());
        }
        let mut rows = vec![self.symbol_row()];
        let largest = self.largest(reached);
        let steps = self.in_row_order(reached.iter().collect());
        let imports = steps.partition_point(|step| matches!(step.node, Node::Item(_)));
        let (items, imports) = steps.split_at(imports);
        for steps in items.chunk_by(|a, b| self.group(a) == self.group(b)) {
            self.add_group(steps, &largest, &mut rows);
        }
        for (group, _) in unfollowed {
            rows.push(self.unfollowed_row(&group));
        }
        if !imports.is_empty() {
            self.add_group(imports, &largest, &mut rows);
        }
        rows
    }

    /// SYMBOL's row.
    pub(super) fn symbol_row(&self) -> FlatRow {
        let reach = self.reach;
        let node = Node::Item(reach.symbol);
        let [address, size] = text::address_and_size(reach.binary, node);
        let cells = vec![address, size, String::new()];
        FlatRow::new(cells, node.name(reach.binary).to_string(), Some(node))
    }

    /// Which group of the table `step`'s row is in: each block's functions,
    /// then its data objects; then the items in no block, which a damaged
    /// file can have; then the imports.
    fn group(&self, step: &Step) -> (usize, bool) {
        let reach = self.reach;
        match step.node {
            Node::Item(index) => {
                let kind = reach.binary.items[index].kind;
                (step.block.unwrap_or(usize::MAX), kind != ItemKind::Function)
            }
            Node::Import(_) => (usize::MAX, true),
        }
    }

    /// `steps` in the order of their rows: by group, each largest first.
    pub(super) fn in_row_order<'s>(&self, mut steps: Vec<&'s Step>) -> Vec<&'s Step> {
        let binary = self.reach.binary;
        steps.sort_by_key(|step| {
            let node = step.node;
            (
                matches!(node, Node::Import(_)),
                self.group(step),
                Reverse(node.size(binary)),
                node.name(binary),
                node,
            )
        });
        steps
    }

    /// The row of a call or jump that elfq can't follow, as `callees`
    /// words it, but for the AT column: to code outside every item, at its
    /// address, or through a pointer. It names no item, and is about the
    /// item that makes it.
    pub(super) fn unfollowed_row(&self, unfollowed: &Unfollowed) -> FlatRow {
        let unresolved = &unfollowed.unresolved;
        let (address, what) = text::cant_follow_target(unresolved);
        let cells = vec![
            address,
            String::new(),
            range_check::unfollowed_how(unfollowed).to_string(),
        ];
        FlatRow::new(cells, String::new(), Some(Node::Item(unresolved.from))).with_rest(format!(
            "{what}, used by {}",
            self.user_name(unresolved.from)
        ))
    }

    /// Adds the rows of `steps`, one group of the table, such as a block's
    /// functions: those in `largest`, and a row that sums up the rest. One
    /// that would be left out on its own is shown too, since its row takes
    /// no more room than the sum's.
    fn add_group(&self, steps: &[&Step], largest: &HashSet<Node>, rows: &mut Vec<FlatRow>) {
        let reach = self.reach;
        let binary = reach.binary;
        let hidden: Vec<&Step> = steps
            .iter()
            .copied()
            .filter(|step| !largest.contains(&step.node))
            .collect();
        for step in steps {
            if hidden.len() < 2 || largest.contains(&step.node) {
                rows.push(self.row(step));
            }
        }
        if hidden.len() < 2 {
            return;
        }
        let first = hidden[0];
        let what = match first.node {
            Node::Item(index) => match binary.items[index].kind {
                ItemKind::Function => "functions",
                ItemKind::Data => "data objects",
            },
            Node::Import(_) => "imports",
        };
        let size = more_size(binary, hidden.iter().map(|step| step.node));
        let place = match first.block {
            Some(block) if reach.blocks.len() > 1 => {
                format!(" in {}", text::range_argument(reach.blocks[block].range))
            }
            _ => String::new(),
        };
        let cells = vec![String::new(), size, String::new()];
        let more = format!("{} more {what}{place}", hidden.len());
        rows.push(FlatRow::new(cells, more, None));
    }

    /// An item's or import's row, with a mark for a function reached
    /// through a pointer, unless it is worded as `callees --to` words it
    /// (see [`Text::like_callees_to`]).
    pub(super) fn row(&self, step: &Step) -> FlatRow {
        let binary = self.reach.binary;
        let [address, size] = text::address_and_size(binary, step.node);
        let used = &step.link().used;
        let mark = if step.through_pointer && !self.like_callees_to() {
            " (through a pointer)"
        } else {
            ""
        };
        let how = text::how_via(binary, step.node, used.kind, used.veneer);
        let cells = vec![address, size, how.to_string()];
        FlatRow::new(cells, step.node.name(binary).to_string(), Some(step.node))
            .with_rest(format!("{mark}, used by {}", self.user_name(used.user)))
    }

    /// The name of `index`, an item that uses another, on that item's row:
    /// the shortest that finds it, or its whole name, as `callees --to`
    /// names it (see [`Text::like_callees_to`]).
    fn user_name(&self, index: usize) -> String {
        if self.like_callees_to() {
            self.reach.binary.items[index].name.clone()
        } else {
            self.names.row_name(index)
        }
    }

    /// The items and imports in `reached` that get rows: the 30 largest,
    /// or every one.
    pub(super) fn largest(&self, reached: &[Step]) -> HashSet<Node> {
        let binary = self.reach.binary;
        let mut nodes: Vec<Node> = reached.iter().map(|step| step.node).collect();
        nodes.sort_by_key(|&node| (Reverse(node.size(binary)), node.name(binary), node));
        nodes.into_iter().take(text::shown(self.all)).collect()
    }

    /// The note on the calls and jumps through pointers in `pointers`, all
    /// in what SYMBOL reaches, since SYMBOL may reach more through them.
    fn pointer_note(&self, pointers: &[&Unresolved]) -> String {
        let reach = self.reach;
        let binary = reach.binary;
        let mut callers: Vec<usize> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        for unresolved in pointers {
            if seen.insert(unresolved.from) {
                callers.push(unresolved.from);
            }
        }
        let names: Vec<&str> = callers
            .iter()
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        let (one, several) = text::call_and_jump_verbs(pointers.iter().copied());
        let through = if pointers.len() == 1 {
            "a pointer"
        } else {
            "pointers"
        };
        let (verb, at) = match callers[..] {
            [_] => {
                let sites: Vec<u64> = pointers.iter().map(|u| u.site).collect();
                (
                    one,
                    format!(", at {}", text::addresses_text(&sites, self.all)),
                )
            }
            _ => (several, String::new()),
        };
        let who = if callers[..] == [reach.symbol] {
            "it"
        } else {
            &binary.items[reach.symbol].name
        };
        format!(
            "{} also {verb} through {through}{at}, which elfq can't follow, so {who} may reach \
             more.",
            text::names_text(&names, self.all)
        )
    }
}

/// The answer when SYMBOL, `name`, reaches nothing but perhaps itself:
/// how it uses itself, if it does ("calls" or "uses"), or else what it is,
/// then its calls and jumps that elfq can't follow, as `callees` says them,
/// with the first few sites, or with `all`, every one. `plt_header` is
/// whether SYMBOL is a PLT's header, whose jump is left out.
fn nothing_reached_lines(
    name: &str,
    self_use: Option<&str>,
    plt_header: bool,
    unresolved: &[Unresolved],
    all: bool,
) -> Vec<String> {
    let pointers = !unresolved.is_empty();
    let mut lines = match self_use {
        Some(verb) if pointers => vec![format!("{name} {verb} itself.")],
        Some(verb) => vec![format!("{name} {verb} only itself.")],
        None if pointers => vec![],
        None if plt_header => {
            vec![format!(
                "{name} is the PLT header, which jumps only to the dynamic loader."
            )]
        }
        None => vec![format!("{name} doesn't call or use anything.")],
    };
    let listed = self_use.is_some();
    lines.extend(text::unresolved_lines(name, unresolved, listed, all));
    lines
}

/// The SIZE cell of a row such as "9 more" that sums up `nodes`: their
/// bytes, or blank when they are all imports, which have no bytes in the
/// file, as the imports' own rows show.
pub(super) fn more_size(binary: &Binary, nodes: impl IntoIterator<Item = Node>) -> String {
    let nodes: Vec<Node> = nodes.into_iter().collect();
    if nodes.iter().all(|node| matches!(node, Node::Import(_))) {
        return String::new();
    }
    total(nodes.iter().map(|node| node.size(binary))).to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::Reach;
    use super::*;
    use crate::cli::Format;
    use crate::commands::symbol::Names;
    use crate::graph::Graph;

    #[test]
    fn what_reaches_nothing_says_how_it_uses_itself() {
        let lines = |self_use, plt_header, unresolved: &[Unresolved]| {
            nothing_reached_lines("f", self_use, plt_header, unresolved, false)
        };
        let pointer = [Unresolved {
            from: 0,
            site: 0x10,
            is_call: true,
            target: None,
        }];
        assert_eq!(lines(None, false, &[]), ["f doesn't call or use anything."]);
        assert_eq!(lines(Some("calls"), false, &[]), ["f calls only itself."]);
        assert_eq!(lines(Some("uses"), false, &[]), ["f uses only itself."]);
        assert_eq!(
            lines(Some("calls"), false, &pointer),
            [
                "f calls itself.",
                "Also 1 call through a pointer, at 0x10, which elfq can't follow.",
            ]
        );
        assert_eq!(
            lines(None, false, &pointer),
            ["f makes 1 call through a pointer, at 0x10, which elfq can't follow."]
        );
        assert_eq!(
            lines(None, true, &[]),
            ["f is the PLT header, which jumps only to the dynamic loader."]
        );
    }

    #[test]
    fn every_row_tells_names_how_many_items_it_names() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&path).unwrap();
        let graph = Graph::build(&binary);
        // Whether wording reach's answer for `symbol`, with every row when
        // `all` is set, tells Names that many items are to be named.
        let names_many = |symbol: &str, all: bool| {
            let names = Names::new(&binary);
            let Node::Item(index) = names.find(symbol).unwrap().node else {
                panic!("{symbol} is an import");
            };
            let reach = Reach::find(&binary, &graph, index, &[]);
            reach.text(&names, symbol, all, Format::Text).answer();
            names.names_many()
        };
        // main reaches 50 of the file's 68 items and 3 imports.
        assert!(names_many("main", true));
        assert!(!names_many("main", false));
        // crc32 reaches only crc_table.
        assert!(!names_many("crc32", true));
    }
}
