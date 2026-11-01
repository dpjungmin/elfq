//! What the walk knows on one way through the code: the numbers in the
//! registers, the condition flags and the words in memory that it can
//! tell ([`Path`]), what it knew when it first got to an instruction
//! ([`Visit`]), and the branches whose condition it couldn't tell, which
//! it may go back to ([`Alternative`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::arch::startup::Source;
use crate::arch::thumb::SP;
use crate::arch::thumb::decode::{Registers, each};

/// What the walk knows on the way it goes.
#[derive(Debug, Clone, Default)]
pub(super) struct Path {
    /// What each register holds, when the walk can tell; pc's is unused.
    pub(super) registers: [Option<u32>; 16],
    /// The words that code stored on the way, by address: what the walk
    /// can tell of one, or `None` when the code stored something it can't
    /// tell there. Shared with the snapshots of earlier instructions until
    /// a store changes it.
    pub(super) memory: Memory,
    pub(super) flags: Flags,
    /// Whether the walk went past a branch or return whose condition it
    /// couldn't tell, so that what it knows holds only on the way it went.
    pub(super) forks: bool,
    /// Of r0 to r3, as bits, those that hold what a call left there (see
    /// `results`), which code hasn't written since.
    pub(super) left: u8,
    /// Of r0 to r3, as bits, those that may hold what the call it is in
    /// leaves, since the walk of that call began, in the function or in
    /// what it called: those that code wrote, and r0 when code read it
    /// other than to push it, as a function such as `memset` returns its
    /// first argument there without writing it. A register that the call
    /// only read keeps what the caller put there, as code built with GCC's
    /// `-fipa-ra` counts on for its next call.
    pub(super) results: u8,
    /// Where the word in each register was loaded from, when the walk can
    /// tell and code hasn't changed the register since (see
    /// [`Store::source`](crate::arch::startup::Store::source)).
    pub(super) sources: [Option<Source>; 16],
    /// The compares in one time through a loop's code that the walk
    /// counts (see [`Walker::count`](super::Walker::count)).
    pub(super) compares: Vec<Compare>,
}

/// The two numbers that an instruction that sets the flags compares, each
/// when the walk can tell it: the flags say how the first compares with
/// the second.
#[derive(Debug, Clone, Copy)]
pub(super) struct Compare {
    pub(super) site: u64,
    pub(super) numbers: [Option<u32>; 2],
}

pub(super) type Memory = Rc<BTreeMap<u32, Option<u32>>>;

/// The condition flags, each when the walk can tell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Flags {
    pub(super) negative: Option<bool>,
    pub(super) zero: Option<bool>,
    pub(super) carry: Option<bool>,
    pub(super) overflow: Option<bool>,
}

/// What the walk knew when it first got to an instruction, for a loop
/// that goes back there.
#[derive(Debug, Clone)]
pub(super) struct Visit {
    pub(super) registers: [Option<u32>; 16],
    pub(super) memory: Memory,
    /// See `Path::sources`, `Path::left` and `Path::results`.
    pub(super) sources: [Option<Source>; 16],
    pub(super) left: u8,
    pub(super) results: u8,
    /// How many events the walk had found.
    pub(super) events: usize,
    /// How many instructions the walk had gone through (see `Run::order`).
    pub(super) order: usize,
    /// See `Path::forks`.
    pub(super) forks: bool,
}

/// A branch whose condition the walk couldn't tell, and what it knew there,
/// to take the branch if the way on doesn't go on.
#[derive(Debug, Clone)]
pub(super) struct Alternative {
    pub(super) site: u64,
    pub(super) target: u64,
    pub(super) events: usize,
    pub(super) path: Path,
    /// How many instructions the walk had gone through.
    pub(super) visited: usize,
    pub(super) pending: Vec<(u64, u64)>,
    pub(super) bounds: Option<(u64, u64)>,
}

/// Forgets what changed in `path`, but for the stack pointer, since it
/// held `registers` and `memory`, and where words were loaded from.
pub(super) fn forget_changes(path: &mut Path, registers: &[Option<u32>; 16], memory: &Memory) {
    path.sources = [None; 16];
    for register in (0..16).filter(|&register| register != usize::from(SP)) {
        if path.registers[register] != registers[register] {
            path.registers[register] = None;
        }
    }
    let changed = stored_since(&path.memory, memory);
    forget_words(path, changed);
}

/// The words that code stored from when memory held `before` to when it
/// held `after`.
pub(super) fn stored_since(after: &Memory, before: &Memory) -> Vec<u32> {
    if Rc::ptr_eq(after, before) {
        return Vec::new();
    }
    (after.iter())
        .filter(|&(address, &word)| before.get(address) != Some(&word))
        .map(|(&address, _)| address)
        .chain(
            (before.keys())
                .filter(|address| !after.contains_key(address))
                .copied(),
        )
        .collect()
}

/// Forgets the words at `addresses` in `path`.
pub(super) fn forget_words(path: &mut Path, addresses: Vec<u32>) {
    if addresses.is_empty() {
        return;
    }
    let words = Rc::make_mut(&mut path.memory);
    for address in addresses {
        words.insert(address, None);
    }
}

/// Forgets what `registers` hold.
pub(super) fn set_unknown(path: &mut Path, registers: Registers) {
    for register in each(registers) {
        path.registers[usize::from(register)] = None;
        path.sources[usize::from(register)] = None;
    }
}
