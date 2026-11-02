//! The sections that startup code sets up, which are all in RAM: the load
//! images, whose bytes the file stores elsewhere for startup code to copy
//! into place, and the sections without contents, such as `.bss`, which
//! it zeroes ([`RamSections`]). It also matches what code copies or
//! zeroes to them: what a call does from the numbers it passes in r0 to
//! r2, as `memcpy(to, from, size)` and `memset(to, 0, size)` take them
//! ([`RamSections::match_call`]), or the bytes that loops and stores copy
//! or zero ([`RamSections::matched`]); and gives the runs of sections that
//! a copy or zeroing of a region covers ([`RamSections::run_from`]).

use std::ops::Range;

use object::SectionKind;

use super::steps::Does;
use crate::binary::{Binary, Section};

/// The names that the ELF specification gives sections of memory that
/// start as zeros, which a program counts on being zeroed: a problem when
/// startup code doesn't zero them, where it is only a note for others,
/// such as `.noinit`, which may be meant to keep what it holds.
const ZEROED: [&str; 2] = [".bss", ".sbss"];

/// Lengths of 2^28 bytes and more aren't the length of a section a call
/// copies or zeroes.
const LONGEST: u32 = 1 << 28;

/// The sections that startup code sets up, as indexes into
/// [`Binary::sections`]: the RAM sections.
pub(super) struct RamSections<'a> {
    binary: &'a Binary,
    /// The load images: sections whose bytes the file stores elsewhere,
    /// for startup code to copy into place.
    pub loads: Vec<usize>,
    /// The sections without contents, such as `.bss`, which start as
    /// zeros.
    pub zeros: Vec<usize>,
    /// Both, by address.
    pub sections: Vec<usize>,
}

impl<'a> RamSections<'a> {
    pub(super) fn new(binary: &'a Binary) -> RamSections<'a> {
        let allocated = || {
            (0..binary.sections.len()).filter(|&section| {
                let Section {
                    allocated, size, ..
                } = &binary.sections[section];
                *allocated && *size > 0
            })
        };
        let loads: Vec<usize> = allocated()
            .filter(|&section| {
                let Section {
                    load_address,
                    file_range,
                    ..
                } = &binary.sections[section];
                load_address.is_some() && file_range.is_some()
            })
            .collect();
        let zeros: Vec<usize> = allocated()
            .filter(|&section| binary.sections[section].kind == SectionKind::UninitializedData)
            .collect();
        let mut sections: Vec<usize> = loads.iter().chain(&zeros).copied().collect();
        sections.sort_by_key(|&section| binary.sections[section].address);
        RamSections {
            binary,
            loads,
            zeros,
            sections,
        }
    }

    pub(super) fn section(&self, section: usize) -> &'a Section {
        &self.binary.sections[section]
    }

