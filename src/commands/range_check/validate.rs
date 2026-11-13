//! The checks a range check makes before it answers: that each range
//! holds something, and that the ranges of `--from` or `--to` are on the
//! far side. An empty range is refused by `checks::empty_place`, which
//! [`check_holds_items`] calls.

use anyhow::{Result, bail};

use super::{Direction, Inside, RangeCheck};
use crate::binary::{AddressRange, Binary, Item};
use crate::commands::checks::empty_place;
use crate::commands::text::{self, Detail};
use crate::graph::{Graph, ItemsInRanges};

/// Checks the ranges that `check` asks about, and those of `--from` or
/// `--to`, in `binary`, whose graph is `graph`. `items` are the items that
/// `check` asks about.
pub(super) fn check(
    binary: &Binary,
    graph: &Graph,
    check: &RangeCheck,
    items: &ItemsInRanges,
) -> Result<()> {
    let code_outside = graph.code_outside_items();
    for &range in check.inside.ranges() {
        // `callers` can ask what calls code outside every item, as in a
        // chip's ROM.
        if check.direction == Direction::In && has_address(&code_outside, range) {
            continue;
        }
        check_holds_items(binary, range)?;
    }
    check_far_side(binary, check, items, &code_outside)?;
    check_nothing_shared(binary, check, items, &code_outside)
}

/// Whether one of `addresses` is in `range`.
fn has_address(addresses: &[u64], range: AddressRange) -> bool {
    addresses.iter().any(|&address| range.contains(address))
}

/// Explains that no item is in `range`, as for a mistyped range, unless
/// one is. `retained` refuses an empty range in place of SYMBOL with it
/// too.
pub(in crate::commands) fn check_holds_items(binary: &Binary, range: AddressRange) -> Result<()> {
    if binary.items.iter().any(|item| item.is_in(&[range])) {
        return Ok(());
    }
    let place = empty_place(binary, Some(range))?;
    // Where the program uses what is stored in the range.
    let used_at: Vec<String> = binary
        .load_images_in(range)
        .into_iter()
        .map(|section| {
            let used_at = AddressRange {
                start: section.address,
                end: section.address + section.size,
            };
            format!("{} at {}", section.name, text::range_argument(used_at))
        })
        .collect();
    if !used_at.is_empty() {
        let used_at: Vec<&str> = used_at.iter().map(String::as_str).collect();
        bail!(
            "elfq found no functions or data in {place}; the program uses {}",
            text::names_text(&used_at, Detail::Brief)
        );
    }
    bail!("elfq found no functions or data in {place}");
}

/// Checks the ranges of `--from` or `--to`: each must hold an item outside
/// what `check` asks about, whose items are `items`, since the far end of
/// each use is outside them, or for `--to`, code outside every item and
/// outside the ranges asked about that the file calls or jumps to, at one
/// of `code_outside`, as in a chip's ROM. Else it is most likely mistyped.
fn check_far_side(
    binary: &Binary,
    check: &RangeCheck,
    items: &ItemsInRanges,
    code_outside: &[u64],
) -> Result<()> {
    let inside = &check.inside;
    let it = inside.it(binary);
    let lists = match check.direction {
        Direction::Out => format!(
            "callees lists only what {} outside {it}",
            if inside.one(binary) {
                "it uses"
            } else {
                "they use"
            }
        ),
        Direction::In => format!("callers lists only what uses {it} from outside"),
    };
    for &range in check.far_side {
        let called = check.direction == Direction::Out && has_address(code_outside, range);
        match check.direction {
            Direction::Out => check_to_range(binary, range, code_outside)?,
            Direction::In => check_holds_items(binary, range)?,
        }
        let item_outside = (binary.items.iter().enumerate())
            .any(|(index, item)| item.is_in(&[range]) && !items.contains_item(index));
        let code_outside_them = called
            && code_outside
                .iter()
                .any(|&address| range.contains(address) && !items.contains_address(address));
        if !item_outside && !code_outside_them {
            bail!(
                "{} {} holds nothing outside {}, and {lists}",
                check.direction.option(),
                text::range_argument(range),
                asked_about(binary, inside)
            );
        }
    }
    Ok(())
}

