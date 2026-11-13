//! The code that runs from load images before startup code copies them,
//! and what must stay out of each that holds code ([`EarlyRunFinder`]):
//! the reset handler's own code, and each function that a call before
//! the copy runs, at any depth, as the call's call tree finds them
//! ([`CallTree`]).

use std::collections::{HashMap, HashSet};

use super::super::Facts;
use super::super::events::Events;
use super::super::start::Main;
use super::super::steps::{Does, Step};
use super::{EarlyFunction, EarlyRun, KeepOut, RunsAt, Setups};
use crate::binary::ItemKind;

/// Finds the code that runs from load images before startup code copies
/// them (see [`EarlyRunFinder::find`]), and what must stay out of each that
/// holds code (see [`EarlyRunFinder::keep_out`]).
pub(super) struct EarlyRunFinder<'a> {
    facts: &'a Facts<'a>,
    /// The steps, which copy the load images.
    steps: &'a [Step],
    /// What sets up each section.
    setups: &'a Setups,
    /// The function that is `main`, which the call trees leave out.
    main: Option<u64>,
    /// The call tree of each function (see [`EarlyRunFinder::call_tree`]).
    call_trees: HashMap<u64, CallTree>,
    /// The functions that each function calls or jumps to.
    callees: HashMap<u64, Vec<u64>>,
}

/// The functions that run when a function runs, each with the function
/// that calls it on a shortest chain from the first (see
/// [`EarlyRunFinder::call_tree`]).
struct CallTree {
    /// Each function, in the order a breadth-first search finds them.
    found: Vec<Called>,
    /// The indexes into `found`, by address.
    by_address: Vec<usize>,
}

/// A function in a call tree.
#[derive(Debug, Clone, Copy)]
struct Called {
    function: u64,
    /// The index in [`CallTree::found`] of the function that calls it on
    /// the shortest chain from the first, or for the first, its own.
    caller: usize,
}

impl CallTree {
    /// The functions from `start` to `end`, in the order they were found,
    /// each with the chain of functions from the first to it.
    fn within(&self, start: u64, end: u64) -> Vec<Hit> {
        let first = (self.by_address).partition_point(|&i| self.found[i].function < start);
        let mut indexes: Vec<usize> = (self.by_address[first..].iter().copied())
            .take_while(|&i| self.found[i].function < end)
            .collect();
        indexes.sort_unstable();
        (indexes.into_iter())
            .map(|i| Hit {
                function: self.found[i].function,
                chain: self.chain(i),
            })
            .collect()
    }

    /// The chain of functions from the first to the one at index `i`.
    fn chain(&self, mut i: usize) -> Vec<u64> {
        let mut chain = vec![self.found[i].function];
        while self.found[i].caller != i {
            i = self.found[i].caller;
            chain.push(self.found[i].function);
        }
        chain.reverse();
        chain
    }
}

/// A function in a load image that the reset handler or a call runs,
/// with the chain of functions that gets to it: for one that a call's
/// call tree finds, from the function that the call goes to down to it
/// (see [`CallTree::within`]); otherwise the function alone.
#[derive(Debug, Clone)]
struct Hit {
    function: u64,
    chain: Vec<u64>,
}

impl Hit {
    /// The function at `function`, which the code runs itself: its chain
    /// is the function alone.
    fn alone(function: u64) -> Hit {
        Hit {
            function,
            chain: vec![function],
        }
    }
}

/// What runs code from a load image (see [`EarlyRunFinder::add`]).
enum RunBy {
    /// The reset handler, whose own code is in it.
    ResetHandler,
    /// The call at `event`, from `site`, in the step at index `step`,
    /// inside the calls to `callers`, from the reset handler's own call
    /// down.
    Call {
        event: usize,
        site: u64,
        step: Option<usize>,
        callers: Vec<u64>,
    },
}

impl RunBy {
    /// Where in the walk it runs the code.
    fn at(&self) -> RunsAt {
        match self {
            RunBy::ResetHandler => RunsAt::ResetHandler,
            RunBy::Call { event, .. } => RunsAt::Event(*event),
        }
    }
}

/// A call that runs before the step that copies a load image (see
/// [`EarlyRunFinder::calls_before`]).
#[derive(Debug, Clone, Copy)]
struct CallBefore {
    event: usize,
    /// The function that the call goes to.
    target: u64,
    /// Whether the call holds the copy, so that only the function's own
    /// code runs before it.
    holds_copy: bool,
}

