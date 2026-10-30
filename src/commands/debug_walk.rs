//! `elfq FILE debug walk`: follows the code that a Cortex-M processor runs
//! from reset, as far as `main`, and lists what the walk finds there (see
//! `arch::startup`): calls with their arguments, stores outside the stack,
//! loops, and how it ends. It exists to check the walk while developing
//! elfq.

use std::io::Write;

use anyhow::{Result, bail};

use crate::arch::startup::{self, End, Event, How, Start, Through};
use crate::binary::{Binary, ItemKind};
use crate::render::{Align, Table};

/// Answers `elfq FILE debug walk`.
pub fn run(binary: &Binary, out: &mut dyn Write) -> Result<()> {
    let start = start(binary);
    let walk = binary
        .thumb
        .then(|| startup::walk(binary.architecture, binary.endianness, binary, &start))
        .flatten();
    let Some(walk) = walk else {
        bail!("elfq walks only Thumb code, which Cortex-M processors run");
    };
    writeln!(
        out,
        "from {}, stack pointer {}, main {}",
        name(binary, start.address),
        number(start.stack_pointer),
        start
            .main
            .map_or("unknown".to_string(), |main| format!("{main:#x}")),
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("SITE", Align::Right),
        ("DEPTH", Align::Right),
        ("WHAT", Align::Left),
    ]);
    for (index, event) in walk.events.iter().enumerate() {
        let (site, depth, what) = match event {
            Event::Call(call) => {
                let target = call.target.map(|target| name(binary, target));
                let what = match (call.how, target) {
                    (How::Direct, Some(target)) => format!("call {target}"),
                    (How::Jump, Some(target)) => format!("tail call {target}"),
                    (How::Register(register), Some(target)) => {
                        format!("call {target} through r{register}")
                    }
                    (How::Register(register), None) => format!("call through r{register}"),
                    (How::Supervisor, _) => "supervisor call".to_string(),
                    (_, None) => "call".to_string(),
                };
                let arguments: Vec<String> = (call.arguments.iter().enumerate())
                    .map(|(n, argument)| format!("r{n}={}", number(*argument)))
                    .collect();
                let condition = if call.conditional {
                    ", conditional"
                } else {
                    ""
                };
                (
                    call.site,
                    call.depth,
                    format!("{what} ({}){condition}", arguments.join(" ")),
                )
            }
            Event::Store(store) => {
                let place = match (store.address, store.base) {
                    (Some(address), _) => format!("at {address:#x}"),
                    (None, Some(base)) => format!("at {base:#x} on"),
                    (None, None) => "somewhere".to_string(),
                };
                let size = match store.size {
                    0 => "? bytes".to_string(),
                    1 => "1 byte".to_string(),
                    size => format!("{size} bytes"),
                };
                let what = format!(
                    "store {} {place}, {size}{}",
                    number(store.value),
                    if store.conditional {
                        ", conditional"
                    } else {
                        ""
                    }
                );
                (store.site, store.depth, what)
            }
            Event::Loop(l) => {
                let what = match index - l.first_event {
                    0 => format!("loop back from {:#x}", l.back),
                    1 => format!("loop back from {:#x} over the row above", l.back),
                    n => format!("loop back from {:#x} over the {n} rows above", l.back),
                };
                let what = match l.end {
                    Some(end) => format!(
                        "{what}; its code stops short: {} at {:#x}",
                        end_text(end),
                        end.site()
                    ),
                    None => what,
                };
                (l.head, l.depth, what)
            }
            Event::Backtrack {
                site,
                target,
                because,
            } => {
                let what = format!(
                    "takes the branch to {target:#x}: the other way {} at {:#x}",
                    end_text(*because),
                    because.site()
                );
                (*site, 0, what)
            }
            Event::Stop { site, depth, end } => {
                let what = format!("stops: {} at {:#x}", end_text(*end), end.site());
                (*site, *depth, what)
            }
        };
        table.row(vec![format!("{site:#x}"), depth.to_string(), what]);
    }
    table.row(vec![
        format!("{:#x}", walk.end.site()),
        String::new(),
        format!("ends: {}", end_text(walk.end)),
    ]);
    table.write(out)?;
    Ok(())
}

/// Where the processor starts, as `startup` will take it: the vector
/// table's reset handler, or without a table, the entry point. `main` is
/// the function of that name.
fn start(binary: &Binary) -> Start {
    let table = binary.vector_table.as_ref();
    let reset = table.and_then(|table| table.reset_handler());
    let stack_pointer = table
        .filter(|_| reset.is_some())
        .and_then(|table| binary.pointer_at(table.address));
    let main = binary
        .items
        .iter()
        .find(|item| item.kind == ItemKind::Function && item.name == "main");
    Start {
        address: reset.unwrap_or(binary.entry),
        stack_pointer: stack_pointer.map(|word| word as u32),
        main: main.map(|item| item.address),
    }
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

/// The item at `address`, and how far into it, or the address alone.
fn name(binary: &Binary, address: u64) -> String {
    match binary.item_at(address).map(|index| &binary.items[index]) {
        Some(item) if item.address == address => format!("{} ({address:#x})", item.name),
        Some(item) => format!("{}+{:#x} ({address:#x})", item.name, address - item.address),
        None => format!("{address:#x}"),
    }
}

/// `number` in hex, or `?` when the walk can't tell it.
fn number(number: Option<u32>) -> String {
    number.map_or("?".to_string(), |number| format!("{number:#x}"))
}
