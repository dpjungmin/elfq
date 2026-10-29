//! How elfq words things: the counts, lists, columns and sentences that
//! commands write, kept in one place so that every command says them the
//! same way.

use object::Architecture;

use super::json;
use crate::binary::{AddressRange, Binary, Item, ItemKind, Section, Source};
use crate::graph::{Node, ReferenceKind, Root, RootKind, Unresolved};

/// "1 function", "2 functions".
pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "1 byte", "2 bytes".
pub(super) fn bytes_text(n: u64) -> String {
    format!("{n} {}", if n == 1 { "byte" } else { "bytes" })
}

/// A change in a number of bytes, with its sign: "+16", "-16", or "0".
pub(super) fn signed(change: i128) -> String {
    if change > 0 {
        format!("+{change}")
    } else {
        change.to_string()
    }
}

/// The bytes of every item, or with a range, those in it, and what they
/// are: "200 bytes of code", "16 bytes of data" or "216 bytes of code and
/// data".
pub(super) fn bytes_of_items(binary: &Binary, range: Option<AddressRange>) -> String {
    let bytes_of = |kind: ItemKind| {
        let items = binary.items.iter().filter(|item| item.kind == kind);
        super::total(items.map(|item| binary.bytes_in(item, range)))
    };
    let (code, data) = (bytes_of(ItemKind::Function), bytes_of(ItemKind::Data));
    let what = match (code > 0, data > 0) {
        (true, false) => "code",
        (false, true) => "data",
        _ => "code and data",
    };
    format!("{} of {what}", bytes_text(code.saturating_add(data)))
}

/// Bytes of code and of data, kept apart: "446 bytes of code and 4128 bytes
/// of data".
pub(super) fn code_and_data(code: u64, data: u64) -> String {
    format!(
        "{} of code and {} of data",
        bytes_text(code),
        bytes_text(data)
    )
}

/// An item's size, `~N` when it is an estimate, or `?` when it is unknown.
pub(super) fn size_text(item: &Item) -> String {
    match (item.size, item.size_estimated) {
        (0, _) => "?".to_string(),
        (size, true) => format!("~{size}"),
        (size, false) => size.to_string(),
    }
}

/// "3 functions", "2 data objects" or "4 items", whichever fits `nodes`.
pub(super) fn count_text(binary: &Binary, nodes: &[Node]) -> String {
    let kind = |node: &Node| match node {
        Node::Item(index) => Some(binary.items[*index].kind),
        Node::Import(_) => None,
    };
    match nodes.first().map(kind) {
        Some(first) if nodes.iter().all(|node| kind(node) == first) => match first {
            Some(ItemKind::Function) => plural(nodes.len(), "function", "functions"),
            Some(ItemKind::Data) => plural(nodes.len(), "data object", "data objects"),
            None => plural(nodes.len(), "import", "imports"),
        },
        _ => plural(nodes.len(), "item", "items"),
    }
}

/// How many of the largest items the tables of `top`, `retained` and
/// `inlined`, and those of `callers SYMBOL` and `callees SYMBOL`, show. A
/// row such as "6 more, kept by main" sums up the rest, and an item that
/// would be left out on its own is shown too.
const SHOWN: usize = 30;

/// How many of the largest items to show: [`SHOWN`], or with `--all`, every
/// one.
pub(super) fn shown(all: bool) -> usize {
    if all { usize::MAX } else { SHOWN }
}

/// How many of `count` things to show where `room` fit: all of them, or
/// `room` and a summary of the rest, such as "and 5 more". A summary never
/// stands for one thing, since naming the thing takes no more room.
pub(super) fn shown_count(count: usize, room: usize) -> usize {
    if count > room.saturating_add(1) {
        room
    } else {
        count
    }
}

