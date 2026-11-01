//! The steps of startup: each copy of a load image and each zeroing of a
//! section without contents, matched by the numbers that a call passes in
//! r0 to r2, as `memcpy(to, from, size)` and `memset(to, 0, size)` take
//! them, or by the bytes that a loop of the code's own, or stores outside
//! loops, store into the section; the calls that the reset handler makes
//! around them; and the code that elfq can't follow, which may set up any
//! section not set up yet: a call through a register, a call the walk
//! doesn't go through, a loop it can't match, and code the walk can't read
//! on in.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::Firmware;
use super::events::{Events, Iterations};
use super::pieces::{Piece, Pieces};
use super::ram_sections::{Cover, Memory};
use super::start::{Main, MainFrom, Reset};
use super::unread::{loop_calls, loop_repeats, loop_targets, own_stores, unfollowed_iterations};
use crate::arch::startup::{End, Event, How};
use crate::binary::ItemKind;

/// What a step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Does {
    /// It copies load images.
    Copy,
    /// It zeroes sections without contents.
    Zero,
    /// A call that the reset handler makes, or `main`.
    Call,
    /// Code that elfq can't follow, which may set up any section not set
    /// up yet.
    Unread,
}

impl Does {
    pub fn word(self) -> &'static str {
        match self {
            Does::Copy => "copy",
            Does::Zero => "zero",
            Does::Call => "call",
            Does::Unread => "unread",
        }
    }
}

/// How a step does what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Way {
    Call,
    TailCall,
    /// `main`'s code, which a compiler inlined into the reset handler.
    Inlined,
    /// A loop of the code's own: one that copies or zeroes, or one that
    /// elfq can't follow.
    Loop,
    /// Stores outside loops.
    Store,
    /// A call through a register whose number the walk can't tell.
    RegisterCall(u16),
    /// A supervisor call, `svc`.
    SupervisorCall,
    /// A call or jump that runs only when the condition flags say, which
    /// the walk doesn't go through.
    Conditional,
    /// A call deeper than the walk follows, [`DEPTH`] calls deep.
    ///
    /// [`DEPTH`]: crate::arch::startup::DEPTH
    Deep,
    /// A call to code at this address, which isn't in the file, as in a
    /// boot ROM.
    Outside(u64),
    /// Code where the walk can't read on, or that it doesn't go through
    /// all of: how it ended.
    Stop(End),
}

impl Way {
    /// The JSON's `how`.
    pub fn field(self) -> &'static str {
        match self {
            Way::Call => "call",
            Way::TailCall => "tail call",
            Way::Inlined => "inlined",
            Way::Loop => "loop",
            Way::Store => "store",
            Way::RegisterCall(_) => "register call",
            Way::SupervisorCall => "supervisor call",
            Way::Conditional => "conditional call",
            Way::Deep => "deep call",
            Way::Outside(_) | Way::Stop(_) => "stop",
        }
    }
}

/// One step of startup.
pub(super) struct Step {
    /// Where its event is in the walk, which orders the steps; the number
    /// of events for a step at the walk's end.
    pub event: usize,
    /// Its number, from 1.
    pub number: usize,
    /// The instruction.
    pub site: u64,
    pub does: Does,
    pub way: Way,
    /// The function it calls, or that runs the loop or the code elfq
    /// can't follow.
    pub function: Option<u64>,
    /// The veneer that the call goes through.
    pub veneer: Option<u64>,
    /// The function that makes the call.
    pub inside: u64,
    /// How many calls deep from the reset handler it is (see
    /// [`Events::level`]).
    pub level: u32,
    /// Whether it is the call to `main`.
    pub main: bool,
    /// The sections it copies or zeroes, with the numbers it passes.
    pub covers: Vec<Cover>,
    pub to: Option<u64>,
    pub from: Option<u64>,
    pub size: Option<u64>,
    /// For code elfq can't follow, the sections it may set up.
    pub may: Vec<usize>,
    /// For a call, where the events of the code it runs end.
    pub span: Option<usize>,
    /// Where the step begins in the walk's events: at `event`, or for a
    /// call to a function that copies or zeroes, whose step stands for
    /// the call, at the call's (see [`helpers`]).
    pub begins: usize,
    /// For a copy or zeroing in an iteration of a loop over a table of
    /// sections, where the iteration reads its row of the table (see
    /// [`Iteration::row`](crate::arch::startup::Iteration::row)).
    pub table: Option<u64>,
}

