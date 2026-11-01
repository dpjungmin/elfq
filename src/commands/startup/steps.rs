//! The steps of startup ([`Step`]), which [`build`] reads from the walk's
//! events: each copy of a load image and each zeroing of a section without
//! contents, matched by the numbers that a call passes in r0 to r2, as
//! `memcpy(to, from, size)` and `memset(to, 0, size)` take them, or by the
//! bytes that a loop of the code's own, or stores outside loops, store
//! into the section (see `pieces.rs`); the calls that the reset handler
//! makes around them; and the code that elfq can't follow, which may set
//! up any section not set up yet (see `unread.rs`): a call through a
//! register, a call the walk doesn't go through, a loop it can't match,
//! and code the walk can't read on in.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::events::{Events, Iterations};
use super::pieces::{Home, Piece, Pieces};
use super::ram_sections::{Cover, Match, Region};
use super::start::{Main, MainFrom};
use super::unread::{loop_calls, loop_repeats, loop_targets, own_stores, unread_walked_loops};
use super::{Facts, Firmware};
use crate::arch::startup::{Call, End, Event, How, Loop};
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
///
/// What its `site`, `function` and `inside` are depends on its kind:
///
/// - A call, a call that copies or zeroes, and a call that elfq can't
///   follow: the call; the function it calls, past a veneer, when the walk
///   can tell; and the function whose code makes the call.
/// - A copy or zeroing by a call to a function that does one step (see
///   [`helpers`]): all three are the call's, but for such a function in
///   the code of the one that calls it, which keeps the function and the
///   way of its loop or call.
/// - A copy or zeroing by a loop or by stores: the first store, of the
///   loop's when there is one; and the function whose code holds it,
///   twice.
/// - A loop that elfq can't follow: its head; and the function whose code
///   holds it, twice.
/// - A function that a call runs, which goes where the walk can't follow
///   or on past its budget ([`Way::Outside`], or [`Way::Stop`] before the
///   walk's end): the call; the function; and the function whose code
///   makes the call. Its `level` is the function's, one deeper than the
///   call's.
/// - The walk's end, where it stops short ([`Way::Stop`]): where the walk
///   stopped; the function whose code holds that, or else the reset
///   handler; and the reset handler.
/// - `main`, which a compiler inlined ([`Way::Inlined`]): where its code
///   starts, as both `site` and `function`; and the function whose code
///   holds that, or else the reset handler.
pub(super) struct Step {
    /// Where its event is in the walk, which orders the steps; the number
    /// of events for a step at the walk's end.
    pub event: usize,
    /// Where the step begins in the walk's events: at `event`, or for a
    /// call to a function that does one step, which the step stands for,
    /// at the call's (see [`helpers`]).
    pub begins: usize,
    /// Its instruction.
    pub site: u64,
    pub does: Does,
    pub way: Way,
    /// The function it calls, or whose code holds it (see the list above).
    pub function: Option<u64>,
    /// The function whose code makes the call, or holds the step (see the
    /// list above).
    pub inside: u64,
    /// How many calls deep from the reset handler it is (see
    /// [`Events::level`]).
    pub level: u32,
    /// For a call, the veneer that it goes through.
    pub veneer: Option<u64>,
    /// Whether it is the call to `main`.
    pub main: bool,
    /// For a call that the reset handler makes, other than to `main`, that
    /// neither copies nor zeroes, where the events of the code it runs end,
    /// for [`helpers`] to read.
    pub span: Option<usize>,
    /// For a copy or zeroing, what it copies or zeroes.
    pub region: Option<Region>,
    /// For a copy or zeroing in an iteration of a loop over a table of
    /// sections, where the iteration reads its row of the table (see
    /// [`Iteration::row`](crate::arch::startup::Iteration::row)).
    pub table: Option<u64>,
    /// For code elfq can't follow, the sections it may set up.
    pub may: Vec<usize>,
}

impl Step {
    /// A step with what each kind has, as the list on [`Step`] says, and
    /// nothing else: the constructors below begin with it.
    fn new(
        event: usize,
        site: u64,
        does: Does,
        way: Way,
        function: Option<u64>,
        inside: u64,
        level: u32,
    ) -> Step {
        Step {
            event,
            begins: event,
            site,
            does,
            way,
            function,
            inside,
            level,
            veneer: None,
            main: false,
            span: None,
            region: None,
            table: None,
            may: Vec::new(),
        }
    }

