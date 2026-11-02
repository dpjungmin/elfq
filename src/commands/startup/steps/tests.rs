use super::super::fixtures::{
    alone, binary, called, closed, copied, described, loop_with, steps, steps_to, walked_each,
    with_facts,
};
use super::*;
use crate::arch::startup::{Iteration, Loop, Runs};

/// A call at `site`, `depth` calls deep, through `how`, to `target`,
/// which passes nothing that the walk can tell, and which the walk
/// goes through.
fn call(site: u64, depth: u32, target: Option<u64>, how: How) -> Call {
    Call {
        site,
        depth,
        target,
        how,
        arguments: [None; 4],
        left: 0,
        conditional: false,
        followed: true,
    }
}

/// A step of code elfq can't follow, a loop at `site` in the reset
/// handler, at `event`, which may set up `may`.
fn unread(event: usize, site: u64, may: Vec<usize>) -> Step {
    Step {
        may,
        ..Step::new(
            event,
            site,
            Does::Unread,
            Way::Loop,
            Some(0x800_0000),
            0x800_0000,
            0,
        )
    }
}

/// As `unread`, a loop that copies .data.
fn copy(ram: &RamSections, event: usize, site: u64) -> Step {
    let mut step = unread(event, site, Vec::new());
    let numbers = [Some(0x2000_0000), Some(0x800_0400), Some(0x20), None];
    step.set_up(ram.match_call(&numbers).expect("a copy of .data"));
    step
}

/// `steps`, as [`described`] writes them.
fn written(ram: &RamSections, steps: &[Step]) -> Vec<String> {
    steps.iter().map(|step| described(ram, step)).collect()
}

#[test]
fn from_events_reads_each_event_as_a_step_or_none() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    let list = [
        // A call through r3, which the walk can't tell.
        Event::Call(call(0x800_0010, 0, None, How::Register(3))),
        called(0x800_0014, [0x2000_0000, 0x800_0400, 0x20]),
        // A call that neither copies nor zeroes, and a call in the
        // code of the function it calls, which is no step.
        Event::Call(call(0x800_0018, 0, Some(0x800_0300), How::Direct)),
        Event::Call(call(0x800_0304, 1, Some(0x800_0200), How::Direct)),
        // A conditional call, which the walk doesn't go through.
        Event::Call(Call {
            conditional: true,
            followed: false,
            ..call(0x800_001c, 0, Some(0x800_0200), How::Direct)
        }),
        // A call to code outside the file, as in a boot ROM, and one
        // to a function that goes on past the walk's budget: each
        // stops in the code of the function it calls.
        Event::Call(call(0x800_0020, 0, Some(0x1000_0000), How::Direct)),
        Event::Stop {
            site: 0x800_0020,
            depth: 1,
            end: End::LeavesFile {
                site: 0x800_0020,
                target: 0x1000_0000,
            },
        },
        Event::Call(call(0x800_0024, 0, Some(0x800_0300), How::Direct)),
        Event::Stop {
            site: 0x800_0024,
            depth: 1,
            end: End::Budget(0x800_0340),
        },
    ];
    let steps = with_facts(&binary, &list, End::Returns(0x800_0028), |facts| {
        from_events(facts, &Iterations::new(facts.events.list))
    });
    assert_eq!(
        written(&ram, &steps),
        [
            "unread 0x0 0 by a register call: .data, .ramfunc, .bss, .noinit",
            "copy 0x20000000 32 by a call: .data 32",
            "call 0x0 0 by a call: ",
            "unread 0x0 0 by a conditional call: .ramfunc, .bss, .noinit",
            "call 0x0 0 by a call: ",
            "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
            "call 0x0 0 by a call: ",
            "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
        ]
    );
    let call = &steps[2];
    assert_eq!(call.function, Some(0x800_0300));
    assert_eq!(call.span, Some(4));
    assert_eq!(steps[3].event, 4);
    // A stop's step is by the function that the call goes to, in the
    // code of the function that makes the call.
    let outside = &steps[5];
    assert_eq!(outside.way, Way::Outside(0x1000_0000));
    assert_eq!(
        (outside.function, outside.inside),
        (Some(0x1000_0000), 0x800_0000)
    );
    let budget = &steps[7];
    assert_eq!(budget.way, Way::Stop(End::Budget(0x800_0340)));
    assert_eq!(
        (budget.function, budget.inside),
        (Some(0x800_0300), 0x800_0000)
    );
}

