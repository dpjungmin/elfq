//! Counting how many times a loop runs by going through its code iteration
//! by iteration ([`Walker::count`]), and what each of its stores does over
//! all its iterations ([`summaries`]).

use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use crate::arch::startup::{Event, FunctionBounds, LoopStore, Source, Store};
use crate::arch::thumb::SP;
use crate::arch::thumb::decode::Flow;

use super::each_iteration::{RowLoad, RowWord};
use super::loops::FoundLoop;
use super::walk_state::{Budget, Compare, Known, Visit};
use super::{
    CANDIDATES, Finish, Frame, MOST_ITERATIONS, MOST_WALKED_ITERATIONS, Run, Walker, decode_at,
};

/// A loop whose iterations the walk goes through one by one, as its `mode`
/// says: to count how many times it runs ([`Walker::count`]), or to go
/// through each as the processor would
/// ([`Walker::walk_each_iteration`]).
pub(super) struct OpenLoop {
    /// The branch back.
    pub(super) back: u64,
    /// The instruction after it, where the loop leaves when it doesn't go
    /// back.
    next: u64,
    /// The loop's code, as the first iteration went through it. Before the
    /// first iteration, the walk's own way through the code, when it went
    /// through it in the same run.
    pub(super) code: LoopCode,
    /// Where the iteration stopped, when it did, as [`Walker::stop_here`]
    /// records it.
    pub(super) stopped: Option<StoppedAt>,
    /// What the walk goes through the loop's iterations for.
    pub(super) mode: Mode,
    /// With [`Mode::WalkEach`], the words in the file, outside the stack,
    /// that the iteration's own code reads, as from a row of a table, each
    /// with the instruction that loads it, so that a loop inside can drop
    /// its own (see [`Walker::close_loop`]).
    ///
    /// A word that a register held at the head and the iteration reads
    /// counts as loaded by the instruction that loaded it, when that is in
    /// the loop's function, and no register points to it
    /// ([`NOWHERE`](super::each_iteration::NOWHERE)): as when the iteration
    /// before loaded the start of the next row at its end.
    pub(super) loads: Vec<RowLoad>,
    /// Where the words that registers held at the head of the iteration
    /// were loaded from, for those that the iteration's code hasn't read or
    /// written yet.
    pub(super) carried: [Option<Source>; 16],
    /// The function that the loop is in.
    pub(super) bounds: Option<FunctionBounds>,
}

/// What the walk knows of the code of a loop that it goes through iteration
/// by iteration, to tell where an iteration leaves the loop (see
/// [`Walker::stop_here`](super::Walker::stop_here)).
pub(super) enum LoopCode {
    /// Nothing yet: the iteration leaves past the branch back.
    NotYet,
    /// The instructions of the loop, and where the branches among them
    /// that weren't taken go: an iteration leaves when it goes elsewhere,
    /// at one of `ways_out`, or a way the walk can't tell.
    Seen {
        code: HashSet<u64>,
        ways_out: Vec<u64>,
    },
}

/// Where one iteration of a loop that the walk goes through iteration by
/// iteration stopped: at the branch back, or where it leaves the loop.
#[derive(Debug, Clone, Copy)]
pub(super) struct StoppedAt {
    pub(super) at: u64,
    /// Whether the loop goes back to its head from there, when the walk
    /// can tell.
    pub(super) goes_back: Option<bool>,
}

/// What the walk goes through a loop's iterations one by one for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// To count how many times the loop runs (see [`Walker::count`]):
    /// following no calls, with instructions and events that don't count
    /// for the walk.
    Count,
    /// To go through each iteration as the processor would, with its calls,
    /// the loops inside and their events, as for a loop over a table of
    /// sections (see [`Walker::walk_each_iteration`]).
    WalkEach,
}

/// What the walk knows where one iteration of a loop stops (see
/// [`Walker::walk_iteration`]).
pub(super) struct IterationEnd {
    /// What the walk knows at the end.
    pub(super) known: Known,
    stores: Vec<Store>,
    /// Whether it makes calls, which the walk doesn't follow, or goes into
    /// loops, which it doesn't count: then `stores` aren't all that the
    /// iteration stores.
    others: bool,
    compares: Vec<Compare>,
    /// Whether the loop goes back to its head after it, rather than
    /// leaving.
    pub(super) again: bool,
    /// Where the loop goes when it leaves after it.
    pub(super) exit: u64,
    /// The instructions it went through, and where the branches it
    /// didn't take go.
    order: Vec<u64>,
    ways_out: Vec<u64>,
    /// The words that it loaded, as [`OpenLoop::loads`] says, the lowest
    /// first.
    pub(super) loads: Vec<RowWord>,
}

