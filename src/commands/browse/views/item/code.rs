//! What tab 1 of a function's page lists: every instruction, or where
//! elfq doesn't print the code, only the lines that use another item.

use object::Architecture;

use super::line_notes::LineNotes;
use super::listing::{Listing, ListingLine};
use crate::binary::{Binary, ItemKind};
use crate::commands::browse::session::Session;
use crate::commands::checks::readable_item;
use crate::graph::Node;

/// What a function's page lists.
enum Code {
    /// Every instruction, and the data among them.
    Instructions(Vec<ListingLine>),
    /// Only the lines that use another item, or that elfq can't follow,
    /// where elfq doesn't print the code, and why it doesn't.
    Links(Vec<ListingLine>, String),
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

/// Function `index`'s code, as tab 1 of its page shows it.
pub(super) fn function_listing(session: &Session, index: usize) -> Listing {
    let kind = ItemKind::Function;
    match code(session, index) {
        Ok(Code::Instructions(lines)) => Listing::new(kind, Some("INSTRUCTION".to_string()), lines),
        Ok(Code::Links(lines, why)) if lines.is_empty() => {
            let row_name = session.row_name(index);
            let why = format!("{why}. No line of {row_name} uses another item.");
            Listing::new(kind, None, lines).saying_instead(why)
        }
        Ok(Code::Links(lines, why)) => {
            let footnote = format!("{why}: these are the lines that use another item.");
            Listing::new(kind, None, lines).with_footnote(footnote)
        }
        Err(why) => Listing::new(kind, None, Vec::new()).saying_instead(why),
    }
}

/// The lines of function `index`'s code, or why there are none to show.
fn code(session: &Session, index: usize) -> Result<Code, String> {
    let binary = session.binary;
    let item = &binary.items[index];
    // Such as "elfq doesn't know where _init ends, so it can't read its
    // code", as the commands say it.
    readable_item(binary, Node::Item(index)).map_err(|error| error.to_string())?;
    let Some(bytes) = binary.bytes(item.address, item.size) else {
        return Err(format!(
            "the file doesn't hold the bytes of {}, so elfq can't read its code",
            item.name
        ));
    };
    let notes = LineNotes::new(session, index);
    let why = match no_text(binary) {
        None => match binary.listing(item, bytes) {
            Some(lines) => return Ok(Code::Instructions(notes.lines(&lines))),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::super::samples;
    use super::super::listing::Link;
    use super::*;

    /// Calls `check` with the session of every sample and the code of each
    /// of its functions that has some.
    fn every_function(mut check: impl FnMut(&Session, usize, &Code)) {
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let session = Session::new(&path, &binary).unwrap();
            for index in 0..binary.items.len() {
                if binary.items[index].kind == ItemKind::Function
                    && let Ok(code) = code(&session, index)
                {
                    check(&session, index, &code);
                }
            }
        }
    }

    fn lines(code: &Code) -> &[ListingLine] {
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
        let (mut links, mut imports) = (0, 0);
        every_function(|session, _, code| {
            for line in lines(code) {
                let shown = format!("{} {}", line.text, line.note);
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
