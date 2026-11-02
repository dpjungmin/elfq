//! An item's page: a header of facts about it, then its tabs. Tab 1 is a
//! function's code, one line per instruction. A line that calls, jumps to
//! or uses another item ends with a note in the words of the CLI's HOW
//! column, such as `call`, `tail call via PLT → log_msg` or `uses
//! frames_ok`, and links to that item: Enter opens its page. A jump inside
//! the function links to the line it goes to. For a data object, tab 1 is
//! its contents, as far as elfq reads them, with the same notes and links
//! (`item/contents.rs`). The other tabs list what the item calls or uses,
//! as `callees SYMBOL` does (tab 2, `callees.rs`), what calls or uses it,
//! as `callers SYMBOL` does (tab 3, `callers.rs`), and everything it calls
//! or uses at any depth, as `reach SYMBOL` does (tab 4, `reach.rs`), the
//! chains from where the program starts that keep it, as `why SYMBOL` does
//! (tab 5, `why.rs`), and what removing it would save, as `retained
//! SYMBOL` does (tab 6, `retained.rs`). Tabs 2, 4, 5 and 6 are worked out
//! the first time they are shown, since what an item reaches can take a
//! while to find in a large file. Once the session has worked out what
//! keeps each item, the header says what `retained` and `why` say about
//! the item: what removing it would save, the item that keeps it, which u
//! opens, and the shortest chain to it from where the program starts.
//!
//! The parts, each in its own file:
//!
//! - This file: [`Tab`], and [`ItemPage`], which holds the header and the
//!   tabs, and passes each action to the tab on the screen. It also has
//!   `listing`, the lines of a function's code as `arch` prints them.
//! - `header.rs`: the facts that the header gives, and what keeps the
//!   item.
//! - `listing.rs`: tab 1 on the screen: its lines, the cursor over them,
//!   and how they are drawn and followed.
//! - `code.rs`: the lines of a function's code.
//! - `contents.rs`: the rows of a data object's contents, and the strings
//!   that it holds.
//! - `line_notes.rs`: the note and link of each line or row, from what the
//!   graph knows about the item.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use super::printed::Printed;
use super::{Cursor, Go, draw_lines, fit_name, follow, imported, keep_lines, name_fitter, wrap};
use crate::arch;
use crate::binary::{Binary, Item};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::checks::readable_item;
use crate::commands::{retained, text, why};
use crate::graph::Node;
use header::{Keeping, header, used_by_text};
use listing::CodeTab;

mod code;
mod contents;
mod header;
mod line_notes;
mod listing;

pub use listing::PageCode;

/// The tabs of an item's page. Each is numbered as the key that shows it,
/// and named after the command that prints the same answer, so that the
/// numbers stay the same as more tabs come.
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

    /// The tab that the key numbered `number` shows.
    fn of_key(number: u8) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|tab| tab.number_and_name().0 == number)
    }

    /// Its name on the tab line: "1 code", or on a data object's page,
    /// "1 contents".
    fn label(self, data: bool) -> String {
        let (number, name) = match (self, data) {
            (Tab::Code, true) => (1, "contents"),
            _ => self.number_and_name(),
        };
        format!("{number} {name}")
    }
}

