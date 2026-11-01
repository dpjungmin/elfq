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
//!
//! [`build`] reads them in passes, a function each: [`from_events`] reads
//! the events in order, with a [`StepReader`]; [`add_end`] adds the step
//! at the walk's end; [`narrow_unread_may`] narrows what code elfq can't
//! follow may set up to what no earlier step does; [`mark_table_rows`]
//! marks where a copy or zeroing reads its row of a table;
//! [`merge_one_step_calls`] makes each call to a function that does one
//! step that step; and [`keep_once_per_table_loop`] lists what a loop
//! over a table does in each iteration once.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::events::{Events, Iterations};
use super::pieces::{Home, Piece, Pieces};
use super::ram_sections::{Cover, Match, RamSections, Region};
use super::start::{Main, MainFrom};
use super::unread::{loop_calls, loop_repeats, loop_targets, own_stores, unread_walked_loops};
use super::{Facts, Firmware, function_holding};
use crate::arch::startup::{Call, End, Event, How, Loop, Store};

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

    /// The register that a call through a register goes through.
    pub fn register(self) -> Option<u16> {
        match self {
            Way::RegisterCall(register) => Some(register),
            Way::Call
            | Way::TailCall
            | Way::Inlined
            | Way::Loop
            | Way::Store
            | Way::SupervisorCall
            | Way::Conditional
            | Way::Deep
            | Way::Outside(_)
            | Way::Stop(_) => None,
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
///   [`merge_one_step_calls`]): all three are the call's, but for such a
///   function in the code of the one that calls it, which keeps the
///   function and the way of its loop or call.
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
    /// at the call's (see [`merge_one_step_calls`]).
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
    /// for [`merge_one_step_calls`] to read.
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
    /// of a call made the step that the function does (see
    /// [`merge_one_step_calls`]).
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
    let iterations = Iterations::new(facts.events.list);
    let mut steps = from_events(facts, &iterations);
    add_end(facts, &mut steps);
    narrow_unread_may(&mut steps);
    mark_table_rows(&iterations, &mut steps);
    let steps = merge_one_step_calls(facts, steps);
    keep_once_per_table_loop(facts.ram, &iterations, steps)
}

/// The steps that the walk's events make, in their order (see
/// [`StepReader`]), with each loop that the walk went through one
/// iteration at a time made code elfq can't follow when its iterations'
/// steps don't stand for all it stores (see [`unread_walked_loops`]).
fn from_events(facts: &Facts, iterations: &Iterations) -> Vec<Step> {
    let mut reader = StepReader::new(facts, iterations);
    for (event, recorded) in facts.events.list.iter().enumerate() {
        reader.read(event, recorded);
    }
    reader.finish()
}

/// What [`from_events`] keeps as it reads the walk's events one by one.
struct StepReader<'r> {
    facts: &'r Facts<'r>,
    iterations: &'r Iterations,
    /// The steps read so far.
    steps: Vec<Step>,
    /// The pieces read so far, which may join those after them.
    pieces: Pieces,
    /// Inside a call that copies or zeroes, where the events of its code
    /// end: its step stands for them.
    skip_to: usize,
    /// For each event, whether it is in the code of a loop, whose stores
    /// the loop's step stands for; but not in that of a loop that the walk
    /// went through one iteration at a time, whose events are those of
    /// every iteration.
    in_loop: Vec<bool>,
    /// The events of the code of each call that copies or zeroes, which
    /// its step stands for.
    skipped_calls: Vec<Range<usize>>,
    /// The loops that the walk went through one iteration at a time,
    /// outside loops that it didn't, by event.
    walked_loops: Vec<usize>,
}

