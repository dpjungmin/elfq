//! The text of `startup`: a line that says whether there are problems,
//! a sentence for each, the steps in a table, notes, and what to try.

use std::io::Write;

use anyhow::Result;

use super::problems::{Kind, Problem, RESET, Status};
use super::start::MainFrom;
use super::steps::{Does, Memory, Step, Way};
use super::{Answer, Firmware};
use crate::arch::startup::{End, Event, Through};
use crate::commands::text::{names_text, plural, shell_quote};
use crate::render::{Align, Table};

/// How many rows the table of steps shows without `--all`: the house cap,
/// with the last row summing up the rest.
const ROWS: usize = 30;

pub(super) fn write(answer: &Answer, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "{}", lead(answer))?;
    let problems = &answer.judged.problems;
    if !problems.is_empty() {
        writeln!(out)?;
        for p in problems {
            writeln!(out, "  {}", sentence(answer, p))?;
        }
    }
    if !answer.steps.is_empty() {
        writeln!(out)?;
        let mut table = Table::new(&[
            ("STEP", Align::Right),
            ("DOES", Align::Left),
            ("FROM", Align::Left),
            ("TO", Align::Left),
            ("SIZE", Align::Right),
            ("AT", Align::Left),
            ("WHAT", Align::Left),
        ]);
        let count = answer.steps.len();
        let shown = if answer.all || count <= ROWS {
            count
        } else {
            ROWS - 1
        };
        for step in &answer.steps[..shown] {
            table.row(row(answer, step));
        }
        if shown < count {
            let mut cells = vec![String::new(); 7];
            cells[0] = format!("{}-{count}", shown + 1);
            cells[6] = format!("{} more steps; --all lists them", count - shown);
            table.row(cells);
        }
        table.write(out)?;
    }
    let notes = &answer.judged.notes;
    if !notes.is_empty() {
        writeln!(out)?;
        for note in notes {
            writeln!(out, "{}", note.text)?;
        }
    }
    let tries = try_lines(answer);
    if !tries.is_empty() {
        writeln!(out)?;
        writeln!(out, "Try:")?;
        for line in tries {
            writeln!(out, "  {line}")?;
        }
    }
    Ok(())
}