    pub(super) fn name(&self, section: usize) -> &'a str {
        &self.section(section).name
    }

    /// Whether `section` is a load image, rather than a section without
    /// contents.
    pub(super) fn is_load(&self, section: usize) -> bool {
        self.section(section).load_address.is_some()
    }

    /// Where the file stores `section`'s bytes.
    pub(super) fn load_address(&self, section: usize) -> u64 {
        let Section {
            address,
            load_address,
            ..
        } = self.section(section);
        load_address.unwrap_or(*address)
    }

    pub(super) fn end(&self, section: usize) -> u64 {
        let Section { address, size, .. } = self.section(section);
        address + size
    }

    /// Whether a problem is to say when startup code doesn't set `section`
    /// up as it should: a load image, `.bss` or `.sbss`.
    pub(super) fn required(&self, section: usize) -> bool {
        self.is_load(section) || ZEROED.contains(&self.name(section))
    }

    /// Whether `section` is `.bss` or `.sbss`.
    pub(super) fn zeroed_by_rule(&self, section: usize) -> bool {
        ZEROED.contains(&self.name(section))
    }

    /// The section that startup code sets up that holds `address`.
    pub(super) fn holding(&self, address: u64) -> Option<usize> {
        (self.sections.iter().copied()).find(|&section| {
            self.section(section).address <= address && address < self.end(section)
        })
    }

    /// Whether a section that startup code sets up starts at `address`.
    pub(super) fn starts_section(&self, address: u64) -> bool {
        self.sections
            .iter()
            .any(|&section| self.section(section).address == address)
    }

    /// Whether the file stores bytes at `address` where they stay, as in
    /// flash: where a copy can read from at reset.
    fn stored(&self, address: u64) -> bool {
        address & 1 == 0
            && self.holding(address).is_none()
            && (0..self.binary.sections.len()).any(|section| {
                let Section {
                    allocated,
                    file_range,
                    size,
                    ..
                } = self.section(section);
                let start = self.load_address(section);
                *allocated
                    && file_range.is_some_and(|(_, stored)| stored > 0)
                    && start <= address
                    && address - start < *size
            })
    }

    /// What a call does that passes `arguments` in r0 to r3: a copy of
    /// load images, as `memcpy(to, from, size)`, when r0 is where one
    /// starts and r1 where the file stores bytes, or a zeroing of sections
    /// without contents, as `memset(to, 0, size)`, when r0 is where a
    /// section starts and r1 is 0.
    pub(super) fn match_call(&self, arguments: &[Option<u32>; 4]) -> Option<Match> {
        let [Some(to), from, Some(size), _] = *arguments else {
            return None;
        };
        if size >= LONGEST {
            return None;
        }
        let (to, size) = (u64::from(to), u64::from(size));
        let from = from.map(u64::from);
        (from.and_then(|from| self.matched(Does::Copy, to, Some(from), size)))
            .or_else(|| (from == Some(0)).then(|| self.matched(Does::Zero, to, None, size))?)
    }

    /// What code does that copies `size` bytes to `to` from `from`, or
    /// zeroes them: a copy of load images, when one starts at `to` and the
    /// file stores bytes at `from`, or a zeroing of the sections without
    /// contents that start in them, when a section starts at `to`.
    pub(super) fn matched(
        &self,
        does: Does,
        to: u64,
        from: Option<u64>,
        size: u64,
    ) -> Option<Match> {
        let covers = match does {
            Does::Copy => {
                let from = from?;
                let starts = self
                    .loads
                    .iter()
                    .any(|&section| self.section(section).address == to);
                if !(starts && self.stored(from)) {
                    return None;
                }
                self.cover(&self.loads, to, size, Some(from))
            }
            Does::Zero if self.starts_section(to) => self.cover(&self.zeros, to, size, None),
            _ => return None,
        };
        if covers.is_empty() {
            return None;
        }
        Some(Match {
            does,
            region: Region {
                to,
                from,
                size,
                covers,
            },
        })
    }

    /// Whether any byte from `bytes` is in a section that startup code
    /// sets up.
    pub(super) fn overlaps(&self, bytes: Range<u64>) -> bool {
        (self.sections.iter()).any(|&section| {
            self.section(section).address < bytes.end && bytes.start < self.end(section)
        })
    }

    /// The sections of `sections` that start in the `size` bytes from
    /// `to`, each with how many of its bytes they cover and, for a copy
    /// from `from`, where it copies them from. A range of 0 bytes at a
    /// section's start covers none of its bytes, but names it.
    fn cover(&self, sections: &[usize], to: u64, size: u64, from: Option<u64>) -> Vec<Cover> {
        let end = to.saturating_add(size.max(1));
        (sections.iter().copied())
            .filter(|&section| (to..end).contains(&self.section(section).address))
            .map(|section| {
                let offset = self.section(section).address - to;
                Cover {
                    section,
                    bytes: self.section(section).size.min(size - offset.min(size)),
                    from: from.map(|from| from.wrapping_add(offset)),
                }
            })
            .collect()
    }

    /// `section` and the sections of its kind that follow it with no gap,
    /// which a copy or zeroing of a region covers too: a load image whose
    /// load image follows `section`'s with no gap either.
    pub(super) fn run_from(&self, section: usize) -> Vec<usize> {
        let mut run = vec![section];
        for &next in &self.sections {
            let last = *run.last().expect("a run starts with section");
            if self.is_load(next) != self.is_load(section)
                || self.section(next).address <= self.section(last).address
            {
                continue;
            }
            let gap = self.section(next).address.checked_sub(self.end(last));
            if !gap.is_some_and(|gap| gap < 8) {
                break;
            }
            if self.is_load(section)
                && self
                    .load_address(next)
                    .wrapping_sub(self.load_address(last))
                    != self.section(next).address - self.section(last).address
            {
                break;
            }
            run.push(next);
        }
        run
    }
}

