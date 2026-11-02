//! An item's page: a header of facts about it, then its tabs.
//!
//! Tab 1 is a function's code, one line per instruction. A line that calls,
//! jumps to or uses another item ends with a note in the words of the CLI's
//! HOW column, such as `call`, `tail call via PLT → log_msg` or `uses
//! frames_ok`, and links to that item: Enter opens its page. A jump inside
//! the function links to the line it goes to. For a data object, tab 1 is
//! its contents, as far as elfq reads them, with the same notes and links
//! (`item/contents.rs`). The other tabs list what the item calls or uses,
//! as `callees SYMBOL` does (tab 2, `callees.rs`), what calls or uses it,
//! as `callers SYMBOL` does (tab 3, `callers.rs`), and everything it calls
//! or uses at any depth, as `reach SYMBOL` does (tab 4, `reach.rs`), the
//! chains from where the program starts that keep it, as `why SYMBOL` does
//! (tab 5, `why.rs`), and what removing it would save, as `retained SYMBOL`
//! does (tab 6, `retained.rs`). Tabs 2, 4, 5 and 6 are worked out the first
//! time they are shown, since what an item reaches can take a while to find
//! in a large file. Once the session has worked out what keeps each item,
//! the header says what `retained` and `why` say about the item: what
//! removing it would save, the item that keeps it, which u opens, and the
//! shortest chain to it from where the program starts.
//!
//! The parts, each in its own file:
//!
//! - This file: [`Tab`], and [`ItemPage`], which holds the header and a
//!   [`Pane`] for each tab, and passes each action to the tab on the
//!   screen. A tab that is worked out after the page opens waits in its
//!   pane as a [`Later`], and one that can take seconds as a [`SlowTab`].
//! - `header.rs`: `Header`, what the header says: the facts about the
//!   item, and what keeps it.
//! - `listing.rs`: `Listing`, what tab 1 lists, and `ListingPane`, which
//!   draws it, moves over its lines and follows their links.
//! - `code.rs`: `function_listing`, the lines of a function's code.
//! - `contents.rs`: `data_listing`, the rows of a data object's contents,
//!   and the strings that it holds.
//! - `line_notes.rs`: `LineNotes`, the note and link of each line or row,
//!   from what the graph knows about the item.

use anyhow::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use super::printed::Printed;
use super::{Cursor, Go, draw_lines, fit_name, follow, imported};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::checks::readable_item;
use crate::commands::{retained, text, why};
use crate::graph::Node;
use header::Header;
use listing::ListingPane;

mod code;
mod contents;
mod header;
mod line_notes;
mod listing;

pub use listing::Listing;

/// What u says where the session hasn't worked out what keeps each item.
///
/// So would a tab 5 or 6 worked out too early (see [`Later::work_out`]).
const NO_KEEPERS: &str = "elfq hasn't worked out what keeps each item yet";

/// The tabs of an item's page.
///
/// Each is numbered as the key that shows it, and named after the command
/// that prints the same answer, so that the numbers stay the same as more
/// tabs come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 1: a function's code, or a data object's contents.
    Code,
    /// 2: what the item calls or uses.
    Callees,
    /// 3: what calls or uses the item.
    Callers,
    /// 4: what the item calls or uses, at any depth.
    Reach,
    /// 5: the chains from where the program starts that keep the item.
    Why,
    /// 6: what removing the item would save.
    Retained,
}

impl Tab {
    /// Every tab, in the order of their numbers.
    const ALL: [Tab; 6] = [
        Tab::Code,
        Tab::Callees,
        Tab::Callers,
        Tab::Reach,
        Tab::Why,
        Tab::Retained,
    ];

    /// The number of the key that shows it, and its name.
    fn number_and_name(self) -> (u8, &'static str) {
        match self {
            Tab::Code => (1, "code"),
            Tab::Callees => (2, "callees"),
            Tab::Callers => (3, "callers"),
            Tab::Reach => (4, "reach"),
            Tab::Why => (5, "why"),
            Tab::Retained => (6, "retained"),
        }
    }

