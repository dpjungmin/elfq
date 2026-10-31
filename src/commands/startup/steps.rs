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

use object::SectionKind;

use super::Firmware;
use super::start::{Main, MainFrom, Reset};
use crate::arch::startup::{Call, End, Event, How, Loop, LoopStore, Store};
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
        let from = from.map(u64::from);
        (from.and_then(|from| self.matched(Does::Copy, to, Some(from), size)))
            .or_else(|| (from == Some(0)).then(|| self.matched(Does::Zero, to, None, size))?)
    }

    /// What code does that copies `size` bytes to `to` from `from`, or
    /// zeroes them: a copy of load images, when one starts at `to` and the
    /// file stores bytes at `from`, or a zeroing of the sections without
    /// contents that start in them, when a section starts at `to`.
    fn matched(&self, does: Does, to: u64, from: Option<u64>, size: u64) -> Option<Match> {
        let covers = match does {
            Does::Copy => {
                let from = from?;
                let starts = self.loads.iter().any(|&x| self.section(x).address == to);
                if !(starts && self.stored(from)) {
                    return None;
                }
                self.cover(&self.loads, to, size, Some(from))
            }
            Does::Zero if self.starts_section(to) => self.cover(&self.zeros, to, size, None),
            _ => return None,
        };
        if covers.is_empty() {
            return None;
        }
        Some(Match {
            does,
            to,
            from,
            size,
            covers,
        })
    }

    /// The bytes that the loop `l` copies or zeroes, when the walk can
    /// tell what each of its stores does each time it runs (see
    /// [`Loop::stores`]): those that its stores into the sections cover
    /// all of, one after another, each time the next ones up or down (see
    /// [`tiled`]). A copy stores each time words that it loads from as far
    /// after or before where it stores them; a zeroing stores 0 each time,
    /// which it doesn't load.
    fn loop_piece(&self, l: &Loop) -> Option<Piece> {
        let stores: Vec<&LoopStore> = (l.stores.as_ref()?.iter())
            .filter(|store| store.times > 0 && self.overlaps(stored_bytes(store)))
            .collect();
        let first = *stores.first()?;
        let (to, size) = tiled(&stores)?;
        // How far after each store a copy loads its words from.
        let gap = |store: &LoopStore| {
            let (from, step) = store.source?;
            (step == store.step).then(|| from.wrapping_sub(store.address))
        };
        let copies = (gap(first)).filter(|&by| stores.iter().all(|&store| gap(store) == Some(by)));
        let zeroes = (stores.iter()).all(|store| store.value == Some(0) && !store.loaded);
        let does = match copies {
            Some(_) => Does::Copy,
            None if zeroes => Does::Zero,
            None => return None,
        };
        Some(Piece {
            does,
            to,
            size,
            gap: copies,
            home: (0, 0),
            site: first.site,
            way: Way::Loop,
            step: None,
        })
    }

    /// The bytes that `store`, outside loops, copies or zeroes: a copy
    /// when the walk can tell where it loaded its words from, or a zeroing
    /// when it stores 0.
    fn store_piece(&self, store: &Store) -> Option<Piece> {
        let address = store.address?;
        let (to, size) = (u64::from(address), u64::from(store.size));
        if store.conditional || size == 0 || !self.overlaps(to..to + size) {
            return None;
        }
        let (does, gap) = match store.source {
            Some(source) => (Does::Copy, Some(source.address.wrapping_sub(address))),
            None if store.value == Some(0) => (Does::Zero, None),
            None => return None,
        };
        Some(Piece {
            does,
            to,
            size,
            gap,
            home: (0, 0),
            site: store.site,
            way: Way::Store,
            step: None,
        })
    }

    /// Whether any byte from `bytes` is in a section that startup code
    /// sets up.
    fn overlaps(&self, bytes: Range<u64>) -> bool {
        (self.ram.iter()).any(|&x| self.section(x).address < bytes.end && bytes.start < self.end(x))
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

/// Bytes that code copies or zeroes one after another: a loop's, or a
/// store's outside loops, or those of several of them next to each other
/// in the same function, as of a loop and of the stores that copy the
/// words its last time leaves to code after it.
#[derive(Debug, Clone, Copy)]
struct Piece {
    does: Does,
    to: u64,
    size: u64,
    /// For a copy, how far after where it stores each word it loads it
    /// from, wrapping round.
    gap: Option<u32>,
    /// The level and function of the code.
    home: (u32, u64),
    /// Its first store, of the loop's when there is a loop.
    site: u64,
    /// [`Way::Loop`] when there is a loop, or else [`Way::Store`].
    way: Way,
    /// The step that copies or zeroes its bytes, as an index into the
    /// steps.
    step: Option<usize>,
}

impl Piece {
    fn end(&self) -> u64 {
        self.to + self.size
    }

    /// Whether `other` does what this does with the bytes next to its
    /// own, in the same function.
    fn next_to(&self, other: &Piece) -> bool {
        self.home == other.home
            && self.does == other.does
            && self.gap == other.gap
            && (self.end() == other.to || other.end() == self.to)
            && !(self.step.is_some() && other.step.is_some())
    }
}

/// The pieces found so far, which steps copy or zero, or may when code
/// after them stores the bytes next to them.
#[derive(Default)]
struct Pieces(Vec<Piece>);

impl Pieces {
    /// Adds `piece`, of the event at `k`, joined with those next to it,
    /// and makes a step of the bytes, or extends the step of a piece it
    /// joins, when [`Memory::matched`] matches them: for the bytes of
    /// stores alone, only when they cover whole sections, copied from
    /// their load images. Returns whether a step stands for `piece`.
    fn add(
        &mut self,
        memory: &Memory,
        piece: Piece,
        k: usize,
        steps: &mut Vec<Step>,
        ready: &mut HashSet<usize>,
    ) -> bool {
        let mut joined = piece;
        let mut taken: Vec<usize> = Vec::new();
        while let Some(i) =
            (0..self.0.len()).find(|&i| !taken.contains(&i) && self.0[i].next_to(&joined))
        {
            let other = self.0[i];
            joined.to = joined.to.min(other.to);
            joined.size += other.size;
            joined.step = joined.step.or(other.step);
            if other.way == Way::Loop && joined.way != Way::Loop {
                (joined.way, joined.site) = (Way::Loop, other.site);
            }
            taken.push(i);
        }
        let from = (joined.gap).map(|gap| u64::from((joined.to as u32).wrapping_add(gap)));
        let matched = memory
            .matched(joined.does, joined.to, from, joined.size)
            .filter(|matched| {
                joined.step.is_some()
                    || joined.way == Way::Loop
                    || matched.covers.iter().all(|cover| {
                        cover.bytes == memory.section(cover.section).size
                            && (cover.from)
                                .is_none_or(|from| from == memory.load_address(cover.section))
                    })
            });
        let Some(matched) = matched else {
            if piece.way == Way::Store {
                self.0.push(piece);
            }
            return false;
        };
        let index = match joined.step {
            Some(index) => index,
            None => {
                let (level, function) = joined.home;
                let mut step = Step::new(k, joined.site, joined.does, joined.way, function, level);
                step.function = Some(function);
                steps.push(step);
                steps.len() - 1
            }
        };
        let step = &mut steps[index];
        step.to = Some(matched.to);
        step.from = matched.from;
        step.size = Some(matched.size);
        ready.extend(matched.covers.iter().map(|cover| cover.section));
        step.covers = matched.covers;
        taken.sort_unstable();
        for i in taken.into_iter().rev() {
            self.0.remove(i);
        }
        self.0.push(Piece {
            step: Some(index),
            ..joined
        });
        true
    }

    /// Takes in `store`, outside loops, of the code at `home`, which goes
    /// through a pointer into a section, at an offset that the walk can't
    /// tell: when a step of the same code covers only part of that
    /// section, as when the store is of a word that a loop's last time
    /// leaves to code after it, that step becomes code that elfq can't
    /// follow, which may set up the sections it covered and those that
    /// follow the section with no gap.
    fn unsure(
        &mut self,
        memory: &Memory,
        store: &Store,
        home: (u32, u64),
        steps: &mut [Step],
        ready: &mut HashSet<usize>,
    ) {
        if store.address.is_some() || store.conditional {
            return;
        }
        let Some(x) = store.base.and_then(|base| memory.holding(u64::from(base))) else {
            return;
        };
        let part = |index: usize| {
            (steps[index].covers.iter())
                .any(|cover| cover.section == x && cover.bytes < memory.section(x).size)
        };
        let Some(i) =
            (self.0.iter()).position(|piece| piece.home == home && piece.step.is_some_and(part))
        else {
            return;
        };
        let Some(index) = self.0.remove(i).step else {
            return;
        };
        let step = &mut steps[index];
        let mut may: Vec<usize> = step.covers.iter().map(|cover| cover.section).collect();
        for y in memory.run_from(x) {
            if !may.contains(&y) {
                may.push(y);
            }
        }
        step.does = Does::Unread;
        (step.covers, step.may) = (Vec::new(), may);
        (step.to, step.from, step.size) = (None, None, None);
        *ready = (steps.iter())
            .flat_map(|step| step.covers.iter().map(|cover| cover.section))
            .collect();
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
    // The events of loops' code, whose stores the loops' steps stand for.
    let mut in_loop = vec![false; events.list.len()];
    for (k, event) in events.list.iter().enumerate() {
        if let Event::Loop(l) = event {
            in_loop[l.first_event..k].fill(true);
        }
    }
    let mut pieces = Pieces::default();
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
                let piece = (memory.loop_piece(walked)).map(|piece| Piece {
                    home: (level, function),
                    ..piece
                });
                if let Some(piece) = piece
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
                let home = (level, at(&holder, l));
                match memory.store_piece(store) {
                    Some(piece) => {
                        let piece = Piece { home, ..piece };
                        pieces.add(memory, piece, k, &mut steps, &mut ready);
                    }
                    None => pieces.unsure(memory, store, home, &mut steps, &mut ready),
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

/// The bytes that a loop's store covers over all the times it stores.
fn stored_bytes(store: &LoopStore) -> Range<u64> {
    let first = i64::from(store.address);
    let last = first + i64::from(store.step) * i64::from(store.times.saturating_sub(1));
    let start = first.min(last).max(0) as u64;
    start..(first.max(last) + i64::from(store.size)).max(0) as u64
}

/// Where the bytes that `stores`, a loop's stores, cover start, and how
/// many there are, when they cover all of them once, one after another:
/// each time the loop runs, its stores store the bytes next to those of
/// the time before, all up or all down, and the last time, when the loop
/// leaves part way through, those next to them.
fn tiled(stores: &[&LoopStore]) -> Option<(u64, u64)> {
    let mut each: Vec<(i64, i64)> = (stores.iter())
        .map(|store| (i64::from(store.address), i64::from(store.size)))
        .collect();
    each.sort_unstable();
    let low = each.first()?.0;
    let size: i64 = each.iter().map(|&(_, size)| size).sum();
    let mut end = low;
    for &(start, size) in &each {
        if start != end {
            return None;
        }
        end += size;
    }
    // How many times all of them store, and those that store once more,
    // the last time.
    let most = stores.iter().map(|store| store.times).max()?;
    let all = stores.iter().all(|store| store.times == most);
    let full = i64::from(if all { most } else { most - 1 });
    let last: Vec<&&LoopStore> = (stores.iter())
        .filter(|store| !all && store.times == most)
        .collect();
    let moving: Vec<i64> = (stores.iter())
        .filter(|store| store.times > 1)
        .map(|store| i64::from(store.step))
        .collect();
    let step = match moving.first() {
        None => size,
        Some(&step) if step.abs() == size && moving.iter().all(|&other| other == step) => step,
        Some(_) => return None,
    };
    // The bytes that the last time stores, at one end of its stores'.
    let extra: i64 = last.iter().map(|store| i64::from(store.size)).sum();
    let (from, to) = match step > 0 {
        true => (low, low + extra),
        false => (low + size - extra, low + size),
    };
    let mut ends: Vec<(i64, i64)> = (last.iter())
        .map(|store| {
            (
                i64::from(store.address),
                i64::from(store.address) + i64::from(store.size),
            )
        })
        .collect();
    ends.sort_unstable();
    let mut at = from;
    for (start, end) in ends {
        if start != at {
            return None;
        }
        at = end;
    }
    if at != to {
        return None;
    }
    let start = match step > 0 {
        true => low,
        false => low - (full - 1) * size - extra,
    };
    Some((
        u64::try_from(start).ok()?,
        u64::try_from(full * size + extra).ok()?,
    ))
}

/// The sections a loop that elfq can't follow may set up: the run of
/// sections that its stores start in, or when it stores through a pointer
/// the walk can't tell, any not set up yet, or only those without
/// contents when it stores only zeros. None when it stores only elsewhere,
/// as to peripherals.
fn loop_targets(memory: &Memory, stores: &[&Store], ready: &HashSet<usize>) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    for store in stores {
        if store.base.is_none() && store.address.is_none() {
            let zeros_only = stores.iter().all(|store| store.value == Some(0));
            return (memory.ram.iter().copied())
                .filter(|x| !ready.contains(x) && (!zeros_only || !memory.is_load(*x)))
                .collect();
        }
        // The pointer, and where the store goes, which may be past it.
        for at in [store.base, store.address].into_iter().flatten() {
            if let Some(x) = memory.holding(u64::from(at)) {
                for y in memory.run_from(x) {
                    if !ready.contains(&y) && !may.contains(&y) {
                        may.push(y);
                    }
                }
            }
        }
    }
    may
}

/// The stores of the loop `l`, whose event is at `k`: its own, not those
/// of a loop inside it, nor of what it calls.
fn own_stores<'w>(events: &Events<'w>, k: usize, l: &Loop) -> Vec<&'w Store> {
    let level = events.level(k);
    let inner: Vec<Range<usize>> = (l.first_event..k)
        .filter_map(|j| match &events.list[j] {
            Event::Loop(inner) if events.level(j) == level => Some(inner.first_event..j),
            _ => None,
        })
        .collect();
    (l.first_event..k)
        .filter(|&j| events.level(j) == level && !inner.iter().any(|range| range.contains(&j)))
        .filter_map(|j| match &events.list[j] {
            Event::Store(store) => Some(store),
            _ => None,
        })
        .collect()
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

/// When the code of a loop that elfq can't follow has a loop that copies
/// or zeroes, as a loop over a table of sections does for its first row,
/// the sections of that kind not set up yet, which the loop's next times
/// may set up.
fn loop_repeats(memory: &Memory, steps: &[Step], l: &Loop, ready: &HashSet<usize>) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    let inner = (steps.iter()).filter(|step| step.way == Way::Loop && step.event >= l.first_event);
    for step in inner {
        let sections = match step.does {
            Does::Copy => &memory.loads,
            Does::Zero => &memory.zeros,
            _ => continue,
        };
        for &x in &memory.ram {
            if sections.contains(&x) && !ready.contains(&x) && !may.contains(&x) {
                may.push(x);
            }
        }
    }
    may
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::startup::Source;
    use crate::graph::Graph;

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

    /// A store of a loop's code at 0x8000018, over all the times the loop
    /// runs: of `size` bytes at `address` the first time, `step` bytes on
    /// each time, `times` times, of `value`.
    fn each(address: u32, step: i32, size: u32, times: u32, value: Option<u32>) -> LoopStore {
        LoopStore {
            site: 0x800_0018,
            address,
            step,
            size,
            times,
            value,
            source: None,
            loaded: false,
        }
    }

    /// As `each`, of words that the loop loads from `from` the first
    /// time, and from as far on each time.
    fn copied(address: u32, from: u32, step: i32, size: u32, times: u32) -> LoopStore {
        LoopStore {
            source: Some((from, step)),
            loaded: true,
            ..each(address, step, size, times, None)
        }
    }

    /// The loop from 0x8000010 to 0x8000020 with `stores`.
    fn walked(stores: Vec<LoopStore>) -> Loop {
        Loop {
            head: 0x800_0010,
            back: 0x800_0020,
            depth: 0,
            first_event: 0,
            end: None,
            passes: stores.iter().map(|store| store.times).max(),
            stores: Some(stores),
        }
    }

    /// What the loop with `stores` does, as "copy 0x20000000 32 from
    /// 0x8000400".
    fn piece(memory: &Memory, stores: Vec<LoopStore>) -> Option<String> {
        let piece = memory.loop_piece(&walked(stores))?;
        let from =
            (piece.gap).map(|gap| format!(" from {:#x}", (piece.to as u32).wrapping_add(gap)));
        Some(format!(
            "{} {:#x} {}{}",
            piece.does.word(),
            piece.to,
            piece.size,
            from.unwrap_or_default()
        ))
    }

    #[test]
    fn a_loop_s_stores_cover_its_bytes_one_time_after_another() {
        let tiled = |stores: &[LoopStore]| tiled(&stores.iter().collect::<Vec<_>>());
        // A word each time, up, or down from the last word; or once.
        assert_eq!(
            tiled(&[each(0x2000_0000, 4, 4, 8, None)]),
            Some((0x2000_0000, 32))
        );
        assert_eq!(
            tiled(&[each(0x2000_001c, -4, 4, 8, None)]),
            Some((0x2000_0000, 32))
        );
        assert_eq!(
            tiled(&[each(0x2000_0000, 0, 4, 1, None)]),
            Some((0x2000_0000, 4))
        );
        // Two words each time, as `strd` stores them, or two stores.
        assert_eq!(
            tiled(&[each(0x2000_0000, 8, 8, 4, None)]),
            Some((0x2000_0000, 32))
        );
        let two = [
            each(0x2000_0004, 8, 4, 4, None),
            each(0x2000_0000, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&two), Some((0x2000_0000, 32)));
        // The last time, the loop leaves after the first store: the next
        // word up, or down.
        let up = [
            each(0x2000_0000, 8, 4, 4, None),
            each(0x2000_0004, 8, 4, 3, None),
        ];
        assert_eq!(tiled(&up), Some((0x2000_0000, 28)));
        let down = [
            each(0x2000_001c, -8, 4, 4, None),
            each(0x2000_0018, -8, 4, 3, None),
        ];
        assert_eq!(tiled(&down), Some((0x2000_0004, 28)));
        // Every other word; stores with a gap between them; and a last
        // time that leaves a gap.
        assert_eq!(tiled(&[each(0x2000_0000, 8, 4, 4, None)]), None);
        let gap = [
            each(0x2000_0000, 8, 4, 4, None),
            each(0x2000_0008, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&gap), None);
        let short = [
            each(0x2000_0000, 8, 4, 3, None),
            each(0x2000_0004, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&short), None);
    }

    #[test]
    fn a_loop_copies_words_it_loads_as_far_on_and_zeroes_with_0() {
        let binary = binary();
        let memory = Memory::new(&binary);
        // .data and .ramfunc from their load images, a word at a time or
        // 16 bytes, as `ldm` and `stm` copy them.
        assert_eq!(
            piece(&memory, vec![copied(0x2000_0000, 0x800_0400, 4, 4, 12)]).as_deref(),
            Some("copy 0x20000000 48 from 0x8000400")
        );
        assert_eq!(
            piece(&memory, vec![copied(0x2000_0000, 0x800_0400, 16, 16, 2)]).as_deref(),
            Some("copy 0x20000000 32 from 0x8000400")
        );
        // .bss from its last word down, beside a store to a peripheral
        // each time, which sets up nothing.
        let zero = vec![
            each(0x2000_006c, -4, 4, 16, Some(0)),
            each(0x4000_0000, 0, 4, 16, Some(1)),
        ];
        assert_eq!(piece(&memory, zero).as_deref(), Some("zero 0x20000030 64"));
        // Not of words from a table that moves otherwise, or as far on
        // for each store; nor of a 0 that the loop loaded, nor of words it
        // can't tell; nor of a loop that stores only elsewhere.
        let mut table = copied(0x2000_0000, 0x800_0400, 4, 4, 8);
        table.source = Some((0x800_0400, 8));
        assert_eq!(piece(&memory, vec![table]), None);
        let apart = vec![
            copied(0x2000_0000, 0x800_0400, 8, 4, 4),
            copied(0x2000_0004, 0x800_0500, 8, 4, 4),
        ];
        assert_eq!(piece(&memory, apart), None);
        let loaded = LoopStore {
            loaded: true,
            ..each(0x2000_0030, 4, 4, 16, Some(0))
        };
        assert_eq!(piece(&memory, vec![loaded]), None);
        assert_eq!(
            piece(&memory, vec![each(0x2000_0030, 4, 4, 16, None)]),
            None
        );
        assert_eq!(
            piece(&memory, vec![each(0x4000_0000, 4, 4, 16, Some(0))]),
            None
        );
    }

    /// The steps of startup code whose walk found `list`, as "copy
    /// 0x20000000 32 by a loop: .data 32", one a line.
    fn steps(binary: &Binary, list: &[Event]) -> Vec<String> {
        let graph = Graph::default();
        let firmware = Firmware::new(binary, &graph);
        let memory = Memory::new(binary);
        let events = Events::new(&firmware, list);
        let reset = Reset {
            address: 0x800_0000,
            stack_pointer: None,
            table: None,
            outside: None,
        };
        let end = End::Returns(0x800_0100);
        (build(&firmware, &memory, &events, &reset, None, end).iter())
            .map(|step| {
                let covers: Vec<String> = (step.covers.iter())
                    .map(|cover| format!("{} {}", memory.name(cover.section), cover.bytes))
                    .chain(step.may.iter().map(|&x| memory.name(x).to_string()))
                    .collect();
                format!(
                    "{} {:#x} {} by a {}: {}",
                    step.does.word(),
                    step.to.unwrap_or_default(),
                    step.size.unwrap_or_default(),
                    step.way.field(),
                    covers.join(", ")
                )
            })
            .collect()
    }

    /// A store at `site`, outside loops, of a word loaded from `from`, or
    /// of `value`, at `address`, or past a pointer to `base` that the
    /// walk can tell when it can't tell `address`.
    fn alone(
        site: u64,
        address: Option<u32>,
        base: u32,
        from: Option<u32>,
        value: Option<u32>,
    ) -> Event {
        Event::Store(Store {
            site,
            depth: 0,
            address,
            base: Some(base),
            size: 4,
            value,
            source: from.map(|address| Source {
                site: site - 2,
                address,
            }),
            conditional: false,
        })
    }

    /// The event of the loop with `stores`, whose code has the `events`
    /// events before it.
    fn closed(stores: Vec<LoopStore>, first_event: usize) -> Event {
        Event::Loop(Loop {
            first_event,
            ..walked(stores)
        })
    }

    #[test]
    fn stores_after_a_loop_extend_its_step_and_alone_copy_only_whole_sections() {
        let binary = binary();
        // A loop that copies .data 8 bytes at a time, and leaves the last
        // word to a store after it, as GCC unrolls a copy.
        let list = [
            closed(vec![copied(0x2000_0000, 0x800_0400, 8, 8, 3)], 0),
            alone(
                0x800_0030,
                Some(0x2000_0018),
                0x2000_0018,
                Some(0x800_0418),
                None,
            ),
            alone(
                0x800_0034,
                Some(0x2000_001c),
                0x2000_0018,
                Some(0x800_041c),
                None,
            ),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a loop: .data 32"]
        );
        // Stores alone, of all of .ramfunc from its load image; but not of
        // part of it, nor from elsewhere, which may be code that sets a
        // variable.
        let whole: Vec<Event> = (0..4)
            .map(|n| {
                let address = 0x2000_0020 + 4 * n;
                alone(
                    0x800_0030 + 4 * u64::from(n),
                    Some(address),
                    address,
                    Some(0x800_0400 + address - 0x2000_0000),
                    None,
                )
            })
            .collect();
        assert_eq!(
            steps(&binary, &whole),
            ["copy 0x20000020 16 by a store: .ramfunc 16"]
        );
        assert_eq!(steps(&binary, &whole[..3]), Vec::<String>::new());
        let elsewhere = [alone(
            0x800_0030,
            Some(0x2000_0000),
            0x2000_0000,
            Some(0x800_0300),
            None,
        )];
        assert_eq!(steps(&binary, &elsewhere), Vec::<String>::new());
    }

    #[test]
    fn a_store_through_a_pointer_after_a_loop_that_sets_up_part_of_a_section_makes_it_unread() {
        let binary = binary();
        // A loop that zeroes all of .bss but its last word, which a store
        // through a pointer to .bss at an offset the walk can't tell may
        // zero, or not.
        let list = [
            closed(vec![each(0x2000_0030, 4, 4, 15, Some(0))], 0),
            alone(0x800_0030, None, 0x2000_0030, None, Some(0)),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
        // When the loop zeroes all of .bss, the store changes nothing.
        let list = [
            closed(vec![each(0x2000_0030, 4, 4, 16, Some(0))], 0),
            alone(0x800_0030, None, 0x2000_0030, None, Some(0)),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["zero 0x20000030 64 by a loop: .bss 64"]
        );
    }

    /// A call at `site` of the function at 0x8000200 with `arguments` in
    /// r0 to r2.
    fn called(site: u64, arguments: [u32; 3]) -> Event {
        Event::Call(Call {
            site,
            depth: 0,
            target: Some(0x800_0200),
            how: How::Direct,
            arguments: [
                Some(arguments[0]),
                Some(arguments[1]),
                Some(arguments[2]),
                None,
            ],
            left: 0,
            conditional: false,
            followed: true,
        })
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

    /// A word that the code at 0x8000018 stores at `address`, and that it
    /// loaded at `loaded` from `from`, or else holds `value`.
    fn stored(address: u32, value: Option<u32>, loaded: Option<(u64, u32)>) -> Store {
        Store {
            site: 0x800_0018,
            depth: 0,
            address: Some(address),
            base: Some(address),
            size: 4,
            value,
            source: loaded.map(|(site, address)| Source { site, address }),
            conditional: false,
        }
    }

    #[test]
    fn a_loop_elfq_cant_match_may_set_up_where_its_stores_go() {
        let binary = binary();
        let memory = Memory::new(&binary);
        // A store 4 bytes past a pointer into .ramfunc goes to .bss.
        let mut store = stored(0x2000_0030, None, None);
        store.base = Some(0x2000_002c);
        let names = |stores: &[&Store]| -> Vec<&str> {
            (loop_targets(&memory, stores, &HashSet::new()).into_iter())
                .map(|x| memory.name(x))
                .collect()
        };
        assert_eq!(names(&[&store]), [".ramfunc", ".bss", ".noinit"]);
        // Through a pointer the walk can't tell, any section not set up
        // yet, or only those without contents for a store of 0.
        store.base = None;
        store.address = None;
        assert_eq!(names(&[&store]), [".data", ".ramfunc", ".bss", ".noinit"]);
        store.value = Some(0);
        assert_eq!(names(&[&store]), [".bss", ".noinit"]);
    }
}