impl<'a> EarlyRunFinder<'a> {
    /// What finds the code that runs from load images before `steps`,
    /// which set up the sections as `setups` says, copy them.
    pub(super) fn new(
        facts: &'a Facts<'a>,
        steps: &'a [Step],
        setups: &'a Setups,
    ) -> EarlyRunFinder<'a> {
        EarlyRunFinder {
            facts,
            steps,
            setups,
            main: facts.main.and_then(Main::function),
            call_trees: HashMap::new(),
            callees: HashMap::new(),
        }
    }

    /// The code that runs from each load image before the steps copy it,
    /// one early run for each: the reset handler's own code, and what each
    /// call runs (see [`EarlyRunFinder::add_calls`]). An early run leaves
    /// out the veneers among its functions, unless all are veneers, and
    /// holds the unread steps that may copy its load image first.
    pub(super) fn find(&mut self) -> Vec<EarlyRun> {
        let Facts {
            firmware,
            ram,
            reset,
            ..
        } = *self.facts;
        let mut runs = Vec::new();
        let boot = reset.address;
        for &section in &ram.loads {
            if ram.section(section).address <= boot && boot < ram.end(section) {
                let hits = [Hit::alone(boot)];
                self.add(&mut runs, section, RunBy::ResetHandler, &hits);
            }
        }
        for &section in &ram.loads {
            self.add_calls(&mut runs, section);
        }
        for run in &mut runs {
            if (run.functions.iter()).any(|early| !firmware.is_veneer(early.function)) {
                (run.functions).retain(|early| !firmware.is_veneer(early.function));
            }
            let first = run.first();
            run.unless = (self.steps.iter().enumerate())
                .filter(|(_, step)| {
                    step.does == Does::Unread
                        && RunsAt::Event(step.event) <= first
                        && step.may.contains(&run.section)
                })
                .map(|(index, _)| index)
                .collect();
        }
        runs
    }

    /// Adds to `runs` the code that each call before the step that copies
    /// the load image `section` runs from it: the function that a call
    /// holding the copy goes to, when it is in the load image, and for
    /// other calls, the functions of its call tree that are; with a
    /// veneer in the load image that the call goes through.
    fn add_calls(&mut self, runs: &mut Vec<EarlyRun>, section: usize) {
        let Facts { ram, events, .. } = *self.facts;
        let (start, stop) = (ram.section(section).address, ram.end(section));
        for CallBefore {
            event,
            target,
            holds_copy,
        } in self.calls_before(section)
        {
            let Some(call) = events.call(event) else {
                continue;
            };
            let mut hits = if holds_copy {
                (start..stop)
                    .contains(&target)
                    .then(|| Hit::alone(target))
                    .into_iter()
                    .collect()
            } else {
                self.call_tree(target).within(start, stop)
            };
            if let Some(veneer) = events
                .veneer(event)
                .filter(|veneer| (start..stop).contains(veneer))
            {
                match hits.iter_mut().find(|hit| hit.function == veneer) {
                    Some(hit) => hit.chain = vec![veneer],
                    None => hits.push(Hit::alone(veneer)),
                }
            }
            if !hits.is_empty() {
                let by = RunBy::Call {
                    event,
                    site: call.site,
                    step: step_at(self.steps, event),
                    callers: enclosing(events, event),
                };
                self.add(runs, section, by, &hits);
            }
        }
    }

    /// The calls that run before the step that copies the load image
    /// `section`, or before `main` when no step does, the copy's own call
    /// among them. Calls to `main` don't count.
    fn calls_before(&self, section: usize) -> Vec<CallBefore> {
        let events = self.facts.events;
        let end = (self.setups.init.get(&section))
            .map_or(events.list.len(), |setup| self.steps[setup.step].event);
        (0..events.list.len().min(end + 1))
            .filter_map(|event| {
                events.call(event)?;
                let target = events
                    .target(event)
                    .filter(|&target| Some(target) != self.main)?;
                Some(CallBefore {
                    event,
                    target,
                    holds_copy: event < end && end < events.span(event),
                })
            })
            .collect()
    }

    /// For each load image that holds code, by address, the functions
    /// that startup runs before it copies it, which must stay out of it:
    /// those that would run from it too early if they were in it. The
    /// reset handler comes first, then each call's function and what it
    /// calls, breadth first, each once, but no veneer.
    pub(super) fn keep_out(&mut self) -> Vec<KeepOut> {
        let Facts {
            firmware,
            ram,
            reset,
            ..
        } = *self.facts;
        let mut rows = Vec::new();
        for &section in &ram.sections {
            if !ram.is_load(section) || !ram.section(section).is_code() {
                continue;
            }
            let mut items: Vec<usize> = Vec::new();
            let (mut addresses, mut indexes) = (HashSet::new(), HashSet::new());
            let mut add = |address: u64| {
                if !addresses.insert(address) || firmware.is_veneer(address) {
                    return;
                }
                let Some(index) = firmware.item(address) else {
                    return;
                };
                if firmware.binary.items[index].kind == ItemKind::Function && indexes.insert(index)
                {
                    items.push(index);
                }
            };
            add(reset.address);
            // A function called again adds nothing new, and going through
            // what it calls each time would take as long as the calls
            // times all they call.
            let mut expanded = HashSet::new();
            for call in self.calls_before(section) {
                if call.holds_copy {
                    add(call.target);
                } else if expanded.insert(call.target) {
                    for called in &self.call_tree(call.target).found {
                        add(called.function);
                    }
                }
            }
            rows.push(KeepOut { section, items });
        }
        rows
    }

    /// Counts the functions of `hits`, each with its chain, as code that
    /// runs from the load image `section` when `by` runs it, in its early
    /// run in `runs`.
    fn add(&self, runs: &mut Vec<EarlyRun>, section: usize, by: RunBy, hits: &[Hit]) {
        // The reset handler's own code is in the load image; a call's
        // code calls on into it.
        let (step, callers, sites): (Option<usize>, &[u64], Vec<u64>) = match &by {
            RunBy::ResetHandler => (None, &[], hits.iter().map(|hit| hit.function).collect()),
            RunBy::Call {
                site,
                step,
                callers,
                ..
            } => (
                *step,
                callers,
                (hits.iter())
                    .flat_map(|hit| self.use_sites(&hit.chain, *site))
                    .collect(),
            ),
        };
        let index = match runs.iter().position(|run| run.section == section) {
            Some(index) => index,
            None => {
                let before = self.setups.init.get(&section).map(|setup| setup.step);
                runs.push(EarlyRun::new(section, before, by.at()));
                runs.len() - 1
            }
        };
        let run = &mut runs[index];
        for hit in hits {
            if !run.runs(hit.function) {
                run.functions.push(EarlyFunction {
                    function: hit.function,
                    chain: callers.iter().chain(&hit.chain).copied().collect(),
                });
            }
        }
        run.runs_at.push(by.at());
        run.first = run.first.min(by.at());
        if let Some(step) = step
            && !run.steps.contains(&step)
        {
            run.steps.push(step);
        }
        for site in sites {
            if !run.sites.contains(&site) {
                run.sites.push(site);
            }
        }
    }

    /// The functions that run when the function at `address` runs: what it
    /// calls and jumps to, at any depth, through veneers, but never `main`.
    fn call_tree(&mut self, address: u64) -> &CallTree {
        if !self.call_trees.contains_key(&address) {
            let mut found = vec![Called {
                function: address,
                caller: 0,
            }];
            let mut seen: HashSet<u64> = HashSet::from([address]);
            let mut next = 0;
            while next < found.len() {
                let from = found[next].function;
                let firmware = self.facts.firmware;
                let callees = (self.callees.entry(from)).or_insert_with(|| firmware.callees(from));
                for &to in callees.iter() {
                    if Some(to) != self.main && seen.insert(to) {
                        found.push(Called {
                            function: to,
                            caller: next,
                        });
                    }
                }
                next += 1;
            }
            let mut by_address: Vec<usize> = (0..found.len()).collect();
            by_address.sort_unstable_by_key(|&i| found[i].function);
            self.call_trees
                .insert(address, CallTree { found, by_address });
        }
        &self.call_trees[&address]
    }

    /// Where the last function of `chain` that isn't a veneer calls on
    /// toward the function at its end: `site` when the chain is that
    /// function alone.
    fn use_sites(&self, chain: &[u64], site: u64) -> Vec<u64> {
        if chain.len() < 2 {
            return vec![site];
        }
        let mut last = chain.len() - 2;
        let firmware = self.facts.firmware;
        while last > 0 && firmware.is_veneer(chain[last]) {
            last -= 1;
        }
        firmware.call_sites(chain[last], chain[last + 1])
    }
}

/// The functions that the calls holding `event` go to, from the reset
/// handler's own call down.
fn enclosing(events: &Events, event: usize) -> Vec<u64> {
    let mut callers = Vec::new();
    let mut level = events.level(event);
    for earlier in (0..event).rev() {
        if level == 0 {
            break;
        }
        if events.level(earlier) == level - 1
            && events.call(earlier).is_some()
            && event < events.span(earlier)
            && let Some(target) = events.target(earlier)
        {
            callers.push(target);
            level -= 1;
        }
    }
    callers.reverse();
    callers
}

/// The step of the reset handler's own code whose run holds `event`, as
/// an index into the steps.
fn step_at(steps: &[Step], event: usize) -> Option<usize> {
    (steps.iter()).rposition(|step| step.begins <= event && step.level == 0)
}