/// What a loop leaves, as the walk counted it.
pub(super) struct Counted {
    pub(super) iterations: u32,
    /// What the walk knows where the loop leaves, and where that is.
    pub(super) known: Known,
    pub(super) exit: u64,
    pub(super) stores: Option<Vec<LoopStore>>,
    /// Whether its iterations make calls or go into loops (see
    /// `IterationEnd::others`).
    pub(super) others: bool,
    /// Whether the walk went through the code in every iteration, so that
    /// what it knows where the loop leaves holds.
    pub(super) every: bool,
}

/// How many iterations of a loop the walk goes through before it fits a
/// [`Line`] through their ends, which takes three.
const FIRST_ITERATIONS: u32 = 3;

/// Where what a loop's code changes is at the start of each iteration, from
/// the second on, as the ends of the first three iterations tell: each
/// register, address a register's word was loaded from, and word in memory
/// either stays, or moves by the same amount each iteration, or the walk
/// can't tell it, as a word that code loaded into a register.
///
/// Iterations count from 1, and the ends of the first three are the starts
/// of the second to the fourth, so the line starts at the second: the
/// `n`th iteration starts `n - 2` steps along it, as [`Line::start`],
/// [`leaving_iterations`] and [`keeps_order`] count.
struct Line {
    /// What the walk knows at the start of the second iteration.
    start: Known,
    /// How far each register moves each iteration, when by the same amount.
    registers: [Option<u32>; 16],
    /// How far the address each register's word was loaded from moves each
    /// iteration, when by the same amount (see `Known::sources`).
    sources: [Option<u32>; 16],
    /// The words in memory that change, and how far each moves each
    /// iteration, when by the same amount.
    memory: Vec<(u32, Option<u32>)>,
}

impl Line {
    /// The line through the ends of the first three iterations: `None` when
    /// the stack pointer moves.
    fn new(first: &Known, second: &Known, third: &Known) -> Option<Line> {
        let sp = usize::from(SP);
        if first.registers[sp] != second.registers[sp]
            || second.registers[sp] != third.registers[sp]
        {
            return None;
        }
        // A word that code loaded moves as the memory it came from does,
        // not by the same amount each iteration.
        let registers = std::array::from_fn(|r| {
            let loaded = [first, second, third]
                .iter()
                .any(|known| known.sources[r].is_some());
            let [a, b, c] = [first, second, third].map(|known| known.registers[r]);
            if loaded { None } else { moves(a, b, c) }
        });
        let sources = std::array::from_fn(|r| {
            let [a, b, c] = [first, second, third].map(|known| known.sources[r]);
            match (a, b, c) {
                (Some(a), Some(b), Some(c)) if a.site == b.site && b.site == c.site => {
                    moves(Some(a.address), Some(b.address), Some(c.address))
                }
                _ => None,
            }
        });
        let mut memory = Vec::new();
        if !(Rc::ptr_eq(&first.memory, &second.memory) && Rc::ptr_eq(&second.memory, &third.memory))
        {
            let addresses: BTreeSet<u32> = [first, second, third]
                .iter()
                .flat_map(|known| known.memory.keys().copied())
                .collect();
            for address in addresses {
                let [a, b, c] =
                    [first, second, third].map(|known| known.memory.get(&address).copied());
                if a == b && b == c {
                    continue;
                }
                let by = match (a, b, c) {
                    (Some(a), Some(b), Some(c)) => moves(a, b, c),
                    _ => None,
                };
                memory.push((address, by));
            }
        }
        let mut start = first.clone();
        if !(first.flags == second.flags && second.flags == third.flags) {
            start.forget_flags();
        }
        Some(Line {
            start,
            registers,
            sources,
            memory,
        })
    }

