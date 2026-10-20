//! Address ranges: what `--range` reads, such as `0x20000000..0x20040000`
//! or `0x20000000+256K`, and the blocks of addresses that a file's sections
//! and their load images occupy, which the overview lists so that nobody has
//! to look them up.

use std::cmp::Ordering;
use std::fmt;
use std::num::ParseIntError;
use std::str::FromStr;

use super::Section;

/// The addresses from `start` up to, but not including, `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressRange {
    /// The first address in the range.
    pub start: u64,
    /// The address just past the range.
    pub end: u64,
}

impl AddressRange {
    /// How many of the `size` bytes at `address` fall in the range.
    pub fn overlap(&self, address: u64, size: u64) -> u64 {
        let start = address.max(self.start);
        let end = address.saturating_add(size).min(self.end);
        end.saturating_sub(start)
    }

    /// The range as `--range` takes it, with each address padded to
    /// `digits` hex digits so that ranges line up in a column.
    pub fn text(&self, digits: usize) -> String {
        format!(
            "{:#0width$x}..{:#0width$x}",
            self.start,
            self.end,
            width = digits + 2
        )
    }
}

impl fmt::Display for AddressRange {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#x}..{:#x}", self.start, self.end)
    }
}

impl FromStr for AddressRange {
    type Err = String;

    /// Reads `START..END` or `START+SIZE`.
    fn from_str(text: &str) -> Result<AddressRange, String> {
        let (start, end) = if let Some((start, end)) = text.split_once("..") {
            (number(start)?, number(end)?)
        } else if let Some((start, size)) = text.split_once('+') {
            let start = number(start)?;
            let end = start
                .checked_add(number(size)?)
                .ok_or("the range goes past the last address")?;
            (start, end)
        } else {
            return Err(format!(
                "expected START..END or START+SIZE, such as {}",
                example_range(text)
            ));
        };
        if end <= start {
            return Err("the range must end after it starts".to_string());
        }
        Ok(AddressRange { start, end })
    }
}

/// A range to show in an error about `text`, written as elfq takes it:
/// the range that `text` means when it puts `-` or `:` between two numbers,
/// as `0x0..0x40000` for `0x0-0x40000`, or `0x20000000+256K` for
/// `0x20000000:256K`, whose second number is too small to be its end.
/// Otherwise the usual examples.
pub fn example_range(text: &str) -> String {
    let meant = |(start, end): (&str, &str)| {
        let (start, end) = (start.trim(), end.trim());
        let range = match number(end).ok()?.cmp(&number(start).ok()?) {
            Ordering::Greater => format!("{start}..{end}"),
            Ordering::Less => format!("{start}+{end}"),
            Ordering::Equal => return None,
        };
        range.parse::<AddressRange>().is_ok().then_some(range)
    };
    text.split_once('-')
        .or_else(|| text.split_once(':'))
        .and_then(meant)
        .unwrap_or_else(|| "0x20000000..0x20040000 or 0x20000000+256K".to_string())
}

/// The number that `text` writes in hexadecimal, after `0x` or `0X`, as
/// in `0x20000000`. `None` when it doesn't start with either, and an error
/// when the rest isn't a hexadecimal number.
pub fn hex_number(text: &str) -> Option<Result<u64, ParseIntError>> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    Some(u64::from_str_radix(digits, 16))
}

/// A number in a range: hex such as `0x20000000`, or decimal with an
/// optional unit such as `256K`. The units, in any case, are `B` for bytes,
/// `K`, `KB` or `KiB` for 1024 bytes and `M`, `MB` or `MiB` for 1024 × 1024,
/// as memory sizes are usually given.
fn number(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("expected START..END or START+SIZE, with a number on each side".to_string());
    }
    if let Some(number) = hex_number(text) {
        return number.map_err(|_| format!("`{text}` isn't a hex number"));
    }
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (value, unit) = text.split_at(digits);
    let value: u64 = value
        .parse()
        .map_err(|_| format!("`{text}` isn't a number"))?;
    let scale = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        _ => return Err(format!("`{unit}` isn't a unit elfq knows; use B, K or M")),
    };
    value
        .checked_mul(scale)
        .ok_or_else(|| format!("`{text}` is too large"))
}

