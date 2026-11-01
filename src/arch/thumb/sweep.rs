//! The sweep through one function's Thumb code: the [`Scanner`], which
//! decodes the instructions that refer to addresses and notes the data
//! they read, and [`Scanner::sweep`], which steps through the code with it
//! and skips that data.

use std::collections::BTreeMap;

use object::{Endian, Endianness};

use super::operands::Found;
use super::{Instruction, LR, PC, SP, align, branch, is_wide, literal, relative};
use crate::arch::CodeReference;
use crate::arch::CodeReferenceKind::{self, *};

/// The references that each instruction in `code` makes on its own: like
/// [`scan`](super::scan), but without following addresses from one
/// instruction to the next, so a branch through a register is always
/// through a pointer.
pub(in crate::arch) fn scan_instructions(
    code: &[u8],
    address: u64,
    endianness: Endianness,
) -> Vec<CodeReference> {
    Scanner::swept(code, address, endianness).references
}

/// Decodes one function's instructions, one at a time from the start, and
/// notes what they refer to: a reference for each instruction that calls,
/// jumps to or uses an address (`references`), and the constants and jump
/// tables that the instructions read (`data`), which the sweep skips. It
/// keeps more when asked: the bits that say where something is, when
/// `operands` is `Some`, for diff. `layout` marks more data before the
/// sweep, the bytes that `$d` symbols mark; the startup walk reads the data
/// that a sweep found, to tell a literal pool from code; and browse reads
/// the tables of addresses it found (`address_tables`).
pub(super) struct Scanner<'a> {
    code: &'a [u8],
    /// The address of `code[0]`.
    address: u64,
    /// The order of the bytes in constants and jump tables. Instructions
    /// are always little-endian, even in big-endian (BE8) files.
    endianness: Endianness,
    pub(super) references: Vec<CodeReference>,
    /// Constants and jump tables found so far, as offsets into `code`: the
    /// start of each, mapped to its end.
    pub(super) data: BTreeMap<usize, usize>,
    /// For each register, where its last `movw` is and its value, which a
    /// `movt` completes into a 32-bit address.
    low_halves: [Option<(usize, u32)>; 16],
    /// The last three instructions decoded, the last one last, which can
    /// say how long a jump table is.
    previous: [Option<Instruction>; 3],
    /// The tables of addresses found so far, which are data too, as ranges
    /// of offsets: the browser links their words to the cases.
    pub(super) address_tables: Vec<std::ops::Range<usize>>,
    /// The bits that say where something is, when they are kept, for
    /// [`operands`](super::operands::operands).
    pub(super) operands: Option<Found>,
}

impl<'a> Scanner<'a> {
    pub(super) fn new(code: &'a [u8], address: u64, endianness: Endianness) -> Self {
        Scanner {
            code,
            address,
            endianness,
            references: Vec::new(),
            data: BTreeMap::new(),
            low_halves: [None; 16],
            previous: [None; 3],
            address_tables: Vec::new(),
            operands: None,
        }
    }

    /// A scanner that has swept all of `code`, with the references and the
    /// data that it found.
    pub(super) fn swept(code: &'a [u8], address: u64, endianness: Endianness) -> Self {
        let mut scanner = Scanner::new(code, address, endianness);
        for _ in scanner.sweep() {}
        scanner
    }

    /// Steps through the code from its start, decoding each instruction
    /// and skipping the constants and jump tables that the instructions
    /// before it read.
    pub(super) fn sweep(&mut self) -> Sweep<'_, 'a> {
        Sweep {
            scanner: self,
            offset: Some(0),
        }
    }

    /// Sweeps the code, keeping the instructions it decodes. The sweep
    /// decodes a constant that only a later instruction loads, as in
    /// Clang's islands of constants, before it knows the constant is data,
    /// so those instructions are dropped.
    pub(super) fn sweep_instructions(&mut self) -> Vec<Instruction> {
        // Instructions take 2 bytes or more.
        let mut instructions = Vec::with_capacity(self.code.len() / 2);
        for swept in self.sweep() {
            if let Swept::Instruction(instruction) = swept {
                instructions.push(instruction);
            }
        }
        // Both lists are in order, so one pass over each drops them.
        let mut data = self.data.iter().peekable();
        instructions.retain(|instruction| {
            while data
                .next_if(|&(_, &end)| end <= instruction.offset)
                .is_some()
            {}
            data.peek()
                .is_none_or(|&(&start, _)| start >= instruction.offset + instruction.size())
        });
        instructions
    }

