//! The text of `startup`: a line that says whether there are problems,
//! a sentence for each, the steps in a table, notes, and what to try.

use std::io::Write;

use anyhow::Result;

use super::problems::{Kind, Problem, RunsAt, Status};
use super::ram_sections::RamSections;
use super::start::{Main, MainFrom};
use super::steps::{Does, Step, Way, number};
use super::{Answer, Ending, Facts, Firmware};
use crate::arch::startup::{End, Through, loop_head};
use crate::binary::Section;
use crate::commands::text::{self as house, names_text, plural, shell_quote};
use crate::render::{Align, Table};

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
        // The house cap, with the last row summing up the rest.
        let room = house::shown(answer.all);
        let shown = if count <= room { count } else { room - 1 };
        for (index, step) in answer.steps[..shown].iter().enumerate() {
            table.row(row(answer, index, step));
        }
        if shown < count {
            let mut cells = vec![String::new(); 7];
            cells[0] = format!("{}-{count}", number(shown));
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
    house::write_try(answer.path, &try_commands(answer), out)
}

/// The first line: how many problems, or what startup does when there are
/// none.
fn lead(answer: &Answer) -> String {
    let firmware = answer.facts.firmware;
    let ram = answer.facts.ram;
    let boot = firmware.name(answer.facts.reset.address);
    let ending = answer.facts.ending();
    let problems = &answer.judged.problems;
    // Without startup code, as when the entry point is main.
    if let Ending::EntryIsMain(_) = ending {
        let main = boot;
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
        return format!(
            "{} in what {boot} does{}:",
            plural(problems.len(), "problem", "problems"),
            before_text(firmware, ending)
        );
    }
    if ram.sections.is_empty() {
        return format!(
            "{boot} has nothing to copy or zero: the file stores every section where it runs, \
             and none starts as zeros."
        );
    }
    // The sections in the order the steps set them up.
    let mut ordered = ram.sections.clone();
    let init = &answer.judged.init;
    ordered.sort_by_key(|section| {
        init.get(section)
            .map_or(answer.steps.len(), |setup| setup.step)
    });
    let status = |section| answer.judged.status(section).status;
    // What the steps from event `from` up to `to` copy and zero.
    let parts = |from: usize, to: usize| {
        let by = |section: usize| {
            (init.get(&section))
                .is_some_and(|setup| (from..to).contains(&answer.steps[setup.step].event))
        };
        let mut parts = Vec::new();
        for (verb, wanted) in [("copies", Status::Copied), ("zeroes", Status::Zeroed)] {
            let names: Vec<&str> = (ordered.iter().copied())
                .filter(|&section| status(section) == wanted && by(section))
                .map(|section| ram.name(section))
                .collect();
            if !names.is_empty() {
                parts.push(format!("{verb} {}", names_text(&names, true)));
            }
        }
        parts
    };
    let events = answer.facts.events;
    // The last call the reset handler makes, when it doesn't return: it may
    // do the copies and zeroings itself, as Zephyr's z_prep_c does.
    let last = if let Ending::Walk(End::RunsOff(_)) = ending {
        (0..events.list.len())
            .rev()
            .filter(|&event| events.level(event) == 0)
            .find_map(|event| events.target(event).map(|target| (event, target)))
    } else {
        None
    };
    if let Some((event, last)) = last {
        let before = parts(0, event);
        let inside = parts(event, events.list.len());
        if !inside.is_empty() {
            let inside = format!("{} and", inside.join(", "));
            let before = match before.is_empty() {
                true => String::new(),
                false => format!("{}, then ", before.join(", ")),
            };
            return format!(
                "No problems: {boot} {before}calls {}, which {inside} doesn't return.",
                firmware.name(last)
            );
        }
    }
    let parts = parts(0, usize::MAX);
    let what = if parts.is_empty() {
        "copies and zeroes nothing".to_string()
    } else {
        parts.join(", ")
    };
    let then = then_text(answer, ending, last.map(|(_, last)| last));
    format!("No problems: {boot} {what}, {then}.")
}