/// The first line: how many problems, or what startup does when there are
/// none.
fn lead(answer: &Answer) -> String {
    let firmware = answer.firmware;
    let memory = answer.memory;
    let boot = firmware.name(answer.reset.address);
    let main = answer.main;
    let inlined = main.is_some_and(|main| main.from == MainFrom::DebugInfo);
    let reached = answer.reached_main();
    let endless = match (answer.walk.end, main) {
        (End::Main(_), Some(main)) if main.from == MainFrom::Loop => Some(main.address),
        _ => None,
    };
    let problems = &answer.judged.problems;
    // Without startup code, as when the entry point is main.
    let first = reached && main.is_some_and(|main| main.address == answer.reset.address);
    if first {
        let main = firmware.name(answer.reset.address);
        return match problems.len() {
            0 => format!(
                "No problems: the processor starts at {main}, so no startup code runs before it."
            ),
            n => format!(
                "{} before {main}, where the processor starts:",
                plural(n, "problem", "problems")
            ),
        };
    }
    if !problems.is_empty() {
        let end = if reached && inlined {
            " before it runs main".to_string()
        } else if reached {
            format!(" before it calls {}", main_text(answer))
        } else if let Some(head) = endless {
            format!(" before its endless loop at {head:#x}")
        } else {
            match answer.walk.end {
                End::Spins(_) => " before it spins".to_string(),
                End::Returns(_) => " before it returns".to_string(),
                _ => String::new(),
            }
        };
        return format!(
            "{} in what {boot} does{end}:",
            plural(problems.len(), "problem", "problems")
        );
    }
    if memory.ram.is_empty() {
        return format!(
            "{boot} has nothing to copy or zero: the file stores every section where it runs, \
             and none starts as zeros."
        );
    }
    // The sections in the order the steps set them up.
    let mut ordered = memory.ram.clone();
    let first = |x: usize| {
        (answer.steps.iter())
            .find(|step| step.covers.iter().any(|cover| cover.section == x))
            .map_or(answer.steps.len() + 1, |step| step.number)
    };
    ordered.sort_by_key(|&x| first(x));
    let status = |x| answer.judged.status(answer.steps, memory, x).0;
    let mut parts = Vec::new();
    for (verb, wanted) in [("copies", Status::Copied), ("zeroes", Status::Zeroed)] {
        let names: Vec<&str> = (ordered.iter().copied())
            .filter(|&x| status(x) == wanted)
            .map(|x| memory.name(x))
            .collect();
        if !names.is_empty() {
            parts.push(format!("{verb} {}", names_text(&names, true)));
        }
    }
    let what = if parts.is_empty() {
        "copies and zeroes nothing".to_string()
    } else {
        parts.join(", ")
    };
    let then = if reached && inlined {
        "then runs main, inlined into it".to_string()
    } else if reached {
        format!("then calls {}", main_text(answer))
    } else if let Some(head) = endless {
        format!("then runs its endless loop at {head:#x}")
    } else {
        match answer.walk.end {
            End::Spins(_) => "then spins".to_string(),
            End::Returns(_) => "then returns".to_string(),
            End::RunsOff(_) => {
                let events = answer.events;
                let last = (0..events.list.len())
                    .rev()
                    .filter(|&k| events.level(k) == 0)
                    .find_map(|k| events.target(k));
                match last {
                    Some(last) => {
                        format!("then calls {}, which doesn't return", firmware.name(last))
                    }
                    None => "then stops".to_string(),
                }
            }
            End::Loops(site) => {
                let head = answer
                    .walk
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match event {
                        Event::Loop(l) if l.back == site => Some(l.head),
                        _ => None,
                    });
                format!("then loops for good at {:#x}", head.unwrap_or(site))
            }
            End::Rejoins(at) => format!("then loops back to {at:#x}"),
            End::Budget(site) => format!("then goes on past elfq's budget at {site:#x}"),
            end => format!("then {}", other_way(end)),
        }
    };
    format!("No problems: {boot} {what}, {then}.")
}

/// `main`'s name, as the lead and the table give it.
fn main_text(answer: &Answer) -> String {
    let Some(main) = answer.main else {
        return String::new();
    };
    match main.from {
        MainFrom::DebugInfo => "main".to_string(),
        MainFrom::Guess => format!("{} (taken as main)", answer.firmware.name(main.address)),
        MainFrom::Symbol | MainFrom::Loop => answer.firmware.name(main.address),
    }
}

/// The name of the item at `address`, with the address unless the name
/// says it.
fn at_name(firmware: &Firmware, address: u64) -> String {
    let name = firmware.name(address);
    if name.contains('@') || name.starts_with("fn_0x") {
        name
    } else {
        format!("{name} at {address:#x}")
    }
}