/// What code does to the sections, as [`RamSections::match_call`] and
/// [`RamSections::matched`] tell: a copy or a zeroing, of a region.
pub(super) struct Match {
    pub does: Does,
    pub region: Region,
}

/// What a copy or zeroing copies or zeroes: `size` bytes at `to`, for a
/// copy from `from`, and the sections that start in them.
pub(super) struct Region {
    pub to: u64,
    pub from: Option<u64>,
    pub size: u64,
    pub covers: Vec<Cover>,
}

/// A section that a step copies or zeroes.
#[derive(Debug, Clone, Copy)]
pub(super) struct Cover {
    pub section: usize,
    /// How many of its bytes the step covers.
    pub bytes: u64,
    /// For a copy, where it copies them from.
    pub from: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::binary;
    use super::*;

    /// What a call that passes `to`, `from` and `size` does, as "copy
    /// .data 32 from 0x8000400", a cover to a line.
    fn matched(ram: &RamSections, to: u32, from: Option<u32>, size: u32) -> Vec<String> {
        let Some(found) = ram.match_call(&[Some(to), from, Some(size), None]) else {
            return Vec::new();
        };
        (found.region.covers.iter())
            .map(|cover| {
                let from = cover.from.map(|from| format!(" from {from:#x}"));
                let name = ram.name(cover.section);
                format!(
                    "{} {name} {}{}",
                    found.does.word(),
                    cover.bytes,
                    from.unwrap_or_default()
                )
            })
            .collect()
    }

    #[test]
    fn a_call_copies_the_load_images_or_zeroes_the_sections_that_start_in_its_range() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        assert_eq!(
            matched(&ram, 0x2000_0000, Some(0x800_0400), 0x30),
            [
                "copy .data 32 from 0x8000400",
                "copy .ramfunc 16 from 0x8000420"
            ]
        );
        // From bytes the file stores elsewhere: the problems tell.
        assert_eq!(
            matched(&ram, 0x2000_0000, Some(0x800_0300), 0x10),
            ["copy .data 16 from 0x8000300"]
        );
        assert_eq!(
            matched(&ram, 0x2000_0030, Some(0), 0x50),
            ["zero .bss 64", "zero .noinit 16"]
        );
        assert_eq!(matched(&ram, 0x2000_0030, Some(0), 0x20), ["zero .bss 32"]);
        // A range of 0 bytes names the section that starts there.
        assert_eq!(matched(&ram, 0x2000_0030, Some(0), 0), ["zero .bss 0"]);
    }

    #[test]
    fn a_call_matches_only_where_a_section_starts_and_from_bytes_that_stay() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let none: [&str; 0] = [];
        // Not where a section starts.
        assert_eq!(matched(&ram, 0x2000_0004, Some(0), 4), none);
        assert_eq!(matched(&ram, 0x2000_0004, Some(0x800_0400), 4), none);
        // From RAM, which holds nothing at reset; from an odd address, as
        // a function's address in Thumb code; from no address it knows.
        assert_eq!(matched(&ram, 0x2000_0000, Some(0x2000_0040), 4), none);
        assert_eq!(matched(&ram, 0x2000_0000, Some(0x800_0401), 4), none);
        assert_eq!(matched(&ram, 0x2000_0000, None, 4), none);
        // A length no section has, as a pointer that r2 still holds.
        assert_eq!(matched(&ram, 0x2000_0030, Some(0), 1 << 28), none);
        // Zeroing a load image is no step of startup.
        assert_eq!(matched(&ram, 0x2000_0000, Some(0), 0x20), none);
    }

    #[test]
    fn a_run_is_the_sections_of_a_kind_that_follow_with_no_gap() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let names = |section: usize| -> Vec<&str> {
            (ram.run_from(section).into_iter())
                .map(|section| ram.name(section))
                .collect()
        };
        assert_eq!(names(1), [".data", ".ramfunc"]);
        assert_eq!(names(3), [".bss", ".noinit"]);
        // With .ramfunc's load image elsewhere, a copy of the run from
        // .data's would copy other bytes to it.
        let mut moved = self::binary();
        moved.sections[2].load_address = Some(0x800_0440);
        let ram = RamSections::new(&moved);
        let run: Vec<&str> = (ram.run_from(1).into_iter())
            .map(|section| ram.name(section))
            .collect();
        assert_eq!(run, [".data"]);
    }
}