    /// Notes, when [`operands`](super::operands::operands) asks, that the
    /// bits under `halfwords` of the instruction at `offset` hold its
    /// distance to `target`.
    fn relative(&mut self, offset: usize, halfwords: [u16; 2], target: u64) {
        if let Some(found) = &mut self.operands {
            found.relative.push((offset, halfwords, target));
        }
    }

    /// Decodes the instruction at `offset`, which starts with `first`, and
    /// notes its reference, if any. `None` when the code ends inside it.
    fn step(&mut self, offset: usize, first: u16) -> Option<Instruction> {
        let (kind, second) = if is_wide(first) {
            let second = self.halfword(offset + 2)?;
            (self.wide(offset, first, second), second)
        } else {
            (self.narrow(offset, first), 0)
        };
        let instruction = Instruction {
            offset,
            first,
            second,
        };
        self.previous = [self.previous[1], self.previous[2], Some(instruction)];
        if let Some(kind) = kind {
            self.references.push(CodeReference {
                site: self.address + offset as u64,
                kind,
            });
        }
        Some(instruction)
    }

    /// A 16-bit instruction.
    fn narrow(&mut self, offset: usize, op: u16) -> Option<CodeReferenceKind> {
        let pc = self.pc(offset);
        // b<cond> label. Conditions 0b1110 and 0b1111 are other instructions.
        if op & 0xf000 == 0xd000 && (op >> 8) & 0xf < 0xe {
            let target = relative(pc, u32::from(op & 0xff) << 1, 9);
            self.relative(offset, [0x00ff, 0], target);
            return Some(Jump(target));
        }
        // b label
        if op & 0xf800 == 0xe000 {
            let target = relative(pc, u32::from(op & 0x7ff) << 1, 12);
            self.relative(offset, [0x07ff, 0], target);
            return Some(Jump(target));
        }
        // cbz, cbnz: always forward.
        if op & 0xf500 == 0xb100 {
            let distance = u64::from((op >> 9) & 1) << 6 | u64::from((op >> 3) & 0x1f) << 1;
            self.relative(offset, [0x02f8, 0], pc + distance);
            return Some(Jump(pc + distance));
        }
        // bx and blx with a register. `bx lr` returns.
        if op & 0xff00 == 0x4700 {
            let calls = op & 0x80 != 0;
            return match (op >> 3) & 0xf {
                _ if calls => Some(IndirectCall),
                LR => None,
                _ => Some(IndirectJump),
            };
        }
        // mov pc, register. `mov pc, lr` returns.
        if op & 0xff00 == 0x4600 && ((op >> 4) & 8 | op & 7) == PC {
            return ((op >> 3) & 0xf != LR).then_some(IndirectJump);
        }
        // ldr register, [pc, #imm]
        if op & 0xf800 == 0x4800 {
            let address = align(pc) + u64::from(op & 0xff) * 4;
            self.relative(offset, [0x00ff, 0], address);
            let value = self.constant(address, 4)?;
            return Some(Address(value));
        }
        // adr register, label
        if op & 0xf800 == 0xa000 {
            let address = align(pc) + u64::from(op & 0xff) * 4;
            self.relative(offset, [0x00ff, 0], address);
            return Some(Address(self.pointed_at(address)));
        }
        None
    }

