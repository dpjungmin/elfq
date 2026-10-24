//! Tab 3 of an item's page: what calls or uses the item, in the rows and
//! notes that `elfq FILE callers SYMBOL` prints, from the same code
//! (`callers::answer`). The tab lists every row, where the command lists
//! the 30 largest callers, and under the rows is that command, ready to
//! paste, with `--all` when the command needs it to list them all. Enter
//! opens the selected caller's page on its callers, with its code at the
//! call.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Row, Table};

use super::item::{ItemPage, Tab};
use super::{Cursor, Go, View, cut, draw_lines, keep_lines, whereabouts, wrap};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::callers::Answer;
use crate::commands::{distinct_nodes, text};
use crate::graph::Node;

/// One row: the cells that `callers` prints, and the caller and the first
/// place where it uses the item, which Enter opens.
struct CallerRow {
    how: &'static str,
    at: String,
    name: String,
    caller: Node,
    site: u64,
}

pub struct Callers {
    rows: Vec<CallerRow>,
    cursor: Cursor,
    /// The widest HOW and AT, worked out once, so that the columns don't
    /// move as the rows scroll.
    widest: (usize, usize),
    /// What `callers` says under its table.
    notes: Vec<String>,
    /// Said in place of the table when nothing uses the item, as the
    /// command says it.
    nothing: Option<String>,
    /// The command that prints the same rows.
    command: String,
}

impl Callers {
    /// The callers of `node`, from `answer`, the answer that `callers
    /// SYMBOL` gives for it.
    pub fn new(session: &Session, node: Node, answer: Answer) -> Callers {
        let binary = session.binary;
        // `callers SYMBOL` lists the rows of the 30 largest callers, unless
        // with --all, and a last row counts the rest.
        let callers = distinct_nodes(&answer.rows).len();
        let cut = text::shown_count(callers, text::shown(false)) < callers;
        let nothing = answer
            .rows
            .is_empty()
            .then(|| format!("Nothing in the file calls or uses {}.", node.name(binary)));
        let rows: Vec<CallerRow> = answer
            .rows
            .into_iter()
            .map(|row| CallerRow {
                how: row.how,
                at: text::at_column(&row.sites),
                name: row.node.name(binary).to_string(),
                caller: row.node,
                site: row.sites[0],
            })
            .collect();
        let widest = |cell: fn(&CallerRow) -> usize| rows.iter().map(cell).max().unwrap_or(0);
        Callers {
            widest: (
                widest(|row| row.how.chars().count()),
                widest(|row| row.at.chars().count()),
            ),
            cursor: Cursor::new(rows.len()),
            rows,
            notes: answer.notes,
            nothing,
            command: session.command("callers", node, cut),
        }
    }