    /// Its place in [`Tab::ALL`], and in an item's page's panes.
    fn index(self) -> usize {
        let (number, _) = self.number_and_name();
        usize::from(number) - 1
    }

    /// The tab that the key numbered `number` shows.
    fn of_key(number: u8) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|tab| tab.number_and_name().0 == number)
    }

    /// Its name on the tab line, such as "1 code".
    ///
    /// On a data object's page, tab 1 is "1 contents".
    fn label(self, data: bool) -> String {
        let (number, name) = match (self, data) {
            (Tab::Code, true) => (1, "contents"),
            _ => self.number_and_name(),
        };
        format!("{number} {name}")
    }
}

/// What one tab of an item's page holds.
///
/// Each tab has one: why the item can't have the tab, what working it
/// out takes until it is, or what it shows.
enum Pane {
    /// Why the item can't have the tab, as pressing its key says.
    ///
    /// It is in the words of the command that would print the tab.
    Refused(String),
    /// A tab that is worked out after the page opens, until it is.
    NotYet(Later),
    /// Tab 1: a function's code, or a data object's contents.
    Listing(ListingPane),
    /// Tabs 2 to 6: a command's answer.
    Printed(Box<Printed>),
}

/// A tab that is worked out after its page opens, with what that takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Later {
    /// Tab 2, the first time it is shown.
    ///
    /// It lists what the item at this index calls or uses.
    Callees(usize),
    /// Tab 4, 5 or 6, once the screen says so.
    Slow(SlowTab),
}

/// A tab that can take seconds to work out in a large file.
///
/// It is worked out only once the screen says so, the first time it is
/// shown (see `App::work`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowTab {
    /// Tab 4: what the item at this index calls or uses, at any depth.
    Reach(usize),
    /// Tab 5: why this item or import is in the file.
    Why(Node),
    /// Tab 6: what removing the item at this index would save.
    Retained(usize),
}

impl SlowTab {
    /// The item or import whose page it is on.
    pub fn node(self) -> Node {
        match self {
            SlowTab::Reach(index) | SlowTab::Retained(index) => Node::Item(index),
            SlowTab::Why(node) => node,
        }
    }
}

impl Later {
    /// The item or import whose page it is on.
    fn node(self) -> Node {
        match self {
            Later::Callees(index) => Node::Item(index),
            Later::Slow(tab) => tab.node(),
        }
    }

    /// Works it out: the answer of the command that prints the tab.
    ///
    /// Tabs 5 and 6 need what keeps each item, which `App::work` has the
    /// session work out before any tab. Were it missing, the tab would be
    /// refused with NO_KEEPERS rather than left waiting, so that the event
    /// loop still gets to the next key.
    fn work_out(self, session: &Session) -> Pane {
        let name = session.lead_name(self.node());
        let printed = match (self, session.keepers()) {
            (Later::Callees(index), _) => super::callees::tab(session, index, &name),
            (Later::Slow(SlowTab::Reach(index)), _) => super::reach::tab(session, index, &name),
            (Later::Slow(SlowTab::Why(node)), Some(keepers)) => {
                super::why::tab(session, keepers, node, &name)
            }
            (Later::Slow(SlowTab::Retained(index)), Some(keepers)) => {
                super::retained::tab(session, keepers, index, &name)
            }
            (Later::Slow(SlowTab::Why(_) | SlowTab::Retained(_)), None) => {
                return Pane::Refused(NO_KEEPERS.to_string());
            }
        };
        Pane::Printed(Box::new(printed))
    }
}

