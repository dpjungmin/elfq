//! The help page: every key, from the table that binds them, then the
//! words that the pages' notes use. It is a page like the others, so it
//! scrolls with the same keys, and Esc goes back from it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;

use super::wrap;
use crate::cli::HOW_WORDS;
use crate::commands::browse::keys::{self, Action};

/// The words that the notes and the headers use besides the HOW column's,
/// with what they mean.
const WORDS: [(&str, &str); 14] = [
    (
        "via veneer",
        "through a stub that the linker added; the note names where it ends up",
    ),
    (
        "via PLT",
        "through a PLT stub that passes the call on to one of the file's own \
         functions; the note names it",
    ),
    (
        "jump ↓  jump ↑",
        "a branch to a line further down or up the function, or a case of a table of \
         jumps; Enter moves there",
    ),
    (
        "address ↓  address ↑",
        "adr loads the address of a line in this function, such as a table's; Enter \
         moves there",
    ),
    (
        "= 0x028f5c28",
        "the constant that a load from a literal pool reads",
    ),
    (
        "in .bss",
        "the section of an item in another block of memory than this function or data \
         object",
    ),
    (
        ".asciz \"tick\\n\"",
        "a string in a data object, which ends in a 0 byte, quoted as Rust writes strings",
    ),
    ("~12  ?", "an estimated size, and an unknown size"),
    (
        "retained",
        "an item's own size plus the sizes of the items that only it leads to, from \
         where the program starts: what removing it would save, as retained SYMBOL says",
    ),
    ("OWN", "an item's own size, without what it keeps"),
    (
        "kept by",
        "the closest item that every path to this one goes through, from where the \
         program starts; u opens it",
    ),
    (
        "(through a pointer)",
        "nothing calls the function directly, so it counts for the code or data that \
         holds its address",
    ),
    (
        "block",
        "a block of memory, as the overview's MEMORY table lists them: sections next to \
         one another, such as those in flash or in ITCM",
    ),
    (
        "load image",
        "the copy of an item that the file stores in one block, for startup code to copy \
         to the block where the program uses it",
    ),
];

/// The columns that a key or a word takes, after two spaces, before what
/// it means.
const NAME_WIDTH: usize = 20;
/// How far in what a key or a word means starts, and its wrapped lines:
/// after the two spaces, the key or word, and a space.
const INDENT: usize = 2 + NAME_WIDTH + 1;

/// A line of the help page.
#[derive(Clone)]
enum HelpLine {
    /// "Keys" or "Words", in bold.
    Heading(&'static str),
    /// A key or a word and what it means, or a wrapped part of what it
    /// means.
    Entry(String),
}

impl HelpLine {
    /// The line of `name`, a key or a word, which says `what` it means.
    fn entry(name: &str, what: &str) -> HelpLine {
        HelpLine::Entry(format!("  {name:<NAME_WIDTH$} {what}"))
    }

    /// Its text, as the page shows it.
    #[cfg(test)]
    fn text(&self) -> &str {
        match self {
            HelpLine::Heading(text) => text,
            HelpLine::Entry(text) => text,
        }
    }
}

pub(in crate::commands::browse) struct Help {
    /// Its lines before they are wrapped.
    lines: Vec<HelpLine>,
    /// The first line on the screen.
    offset: usize,
    /// How many lines the screen held when last drawn.
    page: usize,
    /// The last line that can be first on the screen.
    last: usize,
}

impl Help {
    pub(in crate::commands::browse) fn new() -> Help {
        let mut lines = vec![HelpLine::Heading("Keys")];
        lines.extend(keys::bindings().map(|binding| HelpLine::entry(binding.label, binding.what)));
        lines.push(HelpLine::Heading("Words"));
        lines.extend(
            HOW_WORDS
                .iter()
                .chain(&WORDS)
                .map(|(word, what)| HelpLine::entry(word, what)),
        );
        Help {
            lines,
            offset: 0,
            page: 1,
            last: 0,
        }
    }

    /// The lines wrapped to `width`, each wrapped part lined up under what
    /// the key or word means.
    fn wrapped(&self, width: usize) -> Vec<HelpLine> {
        let mut wrapped = Vec::new();
        for help_line in &self.lines {
            // The headings are shorter than INDENT, so they fit wherever
            // the lines wrap.
            let HelpLine::Entry(line) = help_line else {
                wrapped.push(help_line.clone());
                continue;
            };
            if line.chars().count() <= width || width <= INDENT {
                wrapped.push(help_line.clone());
                continue;
            }
            let lead: String = line.chars().take(INDENT).collect();
            let rest: String = line.chars().skip(INDENT).collect();
            for (index, part) in wrap(&rest, width - INDENT).into_iter().enumerate() {
                let lead = if index == 0 {
                    lead.clone()
                } else {
                    " ".repeat(INDENT)
                };
                wrapped.push(HelpLine::Entry(lead + &part));
            }
        }
        wrapped
    }

    pub(super) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let lines = self.wrapped(usize::from(area.width));
        let height = usize::from(area.height);
        self.page = height.max(1);
        self.last = lines.len().saturating_sub(height);
        self.offset = self.offset.min(self.last);
        for (row, line) in lines.iter().skip(self.offset).take(height).enumerate() {
            let line = match line {
                HelpLine::Heading(text) => Line::raw(*text).bold(),
                HelpLine::Entry(text) => Line::raw(text.as_str()),
            };
            let y = area.y + row as u16;
            frame.render_widget(line, Rect::new(area.x, y, area.width, 1));
        }
    }

    /// Scrolls as `action` says.
    pub(super) fn handle(&mut self, action: Action) {
        let at = self.offset;
        self.offset = match action {
            Action::Up => at.saturating_sub(1),
            Action::Down => (at + 1).min(self.last),
            Action::PageUp => at.saturating_sub(self.page),
            Action::PageDown => (at + self.page).min(self.last),
            Action::First => 0,
            Action::Last => self.last,
            _ => at,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_has_every_row_of_the_table_of_keys() {
        let help = Help::new();
        for binding in keys::BINDINGS {
            let line = format!("  {:<NAME_WIDTH$} {}", binding.label, binding.what);
            let found = help.lines.iter().any(|help_line| help_line.text() == line);
            // Ctrl-Z works only on Unix, and is in the help only there.
            let suspends = binding.label == "Ctrl-Z";
            assert_eq!(found, !suspends || cfg!(unix), "{line}");
        }
    }

    #[test]
    fn long_lines_wrap_under_what_they_say() {
        let help = Help::new();
        let wrapped = help.wrapped(60);
        let at = wrapped
            .iter()
            .position(|line| line.text().starts_with("  address taken"))
            .unwrap();
        assert_eq!(
            wrapped[at..at + 3]
                .iter()
                .map(HelpLine::text)
                .collect::<Vec<_>>(),
            [
                "  address taken        code loads a function's address, as",
                "                       when passing a function pointer, or",
                "                       an import's",
            ]
        );
    }
}
