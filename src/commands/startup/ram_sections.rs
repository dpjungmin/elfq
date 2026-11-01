//! The sections that startup code sets up, which are all in RAM: the load
//! images, whose bytes the file stores elsewhere for startup code to copy
//! into place, and the sections without contents, such as `.bss`, which
//! it zeroes ([`Memory`]). It also matches what code copies or zeroes to
//! them: what a call does from the numbers it passes in r0 to r2, as
//! `memcpy(to, from, size)` and `memset(to, 0, size)` take them
//! ([`Memory::match_call`]), or the bytes that loops and stores copy or
//! zero ([`Memory::matched`]); and gives the runs of sections that a copy
//! or zeroing of a region covers ([`Memory::run_from`]).

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
/// [`Binary::sections`].
pub(super) struct Memory<'a> {
    binary: &'a Binary,
    /// The load images: sections whose bytes the file stores elsewhere,
    /// for startup code to copy into place.
    pub loads: Vec<usize>,
    /// The sections without contents, such as `.bss`, which start as
    /// zeros.
    pub zeros: Vec<usize>,
    /// Both, by address.
    pub ram: Vec<usize>,
}

impl<'a> Memory<'a> {
    pub fn new(binary: &'a Binary) -> Memory<'a> {
        let sections =
            || (binary.sections.iter().enumerate()).filter(|(_, x)| x.allocated && x.size > 0);
        let loads: Vec<usize> = sections()
            .filter(|(_, x)| x.load_address.is_some() && x.file_range.is_some())
            .map(|(index, _)| index)
            .collect();
        let zeros: Vec<usize> = sections()
            .filter(|(_, x)| x.kind == SectionKind::UninitializedData)
            .map(|(index, _)| index)
            .collect();
        let mut ram: Vec<usize> = loads.iter().chain(&zeros).copied().collect();
        ram.sort_by_key(|&x| binary.sections[x].address);
        Memory {
            binary,
            loads,
            zeros,
            ram,
        }
    }

    pub fn section(&self, x: usize) -> &'a Section {
        &self.binary.sections[x]
    }

    pub fn name(&self, x: usize) -> &'a str {
        &self.section(x).name
    }

    /// Whether `x` is a load image, rather than a section without
    /// contents.
    pub fn is_load(&self, x: usize) -> bool {
        self.section(x).load_address.is_some()
    }

    /// Where the file stores `x`'s bytes.
    pub fn load_address(&self, x: usize) -> u64 {
        let section = self.section(x);
        section.load_address.unwrap_or(section.address)
    }

    pub fn end(&self, x: usize) -> u64 {
        let section = self.section(x);
        section.address + section.size
    }

    /// Whether a problem is to say when startup code doesn't set `x` up
    /// as it should: a load image, `.bss` or `.sbss`.
    pub fn required(&self, x: usize) -> bool {
        self.is_load(x) || ZEROED.contains(&self.name(x))
    }

    /// Whether `x` is `.bss` or `.sbss`.
    pub fn zeroed_by_rule(&self, x: usize) -> bool {
        ZEROED.contains(&self.name(x))
    }

    /// The section that startup code sets up that holds `address`.
    pub fn holding(&self, address: u64) -> Option<usize> {
        (self.ram.iter().copied())
            .find(|&x| self.section(x).address <= address && address < self.end(x))
    }

    /// Whether a section that startup code sets up starts at `address`.
    pub fn starts_section(&self, address: u64) -> bool {
        self.ram.iter().any(|&x| self.section(x).address == address)
    }

    /// Whether the file stores bytes at `address` where they stay, as in
    /// flash: where a copy can read from at reset.
    fn stored(&self, address: u64) -> bool {
        address & 1 == 0
            && self.holding(address).is_none()
            && self.binary.sections.iter().any(|section| {
                let start = section.load_address.unwrap_or(section.address);
                section.allocated
                    && section.file_range.is_some_and(|(_, size)| size > 0)
                    && start <= address
                    && address - start < section.size
            })
    }

    /// What a call does that passes `arguments` in r0 to r3: a copy of
    /// load images, as `memcpy(to, from, size)`, when r0 is where one
    /// starts and r1 where the file stores bytes, or a zeroing of sections
    /// without contents, as `memset(to, 0, size)`, when r0 is where a
    /// section starts and r1 is 0.
    pub fn match_call(&self, arguments: &[Option<u32>; 4]) -> Option<Match> {
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
    pub fn matched(&self, does: Does, to: u64, from: Option<u64>, size: u64) -> Option<Match> {
        let covers = match does {
            Does::Copy => {
                let from = from?;
                let starts = self.loads.iter().any(|&x| self.section(x).address == to);
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
            to,
            from,
            size,
            covers,
        })
    }

    /// Whether any byte from `bytes` is in a section that startup code
    /// sets up.
    pub fn overlaps(&self, bytes: Range<u64>) -> bool {
        (self.ram.iter()).any(|&x| self.section(x).address < bytes.end && bytes.start < self.end(x))
    }

    /// The sections of `sections` that start in the `size` bytes from
    /// `to`, each with how many of its bytes they cover and, for a copy
    /// from `from`, where it copies them from. A range of 0 bytes at a
    /// section's start covers none of its bytes, but names it.
    fn cover(&self, sections: &[usize], to: u64, size: u64, from: Option<u64>) -> Vec<Cover> {
        let end = to.saturating_add(size.max(1));
        (sections.iter().copied())
            .filter(|&x| (to..end).contains(&self.section(x).address))
            .map(|x| {
                let offset = self.section(x).address - to;
                Cover {
                    section: x,
                    bytes: self.section(x).size.min(size - offset.min(size)),
                    from: from.map(|from| from.wrapping_add(offset)),
                }
            })
            .collect()
    }

    /// `x` and the sections of its kind that follow it with no gap, which
    /// a copy or zeroing of a region covers too: a load image whose load
    /// image follows `x`'s with no gap either.
    pub fn run_from(&self, x: usize) -> Vec<usize> {
        let mut run = vec![x];
        for &y in &self.ram {
            let last = *run.last().expect("a run starts with x");
            if self.is_load(y) != self.is_load(x)
                || self.section(y).address <= self.section(last).address
            {
                continue;
            }
            let gap = self.section(y).address.checked_sub(self.end(last));
            if !gap.is_some_and(|gap| gap < 8) {
                break;
            }
            if self.is_load(x)
                && self.load_address(y).wrapping_sub(self.load_address(last))
                    != self.section(y).address - self.section(last).address
            {
                break;
            }
            run.push(y);
        }
        run
    }
}