/// Words joined into a phrase: "a", "a and b", "a, b and c", or for more
/// than `room` and one, the first `room` of them and "and 5 more".
fn phrase(words: &[&str], room: usize) -> String {
    let shown = shown_count(words.len(), room);
    match words {
        _ if shown < words.len() => format!(
            "{} and {} more",
            words[..shown].join(", "),
            words.len() - shown
        ),
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Names for a column, such as the exceptions in `reach`'s WHY: "a", "a and
/// b", "a, b and c", or for more than three, "a, b and 5 more". The column
/// comes before the name, so it stays short, even with `--all`; the JSON
/// has every one.
pub(super) fn column_names(names: &[&str]) -> String {
    phrase(names, 2)
}

/// How many names a list of names in a sentence names before "and 5 more",
/// unless `--all` asks for every one.
const NAMED: usize = 10;

/// How many names to name: [`NAMED`], or with `--all`, every one.
pub(super) fn named(all: bool) -> usize {
    if all { usize::MAX } else { NAMED }
}

/// Names joined into a phrase: "a", "a and b", "a, b and c", or for more
/// than eleven, "a, b, … and 5 more" unless `all`.
pub(super) fn names_text(names: &[&str], all: bool) -> String {
    phrase(names, named(all))
}

/// How the notes of `why` and `retained` name the items they list: as the
/// commands do, [`NAMED`] of them, or with `--all`, every one; or as
/// browse does on a screen with little room for a note, only the first,
/// shortened to fit.
pub(super) struct Naming<'f> {
    /// How many names it names before "and 5 more".
    named: usize,
    /// What it writes for each name, when not the name in full.
    shorten: Option<&'f dyn Fn(&str) -> String>,
}

impl<'f> Naming<'f> {
    /// As the commands name them, every one with `all`.
    pub(super) fn of(all: bool) -> Naming<'static> {
        Naming {
            named: named(all),
            shorten: None,
        }
    }

    /// Only the first, as `shorten` writes it.
    #[cfg(feature = "tui")]
    pub(super) fn first(shorten: &'f dyn Fn(&str) -> String) -> Naming<'f> {
        Naming {
            named: 1,
            shorten: Some(shorten),
        }
    }

    /// The names joined into a phrase, as [`names_text`] joins them: "a",
    /// "a and b", "a, b and c", or "a, b, … and 5 more".
    pub(super) fn phrase(&self, names: &[&str]) -> String {
        let (shown, more) = self.shown(names);
        let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
        match more {
            0 => phrase(&shown, usize::MAX),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    }

    /// The names as a list: "a, b, c", or "a, b, … and 5 more".
    pub(super) fn list(&self, names: &[&str]) -> String {
        let (shown, more) = self.shown(names);
        match more {
            0 => shown.join(", "),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    }

    /// The names it names, as it writes them, and how many it leaves out.
    fn shown(&self, names: &[&str]) -> (Vec<String>, usize) {
        let shown = shown_count(names.len(), self.named);
        let write = |name: &&str| match self.shorten {
            Some(shorten) => shorten(name),
            None => name.to_string(),
        };
        (
            names[..shown].iter().map(write).collect(),
            names.len() - shown,
        )
    }
}

/// Addresses for a sentence: "0x1795", "0x1795 and 0x17d4", or for more
/// than four, "0x1795, 0x17d4, 0x17e0 and 3 more" unless `all`.
pub(super) fn addresses_text(sites: &[u64], all: bool) -> String {
    let sites: Vec<String> = sites.iter().map(|site| format!("{site:#x}")).collect();
    let sites: Vec<&str> = sites.iter().map(String::as_str).collect();
    phrase(&sites, if all { usize::MAX } else { 3 })
}

/// Addresses for the AT column: "0x1795", "0x1795, 0x17d4", or for more
/// than two, "0x1795 and 2 more". The column comes before the name, so it
/// stays short; the JSON has every site.
pub(super) fn at_column(sites: &[u64]) -> String {
    let shown: Vec<String> = sites
        .iter()
        .take(shown_count(sites.len(), 1))
        .map(|site| format!("{site:#x}"))
        .collect();
    match sites.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// How a reference of `kind` uses `to`, for the HOW column. Code that
/// reads, writes or loads the address of a data object "uses" it, on every
/// architecture, while "address taken" means a pointer to a function. An
/// import keeps "address taken", since elfq doesn't record yet whether it
/// is a function or data.
pub(super) fn how_column(binary: &Binary, to: Node, kind: ReferenceKind) -> &'static str {
    let is_data = match to {
        Node::Item(index) => binary.items[index].kind == ItemKind::Data,
        Node::Import(_) => false,
    };
    match kind {
        ReferenceKind::Access => "uses",
        ReferenceKind::Address if is_data => "uses",
        kind => json::how_field(kind),
    }
}

/// How a reference of `kind` uses `to`, for the HOW column, as
/// [`how_column`] says it, with "via veneer" after a call or tail call
/// through `veneer`, or "via PLT" when `veneer` is a PLT stub.
pub(super) fn how_via(
    binary: &Binary,
    to: Node,
    kind: ReferenceKind,
    veneer: Option<usize>,
) -> &'static str {
    let plt = veneer.is_some_and(|veneer| binary.items[veneer].source == Source::Plt);
    match (kind, veneer, plt) {
        (ReferenceKind::Call, Some(_), false) => "call via veneer",
        (ReferenceKind::Call, Some(_), true) => "call via PLT",
        (ReferenceKind::TailCall, Some(_), false) => "tail call via veneer",
        (ReferenceKind::TailCall, Some(_), true) => "tail call via PLT",
        (kind, _, _) => how_column(binary, to, kind),
    }
}

/// "call" or "jump", for a call or jump that elfq can't follow.
pub(super) fn call_or_jump(unresolved: &Unresolved) -> &'static str {
    if unresolved.is_call { "call" } else { "jump" }
}

/// "1 call", "2 jumps" or "1 call and 2 jumps", for calls and jumps that
/// elfq can't follow.
pub(super) fn calls_and_jumps<'a>(unresolved: impl IntoIterator<Item = &'a Unresolved>) -> String {
    let unresolved: Vec<&Unresolved> = unresolved.into_iter().collect();
    let calls = unresolved.iter().filter(|u| u.is_call).count();
    let jumps = unresolved.len() - calls;
    match (calls, jumps) {
        (_, 0) => plural(calls, "call", "calls"),
        (0, _) => plural(jumps, "jump", "jumps"),
        _ => format!(
            "{} and {}",
            plural(calls, "call", "calls"),
            plural(jumps, "jump", "jumps")
        ),
    }
}

