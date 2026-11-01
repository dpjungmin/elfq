use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use crate::arch::startup::{Event, LoopStore, Source, Store};
use crate::arch::thumb::decode::Flow;
use crate::arch::thumb::{SP, is_wide};

use super::walk_state::{Compare, Flags, Path, Visit, forget_changes};
use super::{CANDIDATES, Frame, LOOP_LIMIT, MOST_PASSES, MOST_TIMES, Run, Walker};

/// A loop whose code the walk goes through time by time to count how many
/// times it runs (see [`Walker::count`]).
pub(super) struct Counting {
    /// The branch back.
    pub(super) back: u64,
    /// The instruction after it, where the loop leaves when it doesn't go
    /// back.
    next: u64,
    /// The instructions it went through the first time, and where the
    /// branches it didn't take then go: a later time that goes elsewhere
    /// leaves there, or goes a way the walk can't tell. Before the first
    /// time, the walk's own way through the code, when it went through it
    /// in the same run; otherwise empty, and the first time leaves past
    /// `back`.
    pub(super) code: HashSet<u64>,
    pub(super) ways_out: Vec<u64>,
    /// Where the time through the code stopped, and whether it goes back
    /// to the loop's head from there, when the walk can tell.
    pub(super) stopped: Option<(u64, Option<bool>)>,
    /// Whether the times through the code follow calls, count the loops
    /// inside and keep their events, as for a loop that the walk goes
    /// through each time (see [`Walker::walk_times`]), rather than to
    /// count how many times it runs.
    pub(super) walks: bool,
    /// The addresses that the time through the loop's own code loads words
    /// from, in the file and outside the stack, as a row of a table, when
    /// `walks`, each with the load instruction, so that a loop inside can
    /// drop its own, and with where the register it loads through points,
    /// when lower: with those of the words that registers held at the head
    /// that it reads, as a loop does whose time loads the start of the next
    /// row at its end, which point nowhere, `u32::MAX`.
    pub(super) loads: Vec<(u64, u32, u32)>,
    /// Where the words that registers held at the head of the time were
    /// loaded from, for those that the time's code hasn't read or written
    /// yet.
    pub(super) carried: [Option<Source>; 16],
    /// The function that the loop is in.
    pub(super) bounds: Option<(u64, u64)>,
}

/// One time through a loop's code (see [`Walker::pass`]).
pub(super) struct Pass {
    /// What the walk knows at the end.
    pub(super) path: Path,
    stores: Vec<Store>,
    /// Whether it makes calls, which the walk doesn't follow, or goes
    /// into loops, which it doesn't count: then `stores` aren't all that
    /// the time stores.
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
    /// The addresses that it loaded words from, as [`Counting::loads`]
    /// says, the lowest first, each with where the register it loaded
    /// through points, when lower, or `u32::MAX`.
    pub(super) loads: Vec<(u32, u32)>,
}

/// What a loop leaves, as the walk counted it.
pub(super) struct Counted {
    pub(super) passes: u32,
    /// What the walk knows where the loop leaves, and where that is.
    pub(super) path: Path,
    pub(super) exit: u64,
    pub(super) stores: Option<Vec<LoopStore>>,
    /// Whether the times through its code make calls or go into loops (see
    /// `Pass::others`).
    pub(super) others: bool,
    /// Whether the walk went through the code every time the loop runs,
    /// so that what it knows where the loop leaves holds.
    pub(super) every: bool,
}

/// Where what a loop's code changes is at the start of each time it runs,
/// from the second on, as the ends of the first three times tell: each
/// register, address a register's word was loaded from, and word in
/// memory either stays, or moves by the same amount each time, or the walk
/// can't tell it, as a word that code loaded into a register.
struct Line {
    /// What the walk knows at the start of the second time.
    start: Path,
    /// How far each register moves each time, when by the same amount.
    registers: [Option<u32>; 16],
    /// How far the address each register's word was loaded from moves
    /// each time, when by the same amount (see `Path::sources`).
    sources: [Option<u32>; 16],
    /// The words in memory that change, and how far each moves each time,
    /// when by the same amount.
    memory: Vec<(u32, Option<u32>)>,
}

