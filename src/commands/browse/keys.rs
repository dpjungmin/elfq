//! Which key does what. One table, [`BINDINGS`], says which keys make each
//! [`Action`], and the hint line is printed from it too, so that what it
//! says can't drift from what the keys do.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the user asked for. Every page gets the same actions, so a key
/// means the same everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    /// Follow the selected line's link.
    Open,
    /// Go back to where the last Open came from.
    Back,
    /// Undo a Back.
    Forward,
    /// The next or previous line that uses another item.
    NextLink,
    PreviousLink,
    /// Give the terminal back to the shell for now, as Ctrl-Z does.
    Suspend,
    Quit,
}

/// One row of the table of keys.
pub struct Binding {
    /// What the hint line says about them, if anything, and how much that
    /// matters: 0 is always shown, and the highest goes first when the
    /// screen is too narrow.
    pub hint: Option<(&'static str, u8)>,
    /// Each key, with what it does.
    pub keys: &'static [(KeyCode, KeyModifiers, Action)],
}

impl Binding {
    /// Whether one of its keys does `action`.
    fn does(&self, action: Action) -> bool {
        self.keys.iter().any(|&(_, _, does)| does == action)
    }
}

const NONE: KeyModifiers = KeyModifiers::NONE;
const CONTROL: KeyModifiers = KeyModifiers::CONTROL;

/// Every key the browser takes, in the order the hint line names them.
pub const BINDINGS: &[Binding] = &[
    Binding {
        hint: Some(("↑↓ move", 0)),
        keys: &[
            (KeyCode::Up, NONE, Action::Up),
            (KeyCode::Char('k'), NONE, Action::Up),
            (KeyCode::Down, NONE, Action::Down),
            (KeyCode::Char('j'), NONE, Action::Down),
        ],
    },
    Binding {
        hint: None,
        keys: &[
            (KeyCode::PageUp, NONE, Action::PageUp),
            (KeyCode::PageDown, NONE, Action::PageDown),
            (KeyCode::Char(' '), NONE, Action::PageDown),
        ],
    },
    Binding {
        hint: None,
        keys: &[
            (KeyCode::Home, NONE, Action::First),
            (KeyCode::End, NONE, Action::Last),
        ],
    },
    Binding {
        hint: Some(("enter follow", 0)),
        keys: &[
            (KeyCode::Enter, NONE, Action::Open),
            (KeyCode::Right, NONE, Action::Open),
            (KeyCode::Char('l'), NONE, Action::Open),
        ],
    },
    Binding {
        hint: Some(("esc back", 0)),
        keys: &[
            (KeyCode::Esc, NONE, Action::Back),
            (KeyCode::Left, NONE, Action::Back),
            (KeyCode::Char('h'), NONE, Action::Back),
            (KeyCode::Backspace, NONE, Action::Back),
            (KeyCode::Char('o'), CONTROL, Action::Back),
        ],
    },
    Binding {
        hint: None,
        keys: &[(KeyCode::Char(']'), NONE, Action::Forward)],
    },
    Binding {
        hint: Some(("tab next link", 2)),
        keys: &[
            (KeyCode::Tab, NONE, Action::NextLink),
            (KeyCode::BackTab, NONE, Action::PreviousLink),
        ],
    },
    Binding {
        hint: None,
        keys: &[(KeyCode::Char('z'), CONTROL, Action::Suspend)],
    },
    Binding {
        hint: Some(("q quit", 0)),
        keys: &[
            (KeyCode::Char('q'), NONE, Action::Quit),
            (KeyCode::Char('c'), CONTROL, Action::Quit),
        ],
    },
];

/// The bindings that work here: Ctrl-Z only on Unix, where a shell can
/// stop a program and continue it.
pub fn bindings() -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(|binding| cfg!(unix) || !binding.does(Action::Suspend))
}

/// The action for `key`, if it has one. Shift doesn't count, since it is
/// how some keys are typed, such as `?` and Shift-Tab.
pub fn action(key: KeyEvent) -> Option<Action> {
    let modifiers = key.modifiers - KeyModifiers::SHIFT;
    bindings()
        .flat_map(|binding| binding.keys)
        .find(|&&(code, with, _)| code == key.code && with == modifiers)
        .map(|&(_, _, action)| action)
}

/// The hint line for a screen `width` columns wide: the hints that fit,
/// leaving out the least needed first. `links` says whether the page has
/// lines that link, for Tab.
pub fn hints(width: usize, links: bool) -> String {
    let hints: Vec<(&str, u8)> = bindings()
        .filter(|binding| links || !binding.does(Action::NextLink))
        .filter_map(|binding| binding.hint)
        .collect();
    let most = hints.iter().map(|&(_, level)| level).max().unwrap_or(0);
    let mut line = String::new();
    for level in (0..=most).rev() {
        let shown: Vec<&str> = hints
            .iter()
            .filter(|&&(_, needed)| needed <= level)
            .map(|&(hint, _)| hint)
            .collect();
        line = shown.join("  ");
        if line.chars().count() <= width {
            break;
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn no_key_does_two_things() {
        let mut seen = HashSet::new();
        for &(code, modifiers, _) in BINDINGS.iter().flat_map(|binding| binding.keys) {
            assert!(seen.insert((code, modifiers)), "{code:?} {modifiers:?}");
        }
    }

    #[test]
    fn shift_doesnt_count() {
        let key = |code, modifiers| action(KeyEvent::new(code, modifiers));
        assert_eq!(
            key(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(Action::PreviousLink)
        );
        assert_eq!(key(KeyCode::Char('o'), CONTROL), Some(Action::Back));
        assert_eq!(key(KeyCode::Char('o'), NONE), None);
        assert_eq!(key(KeyCode::Char('q'), KeyModifiers::ALT), None);
    }

    #[test]
    fn the_hint_line_leaves_out_what_is_needed_least() {
        let all = "↑↓ move  enter follow  esc back  tab next link  q quit";
        assert_eq!(hints(100, true), all);
        assert_eq!(
            hints(all.chars().count() - 1, true),
            "↑↓ move  enter follow  esc back  q quit"
        );
        assert_eq!(hints(100, false), "↑↓ move  enter follow  esc back  q quit");
    }
}
