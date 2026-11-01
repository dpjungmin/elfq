//! Where startup code starts and ends: the reset handler, and `main`, from
//! its symbol, from debug info where a compiler inlined it into the reset
//! handler, or without either, as in a stripped file, taken from what the
//! reset handler calls.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Result, bail};

use super::Firmware;
use super::events::Events;
use super::ram_sections::RamSections;
use crate::arch::startup::{Call, Code, DEPTH, End, Event, How, Start, Walk};
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
        let reset = binary.reset_handler();
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
    pub fn walk(&self, code: Code, main: Option<u64>, follow: u32) -> Walk {
        walk_from(code, self.address, self.stack_pointer, main, follow)
    }
}

/// Walks the code from `address`, with `stack_pointer` at reset.
fn walk_from(
    code: Code,
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
    code.walk(&start)
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
/// says a compiler inlined it, or without either, what [`guess`] and
/// [`c_runtime`] take for it.
pub(super) fn find_main(firmware: &Firmware, ram: &RamSections, reset: &Reset) -> Option<Main> {
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
    let guessed = guess(firmware, ram, reset)?;
    if guessed.from == MainFrom::Guess
        && let Some(address) = c_runtime(firmware, ram, reset, guessed.address)
    {
        return Some(Main {
            address,
            from: MainFrom::Guess,
        });
    }
    Some(guessed)
}

/// Without a symbol `main`, as in a stripped file, `main` taken from the
/// reset handler's own code, walked without following its calls:
///
/// 1. When it makes no call that the walk can tell the target of, there is
///    no `main`.
/// 2. When the walk ends going back to code at or before the last call, a
///    loop's head or code that it went through, a loop goes back over that
///    call: the reset handler is the program, and startup ends at the
///    loop's head ([`MainFrom::Loop`]). Not when an earlier branch goes
///    past the call: then the call doesn't return, and the compiler put
///    other code after it, such as a loop's body.
/// 3. Else, when the code after the last call spins, returns, loops, runs
///    off its end, or can't be read on, `main` is the function that the
///    last call goes to.
/// 4. Code that runs back into code that the walk went through elsewhere
///    is a loop that elfq can't follow, after which startup may go on: no
///    `main`, unless it runs back into a loop that ran before the last
///    call, where the compiler put that loop's code after a call that
///    doesn't return; then `main` is as in 3. Any other end, such as a
///    branch through a register, gives no `main` either.
/// 5. When the code runs off its end after the last call, so that the call
///    can't return, and the call before it only hands it its result, as in
///    `exit(main())`, `main` is the function that the call before goes to.
///    Not when that call copies or zeroes a section, as in `memset(...);
///    main();` with a `main` that never returns, nor when the last call's
///    function sets up a section (see [`sets_up`]): then it starts a C
///    runtime, as `bl SystemInit; bl _mainCRTStartup` does, rather than
///    exiting.
fn guess(firmware: &Firmware, ram: &RamSections, reset: &Reset) -> Option<Main> {
    let walk = reset.walk(firmware.code, None, 0);
    let calls: Vec<(usize, &Call, u64)> = (0..walk.events.len())
        .filter_map(|event| match &walk.events[event] {
            Event::Call(call) => Some((event, call, firmware.resolve(call.target?).function)),
            _ => None,
        })
        .collect();
    // 1.
    let &(last_event, last_call, last) = calls.last()?;
    let last_site = last_call.site;
    // 2.
    let back = match walk.end {
        End::Loops(site) => walk.events.iter().rev().find_map(|event| match event {
            Event::Loop(l) if l.back == site => Some(l.head),
            _ => None,
        }),
        End::Rejoins(at) => Some(at),
        End::Main(_)
        | End::Spins(_)
        | End::Returns(_)
        | End::RunsOff(_)
        | End::Undecoded(_)
        | End::Branches { .. }
        | End::LeavesFile { .. }
        | End::Budget(_) => None,
    };
    if let Some(head) = back
        && head <= last_site
        && !firmware.code.branches_past(reset.address, last_site)
    {
        return Some(Main {
            address: head,
            from: MainFrom::Loop,
        });
    }
    let ran_before = |at: u64| {
        (walk.events[..last_event].iter())
            .any(|event| matches!(event, Event::Loop(l) if l.head <= at && at <= l.back))
    };
    let ends = match walk.end {
        // 3.
        End::Spins(_) | End::Returns(_) | End::Loops(_) | End::RunsOff(_) | End::Undecoded(_) => {
            true
        }
        // 4.
        End::Rejoins(at) => ran_before(at),
        End::Main(_) | End::Branches { .. } | End::LeavesFile { .. } | End::Budget(_) => false,
    };
    if !ends {
        return None;
    }
    let mut main = last;
    // 5.
    if let End::RunsOff(_) = walk.end
        && let [.., (_, before_call, before), _] = calls[..]
        && firmware.code.passes_result(before_call.site, last_site)
        && ram.match_call(&before_call.passes()).is_none()
        && !sets_up(firmware.code, ram, reset, last)
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
fn sets_up(code: Code, ram: &RamSections, reset: &Reset, address: u64) -> bool {
    let walk = walk_from(code, address, reset.stack_pointer, None, 1);
    walk.events.iter().any(|event| match event {
        Event::Call(call) if call.depth == 0 => {
            let [r0, _, r2, _] = call.passes();
            r2.is_some() && r0.is_some_and(|r0| ram.starts_section(u64::from(r0)))
        }
        _ => false,
    })
}

/// When a guessed `main` sets up a section that the reset handler hasn't,
/// it starts a C runtime, as newlib's `_mainCRTStartup` does, and `main`
/// is the last function it calls that sets up nothing, or the one before
/// that when the last only takes its result, as `exit(main())` does.
fn c_runtime(firmware: &Firmware, ram: &RamSections, reset: &Reset, guess: u64) -> Option<u64> {
    let walk = reset.walk(firmware.code, None, DEPTH);
    let events = Events::new(firmware, &walk.events);
    let call_to_guess = (0..events.list.len())
        .find(|&event| events.level(event) == 0 && events.target(event) == Some(guess))?;
    let inside = call_to_guess + 1..events.span(call_to_guess);
    let set_up = |range: std::ops::Range<usize>, level: u32| -> BTreeSet<usize> {
        range
            .filter(|&event| events.level(event) >= level)
            .filter_map(|event| ram.match_call(&events.call(event)?.passes()))
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
        .filter_map(|event| events.call(event).map(|call| call.site))
        .collect();
    let mut own = 1;
    let mut calls = Vec::new();
    for event in inside {
        let Some(call) = events.call(event).filter(|_| events.level(event) == own) else {
            continue;
        };
        if ran.contains(&call.site) {
            break;
        }
        if events.target(event).is_none() {
            continue;
        }
        if set_up(event..events.span(event), 1).is_empty() {
            calls.push(event);
        } else if call.how == How::Jump && call.followed {
            own += 1;
        }
    }
    let mut main = *calls.last()?;
    if let [.., before, last] = calls[..]
        && let (Some(first), Some(second)) = (events.call(before), events.call(last))
        && firmware.code.passes_result(first.site, second.site)
    {
        main = before;
    }
    events.target(main)
}
