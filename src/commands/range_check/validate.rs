//! The checks a range check makes before it answers: that each range
//! holds something, and that the ranges of `--from` or `--to` are on the
//! far side. An empty range is refused by `checks::empty_place`, which
//! [`check_holds_items`] calls.

use anyhow::{Result, bail};

use super::{Direction, RangeCheck};
use crate::binary::{AddressRange, Binary};
use crate::commands::checks::empty_place;
use crate::commands::text;

/// Whether `address` is in one of `ranges`.
fn contains(ranges: &[AddressRange], address: u64) -> bool {
    ranges.iter().any(|range| range.contains(address))
}

/// Whether one of `addresses` is in `range`.
pub(super) fn has_address(addresses: &[u64], range: AddressRange) -> bool {
    addresses.iter().any(|&address| range.contains(address))
}

/// Explains that no item is in `range`, as for a mistyped range, unless
/// one is.
pub(super) fn check_holds_items(binary: &Binary, range: AddressRange) -> Result<()> {
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

/// Checks the ranges of `--from` or `--to`. Each must hold an item outside
/// the ranges that `check` asks about, since the other end of each use is
/// outside them, or for `--to`, code outside every item and outside those
/// ranges that the file calls or jumps to, at one of `code_outside`, as in
/// a chip's ROM. Else it is most likely mistyped. And when the ranges hold
/// something outside them, nothing may be in both, since each end of a use
/// would then be on either side.
pub(super) fn check_other_side(
    binary: &Binary,
    check: &RangeCheck,
    code_outside: &[u64],
) -> Result<()> {
    let option = check.option();
    let it = check.it();
    let lists = match check.direction {
        Direction::Out => format!(
            "callees lists only what {} outside {it}",
            if check.one() { "it uses" } else { "they use" }
        ),
        Direction::In => format!("callers lists only what uses {it} from outside"),
    };
    let inside = || match &check.name {
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
    };
    for &range in check.other {
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
                "{option} {} holds nothing outside {}, and {lists}",
                text::range_argument(range),
                inside()
            );
        }
    }
    // What the ranges hold, items and code outside every item that the file
    // calls, each with the range of the option that holds it too, if any.
    let items = binary
        .items
        .iter()
        .filter(|item| item.is_in(check.ranges))
        .map(|item| (Some(item), item.address));
    let code = code_outside
        .iter()
        .filter(|&&address| contains(check.ranges, address))
        .map(|&address| (None, address));
    let mut shared = None;
    let mut outside = false;
    for (item, address) in items.chain(code) {
        let other = check.other.iter().copied().find(|&range| match item {
            Some(item) => item.is_in(&[range]),
            None => range.contains(address),
        });
        match other {
            Some(range) => {
                shared.get_or_insert((item, address, range));
            }
            None => outside = true,
        }
    }
    if outside && let Some((item, address, other)) = shared {
        let (what, range) = match item {
            Some(item) => (
                item.name.clone(),
                check.ranges.iter().find(|&&range| item.is_in(&[range])),
            ),
            None => (
                format!("the code at {address:#x}"),
                check.ranges.iter().find(|range| range.contains(address)),
            ),
        };
        let range = range.map_or_else(inside, |&range| text::range_argument(range));
        bail!(
            "{option} {} and {range} both hold {what}; give ranges that don't share it",
            text::range_argument(other)
        );
    }
    Ok(())
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