pub struct ItemPage {
    /// The item or import.
    node: Node,
    /// Its name in the title line's trail.
    title: String,
    /// The title shortened, and the columns it was shortened to.
    short_title: Option<(usize, String)>,
    /// The first line of the header, before it is wrapped: the item's
    /// name, then what SYMBOL said about it and where it is, as in
    /// "Default_Handler" and " (also named NMI_Handler)  function at …".
    identity: (String, String),
    /// The second line of the header: "used by 2 · uses 3".
    facts: String,
    /// What the header says about what keeps the item, once the session
    /// has worked it out, where `retained` or `why` answers about the item.
    keeping: Option<Keeping>,
    /// The header's lines, and the columns and the most lines they were
    /// made for.
    header: Option<((usize, usize), Vec<Line<'static>>)>,
    /// The tab on the screen. Each page in the history keeps its own, so
    /// that Back shows the tab that was left.
    tab: Tab,
    /// Tab 1, a function's code or a data object's contents, or for an
    /// import, which has neither here, why not, as pressing 1 says.
    code: Result<CodeTab, String>,
    /// The item's index, for tabs 2 and 4, or why `callees` and `reach`
    /// can't read its code, in their words, which pressing 2 or 4 says: it
    /// is an import, or a function whose end elfq doesn't know.
    readable: Result<usize, String>,
    /// The item's index, for tab 6, or why `retained` can't answer about
    /// it, in its words, which pressing 6 and u say: as for `readable`, or
    /// the file is a shared library.
    retainable: Result<usize, String>,
    /// Why `why` can't answer about the item, in its words, if it can't,
    /// which pressing 5 says: the file is a shared library.
    why_refused: Option<String>,
    /// Tab 2, once it has been shown.
    callees: Option<Printed>,
    /// Tab 3.
    callers: Printed,
    /// Tab 4, once it has been shown.
    reach: Option<Printed>,
    /// Tab 5, once it has been shown.
    why: Option<Printed>,
    /// Tab 6, once it has been shown.
    retained: Option<Printed>,
}

impl ItemPage {
    /// The page of `node`, an item or an import, with the line that holds
    /// `at` selected. The header names it as `lead`, which may say what
    /// SYMBOL gave, as in "Default_Handler (also named NMI_Handler)". A
    /// function's page opens on its code, and a data object's on its
    /// contents, when they have rows; otherwise, as for an import, which
    /// has no code here, the page opens on its callers.
    pub fn new(session: &Session, node: Node, at: Option<u64>, lead: &str) -> ItemPage {
        let answer = session.callers(node);
        let used_by = answer.node_count();
        let name = node.name(session.binary);
        let (name, note) = match lead.strip_prefix(name) {
            Some(note) => (name, note),
            None => (lead, ""),
        };
        let (title, (place, facts), code) = match node {
            Node::Item(index) => {
                let title = session.row_name(index);
                let code = Ok(CodeTab::new(session, index, at));
                (title, header(session, index, used_by), code)
            }
            Node::Import(_) => {
                let header = (
                    "imported from a shared library: its code isn't in this file".to_string(),
                    used_by_text(used_by),
                );
                let title = node.name(session.binary).to_string();
                (title, header, Err(imported(session, node)))
            }
        };
        let retainable = retained::refusal(session.binary, node);
        let retainable = retainable.map_err(|error| error.to_string());
        let why_refused = why::refusal(session.binary, node).err();
        let why_refused = why_refused.map(|error| error.to_string());
        let keeping = Keeping::new(session, node, retainable.is_ok(), why_refused.is_none());
        ItemPage {
            node,
            title,
            short_title: None,
            identity: (name.to_string(), format!("{note}  {place}")),
            facts,
            keeping,
            header: None,
            tab: match &code {
                Ok(code) if code.opens_page() => Tab::Code,
                _ => Tab::Callers,
            },
            code,
            readable: readable_item(session.binary, node).map_err(|error| error.to_string()),
            retainable,
            why_refused,
            callees: None,
            callers: super::callers::tab(session, node, answer),
            reach: None,
            why: None,
            retained: None,
        }
    }

    /// The page on `tab`, when it has that tab, as a link from that tab
    /// opens it. But a data object whose contents have no rows stays on
    /// its callers, as its page opens: its tab 1 only says where its bytes
    /// are.
    pub fn on(mut self, session: &Session, tab: Tab) -> ItemPage {
        let shown = match tab {
            Tab::Code => self.code.as_ref().is_ok_and(CodeTab::opens_page),
            _ => self.why_not(tab).is_none(),
        };
        if shown {
            self.show(session, tab);
        }
        self
    }

    /// Adds what keeps the item to the header, once the session has
    /// worked it out, where `retained` or `why` answers about the item.
    pub fn add_keeping(&mut self, session: &Session) {
        if self.keeping.is_none() {
            let (retained, why) = (self.retainable.is_ok(), self.why_refused.is_none());
            self.keeping = Keeping::new(session, self.node, retained, why);
            self.header = None;
        }
    }

    /// Why the item doesn't have `tab`, in the words of the command that
    /// would print it, if it doesn't.
    fn why_not(&self, tab: Tab) -> Option<&str> {
        match tab {
            Tab::Code => self.code.as_ref().err(),
            Tab::Callees | Tab::Reach => self.readable.as_ref().err(),
            Tab::Callers => None,
            Tab::Why => self.why_refused.as_ref(),
            Tab::Retained => self.retainable.as_ref().err(),
        }
        .map(String::as_str)
    }

    /// Puts `tab`, which the item has, on the screen, working out tab 2
    /// the first time. Tabs 4, 5 and 6 can take seconds, so
    /// [`ItemPage::work`] works them out, once the screen says so.
    fn show(&mut self, session: &Session, tab: Tab) {
        if let (Tab::Callees, None, Ok(index)) = (tab, &self.callees, &self.readable) {
            let name = session.lead_name(self.node);
            self.callees = Some(super::callees::tab(session, *index, &name));
        }
        self.tab = tab;
    }

    /// What the tab on the screen has yet to work out, which the status
    /// line says while it does: "working out what main reaches…".
    pub fn waiting(&self) -> Option<String> {
        let title = &self.title;
        match self.tab {
            Tab::Reach if self.reach.is_none() => {
                Some(format!("working out what {title} reaches…"))
            }
            Tab::Why if self.why.is_none() => {
                Some(format!("working out what keeps {title} in the file…"))
            }
            Tab::Retained if self.retained.is_none() => {
                Some(format!("working out what only {title} leads to…"))
            }
            _ => None,
        }
    }