    /// What the walk knows at the start of the `n`th iteration, from the
    /// second on.
    fn start(&self, n: u32) -> Known {
        let times = n - 2;
        let at =
            |from: Option<u32>, by: Option<u32>| Some(from?.wrapping_add(by?.wrapping_mul(times)));
        let mut known = self.start.clone();
        for r in 0..16 {
            known.registers[r] = at(known.registers[r], self.registers[r]);
            known.sources[r] = known.sources[r].and_then(|source| {
                let address = at(Some(source.address), self.sources[r])?;
                Some(Source { address, ..source })
            });
        }
        if !self.memory.is_empty() {
            let memory = Rc::make_mut(&mut known.memory);
            for &(address, by) in &self.memory {
                let word = memory.get(&address).copied().flatten();
                memory.insert(address, at(word, by));
            }
        }
        known
    }
}

impl Walker<'_> {
    /// How many times the loop that the walk `found` runs, what it leaves,
    /// and what its stores do, when the walk can tell, from what it knew
    /// the first time it got to the head:
    ///
    /// 1. It goes through the loop's code the first three iterations, as
    ///    the processor would.
    /// 2. Each compare whose numbers move by the same amount each iteration
    ///    tells when it may hold the other way, so that the loop may leave
    ///    then ([`leaving_iterations`], at most [`CANDIDATES`] of them).
    ///    The first such iteration after which the loop leaves, as the walk
    ///    finds going through that iteration from where each number would
    ///    be, and goes back the iteration before, is when it leaves.
    /// 3. When none is, it goes on through the code iteration by iteration,
    ///    up to [`MOST_WALKED_ITERATIONS`] times, as for a loop over a
    ///    table that ends at a zero word.
    ///
    /// Only for a loop whose branch back goes to its head, which one whose
    /// code the compiler put after the function's other code doesn't. The
    /// walk doesn't follow the loop's calls, nor count loops inside it, so
    /// with either it can't tell what the loop's stores do.
    pub(super) fn count(&mut self, run: &Run, frame: Frame, found: &FoundLoop) -> Option<Counted> {
        let counts = (self.open_loops.iter()).any(|open| open.mode == Mode::Count);
        if counts || self.budget.count == 0 {
            return None;
        }
        let open = self.open_loop(run, found, Mode::Count)?;
        self.open_loops.push(open);
        // Counting spends only from the count budget (see
        // [`Walker::walk_iteration`]), not from the walk's own.
        let unlimited = Budget {
            walk: usize::MAX,
            ..self.budget
        };
        let (counted, spent) = self.with_budget(unlimited, |walker| {
            walker.count_iterations(frame, found.head, found.before)
        });
        self.budget.count -= spent.count;
        self.open_loops.pop();
        counted
    }

    /// The loop that the walk `found`, to go through iteration by
    /// iteration, as [`OpenLoop`] says, in `mode`: `None` unless the branch
    /// back goes to the head.
    pub(super) fn open_loop(&self, run: &Run, found: &FoundLoop, mode: Mode) -> Option<OpenLoop> {
        let FoundLoop {
            head, back, before, ..
        } = *found;
        let branch = decode_at(self.program, back)?;
        match branch.effect.flow {
            Flow::Branch { target, .. } if target == head => {}
            _ => return None,
        }
        // The code the walk went through from the head, which may lie
        // past the branch back, as GCC -Os puts a loop's test.
        let code = match run.order.get(before.order) == Some(&head) {
            true => {
                let code: HashSet<u64> = run.order[before.order..].iter().copied().collect();
                let ways_out = (run.pending.iter())
                    .filter(|(from, to)| code.contains(from) && !code.contains(to))
                    .map(|&(_, to)| to)
                    .collect();
                LoopCode::Seen { code, ways_out }
            }
            false => LoopCode::NotYet,
        };
        Some(OpenLoop {
            back,
            next: branch.next(),
            code,
            stopped: None,
            mode,
            loads: Vec::new(),
            carried: [None; 16],
            bounds: self.program.function_at(head),
        })
    }

    /// Goes through the code of the loop that the walk counts from `head`,
    /// from what the walk knew there the first time it got there, `before`,
    /// as [`Walker::count`] says.
    fn count_iterations(&mut self, frame: Frame, head: u64, before: &Visit) -> Option<Counted> {
        let mut start = before.known.first_iteration();
        let mut walked: Vec<(u32, IterationEnd)> = Vec::new();
        for n in 1..=FIRST_ITERATIONS {
            let ended = self.walk_iteration(frame, head, start)?;
            if n == 1 {
                self.learn_code(&ended);
            }
            let again = ended.again;
            start = ended.known.clone();
            walked.push((n, ended));
            if !again {
                return Some(counted(walked, n));
            }
        }
        let [(_, first), (_, second), (_, third)] = walked.as_slice() else {
            unreachable!("the walk went through the first three iterations");
        };
        if let Some(line) = Line::new(&first.known, &second.known, &third.known) {
            for n in leaving_iterations(second, third) {
                if !keeps_order(second, third, n - 1) {
                    break;
                }
                // The iteration before goes back, from where the line says,
                // unless it is the third, which the walk went through.
                let before = match n {
                    n if n == FIRST_ITERATIONS + 1 => None,
                    _ => match self.walk_iteration(frame, head, line.start(n - 1)) {
                        Some(before) if before.again => Some(before),
                        _ => continue,
                    },
                };
                // Where the iteration before ends, which the line must go
                // through.
                let line_start = line.start(n);
                let ends = before.as_ref().map_or(&third.known, |before| &before.known);
                let on_line =
                    |r: usize| line_start.registers[r].is_none_or(|n| ends.registers[r] == Some(n));
                if !(0..16).all(on_line) {
                    continue;
                }
                match self.walk_iteration(frame, head, ends.clone()) {
                    Some(last) if !last.again => {
                        walked.extend(before.map(|before| (n - 1, before)));
                        walked.push((n, last));
                        return Some(counted(walked, n));
                    }
                    _ => continue,
                }
            }
        }
        // Iteration by iteration, as a loop whose compares tell nothing
        // that moves by the same amount, such as one over a table that ends
        // at a row of zeros.
        for n in FIRST_ITERATIONS + 1..=MOST_WALKED_ITERATIONS {
            let ended = self.walk_iteration(frame, head, start)?;
            let again = ended.again;
            start = ended.known.clone();
            walked.push((n, ended));
            if !again {
                return Some(counted(walked, n));
            }
        }
        None
    }

    /// Where the innermost loop that the walk goes through iteration by
    /// iteration leaves, as the first iteration, `first`, tells.
    pub(super) fn learn_code(&mut self, first: &IterationEnd) {
        if let Some(open) = self.open_loops.last_mut() {
            open.code = LoopCode::Seen {
                code: first.order.iter().copied().collect(),
                ways_out: first.ways_out.clone(),
            };
        }
    }

    /// Goes through a loop's code once from `head`, from `known`, as one
    /// iteration, where it stops at the branch back or where the loop
    /// leaves (see [`OpenLoop`]), for the innermost loop that the walk goes
    /// through iteration by iteration: `None` when it goes past a branch
    /// that the walk can't tell, or the walk can't tell whether the loop
    /// goes back after it. To count the loop ([`Mode::Count`]), it follows
    /// no calls, drops its events and spends from the count budget, as
    /// [`Walker::count`] sets the walk's own aside; otherwise, its events
    /// and instructions are the walk's.
    pub(super) fn walk_iteration(
        &mut self,
        frame: Frame,
        head: u64,
        known: Known,
    ) -> Option<IterationEnd> {
        let mark = self.mark();
        let index = self.open_loops.len().checked_sub(1)?;
        let mode = self.open_loops[index].mode;
        let body = frame.iteration(index, mode, self.budget.count);
        if mode == Mode::WalkEach {
            self.open_loops[index].carried = known.sources;
        }
        let (finish, run) = self.run(head, body, known);
        let open = &mut self.open_loops[index];
        let stopped = open.stopped.take();
        let loads = std::mem::take(&mut open.loads);
        let (back, next) = (open.back, open.next);
        let mut loads: Vec<RowWord> = loads.into_iter().map(|load| load.word).collect();
        loads.sort_unstable();
        loads.dedup();
        let mut stores = Vec::new();
        let mut others = false;
        if mode == Mode::Count {
            self.budget.count = self.budget.count.saturating_sub(run.count);
            for event in self.rewind(mark) {
                match event {
                    Event::Store(store) if store.conditional => return None,
                    Event::Store(store) => stores.push(store),
                    _ => others = true,
                }
            }
        }
        let (
            Finish::AtStop,
            Some(StoppedAt {
                at,
                goes_back: Some(again),
            }),
            false,
        ) = (finish, stopped, run.known.forks)
        else {
            return None;
        };
        let exit = if at == back { next } else { at };
        let mut known = run.known;
        let compares = std::mem::take(&mut known.compares);
        Some(IterationEnd {
            known,
            stores,
            others,
            compares,
            again,
            exit,
            order: run.order,
            ways_out: run.pending.iter().map(|&(_, to)| to).collect(),
            loads,
        })
    }
}

