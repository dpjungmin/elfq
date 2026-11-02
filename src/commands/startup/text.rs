//! The text of `startup`: a line that says whether there are problems,
//! a sentence for each, the steps in a table, notes, and what to try. The
//! JSON gives the same sentences for the problems and the notes.

use std::io::Write;
use std::ops::Range;

use anyhow::Result;

use super::problems::{EarlyRun, Note, Problem, Status};
use super::ram_sections::RamSections;
use super::start::{Main, MainFrom};
use super::steps::{Does, Step, Way, number};
use super::{Answer, Ending, Facts, Firmware};
use crate::arch::startup::{End, Through, loop_head};
use crate::binary::{ItemKind, Section};
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
            writeln!(out, "{}", note_text(answer, note))?;
        }
    }
    house::write_try(answer.path, &try_commands(answer), out)
}

/// The first line: how many problems, or what startup does when there are
/// none.
fn lead(answer: &Answer) -> String {
    let boot = answer.facts.firmware.name(answer.facts.reset.address);
    let ending = answer.facts.ending();
    let problems = answer.judged.problems.len();
    // Without startup code, as when the entry point is main.
    if let Ending::EntryIsMain(_) = ending {
        let main = boot;
        return match problems {
            0 => format!(
                "No problems: the processor starts at {main}, so no startup code runs before it."
            ),
            n => format!(
                "{} before {main}, where the processor starts:",
                plural(n, "problem", "problems")
            ),
        };
    }
    if problems > 0 {
        return format!(
            "{} in what {boot} does{}:",
            plural(problems, "problem", "problems"),
            before_text(answer.facts.firmware, ending)
        );
    }
    if answer.facts.ram.sections.is_empty() {
        return format!(
            "{boot} has nothing to copy or zero: the file stores every section where it runs, \
             and none starts as zeros."
        );
    }
    lead_without_problems(answer, &boot, ending)
}

/// The lead when there are no problems and there is something to set up:
/// what the steps copy and zero, then how startup ends, or what the reset
/// handler's last call does when it doesn't return.
fn lead_without_problems(answer: &Answer, boot: &str, ending: Ending) -> String {
    let last = last_call(answer, ending);
    if let Some(LastCall { event, function }) = last {
        let before = set_up_parts(answer, 0..event);
        let inside = set_up_parts(answer, event..answer.facts.events.list.len());
        if !inside.is_empty() {
            let inside = format!("{} and", inside.join(", "));
            let before = match before.is_empty() {
                true => String::new(),
                false => format!("{}, then ", before.join(", ")),
            };
            return format!(
                "No problems: {boot} {before}calls {}, which {inside} doesn't return.",
                answer.facts.firmware.name(function)
            );
        }
    }
    let parts = set_up_parts(answer, 0..usize::MAX);
    let what = if parts.is_empty() {
        "copies and zeroes nothing".to_string()
    } else {
        parts.join(", ")
    };
    let then = then_text(answer, ending, last.map(|last| last.function));
    format!("No problems: {boot} {what}, {then}.")
}

/// The last call that the reset handler makes (see [`last_call`]).
#[derive(Clone, Copy)]
struct LastCall {
    /// Where the call is in the walk's list of events.
    event: usize,
    /// The function that it calls, past a veneer.
    function: u64,
}

/// The last call that the reset handler makes, when the walk runs off the
/// end of its code after it, so that the call doesn't return: it may do
/// the copies and zeroings itself, as Zephyr's z_prep_c does.
fn last_call(answer: &Answer, ending: Ending) -> Option<LastCall> {
    let Ending::Walk(End::RunsOff(_)) = ending else {
        return None;
    };
    let events = answer.facts.events;
    (0..events.list.len())
        .rev()
        .filter(|&event| events.level(event) == 0)
        .find_map(|event| {
            let function = events.target(event)?;
            Some(LastCall { event, function })
        })
}

