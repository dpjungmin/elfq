//! The steps of startup: each copy of a load image and each zeroing of a
//! section without contents, matched by the numbers that a call passes in
//! r0 to r2, as `memcpy(to, from, size)` and `memset(to, 0, size)` take
//! them; the calls that the reset handler makes around them; and the code
//! that elfq can't follow, which may set up any section not set up yet:
//! a call through a register, a call the walk doesn't go through, a
//! loop, and code the walk can't read on in.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use object::SectionKind;

use super::Firmware;
use super::start::{Main, MainFrom, Reset};
use crate::arch::startup::{Call, End, Event, How, Loop, Store};
use crate::binary::{Binary, ItemKind, Section};

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
        if let Some(from) = from.map(u64::from)
            && self.loads.iter().any(|&x| self.section(x).address == to)
            && self.stored(from)
        {
            return Some(Match {
                does: Does::Copy,
                to,
                from: Some(from),
                size,
                covers: self.cover(&self.loads, to, size, Some(from)),
            });
        }
        if from == Some(0) && self.starts_section(to) {
            let covers = self.cover(&self.zeros, to, size, None);
            if !covers.is_empty() {
                return Some(Match {
                    does: Does::Zero,
                    to,
                    from: None,
                    size,
                    covers,
                });
            }
        }
        None
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

/// The walk's events, with what the steps read of them.
pub(super) struct Events<'w> {
    pub list: &'w [Event],
    /// How many calls deep from the reset handler each event is, counting
    /// a tail call as a call, where the walk counts the function it jumps
    /// to at the depth of the one that jumps.
    levels: Vec<u32>,
    /// Whether each event is the jump of a veneer that a call goes to,
    /// which the call's step stands for.
    veneer_jumps: Vec<bool>,
    /// For each call that the walk can tell the target of, where it goes:
    /// past a veneer, to the function the veneer jumps to, with the
    /// veneer.
    targets: Vec<Option<(u64, Option<u64>)>>,
}

impl<'w> Events<'w> {
    pub fn new(firmware: &Firmware, list: &'w [Event]) -> Events<'w> {
        let veneer_jumps: Vec<bool> = (0..list.len())
            .map(|k| {
                let Event::Call(call) = &list[k] else {
                    return false;
                };
                let Some(veneer) = firmware.item(call.site) else {
                    return false;
                };
                let start = firmware.binary.items[veneer].address;
                call.how == How::Jump
                    && k > 0
                    && matches!(&list[k - 1], Event::Call(before) if before.target == Some(start))
                    && firmware.veneer_target(start).is_some()
            })
            .collect();
        let mut open: Vec<u32> = Vec::new();
        let levels = (list.iter().enumerate())
            .map(|(k, event)| {
                let depth = match event {
                    Event::Call(call) => call.depth,
                    Event::Store(store) => store.depth,
                    Event::Loop(l) => l.depth,
                    Event::Stop { depth, .. } => *depth,
                    Event::Backtrack { .. } => return 0,
                };
                // The function a tail call jumps to returns to the caller
                // of the one that jumped.
                while open.last().is_some_and(|&open| depth < open) {
                    open.pop();
                }
                let level = depth + open.len() as u32;
                if let Event::Call(call) = event
                    && call.how == How::Jump
                    && !veneer_jumps[k]
                {
                    open.push(call.depth);
                }
                level
            })
            .collect();
        let targets = (list.iter().zip(&veneer_jumps))
            .map(|(event, &veneer_jump)| match event {
                Event::Call(call) if !veneer_jump => Some(firmware.resolve(call.target?)),
                _ => None,
            })
            .collect();
        Events {
            list,
            levels,
            veneer_jumps,
            targets,
        }
    }

    pub fn level(&self, k: usize) -> u32 {
        self.levels[k]
    }

    /// The call at `k`, unless it is a veneer's jump.
    pub fn call(&self, k: usize) -> Option<&'w Call> {
        match &self.list[k] {
            Event::Call(call) if !self.veneer_jumps[k] => Some(call),
            _ => None,
        }
    }

    /// Where the call at `k` goes, past a veneer.
    pub fn target(&self, k: usize) -> Option<u64> {
        self.targets[k].map(|(target, _)| target)
    }

    /// The veneer that the call at `k` goes through.
    pub fn veneer(&self, k: usize) -> Option<u64> {
        self.targets[k].and_then(|(_, veneer)| veneer)
    }

