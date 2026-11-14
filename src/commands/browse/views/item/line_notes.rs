//! The note at the end of each line of tab 1, and where the line links.
//!
//! A line is an instruction of a function's code, or a row of a data
//! object's contents. Its note comes from what the graph knows about the
//! item, by site.

use std::collections::HashMap;

use object::Endian;

use super::super::PageAt;
use super::contents::string_at;
use super::listing::{Link, ListingLine};
use crate::arch::{self, CodeReferenceKind, Refers};
use crate::binary::{ItemKind, RelocationTarget};
use crate::commands::browse::session::Session;
use crate::commands::text;
use crate::graph::{Node, Reference, Unresolved, VeneerTarget, item_starting_at};

/// What a line that uses another item or import notes and links to, as
/// [`LineNotes::use_link`] works it out.
///
/// `via_veneer` is what the note says the use goes through, and the
/// link's own is what its preview names. They differ for a call through a
/// veneer to code outside every item: the note says it goes through the
/// veneer, but the veneer is all there is to open, so the link goes to it
/// and through nothing.
pub(super) struct UseLink {
    /// What the note names: the item, with how far into it the use is and
    /// the string there, if it holds one; an import; or an address outside
    /// every item.
    pub(super) name: String,
    /// The veneer or PLT stub that the use goes through, if any.
    pub(super) via_veneer: Option<usize>,
    /// Where the line links.
    pub(super) link: Link,
}

/// What the graph knows about one item, by site, to note on its lines.
///
/// The item is a function, whose code it covers, or a data object, whose
/// stored pointers it covers.
pub(super) struct LineNotes<'s, 'a> {
    pub(super) session: &'s Session<'a>,
    pub(super) index: usize,
    pub(super) references: HashMap<u64, Vec<&'s Reference>>,
    pub(super) unresolved: HashMap<u64, &'s Unresolved>,
    /// The address that each instruction uses, or that each pointer holds.
    ///
    /// It can be inside the item it uses: `uses frames+0x8`.
    used: HashMap<u64, u64>,
}

impl<'s, 'a> LineNotes<'s, 'a> {
    pub(super) fn new(session: &'s Session<'a>, index: usize) -> LineNotes<'s, 'a> {
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
        let binary = session.binary;
        let item = &binary.items[index];
        let mut used = HashMap::new();
        for reference in binary.code_references(item) {
            if let CodeReferenceKind::Address(address) | CodeReferenceKind::Memory(address) =
                reference.kind
            {
                used.entry(reference.site).or_insert(address);
            }
        }
        if item.kind == ItemKind::Data {
            for reference in graph.references_from(index) {
                if let Some(RelocationTarget::Address(address)) = binary.stored_at(reference.site) {
                    used.entry(reference.site).or_insert(address);
                }
            }
        }
        LineNotes {
            session,
            index,
            references,
            unresolved,
            used,
        }
    }

    /// The lines of `arch_lines`, each with its note.
    pub(super) fn lines(&self, arch_lines: &[arch::Line]) -> Vec<ListingLine> {
        let mut lines: Vec<ListingLine> = arch_lines
            .iter()
            .map(|printed| self.line(printed.address, Some(printed)))
            .collect();
        // A word that a load from a literal pool reads links where the
        // load does, as `ldr r3, [pc, #8]` uses systick_count, and its word
        // holds systick_count's address.
        let mut loaded = HashMap::new();
        for (printed, line) in arch_lines.iter().zip(&lines) {
            if let Some(Refers::Literal {
                address: word,
                size: 4,
            }) = printed.refers
                && let Some(link @ Link::Node { .. }) = line.link
            {
                loaded.entry(word).or_insert(link);
            }
        }
        for (printed, line) in arch_lines.iter().zip(&mut lines) {
            if let Some(Refers::Word(value)) = printed.refers
                && line.link.is_none()
            {
                self.word(line, value, loaded.get(&printed.address).copied());
            }
        }
        lines
    }

    /// Gives a `.word` that holds `value` its link and note.
    ///
    /// The link is `link`, where a load reads the word, or else the item
    /// that starts at `value`, as a pointer stored in data names it.
    fn word(&self, line: &mut ListingLine, value: u64, link: Option<Link>) {
        let binary = self.session.binary;
        let link = link.or_else(|| {
            let index = item_starting_at(binary, value)?;
            let page = PageAt {
                node: Node::Item(index),
                at: Some(binary.items[index].address),
            };
            Some(Link::Node {
                page,
                via_veneer: None,
            })
        });
        let Some(link @ Link::Node { page, .. }) = link else {
            return;
        };
        line.note = match (page.node, page.at) {
            (Node::Item(index), Some(at)) => self.name_at(index, at),
            // An import has no address in this file; an item without one
            // would be named at its start, as its row name is.
            (node, _) => self.session.row_name(node),
        };
        line.elsewhere = self.elsewhere(link);
        line.link = Some(link);
    }

