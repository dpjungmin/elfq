//! `elfq FILE debug walk`: the walk that `startup` reads, event by event
//! (see `arch::startup`): the calls that the code run from reset makes,
//! with their arguments, its stores outside the stack, its loops, and how
//! it ends. It starts at the reset handler that `Reset::find` takes and
//! ends at the `main` that `start::find_main` finds, as `startup` does. It
//! exists to check the walk while developing elfq.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};

use super::Firmware;
use super::ram_sections::RamSections;
use super::start::{self, Main, MainFrom, Reset};
use crate::arch::startup::{Call, DEPTH, End, Event, How, Loop, LoopStore, Store, Through};
use crate::binary::Binary;
use crate::commands::Outcome;
use crate::commands::kept::Kept;
use crate::graph::Graph;
use crate::render::{Align, Table};

/// Answers `elfq FILE debug walk` for the file at `path`, loaded as
/// `binary`. Its only refusal of its own, for code that isn't Thumb, comes
/// first; then it refuses what `Reset::find` refuses, as `startup` does.
pub(in crate::commands) fn run(
    path: &Path,
    binary: &Binary,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let code = binary
        .instruction_set()
        .and_then(|set| set.startup_code(binary));
    let Some(code) = code else {
        bail!("elfq walks only Thumb code, which Cortex-M processors run");
    };
    let reset = Reset::find(path, binary)?;
    let graph = Kept::new(Graph::build(binary));
    let firmware = Firmware::new(binary, code, &graph);
    let main = start::find_main(&firmware, &RamSections::new(binary), &reset);
    let walk = reset.walk(code, main.map(|main| main.address), DEPTH);
    writeln!(
        out,
        "from {}, stack pointer {}, main {}",
        address_text(binary, reset.address),
        hex(reset.stack_pointer),
        main_text(main),
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("SITE", Align::Right),
        ("DEPTH", Align::Right),
        ("WHAT", Align::Left),
    ]);
    for event in &walk.events {
        let (site, what) = match event {
            Event::Call(call) => (call.site, call_text(binary, call)),
            Event::Store(store) => (store.site, store_text(store)),
            Event::Loop(l) => (l.head, loop_text(l)),
            Event::Backtrack {
                site,
                target,
                because,
            } => (*site, backtrack_text(*target, *because)),
            Event::Stop { site, end, .. } => (*site, format!("stops: {}", end_at(*end))),
        };
        table.row(vec![format!("{site:#x}"), event.depth().to_string(), what]);
    }
    table.row(vec![
        format!("{:#x}", walk.end.site()),
        String::new(),
        format!("ends: {}", end_text(walk.end)),
    ]);
    table.write(out)?;
    Ok(Outcome::Answered)
}

/// Where `main` is, then how elfq found it, as the JSON's `main_from`
/// says, when that isn't its symbol; or `unknown`.
fn main_text(main: Option<Main>) -> String {
    let Some(main) = main else {
        return "unknown".to_string();
    };
    match main.from {
        MainFrom::Symbol => format!("{:#x}", main.address),
        MainFrom::DebugInfo | MainFrom::Guess | MainFrom::Loop => {
            format!("{:#x} ({})", main.address, main.from.field())
        }
    }
}

/// A call: what it calls and how, the numbers in r0 to r3, whether it is
/// conditional or the walk didn't follow it, and which of those numbers
/// an earlier call left.
fn call_text(binary: &Binary, call: &Call) -> String {
    let target = call.target.map(|target| address_text(binary, target));
    let what = match (call.how, target) {
        (How::Direct, Some(target)) => format!("call {target}"),
        (How::Jump, Some(target)) => format!("tail call {target}"),
        (How::Register(register), Some(target)) => format!("call {target} through r{register}"),
        (How::Register(register), None) => format!("call through r{register}"),
        (How::Supervisor, _) => "supervisor call".to_string(),
        (How::Direct | How::Jump, None) => "call".to_string(),
    };
    let arguments: Vec<String> = (call.arguments.iter().enumerate())
        .map(|(n, argument)| format!("r{n}={}", hex(*argument)))
        .collect();
    let condition = if call.conditional {
        ", conditional"
    } else if call.target.is_some() && !call.followed {
        ", not followed"
    } else {
        ""
    };
    let left: Vec<String> = (0..4)
        .filter(|&n| call.left & 1 << n != 0 && call.arguments[n].is_some())
        .map(|n| format!(" r{n}"))
        .collect();
    let left = match left.is_empty() {
        true => String::new(),
        false => format!(", left by a call:{}", left.concat()),
    };
    format!("{what} ({}){condition}{left}", arguments.join(" "))
}

