//! What the walk knows on one way through the code: the numbers in the
//! registers, the condition flags and the words in memory that it can tell,
//! and how it forgets them ([`Known`]); what it knew when it first got to
//! an instruction ([`Visit`]); and the branches whose condition it couldn't
//! tell, which it may go back to ([`Alternative`]). Also how many
//! instructions it may still go through ([`Budget`]), and how a walk that
//! may be dropped puts back what it changed in the walker ([`Mark`],
//! [`Walker::with_budget`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::arch::startup::{Event, LoopStore, Source};
use crate::arch::thumb::SP;
use crate::arch::thumb::decode::{Registers, each};

use super::Walker;

/// How many more instructions the walk may go through: for what it walks,
/// and on top, to count how many times loops run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Budget {
    pub(super) walk: usize,
    pub(super) count: usize,
}

impl Budget {
    /// Each part of this budget, at most as much as `other`'s.
    pub(super) fn min(self, other: Budget) -> Budget {
        Budget {
            walk: self.walk.min(other.walk),
            count: self.count.min(other.count),
        }
    }

    /// What is left of this budget after `spent`, at least none.
    pub(super) fn saturating_sub(self, spent: Budget) -> Budget {
        Budget {
            walk: self.walk.saturating_sub(spent.walk),
            count: self.count.saturating_sub(spent.count),
        }
    }
}

/// What a walk that the walker may drop changes in it, as it was before:
/// how many events the walk had found, and the lowest the stack pointer
/// had been (see [`Walker::rewind`]).
#[derive(Debug, Clone, Copy)]
pub(super) struct Mark {
    pub(super) events: usize,
    pub(super) lowest: Option<u32>,
}

impl Walker<'_> {
    /// Where the walk is now, to go back to with [`Walker::rewind`].
    pub(super) fn mark(&self) -> Mark {
        Mark {
            events: self.events.len(),
            lowest: self.lowest,
        }
    }

    /// Drops the events found since `mark`, which it returns, and puts
    /// back the lowest the stack pointer had been then.
    pub(super) fn rewind(&mut self, mark: Mark) -> Vec<Event> {
        self.lowest = mark.lowest;
        self.events.split_off(mark.events)
    }

    /// Runs `f` with `budget` in place of the walk's, then puts the walk's
    /// back. Returns what `f` returns, and what it spent of `budget`, for
    /// the caller to take from the walk's budget, or not.
    pub(super) fn with_budget<T>(
        &mut self,
        budget: Budget,
        f: impl FnOnce(&mut Self) -> T,
    ) -> (T, Budget) {
        let kept = std::mem::replace(&mut self.budget, budget);
        let result = f(self);
        let spent = budget.saturating_sub(self.budget);
        self.budget = kept;
        (result, spent)
    }
}

/// What the walk knows on the way it goes.
#[derive(Debug, Clone, Default)]
pub(super) struct Known {
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
    /// The compares in one iteration of a loop that the walk counts (see
    /// [`Walker::count`](super::Walker::count)).
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

impl Known {
    /// What the walk knows here, but for the compares of a loop that it
    /// counts, to keep for a loop that comes back here (see [`Visit`]).
    pub(super) fn snapshot(&self) -> Known {
        Known {
            memory: Rc::clone(&self.memory),
            compares: Vec::new(),
            ..*self
        }
    }

    /// What the walk knows at the head of a loop the first iteration, from
    /// what it knew the first time it got there, this, to go through the
    /// code iteration by iteration: not where words in registers were
    /// loaded from, nor the flags, nor that it went past a branch it
    /// couldn't tell.
    pub(super) fn first_iteration(&self) -> Known {
        Known {
            memory: Rc::clone(&self.memory),
            flags: Flags::default(),
            forks: false,
            sources: [None; 16],
            compares: Vec::new(),
            ..*self
        }
    }

    /// Forgets what `registers` hold.
    pub(super) fn forget_registers(&mut self, registers: Registers) {
        for register in each(registers) {
            self.registers[usize::from(register)] = None;
            self.sources[usize::from(register)] = None;
        }
    }

    /// Forgets the words at `addresses`.
    pub(super) fn forget_words(&mut self, addresses: Vec<u32>) {
        if addresses.is_empty() {
            return;
        }
        let words = Rc::make_mut(&mut self.memory);
        for address in addresses {
            words.insert(address, None);
        }
    }

    /// Forgets all the words that code stored on the way.
    pub(super) fn forget_all_words(&mut self) {
        if self.memory.values().any(Option::is_some) {
            (Rc::make_mut(&mut self.memory).values_mut()).for_each(|word| *word = None);
        }
    }

    /// Forgets the flags.
    pub(super) fn forget_flags(&mut self) {
        self.flags = Flags::default();
    }

    /// Forgets what changed, but for the stack pointer, since the registers
    /// held `registers` and memory `memory`, and where words in registers
    /// were loaded from.
    pub(super) fn forget_changes_since(&mut self, registers: &[Option<u32>; 16], memory: &Memory) {
        self.sources = [None; 16];
        for register in (0..16).filter(|&register| register != usize::from(SP)) {
            if self.registers[register] != registers[register] {
                self.registers[register] = None;
            }
        }
        self.forget_words(stored_since(&self.memory, memory));
    }

    /// Forgets the words in memory that a loop may have stored to on the
    /// iterations that the walk didn't go through, from the fourth to the
    /// one before the one before the last, for a loop that it didn't go
    /// through every iteration of (see `Counted::every`): those that its
    /// `stores` cover, or when the walk can't tell them, those that changed
    /// since `before`, at its head.
    pub(super) fn forget_stored(&mut self, stores: Option<&[LoopStore]>, before: &Visit) {
        let Some(stores) = stores else {
            let registers = self.registers;
            self.forget_changes_since(&registers, &before.known.memory);
            return;
        };
        for store in stores {
            let span = i64::from(store.step) * i64::from(store.times - 1);
            let ends = [i64::from(store.address), i64::from(store.address) + span];
            let low = ends[0].min(ends[1]) & !3;
            let high = ends[0].max(ends[1]) + i64::from(store.size) - 1;
            let (Ok(low), Ok(high)) = (u32::try_from(low), u32::try_from(high)) else {
                continue;
            };
            let words = self.memory.range(low..=high).map(|(&address, _)| address);
            self.forget_words(words.collect());
        }
    }
}

/// What the walk knew when it first got to an instruction, for a loop
/// that goes back there.
#[derive(Debug, Clone)]
pub(super) struct Visit {
    /// What it knew, but for the compares (see [`Known::snapshot`]).
    pub(super) known: Known,
    /// How many events the walk had found.
    pub(super) events: usize,
    /// How many instructions the walk had gone through (see `Run::order`).
    pub(super) order: usize,
}

/// A branch whose condition the walk couldn't tell, and what it knew there,
/// to take the branch if the way on doesn't go on.
#[derive(Debug, Clone)]
pub(super) struct Alternative {
    pub(super) site: u64,
    pub(super) target: u64,
    pub(super) events: usize,
    pub(super) known: Known,
    /// How many instructions the walk had gone through.
    pub(super) order: usize,
    pub(super) pending: Vec<(u64, u64)>,
    pub(super) bounds: Option<(u64, u64)>,
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
