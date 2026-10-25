//! An item's page: a header of facts about it, then a function's code, one
//! line per instruction. A line that calls, jumps to or uses another item
//! ends with a note in the words of the CLI's HOW column, such as `call`,
//! `tail call via PLT → log_msg` or `uses frames_ok`, and links to that
//! item: Enter opens its page. A jump inside the function links to the
//! line it goes to.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use object::{Architecture, Endian};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table};

use super::{Cursor, Go, View, cut, imported, wrap};
use crate::arch::{self, CodeReferenceKind, Refers};
use crate::binary::{Binary, Item, ItemKind, Source};
use crate::commands::browse::{keys::Action, session::Session};
use crate::commands::{readable_item, text};
use crate::graph::{Node, Reference, Unresolved, VeneerTarget, item_starting_at};

/// The most columns that the instructions take, and the fewest they give
/// way to when the notes after them need room.
const INSTRUCTION_WIDTH: (usize, usize) = (48, 24);
/// The fewest columns that a note is cut to before its section goes.
const NOTE_WIDTH: usize = 16;

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
    pub title: String,
    /// The lines of facts above the code, before they are wrapped.
    header: Vec<String>,
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

impl ItemPage {
    /// The page of item `index`, with the line that holds `at` selected.
    /// The header names the item as `lead`, which may say what SYMBOL gave,
    /// as in "Default_Handler (also named NMI_Handler)".
    pub fn new(session: &Session, index: usize, at: Option<u64>, lead: &str) -> ItemPage {
        let code = session.page_code(index);
        let mut page = ItemPage {
            title: session.row_name(index),
            header: header(session, index, lead),
            cursor: Cursor::new(code.lines.len()),
            code,
            jumps: Vec::new(),
        };
        if let Some(at) = at {
            page.select_address(at);
        }
        page
    }

    /// Selects the line that holds `address`.
    fn select_address(&mut self, address: u64) {
        let after = self
            .code
            .lines
            .partition_point(|line| line.address <= address);
        self.cursor.select(after.saturating_sub(1));
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let width = usize::from(area.width);
        // The header and the footnote wrap, so that nothing is cut.
        let mut top: Vec<Line> = Vec::new();
        for (index, text) in self.header.iter().enumerate() {
            let style = if index == 0 {
                Style::new().bold()
            } else {
                Style::new().dim()
            };
            top.extend(
                wrap(text, width)
                    .into_iter()
                    .map(|line| Line::styled(line, style)),
            );
        }
        let bottom: Vec<String> = self
            .code
            .footnote
            .iter()
            .flat_map(|text| wrap(text, width))
            .collect();
        let [head, body, foot] = Layout::vertical([
            Constraint::Length(top.len() as u16),
            Constraint::Fill(1),
            Constraint::Length(bottom.len() as u16),
        ])
        .areas(area);
        draw_lines(frame, head, top);
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

    pub fn handle(&mut self, session: &Session, action: Action) -> Option<Go> {
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
                Link::Node {
                    node: Node::Item(index),
                    at,
                    ..
                } => {
                    let name = &session.binary.items[index].name;
                    let page = ItemPage::new(session, index, Some(at), name);
                    Some(Go::Open(View::Item(Box::new(page))))
                }
                Link::Node { node, .. } => Some(Go::Say(imported(session, node))),
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
    pub fn back(&mut self) -> bool {
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
    pub fn preview(&self, session: &Session) -> Option<String> {
        let binary = session.binary;
        let link = self.code.lines.get(self.cursor.selected)?.link?;
        Some(match link {
            Link::Node {
                node: Node::Item(index),
                via,
                ..
            } => {
                let through = match via {
                    Some(veneer) => format!(", through {}", binary.items[veneer].name),
                    None => String::new(),
                };
                let item = &binary.items[index];
                format!(
                    "→ {}  {}{through}",
                    item.name,
                    whereabouts(item, &binary.sections[item.section].name)
                )
            }
            Link::Node { node, .. } => {
                format!("→ {}  imported from a shared library", node.name(binary))
            }
            Link::Here(address) => format!("→ {address:#x}, in this function"),
        })
    }

    /// Whether some line links to another item.
    pub fn has_links(&self) -> bool {
        self.code
            .lines
            .iter()
            .any(|line| matches!(line.link, Some(Link::Node { .. })))
    }
}

/// Draws `lines` one under the other in `area`, as many as fit.
fn draw_lines(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    for (row, line) in (area.y..area.bottom()).zip(lines) {
        frame.render_widget(
            line,
            Rect {
                y: row,
                height: 1,
                ..area
            },
        );
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

/// What and where an item is: "function at 0x1080, 16 bytes, in .plt.sec".
fn whereabouts(item: &Item, section: &str) -> String {
    let kind = match item.kind {
        ItemKind::Function => "function",
        ItemKind::Data => "data object",
    };
    let size = match (item.size, item.size_estimated) {
        (0, _) => "size unknown".to_string(),
        (size, true) => format!("~{}", text::bytes_text(size)),
        (size, false) => text::bytes_text(size),
    };
    format!("{kind} at {:#x}, {size}, in {section}", item.address)
}

/// The lines of facts about item `index`, named `lead`: where it is, then
/// what makes it a root, and how many items use it and it uses.
fn header(session: &Session, index: usize, lead: &str) -> Vec<String> {
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
    facts.push(match session.used_by(node) {
        0 => "used by nothing".to_string(),
        users => format!("used by {users}"),
    });
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
    vec![format!("{lead}  {place}"), facts.join(" · ")]
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
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::binary::Binary;

    /// Every sample in testdata/bin.
    fn samples() -> Vec<PathBuf> {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "elf" || e == "so"))
            .collect();
        paths.sort();
        paths
    }

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
    fn every_link_names_what_it_opens() {
        let mut links = 0;
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
                    }
                    Some(Link::Here(address)) => {
                        assert!(lines(code).iter().any(|line| line.address == address));
                    }
                    None => {}
                }
            }
        });
        assert!(links > 5000, "{links}");
    }
}