    /// A 32-bit instruction, made of `first` and `second`.
    fn wide(&mut self, offset: usize, first: u16, second: u16) -> Option<CodeReferenceKind> {
        let pc = self.pc(offset);
        let rt = second >> 12;
        // The immediates of plain-binary instructions such as adr and movw,
        // spread over imm4:i:imm3:imm8.
        let imm12 = u32::from((first >> 10) & 1) << 11
            | u32::from((second >> 12) & 7) << 8
            | u32::from(second & 0xff);
        let imm16 = u32::from(first & 0xf) << 12 | imm12;

        // bl, b.w and b<cond>.w
        if first & 0xf800 == 0xf000 && second & 0x8000 != 0 {
            let kind = branch(pc, first, second);
            if let Some(Call(target) | Jump(target) | TailJump(target)) = kind {
                // b<cond>.w has a condition where the others have the top
                // bits of the distance.
                let conditional = second & 0x1000 == 0;
                let halfwords = [if conditional { 0x043f } else { 0x07ff }, 0x2fff];
                self.relative(offset, halfwords, target);
            }
            return kind;
        }
        // movw register, #imm16: maybe the low half of an address.
        if first & 0xfbf0 == 0xf240 && second & 0x8000 == 0 {
            self.low_halves[usize::from((second >> 8) & 0xf)] = Some((offset, imm16));
            return None;
        }
        // movt register, #imm16: the high half, completing the movw.
        if first & 0xfbf0 == 0xf2c0 && second & 0x8000 == 0 {
            let (movw, low) = self.low_halves[usize::from((second >> 8) & 0xf)].take()?;
            let value = imm16 << 16 | low;
            if let Some(found) = &mut self.operands {
                found.split.push(([movw, offset], value));
            }
            return Some(Address(u64::from(value)));
        }
        // adr.w register, label, before or after the instruction: bits 5
        // and 7 say which.
        let adr = [0x04a0, 0x70ff];
        if first & 0xfbff == 0xf2af && second & 0x8000 == 0 {
            let address = align(pc).wrapping_sub(u64::from(imm12)) & 0xffff_ffff;
            self.relative(offset, adr, address);
            return Some(Address(self.pointed_at(address)));
        }
        if first & 0xfbff == 0xf20f && second & 0x8000 == 0 {
            let address = align(pc) + u64::from(imm12);
            self.relative(offset, adr, address);
            return Some(Address(self.pointed_at(address)));
        }
        // Loads from a literal pool: bit 7 says whether the constant is
        // after the instruction or before it, and the last 12 or 8 bits how
        // far.
        //
        // ldr.w register, [pc, #imm]. Into pc it jumps, as in the veneers
        // that linkers add for calls too far for bl.
        if first & 0xff7f == 0xf85f {
            let address = literal(pc, first, u64::from(second & 0xfff));
            self.relative(offset, [0x0080, 0x0fff], address);
            let value = self.constant(address, 4)?;
            return Some(if rt == PC {
                TailJump(value & !1)
            } else {
                Address(value)
            });
        }
        // Loads of other sizes from literal pools hold numbers, not
        // addresses, but they are still data to skip: ldrb, ldrsb, ldrh and
        // ldrsh (into pc these are preload hints), ldrd and vldr.
        if first & 0xfe5f == 0xf81f {
            let address = literal(pc, first, u64::from(second & 0xfff));
            self.relative(offset, [0x0080, 0x0fff], address);
            if rt != PC {
                let size = if first & 0x20 == 0 { 1 } else { 2 };
                self.constant(address, size);
            }
            return None;
        }
        if first & 0xff7f == 0xe95f {
            let address = literal(pc, first, u64::from(second & 0xff) * 4);
            self.relative(offset, [0x0080, 0x00ff], address);
            self.constant(address, 8);
            return None;
        }
        // vldr of a half, a single or a double: its size, and what its
        // offset counts in.
        let vldr = match (second >> 8) & 0xf {
            0b1001 => Some((2, 2)),
            0b1010 => Some((4, 4)),
            0b1011 => Some((8, 4)),
            _ => None,
        };
        if first & 0xff3f == 0xed1f
            && let Some((size, scale)) = vldr
        {
            let address = literal(pc, first, u64::from(second & 0xff) * scale);
            self.relative(offset, [0x0080, 0x00ff], address);
            self.constant(address, size);
            return None;
        }
        // ldr pc, [register, ...]: a jump through memory, or a return when
        // it pops the stack.
        if (first & 0xfff0 == 0xf8d0 || first & 0xfff0 == 0xf850) && rt == PC {
            self.address_table(offset, first, second);
            return (first & 0xf != SP).then_some(IndirectJump);
        }
        // tbb [pc, register] and tbh [pc, register, lsl #1]: a jump table,
        // whose targets are all in this function.
        if first == 0xe8df && second & 0xffe0 == 0xf000 {
            let entries = self.table_entries(offset, second & 0xf);
            let entry_size = if second & 0x10 == 0 { 1 } else { 2 };
            self.jump_table(offset, entry_size, entries);
        }
        None
    }