    /// A step of code that elfq can't follow, which goes `way`, and has
    /// nothing else: for the unit tests of other files that read only
    /// what kind of code a step is.
    #[cfg(test)]
    pub(super) fn unread_for_tests(way: Way) -> Step {
        Step::new(0, 0, Does::Unread, way, None, 0, 0)
    }

    /// The step of `call`, at `event`, which goes `way`: a call, until
    /// [`Step::set_up`] makes it a copy or zeroing, or
    /// [`Step::make_unread`] code that elfq can't follow.
    pub fn call(events: &Events, event: usize, call: &Call, way: Way) -> Step {
        Step {
            veneer: events.veneer(event),
            ..Step::new(
                event,
                call.site,
                Does::Call,
                way,
                events.target(event),
                events.function(event),
                events.level(event),
            )
        }
    }

    /// The step at `event` that joined pieces of the code at `home` make
    /// (see `pieces.rs`): the copy or zeroing that `matched` says, by a
    /// loop or by stores, as `way` says, whose first store is `site`.
    pub fn of_pieces(event: usize, site: u64, way: Way, home: Home, matched: Match) -> Step {
        let Home {
            level, function, ..
        } = home;
        Step {
            region: Some(matched.region),
            ..Step::new(
                event,
                site,
                matched.does,
                way,
                Some(function),
                function,
                level,
            )
        }
    }

    /// The step of the loop `l`, at `loop_event`, when elfq can't follow
    /// it: code that may set up `may`.
    pub fn unread_loop(events: &Events, loop_event: usize, l: &Loop, may: Vec<usize>) -> Step {
        let function = events.function(loop_event);
        Step {
            may,
            ..Step::new(
                loop_event,
                l.head,
                Does::Unread,
                Way::Loop,
                Some(function),
                function,
                events.level(loop_event),
            )
        }
    }

    /// The step of the stop at `event`, where a function that the call at
    /// `site` runs goes where the walk can't follow, or on past its budget,
    /// as `way` says: code that may set up `may`.
    pub fn stop(events: &Events, event: usize, site: u64, way: Way, may: Vec<usize>) -> Step {
        Step {
            may,
            ..Step::new(
                event,
                site,
                Does::Unread,
                way,
                Some(events.function(event)),
                events.caller(event),
                events.level(event),
            )
        }
    }

    /// The step after the walk's `count` events, where it ends at `end`,
    /// stopping short, in the code of `function`: code that may set up
    /// `may`, in the reset handler, at `reset_handler`.
    fn stopped_short(
        count: usize,
        end: End,
        function: u64,
        reset_handler: u64,
        may: Vec<usize>,
    ) -> Step {
        Step {
            may,
            ..Step::new(
                count,
                end.site(),
                Does::Unread,
                Way::Stop(end),
                Some(function),
                reset_handler,
                0,
            )
        }
    }

    /// The step after the walk's `count` events of `main`, whose code a
    /// compiler inlined at `site`, into the code of `inside`.
    fn inlined_main(count: usize, site: u64, inside: u64) -> Step {
        Step {
            main: true,
            ..Step::new(count, site, Does::Call, Way::Inlined, Some(site), inside, 0)
        }
    }

    /// Makes the step the copy or zeroing that `matched` says.
    pub fn set_up(&mut self, matched: Match) {
        self.does = matched.does;
        self.region = Some(matched.region);
    }

    /// Makes the step code that elfq can't follow, which may set up `may`.
    pub fn make_unread(&mut self, may: Vec<usize>) {
        self.does = Does::Unread;
        self.region = None;
        self.may = may;
    }

    /// The sections that the step copies or zeroes, with the numbers it
    /// passes: none for a step of another kind.
    pub fn covers(&self) -> &[Cover] {
        self.region.as_ref().map_or(&[], |region| &region.covers)
    }

    /// The function that the step's instruction calls: a call's, or that
    /// of a call made the step that the function does (see [`helpers`]).
    pub(super) fn called(&self) -> Option<u64> {
        let merged = self.begins != self.event && matches!(self.way, Way::Call | Way::TailCall);
        self.function.filter(|_| self.does == Does::Call || merged)
    }
}

/// The number that the text and the JSON give the step at `index` in the
/// steps: they count from 1.
pub(super) fn number(index: usize) -> usize {
    index + 1
}

