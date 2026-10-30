//! Where startup code starts and ends: the reset handler, and `main`, from
//! its symbol, from debug info where a compiler inlined it into the reset
//! handler, or without either, as in a stripped file, taken from what the
//! reset handler calls.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Result, bail};

use super::Firmware;
use super::steps::{Events, Memory};
use crate::arch::startup::{self, Call, DEPTH, End, Event, How, Start, Walk};
use crate::binary::{Binary, ItemKind};
use crate::debuginfo;

/// Where the processor starts running code.
pub(super) struct Reset {
    /// The reset handler, or without a vector table, the entry point.
    pub address: u64,
    /// The vector table's first word, the stack pointer at reset.
    pub stack_pointer: Option<u32>,
    /// Where the vector table is.
    pub table: Option<u64>,
    /// What the vector table gives for reset, when that is in no function,
    /// so that elfq reads from the entry point.
    pub outside: Option<u64>,
}

impl Reset {
    /// The reset handler that the vector table gives, when it is in a
    /// function, or else the entry point, when that is.
    pub fn find(path: &Path, binary: &Binary) -> Result<Reset> {
        let in_function = |address| {
            binary
                .item_at(address)
                .is_some_and(|index| binary.items[index].kind == ItemKind::Function)
        };
        let table = binary.vector_table.as_ref();
        let stack_pointer =
            table.and_then(|table| binary.pointer_at(table.address).map(|word| word as u32));
        let reset = table.and_then(|table| table.reset_handler());
        if let Some(reset) = reset
            && in_function(reset)
        {
            return Ok(Reset {
                address: reset,
                stack_pointer,
                table: table.map(|table| table.address),
                outside: None,
            });
        }
        if !in_function(binary.entry) {
            let file = path.display();
            let entry = binary.entry;
            match reset {
                Some(reset) => bail!(
                    "{file}'s vector table gives {reset:#x} for reset, which is in no \
                     function, and its entry point {entry:#x} is in none either"
                ),
                None => {
                    bail!(
                        "{file} has no vector table, and its entry point {entry:#x} is in no function"
                    )
                }
            }
        }
        Ok(Reset {
            address: binary.entry,
            stack_pointer: reset.and(stack_pointer),
            table: reset.and(table).map(|table| table.address),
            outside: reset,
        })
    }

    /// Walks the code from the reset handler, `follow` calls deep, ending
    /// at `main`.
    pub fn walk(&self, binary: &Binary, main: Option<u64>, follow: u32) -> Walk {
        walk_from(binary, self.address, self.stack_pointer, main, follow)
    }
}

/// Walks the code from `address`, with `stack_pointer` at reset.
fn walk_from(
    binary: &Binary,
    address: u64,
    stack_pointer: Option<u32>,
    main: Option<u64>,
    follow: u32,
) -> Walk {
    let start = Start {
        address,
        stack_pointer,
        main,
        follow,
    };
    startup::walk(binary.architecture, binary.endianness, binary, &start)
        .expect("startup walks only Arm code")
}

/// Where elfq takes startup to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Main {
    /// Where `main` starts, or the head of the reset handler's endless
    /// loop.
    pub address: u64,
    pub from: MainFrom,
}

/// How elfq knows where `main` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MainFrom {
    /// The symbol `main`.
    Symbol,
    /// Debug info that says a compiler inlined `main` into the reset
    /// handler, with link-time optimization, and where its code starts.
    DebugInfo,
    /// Without a symbol `main`, the function that the reset handler calls
    /// last (see [`guess`]).
    Guess,
    /// Without a symbol `main`, the head of a loop that the reset handler
    /// runs for good over its last call: the reset handler is the program,
    /// and its startup ends there.
    Loop,
}

impl MainFrom {
    /// The JSON's `main_from`.
    pub fn field(self) -> &'static str {
        match self {
            MainFrom::Symbol => "symbol",
            MainFrom::DebugInfo => "debug info",
            MainFrom::Guess => "guess",
            MainFrom::Loop => "loop",
        }
    }
}

impl Main {
    /// The function that is `main`, as a call's target: none for an
    /// inlined `main` or the reset handler's loop.
    pub fn function(self) -> Option<u64> {
        matches!(self.from, MainFrom::Symbol | MainFrom::Guess).then_some(self.address)
    }
}

/// Where startup ends: the function named `main`, or where debug info
/// says a compiler inlined it, or in a stripped file, what [`guess`] and
/// [`c_runtime`] take for it.
pub(super) fn find_main(firmware: &Firmware, memory: &Memory, reset: &Reset) -> Option<Main> {
    let binary = firmware.binary;
    let named = |name: &String| name == "main";
    if let Some(item) = binary.items.iter().find(|item| {
        item.kind == ItemKind::Function && (named(&item.name) || item.aliases.iter().any(named))
    }) {
        return Some(Main {
            address: item.address,
            from: MainFrom::Symbol,
        });
    }
    if binary.has_dwarf
        && let Ok(copies) = debuginfo::inlined_copies(binary)
        && let Some(copy) = copies.iter().find(|copy| copy.name == "main")
    {
        return Some(Main {
            address: copy.entry & !1,
            from: MainFrom::DebugInfo,
        });
    }
    let guessed = guess(firmware, memory, reset)?;
    if guessed.from == MainFrom::Guess
        && let Some(address) = c_runtime(firmware, memory, reset, guessed.address)
    {
        return Some(Main {
            address,
            from: MainFrom::Guess,
        });
    }
    Some(guessed)
}