    /// Works out what the tab on the screen has yet to. Tabs 5 and 6 wait
    /// for the session to work out what keeps each item.
    pub fn work(&mut self, session: &Session) {
        let name = session.lead_name(self.node);
        match (self.tab, &self.readable, &self.retainable) {
            (Tab::Reach, Ok(index), _) if self.reach.is_none() => {
                self.reach = Some(super::reach::tab(session, *index, &name));
            }
            (Tab::Why, _, _) if self.why.is_none() => {
                self.why = super::why::tab(session, self.node, &name);
            }
            (Tab::Retained, _, Ok(index)) if self.retained.is_none() => {
                self.retained = super::retained::tab(session, *index, &name);
            }
            _ => {}
        }
    }

    /// The tab on the screen, when it shows a command's answer.
    fn printed(&self) -> Option<&Printed> {
        match self.tab {
            Tab::Code => None,
            Tab::Callees => self.callees.as_ref(),
            Tab::Callers => Some(&self.callers),
            Tab::Reach => self.reach.as_ref(),
            Tab::Why => self.why.as_ref(),
            Tab::Retained => self.retained.as_ref(),
        }
    }

    fn printed_mut(&mut self) -> Option<&mut Printed> {
        match self.tab {
            Tab::Code => None,
            Tab::Callees => self.callees.as_mut(),
            Tab::Callers => Some(&mut self.callers),
            Tab::Reach => self.reach.as_mut(),
            Tab::Why => self.why.as_mut(),
            Tab::Retained => self.retained.as_mut(),
        }
    }

    /// Its name in the title line's trail, shortened to fit `room`
    /// columns: once for each room, since the name of a C++ function can
    /// take thousands of characters.
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
        let has = |tab: &Tab| self.why_not(*tab).is_none();
        Tab::ALL.into_iter().filter(has).collect()
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let top = self.header_lines(width, usize::from(area.height).saturating_sub(3));
        let [head, tabs, body] = Layout::vertical([
            Constraint::Length(top.len() as u16),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        draw_lines(frame, head, top);

        frame.render_widget(self.tab_line(width), tabs);

        if let (Tab::Code, Ok(code)) = (self.tab, &mut self.code) {
            code.draw(frame, body);
        } else if let Some(printed) = self.printed_mut() {
            printed.draw(frame, body);
        }
    }

    /// The tabs it has, with the one on the screen in brackets, and as much
    /// room around the others, so that the names stay where they are as
    /// the brackets move: "[1 code]  2 callees   3 callers   4 reach". On a
    /// screen too narrow for that, `width` columns wide, one space parts
    /// them.
    fn tab_line(&self, width: usize) -> Line<'static> {
        let tabs = self.tabs();
        let data = self.code.as_ref().is_ok_and(CodeTab::is_data);
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

    /// The header, wrapped to `width`, in at most `most` lines. It wraps,
    /// so that nothing is cut, unless the screen is too short for it, with
    /// the tab line and two lines of the tab under it. Then the line with
    /// the chain from where the program starts goes first, since tab 5
    /// shows the chain too; then the item's name is shortened as the title
    /// line's is, but to no less than half a line, and what is still too
    /// long is cut, ending in "…". The chain is one line, which leaves out
    /// names at its start, so that its end shows. They are made once for
    /// each size of screen.
    fn header_lines(&mut self, width: usize, most: usize) -> Vec<Line<'static>> {
        if let Some((size, lines)) = &self.header
            && *size == (width, most)
        {
            return lines.clone();
        }
        let (name, rest) = &self.identity;
        let identity = |name: &str| wrap(&format!("{name}{rest}"), width);
        let mut facts = self.facts.clone();
        let mut chain = None;
        if let Some(keeping) = &self.keeping {
            let more = keeping.facts(&facts, width);
            facts += &more;
            chain = keeping.chain_line(width);
        }
        let facts = wrap(&facts, width);
        let mut first = identity(name);
        if first.len() + facts.len() > most {
            let lines = most.saturating_sub(facts.len()).max(1);
            // The widest shortened name that leaves the line short enough,
            // which is no wider than the lines, but no narrower than half
            // a line, after which the rest of the line is cut instead.
            let fit = name_fitter(name);
            let length = name.chars().count();
            let (mut fits, mut wider) = (0, length.min(lines * width));
            while fits + 1 < wider {
                let middle = (fits + wider) / 2;
                if identity(&fit(middle)).len() <= lines {
                    fits = middle;
                } else {
                    wider = middle;
                }
            }
            let fits = fits.max(length.min(width / 2)).max(1);
            first = keep_lines(identity(&fit(fits)), lines, width);
        }
        let facts = keep_lines(facts, most.saturating_sub(first.len()), width);
        let chain = chain.filter(|_| first.len() + facts.len() < most);
        let first = first.into_iter().map(|line| Line::raw(line).bold());
        let below = facts.into_iter().chain(chain);
        let below = below.map(|line| Line::raw(line).dim());
        let lines: Vec<Line<'static>> = first.chain(below).collect();
        self.header = Some(((width, most), lines.clone()));
        lines
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
        if let (Tab::Code, Ok(code)) = (self.tab, &mut self.code) {
            return code.handle(session, action);
        }
        self.printed_mut()?.handle(session, action)
    }