/// What the steps at `events` copy and zero in full, in the order that the
/// steps set the sections up: "copies .data", "zeroes .bss and .sbss".
fn set_up_parts(answer: &Answer, events: Range<usize>) -> Vec<String> {
    let ram = answer.facts.ram;
    let init = &answer.judged.setups.init;
    let mut ordered = ram.sections.clone();
    ordered.sort_by_key(|section| {
        init.get(section)
            .map_or(answer.steps.len(), |setup| setup.step)
    });
    let by = |section: usize| {
        (init.get(&section)).is_some_and(|setup| events.contains(&answer.steps[setup.step].event))
    };
    let mut parts = Vec::new();
    for (verb, wanted) in [("copies", Status::Copied), ("zeroes", Status::Zeroed)] {
        let names: Vec<&str> = (ordered.iter().copied())
            .filter(|&section| answer.judged.status(section).status == wanted && by(section))
            .map(|section| ram.name(section))
            .collect();
        if !names.is_empty() {
            parts.push(format!("{verb} {}", names_text(&names, true)));
        }
    }
    parts
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
    let ram = answer.facts.ram;
    match p {
        Problem::RunsBeforeCopy(run) => runs_before_copy_text(answer, run),
        Problem::NotCopied { section } | Problem::NotZeroed { section } => {
            format!("Nothing {}.", set_up_text(ram, *section))
        }
        &Problem::PartlyCopied {
            section,
            step,
            bytes,
        } => format!(
            "Step {} copies {bytes} of {}'s {} bytes.",
            number(step),
            ram.name(section),
            ram.section(section).size
        ),
        &Problem::PartlyZeroed {
            section,
            step,
            bytes,
        } => format!(
            "Step {} zeroes {bytes} of {}'s {} bytes.",
            number(step),
            ram.name(section),
            ram.section(section).size
        ),
        &Problem::CopiedFromElsewhere {
            section,
            step,
            from,
        } => format!(
            "Step {} copies {} from {from:#x}, but the file stores it at {:#x}.",
            number(step),
            ram.name(section),
            ram.load_address(section)
        ),
        Problem::CantFollow { sections, steps } => {
            // The steps may set up what the problem says nothing does.
            let what = match sections[..] {
                [section] => set_up_text(ram, section),
                _ => does_text(ram, sections, ["copies", "zeroes"], " or "),
            };
            let verb = if steps.len() == 1 { "does" } else { "do" };
            format!(
                "Nothing {what}, unless {} {verb}.",
                unread_text(answer.steps, steps)
            )
        }
    }
}

/// The sentence of `run`, code that runs from a load image before startup
/// code copies it: "Step 1, SystemInit, calls clock_setup at 0x0 in .itcm
/// before step 2 copies .itcm."
fn runs_before_copy_text(answer: &Answer, run: &EarlyRun) -> String {
    let firmware = answer.facts.firmware;
    let name = answer.facts.ram.name(run.section);
    let copied = copied_yet_text(answer, run);
    if run.reset_runs_from_image() {
        let boot = answer.facts.reset.address;
        return format!(
            "{} runs at {boot:#x} in {name}, {copied}{}.",
            firmware.name(boot),
            unless(answer, run)
        );
    }
    let what = match &run.functions[..] {
        [one] => at_name(firmware, one.function),
        functions => {
            let names: Vec<String> = (functions.iter())
                .map(|early| firmware.name(early.function))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            names_text(&names, answer.all)
        }
    };
    let direct = run.functions.iter().all(|early| {
        let through = early.chain.iter().filter(|&&c| !firmware.is_veneer(c));
        through.count() <= 2
    });
    let verb = match (run.steps.len() == 1, direct) {
        (true, true) => "calls",
        (true, false) => "runs",
        (false, true) => "call",
        (false, false) => "run",
    };
    let mut who = steps_text(&run.steps, true);
    if let [one] = run.steps[..]
        && let Some(function) = answer.steps[one].called()
        && !run.runs(function)
    {
        who = format!("{who}, {},", firmware.name(function));
    }
    let separator = if copied.starts_with("before") {
        " "
    } else {
        ", "
    };
    format!(
        "{who} {verb} {what} in {name}{separator}{copied}{}.",
        unless(answer, run)
    )
}

