//! An item's page: a header of facts about it, then its tabs. Tab 1 is a
//! function's code, one line per instruction. A line that calls, jumps to
//! or uses another item ends with a note in the words of the CLI's HOW
//! column, such as `call`, `tail call via PLT → log_msg` or `uses
//! frames_ok`, and links to that item: Enter opens its page. A jump inside
//! the function links to the line it goes to. The other tabs list what the
//! item calls or uses, as `callees SYMBOL` does (tab 2, `callees.rs`), what
//! calls or uses it, as `callers SYMBOL` does (tab 3, `callers.rs`), and
//! everything it calls or uses at any depth, as `reach SYMBOL` does (tab 4,
//! `reach.rs`). Tabs 2 and 4 are worked out the first time they are shown,
//! since what an item reaches can take a while to find in a large file.
//! Once the session has worked out what keeps each item, the header says
//! what `retained` and `why` say about the item: what removing it would
//! save, the item that keeps it, which u opens, and the shortest chain to
//! it from where the program starts.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::rc::Rc;

use object::{Architecture, Endian};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table};

use super::printed::Printed;
use super::{
    Cursor, Go, cut, cut_left, draw_lines, fit_name, follow, has_ignoring_case, imported,
    keep_lines, link_preview, name_fitter, whereabouts, wrap,
};
use crate::arch::{self, CodeReferenceKind, Refers};
use crate::binary::{Binary, Item, ItemKind, Source};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::{distinct_nodes, readable_item, retained, text, why};
use crate::graph::{Node, Reference, Unresolved, VeneerTarget, item_starting_at};

/// The most columns that the instructions take, and the fewest they give
/// way to when the notes after them need room.
const INSTRUCTION_WIDTH: (usize, usize) = (48, 24);
/// The fewest columns that a note is cut to before its section goes.
const NOTE_WIDTH: usize = 16;
/// The fewest columns that the header shortens the name of the item that
/// keeps this one to, to keep its facts on one line.
const KEEPER_WIDTH: usize = 16;

/// The tabs of an item's page. Each is numbered as the key that shows it,
/// and named after the command that prints the same answer, so that the
/// numbers stay the same as more tabs come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 1: a function's code.
    Code,
    /// 2: what the item calls or uses.
    Callees,
    /// 3: what calls or uses the item.
    Callers,
    /// 4: what the item calls or uses, at any depth.
    Reach,
}

impl Tab {
    /// Every tab, in the order of their numbers.
    const ALL: [Tab; 4] = [Tab::Code, Tab::Callees, Tab::Callers, Tab::Reach];

    /// The number of the key that shows it, and its name.
    fn number_and_name(self) -> (u8, &'static str) {
        match self {
            Tab::Code => (1, "code"),
            Tab::Callees => (2, "callees"),
            Tab::Callers => (3, "callers"),
            Tab::Reach => (4, "reach"),
        }
    }

    /// The tab that the key numbered `number` shows.
    fn of_key(number: u8) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|tab| tab.number_and_name().0 == number)
    }

    /// Its name on the tab line: "1 code".
    fn label(self) -> String {
        let (number, name) = self.number_and_name();
        format!("{number} {name}")
    }
}

/// Where a line's link goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    /// An item or import, the address in it that the line uses, and the
    /// veneer or PLT stub that the code goes through to get there.
    Node {
        node: Node,
        at: u64,
        via: Option<usize>,
    },
    /// Another line of this function.
    Here(u64),
}

/// One line of code.
struct CodeLine {
    address: u64,
    /// The instruction, with the name of the item it calls or jumps to in
    /// place of its address: `call    strlen@plt`. Empty where elfq doesn't
    /// print the code.
    instruction: String,
    /// What the line does with another item, or where it jumps.
    note: String,
    /// The section of the item that the link goes to, when that is in
    /// another block of memory than this function, as in firmware that
    /// calls from ITCM into flash. It is drawn in yellow.
    elsewhere: Option<String>,
    link: Option<Link>,
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
    /// Tab 1, or for an item that has no code, why not, as pressing 1
    /// says.
    code: Result<CodeTab, String>,
    /// The item's index, for tabs 2 and 4, or why `callees` and `reach`
    /// can't read its code, in their words, which pressing 2 or 4 says: it
    /// is an import, or a function whose end elfq doesn't know.
    readable: Result<usize, String>,
    /// The item's index, or why `retained` can't answer about it, in its
    /// words, which u says: as for `readable`, or the file is a shared
    /// library.
    retained: Result<usize, String>,
    /// Why `why` can't answer about the item, in its words, if it can't:
    /// the file is a shared library.
    why_refused: Option<String>,
    /// Tab 2, once it has been shown.
    callees: Option<Printed>,
    /// Tab 3.
    callers: Printed,
    /// Tab 4, once it has been shown.
    reach: Option<Printed>,
}

