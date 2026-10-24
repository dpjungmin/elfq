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

/// The words that the notes use besides the HOW column's, with what they
/// mean.
const WORDS: [(&str, &str); 7] = [
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
        "the section of an item in another block of memory than this function",
    ),
    ("~12  ?", "an estimated size, and an unknown size"),
];

/// How far in what a key or a word means starts, and its wrapped lines.
const INDENT: usize = 23;

pub struct Help {
    /// Its lines before they are wrapped, each with whether it is a
    /// heading.
    lines: Vec<(String, bool)>,
    /// The first line on the screen.
    offset: usize,
    /// How many lines the screen held when last drawn.
    page: usize,
    /// The last line that can be first on the screen.
    last: usize,
}

impl Help {
    pub fn new() -> Help {
        let entry = |name: &str, what: &str| (format!("  {name:<20} {what}"), false);
        let mut lines = vec![("Keys".to_string(), true)];
        lines.extend(keys::bindings().map(|binding| entry(binding.label, binding.what)));
        lines.push(("Words".to_string(), true));
        lines.extend(
            HOW_WORDS
                .iter()
                .chain(&WORDS)
                .map(|(word, what)| entry(word, what)),
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
    fn wrapped(&self, width: usize) -> Vec<(String, bool)> {
        let mut wrapped = Vec::new();
        for (line, heading) in &self.lines {
            if line.chars().count() <= width || width <= INDENT {
                wrapped.push((line.clone(), *heading));
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
                wrapped.push((lead + &part, false));
            }
        }
        wrapped
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let lines = self.wrapped(usize::from(area.width));
        let height = usize::from(area.height);
        self.page = height.max(1);
        self.last = lines.len().saturating_sub(height);
        self.offset = self.offset.min(self.last);
        for (row, (text, heading)) in lines.iter().skip(self.offset).take(height).enumerate() {
            let line = Line::raw(text.as_str());
            let line = if *heading { line.bold() } else { line };
            let y = area.y + row as u16;
            frame.render_widget(line, Rect::new(area.x, y, area.width, 1));
        }
    }

    /// Scrolls as `action` says.
    pub fn handle(&mut self, action: Action) {
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
            let line = format!("  {:<20} {}", binding.label, binding.what);
            let found = help.lines.iter().any(|(text, _)| *text == line);
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
            .position(|(text, _)| text.starts_with("  address taken"))
            .unwrap();
        assert_eq!(
            wrapped[at..at + 3]
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>(),
            [
                "  address taken        code loads a function's address, as",
                "                       when passing a function pointer, or",
                "                       an import's",
            ]
        );
    }
}
