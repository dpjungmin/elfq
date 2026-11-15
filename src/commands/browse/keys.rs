//! Which key does what. One table, [`BINDINGS`], says which keys make each
//! [`Action`], and the help page and the hint line are printed from it too,
//! so that what they say can't drift from what the keys do. The go-to box
//! and find's prompt read their keys through [`typing`], which takes the
//! keys that move from the same table, and their hint lines are beside it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Stop;

/// What the user asked for. Every page gets the same actions, so a key
/// means the same everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
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
    /// Open the page of the item that keeps this one.
    Keeper,
    /// Type text to find in the page's rows.
    Find,
    /// The next or previous row that has the text found last.
    FindNext,
    FindPrevious,
    /// Type a name or an address to go to.
    GoTo,
    /// The help page.
    Help,
    /// Give the terminal back to the shell for now, as Ctrl-Z does.
    Suspend,
    Quit,
}

/// One row of the table of keys, and a line of the help page.
pub(super) struct Binding {
    /// The keys, as the help page writes them.
    pub(super) label: &'static str,
    /// What they do, as the help page says it.
    pub(super) what: &'static str,
    /// What the hint line says about them, if anything.
    hint: Option<Hint>,
    /// Each key, with what it does.
    pub(super) keys: &'static [(KeyCode, KeyModifiers, Action)],
}

impl Binding {
    /// Whether one of its keys does `action`.
    fn does(&self, action: Action) -> bool {
        self.keys.iter().any(|&(_, _, does)| does == action)
    }
}

/// What the hint line says about a row of the table of keys.
#[derive(Clone, Copy)]
struct Hint {
    /// The words, such as "g go to". The tab keys' hint follows the numbers
    /// of the tabs that the page has: "1 3 tabs".
    text: &'static str,
    /// When it goes from a screen too narrow for every hint: the highest
    /// goes first, and 0 is always shown.
    drop_order: u8,
}

const NONE: KeyModifiers = KeyModifiers::NONE;
const CONTROL: KeyModifiers = KeyModifiers::CONTROL;

/// Every key the browser takes, in the order the hint line names them.
pub(super) const BINDINGS: &[Binding] = &[
    Binding {
        label: "↑ ↓  k j",
        what: "move",
        hint: Some(Hint {
            text: "↑↓ move",
            drop_order: 0,
        }),
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
        hint: Some(Hint {
            text: "enter follow",
            drop_order: 0,
        }),
        keys: &[
            (KeyCode::Enter, NONE, Action::Open),
            (KeyCode::Right, NONE, Action::Open),
            (KeyCode::Char('l'), NONE, Action::Open),
        ],
    },
    Binding {
        label: "Esc ← h Bksp Ctrl-O",
        what: "back to where you were",
        hint: Some(Hint {
            text: "esc back",
            drop_order: 0,
        }),
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
        hint: Some(Hint {
            text: "tab next link",
            drop_order: 2,
        }),
        keys: &[
            (KeyCode::Tab, NONE, Action::NextLink),
            (KeyCode::BackTab, NONE, Action::PreviousLink),
        ],
    },
    Binding {
        label: "1 2 3 4 5 6",
        what: "tabs: 1 code or contents, 2 callees, 3 callers, 4 reach, 5 why, 6 retained",
        hint: Some(Hint {
            text: "tabs",
            drop_order: 4,
        }),
        keys: &[
            (KeyCode::Char('1'), NONE, Action::Tab(1)),
            (KeyCode::Char('2'), NONE, Action::Tab(2)),
            (KeyCode::Char('3'), NONE, Action::Tab(3)),
            (KeyCode::Char('4'), NONE, Action::Tab(4)),
            (KeyCode::Char('5'), NONE, Action::Tab(5)),
            (KeyCode::Char('6'), NONE, Action::Tab(6)),
        ],
    },
    Binding {
        label: "u",
        what: "the item that keeps this one, which its header names after \"kept by\"",
        hint: None,
        keys: &[(KeyCode::Char('u'), NONE, Action::Keeper)],
    },
    Binding {
        label: "/",
        what: "find text in the page's rows, ignoring case",
        hint: Some(Hint {
            text: "/ find",
            drop_order: 3,
        }),
        keys: &[(KeyCode::Char('/'), NONE, Action::Find)],
    },
    Binding {
        label: "n N",
        what: "the next or previous row that has the text found",
        hint: None,
        keys: &[
            (KeyCode::Char('n'), NONE, Action::FindNext),
            (KeyCode::Char('N'), NONE, Action::FindPrevious),
        ],
    },
    Binding {
        label: "g",
        what: "go to a function, data object or import, by name or address",
        hint: Some(Hint {
            text: "g go to",
            drop_order: 1,
        }),
        keys: &[(KeyCode::Char('g'), NONE, Action::GoTo)],
    },
    Binding {
        label: "?",
        what: "this help",
        hint: Some(Hint {
            text: "? help",
            drop_order: 0,
        }),
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
        hint: Some(Hint {
            text: "q quit",
            drop_order: 0,
        }),
        keys: &[
            (KeyCode::Char('q'), NONE, Action::Quit),
            (KeyCode::Char('c'), CONTROL, Action::Quit),
        ],
    },
];