/// The sentence that says what problem `p` is.
pub(super) fn sentence(answer: &Answer, p: &Problem) -> String {
    let firmware = answer.firmware;
    let memory = answer.memory;
    let steps = answer.steps;
    let Some(&x) = p.sections.first() else {
        return String::new();
    };
    let name = memory.name(x);
    let section = memory.section(x);
    let step = p.steps.first().copied().unwrap_or(0);
    match p.kind {
        Kind::RunsBeforeCopy => {
            let what = match &p.items[..] {
                [one] => at_name(firmware, *one),
                items => {
                    let names: Vec<String> = items.iter().map(|&a| firmware.name(a)).collect();
                    let names: Vec<&str> = names.iter().map(String::as_str).collect();
                    names_text(&names, answer.all)
                }
            };
            let boot_only = p.uses.contains(&RESET);
            let tail = match p.before {
                None if answer.judged.unknown.contains_key(&x) => {
                    format!("before anything copies {name}")
                }
                None => "which nothing copies".to_string(),
                Some(before) => {
                    let before = &steps[before];
                    let inside = (p.uses.iter()).filter(|&&k| k >= 0).any(|&k| {
                        (k as usize) < before.event && before.event < answer.events.span(k as usize)
                    });
                    if boot_only || inside || p.steps.contains(&before.number) {
                        "which nothing has copied yet".to_string()
                    } else {
                        format!("before step {} copies {name}", before.number)
                    }
                }
            };
            if boot_only {
                let boot = answer.reset.address;
                return format!(
                    "{} runs at {boot:#x} in {name}, {tail}{}.",
                    firmware.name(boot),
                    unless(answer, p)
                );
            }
            let direct = p.items.iter().all(|a| {
                let chain = &p.chains[a];
                (chain.iter())
                    .filter(|&&c| firmware.veneer_target(c).is_none())
                    .count()
                    <= 2
            });
            let verb = match (p.steps.len() == 1, direct) {
                (true, true) => "calls",
                (true, false) => "runs",
                (false, true) => "call",
                (false, false) => "run",
            };
            let mut who = steps_text(&p.steps, true);
            if let [one] = p.steps[..]
                && let first = &steps[one - 1]
                && first.does == Does::Call
                && let Some(function) = first.function
                && !p.items.contains(&function)
            {
                who = format!("{who}, {},", firmware.name(function));
            }
            let separator = if tail.starts_with("before") {
                " "
            } else {
                ", "
            };
            format!(
                "{who} {verb} {what} in {name}{separator}{tail}{}.",
                unless(answer, p)
            )
        }
        Kind::NotCopied => format!(
            "Nothing copies {name}, {} bytes at {:#x} stored at {:#x}.",
            section.size,
            section.address,
            memory.load_address(x)
        ),
        Kind::NotZeroed => format!(
            "Nothing zeroes {name}, {} bytes at {:#x}.",
            section.size, section.address
        ),
        Kind::PartlyCopied => format!(
            "Step {step} copies {} of {name}'s {} bytes.",
            p.bytes.unwrap_or(0),
            section.size
        ),
        Kind::PartlyZeroed => format!(
            "Step {step} zeroes {} of {name}'s {} bytes.",
            p.bytes.unwrap_or(0),
            section.size
        ),
        Kind::CopiedFromElsewhere => format!(
            "Step {step} copies {name} from {:#x}, but the file stores it at {:#x}.",
            p.from.unwrap_or(0),
            memory.load_address(x)
        ),
        Kind::CantFollow => format!(
            "elfq can't follow {}, which may {}.",
            unread_text(steps, &p.steps),
            may_text(memory, &p.sections)
        ),
    }
}

/// For code that runs from a load image after code elfq can't follow,
/// which may copy it first: ", unless step 1 (a loop) copies it first".
fn unless(answer: &Answer, p: &Problem) -> String {
    if p.certain {
        return String::new();
    }
    let x = p.sections[0];
    let first = p.first();
    let numbers: Vec<usize> = (answer.steps.iter())
        .filter(|step| {
            step.does == Does::Unread && (step.event as i64) <= first && step.may.contains(&x)
        })
        .map(|step| step.number)
        .collect();
    if numbers.is_empty() {
        return String::new();
    }
    format!(
        ", unless {} copies it first",
        unread_text(answer.steps, &numbers)
    )
}