/// How far `a`, `b` and `c` move each iteration, when by the same amount: 0
/// when the walk can tell none of them.
fn moves(a: Option<u32>, b: Option<u32>, c: Option<u32>) -> Option<u32> {
    match (a, b, c) {
        (None, None, None) => Some(0),
        (Some(a), Some(b), Some(c)) if b.wrapping_sub(a) == c.wrapping_sub(b) => {
            Some(b.wrapping_sub(a))
        }
        _ => None,
    }
}

/// The numbers that each compare of `second` and `third`, two iterations of
/// a loop in a row, compares, with what they are the first of those
/// iterations and how far they move each iteration, as numbers with no sign
/// or, with `signed`, with one, when the walk can tell them: those of the
/// compares that both iterations make in the same order.
fn compared(second: &IterationEnd, third: &IterationEnd, signed: bool) -> Vec<[(i64, i64); 2]> {
    let view = |n: u32| match signed {
        true => i64::from(n as i32),
        false => i64::from(n),
    };
    let same = |(a, b): &(&Compare, &Compare)| a.site == b.site;
    let pairs = second.compares.iter().zip(&third.compares);
    pairs
        .take_while(same)
        .filter_map(|(a, b)| {
            let ([Some(a1), Some(b1)], [Some(a2), Some(b2)]) = (a.numbers, b.numbers) else {
                return None;
            };
            let by = |from: u32, to: u32| i64::from(to.wrapping_sub(from) as i32);
            Some([(view(a1), by(a1, a2)), (view(b1), by(b1, b2))])
        })
        .collect()
}