/// How far startup code has got with copying the load image of `run` when
/// code runs from it: "before step 3 copies .data", "before anything copies
/// .data", "which nothing has copied yet" or "which nothing copies".
fn copied_yet_text(answer: &Answer, run: &EarlyRun) -> String {
    let name = answer.facts.ram.name(run.section);
    match run.before {
        None if answer.judged.setups.unread.contains_key(&run.section) => {
            format!("before anything copies {name}")
        }
        None => "which nothing copies".to_string(),
        Some(index) => {
            let before = &answer.steps[index];
            let inside = (run.runs_at.iter())
                .filter_map(|at| at.event())
                .any(|event| {
                    event < before.event && before.event < answer.facts.events.span(event)
                });
            if run.reset_runs_from_image() || inside || run.steps.contains(&index) {
                "which nothing has copied yet".to_string()
            } else {
                format!("before step {} copies {name}", number(index))
            }
        }
    }
}

/// The sentence of note `note`.
pub(super) fn note_text(answer: &Answer, note: &Note) -> String {
    let Facts {
        firmware,
        ram,
        reset,
        ..
    } = *answer.facts;
    let boot = firmware.name(reset.address);
    match note {
        Note::NoVectorTable => {
            format!("elfq finds no vector table, so it reads from the entry point, {boot}.")
        }
        Note::ResetInNoFunction { table, outside } => format!(
            "The vector table at {table:#x} gives {outside:#x} for reset, which is in no \
             function, so elfq reads from the entry point, {boot}."
        ),
        Note::EntryDiffers { table } => format!(
            "The vector table at {table:#x} gives {boot} for reset, not the entry point {}.",
            firmware.name(firmware.binary.entry)
        ),
        Note::Branch { site, because } => format!(
            "At {site:#x} elfq takes the branch, since the other way {}.",
            other_way(*because)
        ),
        Note::NotReached { later, unnamed, .. } => {
            let rest: Vec<&str> = unnamed.iter().map(|&section| ram.name(section)).collect();
            let so = if rest.is_empty() {
                String::new()
            } else {
                format!(", so {} may not be set up", names_text(&rest, answer.all))
            };
            format!("{} may never run{so}.", steps_text(later, true))
        }
        Note::NotZeroed { sections } => {
            let parts: Vec<String> = (sections.iter())
                .map(|&section| {
                    let held = data_in(firmware, ram, section);
                    if held.is_empty() {
                        ram.name(section).to_string()
                    } else {
                        format!("{} ({})", ram.name(section), names_text(&held, answer.all))
                    }
                })
                .collect();
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            format!(
                "Not zeroed, which may be intended: {}.",
                names_text(&parts, true)
            )
        }
        Note::PartlyZeroed {
            section,
            step,
            bytes,
        } => format!(
            "Step {} zeroes {bytes} of {}'s {} bytes, which may be intended.",
            number(*step),
            ram.name(*section),
            ram.section(*section).size
        ),
        Note::CantTell { sections, steps } => format!(
            "elfq can't follow {}, which may {}.",
            unread_text(answer.steps, steps),
            may_text(ram, sections)
        ),
    }
}

/// The names of the data objects in `section`, by address, but for those
/// named after where they are.
fn data_in<'a>(firmware: &'a Firmware, ram: &RamSections, section: usize) -> Vec<&'a str> {
    let start = ram.section(section).address;
    (firmware.binary.items.iter())
        .filter(|item| {
            item.kind == ItemKind::Data
                && start <= item.address
                && item.address < ram.end(section)
                && !item.name.contains('@')
        })
        .map(|item| item.name.as_str())
        .collect()
}

/// For `run`, code that runs from a load image after code elfq can't
/// follow, which may copy it first: ", unless step 1 (a loop) copies it
/// first".
fn unless(answer: &Answer, run: &EarlyRun) -> String {
    if run.unless.is_empty() {
        return String::new();
    }
    format!(
        ", unless {} copies it first",
        unread_text(answer.steps, &run.unless)
    )
}