    /// The command that prints the same rows.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Draws the rows, or what the command says when there are none, with
    /// its notes under them. The notes wrap, so that nothing is cut, unless
    /// the screen is too short for them and two lines of the table: its
    /// header and the selected row. Then the last line that fits ends in
    /// "…".
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let notes: Vec<String> = (self.notes.iter())
            .flat_map(|note| wrap(note, width))
            .collect();
        let most = usize::from(area.height).saturating_sub(2);
        let notes: Vec<Line> = (keep_lines(notes, most, width).into_iter())
            .map(Line::raw)
            .collect();
        let [body, foot] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(notes.len() as u16)])
                .areas(area);
        draw_lines(frame, foot, notes);
        if let Some(nothing) = &self.nothing {
            draw_lines(
                frame,
                body,
                wrap(nothing, width).into_iter().map(Line::raw).collect(),
            );
            return;
        }

        // The table's header takes a line.
        let (visible, mut state) = self
            .cursor
            .visible(usize::from(body.height).saturating_sub(1));
        let how_width = self.widest.0.max("HOW".len());
        let at_width = self.widest.1.max("AT".len());
        // What is left for CALLER, after the cursor's "> " and two spaces
        // after each column.
        let room = width.saturating_sub(2 + how_width + 2 + at_width + 2);
        let rows = self.rows[visible]
            .iter()
            .map(|row| Row::new([row.how.to_string(), row.at.clone(), cut(&row.name, room)]));
        let columns = [
            Constraint::Length(how_width as u16),
            Constraint::Length(at_width as u16),
            Constraint::Fill(1),
        ];
        let table = Table::new(rows, columns)
            .column_spacing(2)
            .header(Row::new(["HOW", "AT", "CALLER"]).bold())
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        frame.render_stateful_widget(table, body, &mut state);
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if self.cursor.handle(action) {
            return None;
        }
        match action {
            // Every row links, so Tab goes to the next one.
            Action::NextLink | Action::PreviousLink => {
                let forward = action == Action::NextLink;
                if let Some(row) = self.cursor.next(forward, |_| true) {
                    self.cursor.select(row);
                }
                None
            }
            // The caller's page, on its callers, as a link opens a page on
            // the same tab, so that Enter after Enter walks up the callers.
            // Its code is already at the place where it uses the item.
            Action::Open => {
                let row = self.rows.get(self.cursor.selected)?;
                let name = row.caller.name(session.binary);
                let page = ItemPage::new(session, row.caller, Some(row.site), name);
                Some(Go::Open(View::Item(Box::new(page.on(Tab::Callers)))))
            }
            _ => None,
        }
    }

    /// The selected caller, in full: "→ app_tick  function at 0x300002e4,
    /// 60 bytes, in .text".
    pub fn preview(&self, session: &Session) -> Option<String> {
        let binary = session.binary;
        let row = self.rows.get(self.cursor.selected)?;
        let Node::Item(index) = row.caller else {
            return None;
        };
        let item = &binary.items[index];
        Some(format!(
            "→ {}  {}",
            item.name,
            whereabouts(item, &binary.sections[item.section].name)
        ))
    }

    /// Whether it has rows, each of which links to its caller.
    pub fn has_links(&self) -> bool {
        !self.rows.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::super::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::cli::{Cli, Command, Format};
    use crate::commands::{self, callers};
    use crate::render::{Align, Table};

    /// `line` split into words as a shell splits it, for the words that
    /// `text::shell_quote` writes: plain, or in single quotes, with `'\''`
    /// for a quote.
    fn shell_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word: Option<String> = None;
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                ' ' => words.extend(word.take()),
                '\'' => {
                    let word = word.get_or_insert_with(String::new);
                    word.extend(chars.by_ref().take_while(|&c| c != '\''));
                }
                '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
                c => word.get_or_insert_with(String::new).push(c),
            }
        }
        words.extend(word);
        words
    }

    #[test]
    fn shell_words_undo_shell_quote() {
        for text in [
            "log_msg",
            "log(unsigned int)",
            "vtable for Gain",
            "<&'a T>",
            "",
        ] {
            let line = format!("elfq {} --all", text::shell_quote(text));
            assert_eq!(shell_words(&line), ["elfq", text, "--all"], "{line}");
        }
    }

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

    /// What `callers` prints after its first line for the rows and notes
    /// of `tab`: the table, then the notes, or only the notes when the first
    /// line says that nothing uses the item.
    fn as_printed(tab: &Callers) -> String {
        let mut printed = String::new();
        if tab.nothing.is_none() {
            let mut table = Table::new(&[
                ("HOW", Align::Left),
                ("AT", Align::Left),
                ("CALLER", Align::Left),
            ]);
            for row in &tab.rows {
                table.row(vec![row.how.to_string(), row.at.clone(), row.name.clone()]);
            }
            let mut out = Vec::new();
            table.write(&mut out).unwrap();
            printed += &format!("\n{}", String::from_utf8(out).unwrap());
            if !tab.notes.is_empty() {
                printed.push('\n');
            }
        }
        for note in &tab.notes {
            printed += &format!("{note}\n");
        }
        printed
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
                let tab = Callers::new(&session, node, session.callers(node));
                let name = path.display();
                // The command parses as elfq's arguments, after the shell
                // has split it into words.
                let line = tab.command().strip_prefix("$ ").unwrap();
                let words = shell_words(line);
                assert_eq!(words[0], "elfq", "{line}");
                let (all, cut) = run(&session, &words, node);
                // Its rows and notes are the tab's, with --all.
                let (lead, rest) = all.split_once('\n').unwrap();
                assert_eq!(rest, as_printed(&tab), "{name}: {line}");
                if tab.nothing.is_some() {
                    assert!(lead.starts_with("Nothing in the file calls or uses "));
                }
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
    fn the_command_has_all_exactly_when_the_table_would_cut() {
        // `callers` lists 30 callers and counts the rest in a last row,
        // which never counts one: it lists 31 callers in full. No sample
        // has 31, so the callers of __rust_dealloc are cut to each count.
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/rust-x86_64-nodebug.elf");
        let binary = Binary::load(&path).unwrap();
        let session = Session::new(&path, &binary).unwrap();
        let node = session.names.find("__rust_dealloc").unwrap().node;
        let full = session.callers(node);
        let callers = distinct_nodes(&full.rows);
        for count in 29..=33 {
            let rows = || -> Vec<commands::Row> {
                (full.rows.iter())
                    .filter(|row| callers[..count].contains(&row.node))
                    .map(|row| commands::Row {
                        node: row.node,
                        how: row.how,
                        sites: row.sites.clone(),
                    })
                    .collect()
            };
            let table = |all| {
                let mut out = Vec::new();
                let shown = text::shown(all);
                commands::write_uses_table(&binary, rows(), "CALLER", shown, &mut out).unwrap();
                out
            };
            let answer = Answer {
                rows: rows(),
                notes: Vec::new(),
            };
            let tab = Callers::new(&session, node, answer);
            let all = tab.command().ends_with(" --all");
            assert_eq!(all, table(true) != table(false), "{count} callers");
        }
    }

    #[test]
    fn the_command_quotes_the_file_as_typed() {
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&real).unwrap();
        let typed = Path::new("my samples/it's c-x86_64.elf");
        let session = Session::new(typed, &binary).unwrap();
        let node = session.names.find("log_msg").unwrap().node;
        let tab = Callers::new(&session, node, session.callers(node));
        let words = shell_words(tab.command().strip_prefix("$ ").unwrap());
        let cli = Cli::try_parse_from(&words).unwrap_or_else(|error| panic!("{words:?}: {error}"));
        assert_eq!(cli.file, typed, "{}", tab.command());
    }
}