/// When a loop may leave, from the fourth iteration on, as the compares of
/// the second and third iterations of its code tell: around each iteration
/// when one of them may hold the other way, as the gap between its two
/// numbers closes, taken with no sign or with one. At most [`CANDIDATES`]
/// of them, the earliest first.
fn leaving_iterations(second: &IterationEnd, third: &IterationEnd) -> Vec<u32> {
    let mut candidates = BTreeSet::new();
    for signed in [false, true] {
        for [(a, a_by), (b, b_by)] in compared(second, third, signed) {
            let (gap, closes) = (a - b, b_by - a_by);
            // The gap closes `gap / closes` iterations after the second.
            if closes == 0 || gap.signum() != closes.signum() {
                continue;
            }
            let Ok(after) = u32::try_from(gap / closes) else {
                continue;
            };
            let crossing = after.saturating_add(2);
            for n in crossing.saturating_sub(1)..=crossing.saturating_add(2) {
                if (FIRST_ITERATIONS + 1..=MOST_ITERATIONS).contains(&n) {
                    candidates.insert(n);
                }
            }
        }
    }
    candidates.into_iter().take(CANDIDATES).collect()
}

/// Whether the numbers that each compare of `second` and `third`, the
/// second and third iterations of a loop's code, compares keep moving
/// without wrapping round up to the `last`th iteration, with no sign and
/// with one: then each compare's flags change only where the gap between
/// its numbers closes, where [`leaving_iterations`] looks.
fn keeps_order(second: &IterationEnd, third: &IterationEnd, last: u32) -> bool {
    let times = i64::from(last - 2);
    [
        (false, 0, u32::MAX as i64),
        (true, i32::MIN as i64, i32::MAX as i64),
    ]
    .into_iter()
    .all(|(signed, low, high)| {
        compared(second, third, signed).into_iter().all(|numbers| {
            numbers.into_iter().all(|(from, by)| {
                let to = from + by * times;
                (low..=high).contains(&to)
            })
        })
    })
}

/// What a loop leaves after it runs `iterations` times, with its iterations
/// that the walk went through, `walked`, by number, the last one last.
fn counted(walked: Vec<(u32, IterationEnd)>, iterations: u32) -> Counted {
    let stores = summaries(&walked, iterations);
    let others = walked.iter().any(|(_, ended)| ended.others);
    let every = walked.len() == iterations as usize;
    let (_, last) = walked.into_iter().next_back().expect("the last iteration");
    Counted {
        iterations,
        known: last.known,
        exit: last.exit,
        stores,
        others,
        every,
    }
}