    /// Marks the table of addresses that `ldr.w pc, [rX, rY, lsl #2]` at
    /// `offset` jumps through as data, when the instruction just before it
    /// is `adr rX, table`. GCC builds a switch this way where `tbb` and `tbh`
    /// can't reach its cases:
    ///
    /// ```text
    /// adr   r0, table
    /// ldr.w pc, [r0, r2, lsl #2]
    /// table: .word case0 + 1, .word case1 + 1, ...
    /// ```
    ///
    /// GCC puts `cmp rY, #n` and `bhi` before the `adr`, which say that the
    /// table has n + 1 words. The table runs on while its words are the
    /// Thumb addresses (odd) of code in this function, up to that many, or
    /// without the `cmp`, up to the first case after it. No jump to the
    /// cases is reported: the code there keeps what reading each
    /// instruction on its own finds, as before.
    fn address_table(&mut self, offset: usize, first: u16, second: u16) {
        // The register form, with lsl #2.
        if first & 0xfff0 != 0xf850 || second & 0x0fc0 != 0 || (second >> 4) & 3 != 2 {
            return;
        }
        let base = first & 0xf;
        let [cmp, bhi, Some(adr)] = self.previous else {
            return;
        };
        let pc = self.pc(adr.offset);
        // adr and adr.w, forward, into the base register, right before.
        let table = if adr.first & 0xf800 == 0xa000 && (adr.first >> 8) & 7 == base {
            align(pc) + u64::from(adr.first & 0xff) * 4
        } else if adr.first & 0xfbff == 0xf20f
            && adr.second & 0x8000 == 0
            && (adr.second >> 8) & 0xf == base
        {
            let imm12 = u32::from((adr.first >> 10) & 1) << 11
                | u32::from((adr.second >> 12) & 7) << 8
                | u32::from(adr.second & 0xff);
            align(pc) + u64::from(imm12)
        } else {
            return;
        };
        let Some(start) = table.checked_sub(self.address) else {
            return;
        };
        let Ok(start) = usize::try_from(start) else {
            return;
        };
        if adr.offset + adr.size() != offset || start < offset + 4 {
            return;
        }
        let function_end = self.address + self.code.len() as u64;
        // Where the table ends: after its n + 1 words, or else where the
        // first case after it starts, as far as the words read so far say.
        let mut first_case = match Self::entries(cmp, bhi, adr.offset, second & 0xf) {
            Some(entries) => start + entries * 4,
            None => self.code.len(),
        };
        let mut at = start;
        while at + 4 <= first_case {
            let Some(word) = self.code.get(at..at + 4).and_then(|b| b.try_into().ok()) else {
                break;
            };
            let word = u64::from(self.endianness.read_u32(word));
            let case = word & !1;
            if word & 1 == 0 || case < self.address || case >= function_end {
                break;
            }
            // Cases can come before the table, but not inside it.
            let case = (case - self.address) as usize;
            if (start..at + 4).contains(&case) {
                break;
            }
            if case > at {
                first_case = first_case.min(case);
            }
            at += 4;
        }
        if at > start {
            let end = self.data.entry(start).or_insert(at);
            *end = (*end).max(at);
            self.address_tables.push(start..at);
        }
    }

    /// How many entries the jump table of a `tbb` or `tbh` on `register`
    /// at `offset` has, when the two instructions before it say so: `cmp
    /// register, #n` and then `bhi`, which leaves n + 1.
    fn table_entries(&self, offset: usize, register: u16) -> Option<usize> {
        let [_, cmp, bhi] = self.previous;
        Self::entries(cmp, bhi, offset, register)
    }