/// The cells of the row of `step`, at `index` in the steps.
fn row(answer: &Answer, index: usize, step: &Step) -> Vec<String> {
    let what = match step.does {
        Does::Copy | Does::Zero => set_up_what(answer, step),
        Does::Call => call_what(answer, step),
        Does::Unread => unread_what(answer.facts.firmware, step),
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

/// The WHAT of a copy or zeroing: the sections, and what does it, as
/// ".data, by memcpy" or ".bss, by a loop in Reset_Handler".
fn set_up_what(answer: &Answer, step: &Step) -> String {
    let firmware = answer.facts.firmware;
    let names: Vec<&str> = (step.covers().iter())
        .map(|cover| answer.facts.ram.name(cover.section))
        .collect();
    let names = names_text(&names, answer.all);
    let function = function_name(firmware, step);
    let inside = firmware.name(step.inside);
    let what = match step.way {
        Way::Loop => format!("{names}, by a loop in {function}"),
        Way::Store => format!("{names}, by a store in {function}"),
        // A call whose function the walk can't tell, which passes what a
        // copy or a zeroing of the sections would.
        Way::RegisterCall(register) => format!(
            "{names}, by a call through {} in {inside}",
            register_name(register)
        ),
        Way::SupervisorCall => format!("{names}, by a supervisor call in {inside}"),
        Way::Call
        | Way::TailCall
        | Way::Inlined
        | Way::Conditional
        | Way::Deep
        | Way::Outside(_)
        | Way::Stop(_) => with_inside(
            firmware,
            step,
            format!("{names}, by {function}{}", via(step)),
        ),
    };
    match step.table {
        Some(table) => format!("{what}, from table {table:#x}"),
        None => what,
    }
}

/// The WHAT of a call: the function it calls, or `main`'s code where a
/// compiler inlined it.
fn call_what(answer: &Answer, step: &Step) -> String {
    let firmware = answer.facts.firmware;
    if step.way == Way::Inlined {
        return format!("main, inlined into {}", firmware.name(step.inside));
    }
    let guessed = step.main
        && answer
            .facts
            .main
            .is_some_and(|main| main.from == MainFrom::Guess);
    let taken = if guessed { " (taken as main)" } else { "" };
    let function = function_name(firmware, step);
    with_inside(firmware, step, format!("{function}{}{taken}", via(step)))
}

/// The WHAT of code elfq can't follow: what kind of code it is, and where.
fn unread_what(firmware: &Firmware, step: &Step) -> String {
    let function = function_name(firmware, step);
    let inside = firmware.name(step.inside);
    match step.way {
        Way::Loop => format!("a loop in {function}"),
        Way::RegisterCall(register) => {
            format!("a call through {} in {inside}", register_name(register))
        }
        Way::SupervisorCall => format!("a supervisor call in {inside}"),
        Way::Conditional => format!("a conditional call to {function} in {inside}"),
        Way::Deep => format!("a call to {function} in {inside}, deeper than elfq follows"),
        Way::Outside(target) => format!("a call to {target:#x} in {inside}, outside the file"),
        Way::Stop(end) => match end {
            End::Branches { through, .. } => {
                format!("{} in {function}", branch_noun(through).one)
            }
            End::LeavesFile { .. } => format!("a jump out of the file in {function}"),
            End::Undecoded(_) => format!("code in {function} that elfq can't decode"),
            End::Rejoins(_) => format!("a loop in {function}"),
            // At elfq's budget: a step stops only where the walk stops
            // short (see `End::stops_short`), so the other ends never come.
            End::Budget(_)
            | End::Main(_)
            | End::Spins(_)
            | End::Returns(_)
            | End::Loops(_)
            | End::RunsOff(_) => format!("code in {function} past elfq's budget"),
        },
        Way::Call | Way::TailCall | Way::Inlined | Way::Store => function,
    }
}

/// The name of the function that `step` calls, or whose code holds it:
/// nothing when the walk can't tell.
fn function_name(firmware: &Firmware, step: &Step) -> String {
    (step.function)
        .map(|address| firmware.name(address))
        .unwrap_or_default()
}

/// " via veneer" for a call that goes through a veneer.
fn via(step: &Step) -> &'static str {
    match step.veneer {
        Some(_) => " via veneer",
        None => "",
    }
}

/// `what`, with the function whose code makes the call when that isn't
/// the reset handler's own: "memcpy, in init".
fn with_inside(firmware: &Firmware, step: &Step, what: String) -> String {
    if step.level > 0 {
        format!("{what}, in {}", firmware.name(step.inside))
    } else {
        what
    }
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
        let Problem::RunsBeforeCopy(run) = p else {
            continue;
        };
        let Some(&first) = run.steps.first() else {
            continue;
        };
        if run.reset_runs_from_image() {
            continue;
        }
        let step = &answer.steps[first];
        let Section { address, size, .. } = ram.section(run.section);
        let Some(function) = step.called() else {
            continue;
        };
        if run.runs(function) || (*address..ram.end(run.section)).contains(&function) {
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
fn steps_text(indexes: &[usize], capital: bool) -> String {
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
fn unread_text(steps: &[Step], indexes: &[usize]) -> String {
    let unread: Vec<&Step> = indexes.iter().map(|&index| &steps[index]).collect();
    let mut kinds = counted_kinds(&unread);
    kinds.extend(named_kinds(&unread));
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    format!(
        "{} ({})",
        steps_text(indexes, false),
        names_text(&kinds, true)
    )
}

/// The kinds of code of the `unread` steps that say only what they are,
/// each once, in the plural for more than one step: "a loop", "calls
/// through registers", "a conditional call".
fn counted_kinds(unread: &[&Step]) -> Vec<String> {
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
    kinds
}

/// The kinds of code of the `unread` steps where the walk stopped, or that
/// call outside the file, each once: as one step's kind for one step, and
/// in the plural for more, unless all are branches through one register.
fn named_kinds(unread: &[&Step]) -> Vec<String> {
    // Each kind in the plural, with what each of its steps is.
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for step in unread {
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
                // A loop, which counted_kinds counts.
                End::Rejoins(_) => continue,
                // At elfq's budget: a step stops only where the walk stops
                // short (see `End::stops_short`), so the other ends never
                // come.
                End::Budget(_)
                | End::Main(_)
                | End::Spins(_)
                | End::Returns(_)
                | End::Loops(_)
                | End::RunsOff(_) => noun("code past elfq's budget", "code past elfq's budget"),
            },
            // What counted_kinds counts.
            Way::Loop
            | Way::RegisterCall(_)
            | Way::SupervisorCall
            | Way::Conditional
            | Way::Deep => continue,
            // Never code elfq can't follow: a call that it can't follow
            // takes a way of its own (see `StepReader::on_call`), and
            // inlined code is `main`'s.
            Way::Call | Way::TailCall | Way::Inlined => continue,
            // Stores are code elfq can't follow only when a store through
            // a pointer that the walk can't tell meets their step (see
            // `Pieces::unread_store`). No kind here names them, so the
            // text gives such a step no kind.
            Way::Store => continue,
        };
        match groups.iter_mut().find(|(group, _)| *group == many) {
            Some((_, ones)) => ones.push(one),
            None => groups.push((many, vec![one])),
        }
    }
    let mut kinds = Vec::new();
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
            _ => kinds.push(many.to_string()),
        }
    }
    kinds
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
fn may_text(ram: &RamSections, sections: &[usize]) -> String {
    does_text(ram, sections, ["copy", "zero"], " and ")
}

/// How the way the walk didn't take ended, for the note on a branch it
/// took instead.
fn other_way(end: End) -> String {
    let site = end.site();
    match end {
        End::Branches { through, .. } => {
            format!("ends in {} at {site:#x}", branch_noun(through).one)
        }
        End::LeavesFile { .. } => format!("jumps out of the file at {site:#x}"),
        End::Undecoded(_) => format!("reaches code elfq can't decode at {site:#x}"),
        End::Spins(_) => format!("spins at {site:#x}"),
        End::Returns(_) => format!("returns at {site:#x}"),
        // The walk takes a branch only when the other way ends one of the
        // ways above (see `Event::Backtrack`), and then_text never passes
        // `End::Main`: an end at `main` is an ending of its own (see
        // `Facts::ending`).
        End::Main(_) | End::Loops(_) | End::Rejoins(_) | End::RunsOff(_) | End::Budget(_) => {
            format!("ends at {site:#x}")
        }
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