/// How startup ends, for the lead when there are problems: " before it
/// calls main", or nothing when the walk ends some other way.
fn before_text(firmware: &Firmware, ending: Ending) -> String {
    match ending {
        Ending::EntryIsMain(main) | Ending::Main(main) => match main.from {
            MainFrom::DebugInfo => " before it runs main".to_string(),
            MainFrom::Symbol | MainFrom::Guess | MainFrom::Loop => {
                format!(" before it calls {}", main_text(firmware, main))
            }
        },
        Ending::EndlessLoop(head) => format!(" before its endless loop at {head:#x}"),
        Ending::Walk(End::Spins(_)) => " before it spins".to_string(),
        Ending::Walk(End::Returns(_)) => " before it returns".to_string(),
        Ending::Walk(
            End::Main(_)
            | End::Loops(_)
            | End::Rejoins(_)
            | End::RunsOff(_)
            | End::Undecoded(_)
            | End::Branches { .. }
            | End::LeavesFile { .. }
            | End::Budget(_),
        ) => String::new(),
    }
}

/// How startup ends, for the lead when there are no problems: "then calls
/// main". `last_call` is the function that the reset handler's last call
/// goes to, when the walk runs off the end of its code.
fn then_text(answer: &Answer, ending: Ending, last_call: Option<u64>) -> String {
    let firmware = answer.facts.firmware;
    match ending {
        Ending::EntryIsMain(main) | Ending::Main(main) => match main.from {
            MainFrom::DebugInfo => "then runs main, inlined into it".to_string(),
            MainFrom::Symbol | MainFrom::Guess | MainFrom::Loop => {
                format!("then calls {}", main_text(firmware, main))
            }
        },
        Ending::EndlessLoop(head) => format!("then runs its endless loop at {head:#x}"),
        Ending::Walk(end) => match end {
            End::Spins(_) => "then spins".to_string(),
            End::Returns(_) => "then returns".to_string(),
            End::RunsOff(_) => match last_call {
                Some(last) => format!("then calls {}, which doesn't return", firmware.name(last)),
                None => "then stops".to_string(),
            },
            End::Loops(site) => {
                let head = loop_head(answer.facts.events.list, site);
                format!("then loops for good at {:#x}", head.unwrap_or(site))
            }
            End::Rejoins(at) => format!("then runs a loop elfq can't follow at {at:#x}"),
            End::Budget(site) => format!("then goes on past elfq's budget at {site:#x}"),
            End::Main(_) | End::Undecoded(_) | End::Branches { .. } | End::LeavesFile { .. } => {
                format!("then {}", other_way(end))
            }
        },
    }
}

/// `main`'s name, as the lead gives it.
fn main_text(firmware: &Firmware, main: Main) -> String {
    match main.from {
        MainFrom::DebugInfo => "main".to_string(),
        MainFrom::Guess => format!("{} (taken as main)", firmware.name(main.address)),
        MainFrom::Symbol | MainFrom::Loop => firmware.name(main.address),
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
    let firmware = answer.facts.firmware;
    let ram = answer.facts.ram;
    let steps = answer.steps;
    let Some(&section) = p.sections.first() else {
        return String::new();
    };
    let name = ram.name(section);
    let size = ram.section(section).size;
    // The number of the step it is about.
    let step = p.steps.first().map_or(0, |&index| number(index));
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
            let reset_runs_from_image = p.uses.contains(&RunsAt::ResetHandler);
            let tail = match p.before {
                None if answer.judged.unread.contains_key(&section) => {
                    format!("before anything copies {name}")
                }
                None => "which nothing copies".to_string(),
                Some(index) => {
                    let before = &steps[index];
                    let inside = (p.uses.iter()).filter_map(|at| at.event()).any(|event| {
                        event < before.event && before.event < answer.facts.events.span(event)
                    });
                    if reset_runs_from_image || inside || p.steps.contains(&index) {
                        "which nothing has copied yet".to_string()
                    } else {
                        format!("before step {} copies {name}", number(index))
                    }
                }
            };
            if reset_runs_from_image {
                let boot = answer.facts.reset.address;
                return format!(
                    "{} runs at {boot:#x} in {name}, {tail}{}.",
                    firmware.name(boot),
                    unless(answer, p)
                );
            }
            let direct = p.items.iter().all(|a| {
                let chain = &p.chains[a];
                (chain.iter()).filter(|&&c| !firmware.is_veneer(c)).count() <= 2
            });
            let verb = match (p.steps.len() == 1, direct) {
                (true, true) => "calls",
                (true, false) => "runs",
                (false, true) => "call",
                (false, false) => "run",
            };
            let mut who = steps_text(&p.steps, true);
            if let [one] = p.steps[..]
                && let Some(function) = steps[one].called()
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
        Kind::NotCopied | Kind::NotZeroed => format!("Nothing {}.", set_up_text(ram, section)),
        Kind::PartlyCopied => format!(
            "Step {step} copies {} of {name}'s {size} bytes.",
            p.bytes.unwrap_or(0),
        ),
        Kind::PartlyZeroed => format!(
            "Step {step} zeroes {} of {name}'s {size} bytes.",
            p.bytes.unwrap_or(0),
        ),
        Kind::CopiedFromElsewhere => format!(
            "Step {step} copies {name} from {:#x}, but the file stores it at {:#x}.",
            p.from.unwrap_or(0),
            ram.load_address(section)
        ),
        Kind::CantFollow => {
            // The steps may set up what the problem says nothing does.
            let what = match p.sections[..] {
                [section] => set_up_text(ram, section),
                _ => does_text(ram, &p.sections, ["copies", "zeroes"], " or "),
            };
            let verb = if p.steps.len() == 1 { "does" } else { "do" };
            format!(
                "Nothing {what}, unless {} {verb}.",
                unread_text(steps, &p.steps)
            )
        }
    }
}