#[test]
fn add_end_adds_code_elfq_cant_follow_where_the_walk_stops_short() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    let list = [called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20])];
    let ended = |end: End, main: Option<Main>| {
        with_facts(&binary, &list, end, |facts| {
            let mut steps = from_events(facts, &Iterations::new(facts.events.list));
            add_end(&Facts { main, ..*facts }, &mut steps);
            steps
        })
    };
    let steps = ended(End::Budget(0x800_0020), None);
    assert_eq!(
        written(&ram, &steps),
        [
            "copy 0x20000000 32 by a call: .data 32",
            "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit",
        ]
    );
    let stop = &steps[1];
    assert_eq!((stop.event, stop.site), (1, 0x800_0020));
    assert_eq!((stop.function, stop.inside), (Some(0x800_0000), 0x800_0000));
    // A walk that returns from the reset handler stops nowhere short.
    assert_eq!(ended(End::Returns(0x800_0020), None).len(), 1);
    // Where debug info says a compiler inlined `main`, it is a step.
    let inlined = Main {
        address: 0x800_0020,
        from: MainFrom::DebugInfo,
    };
    let steps = ended(End::Main(0x800_0020), Some(inlined));
    let main = &steps[1];
    assert!(main.main && main.does == Does::Call && main.way == Way::Inlined);
    assert_eq!((main.site, main.function), (0x800_0020, Some(0x800_0020)));
}

#[test]
fn narrow_unread_may_keeps_what_no_earlier_step_sets_up() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    let [data, _, bss, _] = ram.sections[..] else {
        panic!("four sections to set up");
    };
    let mut steps = vec![
        unread(0, 0x800_0010, vec![data, bss]),
        copy(&ram, 1, 0x800_0014),
        unread(2, 0x800_0018, vec![data, bss]),
        unread(3, 0x800_001c, vec![data]),
    ];
    narrow_unread_may(&mut steps);
    assert_eq!(
        written(&ram, &steps),
        [
            "unread 0x0 0 by a loop: .data, .bss",
            "copy 0x20000000 32 by a loop: .data 32",
            "unread 0x0 0 by a loop: .bss",
        ]
    );
}

#[test]
fn mark_table_rows_gives_a_copy_in_an_iteration_its_row() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    // One iteration, which reads its row at 0x8000500.
    let list = [
        called(0x800_0010, [0; 3]),
        Event::Loop(Loop {
            events: 0..1,
            runs: Runs::EachIteration(vec![Iteration {
                events: 0..1,
                row: Some(0x800_0500),
            }]),
            ..loop_with(Vec::new())
        }),
    ];
    let mut steps = vec![
        copy(&ram, 0, 0x800_0010),
        unread(0, 0x800_0010, vec![ram.sections[0]]),
        copy(&ram, 1, 0x800_0010),
    ];
    mark_table_rows(&Iterations::new(&list), &mut steps);
    let tables: Vec<Option<u64>> = steps.iter().map(|step| step.table).collect();
    assert_eq!(tables, [Some(0x800_0500), None, None]);
}

#[test]
fn merge_one_step_calls_makes_a_call_to_a_function_that_copies_the_copy() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    let copy_in = |site, numbers: [u32; 3]| {
        Event::Call(Call {
            arguments: [Some(numbers[0]), Some(numbers[1]), Some(numbers[2]), None],
            ..call(site, 1, Some(0x800_0200), How::Direct)
        })
    };
    let list = [
        // A function that copies .data and does nothing else.
        Event::Call(call(0x800_0010, 0, Some(0x800_0300), How::Direct)),
        copy_in(0x800_0304, [0x2000_0000, 0x800_0400, 0x20]),
        // One that makes another call before it copies .ramfunc.
        Event::Call(call(0x800_0014, 0, Some(0x800_0340), How::Direct)),
        Event::Call(call(0x800_0344, 1, Some(0x800_0380), How::Direct)),
        copy_in(0x800_0348, [0x2000_0020, 0x800_0420, 0x10]),
    ];
    let steps = with_facts(&binary, &list, End::Returns(0x800_0020), |facts| {
        let steps = from_events(facts, &Iterations::new(facts.events.list));
        merge_one_step_calls(facts, steps)
    });
    assert_eq!(
        written(&ram, &steps),
        [
            "copy 0x20000000 32 by a call: .data 32",
            "call 0x0 0 by a call: ",
            "copy 0x20000020 16 by a call: .ramfunc 16",
        ]
    );
    let merged = &steps[0];
    assert_eq!((merged.site, merged.inside), (0x800_0010, 0x800_0000));
    assert_eq!((merged.begins, merged.event, merged.level), (0, 1, 0));
    assert_eq!(merged.called(), Some(0x800_0300));
    assert_eq!(steps[2].level, 1);
}

