//! What the unit tests of `startup` build their walks from: firmware with
//! two load images and two sections without contents ([`binary`]), the
//! walk's events and stores, made by hand, and [`steps`], which writes the
//! steps that [`build`] reads from such a walk, one a line.

use std::ops::Range;

use object::SectionKind;

use super::events::Events;
use super::ram_sections::RamSections;
use super::start::Reset;
use super::steps::build;
use super::{Facts, Firmware};
use crate::arch::InstructionSet;
use crate::arch::startup::{
    Call, End, Event, How, Iteration, Loop, LoopStore, Runs, Source, Store,
};
use crate::binary::{Binary, Section};
use crate::graph::Graph;

/// `binary`, read as Thumb, with `graph`.
pub(super) fn firmware<'a>(binary: &'a Binary, graph: &'a Graph) -> Firmware<'a> {
    let thumb = InstructionSet::Thumb {
        endianness: binary.endianness,
    };
    let code = thumb.startup_code(binary).expect("Thumb code has a walk");
    Firmware::new(binary, code, graph)
}

/// Firmware with code in flash at 0x8000000, and in RAM at 0x20000000:
/// two load images, stored in flash one after the other, and two
/// sections without contents.
pub(super) fn binary() -> Binary {
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

/// A store of a loop's code at 0x8000018, over all the times the loop
/// runs: of `size` bytes at `address` the first time, `step` bytes on
/// each time, `times` times, of `value`.
pub(super) fn each(
    address: u32,
    step: i32,
    size: u32,
    times: u32,
    value: Option<u32>,
) -> LoopStore {
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
pub(super) fn copied(address: u32, from: u32, step: i32, size: u32, times: u32) -> LoopStore {
    LoopStore {
        source: Some((from, step)),
        loaded: true,
        ..each(address, step, size, times, None)
    }
}

/// The loop from 0x8000010 to 0x8000020 with `stores`, which runs as
/// many times as they store at most, and which the walk can't count
/// without any.
pub(super) fn loop_with(stores: Vec<LoopStore>) -> Loop {
    let runs = match stores.iter().map(|store| store.times).max() {
        Some(iterations) => Runs::Counted {
            iterations,
            stores: Some(stores),
        },
        None => Runs::Uncounted { stopped: None },
    };
    Loop {
        head: 0x800_0010,
        back: 0x800_0020,
        depth: 0,
        events: 0..0,
        runs,
    }
}

/// The steps of startup code whose walk found `list`, as "copy
/// 0x20000000 32 by a loop: .data 32", one a line.
pub(super) fn steps(binary: &Binary, list: &[Event]) -> Vec<String> {
    steps_to(binary, list, End::Returns(0x800_0100))
}

/// As `steps`, for a walk that ended at `end`.
pub(super) fn steps_to(binary: &Binary, list: &[Event], end: End) -> Vec<String> {
    let graph = Graph::default();
    let firmware = firmware(binary, &graph);
    let ram = RamSections::new(binary);
    let events = Events::new(&firmware, list);
    let reset = Reset {
        address: 0x800_0000,
        stack_pointer: None,
        table: None,
        outside: None,
    };
    let facts = Facts {
        firmware: &firmware,
        ram: &ram,
        reset: &reset,
        main: None,
        events: &events,
        end,
    };
    (build(&facts).iter())
        .map(|step| {
            let covers: Vec<String> = (step.covers.iter())
                .map(|cover| format!("{} {}", ram.name(cover.section), cover.bytes))
                .chain(
                    step.may
                        .iter()
                        .map(|&section| ram.name(section).to_string()),
                )
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
pub(super) fn alone(
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

/// The event of the loop with `stores`, whose code's events are
/// `events`.
pub(super) fn closed(stores: Vec<LoopStore>, events: Range<usize>) -> Event {
    Event::Loop(Loop {
        events,
        ..loop_with(stores)
    })
}

/// The event of a loop that the walk went through each iteration,
/// whose iterations' events are `iterations`, one after another.
pub(super) fn walked_each(iterations: &[Range<usize>]) -> Event {
    let first = iterations.first().map_or(0, |iteration| iteration.start);
    let last = iterations.last().map_or(0, |iteration| iteration.end);
    Event::Loop(Loop {
        events: first..last,
        runs: Runs::EachIteration(
            (iterations.iter())
                .map(|events| Iteration {
                    events: events.clone(),
                    row: None,
                })
                .collect(),
        ),
        ..loop_with(Vec::new())
    })
}

/// As `walked_each`, for a loop from 0x8000040 to 0x8000048.
pub(super) fn walked_each_inside(iterations: &[Range<usize>]) -> Event {
    let Event::Loop(l) = walked_each(iterations) else {
        unreachable!()
    };
    Event::Loop(Loop {
        head: 0x800_0040,
        back: 0x800_0048,
        ..l
    })
}

/// A call at `site` of the function at 0x8000200 with `arguments` in
/// r0 to r2.
pub(super) fn called(site: u64, arguments: [u32; 3]) -> Event {
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

/// A word that the code at 0x8000018 stores at `address`, and that it
/// loaded at `loaded` from `from`, or else holds `value`.
pub(super) fn stored(address: u32, value: Option<u32>, loaded: Option<(u64, u32)>) -> Store {
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
