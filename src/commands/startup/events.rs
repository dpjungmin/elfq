//! What the steps read of the walk's events ([`Events`]): how many calls
//! deep from the reset handler each event is, which events are the jump of
//! a veneer that a call goes to, where each call goes past a veneer, and
//! where the events of the code that a call runs end. [`Iterations`] says
//! which iteration of a loop that the walk went through one by one each
//! event is in.

use super::{CallTarget, Firmware};
use crate::arch::startup::{Call, Event, How};

/// The walk's events, with what the steps read of them.
pub(super) struct Events<'w> {
    pub list: &'w [Event],
    /// How many calls deep from the reset handler each event is, counting
    /// a tail call that the walk went into as a call, where the walk counts
    /// the function it jumps to at the depth of the one that jumps. A
    /// branch to another function that the walk didn't follow, when a flag
    /// is set, opens no level: the walk goes on in the same function.
    levels: Vec<u32>,
    /// Whether each event is the jump of a veneer that a call goes to,
    /// which the call's step stands for.
    veneer_jumps: Vec<bool>,
    /// For each call that the walk can tell the target of, where it goes:
    /// past a veneer, to the function the veneer jumps to, with the
    /// veneer.
    targets: Vec<Option<CallTarget>>,
}

impl<'w> Events<'w> {
    pub fn new(firmware: &Firmware, list: &'w [Event]) -> Events<'w> {
        let veneer_jumps: Vec<bool> = (0..list.len())
            .map(|event| {
                let Event::Call(call) = &list[event] else {
                    return false;
                };
                let Some(veneer) = firmware.item(call.site) else {
                    return false;
                };
                let start = firmware.binary.items[veneer].address;
                call.how == How::Jump
                    && event > 0
                    && matches!(&list[event - 1], Event::Call(before) if before.target == Some(start))
                    && firmware.veneer_target(start).is_some()
            })
            .collect();
        let mut open: Vec<u32> = Vec::new();
        // How the events nest is what `Walk::events` says.
        let levels = (0..list.len())
            .map(|event| {
                // A backtrack opens or closes no level.
                if let Event::Backtrack { .. } = list[event] {
                    return 0;
                }
                let depth = list[event].depth();
                // The function a tail call jumps to returns to the caller
                // of the one that jumped.
                while open.last().is_some_and(|&open| depth < open) {
                    open.pop();
                }
                let level = depth + open.len() as u32;
                if let Event::Call(call) = &list[event]
                    && call.how == How::Jump
                    && call.followed
                    && !veneer_jumps[event]
                {
                    open.push(call.depth);
                }
                level
            })
            .collect();
        let targets = (list.iter().zip(&veneer_jumps))
            .map(|(recorded, &veneer_jump)| match recorded {
                Event::Call(call) if !veneer_jump => Some(firmware.resolve(call.target?)),
                _ => None,
            })
            .collect();
        Events {
            list,
            levels,
            veneer_jumps,
            targets,
        }
    }

    pub fn level(&self, event: usize) -> u32 {
        self.levels[event]
    }

    /// The call at `event`, unless it is a veneer's jump.
    pub fn call(&self, event: usize) -> Option<&'w Call> {
        match &self.list[event] {
            Event::Call(call) if !self.veneer_jumps[event] => Some(call),
            _ => None,
        }
    }

    /// Where the call at `event` goes, past a veneer.
    pub fn target(&self, event: usize) -> Option<u64> {
        self.targets[event].map(|target| target.function)
    }

    /// The veneer that the call at `event` goes through.
    pub fn veneer(&self, event: usize) -> Option<u64> {
        self.targets[event].and_then(|target| target.veneer)
    }

    /// Where the events of the code that the call at `event` runs end: the
    /// first event after it that is no deeper, but for a backtrack (see
    /// `Walk::events`).
    pub fn span(&self, event: usize) -> usize {
        let level = self.levels[event];
        (event + 1..self.list.len())
            .find(|&later| {
                !matches!(self.list[later], Event::Backtrack { .. }) && self.levels[later] <= level
            })
            .unwrap_or(self.list.len())
    }
}

/// An iteration of a walked loop, by events: the loop's, and the
/// iteration's first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WalkedIteration {
    pub loop_event: usize,
    pub first_event: usize,
}

/// The iterations of loops that the walk went through one by one (see
/// [`Loop::walked_iterations`]), by event.
///
/// [`Loop::walked_iterations`]: crate::arch::startup::Loop::walked_iterations
pub(super) struct Iterations {
    /// For each event, the innermost iteration it is in.
    of: Vec<Option<WalkedIteration>>,
    /// For each event, where the iteration it is in reads its row of a
    /// table.
    rows: Vec<Option<u32>>,
}

impl Iterations {
    pub fn new(list: &[Event]) -> Iterations {
        let mut of = vec![None; list.len()];
        let mut rows = vec![None; list.len()];
        // A loop inside another closes first, so its iterations come first.
        for (loop_event, recorded) in list.iter().enumerate() {
            let Event::Loop(l) = recorded else {
                continue;
            };
            for iteration in l.walked_iterations() {
                for event in iteration.events.clone() {
                    if of[event].is_none() {
                        of[event] = Some(WalkedIteration {
                            loop_event,
                            first_event: iteration.events.start,
                        });
                        rows[event] = iteration.row;
                    }
                }
            }
        }
        Iterations { of, rows }
    }

    /// The iteration that `event` is in (see [`Home`](super::pieces::Home)).
    pub fn of(&self, event: usize) -> Option<WalkedIteration> {
        self.of.get(event).copied().flatten()
    }

    /// Where the iteration that `event` is in reads its row of a table.
    pub fn row(&self, event: usize) -> Option<u32> {
        self.rows.get(event).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{binary, called, firmware, steps};
    use super::*;
    use crate::graph::Graph;

    #[test]
    fn only_a_tail_call_that_the_walk_went_into_opens_a_level() {
        let binary = binary();
        let graph = Graph::default();
        let firmware = firmware(&binary, &graph);
        // A branch to another function when a flag is set, as Clang makes
        // of `if (a) f(); else g();`, then a copy.
        let branch = |followed| {
            Event::Call(Call {
                site: 0x800_0008,
                depth: 0,
                target: Some(0x800_0300),
                how: How::Jump,
                arguments: [None; 4],
                left: 0,
                conditional: true,
                followed,
            })
        };
        // When the walk doesn't follow the branch, it goes on in the reset
        // handler, which makes the copy.
        let list = [
            branch(false),
            called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20]),
        ];
        let events = Events::new(&firmware, &list);
        assert_eq!((events.level(0), events.level(1)), (0, 0));
        assert!(
            steps(&binary, &list).contains(&"copy 0x20000000 32 by a call: .data 32".to_string())
        );
        // When it follows it, the function it jumps to makes the copy.
        let list = [
            branch(true),
            called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20]),
        ];
        let events = Events::new(&firmware, &list);
        assert_eq!((events.level(0), events.level(1)), (0, 1));
    }
}