/// The verbs for the calls and jumps in `unresolved`, after one name and
/// after several: "calls" and "call", "jumps" and "jump", or "calls and
/// jumps" and "call and jump".
pub(super) fn call_and_jump_verbs<'a>(
    unresolved: impl IntoIterator<Item = &'a Unresolved>,
) -> (&'static str, &'static str) {
    let (mut calls, mut jumps) = (false, false);
    for unresolved in unresolved {
        calls |= unresolved.is_call;
        jumps |= !unresolved.is_call;
    }
    match (calls, jumps) {
        (true, true) => ("calls and jumps", "call and jump"),
        (true, false) => ("calls", "call"),
        (false, _) => ("jumps", "jump"),
    }
}

/// The sentence that says elfq can't read `unreadable`, the functions whose
/// end it doesn't know, as `callees SYMBOL` says it.
pub(super) fn unreadable_text(binary: &Binary, unreadable: &[usize], all: bool) -> String {
    let names: Vec<&str> = unreadable
        .iter()
        .map(|&index| binary.items[index].name.as_str())
        .collect();
    format!(
        "elfq doesn't know where {} {}, so it can't read {} code.",
        names_text(&names, all),
        if names.len() == 1 { "ends" } else { "end" },
        if names.len() == 1 { "its" } else { "their" }
    )
}

/// Examples of the names that elfq gives items without symbols: the first
/// unnamed function, such as `fn_0x10a0`, and the first unnamed data object,
/// such as `.rodata@0x2004`, if there is one. `None` when no function is
/// unnamed: unnamed data is common even with symbols, such as string
/// literals, so it needs explaining only in files that have unnamed
/// functions too.
pub(super) fn unnamed_items(binary: &Binary) -> Option<(&Item, Option<&Item>)> {
    let unnamed = |sources: &[Source]| {
        binary
            .items
            .iter()
            .find(|item| sources.contains(&item.source))
    };
    let function = unnamed(&[Source::EhFrame, Source::Reached])?;
    Some((function, unnamed(&[Source::Reference])))
}