/// What each store of a loop that runs `iterations` times does over all the
/// iterations, from those that the walk went through, `walked`: when each
/// iteration but the last stores with the same instructions as the first,
/// and the last with the first of them, and the walk can tell where each
/// stores, which moves by the same amount each iteration.
fn summaries(walked: &[(u32, IterationEnd)], iterations: u32) -> Option<Vec<LoopStore>> {
    if walked.iter().any(|(_, ended)| ended.others) {
        return None;
    }
    let first = &walked.first()?.1.stores;
    let sites = |ended: &IterationEnd| {
        ended
            .stores
            .iter()
            .map(|store| store.site)
            .collect::<Vec<_>>()
    };
    let all = sites(&walked[0].1);
    let same = walked.iter().all(|(n, ended)| match *n == iterations {
        true => all.starts_with(&sites(ended)),
        false => sites(ended) == all,
    });
    if !same {
        return None;
    }
    (first.iter().enumerate())
        .map(|(i, store)| {
            let each: Vec<(u32, &Store)> = (walked.iter())
                .filter_map(|(n, ended)| Some((*n, ended.stores.get(i)?)))
                .collect();
            let (address, step) = line_through(each.iter().map(|&(n, store)| (n, store.address)))?;
            if each.iter().any(|(_, other)| other.size != store.size) || store.size == 0 {
                return None;
            }
            let value = (each.iter())
                .all(|(_, other)| other.value == store.value)
                .then_some(store.value)
                .flatten();
            let source_at = |store: &Store| store.source.map(|source| source.address);
            let loaded = !each
                .iter()
                .all(|(_, other)| source_at(other) == source_at(store));
            let source = line_through(each.iter().map(|&(n, store)| (n, source_at(store))));
            let last = walked
                .last()
                .map(|(_, ended)| ended.stores.len())
                .unwrap_or(0);
            Some(LoopStore {
                site: store.site,
                address,
                step,
                size: store.size,
                times: iterations - 1 + u32::from(i < last),
                value,
                source,
                loaded,
            })
        })
        .collect()
}

