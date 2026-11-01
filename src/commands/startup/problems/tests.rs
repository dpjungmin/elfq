use super::super::fixtures::{binary, called, with_facts};
use super::super::steps::{build, number};
use super::*;
use crate::arch::startup::{Call, How};
use crate::binary::Binary;

/// Where .ramfunc is, which code can run from before it is copied.
const RAMFUNC: u64 = 0x2000_0020;

/// What `judge` makes of the steps of startup code whose walk found
/// `list`, one a line: the state of each RAM section, as ".data copied
/// by step 1, 32 bytes"; each problem, as "problem partly zeroed .bss,
/// steps 3, 32 bytes"; and each note, as "note not zeroed .noinit".
/// The walks have no vector table, which every answer would note
/// first, so that note is left out.
fn judged(binary: &Binary, list: &[Event]) -> Vec<String> {
    with_facts(binary, list, End::Returns(0x800_0100), |facts| {
        let steps = build(facts);
        let judged = judge(facts, &steps);
        let ram = facts.ram;
        let statuses =
            (ram.sections.iter()).map(|&section| status_line(ram, section, judged.status(section)));
        let problems = (judged.problems.iter()).map(|p| problem_line(ram, p));
        let notes = (judged.notes.iter())
            .filter(|note| !matches!(note, Note::NoVectorTable))
            .map(|note| format!("note {} {}", note.field(), names(ram, note.sections())));
        statuses.chain(problems).chain(notes).collect()
    })
}

/// The state of `section`, as ".data copied by step 1, 32 bytes".
fn status_line(ram: &RamSections, section: usize, status: SectionStatus) -> String {
    let mut line = format!("{} {}", ram.name(section), status.status.field());
    if let Some(step) = status.step {
        line += &format!(" by step {}", number(step));
    }
    if let Some(bytes) = status.bytes {
        line += &format!(", {bytes} bytes");
    }
    line
}

/// Problem `p`, as "problem runs before copy .ramfunc, steps 2, running
/// 0x20000020, unless steps 1, not certain".
fn problem_line(ram: &RamSections, p: &Problem) -> String {
    let mut line = format!("problem {} {}", p.field(), names(ram, p.sections()));
    if !p.steps().is_empty() {
        line += &format!(", steps {}", numbers(p.steps()));
    }
    match p {
        Problem::RunsBeforeCopy(run) => {
            let functions: Vec<String> = (run.functions.iter())
                .map(|early| format!("{:#x}", early.function))
                .collect();
            line += &format!(", running {}", functions.join(" "));
            if !run.unless.is_empty() {
                line += &format!(", unless steps {}", numbers(&run.unless));
            }
        }
        Problem::PartlyCopied { bytes, .. } | Problem::PartlyZeroed { bytes, .. } => {
            line += &format!(", {bytes} bytes");
        }
        Problem::CopiedFromElsewhere { from, .. } => line += &format!(", from {from:#x}"),
        Problem::NotCopied { .. } | Problem::NotZeroed { .. } | Problem::CantFollow { .. } => {}
    }
    if !p.certain() {
        line += ", not certain";
    }
    line
}

/// The names of `sections`, as ".data .bss".
fn names(ram: &RamSections, sections: &[usize]) -> String {
    let names: Vec<&str> = sections.iter().map(|&section| ram.name(section)).collect();
    names.join(" ")
}

/// The numbers of the steps at `indexes`, as "1 3".
fn numbers(indexes: &[usize]) -> String {
    let numbers: Vec<String> = (indexes.iter())
        .map(|&index| number(index).to_string())
        .collect();
    numbers.join(" ")
}

/// A call at `site` in the reset handler, through `how`, to `target`,
/// which passes nothing that the walk can tell, and which the walk goes
/// through.
fn call(site: u64, target: Option<u64>, how: How) -> Call {
    Call {
        site,
        depth: 0,
        target,
        how,
        arguments: [None; 4],
        left: 0,
        conditional: false,
        followed: true,
    }
}

/// A call at `site` to the function at `target`.
fn call_to(site: u64, target: u64) -> Event {
    Event::Call(call(site, Some(target), How::Direct))
}

