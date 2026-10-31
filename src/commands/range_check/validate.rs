//! The checks a range check makes before it answers: that each range
//! holds something, and that the ranges of `--from` or `--to` are on the
//! far side. An empty range is refused by `checks::empty_place`, which
//! [`check_holds_items`] calls.

use anyhow::{Result, bail};

use super::{Direction, RangeCheck};
use crate::binary::{AddressRange, Binary, Item};
use crate::commands::checks::empty_place;
use crate::commands::text;
use crate::graph::Graph;

/// Checks the ranges that `check` asks about, and those of `--from` or
/// `--to`, in `binary`, whose graph is `graph`.
pub(super) fn check(binary: &Binary, graph: &Graph, check: &RangeCheck) -> Result<()> {
    let code_outside = graph.code_outside_items();
    for &range in check.ranges {
        // `callers` can ask what calls code outside every item, as in a
        // chip's ROM.
        if check.direction == Direction::In && has_address(&code_outside, range) {
            continue;
        }
        check_holds_items(binary, range)?;
    }
    check_far_side(binary, check, &code_outside)?;
    check_nothing_shared(binary, check, &code_outside)
}

/// Whether `address` is in one of `ranges`.
fn contains(ranges: &[AddressRange], address: u64) -> bool {
    ranges.iter().any(|range| range.contains(address))
}

/// Whether one of `addresses` is in `range`.
fn has_address(addresses: &[u64], range: AddressRange) -> bool {
    addresses.iter().any(|&address| range.contains(address))
}

/// Explains that no item is in `range`, as for a mistyped range, unless
/// one is.
fn check_holds_items(binary: &Binary, range: AddressRange) -> Result<()> {
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
            text::names_text(&used_at, false)
        );
    }
    bail!("elfq found no functions or data in {place}");
}

/// Checks the ranges of `--from` or `--to`: each must hold an item outside
/// the ranges that `check` asks about, since the far end of each use is
/// outside them, or for `--to`, code outside every item and outside those
/// ranges that the file calls or jumps to, at one of `code_outside`, as in
/// a chip's ROM. Else it is most likely mistyped.
fn check_far_side(binary: &Binary, check: &RangeCheck, code_outside: &[u64]) -> Result<()> {
    let it = check.it();
    let lists = match check.direction {
        Direction::Out => format!(
            "callees lists only what {} outside {it}",
            if check.one() { "it uses" } else { "they use" }
        ),
        Direction::In => format!("callers lists only what uses {it} from outside"),
    };
    for &range in check.far_side {
        let called = check.direction == Direction::Out && has_address(code_outside, range);
        match check.direction {
            Direction::Out => check_to_range(binary, range, code_outside)?,
            Direction::In => check_holds_items(binary, range)?,
        }
        let item_outside = binary
            .items
            .iter()
            .any(|item| item.is_in(&[range]) && !item.is_in(check.ranges));
        let code_outside_them = called
            && code_outside
                .iter()
                .any(|&address| range.contains(address) && !contains(check.ranges, address));
        if !item_outside && !code_outside_them {
            bail!(
                "{} {} holds nothing outside {}, and {lists}",
                check.direction.option(),
                text::range_argument(range),
                asked_about(check)
            );
        }
    }
    Ok(())
}

/// What the ranges that a range check asks about hold: an item, or code
/// outside every item that the file calls or jumps to, as in a chip's ROM.
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

/// Checks that, when the ranges that `check` asks about hold something
/// outside the ranges of `--from` or `--to`, nothing is in both, since each
/// end of a use would then be on either side. `code_outside` is where the
/// file calls or jumps to code outside every item.
fn check_nothing_shared(binary: &Binary, check: &RangeCheck, code_outside: &[u64]) -> Result<()> {
    // What the ranges hold, with the range of the option that holds it too,
    // if any.
    let items = binary
        .items
        .iter()
        .filter(|item| item.is_in(check.ranges))
        .map(Held::Item);
    let code = code_outside
        .iter()
        .filter(|&&address| contains(check.ranges, address))
        .map(|&address| Held::Code(address));
    let mut shared = None;
    let mut outside = false;
    for held in items.chain(code) {
        match check
            .far_side
            .iter()
            .copied()
            .find(|&range| held.is_in(range))
        {
            Some(range) => {
                shared.get_or_insert((held, range));
            }
            None => outside = true,
        }
    }
    if outside && let Some((held, far_range)) = shared {
        let what = match held {
            Held::Item(item) => item.name.clone(),
            Held::Code(address) => format!("the code at {address:#x}"),
        };
        let range = check
            .ranges
            .iter()
            .find(|&&range| held.is_in(range))
            .map_or_else(|| asked_about(check), |&range| text::range_argument(range));
        bail!(
            "{} {} and {range} both hold {what}; give ranges that don't share it",
            check.direction.option(),
            text::range_argument(far_range)
        );
    }
    Ok(())
}

/// The ranges that `check` asks about, as its errors name them: SYMBOL, or
/// the ranges as arguments.
fn asked_about(check: &RangeCheck) -> String {
    match &check.name {
        Some(name) => name.clone(),
        None => {
            let texts: Vec<String> = check
                .ranges
                .iter()
                .map(|&range| text::range_argument(range))
                .collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            text::names_text(&texts, true)
        }
    }
}

/// Explains that `range`, given to `--to`, holds nothing that code can use,
/// as for a mistyped range, unless it holds an item, or code outside every
/// item that the file calls or jumps to, at one of `code_outside`, as in a
/// chip's ROM.
pub(crate) fn check_to_range(
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