    /// Where the events of the code that the call at `k` runs end: the
    /// first event after it that is no deeper.
    pub fn span(&self, k: usize) -> usize {
        let level = self.levels[k];
        (k + 1..self.list.len())
            .find(|&j| !matches!(self.list[j], Event::Backtrack { .. }) && self.levels[j] <= level)
            .unwrap_or(self.list.len())
    }
}

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
    /// A loop that elfq can't follow.
    Loop,
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
        }
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
                let way = if call.how == How::Jump {
                    Way::TailCall
                } else {
                    Way::Call
                };
                let mut step = Step::new(k, call.site, Does::Call, way, inside, level);
                step.function = target;
                step.veneer = events.veneer(k);
                if target.is_some() && target == main_function {
                    step.main = true;
                    steps.push(step);
                } else if let Some(matched) = memory.match_call(&call.passes()) {
                    step.does = matched.does;
                    step.to = Some(matched.to);
                    step.from = matched.from;
                    step.size = Some(matched.size);
                    ready.extend(matched.covers.iter().map(|cover| cover.section));
                    step.covers = matched.covers;
                    steps.push(step);
                    skip_to = events.span(k);
                } else if target.is_none() {
                    step.does = Does::Unread;
                    step.way = match call.how {
                        How::Register(register) => Way::RegisterCall(register),
                        How::Supervisor => Way::SupervisorCall,
                        How::Direct | How::Jump => continue,
                    };
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
            Event::Loop(walked) => {
                let function = at(&holder, l);
                let mut may = loop_targets(memory, events, k, walked, &ready);
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
            Event::Store(_) | Event::Backtrack { .. } => {}
        }
    }
    let k = events.list.len();
    match end {
        // Where the walk can't read on, or stops short, what comes after
        // may set up anything.
        End::Undecoded(_) | End::Branches { .. } | End::LeavesFile { .. } | End::Budget(_) => {
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
    for (n, step) in steps.iter_mut().enumerate() {
        step.number = n + 1;
    }
    steps
}

/// The sections a loop that elfq can't follow may set up: the run of
/// sections that its stores start in, or when it stores through a pointer
/// the walk can't tell, any not set up yet, or only those without
/// contents when it stores only zeros. None when it stores only elsewhere,
/// as to peripherals.
fn loop_targets(
    memory: &Memory,
    events: &Events,
    k: usize,
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let level = events.level(k);
    // Its own stores, not those of a loop inside it, nor of what it calls.
    let inner: Vec<Range<usize>> = (l.first_event..k)
        .filter_map(|j| match &events.list[j] {
            Event::Loop(inner) if events.level(j) == level => Some(inner.first_event..j),
            _ => None,
        })
        .collect();
    let stores: Vec<&Store> = (l.first_event..k)
        .filter(|&j| events.level(j) == level && !inner.iter().any(|range| range.contains(&j)))
        .filter_map(|j| match &events.list[j] {
            Event::Store(store) => Some(store),
            _ => None,
        })
        .collect();
    let mut may: Vec<usize> = Vec::new();
    for store in &stores {
        match store.base.or(store.address) {
            None => {
                let zeros_only = stores.iter().all(|store| store.value == Some(0));
                return (memory.ram.iter().copied())
                    .filter(|x| !ready.contains(x) && (!zeros_only || !memory.is_load(*x)))
                    .collect();
            }
            Some(at) => {
                if let Some(x) = memory.holding(u64::from(at)) {
                    for y in memory.run_from(x) {
                        if !ready.contains(&y) && !may.contains(&y) {
                            may.push(y);
                        }
                    }
                }
            }
        }
    }
    may
}

/// When a loop's code calls a function that copies, zeroes, or stores
/// into a section, as a loop over a table of sections does, every
/// section not set up yet.
fn loop_calls(
    memory: &Memory,
    events: &Events,
    k: usize,
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let level = events.level(k);
    let stores_into_ram = |j: usize| match &events.list[j] {
        Event::Store(store) => match store.address.or(store.base) {
            Some(at) => memory.holding(u64::from(at)).is_some(),
            None => true,
        },
        _ => false,
    };
    let sets_up = (l.first_event..k).any(|j| {
        events.level(j) == level
            && events.call(j).is_some_and(|call| {
                memory.match_call(&call.passes()).is_some()
                    || (j + 1..events.span(j)).any(stores_into_ram)
            })
    });
    if !sets_up {
        return Vec::new();
    }
    (memory.ram.iter().copied())
        .filter(|x| !ready.contains(x))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Firmware with code in flash at 0x8000000, and in RAM at 0x20000000:
    /// two load images, stored in flash one after the other, and two
    /// sections without contents.
    fn binary() -> Binary {
        let section = |name: &str, address, size, kind, load_address: Option<u64>| Section {
            name: name.to_string(),
            address,
            size,
            kind,
            allocated: true,
            file_range: (kind != SectionKind::UninitializedData).then_some((0x1000, size)),
            entry_size: 0,
            load_address,
        };
        let sections = vec![
            section(".text", 0x800_0000, 0x400, SectionKind::Text, None),
            section(
                ".data",
                0x2000_0000,
                0x20,
                SectionKind::Data,
                Some(0x800_0400),
            ),
            section(
                ".ramfunc",
                0x2000_0020,
                0x10,
                SectionKind::Text,
                Some(0x800_0420),
            ),
            section(
                ".bss",
                0x2000_0030,
                0x40,
                SectionKind::UninitializedData,
                None,
            ),
            section(
                ".noinit",
                0x2000_0070,
                0x10,
                SectionKind::UninitializedData,
                None,
            ),
        ];
        Binary::for_tests(sections, Vec::new(), Vec::new())
    }

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