/// Says that nothing reaches the item called `name` from where the program
/// starts.
pub(super) fn nothing_reaches(name: &str) -> String {
    format!("Nothing reaches {name}: the program never uses it, or elfq can't see how.")
}

/// Why `root`, called `name`, is one, as a sentence.
pub(super) fn root_sentence(binary: &Binary, name: &str, root: &Root) -> String {
    // Whether the program uses a startup section when it starts or exits.
    let when = |section: usize| {
        let section = &binary.sections[section].name;
        if section.contains("fini") || section.contains("dtors") {
            "exits"
        } else {
            "starts"
        }
    };
    match root.kind {
        RootKind::Entry => format!("{name} is the entry point, where the program starts."),
        RootKind::InSection(section)
            if binary.sections[section].kind == object::SectionKind::Note =>
        {
            format!(
                "{name} is a note in {}, which the system reads when it loads the program.",
                binary.sections[section].name
            )
        }
        RootKind::InSection(section) => format!(
            "{name} is in {}, which the program uses when it {}.",
            binary.sections[section].name,
            when(section)
        ),
        RootKind::ListedIn(section) => format!(
            "{name} is listed in {}, so the program runs it when it {}.",
            binary.sections[section].name,
            when(section)
        ),
        RootKind::PltHeader => {
            format!("{name} is the PLT header, which the dynamic loader uses to find imports.")
        }
        RootKind::Personality => format!(
            "{name} is a personality routine, which the unwinder calls on a panic or exception."
        ),
        RootKind::VectorTable => {
            format!("{name} holds the vector table, where the processor finds its handlers.")
        }
        // The reset handler, when it isn't the entry point.
        RootKind::Handler if is_reset_handler(binary, root.node) => format!(
            "{name} is the reset handler in the vector table: the processor runs it first, at reset."
        ),
        RootKind::Handler => format!(
            "{name} is in the vector table: the processor runs it on an interrupt or exception."
        ),
    }
}

/// Whether `node` is the item where the vector table's reset handler
/// starts.
fn is_reset_handler(binary: &Binary, node: Node) -> bool {
    let reset = (binary.vector_table.as_ref()).and_then(|table| table.reset_handler());
    matches!(node, Node::Item(index) if Some(binary.items[index].address) == reset)
}

/// Why a root is one, for the HOW column: as [`json::root_field`] says
/// it, but shorter for a handler.
pub(super) fn root_column(binary: &Binary, kind: RootKind) -> String {
    match kind {
        RootKind::Handler => "vector table".to_string(),
        _ => json::root_field(binary, kind),
    }
}

/// The name of an architecture as elfq prints it: `x86-64`, `32-bit Arm`,
/// or for any other, the `object` crate's name for it, such as `Aarch64`.
pub(super) fn architecture_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => "x86-64".to_string(),
        Architecture::Arm => "32-bit Arm".to_string(),
        other => format!("{other:?}"),
    }
}

/// Why elfq has no functions in a file but its entry point.
pub(super) const NO_FUNCTIONS: &str =
    "elfq can't yet find functions without symbols or an unwind table";

/// Says that elfq can't read the file's code yet, and so can't tell what
/// calls what.
pub(super) fn cannot_read_code(binary: &Binary) -> String {
    let code = match binary.architecture {
        // Thumb code is readable; this is the older instruction set.
        Architecture::Arm => "32-bit Arm (A32)".to_string(),
        other => architecture_name(other),
    };
    format!("elfq can't yet read {code} code to tell what calls what")
}

/// The names of the sections that have bytes in `range`.
pub(super) fn sections_in(binary: &Binary, range: AddressRange) -> Vec<&str> {
    binary
        .sections
        .iter()
        .filter(|section| section.allocated && range.overlap(section.address, section.size) > 0)
        .map(|section| section.name.as_str())
        .collect()
}

/// The sections whose load images have bytes in `range`.
pub(super) fn load_images_in(binary: &Binary, range: AddressRange) -> Vec<&Section> {
    binary
        .sections
        .iter()
        .filter(|section| {
            section
                .load_address
                .is_some_and(|address| range.overlap(address, section.size) > 0)
        })
        .collect()
}