/// Without a symbol `main`, as in a stripped file, `main` taken from the
/// reset handler's own code: the function it calls last before it spins,
/// returns, loops or runs off its end, or where it can't read on. When
/// that call can't return, and the call before it only hands it its
/// result, as in `exit(main())`, `main` is the call before, unless that
/// call copies or zeroes a section. When a loop goes back over the last
/// call, the reset handler is the program, and its loop ends startup.
fn guess(firmware: &Firmware, memory: &Memory, reset: &Reset) -> Option<Main> {
    let binary = firmware.binary;
    let walk = reset.walk(binary, None, 0);
    let calls: Vec<(&Call, u64)> = (walk.events.iter())
        .filter_map(|event| match event {
            Event::Call(call) => Some((call, firmware.resolve(call.target?).0)),
            _ => None,
        })
        .collect();
    let &(last_call, last) = calls.last()?;
    let last_site = last_call.site;
    let back = match walk.end {
        End::Loops(site) => walk.events.iter().rev().find_map(|event| match event {
            Event::Loop(l) if l.back == site => Some(l.head),
            _ => None,
        }),
        End::Rejoins(at) => Some(at),
        _ => None,
    };
    // Unless the code after the call is where an earlier branch goes: then
    // the call doesn't return, and the compiler put other code after it,
    // such as a loop's body.
    if let Some(head) = back
        && head <= last_site
        && !startup::branches_past(binary.architecture, binary, reset.address, last_site)
    {
        return Some(Main {
            address: head,
            from: MainFrom::Loop,
        });
    }
    if !matches!(
        walk.end,
        End::Spins(_)
            | End::Returns(_)
            | End::Loops(_)
            | End::Rejoins(_)
            | End::RunsOff(_)
            | End::Undecoded(_)
    ) {
        return None;
    }
    let mut main = last;
    // A call before that copies or zeroes a section isn't `main`, as in
    // `memset(...); main();` with a `main` that never returns. A last call
    // that copies or zeroes a section starts a C runtime, as `bl
    // SystemInit; bl _mainCRTStartup` does, rather than exiting.
    if let End::RunsOff(_) = walk.end
        && let [.., (before_call, before), _] = calls[..]
        && startup::passes_result(binary.architecture, binary, before_call.site, last_site)
        && memory.match_call(&before_call.passes()).is_none()
        && !sets_up(binary, memory, reset, last)
    {
        main = before;
    }
    Some(Main {
        address: main,
        from: MainFrom::Guess,
    })
}

/// Whether the function at `address` calls a function with the start of
/// a section that startup code sets up in r0, and a length in r2, in its
/// own code or code it jumps to, as picolibc's `_start` jumps to
/// `_cstart`.
fn sets_up(binary: &Binary, memory: &Memory, reset: &Reset, address: u64) -> bool {
    let walk = walk_from(binary, address, reset.stack_pointer, None, 1);
    walk.events.iter().any(|event| match event {
        Event::Call(call) if call.depth == 0 => {
            let [r0, _, r2, _] = call.passes();
            r2.is_some() && r0.is_some_and(|r0| memory.starts_section(u64::from(r0)))
        }
        _ => false,
    })
}

/// When a guessed `main` sets up a section that the reset handler hasn't,
/// it starts a C runtime, as newlib's `_mainCRTStartup` does, and `main`
/// is the last function it calls that sets up nothing, or the one before
/// that when the last only takes its result, as `exit(main())` does.
fn c_runtime(firmware: &Firmware, memory: &Memory, reset: &Reset, guess: u64) -> Option<u64> {
    let binary = firmware.binary;
    let walk = reset.walk(binary, None, DEPTH);
    let events = Events::new(firmware, &walk.events);
    let call_to_guess = (0..events.list.len())
        .find(|&k| events.level(k) == 0 && events.target(k) == Some(guess))?;
    let inside = call_to_guess + 1..events.span(call_to_guess);
    let set_up = |range: std::ops::Range<usize>, level: u32| -> BTreeSet<usize> {
        range
            .filter(|&k| events.level(k) >= level)
            .filter_map(|k| memory.match_call(&events.call(k)?.passes()))
            .flat_map(|matched| matched.covers.into_iter().map(|cover| cover.section))
            .collect()
    };
    let earlier = set_up(0..call_to_guess, 0);
    if set_up(inside.clone(), 1).is_subset(&earlier) {
        return None;
    }
    // Its own calls, as far as the code that it shares with the reset
    // handler, and those of the code it jumps to when that sets up a
    // section, as picolibc's `_start` jumps to `_cstart`.
    let ran: HashSet<u64> = (0..call_to_guess)
        .filter_map(|k| events.call(k).map(|call| call.site))
        .collect();
    let mut own = 1;
    let mut calls = Vec::new();
    for k in inside {
        let Some(call) = events.call(k).filter(|_| events.level(k) == own) else {
            continue;
        };
        if ran.contains(&call.site) {
            break;
        }
        if events.target(k).is_none() {
            continue;
        }
        if set_up(k..events.span(k), 1).is_empty() {
            calls.push(k);
        } else if call.how == How::Jump && call.followed {
            own += 1;
        }
    }
    let mut main = *calls.last()?;
    if let [.., before, last] = calls[..]
        && let (Some(first), Some(second)) = (events.call(before), events.call(last))
        && startup::passes_result(binary.architecture, binary, first.site, second.site)
    {
        main = before;
    }
    events.target(main)
}