/// Sections whose addresses are closer than this share a block. Linkers
/// leave small gaps for alignment; separate memories are much further
/// apart.
const LARGEST_GAP: u64 = 64 * 1024;

/// A run of sections and load images with no large gap between them, such
/// as a microcontroller's flash or one of its RAMs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBlock {
    /// From the start of the first section or load image in it to the
    /// furthest end of any of them, gaps included.
    pub range: AddressRange,
    /// The sections in it, by index into the binary's sections, in address
    /// order.
    pub sections: Vec<usize>,
    /// The sections whose load images are in it (see
    /// [`Section::load_address`]), in the order of those addresses.
    pub load_images: Vec<usize>,
}

/// The blocks of addresses that the program's sections occupy, and the
/// load images of those whose bytes are stored elsewhere, in address order.
pub fn memory_blocks(sections: &[Section]) -> Vec<MemoryBlock> {
    // Where each section, or load image, is: (address, size, section, is
    // a load image).
    let mut places: Vec<(u64, u64, usize, bool)> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if !section.allocated || section.size == 0 {
            continue;
        }
        places.push((section.address, section.size, index, false));
        if let Some(load_address) = section.load_address {
            places.push((load_address, section.size, index, true));
        }
    }
    places.sort_by_key(|&(address, _, index, load_image)| (address, load_image, index));
    let mut blocks: Vec<MemoryBlock> = Vec::new();
    for (address, size, index, load_image) in places {
        let end = address.saturating_add(size);
        let block = match blocks.last_mut() {
            Some(block) if address < block.range.end.saturating_add(LARGEST_GAP) => {
                block.range.end = block.range.end.max(end);
                block
            }
            _ => {
                blocks.push(MemoryBlock {
                    range: AddressRange {
                        start: address,
                        end,
                    },
                    sections: Vec::new(),
                    load_images: Vec::new(),
                });
                blocks.last_mut().expect("a block was just added")
            }
        };
        if load_image {
            block.load_images.push(index);
        } else {
            block.sections.push(index);
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> Result<(u64, u64), String> {
        text.parse::<AddressRange>()
            .map(|range| (range.start, range.end))
    }

    #[test]
    fn a_range_has_an_end_or_a_size() {
        assert_eq!(
            range("0x20000000..0x20040000"),
            Ok((0x2000_0000, 0x2004_0000))
        );
        assert_eq!(range("0x20000000+0x40000"), Ok((0x2000_0000, 0x2004_0000)));
        assert_eq!(range("0x20000000+256K"), Ok((0x2000_0000, 0x2004_0000)));
        assert_eq!(range("0..224"), Ok((0, 224)));
        assert_eq!(range("0X0..0XE0"), Ok((0, 0xe0)));
    }

    #[test]
    fn units_are_powers_of_two_in_any_case() {
        for (size, bytes) in [
            ("5", 5),
            ("5b", 5),
            ("5B", 5),
            ("5k", 5 * 1024),
            ("5kb", 5 * 1024),
            ("5kB", 5 * 1024),
            ("5KiB", 5 * 1024),
            ("5MB", 5 * 1024 * 1024),
            ("5mib", 5 * 1024 * 1024),
        ] {
            assert_eq!(range(&format!("0+{size}")), Ok((0, bytes)), "{size}");
        }
        // Hex numbers take no unit, so a trailing b is a digit.
        assert_eq!(range("0+0x5b"), Ok((0, 0x5b)));
    }

    #[test]
    fn mistakes_are_explained() {
        assert!(range("0x20000000").unwrap_err().contains("START..END"));
        assert!(range("..0x10").unwrap_err().contains("number on each side"));
        assert!(
            range("0x10..0x10")
                .unwrap_err()
                .contains("end after it starts")
        );
        assert!(
            range("0x20..0x10")
                .unwrap_err()
                .contains("end after it starts")
        );
        assert!(range("0+5G").unwrap_err().contains("`G` isn't a unit"));
        assert!(range("0xzz..0x10").unwrap_err().contains("hex number"));
        assert!(
            range("0xffffffffffffffff+2")
                .unwrap_err()
                .contains("past the last")
        );
        // A range typed with `-` or `:` gets the same range as elfq takes it.
        assert_eq!(
            range("0x0-0x40000"),
            Err("expected START..END or START+SIZE, such as 0x0..0x40000".to_string())
        );
        assert_eq!(example_range("0x0:0x40000"), "0x0..0x40000");
        assert_eq!(example_range("0 - 256K"), "0..256K");
        // A second number smaller than the first is a size.
        assert_eq!(example_range("0x20000000:256K"), "0x20000000+256K");
        let usual = "0x20000000..0x20040000 or 0x20000000+256K";
        for text in ["0x10-0x10", "0x40000-0x0", "0x0-x", "0x1700"] {
            assert_eq!(example_range(text), usual, "{text}");
        }
    }

    #[test]
    fn overlap_counts_the_bytes_inside() {
        let range = AddressRange {
            start: 0x100,
            end: 0x200,
        };
        assert_eq!(range.overlap(0x180, 0x10), 0x10);
        assert_eq!(range.overlap(0xf0, 0x20), 0x10);
        assert_eq!(range.overlap(0x1f0, 0x20), 0x10);
        assert_eq!(range.overlap(0x200, 0x10), 0);
        assert_eq!(range.overlap(0x80, 0x10), 0);
        assert_eq!(range.text(8), "0x00000100..0x00000200");
        assert_eq!(range.to_string(), "0x100..0x200");
    }

    #[test]
    fn nearby_sections_share_a_block() {
        let section = |name: &str, address: u64, size: u64, allocated: bool| Section {
            name: name.to_string(),
            address,
            size,
            kind: object::SectionKind::Data,
            allocated,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        // Stored in flash, between .isr_vector and .text, and after .text,
        // as the Cortex-M7 sample stores them.
        let stored = |section: Section, load_address: u64| Section {
            load_address: Some(load_address),
            ..section
        };
        let sections = [
            section("", 0, 0, false),
            section(".text", 0x3000_0140, 0x880, true),
            section(".isr_vector", 0x3000_0000, 0x40, true),
            stored(section(".itcm", 0, 0xe0, true), 0x3000_0040),
            stored(section(".data", 0x2000_0000, 0x420, true), 0x3000_09c0),
            section(".bss", 0x2000_0420, 0x30, true),
            section(".ocram_bss", 0x2024_0000, 0x1000, true),
            section(".comment", 0, 0x20, false),
            // A load image after the last section makes the block longer.
            stored(section(".ramfunc", 0x10, 0x8, true), 0x3000_09c0 + 0x420),
            // One far from every section has a block of its own.
            stored(section(".ocram_data", 0x2024_1000, 0x10, true), 0x6000_0000),
        ];
        let blocks: Vec<(String, Vec<usize>, Vec<usize>)> = memory_blocks(&sections)
            .into_iter()
            .map(|block| (block.range.to_string(), block.sections, block.load_images))
            .collect();
        assert_eq!(
            blocks,
            [
                ("0x0..0xe0".to_string(), vec![3, 8], vec![]),
                ("0x20000000..0x20000450".to_string(), vec![4, 5], vec![]),
                ("0x20240000..0x20241010".to_string(), vec![6, 9], vec![]),
                (
                    "0x30000000..0x30000de8".to_string(),
                    vec![2, 1],
                    vec![3, 4, 8]
                ),
                ("0x60000000..0x60000010".to_string(), vec![], vec![9]),
            ]
        );
    }
}