/// What a call does to the sections, as [`Memory::match_call`] tells.
pub(super) struct Match {
    pub does: Does,
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
    fn matched(memory: &Memory, to: u32, from: Option<u32>, size: u32) -> Vec<String> {
        let Some(found) = memory.match_call(&[Some(to), from, Some(size), None]) else {
            return Vec::new();
        };
        (found.covers.iter())
            .map(|cover| {
                let from = cover.from.map(|from| format!(" from {from:#x}"));
                let name = memory.name(cover.section);
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
        let memory = Memory::new(&binary);
        assert_eq!(
            matched(&memory, 0x2000_0000, Some(0x800_0400), 0x30),
            [
                "copy .data 32 from 0x8000400",
                "copy .ramfunc 16 from 0x8000420"
            ]
        );
        // From bytes the file stores elsewhere: the problems tell.
        assert_eq!(
            matched(&memory, 0x2000_0000, Some(0x800_0300), 0x10),
            ["copy .data 16 from 0x8000300"]
        );
        assert_eq!(
            matched(&memory, 0x2000_0030, Some(0), 0x50),
            ["zero .bss 64", "zero .noinit 16"]
        );
        assert_eq!(
            matched(&memory, 0x2000_0030, Some(0), 0x20),
            ["zero .bss 32"]
        );
        // A range of 0 bytes names the section that starts there.
        assert_eq!(matched(&memory, 0x2000_0030, Some(0), 0), ["zero .bss 0"]);
    }

    #[test]
    fn a_call_matches_only_where_a_section_starts_and_from_bytes_that_stay() {
        let binary = binary();
        let memory = Memory::new(&binary);
        let none: [&str; 0] = [];
        // Not where a section starts.
        assert_eq!(matched(&memory, 0x2000_0004, Some(0), 4), none);
        assert_eq!(matched(&memory, 0x2000_0004, Some(0x800_0400), 4), none);
        // From RAM, which holds nothing at reset; from an odd address, as
        // a function's address in Thumb code; from no address it knows.
        assert_eq!(matched(&memory, 0x2000_0000, Some(0x2000_0040), 4), none);
        assert_eq!(matched(&memory, 0x2000_0000, Some(0x800_0401), 4), none);
        assert_eq!(matched(&memory, 0x2000_0000, None, 4), none);
        // A length no section has, as a pointer that r2 still holds.
        assert_eq!(matched(&memory, 0x2000_0030, Some(0), 1 << 28), none);
        // Zeroing a load image is no step of startup.
        assert_eq!(matched(&memory, 0x2000_0000, Some(0), 0x20), none);
    }

    #[test]
    fn a_run_is_the_sections_of_a_kind_that_follow_with_no_gap() {
        let binary = binary();
        let memory = Memory::new(&binary);
        let names = |x: usize| -> Vec<&str> {
            (memory.run_from(x).into_iter())
                .map(|x| memory.name(x))
                .collect()
        };
        assert_eq!(names(1), [".data", ".ramfunc"]);
        assert_eq!(names(3), [".bss", ".noinit"]);
        // With .ramfunc's load image elsewhere, a copy of the run from
        // .data's would copy other bytes to it.
        let mut moved = self::binary();
        moved.sections[2].load_address = Some(0x800_0440);
        let memory = Memory::new(&moved);
        let run: Vec<&str> = (memory.run_from(1).into_iter())
            .map(|x| memory.name(x))
            .collect();
        assert_eq!(run, [".data"]);
    }
}