    /// How many entries a table on `register` has when the instructions
    /// `cmp` and `bhi`, which end right before `next`, are `cmp register,
    /// #n` and `bhi`: n + 1.
    fn entries(
        cmp: Option<Instruction>,
        bhi: Option<Instruction>,
        next: usize,
        register: u16,
    ) -> Option<usize> {
        let (Some(cmp), Some(bhi)) = (cmp, bhi) else {
            return None;
        };
        // bhi and bhi.w.
        let is_bhi = bhi.first & 0xff00 == 0xd800
            || (bhi.first & 0xfbc0 == 0xf200 && bhi.second & 0xd000 == 0x8000);
        // cmp and cmp.w, with an immediate below 256.
        let (cmp_register, n) = if cmp.first & 0xf800 == 0x2800 {
            ((cmp.first >> 8) & 7, cmp.first & 0xff)
        } else if cmp.first & 0xfff0 == 0xf1b0 && cmp.second & 0xff00 == 0x0f00 {
            (cmp.first & 0xf, cmp.second & 0xff)
        } else {
            return None;
        };
        let in_a_row = cmp.offset + cmp.size() == bhi.offset && bhi.offset + bhi.size() == next;
        (is_bhi && in_a_row && cmp_register == register).then_some(usize::from(n) + 1)
    }

    /// Marks the jump table after the `tbb` or `tbh` at `site` as data, and
    /// reports a jump to each of its targets. Its entries are distances
    /// forward from the table's start, in halfwords. It has `entries`
    /// entries when the code before says so, and otherwise ends where the
    /// nearest target begins.
    fn jump_table(&mut self, site: usize, entry_size: usize, entries: Option<usize>) {
        let start = site + 4;
        let mut end = entries.map_or(self.code.len(), |entries| start + entries * entry_size);
        let mut entry = start;
        let mut targets = Vec::new();
        while entry + entry_size <= end.min(self.code.len()) {
            let distance = match entry_size {
                1 => u16::from(self.code[entry]),
                _ => self.table_entry(entry).unwrap_or(0),
            };
            let target = start + usize::from(distance) * 2;
            if entries.is_none() {
                end = end.min(target);
            }
            targets.push(target);
            entry += entry_size;
        }
        self.data.insert(start, entry.next_multiple_of(2));
        // An entry of 0, as in the byte that pads a table, points at the
        // table itself.
        targets.retain(|&target| target >= entry);
        for target in targets {
            self.references.push(CodeReference {
                site: self.address + site as u64,
                kind: Jump(self.address + target as u64),
            });
        }
    }

    /// Reads the constant of `size` bytes at `address` and marks it as
    /// data. Returns its value if it is a 32-bit word inside this function.
    fn constant(&mut self, address: u64, size: usize) -> Option<u64> {
        let offset = usize::try_from(address.checked_sub(self.address)?).ok()?;
        let bytes = self.code.get(offset..offset.checked_add(size)?)?;
        let end = self.data.entry(offset).or_insert(offset + size);
        *end = (*end).max(offset + size);
        let word: [u8; 4] = bytes.try_into().ok()?;
        Some(u64::from(self.endianness.read_u32(word)))
    }

    /// Marks what an `adr` at `address` points to in this function as data,
    /// and returns `address`. Compilers point `adr` at a `double`, or at a
    /// table of words, so its first 8 bytes are data. An odd address is
    /// Thumb code, as when hand-written assembly takes a function's address.
    fn pointed_at(&mut self, address: u64) -> u64 {
        if address & 1 == 0 {
            self.constant(address, 8);
        }
        address
    }

    /// Where the data at `offset` ends, if `offset` is inside data.
    fn data_end(&self, offset: usize) -> Option<usize> {
        let (_, &end) = self.data.range(..=offset).next_back()?;
        (offset < end).then_some(end)
    }