/// Where `points`, the `n`th iteration's number for some `n`, the first
/// first, start and how far they move each iteration, when the walk can
/// tell each and they move by the same amount.
pub(super) fn line_through(
    mut points: impl Iterator<Item = (u32, Option<u32>)>,
) -> Option<(u32, i32)> {
    let (1, Some(first)) = points.next()? else {
        return None;
    };
    let mut step = None;
    for (n, number) in points {
        let number = number?;
        let times = (n - 1) as i32;
        let by = *step.get_or_insert_with(|| number.wrapping_sub(first) as i32 / times);
        if first.wrapping_add_signed(by.wrapping_mul(times)) != number {
            return None;
        }
    }
    Some((first, step.unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An iteration of a loop whose compares, at 0x100, 0x110 and on,
    /// compare `numbers`.
    fn iteration(numbers: &[[Option<u32>; 2]]) -> IterationEnd {
        IterationEnd {
            known: Known::default(),
            stores: Vec::new(),
            others: false,
            compares: (numbers.iter().zip(0..))
                .map(|(&numbers, k)| Compare {
                    site: 0x100 + 0x10 * k,
                    numbers,
                })
                .collect(),
            again: true,
            exit: 0,
            order: Vec::new(),
            ways_out: Vec::new(),
            loads: Vec::new(),
        }
    }

    #[test]
    fn a_line_through_numbers_that_move_by_the_same_amount() {
        let line = |points: &[(u32, Option<u32>)]| line_through(points.iter().copied());
        // The iterations it went through need not follow each other.
        assert_eq!(
            line(&[(1, Some(0x100)), (2, Some(0x104)), (4, Some(0x10c))]),
            Some((0x100, 4))
        );
        assert_eq!(
            line(&[(1, Some(0x100)), (2, Some(0xfc))]),
            Some((0x100, -4))
        );
        assert_eq!(
            line(&[(1, Some(0xffff_fffc)), (2, Some(0)), (3, Some(4))]),
            Some((0xffff_fffc, 4))
        );
        assert_eq!(line(&[(1, Some(0x100))]), Some((0x100, 0)));
        // Not by the same amount: 5 bytes in two iterations is no whole
        // step.
        assert_eq!(line(&[(1, Some(0x100)), (3, Some(0x105))]), None);
        assert_eq!(
            line(&[(1, Some(0x100)), (2, Some(0x104)), (3, Some(0x10c))]),
            None
        );
        // A number the walk can't tell, or no first iteration.
        assert_eq!(line(&[(1, Some(0x100)), (2, None)]), None);
        assert_eq!(line(&[(2, Some(0x104)), (3, Some(0x108))]), None);
        assert_eq!(line(&[]), None);
    }

    #[test]
    fn a_loop_may_leave_around_where_a_compare_closes_its_gap() {
        // `cmp r0, r1` with r0 counting up by 4 to r1, 0x40: 8 the second
        // iteration and 12 the third, so it gets to 0x40 the 16th.
        let (second, third) = (
            iteration(&[[Some(8), Some(0x40)]]),
            iteration(&[[Some(12), Some(0x40)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [15, 16, 17, 18]);
        // Counting down to 0 too, as `subs r2, #4; bne` does.
        let (second, third) = (
            iteration(&[[Some(0x18), Some(0)]]),
            iteration(&[[Some(0x14), Some(0)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [7, 8, 9, 10]);
    }

    #[test]
    fn only_from_the_fourth_iteration_on() {
        // The gap closes the third iteration, so the iterations around it
        // would be the 2nd to the 5th, but the walk went through the first
        // three.
        let (second, third) = (
            iteration(&[[Some(4), Some(8)]]),
            iteration(&[[Some(8), Some(8)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [4, 5]);
    }

    #[test]
    fn a_gap_that_doesnt_close_says_nothing() {
        // r0 counts away from the bound, or both move together.
        let (second, third) = (
            iteration(&[[Some(8), Some(0x40)]]),
            iteration(&[[Some(4), Some(0x40)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [] as [u32; 0]);
        let (second, third) = (
            iteration(&[[Some(8), Some(0x40)]]),
            iteration(&[[Some(12), Some(0x44)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [] as [u32; 0]);
        // A number the walk can't tell.
        let (second, third) = (
            iteration(&[[None, Some(0x40)]]),
            iteration(&[[None, Some(0x40)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [] as [u32; 0]);
    }

    #[test]
    fn compares_count_only_while_both_iterations_make_them_in_the_same_order() {
        let mut second = iteration(&[[Some(8), Some(0x40)]]);
        let third = iteration(&[[Some(12), Some(0x40)]]);
        second.compares[0].site = 0x104;
        assert_eq!(leaving_iterations(&second, &third), [] as [u32; 0]);
    }

    #[test]
    fn numbers_are_taken_with_a_sign_too() {
        // r0 counts up from -8 to 8: with no sign, it starts above 8 and
        // moves away; with one, it gets there in the 6th.
        let (second, third) = (
            iteration(&[[Some(-8i32 as u32), Some(8)]]),
            iteration(&[[Some(-4i32 as u32), Some(8)]]),
        );
        assert_eq!(leaving_iterations(&second, &third), [5, 6, 7, 8]);
    }

    #[test]
    fn at_most_twelve_candidates_are_tried_the_earliest_first() {
        // Five compares, which close their gaps in the 20th, 30th, 40th,
        // 50th and 60th iterations: four iterations around each.
        let numbers = |third: bool| {
            (0..5)
                .map(|k| [Some(u32::from(third) * 4), Some(72 + 40 * k)])
                .collect::<Vec<_>>()
        };
        let (second, third) = (iteration(&numbers(false)), iteration(&numbers(true)));
        assert_eq!(
            leaving_iterations(&second, &third),
            [19, 20, 21, 22, 29, 30, 31, 32, 39, 40, 41, 42]
        );
    }

    #[test]
    fn numbers_keep_their_order_unless_they_wrap_round() {
        let (second, third) = (
            iteration(&[[Some(8), Some(0x40)]]),
            iteration(&[[Some(12), Some(0x40)]]),
        );
        assert!(keeps_order(&second, &third, 16));
        // Past 0xffffffff with no sign, from the 6th iteration on.
        let (second, third) = (
            iteration(&[[Some(0xffff_fff0), Some(0)]]),
            iteration(&[[Some(0xffff_fff4), Some(0)]]),
        );
        assert!(keeps_order(&second, &third, 5));
        assert!(!keeps_order(&second, &third, 6));
        // Past 0x7fffffff with a sign.
        let (second, third) = (
            iteration(&[[Some(0x7fff_fff0), Some(0)]]),
            iteration(&[[Some(0x7fff_fff4), Some(0)]]),
        );
        assert!(keeps_order(&second, &third, 5));
        assert!(!keeps_order(&second, &third, 6));
    }
}
