//! Which key does what. One table, [`BINDINGS`], says which keys make each
//! [`Action`], and the help page and the hint line are printed from it too,
//! so that what they say can't drift from what the keys do.

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
    /// Show the page's tab with this number, as `3` shows the callers.
    Tab(u8),
    /// Type a name or an address to go to.
    GoTo,
    /// The help page.
    Help,
    /// Give the terminal back to the shell for now, as Ctrl-Z does.
    Suspend,
    Quit,
}

/// One row of the table of keys, and a line of the help page.
pub struct Binding {
    /// The keys, as the help page writes them.
    pub label: &'static str,
    /// What they do, as the help page says it.
    pub what: &'static str,
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
        label: "↑ ↓  k j",
        what: "move",
        hint: Some(("↑↓ move", 0)),
        keys: &[
            (KeyCode::Up, NONE, Action::Up),
            (KeyCode::Char('k'), NONE, Action::Up),
            (KeyCode::Down, NONE, Action::Down),
            (KeyCode::Char('j'), NONE, Action::Down),
        ],
    },
    Binding {
        label: "PgUp PgDn Space",
        what: "a page up or down",
        hint: None,
        keys: &[
            (KeyCode::PageUp, NONE, Action::PageUp),
            (KeyCode::PageDown, NONE, Action::PageDown),
            (KeyCode::Char(' '), NONE, Action::PageDown),
        ],
    },
    Binding {
        label: "Home End",
        what: "the first or last line",
        hint: None,
        keys: &[
            (KeyCode::Home, NONE, Action::First),
            (KeyCode::End, NONE, Action::Last),
        ],
    },
    Binding {
        label: "Enter → l",
        what: "follow the line's link, or open the row",
        hint: Some(("enter follow", 0)),
        keys: &[
            (KeyCode::Enter, NONE, Action::Open),
            (KeyCode::Right, NONE, Action::Open),
            (KeyCode::Char('l'), NONE, Action::Open),
        ],
    },
    Binding {
        label: "Esc ← h Bksp Ctrl-O",
        what: "back to where you were",
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
        label: "]",
        what: "forward again",
        hint: None,
        keys: &[(KeyCode::Char(']'), NONE, Action::Forward)],
    },
    Binding {
        label: "Tab Shift-Tab",
        what: "the next or previous line that uses another item",
        hint: Some(("tab next link", 2)),
        keys: &[
            (KeyCode::Tab, NONE, Action::NextLink),
            (KeyCode::BackTab, NONE, Action::PreviousLink),
        ],
    },
    Binding {
        label: "1 3",
        what: "tabs: 1 code, 3 callers",
        hint: Some(("1 3 tabs", 3)),
        keys: &[
            (KeyCode::Char('1'), NONE, Action::Tab(1)),
            (KeyCode::Char('3'), NONE, Action::Tab(3)),
        ],
    },
    Binding {
        label: "g",
        what: "go to a function or data object, by name or address",
        hint: Some(("g go to", 1)),
        keys: &[(KeyCode::Char('g'), NONE, Action::GoTo)],
    },
    Binding {
        label: "?",
        what: "this help",
        hint: Some(("? help", 0)),
        keys: &[(KeyCode::Char('?'), NONE, Action::Help)],
    },
    Binding {
        label: "Ctrl-Z",
        what: "back to the shell; fg returns",
        hint: None,
        keys: &[(KeyCode::Char('z'), CONTROL, Action::Suspend)],
    },
    Binding {
        label: "q Ctrl-C",
        what: "quit",
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
/// lines that link, for Tab, and `tabs` whether it has tabs, for 1 and 3.
pub fn hints(width: usize, links: bool, tabs: bool) -> String {
    let hints: Vec<(&str, u8)> = bindings()
        .filter(|binding| links || !binding.does(Action::NextLink))
        .filter(|binding| tabs || !binding.does(Action::Tab(1)))
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

/// The action for `key` while text is typed, as into the go-to box: only
/// Ctrl-C and Ctrl-Z, since other keys type.
pub fn while_typing(key: KeyEvent) -> Option<Action> {
    if !key.modifiers.contains(CONTROL) {
        return None;
    }
    action(key).filter(|&action| matches!(action, Action::Quit | Action::Suspend))
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
        assert_eq!(
            key(KeyCode::Char('?'), KeyModifiers::SHIFT),
            Some(Action::Help)
        );
    }

    #[test]
    fn the_hint_line_leaves_out_what_is_needed_least() {
        let all =
            "↑↓ move  enter follow  esc back  tab next link  1 3 tabs  g go to  ? help  q quit";
        assert_eq!(hints(100, true, true), all);
        let without_tabs =
            "↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit";
        assert_eq!(hints(all.chars().count() - 1, true, true), without_tabs);
        assert_eq!(hints(100, true, false), without_tabs);
        let without_tab = "↑↓ move  enter follow  esc back  g go to  ? help  q quit";
        assert_eq!(
            hints(without_tabs.chars().count() - 1, true, true),
            without_tab
        );
        assert_eq!(hints(100, false, false), without_tab);
        let least = "↑↓ move  enter follow  esc back  ? help  q quit";
        assert_eq!(hints(without_tab.chars().count() - 1, true, true), least);
    }

    #[test]
    fn only_ctrl_c_and_ctrl_z_work_while_typing() {
        let typing = |code, modifiers| while_typing(KeyEvent::new(code, modifiers));
        assert_eq!(typing(KeyCode::Char('c'), CONTROL), Some(Action::Quit));
        assert_eq!(typing(KeyCode::Char('z'), CONTROL), Some(Action::Suspend));
        assert_eq!(typing(KeyCode::Char('q'), NONE), None);
        assert_eq!(typing(KeyCode::Char('o'), CONTROL), None);
    }
}