/// An item's page: its header, and a pane for each tab.
pub struct ItemPage {
    /// The item or import.
    node: Node,
    /// Its name in the title line's trail.
    title: String,
    /// The title shortened, and the columns it was shortened to.
    short_title: Option<(usize, String)>,
    /// The lines above the tabs.
    ///
    /// They say where the item is, its facts, and what keeps it.
    header: Header,
    /// The tab on the screen.
    ///
    /// Each page in the history keeps its own, so that Back shows the tab
    /// that was left.
    tab: Tab,
    /// What each tab holds, at the tab's index (see [`Tab::index`]).
    panes: [Pane; Tab::ALL.len()],
}

impl ItemPage {
    /// The page of `node`, with the line that holds `at` selected.
    ///
    /// `node` is an item or an import. The header names it as
    /// `header_name`, which may say what SYMBOL gave, as in
    /// "Default_Handler (also named NMI_Handler)" (see the names in
    /// `session.rs`). A function's page opens on its code, and a data
    /// object's on its contents, when they have rows. Otherwise, as for an
    /// import, which has no code here, the page opens on its callers.
    pub fn new(session: &Session, node: Node, at: Option<u64>, header_name: &str) -> ItemPage {
        let binary = session.binary;
        let answer = session.callers(node);
        let (title, listing) = match node {
            Node::Item(index) => {
                let listing = ListingPane::new(session, index, at);
                (session.row_name(index), Pane::Listing(listing))
            }
            Node::Import(_) => {
                let title = node.name(binary).to_string();
                (title, Pane::Refused(imported(session, node)))
            }
        };
        let header = Header::new(session, node, header_name, answer.node_count());
        // A tab worked out later, or why the item can't have it, as the
        // command that prints the tab says it.
        let pane = |tab: Result<Later>| match tab {
            Ok(later) => Pane::NotYet(later),
            Err(error) => Pane::Refused(error.to_string()),
        };
        let slow = |tab: Result<SlowTab>| pane(tab.map(Later::Slow));
        let mut page = ItemPage {
            node,
            title,
            short_title: None,
            header,
            tab: Tab::Callers,
            // In the order of the tabs' numbers.
            panes: [
                listing,
                pane(readable_item(binary, node).map(Later::Callees)),
                Pane::Printed(Box::new(super::callers::tab(session, node, answer))),
                slow(readable_item(binary, node).map(SlowTab::Reach)),
                slow(why::refusal(binary, node).map(|()| SlowTab::Why(node))),
                slow(retained::refusal(binary, node).map(SlowTab::Retained)),
            ],
        };
        if page.opens_on(Tab::Code) {
            page.tab = Tab::Code;
        }
        page.add_keeping(session);
        page
    }

    /// The page on `tab`, if it can open on it.
    ///
    /// A link from that tab opens the page so (see [`ItemPage::opens_on`]).
    pub fn on(mut self, session: &Session, tab: Tab) -> ItemPage {
        if self.opens_on(tab) {
            self.show(session, tab);
        }
        self
    }

    /// Whether the page can open on `tab`.
    ///
    /// It can on a tab the item has, but on tab 1 only when it has rows: a
    /// data object whose contents have none stays on its callers, as its
    /// page opens, since its tab 1 only says where its bytes are.
    fn opens_on(&self, tab: Tab) -> bool {
        match self.pane_of(tab) {
            Pane::Listing(listing) => listing.opens_page(),
            Pane::Refused(_) => false,
            Pane::NotYet(_) | Pane::Printed(_) => true,
        }
    }

    /// Adds what keeps the item to the header, once it is worked out.
    ///
    /// It adds it only where `retained` or `why` answers about the item.
    pub fn add_keeping(&mut self, session: &Session) {
        let (retained, why) = (self.has(Tab::Retained), self.has(Tab::Why));
        self.header.add_keeping(session, retained, why);
    }

    /// What `tab` holds.
    fn pane_of(&self, tab: Tab) -> &Pane {
        &self.panes[tab.index()]
    }

    /// What the tab on the screen holds.
    fn pane(&self) -> &Pane {
        self.pane_of(self.tab)
    }