/// The cells of a step's row.
fn row(answer: &Answer, step: &Step) -> Vec<String> {
    let firmware = answer.firmware;
    let name = |address: u64| firmware.name(address);
    let function = step.function.map(name).unwrap_or_default();
    let via = if step.veneer.is_some() {
        " via veneer"
    } else {
        ""
    };
    let in_inside = |what: String| {
        if step.level > 0 {
            format!("{what}, in {}", name(step.inside))
        } else {
            what
        }
    };
    let what = match step.does {
        Does::Copy | Does::Zero => {
            let names: Vec<&str> = (step.covers.iter())
                .map(|cover| answer.memory.name(cover.section))
                .collect();
            in_inside(format!(
                "{}, by {function}{via}",
                names_text(&names, answer.all)
            ))
        }
        Does::Call if step.way == Way::Inlined => {
            format!("main, inlined into {}", name(step.inside))
        }
        Does::Call => {
            let guessed = step.main && answer.main.is_some_and(|main| main.from == MainFrom::Guess);
            let taken = if guessed { " (taken as main)" } else { "" };
            in_inside(format!("{function}{via}{taken}"))
        }
        Does::Unread => match step.way {
            Way::Loop => format!("a loop in {function}"),
            Way::RegisterCall(register) => {
                format!(
                    "a call through {} in {}",
                    register_name(register),
                    name(step.inside)
                )
            }
            Way::SupervisorCall => format!("a supervisor call in {}", name(step.inside)),
            Way::Conditional => {
                format!("a conditional call to {function} in {}", name(step.inside))
            }
            Way::Deep => format!(
                "a call to {function} in {}, deeper than elfq follows",
                name(step.inside)
            ),
            Way::Outside(target) => {
                format!(
                    "a call to {target:#x} in {}, outside the file",
                    name(step.inside)
                )
            }
            Way::Stop(end) => match end {
                End::Branches {
                    through: Through::Register(register),
                    ..
                } => format!("a branch through {} in {function}", register_name(register)),
                End::Branches {
                    through: Through::Memory,
                    ..
                } => format!("a branch through a word in memory in {function}"),
                End::Branches {
                    through: Through::Table,
                    ..
                } => format!("a jump table in {function}"),
                End::LeavesFile { .. } => format!("a jump out of the file in {function}"),
                End::Undecoded(_) => format!("code in {function} that elfq can't decode"),
                _ => format!("code in {function} past elfq's budget"),
            },
            Way::Call | Way::TailCall | Way::Inlined => function,
        },
    };
    let hex = |value: Option<u64>| value.map_or(String::new(), |value| format!("{value:#x}"));
    vec![
        step.number.to_string(),
        step.does.word().to_string(),
        hex(step.from),
        hex(step.to),
        step.size.map_or(String::new(), |size| size.to_string()),
        format!("{:#x}", step.site),
        what,
    ]
}

/// The name of register `register`.
pub(super) fn register_name(register: u16) -> String {
    match register {
        13 => "sp".to_string(),
        14 => "lr".to_string(),
        15 => "pc".to_string(),
        _ => format!("r{register}"),
    }
}