/// A call at `site` through r3, which the walk can't tell: code that
/// elfq can't follow.
fn through_register(site: u64) -> Event {
    Event::Call(call(site, None, How::Register(3)))
}

/// A call at `site` that copies .data, all 32 bytes, from where the
/// file stores it.
fn copy_data(site: u64) -> Event {
    called(site, [0x2000_0000, 0x800_0400, 0x20])
}

/// A call at `site` that copies .ramfunc, all 16 bytes, from `from`.
fn copy_ramfunc(site: u64, from: u32) -> Event {
    called(site, [0x2000_0020, from, 0x10])
}

/// A call at `site` that zeroes `size` bytes of .bss.
fn zero_bss(site: u64, size: u32) -> Event {
    called(site, [0x2000_0030, 0, size])
}

/// A call at `site` that zeroes `size` bytes of .noinit.
fn zero_noinit(site: u64, size: u32) -> Event {
    called(site, [0x2000_0070, 0, size])
}

#[test]
fn a_load_image_nothing_copies_is_not_copied_unless_code_runs_from_it() {
    let binary = binary();
    let list = [copy_data(0x800_0010), zero_bss(0x800_0014, 0x40)];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data copied by step 1, 32 bytes",
            ".ramfunc not copied, 0 bytes",
            ".bss zeroed by step 2, 64 bytes",
            ".noinit not zeroed, 0 bytes",
            "problem not copied .ramfunc",
            "note not zeroed .noinit",
        ]
    );
    // Code that runs from .ramfunc names it, and goes wrong first: no
    // step after it can be counted on.
    let list = [
        copy_data(0x800_0010),
        call_to(0x800_0014, RAMFUNC),
        zero_bss(0x800_0018, 0x40),
    ];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data copied by step 1, 32 bytes",
            ".ramfunc not copied, 0 bytes",
            ".bss not reached by step 3, 0 bytes",
            ".noinit not zeroed, 0 bytes",
            "problem runs before copy .ramfunc, steps 2, running 0x20000020",
            "note not reached .bss",
            "note not zeroed .noinit",
        ]
    );
}

#[test]
fn code_that_runs_before_its_copy_after_unread_code_that_may_copy_it_is_not_certain() {
    let binary = binary();
    let set_up = |site| {
        [
            copy_data(site),
            copy_ramfunc(site + 4, 0x800_0420),
            zero_bss(site + 8, 0x40),
            zero_noinit(site + 12, 0x10),
        ]
    };
    // The call through r3 may copy .ramfunc before code runs from it,
    // so nothing after it is lost.
    let mut list = vec![through_register(0x800_0010), call_to(0x800_0014, RAMFUNC)];
    list.extend(set_up(0x800_0018));
    assert_eq!(
        judged(&binary, &list),
        [
            ".data copied by step 3, 32 bytes",
            ".ramfunc copied by step 4, 16 bytes",
            ".bss zeroed by step 5, 64 bytes",
            ".noinit zeroed by step 6, 16 bytes",
            "problem runs before copy .ramfunc, steps 2, running 0x20000020, \
             unless steps 1, not certain",
        ]
    );
    // So may a call into it that the walk doesn't go through.
    let list = [Event::Call(Call {
        conditional: true,
        followed: false,
        ..call(0x800_0010, Some(RAMFUNC), How::Direct)
    })];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data unknown",
            ".ramfunc unknown",
            ".bss unknown",
            ".noinit unknown",
            "problem runs before copy .ramfunc, steps 1, running 0x20000020, \
             unless steps 1, not certain",
            "problem can't follow .data .ramfunc .bss, steps 1, not certain",
            "note can't tell .noinit",
        ]
    );
    // After the code that runs from it, the call can't copy it first.
    let mut list = vec![call_to(0x800_0010, RAMFUNC), through_register(0x800_0014)];
    list.extend(set_up(0x800_0018));
    assert_eq!(
        judged(&binary, &list),
        [
            ".data not reached by step 3, 0 bytes",
            ".ramfunc not reached by step 4, 0 bytes",
            ".bss not reached by step 5, 0 bytes",
            ".noinit not reached by step 6, 0 bytes",
            "problem runs before copy .ramfunc, steps 1, running 0x20000020",
            "note not reached .data .ramfunc .bss .noinit",
        ]
    );
}