    /// What the tab on the screen holds, to change.
    fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.tab.index()]
    }

    /// Whether the item has `tab`.
    fn has(&self, tab: Tab) -> bool {
        self.why_not(tab).is_none()
    }

    /// Why the item doesn't have `tab`, if it doesn't.
    ///
    /// It is in the words of the command that would print the tab.
    fn why_not(&self, tab: Tab) -> Option<&str> {
        match self.pane_of(tab) {
            Pane::Refused(why) => Some(why),
            Pane::NotYet(_) | Pane::Listing(_) | Pane::Printed(_) => None,
        }
    }

    /// Puts `tab`, which the item has, on the screen.
    ///
    /// It works out tab 2 the first time. Tabs 4, 5 and 6 can take seconds,
    /// so [`ItemPage::work_out`] works them out, once the screen says so.
    fn show(&mut self, session: &Session, tab: Tab) {
        if let Pane::NotYet(later) = self.panes[tab.index()] {
            match later {
                Later::Callees(_) => self.panes[tab.index()] = later.work_out(session),
                // Slow tabs wait for `App::work`.
                Later::Slow(_) => {}
            }
        }
        self.tab = tab;
    }

    /// The tab on the screen, if it waits for the screen to say so.
    ///
    /// That is tab 4, 5 or 6, the first time it is shown.
    pub fn pending(&self) -> Option<SlowTab> {
        match self.pane() {
            Pane::NotYet(Later::Slow(tab)) => Some(*tab),
            Pane::NotYet(Later::Callees(_))
            | Pane::Refused(_)
            | Pane::Listing(_)
            | Pane::Printed(_) => None,
        }
    }

    /// What the tab on the screen has yet to work out, if anything.
    ///
    /// The status line says it while it does: "working out what main
    /// reaches…".
    pub fn waiting(&self) -> Option<String> {
        let title = &self.title;
        Some(match self.pending()? {
            SlowTab::Reach(_) => format!("working out what {title} reaches…"),
            SlowTab::Why(_) => format!("working out what keeps {title} in the file…"),
            SlowTab::Retained(_) => format!("working out what only {title} leads to…"),
        })
    }

    /// Works out the tab on the screen, if it waits to be.
    ///
    /// That is the tab that [`ItemPage::pending`] gives.
    pub fn work_out(&mut self, session: &Session) {
        if let Some(tab) = self.pending() {
            *self.pane_mut() = Later::Slow(tab).work_out(session);
        }
    }

    /// Its name in the title line's trail, shortened to fit `room` columns.
    ///
    /// It is shortened once for each room, since the name of a C++ function
    /// can take thousands of characters.
    pub fn title(&mut self, room: usize) -> &str {
        if self
            .short_title
            .as_ref()
            .is_none_or(|&(shortened, _)| shortened != room)
        {
            self.short_title = Some((room, fit_name(&self.title, room)));
        }
        self.short_title.as_ref().map_or("", |(_, title)| title)
    }

    /// The numbers of the tabs it has, in order.
    pub fn tab_numbers(&self) -> Vec<u8> {
        let tabs = self.tabs().into_iter();
        tabs.map(|tab| tab.number_and_name().0).collect()
    }

    /// The tabs it has, in the order of their numbers.
    fn tabs(&self) -> Vec<Tab> {
        Tab::ALL.into_iter().filter(|&tab| self.has(tab)).collect()
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let top = self
            .header
            .lines(width, usize::from(area.height).saturating_sub(3));
        let [head, tabs, body] = Layout::vertical([
            Constraint::Length(top.len() as u16),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        draw_lines(frame, head, top);

        frame.render_widget(self.tab_line(width), tabs);

        match self.pane_mut() {
            Pane::Listing(listing) => listing.draw(frame, body),
            Pane::Printed(printed) => printed.draw(frame, body),
            // Nothing yet, while the status line says what is worked out.
            Pane::NotYet(_) | Pane::Refused(_) => {}
        }
    }

    /// The tabs it has, with the one on the screen in brackets.
    ///
    /// The others have as much room around them, so that the names stay
    /// where they are as the brackets move:
    /// "[1 code]  2 callees   3 callers   4 reach". On a screen too narrow
    /// for that, `width` columns wide, one space parts them.
    fn tab_line(&self, width: usize) -> Line<'static> {
        let tabs = self.tabs();
        let data = matches!(self.pane_of(Tab::Code), Pane::Listing(listing) if listing.is_data());
        let label = |tab: Tab| tab.label(data);
        let wide: usize = tabs.iter().map(|&tab| label(tab).chars().count() + 3).sum();
        let pad = if wide <= width { " " } else { "" };
        let mut line = Vec::new();
        for tab in tabs {
            if tab == self.tab {
                line.push(Span::raw(format!("[{}]", label(tab))).bold());
            } else {
                line.push(Span::raw(format!("{pad}{}{pad}", label(tab))));
            }
            line.push(Span::raw(" "));
        }
        Line::from(line)
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if action == Action::Keeper {
            return Some(self.keeper(session));
        }
        if let Action::Tab(number) = action {
            let tab = Tab::of_key(number)?;
            // A tab that the item doesn't have says why.
            if let Some(why) = self.why_not(tab) {
                return Some(Go::Say(why.to_string()));
            }
            self.show(session, tab);
            return None;
        }
        match self.pane_mut() {
            Pane::Listing(listing) => listing.handle(session, action),
            Pane::Printed(printed) => printed.handle(session, action),
            Pane::NotYet(_) | Pane::Refused(_) => None,
        }
    }

    /// Opens the page of the item that keeps this one, as u asks.
    ///
    /// That is the closest item that every chain from where the program
    /// starts to it goes through, which `retained`'s rows name after "kept
    /// by". Its page opens on the same tab. Where there is none, or
    /// `retained` refuses the item, it says why, as the commands say it.
    /// For a root, it says what `why` says the root is, even where
    /// `retained` refuses it, as it does a root whose end elfq doesn't know.
    fn keeper(&self, session: &Session) -> Go {
        let binary = session.binary;
        let node = self.node;
        if self.has(Tab::Why)
            && let Some(root) = session.graph.root(node)
        {
            let name = session.lead_name(node);
            return Go::Say(text::root_sentence(binary, &name, root));
        }
        if let Some(why) = self.why_not(Tab::Retained) {
            return Go::Say(why.to_string());
        }
        let Some(keepers) = session.keepers() else {
            return Go::Say(NO_KEEPERS.to_string());
        };
        if !keepers.tree.is_reachable(node) {
            return Go::Say(text::nothing_reaches(&session.lead_name(node)));
        }
        match keepers.tree.parent(node) {
            Some(parent) => follow(session, parent, None, Some(node), self.tab),
            None => Go::Say(why::no_single_keeper(node.name(binary))),
        }
    }

    /// Goes back to the line before the last jump inside the function.
    ///
    /// It says whether it did: it does only if there was a jump and the
    /// code is on the screen.
    pub fn back(&mut self) -> bool {
        match self.pane_mut() {
            Pane::Listing(listing) => listing.back(),
            Pane::Printed(_) | Pane::NotYet(_) | Pane::Refused(_) => false,
        }
    }

    /// Where the selected line's link goes, in full.
    ///
    /// An example: "→ strlen@plt function at 0x1080, 16 bytes, in
    /// .plt.sec".
    pub fn preview(&self, session: &Session) -> Option<String> {
        match self.pane() {
            Pane::Listing(listing) => listing.preview(session),
            Pane::Printed(printed) => printed.preview(session),
            Pane::NotYet(_) | Pane::Refused(_) => None,
        }
    }

    /// The selected row's value, when its row cuts it.
    ///
    /// That happens to a long string in a data object's contents, on a
    /// screen `width` columns wide.
    pub fn value(&self, width: usize) -> Option<&str> {
        match self.pane() {
            Pane::Listing(listing) => listing.value(width),
            Pane::Printed(_) | Pane::NotYet(_) | Pane::Refused(_) => None,
        }
    }

    /// Whether some line or row links to another item.
    pub fn has_links(&self) -> bool {
        match self.pane() {
            Pane::Listing(listing) => listing.has_links(),
            Pane::Printed(printed) => printed.has_links(),
            Pane::NotYet(_) | Pane::Refused(_) => false,
        }
    }

    /// The cursor over the tab's lines or rows.
    pub fn cursor(&mut self) -> Option<&mut Cursor> {
        match self.pane_mut() {
            Pane::Listing(listing) => Some(listing.cursor()),
            Pane::Printed(printed) => Some(printed.cursor()),
            Pane::NotYet(_) | Pane::Refused(_) => None,
        }
    }

    /// What find calls the tab's rows: "line" on a function's code.
    pub fn row_noun(&self) -> &'static str {
        match self.pane() {
            Pane::Listing(listing) if !listing.is_data() => "line",
            Pane::Listing(_) | Pane::Printed(_) | Pane::NotYet(_) | Pane::Refused(_) => "row",
        }
    }

    /// The tab's lines or rows that have `lowercase`, ignoring case.
    pub fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        match self.pane() {
            Pane::Listing(listing) => listing.rows_with(lowercase),
            Pane::Printed(printed) => printed.rows_with(lowercase),
            Pane::NotYet(_) | Pane::Refused(_) => Vec::new(),
        }
    }

    /// The command that prints what the tab on the screen shows, if any.
    ///
    /// A function's code has none. Under a data object's contents is the
    /// objdump command that prints its bytes.
    pub fn command(&self) -> Option<&str> {
        match self.pane() {
            Pane::Listing(listing) => listing.command(),
            Pane::Printed(printed) => Some(printed.command()),
            Pane::NotYet(_) | Pane::Refused(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::commands::browse::keys::BINDINGS;

    #[test]
    fn the_help_names_every_tab_with_its_key() {
        let shows_a_tab =
            |&(_, _, action): &(KeyCode, KeyModifiers, Action)| matches!(action, Action::Tab(_));
        let binding = (BINDINGS.iter())
            .find(|binding| binding.keys.iter().any(shows_a_tab))
            .unwrap();
        // "tabs: 1 code or contents, 2 callees, …", in the order of
        // Tab::ALL, which has every tab once.
        let named: Vec<&str> = (binding.what.strip_prefix("tabs: ").unwrap())
            .split(", ")
            .collect();
        assert_eq!(named.len(), Tab::ALL.len(), "{}", binding.what);
        assert_eq!(binding.keys.len(), Tab::ALL.len());
        for (tab, named) in Tab::ALL.into_iter().zip(named) {
            // Tab 1 is "1 code or contents", as a data object's page names
            // it.
            let (on_code, on_data) = (tab.label(false), tab.label(true));
            let words = if on_data == on_code {
                on_code
            } else {
                let (_, name) = on_data.split_once(' ').unwrap();
                format!("{on_code} or {name}")
            };
            assert_eq!(named, words, "{tab:?}");
            let (number, _) = tab.number_and_name();
            let key = number.to_string();
            assert!(
                binding.label.split(' ').any(|label| label == key),
                "{tab:?}"
            );
            let code = KeyCode::Char(char::from(b'0' + number));
            let binds = (code, KeyModifiers::NONE, Action::Tab(number));
            assert!(binding.keys.contains(&binds), "{tab:?}");
            assert_eq!(Tab::of_key(number), Some(tab));
            assert_eq!(Tab::ALL[tab.index()], tab);
        }
    }
}