impl<'r> StepReader<'r> {
    fn new(facts: &'r Facts<'r>, iterations: &'r Iterations) -> StepReader<'r> {
        let list = facts.events.list;
        let mut in_loop = vec![false; list.len()];
        for recorded in list {
            if let Event::Loop(l) = recorded
                && l.walked_iterations().is_empty()
            {
                in_loop[l.events.clone()].fill(true);
            }
        }
        StepReader {
            facts,
            iterations,
            steps: Vec::new(),
            pieces: Pieces::default(),
            skip_to: 0,
            in_loop,
            skipped_calls: Vec::new(),
            walked_loops: Vec::new(),
        }
    }

    /// Reads `recorded`, the event at `event`.
    fn read(&mut self, event: usize, recorded: &Event) {
        if event < self.skip_to {
            return;
        }
        match recorded {
            Event::Call(call) => self.on_call(event, call),
            // Its iterations' events are steps of their own (see
            // [`unread_walked_loops`]). In a loop that the walk didn't go
            // through each iteration of, they are those of one iteration,
            // so it is a loop that elfq can't follow, as it would be if the
            // walk went through it once.
            Event::Loop(l) if !l.walked_iterations().is_empty() && !self.in_loop[event] => {
                self.walked_loops.push(event);
            }
            Event::Loop(l) => self.on_loop(event, l),
            Event::Stop { site, end, .. } => self.on_stop(event, *site, *end),
            Event::Store(store) if !self.in_loop[event] => self.on_store(event, store),
            Event::Store(_) | Event::Backtrack { .. } => {}
        }
    }

    /// Reads `call`, at `event`, unless it is the jump of a veneer, which
    /// the call to the veneer stands for. It is:
    ///
    /// 1. the call to `main`, when it goes there;
    /// 2. else a copy or zeroing, when it passes the numbers that one
    ///    would (see [`RamSections::match_call`]), but for a copy of 0
    ///    bytes, as of an empty section that has no bytes in memory, where
    ///    a section that an earlier step sets up starts, which doesn't set
    ///    that one up; its step stands for the events of its code;
    /// 3. else, when the walk can't tell where it goes, code elfq can't
    ///    follow, for a call through a register or a supervisor call, and
    ///    no step for a call or jump to an address in its instruction;
    /// 4. else, when the walk didn't go through it, code elfq can't follow:
    ///    a conditional call, or one deeper than the walk follows;
    /// 5. else a call, when the reset handler makes it, and no step when
    ///    the code of a call makes it.
    fn on_call(&mut self, event: usize, call: &Call) {
        let Facts {
            ram, main, events, ..
        } = *self.facts;
        if events.call(event).is_none() {
            return;
        }
        let target = events.target(event);
        let way = match (call.how, target) {
            (How::Jump, _) => Way::TailCall,
            (How::Register(register), None) => Way::RegisterCall(register),
            (How::Supervisor, None) => Way::SupervisorCall,
            (How::Direct, _) | (How::Register(_) | How::Supervisor, Some(_)) => Way::Call,
        };
        let mut step = Step::call(events, event, call, way);
        // 1.
        if target.is_some() && target == main.and_then(Main::function) {
            step.main = true;
            self.steps.push(step);
            return;
        }
        // 2.
        let matched = ram.match_call(&call.passes()).filter(|matched| {
            let Region { size, covers, .. } = &matched.region;
            let covered = || sections_covered(&self.steps);
            *size > 0 || !(covers.iter()).all(|cover| covered().contains(&cover.section))
        });
        if let Some(matched) = matched {
            step.set_up(matched);
            self.steps.push(step);
            self.skip_to = events.span(event);
            self.skipped_calls.push(event..self.skip_to);
            return;
        }
        // 3.
        if target.is_none() {
            if matches!(call.how, How::Register(_) | How::Supervisor) {
                step.make_unread(not_set_up(ram, &self.steps));
                self.push_unread(step);
            }
            return;
        }
        // 4.
        if !call.followed {
            step.way = if call.conditional {
                Way::Conditional
            } else {
                Way::Deep
            };
            step.make_unread(not_set_up(ram, &self.steps));
            self.push_unread(step);
            return;
        }
        // 5.
        if events.level(event) == 0 {
            step.span = Some(events.span(event));
            self.steps.push(step);
        }
    }

    /// Reads the loop `l`, at `event`, which the walk didn't go through
    /// one iteration at a time, or which is inside a loop that it didn't
    /// (see [`StepReader::read`]): a copy or zeroing, when its piece makes
    /// a step or extends one (see [`Pieces::add`]), or else code elfq
    /// can't follow, when it may set up a section (see `unread.rs`).
    fn on_loop(&mut self, event: usize, l: &Loop) {
        let Facts { ram, events, .. } = *self.facts;
        if let Some(piece) = Piece::of_loop(ram, l, self.home(event))
            && self.pieces.add(ram, piece, event, &mut self.steps)
        {
            return;
        }
        let covered = sections_covered(&self.steps);
        let stores = own_stores(events, event, l);
        let mut may = loop_targets(ram, &stores, &covered);
        if may.is_empty() {
            may = loop_repeats(ram, &self.steps, l, &covered);
        }
        if may.is_empty() {
            may = loop_calls(ram, events, event, l, &covered);
        }
        self.push_unread(Step::unread_loop(events, event, l, may));
    }

    /// Reads the stop at `event`, where a function that the call at `site`
    /// runs goes where the walk can't follow, or on past its budget, as
    /// `end` says: code elfq can't follow, which may set up any section
    /// not set up yet.
    fn on_stop(&mut self, event: usize, site: u64, end: End) {
        let Facts { ram, events, .. } = *self.facts;
        // A call to code outside the file, or through a veneer that jumps
        // there.
        let way = match end {
            End::LeavesFile { target, .. } if target == events.function(event) => {
                Way::Outside(target)
            }
            end => Way::Stop(end),
        };
        let may = not_set_up(ram, &self.steps);
        self.push_unread(Step::stop(events, event, site, way, may));
    }

    /// Reads `store`, at `event`, outside loops: a piece, when it copies
    /// or zeroes (see [`Piece::of_store`]), or else a store that may make a
    /// step of the same code one of code elfq can't follow (see
    /// [`Pieces::unread_store`]).
    fn on_store(&mut self, event: usize, store: &Store) {
        let ram = self.facts.ram;
        let home = self.home(event);
        match Piece::of_store(ram, store, home) {
            Some(piece) => {
                self.pieces.add(ram, piece, event, &mut self.steps);
            }
            None => self.pieces.unread_store(ram, store, home, &mut self.steps),
        }
    }

    /// Where the code of `event` is.
    fn home(&self, event: usize) -> Home {
        let events = self.facts.events;
        Home {
            level: events.level(event),
            function: events.function(event),
            iteration: self.iterations.of(event),
        }
    }

    /// Adds `step`, of code elfq can't follow, unless it may set up nothing.
    fn push_unread(&mut self, step: Step) {
        if !step.may.is_empty() {
            self.steps.push(step);
        }
    }

    /// The steps read, with each loop that the walk went through one
    /// iteration at a time made code elfq can't follow when its
    /// iterations' steps don't stand for all it stores.
    fn finish(self) -> Vec<Step> {
        let StepReader {
            facts,
            mut steps,
            in_loop,
            skipped_calls,
            walked_loops,
            ..
        } = self;
        let skipped = |event: usize| {
            in_loop[event] || (skipped_calls.iter()).any(|code| code.contains(&event))
        };
        unread_walked_loops(facts.ram, facts.events, &walked_loops, skipped, &mut steps);
        steps
    }
}

/// The sections that `steps` copy or zero.
fn sections_covered(steps: &[Step]) -> HashSet<usize> {
    (steps.iter())
        .flat_map(|step| step.covers().iter().map(|cover| cover.section))
        .collect()
}

/// The sections that no step of `steps` copies or zeroes, by address: what
/// code elfq can't follow after them may set up.
fn not_set_up(ram: &RamSections, steps: &[Step]) -> Vec<usize> {
    let covered = sections_covered(steps);
    (ram.sections.iter().copied())
        .filter(|section| !covered.contains(section))
        .collect()
}

/// Adds the step at the walk's end: where the walk can't read on, or
/// stops short, code elfq can't follow, since what comes after may set
/// up any section not set up yet; or `main`, when a compiler inlined it
/// into the reset handler, as debug info says.
fn add_end(facts: &Facts, steps: &mut Vec<Step>) {
    let Facts {
        firmware,
        ram,
        reset,
        main,
        events,
        end,
    } = *facts;
    let count = events.list.len();
    if end.stops_short() {
        let function = function_holding(firmware.binary, end.site()).unwrap_or(reset.address);
        let may = not_set_up(ram, steps);
        steps.push(Step::stopped_short(
            count,
            end,
            function,
            reset.address,
            may,
        ));
    } else if let End::Main(site) = end
        && main.is_some_and(|main| main.from == MainFrom::DebugInfo)
    {
        // Debug info puts main's code in the function that it was inlined
        // into, so the item at the site is taken as that function, whatever
        // its kind.
        let inside = firmware
            .item(site)
            .map_or(reset.address, |index| firmware.binary.items[index].address);
        steps.push(Step::inlined_main(count, site, inside));
    }
}

/// Narrows what each step of code elfq can't follow may set up to what no
/// earlier step copies or zeroes, and drops those left with nothing.
fn narrow_unread_may(steps: &mut Vec<Step>) {
    // The event of the first step that copies or zeroes each section.
    let mut first: HashMap<usize, usize> = HashMap::new();
    for step in steps.iter() {
        for cover in step.covers() {
            first.entry(cover.section).or_insert(step.event);
        }
    }
    for step in steps.iter_mut() {
        if step.does == Does::Unread {
            let event = step.event;
            step.may
                .retain(|section| !first.get(section).is_some_and(|&covered| covered < event));
        }
    }
    steps.retain(|step| step.does != Does::Unread || !step.may.is_empty());
}

/// Marks each copy or zeroing in an iteration of a loop over a table of
/// sections with where the iteration reads its row (see [`Step::table`]).
fn mark_table_rows(iterations: &Iterations, steps: &mut [Step]) {
    for step in steps {
        if matches!(step.does, Does::Copy | Does::Zero) {
            step.table = iterations.row(step.event).map(u64::from);
        }
    }
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
fn merge_one_step_calls(facts: &Facts, steps: Vec<Step>) -> Vec<Step> {
    // The calls that go, by index: the step after each takes its place.
    let calls: Vec<usize> = (0..steps.len())
        .filter(|&index| does_one_step(facts.events, &steps, index))
        .collect();
    let mut merged: Vec<Step> = Vec::with_capacity(steps.len());
    let mut steps = steps.into_iter().enumerate();
    while let Some((index, step)) = steps.next() {
        if calls.binary_search(&index).is_ok()
            && let Some((_, done)) = steps.next()
        {
            merged.push(done_by_call(facts.firmware, step, done));
        } else {
            merged.push(step);
        }
    }
    merged
}

/// Whether the step at `index` in `steps` is a call to a function that
/// does one step, the next, by rules 1 to 3 of [`merge_one_step_calls`].
fn does_one_step(events: &Events, steps: &[Step], index: usize) -> bool {
    let call = &steps[index];
    // 1.
    let (Does::Call, false, Some(span)) = (call.does, call.main, call.span) else {
        return false;
    };
    // 2.
    let inside = |index: usize| steps.get(index).is_some_and(|step| step.event < span);
    let Some(only) = steps
        .get(index + 1)
        .filter(|_| inside(index + 1) && !inside(index + 2))
    else {
        return false;
    };
    if !matches!(only.does, Does::Copy | Does::Zero) || only.level <= call.level {
        return false;
    }
    // 3. The events of the step's own call, when it is one.
    let own = match events.list[only.event] {
        Event::Call(_) => only.event..events.span(only.event),
        Event::Store(_) | Event::Loop(_) | Event::Backtrack { .. } | Event::Stop { .. } => 0..0,
    };
    !(call.event + 1..span).any(|event| !own.contains(&event) && events.call(event).is_some())
}

/// `done`, the step of a function that does one step, as `call`, the
/// step of the call to it, makes it: at the call's site, by the function
/// it calls, in the function that makes it, at its level, and beginning
/// at its event, by rule 4 of [`merge_one_step_calls`].
fn done_by_call(firmware: &Firmware, call: Step, done: Step) -> Step {
    // 4.
    let item = firmware.item(call.inside);
    let local =
        item.is_some() && call.function.map(|function| firmware.item(function)) == Some(item);
    let (way, function, veneer) = match local {
        true => (done.way, done.function, done.veneer),
        false => (call.way, call.function, call.veneer),
    };
    Step {
        site: call.site,
        way,
        function,
        veneer,
        inside: call.inside,
        level: call.level,
        begins: call.event,
        ..done
    }
}

/// `steps`, with a call that a loop over a table makes in each iteration
/// listed once, but for the iterations in which it copies or zeroes, and
/// not at all when it does in some, as for a row that copies none; and
/// code that elfq can't follow in each iteration listed once, with what
/// it may set up in any.
fn keep_once_per_table_loop(
    ram: &RamSections,
    iterations: &Iterations,
    steps: Vec<Step>,
) -> Vec<Step> {
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
            Does::Call | Does::Copy | Does::Zero => {}
        }
        kept.push(step);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        alone, binary, called, closed, copied, described, loop_with, steps, steps_to, walked_each,
        with_facts,
    };
    use super::*;
    use crate::arch::startup::{Iteration, Loop, Runs};