#[test]
fn keep_once_per_table_loop_lists_what_each_iteration_does_once() {
    let binary = binary();
    let ram = RamSections::new(&binary);
    let [_, _, bss, noinit] = ram.sections[..] else {
        panic!("four sections to set up");
    };
    // Two iterations, of one event each.
    let list = [
        called(0x800_0010, [0; 3]),
        called(0x800_0010, [0; 3]),
        walked_each(&[0..1, 1..2]),
    ];
    let iterations = Iterations::new(&list);
    let call = |event| {
        Step::new(
            event,
            0x800_0010,
            Does::Call,
            Way::Call,
            Some(0x800_0200),
            0x800_0000,
            0,
        )
    };
    let steps = vec![
        call(0),
        unread(0, 0x800_0014, vec![noinit]),
        call(1),
        unread(1, 0x800_0014, vec![bss]),
    ];
    assert_eq!(
        written(&ram, &keep_once_per_table_loop(&ram, &iterations, steps)),
        [
            "call 0x0 0 by a call: ",
            "unread 0x0 0 by a loop: .bss, .noinit"
        ]
    );
    // A call that copies in one iteration isn't listed for the others.
    let steps = vec![call(0), copy(&ram, 1, 0x800_0010)];
    assert_eq!(
        written(&ram, &keep_once_per_table_loop(&ram, &iterations, steps)),
        ["copy 0x20000000 32 by a loop: .data 32"]
    );
}

#[test]
fn a_loop_that_elfq_cant_follow_in_each_time_may_set_up_what_it_may_any_time() {
    let binary = binary();
    // The first time stores past a pointer to .noinit in a loop, the
    // second past one to .bss.
    let row = |at: usize, base: u32| {
        [
            alone(0x800_0030, None, base, None, None),
            closed(Vec::new(), at..at + 1),
        ]
    };
    let mut list: Vec<Event> = (row(0, 0x2000_0070).into_iter())
        .chain(row(2, 0x2000_0030))
        .collect();
    list.push(walked_each(&[0..2, 2..4]));
    assert_eq!(
        steps(&binary, &list),
        ["unread 0x0 0 by a loop: .bss, .noinit"]
    );
}

#[test]
fn a_call_in_a_loop_over_a_table_that_sets_up_nothing_is_no_step() {
    let binary = binary();
    // A first row that copies no bytes, then one that copies .data, by
    // a function that copies.
    let list = [
        called(0x800_0010, [0x2000_0080, 0x800_0480, 0]),
        called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20]),
        walked_each(&[0..1, 1..2]),
    ];
    assert_eq!(
        steps(&binary, &list),
        ["copy 0x20000000 32 by a call: .data 32"]
    );
    // As data_init(load image, start, size) does, with a loop.
    let list = [
        called(0x800_0010, [0x800_0480, 0x2000_0080, 0]),
        called(0x800_0010, [0x800_0400, 0x2000_0000, 0x20]),
        Event::Loop(Loop {
            depth: 1,
            events: 2..2,
            ..loop_with(vec![copied(0x2000_0000, 0x800_0400, 4, 4, 8)])
        }),
        walked_each(&[0..1, 1..3]),
    ];
    assert_eq!(
        steps(&binary, &list),
        ["copy 0x20000000 32 by a call: .data 32"]
    );
}

#[test]
fn a_walk_that_runs_back_into_its_code_stops_at_a_loop_it_cant_follow() {
    let binary = binary();
    // As GCC's -Og makes of CMSIS's __cmsis_start: after the copy of
    // .data, code that the walk can't make out a loop of may set up the
    // rest.
    let list = [called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20])];
    assert_eq!(
        steps_to(&binary, &list, End::Rejoins(0x800_0020)),
        [
            "copy 0x20000000 32 by a call: .data 32",
            "unread 0x0 0 by a stop: .ramfunc, .bss, .noinit"
        ]
    );
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