/// What is inside a range check's boundary: an item, or code outside every
/// item that the file calls or jumps to, as in a chip's ROM.
#[derive(Clone, Copy)]
enum Held<'a> {
    Item(&'a Item),
    Code(u64),
}

impl Held<'_> {
    fn is_in(self, range: AddressRange) -> bool {
        match self {
            Held::Item(item) => item.is_in(&[range]),
            Held::Code(address) => range.contains(address),
        }
    }
}

/// Checks that, when what `check` asks about, whose items are `items`,
/// holds something outside the ranges of `--from` or `--to`, nothing is in
/// both, since each end of a use would then be on either side.
/// `code_outside` is where the file calls or jumps to code outside every
/// item. Only what the ranges asked about hold can be in both: an item
/// that a set's SYMBOL or pattern picked outside them is inside wherever
/// it lies, as one SYMBOL's item is.
fn check_nothing_shared(
    binary: &Binary,
    check: &RangeCheck,
    items: &ItemsInRanges,
    code_outside: &[u64],
) -> Result<()> {
    // What is inside: what the ranges asked about hold, and the other items
    // of a set.
    let held_items = (items.indices()).map(|index| Held::Item(&binary.items[index]));
    let held_code = code_outside
        .iter()
        .filter(|&&address| items.contains_address(address))
        .map(|&address| Held::Code(address));
    let first_holding = |ranges: &[AddressRange], held: Held| {
        ranges.iter().copied().find(|&range| held.is_in(range))
    };
    // The first thing in both, with the range of the option and the range
    // asked about that hold it.
    let mut shared = None;
    let mut outside = false;
    for held in held_items.chain(held_code) {
        let far_range = first_holding(check.far_side, held);
        match (far_range, first_holding(check.inside.ranges(), held)) {
            (Some(far_range), Some(range)) => {
                shared.get_or_insert((held, far_range, range));
            }
            // A set's item, picked outside the ranges asked about.
            (Some(_), None) => {}
            (None, _) => outside = true,
        }
    }
    if outside && let Some((held, far_range, range)) = shared {
        let what = match held {
            Held::Item(item) => item.name.clone(),
            Held::Code(address) => format!("the code at {address:#x}"),
        };
        bail!(
            "{} {} and {} both hold {what}; give ranges that don't share it",
            check.direction.option(),
            text::range_argument(far_range),
            text::range_argument(range)
        );
    }
    Ok(())
}

/// What a range check asks about, `inside`, as its errors name it: SYMBOL,
/// the ranges as arguments, or a set as its first line names it, with
/// every range and section.
fn asked_about(binary: &Binary, inside: &Inside) -> String {
    match inside {
        Inside::Ranges(ranges) => {
            let texts: Vec<String> = ranges
                .iter()
                .map(|&range| text::range_argument(range))
                .collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            text::names_text(&texts, Detail::All)
        }
        Inside::Symbol { lead_name, .. } => lead_name.clone(),
        Inside::Set(set) => set.lead_name(binary, Detail::All).to_string(),
    }
}

/// Explains that `range`, given to `--to`, holds nothing that code can use,
/// as for a mistyped range, unless it holds an item, or code outside every
/// item that the file calls or jumps to, at one of `code_outside`, as in a
/// chip's ROM.
pub(in crate::commands) fn check_to_range(
    binary: &Binary,
    range: AddressRange,
    code_outside: &[u64],
) -> Result<()> {
    if has_address(code_outside, range) {
        return Ok(());
    }
    if binary.sections_in(range).is_empty() && binary.load_images_in(range).is_empty() {
        bail!(
            "--to {} holds no functions or data, and no code in the file calls or jumps there",
            text::range_argument(range)
        );
    }
    check_holds_items(binary, range)
}