    /// A call at `site`, `depth` calls deep, through `how`, to `target`,
    /// which passes nothing that the walk can tell, and which the walk
    /// goes through.
    fn call(site: u64, depth: u32, target: Option<u64>, how: How) -> Call {
        Call {
            site,
            depth,
            target,
            how,
            arguments: [None; 4],
            left: 0,
            conditional: false,
            followed: true,
        }
    }

    /// A step of code elfq can't follow, a loop at `site` in the reset
    /// handler, at `event`, which may set up `may`.
    fn unread(event: usize, site: u64, may: Vec<usize>) -> Step {
        Step {
            may,
            ..Step::new(
                event,
                site,
                Does::Unread,
                Way::Loop,
                Some(0x800_0000),
                0x800_0000,
                0,
            )
        }
    }

    /// As `unread`, a loop that copies .data.
    fn copy(ram: &RamSections, event: usize, site: u64) -> Step {
        let mut step = unread(event, site, Vec::new());
        let numbers = [Some(0x2000_0000), Some(0x800_0400), Some(0x20), None];
        step.set_up(ram.match_call(&numbers).expect("a copy of .data"));
        step
    }

    /// `steps`, as [`described`] writes them.
    fn written(ram: &RamSections, steps: &[Step]) -> Vec<String> {
        steps.iter().map(|step| described(ram, step)).collect()
    }

