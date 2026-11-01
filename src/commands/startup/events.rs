//! What the steps read of the walk's events ([`Events`]): how many calls
//! deep from the reset handler each event is, which events are the jump of
//! a veneer that a call goes to, where each call goes past a veneer, and
//! where the events of the code that a call runs end. [`Iterations`] says
//! which iteration of a loop that the walk went through one by one each
//! event is in.

use super::Firmware;
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
    targets: Vec<Option<(u64, Option<u64>)>>,
}

impl<'w> Events<'w> {
    pub fn new(firmware: &Firmware, list: &'w [Event]) -> Events<'w> {
        let veneer_jumps: Vec<bool> = (0..list.len())
            .map(|k| {
                let Event::Call(call) = &list[k] else {
                    return false;
                };
                let Some(veneer) = firmware.item(call.site) else {
                    return false;
                };
                let start = firmware.binary.items[veneer].address;
                call.how == How::Jump
                    && k > 0
                    && matches!(&list[k - 1], Event::Call(before) if before.target == Some(start))
                    && firmware.veneer_target(start).is_some()
            })
            .collect();
        let mut open: Vec<u32> = Vec::new();
        // How the events nest is what `Walk::events` says.
        let levels = (list.iter().enumerate())
            .map(|(k, event)| {
                // A backtrack opens or closes no level.
                if let Event::Backtrack { .. } = event {
                    return 0;
                }
                let depth = event.depth();
                // The function a tail call jumps to returns to the caller
                // of the one that jumped.
                while open.last().is_some_and(|&open| depth < open) {
                    open.pop();
                }
                let level = depth + open.len() as u32;
                if let Event::Call(call) = event
                    && call.how == How::Jump
                    && call.followed
                    && !veneer_jumps[k]
                {
                    open.push(call.depth);
                }
                level
            })
            .collect();
        let targets = (list.iter().zip(&veneer_jumps))
            .map(|(event, &veneer_jump)| match event {
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

    pub fn level(&self, k: usize) -> u32 {
        self.levels[k]
    }

    /// The call at `k`, unless it is a veneer's jump.
    pub fn call(&self, k: usize) -> Option<&'w Call> {
        match &self.list[k] {
            Event::Call(call) if !self.veneer_jumps[k] => Some(call),
            _ => None,
        }
    }

    /// Where the call at `k` goes, past a veneer.
    pub fn target(&self, k: usize) -> Option<u64> {
        self.targets[k].map(|(target, _)| target)
    }

    /// The veneer that the call at `k` goes through.
    pub fn veneer(&self, k: usize) -> Option<u64> {
        self.targets[k].and_then(|(_, veneer)| veneer)
    }

    /// Where the events of the code that the call at `k` runs end: the
    /// first event after it that is no deeper, but for a backtrack (see
    /// `Walk::events`).
    pub fn span(&self, k: usize) -> usize {
        let level = self.levels[k];
        (k + 1..self.list.len())
            .find(|&j| !matches!(self.list[j], Event::Backtrack { .. }) && self.levels[j] <= level)
            .unwrap_or(self.list.len())
    }
}

/// The iterations of loops that the walk went through one by one (see
/// [`Loop::walked_iterations`]), by event.
///
/// [`Loop::walked_iterations`]: crate::arch::startup::Loop::walked_iterations
pub(super) struct Iterations {
    /// For each event, the innermost iteration it is in, as the first
    /// event of that iteration, with the loop's event and the iteration's
    /// row of a table.
    of: Vec<Option<(usize, usize, Option<u32>)>>,
}

impl Iterations {
    pub fn new(list: &[Event]) -> Iterations {
        let mut of = vec![None; list.len()];
        // A loop inside another closes first, so its iterations come first.
        for (k, event) in list.iter().enumerate() {
            let Event::Loop(l) = event else {
                continue;
            };
            for iteration in l.walked_iterations() {
                for slot in &mut of[iteration.events.clone()] {
                    slot.get_or_insert((iteration.events.start, k, iteration.row));
                }
            }
        }
        Iterations { of }
    }

    /// The iteration that the event at `k` is in, by its first event.
    pub fn of(&self, k: usize) -> Option<usize> {
        self.of.get(k).copied().flatten().map(|(first, _, _)| first)
    }

    /// The loop and the iteration that the event at `k` is in, by their
    /// events (see [`Home`](super::pieces::Home)).
    pub fn home(&self, k: usize) -> Option<(usize, usize)> {
        (self.of.get(k).copied().flatten()).map(|(first, loop_event, _)| (loop_event, first))
    }

    /// Where the iteration that the event at `k` is in reads its row of a
    /// table.
    pub fn row(&self, k: usize) -> Option<u32> {
        self.of
            .get(k)
            .copied()
            .flatten()
            .and_then(|(_, _, row)| row)
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
