//! Where the instructions and the data in a function's code are, for
//! browse to print the code as text (see `InstructionSet::listing`).

use object::Endianness;

use super::sweep::Scanner;

/// What a piece of a function's code is, as [`layout`] cuts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::arch) enum Piece {
    /// An instruction, 2 or 4 bytes long.
    Instruction,
    /// Data that the code reads, such as a constant in a literal pool or
    /// a jump table of `tbb` or `tbh`, or bytes the file marks as data.
    Data,
    /// A word of a table of addresses that `ldr.w pc` jumps through: the
    /// Thumb address of a case of a switch, in this function.
    Case,
}

/// Where the instructions and the data in `code` are, as the sweep that
/// [`scan`](super::scan) makes finds them, as (offset, size, piece), in
/// order. Data is cut into words, or halfwords and bytes where a word
/// doesn't fit, as objdump prints it, and so are the bytes that no
/// instruction covers, such as a cut-off instruction at the end. `marked`
/// lists more data, as ranges of offsets, such as the bytes that the file's
/// `$d` symbols mark: the sweep skips them too, so it starts again where
/// they end.
///
/// Without `marked`, this is `scan`'s own sweep, so every reference that
/// `scan` reports starts a piece: an instruction, or a constant that the
/// sweep decoded before a later load showed that it is data. With it, the
/// two sweeps can cut the code differently after a mark, until they meet
/// again: `every_use_that_elfq_finds_starts_a_line_of_code`, in
/// `commands/browse/views/item/code.rs`, checks on every sample that every
/// use that the graph lists still starts a line.
pub(in crate::arch) fn layout(
    code: &[u8],
    address: u64,
    endianness: Endianness,
    marked: &[std::ops::Range<usize>],
) -> Vec<(usize, usize, Piece)> {
    let mut scanner = Scanner::new(code, address, endianness);
    for range in marked {
        let end = scanner.data.entry(range.start).or_insert(range.end);
        *end = (*end).max(range.end);
    }
    let instructions = scanner.sweep_instructions();
    let mut pieces: Vec<(usize, usize, Piece)> = instructions
        .iter()
        .map(|instruction| (instruction.offset, instruction.size(), Piece::Instruction))
        .collect();
    // The bytes between instructions, in order.
    let mut gaps = Vec::new();
    let mut covered = 0;
    for instruction in &instructions {
        if covered < instruction.offset {
            gaps.push(covered..instruction.offset);
        }
        covered = instruction.offset + instruction.size();
    }
    if covered < code.len() {
        gaps.push(covered..code.len());
    }
    for gap in gaps {
        let mut at = gap.start;
        while at < gap.end {
            let case = scanner
                .address_tables
                .iter()
                .any(|table| table.contains(&at));
            // A word, halfword or byte, as fits before the end of the gap
            // and the start of the next stretch of data, as objdump cuts
            // data at each `$d`.
            let next = scanner
                .data
                .range(at + 1..)
                .next()
                .map_or(gap.end, |(&start, _)| start);
            let aligned = |size: u64| (address + at as u64).is_multiple_of(size);
            let size = match next.min(gap.end) - at {
                left if left >= 4 && aligned(4) => 4,
                left if left >= 2 && aligned(2) => 2,
                _ => 1,
            };
            let piece = if case && size == 4 {
                Piece::Case
            } else {
                Piece::Data
            };
            pieces.push((at, size, piece));
            at += size;
        }
    }
    pieces.sort_unstable_by_key(|&(offset, _, _)| offset);
    pieces
}