    /// The 16 bits of an instruction at `offset`.
    fn halfword(&self, offset: usize) -> Option<u16> {
        let bytes = self.code.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// The entry of a `tbh` jump table at `offset`, which is data.
    fn table_entry(&self, offset: usize) -> Option<u16> {
        let bytes: [u8; 2] = self.code.get(offset..offset + 2)?.try_into().ok()?;
        Some(self.endianness.read_u16(bytes))
    }

    /// The value of the program counter when the instruction at `offset`
    /// runs: four bytes past the instruction.
    fn pc(&self, offset: usize) -> u64 {
        self.address + offset as u64 + 4
    }
}

/// What [`Scanner::sweep`] finds next in the code.
pub(super) enum Swept {
    /// Data that the code reads, such as a constant or a jump table, which
    /// the sweep skips to `end`.
    Data { end: usize },
    /// An instruction, which the scanner decoded.
    Instruction(Instruction),
    /// An instruction that the end of the code cuts off, at this offset.
    /// The sweep stops there.
    Cut(usize),
}

/// The sweep through a scanner's code (see [`Scanner::sweep`]).
pub(super) struct Sweep<'s, 'a> {
    scanner: &'s mut Scanner<'a>,
    /// Where the next instruction or data starts, or `None` after a cut.
    offset: Option<usize>,
}

impl Iterator for Sweep<'_, '_> {
    type Item = Swept;

    fn next(&mut self) -> Option<Swept> {
        let offset = self.offset?;
        let first = self.scanner.halfword(offset)?;
        if let Some(end) = self.scanner.data_end(offset) {
            self.offset = Some(end);
            return Some(Swept::Data { end });
        }
        let Some(instruction) = self.scanner.step(offset, first) else {
            self.offset = None;
            return Some(Swept::Cut(offset));
        };
        self.offset = Some(offset + instruction.size());
        Some(Swept::Instruction(instruction))
    }
}

#[cfg(test)]
mod tests {
    use object::Endianness::{Big, Little};

    use super::*;
    #[cfg(feature = "tui")]
    use crate::arch::thumb::{Piece, layout};
    use crate::arch::thumb::{code_size, scan};

    fn kinds(code: &[u8], address: u64) -> Vec<(u64, CodeReferenceKind)> {
        scan(code, address, Little)
            .into_iter()
            .map(|r| (r.site, r.kind))
            .collect()
    }

    #[test]
    fn calls_and_jumps() {
        // From the Cortex-M C sample:
        //   800010e: f7ff ffd3  bl   80000b8 <filter_tap>
        //   8000184: f000 b808  b.w  8000198 <parse_list>
        //   80000ca: e001       b.n  80000d0
        assert_eq!(
            kinds(&[0xff, 0xf7, 0xd3, 0xff], 0x0800_010e),
            [(0x0800_010e, Call(0x0800_00b8))]
        );
        assert_eq!(
            kinds(&[0x00, 0xf0, 0x08, 0xb8], 0x0800_0184),
            [(0x0800_0184, TailJump(0x0800_0198))]
        );
        assert_eq!(
            kinds(&[0x01, 0xe0], 0x0800_00ca),
            [(0x0800_00ca, Jump(0x0800_00d0))]
        );
        // bx lr returns; blx r3 calls through a register.
        assert_eq!(kinds(&[0x70, 0x47], 0), []);
        assert_eq!(kinds(&[0x98, 0x47], 0), [(0, IndirectCall)]);
    }

    #[test]
    fn jumps_that_can_carry_on_in_another_function() {
        // A conditional b.w stays inside its function; from the C sample:
        //   80002dc: f040 8091  bne.w 8000402 <process_frame+0x13a>
        assert_eq!(
            kinds(&[0x40, 0xf0, 0x91, 0x80], 0x0800_02dc),
            [(0x0800_02dc, Jump(0x0800_0402))]
        );
        // A linker's veneer loads where to jump; from the Cortex-M7 sample:
        //   300009b0: f85f f000  ldr.w pc, [pc]
        //   300009b4: 00000011   .word 0x00000011 <crc32 + 1>
        assert_eq!(
            kinds(
                &[0x5f, 0xf8, 0x00, 0xf0, 0x11, 0x00, 0x00, 0x00],
                0x3000_09b0
            ),
            [(0x3000_09b0, TailJump(0x10))]
        );
    }

    #[test]
    fn constants_in_literal_pools_are_read_and_skipped() {
        // ldr r2, [pc, #0]; bx lr; then the constant 0x20000024, which
        // would otherwise decode as instructions.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x24, 0x00, 0x00, 0x20];
        assert_eq!(
            kinds(&code, 0x0800_0040),
            [(0x0800_0040, Address(0x2000_0024))]
        );
    }