    /// The line at `address`, with its note and link.
    ///
    /// Its text is the instruction `printed`, if elfq prints it.
    pub(super) fn line(&self, address: u64, printed: Option<&arch::Line>) -> ListingLine {
        let binary = self.session.binary;
        let mut line = ListingLine {
            address,
            text: printed.map_or(String::new(), |printed| self.instruction(printed)),
            note: String::new(),
            elsewhere: None,
            link: None,
        };
        if let Some(references) = self.references.get(&address) {
            let reference = references[0];
            let UseLink {
                name,
                via_veneer,
                link,
            } = self.use_link(reference);
            let how = text::how_via(binary, reference.to, reference.kind, via_veneer);
            let more = match references.len() {
                1 => String::new(),
                count => format!(" and {} more", count - 1),
            };
            // The name only when the instruction doesn't say it already.
            line.note = if via_veneer.is_some() {
                format!("{how} → {name}{more}")
            } else if !line.text.is_empty() && line.text.ends_with(&name) {
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

    /// The note of a line that refers to this function or to a constant.
    ///
    /// The note is `jump ↓` or `address ↑` for an address in this function,
    /// or `= 0x028f5c28` for the word or double that a load from a literal
    /// pool reads. It links to the line it refers to, if any.
    fn inside(&self, line: &mut ListingLine, refers: Refers) -> String {
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

    /// The instruction's text, with the mnemonic in 7 columns.
    ///
    /// A call or jump names the item it goes to in place of its address,
    /// unless it goes to another line of this function.
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

    /// The row name of item `index`, with how far into it `address` is.
    ///
    /// The offset is there only when `address` isn't the item's start:
    /// `frames+0x8`.
    fn name_at(&self, index: usize, address: u64) -> String {
        let name = self.session.names.row_name(index);
        match address.saturating_sub(self.session.binary.items[index].address) {
            0 => name,
            offset => format!("{name}+{offset:#x}"),
        }
    }

    /// What `reference` uses, as its note names it, and where its line
    /// links (see [`UseLink`] for when the two veneers differ).
    pub(super) fn use_link(&self, reference: &Reference) -> UseLink {
        let binary = self.session.binary;
        if let Node::Item(veneer) = reference.to
            && reference.kind.is_call()
            && let Some(&target) = self.session.veneers.get(&veneer)
        {
            return match target {
                VeneerTarget::Node(node) => {
                    let at = match node {
                        Node::Item(index) => Some(binary.items[index].address),
                        Node::Import(_) => None,
                    };
                    UseLink {
                        name: self.session.row_name(node),
                        via_veneer: Some(veneer),
                        link: Link::Node {
                            page: PageAt { node, at },
                            via_veneer: Some(veneer),
                        },
                    }
                }
                // The veneer is all there is to open.
                VeneerTarget::Outside(address) => {
                    let page = PageAt {
                        node: reference.to,
                        at: Some(binary.items[veneer].address),
                    };
                    UseLink {
                        name: format!("{address:#x}, outside any function"),
                        via_veneer: Some(veneer),
                        link: Link::Node {
                            page,
                            via_veneer: None,
                        },
                    }
                }
            };
        }
        let (name, at) = match reference.to {
            Node::Item(index) => {
                let item = &binary.items[index];
                // A use of data, or a pointer to it, can be inside it; a
                // call, or a pointer to code, is to its start.
                let at = match self.used.get(&reference.site) {
                    Some(&used) if item.kind == ItemKind::Data && item.contains(used) => used,
                    _ => item.address,
                };
                let mut name = self.name_at(index, at);
                if let Some(string) = string_at(self.session, index, at) {
                    name += &format!(" {string}");
                }
                (name, Some(at))
            }
            Node::Import(_) => (self.session.row_name(reference.to), None),
        };
        let page = PageAt {
            node: reference.to,
            at,
        };
        UseLink {
            name,
            via_veneer: None,
            link: Link::Node {
                page,
                via_veneer: None,
            },
        }
    }

    /// The section of the item that `link` goes to, if in another block.
    ///
    /// That is a block of memory other than this item's.
    pub(super) fn elsewhere(&self, link: Link) -> Option<String> {
        let Link::Node {
            page: PageAt {
                node: Node::Item(to),
                ..
            },
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
