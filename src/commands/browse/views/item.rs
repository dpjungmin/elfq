//! An item's page: a header of facts about it, then its tabs. Tab 1 is a
//! function's code, one line per instruction. A line that calls, jumps to
//! or uses another item ends with a note in the words of the CLI's HOW
//! column, such as `call`, `tail call via PLT → log_msg` or `uses
//! frames_ok`, and links to that item: Enter opens its page. A jump inside
//! the function links to the line it goes to. Tab 3 lists what calls or
//! uses the item, as `callers SYMBOL` does (see `callers.rs`).

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use object::{Architecture, Endian};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table};

use super::printed::Printed;
use super::{
    Cursor, Go, cut, draw_lines, fit_name, follow, imported, keep_lines, link_preview, name_fitter,
    whereabouts, wrap,
};
use crate::arch::{self, CodeReferenceKind, Refers};
use crate::binary::{Binary, Item, ItemKind, Source};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::{distinct_nodes, readable_item, text};
use crate::graph::{Node, Reference, Unresolved, VeneerTarget, item_starting_at};

/// The most columns that the instructions take, and the fewest they give
/// way to when the notes after them need room.
const INSTRUCTION_WIDTH: (usize, usize) = (48, 24);
/// The fewest columns that a note is cut to before its section goes.
const NOTE_WIDTH: usize = 16;

/// The tabs of an item's page. Each is numbered as the key that shows it,
/// and named after the command that prints the same answer, so that the
/// numbers stay the same as more tabs come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 1: a function's code.
    Code,
    /// 3: what calls or uses the item.
    Callers,
}

impl Tab {
    /// Every tab, in the order of their numbers.
    const ALL: [Tab; 2] = [Tab::Code, Tab::Callers];

    /// The number of the key that shows it, and its name.
    fn number_and_name(self) -> (u8, &'static str) {
        match self {
            Tab::Code => (1, "code"),
            Tab::Callers => (3, "callers"),
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
    /// The header's lines, and the columns and the most lines they were
    /// made for.
    header: Option<((usize, usize), Vec<Line<'static>>)>,
    /// The tab on the screen. Each page in the history keeps its own, so
    /// that Back shows the tab that was left.
    tab: Tab,
    /// Tab 1, or for an item that has no code, why not, as pressing 1
    /// says.
    code: Result<CodeTab, String>,
    /// Tab 3.
    callers: Printed,
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
        ItemPage {
            title,
            short_title: None,
            identity: (name.to_string(), format!("{note}  {place}")),
            facts,
            header: None,
            tab: if code.is_ok() {
                Tab::Code
            } else {
                Tab::Callers
            },
            code,
            callers: super::callers::tab(session, node, answer),
        }
    }

    /// The page on `tab`, when it has that tab, as a link from that tab
    /// opens it.
    pub fn on(mut self, tab: Tab) -> ItemPage {
        if self.tabs().contains(&tab) {
            self.tab = tab;
        }
        self
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
        let has = |tab: &Tab| match tab {
            Tab::Code => self.code.is_ok(),
            Tab::Callers => true,
        };
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
        // " 1 code  [3 callers]".
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

        match (self.tab, &mut self.code) {
            (Tab::Code, Ok(code)) => code.draw(frame, body),
            // Never on the screen: 1 says why instead.
            (Tab::Code, Err(why)) => draw_lines(frame, body, vec![Line::raw(why.clone())]),
            (Tab::Callers, _) => self.callers.draw(frame, body),
        }
    }

    /// The header, wrapped to `width`, in at most `most` lines. It wraps,
    /// so that nothing is cut, unless the screen is too short for it, with
    /// the tab line and two lines of the tab under it. Then the item's name
    /// is shortened as the title line's is, and what is still too long is
    /// cut, ending in "…". They are made once for each size of screen.
    fn header_lines(&mut self, width: usize, most: usize) -> Vec<Line<'static>> {
        if let Some((size, lines)) = &self.header
            && *size == (width, most)
        {
            return lines.clone();
        }
        let (name, rest) = &self.identity;
        let identity = |name: &str| wrap(&format!("{name}{rest}"), width);
        let facts = wrap(&self.facts, width);
        let mut first = identity(name);
        if first.len() + facts.len() > most {
            let lines = most.saturating_sub(facts.len()).max(1);
            // The widest shortened name that leaves the line short enough,
            // which is no wider than the lines.
            let fit = name_fitter(name);
            let (mut fits, mut wider) = (0, name.chars().count().min(lines * width));
            while fits + 1 < wider {
                let middle = (fits + wider) / 2;
                if identity(&fit(middle)).len() <= lines {
                    fits = middle;
                } else {
                    wider = middle;
                }
            }
            first = keep_lines(identity(&fit(fits.max(1))), lines, width);
        }
        let facts = keep_lines(facts, most.saturating_sub(first.len()), width);
        let first = first.into_iter().map(|line| Line::raw(line).bold());
        let facts = facts.into_iter().map(|line| Line::raw(line).dim());
        let lines: Vec<Line<'static>> = first.chain(facts).collect();
        self.header = Some(((width, most), lines.clone()));
        lines
    }

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
        if let Action::Tab(number) = action {
            match (Tab::of_key(number)?, &self.code) {
                // A tab that the item doesn't have says why.
                (Tab::Code, Err(why)) => return Some(Go::Say(why.clone())),
                (tab, _) => self.tab = tab,
            }
            return None;
        }
        match (self.tab, &mut self.code) {
            (Tab::Code, Ok(code)) => code.handle(session, action),
            (Tab::Code, Err(_)) => None,
            (Tab::Callers, _) => self.callers.handle(session, action),
        }
    }

    /// Goes back to the line before the last jump inside the function, if
    /// there was one and the code is on the screen.
    pub fn back(&mut self) -> bool {
        match (self.tab, &mut self.code) {
            (Tab::Code, Ok(code)) => code.back(),
            (Tab::Code, Err(_)) | (Tab::Callers, _) => false,
        }
    }

    /// Where the selected line's link goes, in full: "→ strlen@plt
    /// function at 0x1080, 16 bytes, in .plt.sec".
    pub fn preview(&self, session: &Session) -> Option<String> {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.preview(session),
            (Tab::Code, Err(_)) => None,
            (Tab::Callers, _) => self.callers.preview(session),
        }
    }

    /// Whether some line or row links to another item.
    pub fn has_links(&self) -> bool {
        match (self.tab, &self.code) {
            (Tab::Code, Ok(code)) => code.has_links(),
            (Tab::Code, Err(_)) => false,
            (Tab::Callers, _) => self.callers.has_links(),
        }
    }

    /// The command that prints what the tab on the screen shows, if one
    /// does: the code tab has none.
    pub fn command(&self) -> Option<&str> {
        match self.tab {
            Tab::Code => None,
            Tab::Callers => Some(self.callers.command()),
        }
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
    use super::super::samples;
    use super::*;

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