/// The bindings that work here: Ctrl-Z only on Unix, where a shell can
/// stop a program and continue it.
pub(super) fn bindings() -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(|binding| cfg!(unix) || !binding.does(Action::Suspend))
}

/// The action for `key`, if it has one. Shift doesn't count, since it is
/// how some keys are typed, such as `?` and Shift-Tab.
pub(super) fn action(key: KeyEvent) -> Option<Action> {
    let modifiers = key.modifiers - KeyModifiers::SHIFT;
    bindings()
        .flat_map(|binding| binding.keys)
        .find(|&&(code, with, _)| code == key.code && with == modifiers)
        .map(|&(_, _, action)| action)
}

/// What the page has that the hint line names keys for.
pub(super) struct PageKeys {
    /// Whether it has lines that link to another item, which Tab moves
    /// between.
    pub(super) links: bool,
    /// The numbers of its tabs, whose keys show them.
    pub(super) tabs: Vec<u8>,
    /// Whether it has rows, which find looks in.
    pub(super) rows: bool,
}

/// The hint line for a screen `width` columns wide, on a page that has
/// what `page` says: the hints that fit, leaving out those of the highest
/// drop order first. Tab's hint is there only when the page has lines
/// that link, the tab keys' only when it has two tabs or more, and find's
/// only when it has rows.
pub(super) fn hints(width: usize, page: &PageKeys) -> String {
    let numbers: Vec<String> = page.tabs.iter().map(u8::to_string).collect();
    let hints: Vec<(String, u8)> = bindings()
        .filter(|binding| page.links || !binding.does(Action::NextLink))
        .filter(|binding| page.rows || !binding.does(Action::Find))
        .filter(|binding| page.tabs.len() > 1 || !binding.does(Action::Tab(1)))
        .filter_map(|binding| {
            let Hint { text, drop_order } = binding.hint?;
            Some(match binding.does(Action::Tab(1)) {
                true => (format!("{} {text}", numbers.join(" ")), drop_order),
                false => (text.to_string(), drop_order),
            })
        })
        .collect();
    let most = hints.iter().map(|&(_, order)| order).max().unwrap_or(0);
    let mut line = String::new();
    // The highest drop order kept, from every hint down to those always
    // shown.
    for kept in (0..=most).rev() {
        let shown: Vec<&str> = hints
            .iter()
            .filter(|&&(_, order)| order <= kept)
            .map(|(hint, _)| hint.as_str())
            .collect();
        line = shown.join("  ");
        if line.chars().count() <= width {
            break;
        }
    }
    line
}

/// What a key does while a prompt is open (see [`typing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Typing {
    /// Types the character.
    Char(char),
    /// Takes the last character back, as Backspace does.
    Erase,
    /// Closes the prompt, as Esc does.
    Cancel,
    /// Does what the prompt is for, as Enter does: the go-to box opens the
    /// selected row's page, and find looks for the text.
    Submit,
    /// Moves the go-to box's cursor. It holds only the six moves that
    /// [`typing`] gives: [`Action::Up`], [`Action::Down`],
    /// [`Action::PageUp`], [`Action::PageDown`], [`Action::First`] and
    /// [`Action::Last`].
    Move(Action),
    /// Stops the browser, as Ctrl-C and Ctrl-Z do everywhere.
    Stop(Stop),
}

/// The hint line while the go-to box is open. The prompts' hint lines
/// are written out, rather than printed from [`BINDINGS`], since in a
/// prompt Enter and Esc do what [`Typing`] says, not what their rows of
/// the table say.
pub(super) const GO_TO_HINT: &str = "↑↓ move  enter open  esc cancel";
/// The hint line while find's prompt is open, written out as
/// [`GO_TO_HINT`] is.
pub(super) const FIND_HINT: &str = "enter find  esc cancel";