#[test]
fn setups_after_the_first_certain_problem_are_not_reached() {
    let binary = binary();
    // What only unread code after the problem may set up isn't reached
    // either, and isn't a section that elfq can't tell about.
    let list = [
        call_to(0x800_0010, RAMFUNC),
        copy_data(0x800_0014),
        through_register(0x800_0018),
    ];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data not reached by step 2, 0 bytes",
            ".ramfunc not reached, 0 bytes",
            ".bss not reached, 0 bytes",
            ".noinit not reached, 0 bytes",
            "problem runs before copy .ramfunc, steps 1, running 0x20000020",
            "note not reached .data .ramfunc .bss .noinit",
        ]
    );
    // A step that may never run is still a problem when it copies short.
    let list = [
        call_to(0x800_0010, RAMFUNC),
        called(0x800_0014, [0x2000_0000, 0x800_0400, 0x10]),
    ];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data not reached by step 2, 0 bytes",
            ".ramfunc not copied, 0 bytes",
            ".bss not zeroed, 0 bytes",
            ".noinit not zeroed, 0 bytes",
            "problem runs before copy .ramfunc, steps 1, running 0x20000020",
            "problem partly copied .data, steps 2, 16 bytes",
            "problem not zeroed .bss",
            "note not reached .data",
            "note not zeroed .noinit",
        ]
    );
}

#[test]
fn a_copy_by_code_that_runs_before_its_own_copy_may_never_run() {
    let binary = binary();
    // The call that copies .data goes to a function in .ramfunc.
    let Event::Call(copy) = copy_data(0x800_0010) else {
        unreachable!("copy_data makes a call");
    };
    let list = [Event::Call(Call {
        target: Some(RAMFUNC),
        ..copy
    })];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data not reached by step 1, 0 bytes",
            ".ramfunc not copied, 0 bytes",
            ".bss not zeroed, 0 bytes",
            ".noinit not zeroed, 0 bytes",
            "problem runs before copy .ramfunc, steps 1, running 0x20000020",
            "problem not zeroed .bss",
            "note not reached .data",
            "note not zeroed .noinit",
        ]
    );
}

#[test]
fn partly_zeroed_noinit_is_a_note_and_partly_zeroed_bss_a_problem() {
    let binary = binary();
    let list = [
        copy_data(0x800_0010),
        copy_ramfunc(0x800_0014, 0x800_0420),
        zero_bss(0x800_0018, 0x20),
        zero_noinit(0x800_001c, 8),
    ];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data copied by step 1, 32 bytes",
            ".ramfunc copied by step 2, 16 bytes",
            ".bss partly zeroed by step 3, 32 bytes",
            ".noinit partly zeroed by step 4, 8 bytes",
            "problem partly zeroed .bss, steps 3, 32 bytes",
            "note partly zeroed .noinit",
        ]
    );
}

#[test]
fn a_load_image_copied_short_or_from_elsewhere_is_a_problem() {
    let binary = binary();
    let list = [
        called(0x800_0010, [0x2000_0000, 0x800_0400, 0x10]),
        copy_ramfunc(0x800_0014, 0x800_0100),
        zero_bss(0x800_0018, 0x40),
    ];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data partly copied by step 1, 16 bytes",
            ".ramfunc copied from elsewhere by step 2, 16 bytes",
            ".bss zeroed by step 3, 64 bytes",
            ".noinit not zeroed, 0 bytes",
            "problem partly copied .data, steps 1, 16 bytes",
            "problem copied from elsewhere .ramfunc, steps 2, from 0x8000100",
            "note not zeroed .noinit",
        ]
    );
}

#[test]
fn unread_code_is_a_problem_for_what_must_be_set_up_and_a_note_for_the_rest() {
    let binary = binary();
    let list = [copy_data(0x800_0010), through_register(0x800_0014)];
    assert_eq!(
        judged(&binary, &list),
        [
            ".data copied by step 1, 32 bytes",
            ".ramfunc unknown",
            ".bss unknown",
            ".noinit unknown",
            "problem can't follow .ramfunc .bss, steps 2, not certain",
            "note can't tell .noinit",
        ]
    );
}