impl ItemPage {
    /// The page of `node`, an item or an import, with the line that holds
    /// `at` selected. The header names it as `lead`, which may say what
    /// SYMBOL gave, as in "Default_Handler (also named NMI_Handler)". A
    /// function's page opens on its code, and the page of a data object or
    /// an import, which have none, on their callers.
    pub fn new(session: &Session, node: Node, at: Option<u64>, lead: &str) -> ItemPage {
        let answer = session.callers(node);
        let used_by = distinct_nodes(&answer.rows).len();
        let name = node.name(session.binary);
        let (name, note) = match lead.strip_prefix(name) {
            Some(note) => (name, note),
            None => (lead, ""),
        };
        let (title, (place, facts), code) = match node {
            Node::Item(index) => {
                let title = session.row_name(index);
                let code = match session.binary.items[index].kind {
                    ItemKind::Function => Ok(CodeTab::new(session, index, at)),
                    ItemKind::Data => Err(format!("{title} is a data object: it has no code")),
                };
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
        let retained = retained::refusal(session.binary, node).map_err(|error| error.to_string());
        let why_refused = why::refusal(session.binary, node).err();
        let why_refused = why_refused.map(|error| error.to_string());
        let keeping = Keeping::new(session, node, retained.is_ok(), why_refused.is_none());
        ItemPage {
            node,
            title,
            short_title: None,
            identity: (name.to_string(), format!("{note}  {place}")),
            facts,
            keeping,
            header: None,
            tab: if code.is_ok() {
                Tab::Code
            } else {
                Tab::Callers
            },
            code,
            readable: readable_item(session.binary, node).map_err(|error| error.to_string()),
            retained,
            why_refused,
            callees: None,
            callers: super::callers::tab(session, node, answer),
            reach: None,
        }
    }

    /// The page on `tab`, when it has that tab, as a link from that tab
    /// opens it.
    pub fn on(mut self, session: &Session, tab: Tab) -> ItemPage {
        if self.why_not(tab).is_none() {
            self.show(session, tab);
        }
        self
    }

    /// Adds what keeps the item to the header, once the session has
    /// worked it out, where `retained` or `why` answers about the item.
    pub fn add_keeping(&mut self, session: &Session) {
        if self.keeping.is_none() {
            let (retained, why) = (self.retained.is_ok(), self.why_refused.is_none());
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
        }
        .map(String::as_str)
    }

    /// Puts `tab`, which the item has, on the screen, working out tab 2
    /// the first time. Tab 4 can take seconds, so [`ItemPage::work`] works
    /// it out, once the screen says so.
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
        let reach = self.tab == Tab::Reach && self.reach.is_none();
        reach.then(|| format!("working out what {} reaches…", self.title))
    }

    /// Works out what the tab on the screen has yet to.
    pub fn work(&mut self, session: &Session) {
        if let (Tab::Reach, None, Ok(index)) = (self.tab, &self.reach, &self.readable) {
            let name = session.lead_name(self.node);
            self.reach = Some(super::reach::tab(session, *index, &name));
        }
    }

    /// The tab on the screen, when it shows a command's answer.
    fn printed(&self) -> Option<&Printed> {
        match self.tab {
            Tab::Code => None,
            Tab::Callees => self.callees.as_ref(),
            Tab::Callers => Some(&self.callers),
            Tab::Reach => self.reach.as_ref(),
        }
    }

    fn printed_mut(&mut self) -> Option<&mut Printed> {
        match self.tab {
            Tab::Code => None,
            Tab::Callees => self.callees.as_mut(),
            Tab::Callers => Some(&mut self.callers),
            Tab::Reach => self.reach.as_mut(),
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

        // The tabs it has, with the one on the screen in brackets:
        // "[1 code]  2 callees   3 callers   4 reach".
        let mut line = Vec::new();
        for tab in self.tabs() {
            if tab == self.tab {
                line.push(Span::raw(format!("[{}]", tab.label())).bold());
            } else {
                line.push(Span::raw(format!(" {} ", tab.label())));
            }
            line.push(Span::raw(" "));
        }
        frame.render_widget(Line::from(line), tabs);

        if let (Tab::Code, Ok(code)) = (self.tab, &mut self.code) {
            code.draw(frame, body);
        } else if let Some(printed) = self.printed_mut() {
            printed.draw(frame, body);
        }
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
            return Go::Say(text::root_sentence(binary, &name, root.kind));
        }
        if let Err(why) = &self.retained {
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

    /// What find calls the tab's rows: "line" on the code tab.
    pub fn row_noun(&self) -> &'static str {
        match self.tab {
            Tab::Code => "line",
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
    /// does: the code tab has none.
    pub fn command(&self) -> Option<&str> {
        self.printed().map(Printed::command)
    }
}

/// Tab 1: a function's code.
struct CodeTab {
    /// The function, by its index.
    index: usize,
    code: Rc<PageCode>,
    cursor: Cursor,
    /// The rows the cursor was on before each jump inside the function,
    /// for Back.
    jumps: Vec<usize>,
}

/// An item's code as its page shows it. The session keeps it, so that
/// every page opened on the item shares it, and it is worked out once.
pub struct PageCode {
    lines: Vec<CodeLine>,
    /// Whether the lines show their instructions.
    instructions: bool,
    /// The widest address and instruction, worked out once, so that a
    /// redraw reads only the rows on the screen.
    widest: (usize, usize),
    /// Said in place of the code when there is none to show.
    empty: Option<String>,
    /// Said under the code.
    footnote: Option<String>,
}

impl CodeTab {
    /// The code of function `index`, with the line that holds `at`
    /// selected.
    fn new(session: &Session, index: usize, at: Option<u64>) -> CodeTab {
        let code = session.page_code(index);
        let mut tab = CodeTab {
            index,
            cursor: Cursor::new(code.lines.len()),
            code,
            jumps: Vec::new(),
        };
        if let Some(at) = at {
            tab.select_address(at);
        }
        tab
    }

    /// Selects the line that holds `address`.
    fn select_address(&mut self, address: u64) {
        let after = self
            .code
            .lines
            .partition_point(|line| line.address <= address);
        self.cursor.select(after.saturating_sub(1));
    }

    /// Draws the code, or why there is none, with the footnote under it.
    /// The footnote wraps, so that nothing is cut.
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        let bottom: Vec<String> = self
            .code
            .footnote
            .iter()
            .flat_map(|text| wrap(text, width))
            .collect();
        let [body, foot] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(bottom.len() as u16)])
                .areas(area);
        draw_lines(frame, foot, bottom.into_iter().map(Line::raw).collect());
        if let Some(text) = &self.code.empty {
            draw_lines(
                frame,
                body,
                wrap(text, width).into_iter().map(Line::raw).collect(),
            );
            return;
        }
        self.draw_code(frame, body);
    }

    /// The lines of code, as a table: ADDRESS, INSTRUCTION and the notes.
    fn draw_code(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        // The table's header takes a line.
        let (visible, mut state) = self
            .cursor
            .visible(usize::from(area.height).saturating_sub(1));
        let (widest_address, widest_instruction) = self.code.widest;
        let address_width = widest_address.max("ADDRESS".len());
        // Before the notes: the selection's "> ", the columns, and the
        // spaces after each.
        let mut used = 2 + address_width + 2;
        let mut instruction_width = 0;
        if self.code.instructions {
            let (most, fewest) = INSTRUCTION_WIDTH;
            instruction_width = widest_instruction.min(most);
            let wanted = used + instruction_width + 2 + NOTE_WIDTH;
            if wanted > width {
                instruction_width = instruction_width.saturating_sub(wanted - width).max(fewest);
            }
            used += instruction_width + 2;
        }
        let room = width.saturating_sub(used);
        let rows = self.code.lines[visible].iter().map(|line| {
            let mut cells = vec![Line::raw(format!("{:#x}", line.address))];
            if self.code.instructions {
                cells.push(Line::raw(cut(&line.instruction, instruction_width)));
            }
            cells.push(note_cell(line, room));
            Row::new(cells)
        });
        let mut header = vec!["ADDRESS"];
        let mut columns = vec![Constraint::Length(address_width as u16)];
        if self.code.instructions {
            header.push("INSTRUCTION");
            columns.push(Constraint::Length(instruction_width as u16));
        }
        header.push("");
        columns.push(Constraint::Fill(1));
        let table = Table::new(rows, columns)
            .column_spacing(2)
            .header(Row::new(header).bold())
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if self.cursor.handle(action) {
            return None;
        }
        let selected = self.cursor.selected;
        match action {
            Action::NextLink | Action::PreviousLink => {
                // Jumps inside the function don't count: Tab goes from one
                // use of another item to the next.
                let lines = &self.code.lines;
                let next = self.cursor.next(action == Action::NextLink, |row| {
                    matches!(lines[row].link, Some(Link::Node { .. }))
                });
                if let Some(row) = next {
                    self.cursor.select(row);
                }
                None
            }
            Action::Open => match self.code.lines.get(selected)?.link? {
                Link::Node { node, at, .. } => {
                    let this = Some(Node::Item(self.index));
                    Some(follow(session, node, Some(at), this, Tab::Code))
                }
                // A jump inside the function moves the cursor, and Back
                // brings it back.
                Link::Here(address) => {
                    self.jumps.push(selected);
                    self.select_address(address);
                    None
                }
            },
            _ => None,
        }
    }

    /// Goes back to the line before the last jump inside the function, if
    /// there was one.
    fn back(&mut self) -> bool {
        match self.jumps.pop() {
            Some(row) => {
                self.cursor.select(row);
                true
            }
            None => false,
        }
    }

    /// Where the selected line's link goes, in full: "→ strlen@plt
    /// function at 0x1080, 16 bytes, in .plt.sec".
    fn preview(&self, session: &Session) -> Option<String> {
        let link = self.code.lines.get(self.cursor.selected)?.link?;
        Some(match link {
            Link::Node { node, via, .. } => {
                link_preview(session, node, Some(Node::Item(self.index)), via)
            }
            Link::Here(address) => format!("→ {address:#x}, in this function"),
        })
    }

    /// Whether some line links to another item.
    fn has_links(&self) -> bool {
        self.code
            .lines
            .iter()
            .any(|line| matches!(line.link, Some(Link::Node { .. })))
    }

    /// The lines that have `lowercase` in their address, instruction or
    /// note, or the section that the note names, ignoring case.
    fn rows_with(&self, lowercase: &str) -> Vec<usize> {
        let has = |text: &str| has_ignoring_case(text, lowercase);
        let mut address = String::new();
        (self.code.lines.iter().enumerate())
            .filter(|(_, line)| {
                address.clear();
                write!(address, "{:#x}", line.address).expect("writing to a string");
                has(&address)
                    || has(&line.instruction)
                    || has(&line.note)
                    || line.elsewhere.as_deref().is_some_and(has)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// A line's note, cut to `room` columns, with the section of an item in
/// another block of memory after it in yellow. When both don't fit, the
/// note keeps at least [`NOTE_WIDTH`] columns before the section goes, and
/// then the note alone is yellow.
fn note_cell(line: &CodeLine, room: usize) -> Line<'static> {
    let yellow = Style::new().fg(Color::Yellow);
    let Some(section) = &line.elsewhere else {
        return Line::raw(cut(&line.note, room));
    };
    let place = format!(" in {section}");
    let left = room.saturating_sub(place.chars().count());
    if line.note.chars().count() <= left || left >= NOTE_WIDTH {
        Line::from(vec![
            Span::raw(cut(&line.note, left)),
            Span::styled(place, yellow),
        ])
    } else {
        Line::styled(cut(&line.note, room), yellow)
    }
}

/// The facts about item `index` that its header gives: where it is, and
/// on the next line, what makes it a root, how many items use it,
/// `used_by`, and how many it uses.
fn header(session: &Session, index: usize, used_by: usize) -> (String, String) {
    let binary = session.binary;
    let item = &binary.items[index];
    let mut place = whereabouts(item, &binary.sections[item.section].name);
    // Which block of memory, when there are several, as in firmware.
    if session.blocks.len() > 1
        && let Some(block) = session.block_of(item.address)
    {
        place += &format!(" ({})", text::range_argument(session.blocks[block].range));
    }
    if let Some(load) = binary.load_address(item) {
        place += &format!(", copied there from {load:#x}");
    }
    let mut facts = Vec::new();
    if item.source == Source::Reached {
        facts.push("found by following the code".to_string());
    }
    let node = Node::Item(index);
    if let Some(root) = session.graph.root(node) {
        facts.push(format!("root: {}", text::root_column(binary, root.kind)));
    }
    facts.push(used_by_text(used_by));
    if item.size > 0 {
        let uses: BTreeSet<Node> = session
            .graph
            .references_from(index)
            .iter()
            .map(|reference| reference.to)
            .collect();
        facts.push(format!("uses {}", uses.len()));
    }
    let unresolved = session.graph.unresolved_in(index);
    if !unresolved.is_empty() {
        facts.push(format!(
            "{} elfq can't follow",
            text::calls_and_jumps(unresolved)
        ));
    }
    (place, facts.join(" · "))
}

/// "used by 5", or "used by nothing".
fn used_by_text(used_by: usize) -> String {
    match used_by {
        0 => "used by nothing".to_string(),
        users => format!("used by {users}"),
    }
}

/// What the header says about what keeps an item, as `retained` and `why`
/// say it.
struct Keeping {
    /// What removing the item would save, where `retained` answers about it
    /// and something reaches the item.
    retained: Option<u64>,
    /// The name of the item that keeps it, if one does, as `retained`'s
    /// rows name it after "kept by".
    kept_by: Option<String>,
    /// Whether nothing reaches the item from where the program starts.
    unreached: bool,
    /// The names on the shortest chain to the item from where the program
    /// starts, as the first that `why SYMBOL --json` lists, each as rows
    /// name it after "used by": empty for a root, which the facts already
    /// say, for an item that nothing reaches, and where `why` refuses the
    /// item.
    chain: Vec<String>,
}

impl Keeping {
    /// What the header says about what keeps `node`, if the session has
    /// worked out what keeps each item and `retained` or `why` answers
    /// about it, as `retained` and `why` say.
    fn new(session: &Session, node: Node, retained: bool, why: bool) -> Option<Keeping> {
        let keepers = session.keepers()?;
        if !retained && !why {
            return None;
        }
        let name = |node: Node| session.short_name(node);
        let unreached = !keepers.tree.is_reachable(node);
        let chain = match why {
            true => why::first_chain(session.binary, &session.graph, &keepers.paths, node),
            false => None,
        };
        Some(Keeping {
            retained: (retained && !unreached).then(|| keepers.tree.retained(node)),
            kept_by: (retained.then(|| keepers.tree.parent(node)).flatten()).map(name),
            unreached,
            chain: chain.unwrap_or_default().into_iter().map(name).collect(),
        })
    }

    /// The facts it adds to the header's second line after `before`, the
    /// facts there already, on a line `width` columns wide: " · retained
    /// 1072, kept by __crc32_veneer", or " · nothing reaches it". The name
    /// of the item that keeps it is shortened to fit what is left of the
    /// line, so that the facts stay on one, or where less than
    /// [`KEEPER_WIDTH`] columns are left, to fit a line of its own.
    fn facts(&self, before: &str, width: usize) -> String {
        if self.unreached {
            return " · nothing reaches it".to_string();
        }
        let Some(retained) = self.retained else {
            return String::new();
        };
        let facts = format!(" · retained {retained}");
        let Some(name) = &self.kept_by else {
            return facts;
        };
        let facts = facts + ", kept by ";
        let used = before.chars().count() + facts.chars().count();
        let room = match width.saturating_sub(used) {
            left if left >= KEEPER_WIDTH => left,
            _ => width,
        };
        facts + &fit_name(name, room)
    }

    /// The header's line with the chain, to fit `width` columns: "from a
    /// root: Reset_Handler › main › process_frame". Each name is shortened
    /// to fit half of the line, and where the chain is still too long, the
    /// names at its start are left out, so that its end shows: "from a
    /// root: … › process_frame › __crc32_veneer › crc32".
    fn chain_line(&self, width: usize) -> Option<String> {
        if self.chain.is_empty() {
            return None;
        }
        let lead = "from a root: ";
        let room = width.saturating_sub(lead.chars().count()).max(20);
        let names: Vec<String> = (self.chain.iter())
            .map(|name| fit_name(name, (room / 2).max(20)))
            .collect();
        let chain = |from: usize| match from {
            0 => names.join(" › "),
            _ => format!("… › {}", names[from..].join(" › ")),
        };
        // The fewest names left out that let it fit, or all but the last,
        // which is cut from the left where even that doesn't fit.
        let from = (0..names.len())
            .find(|&from| chain(from).chars().count() <= room)
            .unwrap_or(names.len() - 1);
        Some(format!("{lead}{}", cut_left(&chain(from), room)))
    }
}

/// What a function's page lists.
enum Code {
    /// Every instruction, and the data among them.
    Instructions(Vec<CodeLine>),
    /// Only the lines that use another item, or that elfq can't follow,
    /// where elfq doesn't print the code, and why it doesn't.
    Links(Vec<CodeLine>, String),
}

/// Why elfq doesn't print the instructions of `binary`, or `None` when it
/// does. elfq prints Thumb code only for the Cortex-M cores up to Armv7E-M:
/// the printer reads some instructions of later cores as others.
fn no_text(binary: &Binary) -> Option<String> {
    if binary.architecture != Architecture::Arm {
        return None;
    }
    match binary.arm_architecture {
        Some(architecture) if architecture.is_cortex_m_up_to_v7() => None,
        Some(architecture) => Some(format!(
            "elfq prints only the instructions of Cortex-M cores up to Armv7E-M, and this file \
             is for {}",
            architecture.name()
        )),
        None => Some(
            "This file doesn't say which Arm core its code is for, so elfq doesn't print its \
             instructions"
                .to_string(),
        ),
    }
}

/// The lines of `item`'s code, whose bytes are `bytes`, as `arch` prints
/// them, with the data that the file marks there (see
/// [`Binary::data_in_code`]); `None` for an instruction set that elfq
/// doesn't print.
pub(super) fn listing(binary: &Binary, item: &Item, bytes: &[u8]) -> Option<Vec<arch::Line>> {
    // The marks, as offsets into `bytes`.
    let marked: Vec<std::ops::Range<usize>> = binary
        .data_in_code(item.address, item.size)
        .map(|(start, end)| (start - item.address) as usize..(end - item.address) as usize)
        .collect();
    arch::listing(
        binary.architecture,
        binary.endianness,
        bytes,
        item.address,
        &marked,
    )
}

impl PageCode {
    /// Item `index`'s code, as its page shows it.
    pub fn new(session: &Session, index: usize) -> PageCode {
        let mut shown = PageCode {
            lines: Vec::new(),
            instructions: false,
            widest: (0, 0),
            empty: None,
            footnote: None,
        };
        match code(session, index) {
            Ok(Code::Instructions(lines)) => {
                shown.lines = lines;
                shown.instructions = true;
            }
            Ok(Code::Links(lines, why)) if lines.is_empty() => {
                shown.empty = Some(format!(
                    "{why}. No line of {} uses another item.",
                    session.row_name(index)
                ));
            }
            Ok(Code::Links(lines, why)) => {
                shown.lines = lines;
                shown.footnote = Some(format!("{why}: these are the lines that use another item."));
            }
            Err(why) => shown.empty = Some(why),
        }
        let widest = |cell: fn(&CodeLine) -> usize| shown.lines.iter().map(cell).max().unwrap_or(0);
        shown.widest = (
            widest(|line| format!("{:#x}", line.address).len()),
            widest(|line| line.instruction.chars().count()),
        );
        shown
    }
}

/// The lines of item `index`'s code, or why there are none to show.
fn code(session: &Session, index: usize) -> Result<Code, String> {
    let binary = session.binary;
    let item = &binary.items[index];
    if item.kind == ItemKind::Data {
        return Err(format!(
            "{} is a data object: it has no code",
            session.row_name(index)
        ));
    }
    // Such as "elfq doesn't know where _init ends, so it can't read its
    // code", as the commands say it.
    readable_item(binary, Node::Item(index)).map_err(|error| error.to_string())?;
    let Some(bytes) = binary.bytes(item.address, item.size) else {
        return Err(format!(
            "the file doesn't hold the bytes of {}, so elfq can't read its code",
            item.name
        ));
    };
    let notes = Notes::new(session, index);
    let why = match no_text(binary) {
        None => match listing(binary, item, bytes) {
            Some(listing) => return Ok(Code::Instructions(notes.lines(&listing))),
            None => "elfq doesn't print this code".to_string(),
        },
        Some(why) => why,
    };
    let mut sites: Vec<u64> = notes.references.keys().copied().collect();
    sites.extend(notes.unresolved.keys());
    sites.sort_unstable();
    sites.dedup();
    let lines = sites
        .into_iter()
        .map(|site| notes.line(site, None))
        .collect();
    Ok(Code::Links(lines, why))
}

/// What the graph knows about one function's code, by site, to write the
/// notes on its lines.
struct Notes<'s, 'a> {
    session: &'s Session<'a>,
    index: usize,
    references: HashMap<u64, Vec<&'s Reference>>,
    unresolved: HashMap<u64, &'s Unresolved>,
    /// The address that each instruction uses, which can be inside the
    /// item it uses: `uses frames+0x8`.
    used: HashMap<u64, u64>,
}

impl<'s, 'a> Notes<'s, 'a> {
    fn new(session: &'s Session<'a>, index: usize) -> Notes<'s, 'a> {
        let graph = &session.graph;
        let mut references: HashMap<u64, Vec<&Reference>> = HashMap::new();
        for reference in graph.references_from(index) {
            references
                .entry(reference.site)
                .or_default()
                .push(reference);
        }
        let unresolved = graph
            .unresolved_in(index)
            .iter()
            .map(|unresolved| (unresolved.site, unresolved))
            .collect();
        let mut used = HashMap::new();
        for reference in session.binary.code_references(&session.binary.items[index]) {
            if let CodeReferenceKind::Address(address) | CodeReferenceKind::Memory(address) =
                reference.kind
            {
                used.entry(reference.site).or_insert(address);
            }
        }
        Notes {
            session,
            index,
            references,
            unresolved,
            used,
        }
    }

    /// The lines of `listing`, each with its note.
    fn lines(&self, listing: &[arch::Line]) -> Vec<CodeLine> {
        let mut lines: Vec<CodeLine> = listing
            .iter()
            .map(|printed| self.line(printed.address, Some(printed)))
            .collect();
        // A word that a load from a literal pool reads links where the
        // load does, as `ldr r3, [pc, #8]` uses systick_count, and its word
        // holds systick_count's address.
        let mut loaded = HashMap::new();
        for (printed, line) in listing.iter().zip(&lines) {
            if let Some(Refers::Literal {
                address: word,
                size: 4,
            }) = printed.refers
                && let Some(link @ Link::Node { .. }) = line.link
            {
                loaded.entry(word).or_insert(link);
            }
        }
        for (printed, line) in listing.iter().zip(&mut lines) {
            if let Some(Refers::Word(value)) = printed.refers
                && line.link.is_none()
            {
                self.word(line, value, loaded.get(&printed.address).copied());
            }
        }
        lines
    }

    /// Gives a `.word` that holds `value` its link and note: `link`, where
    /// a load reads it, or else the item that starts at `value`, as a
    /// pointer stored in data names it.
    fn word(&self, line: &mut CodeLine, value: u64, link: Option<Link>) {
        let binary = self.session.binary;
        let link = link.or_else(|| {
            let index = item_starting_at(binary, value)?;
            let at = binary.items[index].address;
            Some(Link::Node {
                node: Node::Item(index),
                at,
                via: None,
            })
        });
        let Some(link @ Link::Node { node, at, .. }) = link else {
            return;
        };
        line.note = match node {
            Node::Item(index) => self.name_at(index, at),
            Node::Import(_) => self.name(node),
        };
        line.elsewhere = self.elsewhere(link);
        line.link = Some(link);
    }

    /// The line at `address`, with its instruction `printed`, if elfq
    /// prints it.
    fn line(&self, address: u64, printed: Option<&arch::Line>) -> CodeLine {
        let binary = self.session.binary;
        let mut line = CodeLine {
            address,
            instruction: printed.map_or(String::new(), |printed| self.instruction(printed)),
            note: String::new(),
            elsewhere: None,
            link: None,
        };
        if let Some(references) = self.references.get(&address) {
            let reference = references[0];
            let (name, via, link) = self.target(reference);
            let how = text::how_via(binary, reference.to, reference.kind, via);
            let more = match references.len() {
                1 => String::new(),
                count => format!(" and {} more", count - 1),
            };
            // The name only when the instruction doesn't say it already.
            line.note = if via.is_some() {
                format!("{how} → {name}{more}")
            } else if !line.instruction.is_empty() && line.instruction.ends_with(&name) {
                format!("{how}{more}")
            } else {
                format!("{how} {name}{more}")
            };
            line.elsewhere = self.elsewhere(link);
            line.link = Some(link);
        } else if let Some(unresolved) = self.unresolved.get(&address) {
            let what = text::call_or_jump(unresolved);
            line.note = match unresolved.target {
                Some(target) => format!("{what} {target:#x}, outside any function"),
                None => format!("{what} through a pointer: elfq can't follow"),
            };
        } else if let Some(refers) = printed.and_then(|printed| printed.refers) {
            line.note = self.inside(&mut line, refers);
        }
        line
    }

    /// The note of a line that refers to an address in this function, or
    /// to a constant: `jump ↓`, `address ↑`, or `= 0x028f5c28` for the
    /// word or double that a load from a literal pool reads. It links to
    /// the line it refers to, if any.
    fn inside(&self, line: &mut CodeLine, refers: Refers) -> String {
        let binary = self.session.binary;
        let item = &binary.items[self.index];
        let (what, target) = match refers {
            Refers::Branch(target) | Refers::Case(target) => ("jump", target),
            Refers::Address(target) => ("address", target),
            Refers::Literal { address, size } => {
                let endianness = binary.endianness;
                return match binary.bytes(address, size as u64) {
                    Some(&[a, b, c, d]) => format!("= {:#010x}", endianness.read_u32([a, b, c, d])),
                    Some(&[a, b, c, d, e, f, g, h]) => {
                        format!("= {:#018x}", endianness.read_u64([a, b, c, d, e, f, g, h]))
                    }
                    _ => String::new(),
                };
            }
            Refers::Word(_) => return String::new(),
        };
        if !item.contains(target) {
            return String::new();
        }
        line.link = Some(Link::Here(target));
        let arrow = if target <= line.address { "↑" } else { "↓" };
        format!("{what} {arrow}")
    }

    /// The instruction's text, with the mnemonic in 7 columns, and the
    /// name of the item that a call or jump goes to in place of its
    /// address, unless it goes to another line of this function.
    fn instruction(&self, printed: &arch::Line) -> String {
        let binary = self.session.binary;
        let mut operands = printed.operands.clone();
        if let Some(Refers::Branch(target)) = printed.refers
            && !binary.items[self.index].contains(target)
            && let Some(index) = binary.item_at(target)
            && let Some(before) = operands.strip_suffix(&format!("{target:#x}"))
        {
            operands = format!("{before}{}", self.name_at(index, target));
        }
        format!("{:<7} {operands}", printed.mnemonic)
            .trim_end()
            .to_string()
    }

    /// The name of item `index`, as rows give it, with how far into it
    /// `address` is when that isn't its start: `frames+0x8`.
    fn name_at(&self, index: usize, address: u64) -> String {
        let name = self.session.row_name(index);
        match address.saturating_sub(self.session.binary.items[index].address) {
            0 => name,
            offset => format!("{name}+{offset:#x}"),
        }
    }

    /// What `reference` uses, as its note names it, the veneer or PLT stub
    /// it goes through, and the link to it. A call through a veneer links
    /// to where the veneer goes.
    fn target(&self, reference: &Reference) -> (String, Option<usize>, Link) {
        let binary = self.session.binary;
        if let Node::Item(veneer) = reference.to
            && reference.kind.is_call()
            && let Some(&target) = self.session.veneers.get(&veneer)
        {
            return match target {
                VeneerTarget::Node(node) => {
                    let at = match node {
                        Node::Item(index) => binary.items[index].address,
                        Node::Import(_) => 0,
                    };
                    let link = Link::Node {
                        node,
                        at,
                        via: Some(veneer),
                    };
                    (self.name(node), Some(veneer), link)
                }
                // The veneer is all there is to open.
                VeneerTarget::Outside(address) => {
                    let link = Link::Node {
                        node: reference.to,
                        at: binary.items[veneer].address,
                        via: None,
                    };
                    (
                        format!("{address:#x}, outside any function"),
                        Some(veneer),
                        link,
                    )
                }
            };
        }
        let (name, at) = match reference.to {
            Node::Item(index) => {
                let item = &binary.items[index];
                // A use of data can be inside it; a call is to the start.
                let at = match self.used.get(&reference.site) {
                    Some(&used) if item.kind == ItemKind::Data && item.contains(used) => used,
                    _ => item.address,
                };
                let mut name = self.name_at(index, at);
                if let Some(string) = string_at(self.session, index) {
                    name += &format!(" {string}");
                }
                (name, at)
            }
            Node::Import(_) => (self.name(reference.to), 0),
        };
        let link = Link::Node {
            node: reference.to,
            at,
            via: None,
        };
        (name, None, link)
    }

    /// The name that rows give `node`.
    fn name(&self, node: Node) -> String {
        match node {
            Node::Item(index) => self.session.row_name(index),
            Node::Import(_) => node.name(self.session.binary).to_string(),
        }
    }

    /// The section of the item that `link` goes to, when that item is in
    /// another block of memory than this function.
    fn elsewhere(&self, link: Link) -> Option<String> {
        let Link::Node {
            node: Node::Item(to),
            ..
        } = link
        else {
            return None;
        };
        let items = &self.session.binary.items;
        let here = self.session.block_of(items[self.index].address)?;
        let there = self.session.block_of(items[to].address)?;
        (here != there).then(|| self.session.binary.sections[items[to].section].name.clone())
    }
}

/// The text that data object `index` starts with, quoted as Rust writes
/// strings, when it is a short string of printable text, as a string
/// literal is: `"tick\n"`.
fn string_at(session: &Session, index: usize) -> Option<String> {
    let item = &session.binary.items[index];
    if item.kind != ItemKind::Data {
        return None;
    }
    let bytes = session.binary.bytes(item.address, item.size.min(64))?;
    let text = &bytes[..bytes.iter().position(|&byte| byte == 0)?];
    let printable = |byte: &u8| byte.is_ascii_graphic() || b" \t\n".contains(byte);
    if text.is_empty() || !text.iter().all(printable) {
        return None;
    }
    Some(format!("{:?}", String::from_utf8_lossy(text)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::printed::table_lines;
    use super::super::samples;
    use super::*;
    use crate::commands::json;
    use crate::commands::retained_tree::RetainedTree;
    use crate::graph::DominatorTree;
    use crate::render::{Align, Table, TreeShape};

    /// Calls `check` with the session of every sample and the code of each
    /// of its functions that has some.
    fn every_function(mut check: impl FnMut(&Session, usize, &Code)) {
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            for index in 0..binary.items.len() {
                if let Ok(code) = code(&session, index) {
                    check(&session, index, &code);
                }
            }
        }
    }

    fn lines(code: &Code) -> &[CodeLine] {
        match code {
            Code::Instructions(lines) | Code::Links(lines, _) => lines,
        }
    }

    #[test]
    fn every_use_that_elfq_finds_starts_a_line_of_code() {
        // Functions with lines, by architecture.
        let mut functions: HashMap<Architecture, usize> = HashMap::new();
        every_function(|session, index, code| {
            let Code::Instructions(lines) = code else {
                return;
            };
            *functions.entry(session.binary.architecture).or_default() += 1;
            let graph = &session.graph;
            let sites = (graph.references_from(index).iter().map(|r| r.site))
                .chain(graph.unresolved_in(index).iter().map(|u| u.site));
            for site in sites {
                let line = lines.iter().find(|line| line.address == site);
                let name = &session.binary.items[index].name;
                let line = line.unwrap_or_else(|| panic!("{site:#x} in {name} starts no line"));
                assert!(!line.note.is_empty(), "{site:#x} in {name} has no note");
            }
        });
        assert!(functions[&Architecture::X86_64] > 1000, "{functions:?}");
        assert!(functions[&Architecture::Arm] > 500, "{functions:?}");
    }

    #[test]
    fn the_status_line_names_what_enter_opens() {
        // Lines that link to the function they are in, as a recursive
        // call does, and to an import through its own PLT stub.
        let (mut to_itself, mut to_own_import) = (0, 0);
        every_function(|session, index, _| {
            let binary = session.binary;
            let mut tab = CodeTab::new(session, index, None);
            for row in 0..tab.code.lines.len() {
                let Some(Link::Node { node, via, .. }) = tab.code.lines[row].link else {
                    continue;
                };
                tab.cursor.select(row);
                let preview = tab.preview(session).unwrap();
                let opened = session.page_node(node);
                let name = match opened {
                    _ if matches!(node, Node::Import(_)) && opened == Node::Item(index) => {
                        to_own_import += 1;
                        format!("{}  imported", node.name(binary))
                    }
                    Node::Item(index) => format!("{}  ", binary.items[index].name),
                    Node::Import(_) => format!("{}  imported", node.name(binary)),
                };
                to_itself += usize::from(node == Node::Item(index) && via.is_none());
                assert!(preview.starts_with(&format!("→ {name}")), "{preview}");
            }
        });
        assert!(
            to_itself > 50 && to_own_import > 30,
            "{to_itself} {to_own_import}"
        );
    }

    #[test]
    fn the_header_says_what_retained_and_why_say() {
        // Items whose header names what keeps them, roots, items with a
        // chain, and items that nothing reaches.
        let (mut kept, mut roots, mut chains, mut unreached) = (0, 0, 0, 0);
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let mut session = Session::new(&path, &binary).unwrap();
            session.work_out_keepers();
            let graph = &session.graph;
            // A shared library, about whose items `retained` and `why`
            // don't answer, needn't know what keeps them.
            let Some(keepers) = session.keepers() else {
                assert!(retained::refusal(&binary, Node::Item(0)).is_err());
                assert!(why::refusal(&binary, Node::Item(0)).is_err());
                continue;
            };
            // What the commands write, as they run them, but on the graph
            // that the session has built.
            let retained_tree = RetainedTree::new(&binary, graph, None);
            let tree = DominatorTree::build(&binary, graph, None);
            let json = |write: &dyn Fn(&mut Vec<u8>) -> anyhow::Result<()>| {
                let mut out = Vec::new();
                write(&mut out).unwrap();
                serde_json::from_slice::<serde_json::Value>(&out).unwrap()
            };
            let item = |node: Node| serde_json::to_value(json::Item::new(&binary, node)).unwrap();
            let items = (0..binary.items.len()).map(Node::Item);
            for node in items.chain((0..binary.imports.len()).map(Node::Import)) {
                let shown = format!("{}: {}", path.display(), node.name(&binary));
                let retained_index = retained::refusal(&binary, node).ok();
                let why_answers = why::refusal(&binary, node).is_ok();
                let keeping = Keeping::new(&session, node, retained_index.is_some(), why_answers);
                let Some(keeping) = keeping else {
                    assert!(retained_index.is_none() && !why_answers, "{shown}");
                    continue;
                };
                // R is what `retained SYMBOL --json` says removing it would
                // save, where it answers.
                if let Some(index) = retained_index {
                    let symbol = retained::SymbolInTree {
                        retained: &retained_tree,
                        index,
                        lead_name: "",
                    };
                    let answer = json(&|out| retained::write_json(&symbol, out));
                    let reachable = answer["reachable"].as_bool().unwrap();
                    assert_eq!(keeping.unreached, !reachable, "{shown}");
                    let saved = reachable.then(|| answer["retained"].as_u64().unwrap());
                    assert_eq!(keeping.retained, saved, "{shown}");
                }
                if graph.root(node).is_some() {
                    assert!(keeping.kept_by.is_none() && keeping.chain.is_empty());
                    roots += 1;
                    continue;
                }
                // K is the item on every chain that `why SYMBOL --json`
                // names, and the chain its first.
                let answer = json(&|out| why::write_json(&binary, graph, &tree, node, out));
                let reachable = answer["reachable"].as_bool().unwrap();
                assert_eq!(keeping.unreached, !reachable, "{shown}");
                unreached += usize::from(keeping.unreached);
                let parent = keepers.tree.parent(node);
                let on_every_chain = parent.map_or(serde_json::Value::Null, item);
                assert_eq!(answer["on_every_chain"], on_every_chain, "{shown}");
                if retained_index.is_some() {
                    assert_eq!(keeping.kept_by.is_some(), parent.is_some(), "{shown}");
                }
                if let (Some(index), Some(Node::Item(parent))) = (retained_index, parent) {
                    // As `retained`'s own rows name it after "kept by": the
                    // row of this item, which comes right after K's in the
                    // text of the tree under K, after the columns' names.
                    let shape = TreeShape::Rows { words: "kept by" };
                    let with_rows = HashSet::from([parent, index]);
                    let mut table = Table::new(&[
                        ("RETAINED", Align::Right),
                        ("OWN", Align::Right),
                        ("ITEM", Align::Left),
                    ]);
                    table.tree(&[retained_tree.tree_row(&with_rows, parent, shape)], shape);
                    let row = &table_lines(&table)[2];
                    let kept_by = format!(", kept by {}", keeping.kept_by.as_ref().unwrap());
                    assert!(
                        row.contains(binary.items[index].name.as_str()),
                        "{shown}: {row}"
                    );
                    assert!(row.ends_with(&kept_by), "{shown}: {row}");
                    kept += 1;
                }
                let first = why::first_chain(&binary, graph, &keepers.paths, node);
                let first = first.unwrap_or_default();
                let steps: Vec<_> = (answer["chains"].get(0).and_then(|c| c.as_array()))
                    .map_or(Vec::new(), |steps| {
                        steps.iter().map(|s| s["item"].clone()).collect()
                    });
                let nodes: Vec<_> = first.iter().map(|&node| item(node)).collect();
                assert_eq!(nodes, steps, "{shown}");
                // Each named as why's rows name the row they hang under.
                let names: Vec<String> = (first.into_iter())
                    .map(|node| session.names.short_name(node))
                    .collect();
                assert_eq!(keeping.chain, names, "{shown}");
                chains += usize::from(!keeping.chain.is_empty());
            }
        }
        assert!(
            kept > 4000 && roots > 150 && chains > 4000 && unreached > 100,
            "{kept} {roots} {chains} {unreached}"
        );
    }

    #[test]
    fn every_link_names_what_it_opens() {
        let (mut links, mut imports) = (0, 0);
        every_function(|session, _, code| {
            for line in lines(code) {
                let shown = format!("{} {}", line.instruction, line.note);
                match line.link {
                    Some(Link::Node {
                        node: node @ Node::Item(index),
                        ..
                    }) => {
                        // The name the line shows finds the item as SYMBOL,
                        // but for a veneer to code outside every item,
                        // which the line names by its address.
                        let name = session.row_name(index);
                        assert!(
                            shown.contains(&name) || shown.ends_with(", outside any function"),
                            "{shown:?} doesn't name {name}"
                        );
                        assert_eq!(session.names.find(&name).unwrap().node, node, "{name}");
                        links += 1;
                    }
                    Some(Link::Node { node, .. }) => {
                        let name = node.name(session.binary);
                        assert!(shown.contains(name), "{shown:?} doesn't name {name}");
                        // An import opens what its name finds as SYMBOL:
                        // its PLT stub, when it has one.
                        let found = session.names.find(name).unwrap().node;
                        assert_eq!(session.page_node(node), found, "{name}");
                        imports += 1;
                    }
                    Some(Link::Here(address)) => {
                        assert!(lines(code).iter().any(|line| line.address == address));
                    }
                    None => {}
                }
            }
        });
        assert!(links > 5000 && imports > 100, "{links} {imports}");
    }
}