    #[test]
    fn half_precision_constants_are_skipped() {
        // vldr.16 s2, [pc, #2]; bx lr; then two halves that vldr.16 loads,
        // which would otherwise decode as a bl.
        let code = [0x9f, 0xed, 0x01, 0x19, 0x70, 0x47, 0x00, 0xf0, 0x01, 0xf8];
        assert_eq!(kinds(&code, 0), []);
        assert_eq!(code_size(&code, 0, Little), 10);
    }

    #[test]
    fn movw_and_movt_build_an_address() {
        // movw r0, #0x1234; movt r0, #0x0800
        let code = [0x41, 0xf2, 0x34, 0x20, 0xc0, 0xf6, 0x00, 0x00];
        assert_eq!(kinds(&code, 0), [(4, Address(0x0800_1234))]);
    }

    #[test]
    fn what_adr_points_to_is_data() {
        // adr r0, 0x4; bx lr; then the double 0.0 it points to, which would
        // otherwise look like padding.
        let code = [0x00, 0xa0, 0x70, 0x47, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(kinds(&code, 0), [(0, Address(4))]);
        assert_eq!(code_size(&code, 0, Little), 12);
    }

    #[test]
    fn jump_tables_are_skipped_and_jump_to_their_targets() {
        // tbb [pc, r0], then a table of four byte offsets, then four cases
        // of bx lr. The table ends where the nearest case starts.
        let code = [
            0xdf, 0xe8, 0x00, 0xf0, 0x02, 0x03, 0x04, 0x05, 0x70, 0x47, 0x70, 0x47, 0x70, 0x47,
            0x70, 0x47,
        ];
        let jumps = [8, 10, 12, 14].map(|target| (0, Jump(target)));
        assert_eq!(kinds(&code, 0), jumps);
        // cmp r0, #1; bhi 0xa; tbb [pc, r0], then a table of two entries.
        // The code for other values follows it (movs r0, #5; bx lr), before
        // the two cases (bx lr): cmp and bhi say where the table ends.
        let code = [
            0x01, 0x28, 0x02, 0xd8, 0xdf, 0xe8, 0x00, 0xf0, 0x03, 0x04, 0x05, 0x20, 0x70, 0x47,
            0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(
            kinds(&code, 0),
            [(2, Jump(0xa)), (4, Jump(0xe)), (4, Jump(0x10))]
        );
    }

    #[test]
    fn tables_of_addresses_are_skipped() {
        // adr r0, 0xe008; ldr.w pc, [r0, r2, lsl #2]; nop; then a table of
        // three cases, as GCC makes for a switch, and the cases, bx lr. The
        // last word, 0xe019, would otherwise decode as b.n 0xe046.
        let code = [
            0x01, 0xa0, 0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, 0x15, 0xe0, 0x00, 0x00, 0x17, 0xe0,
            0x00, 0x00, 0x19, 0xe0, 0x00, 0x00, 0x70, 0x47, 0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(
            kinds(&code, 0xe000),
            [(0xe000, Address(0xe008)), (0xe002, IndirectJump)]
        );
        // The same with adr into another register than the load's: the
        // words are instructions.
        let mut other = code;
        other[1] = 0xa1;
        assert!(kinds(&other, 0xe000).contains(&(0xe010, Jump(0xe046))));
    }

    #[test]
    fn a_table_of_addresses_ends_before_its_first_case_after_it() {
        // adr r0, 0xe008; ldr.w pc, [r0, r2, lsl #2]; nop; a table of two
        // words that both hold 0xe011, then that case at 0xe010: b.n 0xe03e,
        // whose first word, 0x0000e015, is also the Thumb address of code
        // in the function. Then bx lr up to 0xe040.
        let mut code = vec![
            0x01, 0xa0, 0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, 0x11, 0xe0, 0x00, 0x00, 0x11, 0xe0,
            0x00, 0x00, 0x15, 0xe0, 0x00, 0x00,
        ];
        while code.len() < 0x40 {
            code.extend_from_slice(&[0x70, 0x47]);
        }
        assert_eq!(
            kinds(&code, 0xe000),
            [
                (0xe000, Address(0xe008)),
                (0xe002, IndirectJump),
                (0xe010, Jump(0xe03e))
            ]
        );
    }

    #[test]
    fn a_table_of_addresses_has_as_many_words_as_its_cmp_says() {
        // Code in RAM: a switch on r2 whose two cases, at 0x20004800 and
        // 0x20004802, come before its table, and after the table, code that
        // is no case: ldr r1, [pc, #260]; movs r0, #0; bx r1. Its first
        // word, 0x20004941, is also the Thumb address of code in the
        // function, but cmp r2, #1 says that the table has two words.
        let mut code = vec![
            0x01, 0x20, 0x70, 0x47, // movs r0, #1; bx lr
            0x01, 0x2a, 0xfc, 0xd8, // cmp r2, #1; bhi 0x20004802
            0x01, 0xa0, // adr r0, 0x20004810
            0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, // ldr.w pc, [r0, r2, lsl #2]; nop
            0x01, 0x48, 0x00, 0x20, 0x03, 0x48, 0x00, 0x20, // the table
            0x41, 0x49, 0x00, 0x20, 0x08, 0x47, // ldr r1, [pc, #260]; movs r0, #0; bx r1
        ];
        while code.len() < 0x120 {
            code.extend_from_slice(&[0x00, 0xbf]);
        }
        // The word that the ldr reads, then bx lr up to 0x20004950.
        code.extend_from_slice(&[0x01, 0x01, 0x00, 0x08]);
        while code.len() < 0x150 {
            code.extend_from_slice(&[0x70, 0x47]);
        }
        let found = kinds(&code, 0x2000_4800);
        assert!(found.contains(&(0x2000_480a, IndirectJump)), "{found:x?}");
        assert!(
            found.contains(&(0x2000_4818, Address(0x0800_0101))),
            "{found:x?}"
        );
        #[cfg(feature = "tui")]
        {
            let cases: Vec<usize> = layout(&code, 0x2000_4800, Little, &[])
                .into_iter()
                .filter(|&(_, _, piece)| piece == Piece::Case)
                .map(|(offset, _, _)| offset)
                .collect();
            assert_eq!(cases, [0x10, 0x14]);
        }
    }

    #[test]
    fn big_endian_files_have_big_endian_constants_and_tables() {
        // In big-endian (BE8) firmware, instructions are still
        // little-endian. ldr r2, [pc, #0]; bx lr; then 0x20000024.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x20, 0x00, 0x00, 0x24];
        let kinds = |code: &[u8]| -> Vec<(u64, CodeReferenceKind)> {
            scan(code, 0, Big)
                .into_iter()
                .map(|r| (r.site, r.kind))
                .collect()
        };
        assert_eq!(kinds(&code), [(0, Address(0x2000_0024))]);
        // tbh [pc, r0, lsl #1], then a table of two halfwords, 2 and 3,
        // then two cases of bx lr.
        let code = [
            0xdf, 0xe8, 0x10, 0xf0, 0x00, 0x02, 0x00, 0x03, 0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(kinds(&code), [(0, Jump(8)), (0, Jump(10))]);
    }

    #[test]
    fn an_adr_to_thumb_code_marks_no_data() {
        // adr.w r0, 0xd, the address of Thumb code at 0xc; bx lr; four
        // nops; bl 0x20, which the scan still decodes.
        let code = [
            0x0f, 0xf2, 0x09, 0x00, 0x70, 0x47, 0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf,
            0x00, 0xf0, 0x07, 0xf8,
        ];
        assert_eq!(kinds(&code, 0), [(0, Address(0xd)), (14, Call(0x20))]);
    }

    #[test]
    fn addresses_before_0_are_not_an_error() {
        // adr.w r0 and ldr.w r0 eight bytes before the program counter, in
        // code at address 0. Addresses are 32 bits long, so adr.w wraps.
        let adr = [0xaf, 0xf2, 0x08, 0x00];
        assert_eq!(kinds(&adr, 0), [(0, Address(0xffff_fffc))]);
        assert_eq!(kinds(&[0x5f, 0xf8, 0x08, 0x00], 0), []);
    }
}