    /// Opens the page of the item that keeps this one, on the same tab, as
    /// u asks: the closest item that every chain from where the program
    /// starts to it goes through, which `retained`'s rows name after "kept
    /// by". Where there is none, or `retained` refuses the item, it says
    /// why, as the commands say it: for a root, as `why` says what it is,
    /// even where `retained` refuses it, as it does a root whose end elfq
    /// doesn't know.
    fn keeper(&self, session: &Session) -> Go {
        let binary = session.binary;
        let node = self.node;
        if let (None, Some(root)) = (&self.why_refused, session.graph.root(node)) {
            let name = session.lead_name(node);
            return Go::Say(text::root_sentence(binary, &name, root));
        }
        if let Err(why) = &self.retainable {
            return Go::Say(why.clone());
        }
        let Some(keepers) = session.keepers() else {
            return Go::Say("elfq hasn't worked out what keeps each item yet".to_string());
        };
        if !keepers.tree.is_reachable(node) {
            return Go::Say(text::nothing_reaches(&session.lead_name(node)));
        }
        match keepers.tree.parent(node) {
            Some(parent) => follow(session, parent, None, Some(node), self.tab),
            None => Go::Say(why::no_single_keeper(node.name(binary))),
        }
    }

    /// Goes back to the line before the last jump inside the function, if
    /// there was one and the code is on the screen.
    pub fn back(&mut self) -> bool {
        match (self.tab, &mut self.code) {
            (Tab::Code, Ok(code)) => code.back(),
            _ => false,
        }
    }

    /// Where the selected line's link goes, in full: "→ strlen@plt
    /// function at 0x1080, 16 bytes, in .plt.sec".
    pub fn preview(&self, session: &Session) -> Option<String> {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.preview(session),
            _ => self.printed()?.preview(session),
        }
    }

    /// The selected row's value, when its row cuts it to fit a screen
    /// `width` columns wide, as a long string in a data object's contents.
    pub fn value(&self, width: usize) -> Option<&str> {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.value(width),
            _ => None,
        }
    }

    /// Whether some line or row links to another item.
    pub fn has_links(&self) -> bool {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.has_links(),
            _ => self.printed().is_some_and(Printed::has_links),
        }
    }

    /// The cursor over the tab's lines or rows.
    pub fn cursor(&mut self) -> Option<&mut Cursor> {
        if self.tab == Tab::Code {
            return self.code.as_mut().ok().map(|code| &mut code.cursor);
        }
        self.printed_mut().map(Printed::cursor)
    }

    /// What find calls the tab's rows: "line" on a function's code.
    pub fn row_noun(&self) -> &'static str {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) if !code.is_data() => "line",
            _ => "row",
        }
    }

    /// The tab's lines or rows that have `lowercase`, ignoring case.
    pub fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.rows_with(lowercase),
            _ => (self.printed()).map_or_else(Vec::new, |printed| printed.rows_with(lowercase)),
        }
    }

    /// The command that prints what the tab on the screen shows, if one
    /// does: a function's code has none, and under a data object's
    /// contents is the objdump command that prints its bytes.
    pub fn command(&self) -> Option<&str> {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.code.command.as_deref(),
            _ => self.printed().map(Printed::command),
        }
    }
}

/// The lines of `item`'s code, whose bytes are `bytes`, as `arch` prints
/// them, with the data that the file marks there (see
/// [`Binary::data_in_code`]); `None` when elfq can't read the file's
/// code, which browse refuses before it opens (see `Session::new`).
pub(super) fn listing(binary: &Binary, item: &Item, bytes: &[u8]) -> Option<Vec<arch::Line>> {
    // The marks, as offsets into `bytes`.
    let marked: Vec<std::ops::Range<usize>> = binary
        .data_in_code(item.address, item.size)
        .map(|stretch| {
            (stretch.start - item.address) as usize..(stretch.end - item.address) as usize
        })
        .collect();
    let instruction_set = binary.instruction_set()?;
    Some(instruction_set.listing(bytes, item.address, &marked))
}