/// For code that runs from a load image after code elfq can't follow,
/// which may copy it first: ", unless step 1 (a loop) copies it first".
fn unless(answer: &Answer, p: &Problem) -> String {
    if p.unless.is_empty() {
        return String::new();
    }
    format!(
        ", unless {} copies it first",
        unread_text(answer.steps, &p.unless)
    )
}

/// The cells of the row of `step`, at `index` in the steps.
fn row(answer: &Answer, index: usize, step: &Step) -> Vec<String> {
    let firmware = answer.facts.firmware;
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
            let names: Vec<&str> = (step.covers().iter())
                .map(|cover| answer.facts.ram.name(cover.section))
                .collect();
            let names = names_text(&names, answer.all);
            let what = match step.way {
                Way::Loop => format!("{names}, by a loop in {function}"),
                Way::Store => format!("{names}, by a store in {function}"),
                // A call whose function the walk can't tell, which passes
                // what a copy or a zeroing of the sections would.
                Way::RegisterCall(register) => format!(
                    "{names}, by a call through {} in {}",
                    register_name(register),
                    name(step.inside)
                ),
                Way::SupervisorCall => {
                    format!("{names}, by a supervisor call in {}", name(step.inside))
                }
                _ => in_inside(format!("{names}, by {function}{via}")),
            };
            match step.table {
                Some(table) => format!("{what}, from table {table:#x}"),
                None => what,
            }
        }
        Does::Call if step.way == Way::Inlined => {
            format!("main, inlined into {}", name(step.inside))
        }
        Does::Call => {
            let guessed = step.main
                && answer
                    .facts
                    .main
                    .is_some_and(|main| main.from == MainFrom::Guess);
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
                End::Branches { through, .. } => {
                    format!("{} in {function}", branch_noun(through).one)
                }
                End::LeavesFile { .. } => format!("a jump out of the file in {function}"),
                End::Undecoded(_) => format!("code in {function} that elfq can't decode"),
                End::Rejoins(_) => format!("a loop in {function}"),
                _ => format!("code in {function} past elfq's budget"),
            },
            Way::Call | Way::TailCall | Way::Inlined | Way::Store => function,
        },
    };
    let hex = |value: Option<u64>| value.map_or(String::new(), |value| format!("{value:#x}"));
    let region = step.region.as_ref();
    vec![
        number(index).to_string(),
        step.does.word().to_string(),
        hex(region.and_then(|region| region.from)),
        hex(region.map(|region| region.to)),
        region.map_or(String::new(), |region| region.size.to_string()),
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
/// how the call gets there, each given without `elfq FILE`.
fn try_commands(answer: &Answer) -> Vec<String> {
    let Facts { firmware, ram, .. } = *answer.facts;
    let mut lines = Vec::new();
    for p in &answer.judged.problems {
        // Code that runs inside a call, but not the reset handler's own.
        let (Kind::RunsBeforeCopy, Some(&first)) = (p.kind, p.steps.first()) else {
            continue;
        };
        if p.uses.contains(&RunsAt::ResetHandler) {
            continue;
        }
        let step = &answer.steps[first];
        let section = p.sections[0];
        let Section { address, size, .. } = ram.section(section);
        let Some(function) = step.called() else {
            continue;
        };
        if p.items.contains(&function) || (*address..ram.end(section)).contains(&function) {
            continue;
        }
        let line = format!(
            "reach {} --to {address:#x}+{size}",
            shell_quote(&firmware.name(function)),
        );
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// The steps at `indexes` in the steps, by number: "Step 3", "Steps 2 and
/// 5", "Steps 6 to 24", or for many runs, "40 steps from 2 to 90".
pub(super) fn steps_text(indexes: &[usize], capital: bool) -> String {
    let mut numbers: Vec<usize> = indexes.iter().map(|&index| number(index)).collect();
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

/// The steps at `indexes` in `steps`, of code elfq can't follow, with what
/// kinds of code they are: "step 3 (a loop)", "steps 2 and 5 (a loop and
/// a call through r3)".
pub(super) fn unread_text(steps: &[Step], indexes: &[usize]) -> String {
    let unread: Vec<&Step> = indexes.iter().map(|&index| &steps[index]).collect();
    let loops = (unread.iter())
        .filter(|step| matches!(step.way, Way::Loop | Way::Stop(End::Rejoins(_))))
        .count();
    let registers: Vec<u16> = unread
        .iter()
        .filter_map(|step| step.way.register())
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
        let noun = |one: &str, many| Noun {
            one: one.to_string(),
            many,
        };
        let Noun { one, many } = match step.way {
            Way::Outside(_) => noun("a call outside the file", "calls outside the file"),
            Way::Stop(end) => match end {
                End::Branches { through, .. } => branch_noun(through),
                End::LeavesFile { .. } => noun("a jump out of the file", "jumps out of the file"),
                End::Undecoded(_) => noun("code elfq can't decode", "code elfq can't decode"),
                End::Rejoins(_) => continue,
                _ => noun("code past elfq's budget", "code past elfq's budget"),
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
        steps_text(indexes, false),
        names_text(&kinds, true)
    )
}

/// A kind of code, as one of it and as several: "a jump table" and "jump
/// tables".
struct Noun {
    one: String,
    many: &'static str,
}

/// A branch that the walk can't follow, by what it goes through: "a branch
/// through r3", and for several, "branches through registers".
fn branch_noun(through: Through) -> Noun {
    match through {
        Through::Register(register) => Noun {
            one: format!("a branch through {}", register_name(register)),
            many: "branches through registers",
        },
        Through::Memory => Noun {
            one: "a branch through a word in memory".to_string(),
            many: "branches through words in memory",
        },
        Through::Table => Noun {
            one: "a jump table".to_string(),
            many: "jump tables",
        },
    }
}

/// How startup code sets up `section`, with where it is: "copies .data, 4
/// bytes at 0x20000000 stored at 0x8001000" or "zeroes .bss, 56 bytes at
/// 0x20000004".
fn set_up_text(ram: &RamSections, section: usize) -> String {
    let Section {
        name,
        size,
        address,
        ..
    } = ram.section(section);
    match ram.is_load(section) {
        true => format!(
            "copies {name}, {size} bytes at {address:#x} stored at {:#x}",
            ram.load_address(section)
        ),
        false => format!("zeroes {name}, {size} bytes at {address:#x}"),
    }
}

/// What a copy and a zeroing, by the `verbs` that say so, do to `sections`,
/// joined by `joiner`: "copies .data or zeroes .bss".
fn does_text(ram: &RamSections, sections: &[usize], verbs: [&str; 2], joiner: &str) -> String {
    let mut parts = Vec::new();
    for (verb, load) in verbs.into_iter().zip([true, false]) {
        let names: Vec<&str> = (sections.iter().copied())
            .filter(|&section| ram.is_load(section) == load)
            .map(|section| ram.name(section))
            .collect();
        if !names.is_empty() {
            parts.push(format!("{verb} {}", names_text(&names, true)));
        }
    }
    parts.join(joiner)
}

/// What code elfq can't follow may do to `sections`: "copy .data and zero
/// .bss".
pub(super) fn may_text(ram: &RamSections, sections: &[usize]) -> String {
    does_text(ram, sections, ["copy", "zero"], " and ")
}

/// How the way the walk didn't take ended, for the note on a branch it
/// took instead.
pub(super) fn other_way(end: End) -> String {
    let site = end.site();
    match end {
        End::Branches { through, .. } => {
            format!("ends in {} at {site:#x}", branch_noun(through).one)
        }
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
        // By index, so that the step at 2 is step 3.
        assert_eq!(steps_text(&[2], true), "Step 3");
        assert_eq!(steps_text(&[4, 1], false), "steps 2 and 5");
        assert_eq!(steps_text(&[3, 1, 2, 2], false), "steps 2 to 4");
        assert_eq!(
            steps_text(&[0, 1, 3, 5, 6, 7], true),
            "Steps 1, 2, 4 and 6 to 8"
        );
        // Past four parts, the JSON lists them.
        assert_eq!(
            steps_text(&[0, 2, 4, 6, 8, 9, 10], false),
            "7 steps from 1 to 11"
        );
        assert_eq!(steps_text(&[], false), "");
    }

    #[test]
    fn unread_steps_say_what_kind_of_code_each_is() {
        let step = Step::unread_for_tests;
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
        let text = |indexes: &[usize]| unread_text(&steps, indexes);
        assert_eq!(text(&[0]), "step 1 (a loop)");
        assert_eq!(
            text(&[0, 1]),
            "steps 1 and 2 (a loop and a call through r3)"
        );
        assert_eq!(
            text(&[0, 2, 3]),
            "steps 1, 3 and 4 (loops and a call deeper than elfq follows)"
        );
        // Branches through the same register name it once.
        assert_eq!(text(&[4, 5]), "steps 5 and 6 (branches through r2)");
        assert_eq!(text(&[4, 6]), "steps 5 and 7 (branches through registers)");
        assert_eq!(
            text(&[7, 8, 9]),
            "steps 8 to 10 (a supervisor call, a conditional call and a call outside the file)"
        );
    }

    #[test]
    fn what_is_done_to_sections_names_copies_then_zeroes() {
        use object::SectionKind;

        use crate::binary::{Binary, Section};

        let section = |name: &str, address, kind, load_address: Option<u64>| Section {
            name: name.to_string(),
            address,
            size: 0x10,
            kind,
            allocated: true,
            file_range: (kind != SectionKind::UninitializedData).then_some((0x1000, 0x10)),
            entry_size: 0,
            load_address,
        };
        let bss = SectionKind::UninitializedData;
        let binary = Binary::for_tests(
            vec![
                section(".text", 0x800_0000, SectionKind::Text, None),
                section(".data", 0x2000_0000, SectionKind::Data, Some(0x800_0010)),
                section(".bss", 0x2000_0010, bss, None),
                section(".sbss", 0x2000_0020, bss, None),
            ],
            Vec::new(),
            Vec::new(),
        );
        let ram = RamSections::new(&binary);
        let named =
            |name: &str| (ram.sections.iter().copied()).find(|&section| ram.name(section) == name);
        let (data, bss, sbss) = (
            named(".data").unwrap(),
            named(".bss").unwrap(),
            named(".sbss").unwrap(),
        );
        let nothing = |sections: &[usize]| does_text(&ram, sections, ["copies", "zeroes"], " or ");
        assert_eq!(nothing(&[data]), "copies .data");
        assert_eq!(nothing(&[bss, sbss]), "zeroes .bss and .sbss");
        assert_eq!(
            nothing(&[sbss, data, bss]),
            "copies .data or zeroes .sbss and .bss"
        );
        assert_eq!(may_text(&ram, &[data, bss]), "copy .data and zero .bss");
    }
}