impl Step {
    pub(super) fn new(
        event: usize,
        site: u64,
        does: Does,
        way: Way,
        inside: u64,
        level: u32,
    ) -> Step {
        Step {
            event,
            number: 0,
            site,
            does,
            way,
            function: None,
            veneer: None,
            inside,
            level,
            main: false,
            covers: Vec::new(),
            to: None,
            from: None,
            size: None,
            may: Vec::new(),
            span: None,
            begins: event,
            table: None,
        }
    }

    /// The function that the step's instruction calls: a call's, or that
    /// of a call made the step that the function does (see [`helpers`]).
    pub(super) fn called(&self) -> Option<u64> {
        let merged = self.begins != self.event && matches!(self.way, Way::Call | Way::TailCall);
        self.function.filter(|_| self.does == Does::Call || merged)
    }
}

/// Reads the walk's events as the steps of startup, up to `main`.
pub(super) fn build(
    firmware: &Firmware,
    memory: &Memory,
    events: &Events,
    reset: &Reset,
    main: Option<Main>,
    end: End,
) -> Vec<Step> {
    let main_function = main.and_then(Main::function);
    let mut steps: Vec<Step> = Vec::new();
    // The function whose code runs at each level.
    let mut holder: Vec<u64> = vec![reset.address];
    let at = |holder: &Vec<u64>, level: usize| holder.get(level).copied().unwrap_or(reset.address);
    let mut ready: HashSet<usize> = HashSet::new();
    let not_ready = |ready: &HashSet<usize>| -> Vec<usize> {
        (memory.ram.iter().copied())
            .filter(|x| !ready.contains(x))
            .collect()
    };
    // Inside a call that copies or zeroes: its own code.
    let mut skip_to = 0;
    // The events of loops' code, whose stores the loops' steps stand for,
    // but for loops that the walk went through each time, whose events
    // are those of every time.
    let mut in_loop = vec![false; events.list.len()];
    for event in events.list {
        if let Event::Loop(l) = event
            && l.walked_iterations().is_empty()
        {
            in_loop[l.events.clone()].fill(true);
        }
    }
    let iterations = Iterations::new(events.list);
    let mut pieces = Pieces::default();
    // The events of calls that copy or zero, whose steps stand for their
    // code, and each loop that the walk went through each time, outside
    // loops that it didn't, with the function that runs it.
    let mut called: Vec<Range<usize>> = Vec::new();
    let mut walked_loops: Vec<(usize, u64)> = Vec::new();
    for (k, event) in events.list.iter().enumerate() {
        if k < skip_to {
            continue;
        }
        let level = events.level(k);
        let l = level as usize;
        match event {
            Event::Call(call) => {
                if events.call(k).is_none() {
                    continue;
                }
                let inside = at(&holder, l);
                let target = events.target(k);
                if let Some(target) = target {
                    if holder.len() <= l + 1 {
                        holder.resize(l + 2, reset.address);
                    }
                    holder[l + 1] = target;
                }
                let way = match (call.how, target) {
                    (How::Jump, _) => Way::TailCall,
                    (How::Register(register), None) => Way::RegisterCall(register),
                    (How::Supervisor, None) => Way::SupervisorCall,
                    _ => Way::Call,
                };
                let mut step = Step::new(k, call.site, Does::Call, way, inside, level);
                step.function = target;
                step.veneer = events.veneer(k);
                if target.is_some() && target == main_function {
                    step.main = true;
                    steps.push(step);
                } else if let Some(matched) = memory
                    .match_call(&call.passes())
                    // A copy of 0 bytes, as of an empty section that has
                    // no bytes in memory, where a section that an earlier
                    // step set up starts, doesn't set that one up.
                    .filter(|matched| {
                        matched.size > 0
                            || !(matched.covers.iter()).all(|cover| ready.contains(&cover.section))
                    })
                {
                    step.does = matched.does;
                    step.to = Some(matched.to);
                    step.from = matched.from;
                    step.size = Some(matched.size);
                    ready.extend(matched.covers.iter().map(|cover| cover.section));
                    step.covers = matched.covers;
                    steps.push(step);
                    skip_to = events.span(k);
                    called.push(k..skip_to);
                } else if target.is_none() {
                    if matches!(call.how, How::Direct | How::Jump) {
                        continue;
                    }
                    step.does = Does::Unread;
                    step.may = not_ready(&ready);
                    if !step.may.is_empty() {
                        steps.push(step);
                    }
                } else if !call.followed {
                    // Code the walk didn't go through.
                    step.does = Does::Unread;
                    step.way = if call.conditional {
                        Way::Conditional
                    } else {
                        Way::Deep
                    };
                    step.may = not_ready(&ready);
                    if !step.may.is_empty() {
                        steps.push(step);
                    }
                } else if level == 0 {
                    step.span = Some(events.span(k));
                    steps.push(step);
                }
            }
            // Its iterations' events are steps of their own (see
            // [`unfollowed_iterations`]). In a loop that the walk didn't go
            // through each iteration of, they are those of one iteration,
            // so it is a loop that elfq can't follow, as it would be if the
            // walk went through it once.
            Event::Loop(walked) if !walked.walked_iterations().is_empty() && !in_loop[k] => {
                walked_loops.push((k, at(&holder, l)));
            }
            Event::Loop(walked) => {
                let function = at(&holder, l);
                let home = (level, function, iterations.home(k));
                if let Some(piece) = Piece::of_loop(memory, walked, home)
                    && pieces.add(memory, piece, k, &mut steps, &mut ready)
                {
                    continue;
                }
                let stores = own_stores(events, k, walked);
                let mut may = loop_targets(memory, &stores, &ready);
                if may.is_empty() {
                    may = loop_repeats(memory, &steps, walked, &ready);
                }
                if may.is_empty() {
                    may = loop_calls(memory, events, k, walked, &ready);
                }
                if !may.is_empty() {
                    let mut step =
                        Step::new(k, walked.head, Does::Unread, Way::Loop, function, level);
                    step.function = Some(function);
                    step.may = may;
                    steps.push(step);
                }
            }
            Event::Stop { site, end, .. } => {
                let inside = if l > 0 {
                    at(&holder, l - 1)
                } else {
                    reset.address
                };
                // A call to code outside the file, or through a veneer
                // that jumps there.
                let way = match *end {
                    End::LeavesFile { target, .. } if target == at(&holder, l) => {
                        Way::Outside(target)
                    }
                    end => Way::Stop(end),
                };
                let mut step = Step::new(k, *site, Does::Unread, way, inside, level);
                step.function = Some(at(&holder, l));
                step.may = not_ready(&ready);
                if !step.may.is_empty() {
                    steps.push(step);
                }
            }
            Event::Store(store) if !in_loop[k] => {
                let home = (level, at(&holder, l), iterations.home(k));
                match Piece::of_store(memory, store, home) {
                    Some(piece) => {
                        pieces.add(memory, piece, k, &mut steps, &mut ready);
                    }
                    None => pieces.unsure(memory, store, home, &mut steps, &mut ready),
                }
            }
            Event::Store(_) | Event::Backtrack { .. } => {}
        }
    }
    let skipped = |j: usize| in_loop[j] || called.iter().any(|run| run.contains(&j));
    unfollowed_iterations(memory, events, &walked_loops, skipped, &mut steps);
    ready = (steps.iter())
        .flat_map(|step| step.covers.iter().map(|cover| cover.section))
        .collect();
    let k = events.list.len();
    match end {
        // Where the walk can't read on, or stops short, what comes after
        // may set up anything.
        end if end.stops_short() => {
            let site = end.site();
            let function = firmware
                .item(site)
                .filter(|&index| firmware.binary.items[index].kind == ItemKind::Function)
                .map_or(reset.address, |index| firmware.binary.items[index].address);
            let mut step = Step::new(k, site, Does::Unread, Way::Stop(end), reset.address, 0);
            step.function = Some(function);
            step.may = not_ready(&ready);
            steps.push(step);
        }
        End::Main(site) if main.is_some_and(|main| main.from == MainFrom::DebugInfo) => {
            let inside = firmware
                .item(site)
                .map_or(reset.address, |index| firmware.binary.items[index].address);
            let mut step = Step::new(k, site, Does::Call, Way::Inlined, inside, 0);
            step.function = Some(site);
            step.main = true;
            steps.push(step);
        }
        _ => {}
    }
    // Code elfq can't follow may set up only what no earlier step does.
    let mut first: HashMap<usize, usize> = HashMap::new();
    for step in &steps {
        for cover in &step.covers {
            first.entry(cover.section).or_insert(step.event);
        }
    }
    for step in &mut steps {
        if step.does == Does::Unread {
            let event = step.event;
            step.may
                .retain(|x| !first.get(x).is_some_and(|&k| k < event));
        }
    }
    steps.retain(|step| step.does != Does::Unread || !step.may.is_empty());
    for step in &mut steps {
        if matches!(step.does, Does::Copy | Does::Zero) {
            step.table = iterations.row(step.event).map(u64::from);
        }
    }
    let steps = helpers(firmware, events, steps);
    // A call that a loop over a table makes each time is listed once, but
    // for the times that it copies or zeroes, and not at all when it does
    // in some, as for a row that copies none; and code that the walk can't
    // follow in each time, once, with what it may set up any time.
    let does: HashSet<u64> = (steps.iter())
        .filter(|step| matches!(step.does, Does::Copy | Does::Zero))
        .map(|step| step.site)
        .collect();
    let mut sites: HashSet<u64> = HashSet::new();
    let mut kept: Vec<Step> = Vec::with_capacity(steps.len());
    // The steps of `kept` that elfq can't follow, each the first of its
    // kind at its site.
    let mut unread: Vec<usize> = Vec::new();
    for step in steps {
        let again = !sites.insert(step.site);
        let in_iteration = iterations.of(step.begins).is_some();
        match step.does {
            Does::Call if (again || does.contains(&step.site)) && !step.main && in_iteration => {
                continue;
            }
            Does::Unread => {
                let key = (step.site, step.way, step.function);
                let seen = (unread.iter().copied())
                    .find(|&i| (kept[i].site, kept[i].way, kept[i].function) == key);
                if let Some(first) = seen
                    && in_iteration
                {
                    let first = &mut kept[first];
                    for x in step.may {
                        if !first.may.contains(&x) {
                            first.may.push(x);
                        }
                    }
                    first.may.sort_by_key(|&x| memory.section(x).address);
                    continue;
                }
                if seen.is_none() {
                    unread.push(kept.len());
                }
            }
            _ => {}
        }
        kept.push(step);
    }
    let mut steps = kept;
    for (n, step) in steps.iter_mut().enumerate() {
        step.number = n + 1;
    }
    steps
}