/// `reach --to` for code that runs before its copy inside a call, to show
/// how the call gets there.
fn try_lines(answer: &Answer) -> Vec<String> {
    let mut lines = Vec::new();
    let file = shell_quote(&answer.path.display().to_string());
    for p in &answer.judged.problems {
        // Code that runs inside a call, but not the reset handler's own.
        let (Kind::RunsBeforeCopy, Some(&first)) = (p.kind, p.steps.first()) else {
            continue;
        };
        if p.uses.contains(&RESET) {
            continue;
        }
        let step = &answer.steps[first - 1];
        let x = p.sections[0];
        let section = answer.memory.section(x);
        let Some(function) = step.function else {
            continue;
        };
        if step.does != Does::Call
            || p.items.contains(&function)
            || (section.address..answer.memory.end(x)).contains(&function)
        {
            continue;
        }
        let line = format!(
            "elfq {file} reach {} --to {:#x}+{}",
            shell_quote(&answer.firmware.name(function)),
            section.address,
            section.size
        );
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// The steps numbered `numbers`: "Step 3", "Steps 2 and 5", "Steps 6 to
/// 24", or for many runs, "40 steps from 2 to 90".
pub(super) fn steps_text(numbers: &[usize], capital: bool) -> String {
    let mut numbers = numbers.to_vec();
    numbers.sort_unstable();
    numbers.dedup();
    let (one, many) = if capital {
        ("Step", "Steps")
    } else {
        ("step", "steps")
    };
    let word = if numbers.len() == 1 { one } else { many };
    let (Some(&first), Some(&last)) = (numbers.first(), numbers.last()) else {
        return String::new();
    };
    if numbers.len() > 2 && last - first == numbers.len() - 1 {
        return format!("{word} {first} to {last}");
    }
    // Runs of three or more read "6 to 24".
    let mut parts = Vec::new();
    let mut i = 0;
    while i < numbers.len() {
        let mut j = i;
        while j + 1 < numbers.len() && numbers[j + 1] == numbers[j] + 1 {
            j += 1;
        }
        if j - i >= 2 {
            parts.push(format!("{} to {}", numbers[i], numbers[j]));
        } else {
            parts.extend(numbers[i..=j].iter().map(usize::to_string));
        }
        i = j + 1;
    }
    if parts.len() > 4 {
        // The JSON lists each one.
        return format!("{} {many} from {first} to {last}", numbers.len());
    }
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    format!("{word} {}", names_text(&parts, true))
}

/// The steps numbered `numbers` of code elfq can't follow, with what kinds
/// of code they are: "step 3 (a loop)", "steps 2 and 5 (a loop and a call
/// through r3)".
pub(super) fn unread_text(steps: &[Step], numbers: &[usize]) -> String {
    let unread: Vec<&Step> = numbers.iter().map(|&n| &steps[n - 1]).collect();
    let loops = unread.iter().filter(|step| step.way == Way::Loop).count();
    let registers: Vec<u16> = (unread.iter())
        .filter_map(|step| match step.way {
            Way::RegisterCall(register) => Some(register),
            _ => None,
        })
        .collect();
    let mut kinds: Vec<String> = Vec::new();
    match loops {
        0 => {}
        1 => kinds.push("a loop".to_string()),
        _ => kinds.push("loops".to_string()),
    }
    match registers[..] {
        [] => {}
        [one] => kinds.push(format!("a call through {}", register_name(one))),
        _ => kinds.push("calls through registers".to_string()),
    }
    for (way, one, many) in [
        (Way::SupervisorCall, "a supervisor call", "supervisor calls"),
        (Way::Conditional, "a conditional call", "conditional calls"),
        (
            Way::Deep,
            "a call deeper than elfq follows",
            "calls deeper than elfq follows",
        ),
    ] {
        match unread.iter().filter(|step| step.way == way).count() {
            0 => {}
            1 => kinds.push(one.to_string()),
            _ => kinds.push(many.to_string()),
        }
    }
    // Each kind once, for more than one step in the plural.
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for step in &unread {
        let (one, many) = match step.way {
            Way::Outside(_) => (
                "a call outside the file".to_string(),
                "calls outside the file",
            ),
            Way::Stop(end) => match end {
                End::Branches {
                    through: Through::Register(register),
                    ..
                } => (
                    format!("a branch through {}", register_name(register)),
                    "branches through registers",
                ),
                End::Branches {
                    through: Through::Memory,
                    ..
                } => (
                    "a branch through a word in memory".to_string(),
                    "branches through words in memory",
                ),
                End::Branches {
                    through: Through::Table,
                    ..
                } => ("a jump table".to_string(), "jump tables"),
                End::LeavesFile { .. } => (
                    "a jump out of the file".to_string(),
                    "jumps out of the file",
                ),
                End::Undecoded(_) => (
                    "code elfq can't decode".to_string(),
                    "code elfq can't decode",
                ),
                _ => ("the end of its budget".to_string(), "the end of its budget"),
            },
            _ => continue,
        };
        match groups.iter_mut().find(|(group, _)| group == many) {
            Some((_, ones)) => ones.push(one),
            None => groups.push((many.to_string(), vec![one])),
        }
    }
    for (many, ones) in groups {
        match &ones[..] {
            [one] => kinds.push(one.clone()),
            [first, rest @ ..]
                if rest.iter().all(|one| one == first)
                    && first.starts_with("a branch through r") =>
            {
                kinds.push(format!(
                    "branches through {}",
                    &first["a branch through ".len()..]
                ));
            }
            _ => kinds.push(many),
        }
    }
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    format!(
        "{} ({})",
        steps_text(numbers, false),
        names_text(&kinds, true)
    )
}

/// What code elfq can't follow may do to `sections`: "copy .data and zero
/// .bss".
pub(super) fn may_text(memory: &Memory, sections: &[usize]) -> String {
    let copies: Vec<&str> = (sections.iter().copied())
        .filter(|&x| memory.is_load(x))
        .map(|x| memory.name(x))
        .collect();
    let zeroes: Vec<&str> = (sections.iter().copied())
        .filter(|&x| !memory.is_load(x))
        .map(|x| memory.name(x))
        .collect();
    let mut parts = Vec::new();
    if !copies.is_empty() {
        parts.push(format!("copy {}", names_text(&copies, true)));
    }
    if !zeroes.is_empty() {
        parts.push(format!("zero {}", names_text(&zeroes, true)));
    }
    parts.join(" and ")
}

/// How the way the walk didn't take ended, for the note on a branch it
/// took instead.
pub(super) fn other_way(end: End) -> String {
    let site = end.site();
    match end {
        End::Branches {
            through: Through::Register(register),
            ..
        } => format!(
            "ends in a branch through {} at {site:#x}",
            register_name(register)
        ),
        End::Branches {
            through: Through::Memory,
            ..
        } => format!("ends in a branch through a word in memory at {site:#x}"),
        End::Branches {
            through: Through::Table,
            ..
        } => format!("ends in a jump table at {site:#x}"),
        End::LeavesFile { .. } => format!("jumps out of the file at {site:#x}"),
        End::Undecoded(_) => format!("reaches code elfq can't decode at {site:#x}"),
        End::Spins(_) => format!("spins at {site:#x}"),
        End::Returns(_) => format!("returns at {site:#x}"),
        _ => format!("ends at {site:#x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_named_with_runs_as_ranges() {
        assert_eq!(steps_text(&[3], true), "Step 3");
        assert_eq!(steps_text(&[5, 2], false), "steps 2 and 5");
        assert_eq!(steps_text(&[4, 2, 3, 3], false), "steps 2 to 4");
        assert_eq!(
            steps_text(&[1, 2, 4, 6, 7, 8], true),
            "Steps 1, 2, 4 and 6 to 8"
        );
        // Past four parts, the JSON lists them.
        assert_eq!(
            steps_text(&[1, 3, 5, 7, 9, 10, 11], false),
            "7 steps from 1 to 11"
        );
        assert_eq!(steps_text(&[], false), "");
    }

    #[test]
    fn unread_steps_say_what_kind_of_code_each_is() {
        let step = |way| Step::new(0, 0, Does::Unread, way, 0, 0);
        let branch = |site, register| {
            step(Way::Stop(End::Branches {
                site,
                through: Through::Register(register),
            }))
        };
        let steps = [
            step(Way::Loop),
            step(Way::RegisterCall(3)),
            step(Way::Loop),
            step(Way::Deep),
            branch(0x10, 2),
            branch(0x20, 2),
            branch(0x30, 3),
            step(Way::SupervisorCall),
            step(Way::Conditional),
            step(Way::Outside(0x1fff_0000)),
        ];
        let text = |numbers: &[usize]| unread_text(&steps, numbers);
        assert_eq!(text(&[1]), "step 1 (a loop)");
        assert_eq!(
            text(&[1, 2]),
            "steps 1 and 2 (a loop and a call through r3)"
        );
        assert_eq!(
            text(&[1, 3, 4]),
            "steps 1, 3 and 4 (loops and a call deeper than elfq follows)"
        );
        // Branches through the same register name it once.
        assert_eq!(text(&[5, 6]), "steps 5 and 6 (branches through r2)");
        assert_eq!(text(&[5, 7]), "steps 5 and 7 (branches through registers)");
        assert_eq!(
            text(&[8, 9, 10]),
            "steps 8 to 10 (a supervisor call, a conditional call and a call outside the file)"
        );
    }
}