/// A range as `--range` takes it, with the sections and load images that
/// have bytes in it, such as `0x00000000..0x000000e0 (.itcm)` or
/// `0x30000000..0x30000edc (.isr_vector, .text, .rodata and .ARM.exidx,
/// with the load images of .itcm and .data)`, every one with `all`.
pub(super) fn range_text(binary: &Binary, range: AddressRange, all: bool) -> String {
    let load_images: Vec<&str> = load_images_in(binary, range)
        .into_iter()
        .map(|section| section.name.as_str())
        .collect();
    let load_images = match load_images[..] {
        [] => None,
        [one] => Some(format!("the load image of {one}")),
        _ => Some(format!(
            "the load images of {}",
            names_text(&load_images, all)
        )),
    };
    let sections = sections_in(binary, range);
    let sections = (!sections.is_empty()).then(|| names_text(&sections, all));
    let range = range_argument(range);
    match (sections, load_images) {
        (None, None) => range,
        (Some(sections), None) => format!("{range} ({sections})"),
        (None, Some(load_images)) => format!("{range} ({load_images})"),
        (Some(sections), Some(load_images)) => format!("{range} ({sections}, with {load_images})"),
    }
}

/// A range as `--range` takes it, with the sections that have bytes in
/// it, every one with `all`: [`range_text`] without the load images, for
/// range checks, which look only where the program uses its code and data.
pub(super) fn range_sections_text(binary: &Binary, range: AddressRange, all: bool) -> String {
    let sections = sections_in(binary, range);
    match sections[..] {
        [] => range_argument(range),
        _ => format!("{} ({})", range_argument(range), names_text(&sections, all)),
    }
}

/// A range as `--range` takes it, its addresses padded to 8 hex digits so
/// that a column of ranges lines up, such as `0x00000000..0x000000e0`.
pub(super) fn range_argument(range: AddressRange) -> String {
    range.text(range_digits(range))
}

/// Where a block of memory starts, as [`range_argument`] writes it, for a
/// column's header: "0x20000000".
pub(super) fn range_start(range: AddressRange) -> String {
    format!("{:#0width$x}", range.start, width = range_digits(range) + 2)
}

/// How many hex digits [`range_argument`] gives each address of `range`:
/// as many as its end needs, and at least 8.
fn range_digits(range: AddressRange) -> usize {
    format!("{:x}", range.end).len().max(8)
}

/// `text` as one shell word: quoted when it holds anything but letters,
/// digits and `_./@:+-`.
pub(super) fn shell_quote(text: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./@:+-".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_never_stands_for_one_thing() {
        let names: Vec<String> = (1..=12).map(|n| format!("f{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            names_text(&names[..11], false),
            "f1, f2, f3, f4, f5, f6, f7, f8, f9, f10 and f11"
        );
        assert_eq!(
            names_text(&names, false),
            "f1, f2, f3, f4, f5, f6, f7, f8, f9, f10 and 2 more"
        );
        assert_eq!(
            names_text(&names, true),
            "f1, f2, f3, f4, f5, f6, f7, f8, f9, f10, f11 and f12"
        );
        assert_eq!(at_column(&[1]), "0x1");
        assert_eq!(at_column(&[1, 2]), "0x1, 0x2");
        assert_eq!(at_column(&[1, 2, 3]), "0x1 and 2 more");
        assert_eq!(
            addresses_text(&[1, 2, 3, 4], false),
            "0x1, 0x2, 0x3 and 0x4"
        );
        assert_eq!(
            addresses_text(&[1, 2, 3, 4, 5], false),
            "0x1, 0x2, 0x3 and 2 more"
        );
        assert_eq!(
            addresses_text(&[1, 2, 3, 4, 5], true),
            "0x1, 0x2, 0x3, 0x4 and 0x5"
        );
    }

    #[test]
    fn a_sentence_joins_its_last_two_words_with_and() {
        assert_eq!(addresses_text(&[0x10], false), "0x10");
        assert_eq!(addresses_text(&[0x10, 0x20], false), "0x10 and 0x20");
    }

    #[test]
    fn one_byte_is_a_byte() {
        assert_eq!(bytes_text(0), "0 bytes");
        assert_eq!(bytes_text(1), "1 byte");
        assert_eq!(bytes_text(2), "2 bytes");
    }
}