/// `steps`, with each call that the reset handler makes to a function
/// that does one step, a copy or a zeroing, and calls nothing else, as
/// MCUXpresso's `data_init` and `bss_init` do, made that step, as a call
/// to `memcpy` is: the call's, by the function, at its site. A call to
/// code in the function that makes it, as Pico SDK's `data_cpy` is, which
/// isn't a function of its own, keeps how the step does it, as a loop in
/// that function.
fn helpers(firmware: &Firmware, events: &Events, steps: Vec<Step>) -> Vec<Step> {
    // For each step, whether it is such a call, and so the step after
    // it, the one it does, goes.
    let merged: Vec<bool> = (0..steps.len())
        .map(|i| {
            let step = &steps[i];
            let (Does::Call, false, Some(span)) = (step.does, step.main, step.span) else {
                return false;
            };
            let inside = |j: usize| steps.get(j).is_some_and(|other| other.event < span);
            let Some(only) = steps.get(i + 1).filter(|_| inside(i + 1) && !inside(i + 2)) else {
                return false;
            };
            // The events of the step's own call, when it is one.
            let own = match events.list[only.event] {
                Event::Call(_) => only.event..events.span(only.event),
                _ => 0..0,
            };
            let calls_else =
                (step.event + 1..span).any(|j| !own.contains(&j) && events.call(j).is_some());
            matches!(only.does, Does::Copy | Does::Zero) && only.level > step.level && !calls_else
        })
        .collect();
    let local = |call: &Step| {
        let item = firmware.item(call.inside);
        item.is_some() && call.function.map(|function| firmware.item(function)) == Some(item)
    };
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    let mut call: Option<Step> = None;
    for (step, merged) in steps.into_iter().zip(merged) {
        if merged {
            call = Some(step);
            continue;
        }
        match call.take() {
            Some(call) if local(&call) => out.push(Step {
                site: call.site,
                inside: call.inside,
                level: call.level,
                span: call.span,
                begins: call.event,
                ..step
            }),
            Some(call) => out.push(Step {
                site: call.site,
                way: call.way,
                function: call.function,
                veneer: call.veneer,
                inside: call.inside,
                level: call.level,
                span: call.span,
                begins: call.event,
                ..step
            }),
            None => out.push(step),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        alone, binary, called, closed, copied, steps, steps_to, walked, walked_each,
    };
    use super::*;
    use crate::arch::startup::Loop;

    #[test]
    fn a_loop_that_elfq_cant_follow_in_each_time_may_set_up_what_it_may_any_time() {
        let binary = binary();
        // The first time stores past a pointer to .noinit in a loop, the
        // second past one to .bss.
        let row = |at: usize, base: u32| {
            [
                alone(0x800_0030, None, base, None, None),
                closed(Vec::new(), at..at + 1),
            ]
        };
        let mut list: Vec<Event> = (row(0, 0x2000_0070).into_iter())
            .chain(row(2, 0x2000_0030))
            .collect();
        list.push(walked_each(&[0..2, 2..4]));
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
    }

    #[test]
    fn a_call_in_a_loop_over_a_table_that_sets_up_nothing_is_no_step() {
        let binary = binary();
        // A first row that copies no bytes, then one that copies .data, by
        // a function that copies.
        let list = [
            called(0x800_0010, [0x2000_0080, 0x800_0480, 0]),
            called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20]),
            walked_each(&[0..1, 1..2]),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a call: .data 32"]
        );
        // As data_init(load image, start, size) does, with a loop.
        let list = [
            called(0x800_0010, [0x800_0480, 0x2000_0080, 0]),
            called(0x800_0010, [0x800_0400, 0x2000_0000, 0x20]),
            Event::Loop(Loop {
                depth: 1,
                events: 2..2,
                ..walked(vec![copied(0x2000_0000, 0x800_0400, 4, 4, 8)])
            }),
            walked_each(&[0..1, 1..3]),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a call: .data 32"]
        );
    }

    #[test]
    fn a_walk_that_runs_back_into_its_code_stops_at_a_loop_it_cant_follow() {
        let binary = binary();
        // As GCC's -Og makes of CMSIS's __cmsis_start: after the copy of
        // .data, code that the walk can't make out a loop of may set up the
        // rest.
        let list = [called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20])];
        assert_eq!(
            steps_to(&binary, &list, End::Rejoins(0x800_0020)),
            [
                "copy 0x20000000 32 by a call: .data 32",
                "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit"
            ]
        );
    }

    #[test]
    fn a_copy_of_0_bytes_where_a_section_set_up_before_starts_is_only_a_call() {
        let binary = binary();
        // As of an empty section, which has no bytes in memory, where
        // .data starts, after the copy of .data.
        let list = [
            called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20]),
            called(0x800_0014, [0x2000_0000, 0x800_0420, 0]),
        ];
        assert_eq!(
            steps(&binary, &list),
            [
                "copy 0x20000000 32 by a call: .data 32",
                "call 0x0 0 by a call: "
            ]
        );
        // Before it, the copy names .data.
        let list = [
            called(0x800_0010, [0x2000_0000, 0x800_0400, 0]),
            called(0x800_0014, [0x2000_0000, 0x800_0400, 0x20]),
        ];
        assert_eq!(
            steps(&binary, &list),
            [
                "copy 0x20000000 0 by a call: .data 0",
                "copy 0x20000000 32 by a call: .data 32"
            ]
        );
    }
}