    #[test]
    fn from_events_reads_each_event_as_a_step_or_none() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let list = [
            // A call through r3, which the walk can't tell.
            Event::Call(call(0x800_0010, 0, None, How::Register(3))),
            called(0x800_0014, [0x2000_0000, 0x800_0400, 0x20]),
            // A call that neither copies nor zeroes, and a call in the
            // code of the function it calls, which is no step.
            Event::Call(call(0x800_0018, 0, Some(0x800_0300), How::Direct)),
            Event::Call(call(0x800_0304, 1, Some(0x800_0200), How::Direct)),
            // A conditional call, which the walk doesn't go through.
            Event::Call(Call {
                conditional: true,
                followed: false,
                ..call(0x800_001c, 0, Some(0x800_0200), How::Direct)
            }),
            // A call to code outside the file, as in a boot ROM, and one
            // to a function that goes on past the walk's budget: each
            // stops in the code of the function it calls.
            Event::Call(call(0x800_0020, 0, Some(0x1000_0000), How::Direct)),
            Event::Stop {
                site: 0x800_0020,
                depth: 1,
                end: End::LeavesFile {
                    site: 0x800_0020,
                    target: 0x1000_0000,
                },
            },
            Event::Call(call(0x800_0024, 0, Some(0x800_0300), How::Direct)),
            Event::Stop {
                site: 0x800_0024,
                depth: 1,
                end: End::Budget(0x800_0340),
            },
        ];
        let steps = with_facts(&binary, &list, End::Returns(0x800_0028), |facts| {
            from_events(facts, &Iterations::new(facts.events.list))
        });
        assert_eq!(
            written(&ram, &steps),
            [
                "unread 0x0 0 by a register call: .data, .ramfunc, .bss, .noinit",
                "copy 0x20000000 32 by a call: .data 32",
                "call 0x0 0 by a call: ",
                "unread 0x0 0 by a conditional call: .ramfunc, .bss, .noinit",
                "call 0x0 0 by a call: ",
                "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
                "call 0x0 0 by a call: ",
                "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
            ]
        );
        let call = &steps[2];
        assert_eq!(call.function, Some(0x800_0300));
        assert_eq!(call.span, Some(4));
        assert_eq!(steps[3].event, 4);
        // A stop's step is by the function that the call goes to, in the
        // code of the function that makes the call.
        let outside = &steps[5];
        assert_eq!(outside.way, Way::Outside(0x1000_0000));
        assert_eq!(
            (outside.function, outside.inside),
            (Some(0x1000_0000), 0x800_0000)
        );
        let budget = &steps[7];
        assert_eq!(budget.way, Way::Stop(End::Budget(0x800_0340)));
        assert_eq!(
            (budget.function, budget.inside),
            (Some(0x800_0300), 0x800_0000)
        );
    }

    #[test]
    fn add_end_adds_code_elfq_cant_follow_where_the_walk_stops_short() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let list = [called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20])];
        let ended = |end: End, main: Option<Main>| {
            with_facts(&binary, &list, end, |facts| {
                let mut steps = from_events(facts, &Iterations::new(facts.events.list));
                add_end(&Facts { main, ..*facts }, &mut steps);
                steps
            })
        };
        let steps = ended(End::Budget(0x800_0020), None);
        assert_eq!(
            written(&ram, &steps),
            [
                "copy 0x20000000 32 by a call: .data 32",
                "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
            ]
        );
        let stop = &steps[1];
        assert_eq!((stop.event, stop.site), (1, 0x800_0020));
        assert_eq!((stop.function, stop.inside), (Some(0x800_0000), 0x800_0000));
        // A walk that returns from the reset handler stops nowhere short.
        assert_eq!(ended(End::Returns(0x800_0020), None).len(), 1);
        // Where debug info says a compiler inlined `main`, it is a step.
        let inlined = Main {
            address: 0x800_0020,
            from: MainFrom::DebugInfo,
        };
        let steps = ended(End::Main(0x800_0020), Some(inlined));
        let main = &steps[1];
        assert!(main.main && main.does == Does::Call && main.way == Way::Inlined);
        assert_eq!((main.site, main.function), (0x800_0020, Some(0x800_0020)));
    }

    #[test]
    fn narrow_unread_may_keeps_what_no_earlier_step_sets_up() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let [data, _, bss, _] = ram.sections[..] else {
            panic!("four sections to set up");
        };
        let mut steps = vec![
            unread(0, 0x800_0010, vec![data, bss]),
            copy(&ram, 1, 0x800_0014),
            unread(2, 0x800_0018, vec![data, bss]),
            unread(3, 0x800_001c, vec![data]),
        ];
        narrow_unread_may(&mut steps);
        assert_eq!(
            written(&ram, &steps),
            [
                "unread 0x0 0 by a loop: .data, .bss",
                "copy 0x20000000 32 by a loop: .data 32",
                "unread 0x0 0 by a loop: .bss",
            ]
        );
    }

    #[test]
    fn mark_table_rows_gives_a_copy_in_an_iteration_its_row() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        // One iteration, which reads its row at 0x8000500.
        let list = [
            called(0x800_0010, [0; 3]),
            Event::Loop(Loop {
                events: 0..1,
                runs: Runs::EachIteration(vec![Iteration {
                    events: 0..1,
                    row: Some(0x800_0500),
                }]),
                ..loop_with(Vec::new())
            }),
        ];
        let mut steps = vec![
            copy(&ram, 0, 0x800_0010),
            unread(0, 0x800_0010, vec![ram.sections[0]]),
            copy(&ram, 1, 0x800_0010),
        ];
        mark_table_rows(&Iterations::new(&list), &mut steps);
        let tables: Vec<Option<u64>> = steps.iter().map(|step| step.table).collect();
        assert_eq!(tables, [Some(0x800_0500), None, None]);
    }

    #[test]
    fn merge_one_step_calls_makes_a_call_to_a_function_that_copies_the_copy() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let copy_in = |site, numbers: [u32; 3]| {
            Event::Call(Call {
                arguments: [Some(numbers[0]), Some(numbers[1]), Some(numbers[2]), None],
                ..call(site, 1, Some(0x800_0200), How::Direct)
            })
        };
        let list = [
            // A function that copies .data and does nothing else.
            Event::Call(call(0x800_0010, 0, Some(0x800_0300), How::Direct)),
            copy_in(0x800_0304, [0x2000_0000, 0x800_0400, 0x20]),
            // One that makes another call before it copies .ramfunc.
            Event::Call(call(0x800_0014, 0, Some(0x800_0340), How::Direct)),
            Event::Call(call(0x800_0344, 1, Some(0x800_0380), How::Direct)),
            copy_in(0x800_0348, [0x2000_0020, 0x800_0420, 0x10]),
        ];
        let steps = with_facts(&binary, &list, End::Returns(0x800_0020), |facts| {
            let steps = from_events(facts, &Iterations::new(facts.events.list));
            merge_one_step_calls(facts, steps)
        });
        assert_eq!(
            written(&ram, &steps),
            [
                "copy 0x20000000 32 by a call: .data 32",
                "call 0x0 0 by a call: ",
                "copy 0x20000020 16 by a call: .ramfunc 16",
            ]
        );
        let merged = &steps[0];
        assert_eq!((merged.site, merged.inside), (0x800_0010, 0x800_0000));
        assert_eq!((merged.begins, merged.event, merged.level), (0, 1, 0));
        assert_eq!(merged.called(), Some(0x800_0300));
        assert_eq!(steps[2].level, 1);
    }

    #[test]
    fn keep_once_per_table_loop_lists_what_each_iteration_does_once() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        let [_, _, bss, noinit] = ram.sections[..] else {
            panic!("four sections to set up");
        };
        // Two iterations, of one event each.
        let list = [
            called(0x800_0010, [0; 3]),
            called(0x800_0010, [0; 3]),
            walked_each(&[0..1, 1..2]),
        ];
        let iterations = Iterations::new(&list);
        let call = |event| {
            Step::new(
                event,
                0x800_0010,
                Does::Call,
                Way::Call,
                Some(0x800_0200),
                0x800_0000,
                0,
            )
        };
        let steps = vec![
            call(0),
            unread(0, 0x800_0014, vec![noinit]),
            call(1),
            unread(1, 0x800_0014, vec![bss]),
        ];
        assert_eq!(
            written(&ram, &keep_once_per_table_loop(&ram, &iterations, steps)),
            [
                "call 0x0 0 by a call: ",
                "unread 0x0 0 by a loop: .bss, .noinit"
            ]
        );
        // A call that copies in one iteration isn't listed for the others.
        let steps = vec![call(0), copy(&ram, 1, 0x800_0010)];
        assert_eq!(
            written(&ram, &keep_once_per_table_loop(&ram, &iterations, steps)),
            ["copy 0x20000000 32 by a loop: .data 32"]
        );
    }

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