/// Reads the walk's events as the steps of startup, up to `main`.
pub(super) fn build(facts: &Facts) -> Vec<Step> {
    let Facts {
        firmware,
        ram,
        reset,
        main,
        events,
        end,
    } = *facts;
    let main_function = main.and_then(Main::function);
    let mut steps: Vec<Step> = Vec::new();
    let mut ready: HashSet<usize> = HashSet::new();
    let not_ready = |ready: &HashSet<usize>| -> Vec<usize> {
        (ram.sections.iter().copied())
            .filter(|section| !ready.contains(section))
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
    // loops that it didn't, by event.
    let mut called: Vec<Range<usize>> = Vec::new();
    let mut walked_loops: Vec<usize> = Vec::new();
    for (event, recorded) in events.list.iter().enumerate() {
        if event < skip_to {
            continue;
        }
        let level = events.level(event);
        match recorded {
            Event::Call(call) => {
                if events.call(event).is_none() {
                    continue;
                }
                let target = events.target(event);
                let way = match (call.how, target) {
                    (How::Jump, _) => Way::TailCall,
                    (How::Register(register), None) => Way::RegisterCall(register),
                    (How::Supervisor, None) => Way::SupervisorCall,
                    _ => Way::Call,
                };
                let mut step = Step::call(events, event, call, way);
                if target.is_some() && target == main_function {
                    step.main = true;
                    steps.push(step);
                } else if let Some(matched) = ram
                    .match_call(&call.passes())
                    // A copy of 0 bytes, as of an empty section that has
                    // no bytes in memory, where a section that an earlier
                    // step set up starts, doesn't set that one up.
                    .filter(|matched| {
                        let region = &matched.region;
                        region.size > 0
                            || !(region.covers.iter()).all(|cover| ready.contains(&cover.section))
                    })
                {
                    step.set_up(matched);
                    ready.extend(step.covers().iter().map(|cover| cover.section));
                    steps.push(step);
                    skip_to = events.span(event);
                    called.push(event..skip_to);
                } else if target.is_none() {
                    if matches!(call.how, How::Direct | How::Jump) {
                        continue;
                    }
                    step.make_unread(not_ready(&ready));
                    if !step.may.is_empty() {
                        steps.push(step);
                    }
                } else if !call.followed {
                    // Code the walk didn't go through.
                    step.way = if call.conditional {
                        Way::Conditional
                    } else {
                        Way::Deep
                    };
                    step.make_unread(not_ready(&ready));
                    if !step.may.is_empty() {
                        steps.push(step);
                    }
                } else if level == 0 {
                    step.span = Some(events.span(event));
                    steps.push(step);
                }
            }
            // Its iterations' events are steps of their own (see
            // [`unread_walked_loops`]). In a loop that the walk didn't go
            // through each iteration of, they are those of one iteration,
            // so it is a loop that elfq can't follow, as it would be if the
            // walk went through it once.
            Event::Loop(l) if !l.walked_iterations().is_empty() && !in_loop[event] => {
                walked_loops.push(event);
            }
            Event::Loop(l) => {
                let function = events.function(event);
                let home = Home {
                    level,
                    function,
                    iteration: iterations.of(event),
                };
                if let Some(piece) = Piece::of_loop(ram, l, home)
                    && pieces.add(ram, piece, event, &mut steps, &mut ready)
                {
                    continue;
                }
                let stores = own_stores(events, event, l);
                let mut may = loop_targets(ram, &stores, &ready);
                if may.is_empty() {
                    may = loop_repeats(ram, &steps, l, &ready);
                }
                if may.is_empty() {
                    may = loop_calls(ram, events, event, l, &ready);
                }
                if !may.is_empty() {
                    steps.push(Step::unread_loop(events, event, l, may));
                }
            }
            Event::Stop { site, end, .. } => {
                let function = events.function(event);
                // A call to code outside the file, or through a veneer
                // that jumps there.
                let way = match *end {
                    End::LeavesFile { target, .. } if target == function => Way::Outside(target),
                    end => Way::Stop(end),
                };
                let step = Step::stop(events, event, *site, way, not_ready(&ready));
                if !step.may.is_empty() {
                    steps.push(step);
                }
            }
            Event::Store(store) if !in_loop[event] => {
                let home = Home {
                    level,
                    function: events.function(event),
                    iteration: iterations.of(event),
                };
                match Piece::of_store(ram, store, home) {
                    Some(piece) => {
                        pieces.add(ram, piece, event, &mut steps, &mut ready);
                    }
                    None => pieces.unread_store(ram, store, home, &mut steps, &mut ready),
                }
            }
            Event::Store(_) | Event::Backtrack { .. } => {}
        }
    }
    let skipped = |event: usize| in_loop[event] || called.iter().any(|run| run.contains(&event));
    unread_walked_loops(ram, events, &walked_loops, skipped, &mut steps);
    ready = (steps.iter())
        .flat_map(|step| step.covers().iter().map(|cover| cover.section))
        .collect();
    let event = events.list.len();
    match end {
        // Where the walk can't read on, or stops short, what comes after
        // may set up anything.
        end if end.stops_short() => {
            let site = end.site();
            let function = firmware
                .item(site)
                .filter(|&index| firmware.binary.items[index].kind == ItemKind::Function)
                .map_or(reset.address, |index| firmware.binary.items[index].address);
            let may = not_ready(&ready);
            steps.push(Step::stopped_short(
                event,
                end,
                function,
                reset.address,
                may,
            ));
        }
        End::Main(site) if main.is_some_and(|main| main.from == MainFrom::DebugInfo) => {
            let inside = firmware
                .item(site)
                .map_or(reset.address, |index| firmware.binary.items[index].address);
            steps.push(Step::inlined_main(event, site, inside));
        }
        _ => {}
    }
    // Code elfq can't follow may set up only what no earlier step does.
    let mut first: HashMap<usize, usize> = HashMap::new();
    for step in &steps {
        for cover in step.covers() {
            first.entry(cover.section).or_insert(step.event);
        }
    }
    for step in &mut steps {
        if step.does == Does::Unread {
            let event = step.event;
            step.may
                .retain(|section| !first.get(section).is_some_and(|&covered| covered < event));
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
                    for section in step.may {
                        if !first.may.contains(&section) {
                            first.may.push(section);
                        }
                    }
                    first
                        .may
                        .sort_by_key(|&section| ram.section(section).address);
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
    kept
}

/// `steps`, with each call that the reset handler makes to a function
/// that does one step, a copy or a zeroing, as MCUXpresso's `data_init`
/// and `bss_init` do, made that step, as a call to `memcpy` is. Such a
/// call:
///
/// 1. is one that the reset handler makes, other than to `main`;
/// 2. runs code in which there is one step, the next, and that step
///    copies or zeroes, deeper than the call;
/// 3. and makes no call in that code but that step's own, if it is one.
///
/// The call goes, and the step takes its site, the function that makes
/// it and its level, and is by the function it calls, but:
///
/// 4. a step that a call to code in the function that makes it does, as
///    Pico SDK's `data_cpy` is, which isn't a function of its own, keeps
///    how it does it, as a loop in that function.
fn helpers(firmware: &Firmware, events: &Events, steps: Vec<Step>) -> Vec<Step> {
    // For each step, whether it is such a call, and so the step after
    // it, the one it does, goes.
    let merged: Vec<bool> = (0..steps.len())
        .map(|i| {
            let step = &steps[i];
            // 1.
            let (Does::Call, false, Some(span)) = (step.does, step.main, step.span) else {
                return false;
            };
            // 2.
            let inside = |index: usize| steps.get(index).is_some_and(|other| other.event < span);
            let Some(only) = steps.get(i + 1).filter(|_| inside(i + 1) && !inside(i + 2)) else {
                return false;
            };
            if !matches!(only.does, Does::Copy | Does::Zero) || only.level <= step.level {
                return false;
            }
            // 3. The events of the step's own call, when it is one.
            let own = match events.list[only.event] {
                Event::Call(_) => only.event..events.span(only.event),
                Event::Store(_) | Event::Loop(_) | Event::Backtrack { .. } | Event::Stop { .. } => {
                    0..0
                }
            };
            !(step.event + 1..span)
                .any(|event| !own.contains(&event) && events.call(event).is_some())
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
            // 4.
            Some(call) if local(&call) => out.push(Step {
                site: call.site,
                inside: call.inside,
                level: call.level,
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
        alone, binary, called, closed, copied, loop_with, steps, steps_to, walked_each,
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
                ..loop_with(vec![copied(0x2000_0000, 0x800_0400, 4, 4, 8)])
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