/// What `key` does while a prompt is open, if anything. Of the keys that
/// stop the browser, only Ctrl-C and Ctrl-Z do, since q types; other Ctrl
/// and Alt characters type nothing. Esc cancels, Enter submits and
/// Backspace erases, and the keys of [`BINDINGS`] that move and aren't
/// characters move: Up, Down, PageUp, PageDown, Home and End. Those nine
/// are matched on the key alone, so Ctrl-Up moves as Up does, while k, j
/// and Space type.
pub(super) fn typing(key: KeyEvent) -> Option<Typing> {
    if key.modifiers.contains(CONTROL) {
        match action(key) {
            Some(Action::Quit) => return Some(Typing::Stop(Stop::Quit)),
            Some(Action::Suspend) => return Some(Typing::Stop(Stop::Suspend)),
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => Some(Typing::Cancel),
        KeyCode::Enter => Some(Typing::Submit),
        KeyCode::Backspace => Some(Typing::Erase),
        KeyCode::Char(c) if !key.modifiers.intersects(CONTROL | KeyModifiers::ALT) => {
            Some(Typing::Char(c))
        }
        KeyCode::Char(_) => None,
        code => {
            let moves = |action: Action| {
                matches!(
                    action,
                    Action::Up
                        | Action::Down
                        | Action::PageUp
                        | Action::PageDown
                        | Action::First
                        | Action::Last
                )
            };
            bindings()
                .flat_map(|binding| binding.keys)
                .find(|&&(bound, _, action)| bound == code && moves(action))
                .map(|&(_, _, action)| Typing::Move(action))
        }
    }
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
        let page = PageKeys {
            links: true,
            tabs: vec![1, 2, 3, 4],
            rows: true,
        };
        let all = "↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  \
                   ? help  q quit";
        assert_eq!(hints(100, &page), all);
        // The keys of the tabs that the page has.
        let two_tabs = PageKeys {
            tabs: vec![1, 3],
            ..page
        };
        assert_eq!(
            hints(100, &two_tabs),
            all.replace("1 2 3 4 tabs", "1 3 tabs")
        );
        let without_tabs =
            "↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit";
        assert_eq!(hints(all.chars().count() - 1, &page), without_tabs);
        let no_tabs = PageKeys {
            tabs: Vec::new(),
            ..page
        };
        assert_eq!(hints(100, &no_tabs), without_tabs);
        // One tab needs no key.
        let one_tab = PageKeys {
            tabs: vec![3],
            ..page
        };
        assert_eq!(hints(100, &one_tab), without_tabs);
        let without_find =
            "↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit";
        assert_eq!(hints(without_tabs.chars().count() - 1, &page), without_find);
        let no_rows = PageKeys {
            tabs: Vec::new(),
            rows: false,
            ..page
        };
        assert_eq!(hints(100, &no_rows), without_find);
        let without_tab = "↑↓ move  enter follow  esc back  g go to  ? help  q quit";
        assert_eq!(hints(without_find.chars().count() - 1, &page), without_tab);
        let no_links = PageKeys {
            links: false,
            tabs: Vec::new(),
            rows: false,
        };
        assert_eq!(hints(100, &no_links), without_tab);
        let least = "↑↓ move  enter follow  esc back  ? help  q quit";
        assert_eq!(hints(without_tab.chars().count() - 1, &page), least);
    }

    #[test]
    fn only_ctrl_c_and_ctrl_z_work_while_typing() {
        let typed = |code, modifiers| typing(KeyEvent::new(code, modifiers));
        assert_eq!(
            typed(KeyCode::Char('c'), CONTROL),
            Some(Typing::Stop(Stop::Quit))
        );
        assert_eq!(
            typed(KeyCode::Char('z'), CONTROL),
            Some(Typing::Stop(Stop::Suspend))
        );
        assert_eq!(typed(KeyCode::Char('q'), NONE), Some(Typing::Char('q')));
        assert_eq!(
            typed(KeyCode::Char('Q'), KeyModifiers::SHIFT),
            Some(Typing::Char('Q'))
        );
        // Other Ctrl and Alt keys type nothing.
        assert_eq!(typed(KeyCode::Char('o'), CONTROL), None);
        assert_eq!(typed(KeyCode::Char('x'), KeyModifiers::ALT), None);
        assert_eq!(typed(KeyCode::Char('k'), KeyModifiers::ALT), None);
    }

    #[test]
    fn while_typing_only_the_keys_that_arent_characters_move() {
        let typed = |code, modifiers| typing(KeyEvent::new(code, modifiers));
        let moves = [
            (KeyCode::Up, Action::Up),
            (KeyCode::Down, Action::Down),
            (KeyCode::PageUp, Action::PageUp),
            (KeyCode::PageDown, Action::PageDown),
            (KeyCode::Home, Action::First),
            (KeyCode::End, Action::Last),
        ];
        // With Ctrl, Alt or Shift too.
        for modifiers in [NONE, CONTROL, KeyModifiers::ALT, KeyModifiers::SHIFT] {
            for (code, action) in moves {
                let moved = Some(Typing::Move(action));
                assert_eq!(typed(code, modifiers), moved, "{code:?} {modifiers:?}");
            }
            assert_eq!(typed(KeyCode::Esc, modifiers), Some(Typing::Cancel));
            assert_eq!(typed(KeyCode::Enter, modifiers), Some(Typing::Submit));
            assert_eq!(typed(KeyCode::Backspace, modifiers), Some(Typing::Erase));
        }
        // The keys that move, open or go back and are characters type: k,
        // j, Space, l and h. Left and Right, which open and go back, do
        // nothing, and so do Tab and Shift-Tab.
        for c in ['k', 'j', ' ', 'l', 'h'] {
            assert_eq!(typed(KeyCode::Char(c), NONE), Some(Typing::Char(c)));
        }
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::BackTab,
        ] {
            assert_eq!(typed(code, NONE), None, "{code:?}");
        }
    }
}