/// A store outside the stack: what it stores, where and how many bytes,
/// where the code loaded what it stores from, and whether it is
/// conditional.
fn store_text(store: &Store) -> String {
    let place = match (store.address, store.base) {
        (Some(address), _) => format!("at {address:#x}"),
        (None, Some(base)) => format!("at {base:#x} on"),
        (None, None) => "somewhere".to_string(),
    };
    let from = match store.source {
        Some(source) => format!(", loaded from {:#x} at {:#x}", source.address, source.site),
        None => String::new(),
    };
    let condition = if store.conditional {
        ", conditional"
    } else {
        ""
    };
    format!(
        "store {} {place}, {}{from}{condition}",
        hex(store.value),
        bytes_text(store.size)
    )
}

/// A loop: how many of the rows above are its code's, how many times it
/// runs, the rows of a table that it reads when the walk went through
/// each iteration, what each of its stores does, and where the walk of
/// its code stopped short, if it did.
fn loop_text(l: &Loop) -> String {
    let mut what = match l.events.len() {
        0 => format!("loop back from {:#x}", l.back),
        1 => format!("loop back from {:#x} over the row above", l.back),
        n => format!("loop back from {:#x} over the {n} rows above", l.back),
    };
    if let Some(iterations) = l.iterations() {
        what += &format!(", runs {}", times_text(iterations));
    }
    what += &walked_text(l);
    for store in l.stores().into_iter().flatten() {
        what += &loop_store_text(store);
    }
    if let Some(end) = l.stopped() {
        what += &format!("; its code stops short: {}", end_at(end));
    }
    what
}

/// That the walk went through each iteration of a loop, and where they
/// read the rows of a table from; nothing for a loop whose code it went
/// through once.
fn walked_text(l: &Loop) -> String {
    let iterations = l.walked_iterations();
    if iterations.is_empty() {
        return String::new();
    }
    let rows: Vec<u32> = (iterations.iter())
        .filter_map(|iteration| iteration.row)
        .collect();
    match rows[..] {
        [] => ", each time walked".to_string(),
        [row] => format!(", each time walked, reading a row at {row:#x}"),
        [first, second, ..] => {
            let apart = second.wrapping_sub(first) as i32;
            format!(", each time walked, reading rows at {first:#x} {apart:+}n")
        }
    }
}

/// What a store in a loop's code does over all the loop's iterations:
/// where it stores in the first, and how far that moves each time.
fn loop_store_text(store: &LoopStore) -> String {
    let at = |address: u32, moves: i32| match store.times {
        1 => format!("{address:#x}"),
        _ => format!("{address:#x} {moves:+}n"),
    };
    let from = match store.source {
        Some((address, moves)) => format!(", from {}", at(address, moves)),
        None => format!(", of {}", hex(store.value)),
    };
    format!(
        "; {:#x} stores {} at {} {}{from}",
        store.site,
        bytes_text(store.size),
        at(store.address, store.step),
        times_text(store.times)
    )
}

/// The branch that the walk took when it backtracked, and how the other
/// way ended.
fn backtrack_text(target: u64, because: End) -> String {
    format!(
        "takes the branch to {target:#x}: the other way {}",
        end_at(because)
    )
}

/// How the walk, or the walk of a function it calls, ended.
fn end_text(end: End) -> String {
    match end {
        End::Main(_) => "reaches main".to_string(),
        End::Spins(_) => "spins".to_string(),
        End::Returns(_) => "returns".to_string(),
        End::Loops(_) => "loops for good".to_string(),
        End::Rejoins(_) => "gets back to code it went through".to_string(),
        End::RunsOff(_) => "runs on past a call that can't return".to_string(),
        End::Undecoded(_) => "finds no code it can run".to_string(),
        End::Branches { through, .. } => match through {
            Through::Register(register) => format!("branches through r{register}"),
            Through::Memory => "branches through a word in memory".to_string(),
            Through::Table => "branches to a case of a jump table".to_string(),
        },
        End::LeavesFile { target, .. } => format!("goes to {target:#x}, outside the file's code"),
        End::Budget(_) => "stops after as many instructions as it may".to_string(),
    }
}

/// How the walk of some code ended, and at which instruction.
fn end_at(end: End) -> String {
    format!("{} at {:#x}", end_text(end), end.site())
}

/// `size` bytes, or `?` bytes when the store doesn't say how many.
fn bytes_text(size: u32) -> String {
    match size {
        0 => "? bytes".to_string(),
        1 => "1 byte".to_string(),
        size => format!("{size} bytes"),
    }
}

/// How many times a loop runs, or one of its stores stores.
fn times_text(times: u32) -> String {
    match times {
        1 => "once".to_string(),
        times => format!("{times} times"),
    }
}

/// `address`, after the item whose bytes include it and how far into it
/// it is, when an item's do.
fn address_text(binary: &Binary, address: u64) -> String {
    match binary.item_at(address).map(|index| &binary.items[index]) {
        Some(item) if item.address == address => format!("{} ({address:#x})", item.name),
        Some(item) => format!("{}+{:#x} ({address:#x})", item.name, address - item.address),
        None => format!("{address:#x}"),
    }
}

/// `number` in hex, or `?` when the walk can't tell it.
fn hex(number: Option<u32>) -> String {
    number.map_or("?".to_string(), |number| format!("{number:#x}"))
}