impl Line {
    /// The line through the ends of the first three times: `None` when
    /// the stack pointer moves.
    fn new(first: &Path, second: &Path, third: &Path) -> Option<Line> {
        let sp = usize::from(SP);
        if first.registers[sp] != second.registers[sp]
            || second.registers[sp] != third.registers[sp]
        {
            return None;
        }
        // A word that code loaded moves as the memory it came from does,
        // not by the same amount each time.
        let registers = std::array::from_fn(|r| {
            let loaded = [first, second, third]
                .iter()
                .any(|path| path.sources[r].is_some());
            let [a, b, c] = [first, second, third].map(|path| path.registers[r]);
            if loaded { None } else { moves(a, b, c) }
        });
        let sources = std::array::from_fn(|r| {
            let [a, b, c] = [first, second, third].map(|path| path.sources[r]);
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
                .flat_map(|path| path.memory.keys().copied())
                .collect();
            for address in addresses {
                let [a, b, c] =
                    [first, second, third].map(|path| path.memory.get(&address).copied());
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
            start.flags = Flags::default();
        }
        Some(Line {
            start,
            registers,
            sources,
            memory,
        })
    }

    /// What the walk knows at the start of the `n`th time, from the
    /// second on.
    fn start(&self, n: u32) -> Path {
        let times = n - 2;
        let at =
            |from: Option<u32>, by: Option<u32>| Some(from?.wrapping_add(by?.wrapping_mul(times)));
        let mut path = self.start.clone();
        for r in 0..16 {
            path.registers[r] = at(path.registers[r], self.registers[r]);
            path.sources[r] = path.sources[r].and_then(|source| {
                let address = at(Some(source.address), self.sources[r])?;
                Some(Source { address, ..source })
            });
        }
        if !self.memory.is_empty() {
            let memory = Rc::make_mut(&mut path.memory);
            for &(address, by) in &self.memory {
                let word = memory.get(&address).copied().flatten();
                memory.insert(address, at(word, by));
            }
        }
        path
    }
}

impl Walker<'_> {
    /// How many times the loop from `head` to the branch back at `back`
    /// runs, what it leaves, and what its stores do, when the walk can
    /// tell. It goes through the loop's code from what it knew at the
    /// head, `before`, the first three times, as the processor would. Then
    /// each compare whose numbers move by the same amount each time tells
    /// when it may hold the other way, and the loop may leave; the first
    /// such time that the walk finds the loop leaves, going through the
    /// code that time from where each number would be, and goes back the
    /// time before, is when it leaves. When none does, it goes on through
    /// the code time by time, up to [`MOST_TIMES`] times, as for a loop
    /// over a table that ends at a zero word. Only a loop whose branch
    /// back goes to its head, as one with its code after the function's
    /// other code doesn't; the walk doesn't follow its calls, nor count
    /// loops inside it, and then can't tell what its stores do.
    pub(super) fn count(
        &mut self,
        run: &Run,
        frame: Frame,
        head: u64,
        back: u64,
        before: &Visit,
    ) -> Option<Counted> {
        let counts = self.counting.iter().any(|counting| !counting.walks);
        if counts || self.count_budget == 0 {
            return None;
        }
        let counting = self.times_of(run, head, back, before, false)?;
        self.counting.push(counting);
        let budget = self.budget;
        let counted = self.count_passes(frame, head, before);
        self.budget = budget;
        self.counting.pop();
        counted
    }

    /// The loop from `head` to the branch back at `back`, to go through
    /// time by time from what the walk knew at its head the first time,
    /// `before`, as [`Counting`] says, with `walks`: `None` unless the
    /// branch back goes to the head.
    pub(super) fn times_of(
        &self,
        run: &Run,
        head: u64,
        back: u64,
        before: &Visit,
        walks: bool,
    ) -> Option<Counting> {
        let (first, _, effect) = self.decode(back)?;
        match effect.flow {
            Flow::Branch { target, .. } if target == head => {}
            _ => return None,
        }
        // The code the walk went through from the head, which may lie
        // past the branch back, as GCC -Os puts a loop's test.
        let walked = match run.order.get(before.order) == Some(&head) {
            true => &run.order[before.order..],
            false => &[],
        };
        let code: HashSet<u64> = walked.iter().copied().collect();
        let ways_out = (run.pending.iter())
            .filter(|(from, to)| code.contains(from) && !code.contains(to))
            .map(|&(_, to)| to)
            .collect();
        Some(Counting {
            back,
            next: back + if is_wide(first) { 4 } else { 2 },
            code,
            ways_out,
            stopped: None,
            walks,
            loads: Vec::new(),
            carried: [None; 16],
            bounds: self.program.function_at(head),
        })
    }

    /// Goes through the code of the loop that the walk counts from
    /// `head`, from what the walk knew there the first time, `before`, as
    /// [`Walker::count`] says.
    fn count_passes(&mut self, frame: Frame, head: u64, before: &Visit) -> Option<Counted> {
        let mut start = first_time(before);
        let mut walked: Vec<(u32, Pass)> = Vec::new();
        for n in 1..=3 {
            let pass = self.pass(frame, head, start)?;
            if n == 1 {
                self.learn_code(&pass);
            }
            let again = pass.again;
            start = pass.path.clone();
            walked.push((n, pass));
            if !again {
                return Some(counted(walked, n));
            }
        }
        if let Some(line) = Line::new(&walked[0].1.path, &walked[1].1.path, &walked[2].1.path) {
            let (second, third) = (&walked[1].1, &walked[2].1);
            for n in leaving_times(second, third) {
                if !keeps_order(second, third, n - 1) {
                    break;
                }
                // The time before goes back, from where the line says.
                let before = match n {
                    4 => None,
                    _ => match self.pass(frame, head, line.start(n - 1)) {
                        Some(before) if before.again => Some(before),
                        _ => continue,
                    },
                };
                // Where the time before ends, which the line must go
                // through.
                let line_start = line.start(n);
                let ends = before
                    .as_ref()
                    .map_or(&walked[2].1.path, |before| &before.path);
                let on_line =
                    |r: usize| line_start.registers[r].is_none_or(|n| ends.registers[r] == Some(n));
                if !(0..16).all(on_line) {
                    continue;
                }
                match self.pass(frame, head, ends.clone()) {
                    Some(last) if !last.again => {
                        walked.extend(before.map(|before| (n - 1, before)));
                        walked.push((n, last));
                        return Some(counted(walked, n));
                    }
                    _ => continue,
                }
            }
        }
        // Time by time, as a loop whose compares tell nothing that moves
        // by the same amount, such as one over a table that ends at a row
        // of zeros.
        for n in 4..=MOST_TIMES {
            let pass = self.pass(frame, head, start)?;
            let again = pass.again;
            start = pass.path.clone();
            walked.push((n, pass));
            if !again {
                return Some(counted(walked, n));
            }
        }
        None
    }

    /// Where the innermost loop that the walk goes through time by time
    /// leaves, as the first time through its code, `first`, tells.
    pub(super) fn learn_code(&mut self, first: &Pass) {
        if let Some(counting) = self.counting.last_mut() {
            counting.code = first.order.iter().copied().collect();
            counting.ways_out = first.ways_out.clone();
        }
    }

    /// Goes through a loop's code once from `head`, from `path`, as one
    /// time the loop runs, where it stops at the branch back or where the
    /// loop leaves (see [`Counting`]), for the innermost loop that the
    /// walk goes through time by time: `None` when it goes past a branch
    /// that the walk can't tell, or the walk can't tell whether the loop
    /// goes back after it. To count the loop, it doesn't follow calls, and
    /// its events and instructions don't count for the walk; otherwise,
    /// they are the walk's.
    pub(super) fn pass(&mut self, frame: Frame, head: u64, path: Path) -> Option<Pass> {
        let events = self.events.len();
        let lowest = self.lowest;
        let index = self.counting.len().checked_sub(1)?;
        let walks = self.counting[index].walks;
        let body = Frame {
            depth: frame.depth,
            follow: if walks { frame.follow } else { 0 },
            limit: match walks {
                true => LOOP_LIMIT,
                false => LOOP_LIMIT.min(self.count_budget),
            },
            stop: None,
            nesting: frame.nesting + 1,
            pass: Some(index),
            rows: walks.then_some(index),
        };
        if walks {
            self.counting[index].carried = path.sources;
        } else {
            self.budget = usize::MAX;
        }
        let (end, run) = self.run(head, body, path);
        let counting = &mut self.counting[index];
        let stopped = counting.stopped.take();
        let loads = std::mem::take(&mut counting.loads);
        let (back, next) = (counting.back, counting.next);
        let mut loads: Vec<(u32, u32)> = (loads.into_iter())
            .map(|(_, at, pointer)| (at, pointer))
            .collect();
        loads.sort_unstable();
        loads.dedup();
        let mut stores = Vec::new();
        let mut others = false;
        if !walks {
            self.count_budget = self.count_budget.saturating_sub(run.count);
            self.lowest = lowest;
            for event in self.events.split_off(events) {
                match event {
                    Event::Store(store) if store.conditional => return None,
                    Event::Store(store) => stores.push(store),
                    _ => others = true,
                }
            }
        }
        let (Some((at, Some(again))), None, false) = (stopped, end, run.path.forks) else {
            return None;
        };
        let exit = if at == back { next } else { at };
        let mut path = run.path;
        let compares = std::mem::take(&mut path.compares);
        Some(Pass {
            path,
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

/// How far `a`, `b` and `c` move each time, when by the same amount: 0
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

/// The numbers that each compare of `second` and `third`, two times in a
/// row through a loop's code, compares, with what they are the first of
/// those times and how far they move each time, as numbers with no sign
/// or, with `signed`, with one, when the walk can tell them: those of the
/// compares that both times make in the same order.
fn compared(second: &Pass, third: &Pass, signed: bool) -> Vec<[(i64, i64); 2]> {
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

/// When a loop may leave, from the fourth time on, as the compares of the
/// second and third times through its code tell: around each time when
/// one of them may hold the other way, as the gap between its two
/// numbers closes, taken with no sign or with one. At most [`CANDIDATES`]
/// of them, the earliest first.
fn leaving_times(second: &Pass, third: &Pass) -> Vec<u32> {
    let mut times = BTreeSet::new();
    for signed in [false, true] {
        for [(a, a_by), (b, b_by)] in compared(second, third, signed) {
            let (gap, closes) = (a - b, b_by - a_by);
            // The gap closes after `gap / closes` times from the second.
            if closes == 0 || gap.signum() != closes.signum() {
                continue;
            }
            let Ok(after) = u32::try_from(gap / closes) else {
                continue;
            };
            let crossing = after.saturating_add(2);
            for n in crossing.saturating_sub(1)..=crossing.saturating_add(2) {
                if (4..=MOST_PASSES).contains(&n) {
                    times.insert(n);
                }
            }
        }
    }
    times.into_iter().take(CANDIDATES).collect()
}

/// Whether the numbers that each compare of `second` and `third`, the
/// second and third times through a loop's code, compares keep moving
/// without wrapping round up to the `last`th time, with no sign and with
/// one: then each compare's flags change only where the gap between its
/// numbers closes, where [`leaving_times`] looks.
fn keeps_order(second: &Pass, third: &Pass, last: u32) -> bool {
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

/// What a loop leaves after it runs `passes` times, with the times through
/// its code that the walk went through, `walked`, by number, the last one
/// last.
fn counted(walked: Vec<(u32, Pass)>, passes: u32) -> Counted {
    let stores = summaries(&walked, passes);
    let others = walked.iter().any(|(_, pass)| pass.others);
    let every = walked.len() == passes as usize;
    let (_, last) = walked.into_iter().next_back().expect("the last time");
    Counted {
        passes,
        path: last.path,
        exit: last.exit,
        stores,
        others,
        every,
    }
}

/// What the walk knows at the head of a loop the first time through its
/// code, as it was there, `before`, to go through the code time by time:
/// not where words in registers were loaded from, nor the flags.
pub(super) fn first_time(before: &Visit) -> Path {
    Path {
        registers: before.registers,
        memory: Rc::clone(&before.memory),
        flags: Flags::default(),
        forks: false,
        left: before.left,
        results: before.results,
        sources: [None; 16],
        compares: Vec::new(),
    }
}

/// What each store of a loop that runs `passes` times does over all the
/// times, from those that the walk went through, `walked`: when each time
/// but the last stores with the same instructions as the first, and the
/// last with the first of them, and the walk can tell where each stores,
/// which moves by the same amount each time.
fn summaries(walked: &[(u32, Pass)], passes: u32) -> Option<Vec<LoopStore>> {
    if walked.iter().any(|(_, pass)| pass.others) {
        return None;
    }
    let first = &walked.first()?.1.stores;
    let sites = |pass: &Pass| {
        pass.stores
            .iter()
            .map(|store| store.site)
            .collect::<Vec<_>>()
    };
    let all = sites(&walked[0].1);
    let same = walked.iter().all(|(n, pass)| match *n == passes {
        true => all.starts_with(&sites(pass)),
        false => sites(pass) == all,
    });
    if !same {
        return None;
    }
    (first.iter().enumerate())
        .map(|(i, store)| {
            let each: Vec<(u32, &Store)> = (walked.iter())
                .filter_map(|(n, pass)| Some((*n, pass.stores.get(i)?)))
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
                .map(|(_, pass)| pass.stores.len())
                .unwrap_or(0);
            Some(LoopStore {
                site: store.site,
                address,
                step,
                size: store.size,
                times: passes - 1 + u32::from(i < last),
                value,
                source,
                loaded,
            })
        })
        .collect()
}

/// Where `points`, the `n`th time's number for some `n`, the first first,
/// start and how far they move each time, when the walk can tell each and
/// they move by the same amount.
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

/// Forgets the words in memory that a loop that runs `passes` times may
/// have stored to on the times that the walk didn't go through, from the
/// fourth to the one before the one before the last: those that its
/// `stores` cover, or when the walk can't tell them, those that changed
/// since `before`, at its head.
pub(super) fn forget_stored(
    path: &mut Path,
    passes: u32,
    stores: Option<&[LoopStore]>,
    before: &Visit,
) {
    if passes <= 5 {
        return;
    }
    let Some(stores) = stores else {
        let registers = path.registers;
        forget_changes(path, &registers, &before.memory);
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
        let words: Vec<u32> = path
            .memory
            .range(low..=high)
            .map(|(&address, _)| address)
            .collect();
        if !words.is_empty() {
            let memory = Rc::make_mut(&mut path.memory);
            words
                .into_iter()
                .for_each(|address| _ = memory.insert(address, None));
        }
    }
}
