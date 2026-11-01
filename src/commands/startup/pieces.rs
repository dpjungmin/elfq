//! The bytes that code copies or zeroes one after another, which steps
//! are made of ([`Piece`]): a loop's, from all its stores
//! ([`Piece::of_loop`]), or a store's outside loops
//! ([`Piece::of_store`]). Pieces next to each other in the same code join,
//! and when what they copy or zero matches the sections (see
//! [`RamSections::matched`]), they make a step, or extend the step of a
//! piece they join ([`Pieces::add`]).

use std::collections::HashSet;
use std::ops::Range;

use super::ram_sections::{Match, RamSections};
use super::steps::{Does, Step, Way};
use crate::arch::startup::{Loop, LoopStore, Store};

/// The level and function of code, and the iteration of a loop it is in,
/// with the loop, as their events (see [`Piece::home`]).
pub(super) type Home = (u32, u64, Option<(usize, usize)>);

/// Bytes that code copies or zeroes one after another: a loop's, or a
/// store's outside loops, or those of several of them next to each other
/// in the same function, as of a loop and of the stores that copy the
/// words its last time leaves to code after it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Piece {
    does: Does,
    to: u64,
    size: u64,
    /// For a copy, how far after where it stores each word it loads it
    /// from, wrapping round.
    gap: Option<u32>,
    /// Where the code is: its level and function, and the iteration that
    /// it is in of a loop that the walk went through one iteration at a
    /// time (see [`Iterations`]). Pieces join only when they have the same
    /// home, but for those of one loop's iterations by the same store,
    /// which make no step alone (see [`Pieces::add`]).
    ///
    /// [`Iterations`]: super::events::Iterations
    home: Home,
    /// Its first store, of the loop's when there is a loop.
    site: u64,
    /// [`Way::Loop`] when there is a loop, or else [`Way::Store`].
    way: Way,
    /// The step that copies or zeroes its bytes, as an index into the
    /// steps.
    step: Option<usize>,
}

impl Piece {
    /// The bytes that the loop `l`, of the code at `home`, copies or
    /// zeroes, when:
    ///
    /// 1. the walk can tell what each of its stores does each time the loop
    ///    runs (see [`Loop::stores`]), and some of them store into the
    ///    sections;
    /// 2. those stores cover all their bytes once, one after another, each
    ///    time the next ones up or down (see [`tiled`]);
    /// 3. and they copy, each time storing words that they load from as far
    ///    after or before where they store them, the same distance for
    ///    every store; or else zero, each time storing 0, which they don't
    ///    load.
    pub fn of_loop(ram: &RamSections, l: &Loop, home: Home) -> Option<Piece> {
        // 1.
        let stores: Vec<&LoopStore> = (l.stores()?.iter())
            .filter(|store| store.times > 0 && ram.overlaps(stored_bytes(store)))
            .collect();
        let first = *stores.first()?;
        // 2.
        let (to, size) = tiled(&stores)?;
        // 3. How far after each store a copy loads its words from.
        let gap = |store: &LoopStore| {
            let (from, step) = store.source?;
            (step == store.step).then(|| from.wrapping_sub(store.address))
        };
        let copies = (gap(first)).filter(|&by| stores.iter().all(|&store| gap(store) == Some(by)));
        let zeroes = (stores.iter()).all(|store| store.value == Some(0) && !store.loaded);
        let does = match copies {
            Some(_) => Does::Copy,
            None if zeroes => Does::Zero,
            None => return None,
        };
        Some(Piece {
            does,
            to,
            size,
            gap: copies,
            home,
            site: first.site,
            way: Way::Loop,
            step: None,
        })
    }

    /// The bytes that `store`, outside loops, of the code at `home`, copies
    /// or zeroes: a copy when the walk can tell where it loaded its words
    /// from, or a zeroing when it stores 0.
    pub fn of_store(ram: &RamSections, store: &Store, home: Home) -> Option<Piece> {
        let address = store.address?;
        let (to, size) = (u64::from(address), u64::from(store.size));
        if store.conditional || size == 0 || !ram.overlaps(to..to + size) {
            return None;
        }
        let (does, gap) = match store.source {
            Some(source) => (Does::Copy, Some(source.address.wrapping_sub(address))),
            None if store.value == Some(0) => (Does::Zero, None),
            None => return None,
        };
        Some(Piece {
            does,
            to,
            size,
            gap,
            home,
            site: store.site,
            way: Way::Store,
            step: None,
        })
    }

    fn end(&self) -> u64 {
        self.to + self.size
    }

    /// Whether `other` does what this does with the bytes next to its
    /// own, in the same function, and the same time through a loop, or
    /// `across` another time of the same loop, by the same store.
    fn next_to(&self, other: &Piece, across: bool) -> bool {
        let (level, function, time) = self.home;
        let same_loop = |(loop_event, _): (usize, usize)| {
            (other.home.2).is_some_and(|(theirs, _)| theirs == loop_event)
        };
        let home = self.home == other.home
            || (across
                && (level, function) == (other.home.0, other.home.1)
                && time.is_some_and(same_loop)
                && self.site == other.site);
        home && self.does == other.does
            && self.gap == other.gap
            && (self.end() == other.to || other.end() == self.to)
            && !(self.step.is_some() && other.step.is_some())
    }
}

/// What [`RamSections::matched`] matches of the bytes of `piece`, when they
/// make a step (see [`Pieces::add`]).
fn step_match(ram: &RamSections, piece: &Piece) -> Option<Match> {
    let from = (piece.gap).map(|gap| u64::from((piece.to as u32).wrapping_add(gap)));
    (ram.matched(piece.does, piece.to, from, piece.size)).filter(|matched| {
        piece.step.is_some()
            || piece.way == Way::Loop
            || matched.covers.iter().all(|cover| {
                cover.bytes == ram.section(cover.section).size
                    && (cover.from).is_none_or(|from| from == ram.load_address(cover.section))
            })
    })
}

/// The pieces found so far, which steps copy or zero, or may when code
/// after them stores the bytes next to them.
#[derive(Default)]
pub(super) struct Pieces(Vec<Piece>);

impl Pieces {
    /// Adds `piece`, of `event`, and returns whether a step stands for it:
    ///
    /// 1. It joins the pieces next to it, one after another (see
    ///    [`Piece::next_to`]). A piece in an iteration of a loop that the
    ///    walk went through one iteration at a time joins those of the
    ///    loop's other iterations by the same store, as the word that a
    ///    loop copies each iteration with a call does, unless it makes a
    ///    step alone, as the copy of a row of a table does, which is a step
    ///    of its own. The joined bytes are a loop's when any of the pieces
    ///    is, or when they are of several iterations.
    /// 2. When the joined bytes make no step (see [`step_match`]: for the
    ///    bytes of stores alone, only when they cover whole sections,
    ///    copied from their load images), the pieces it would join stay as
    ///    they are. A store's piece is kept alone, for the stores after it
    ///    to join; a loop's goes, and `build` asks what the loop may set up
    ///    (see `unread.rs`).
    /// 3. Else the joined bytes make a step, or extend the step of a piece
    ///    they join, and take the place of the pieces they join.
    pub fn add(
        &mut self,
        ram: &RamSections,
        piece: Piece,
        event: usize,
        steps: &mut Vec<Step>,
        ready: &mut HashSet<usize>,
    ) -> bool {
        // 1.
        let across = piece.home.2.is_some() && step_match(ram, &piece).is_none();
        let mut joined = piece;
        let mut taken: Vec<usize> = Vec::new();
        while let Some(i) =
            (0..self.0.len()).find(|&i| !taken.contains(&i) && self.0[i].next_to(&joined, across))
        {
            let other = self.0[i];
            joined.to = joined.to.min(other.to);
            joined.size += other.size;
            joined.step = joined.step.or(other.step);
            if other.way == Way::Loop && joined.way != Way::Loop {
                (joined.way, joined.site) = (Way::Loop, other.site);
            }
            // The bytes of several iterations are a loop's.
            if other.home != joined.home {
                joined.way = Way::Loop;
            }
            taken.push(i);
        }
        // 2.
        let Some(matched) = step_match(ram, &joined) else {
            if piece.way == Way::Store {
                self.0.push(piece);
            }
            return false;
        };
        // 3.
        let index = match joined.step {
            Some(index) => index,
            None => {
                let (level, function, _) = joined.home;
                let mut step =
                    Step::new(event, joined.site, joined.does, joined.way, function, level);
                step.function = Some(function);
                steps.push(step);
                steps.len() - 1
            }
        };
        let step = &mut steps[index];
        step.to = Some(matched.to);
        step.from = matched.from;
        step.size = Some(matched.size);
        ready.extend(matched.covers.iter().map(|cover| cover.section));
        step.covers = matched.covers;
        taken.sort_unstable();
        for i in taken.into_iter().rev() {
            self.0.remove(i);
        }
        self.0.push(Piece {
            step: Some(index),
            ..joined
        });
        true
    }

    /// Takes in `store`, outside loops, of the code at `home`, which goes
    /// through a pointer into a section, at an offset that the walk can't
    /// tell: when a step of the same code covers only part of that
    /// section, as when the store is of a word that a loop's last time
    /// leaves to code after it, that step becomes code that elfq can't
    /// follow, which may set up the sections it covered and those that
    /// follow the section with no gap.
    pub fn unread_store(
        &mut self,
        ram: &RamSections,
        store: &Store,
        home: Home,
        steps: &mut [Step],
        ready: &mut HashSet<usize>,
    ) {
        if store.address.is_some() || store.conditional {
            return;
        }
        let Some(section) = store.base.and_then(|base| ram.holding(u64::from(base))) else {
            return;
        };
        let part = |index: usize| {
            (steps[index].covers.iter())
                .any(|cover| cover.section == section && cover.bytes < ram.section(section).size)
        };
        let Some(i) =
            (self.0.iter()).position(|piece| piece.home == home && piece.step.is_some_and(part))
        else {
            return;
        };
        let Some(index) = self.0.remove(i).step else {
            return;
        };
        let step = &mut steps[index];
        let mut may: Vec<usize> = step.covers.iter().map(|cover| cover.section).collect();
        for other in ram.run_from(section) {
            if !may.contains(&other) {
                may.push(other);
            }
        }
        step.does = Does::Unread;
        (step.covers, step.may) = (Vec::new(), may);
        (step.to, step.from, step.size) = (None, None, None);
        *ready = (steps.iter())
            .flat_map(|step| step.covers.iter().map(|cover| cover.section))
            .collect();
    }
}

/// The bytes that a loop's store covers over all the times it stores.
fn stored_bytes(store: &LoopStore) -> Range<u64> {
    let first = i64::from(store.address);
    let last = first + i64::from(store.step) * i64::from(store.times.saturating_sub(1));
    let start = first.min(last).max(0) as u64;
    start..(first.max(last) + i64::from(store.size)).max(0) as u64
}

/// Where the bytes that `stores`, a loop's stores, cover start, and how
/// many there are, when they cover all of them once, one after another:
/// each time the loop runs, its stores store the bytes next to those of
/// the time before, all up or all down, and the last time, when the loop
/// leaves part way through, those next to them.
fn tiled(stores: &[&LoopStore]) -> Option<(u64, u64)> {
    let mut each: Vec<(i64, i64)> = (stores.iter())
        .map(|store| (i64::from(store.address), i64::from(store.size)))
        .collect();
    each.sort_unstable();
    let low = each.first()?.0;
    let size: i64 = each.iter().map(|&(_, size)| size).sum();
    let mut end = low;
    for &(start, size) in &each {
        if start != end {
            return None;
        }
        end += size;
    }
    // How many times all of them store, and those that store once more,
    // the last time.
    let most = stores.iter().map(|store| store.times).max()?;
    let all = stores.iter().all(|store| store.times == most);
    let full = i64::from(if all { most } else { most - 1 });
    let last: Vec<&&LoopStore> = (stores.iter())
        .filter(|store| !all && store.times == most)
        .collect();
    let moving: Vec<i64> = (stores.iter())
        .filter(|store| store.times > 1)
        .map(|store| i64::from(store.step))
        .collect();
    let step = match moving.first() {
        None => size,
        Some(&step) if step.abs() == size && moving.iter().all(|&other| other == step) => step,
        Some(_) => return None,
    };
    // The bytes that the last time stores, at one end of its stores'.
    let extra: i64 = last.iter().map(|store| i64::from(store.size)).sum();
    let (from, to) = match step > 0 {
        true => (low, low + extra),
        false => (low + size - extra, low + size),
    };
    let mut ends: Vec<(i64, i64)> = (last.iter())
        .map(|store| {
            (
                i64::from(store.address),
                i64::from(store.address) + i64::from(store.size),
            )
        })
        .collect();
    ends.sort_unstable();
    let mut at = from;
    for (start, end) in ends {
        if start != at {
            return None;
        }
        at = end;
    }
    if at != to {
        return None;
    }
    let start = match step > 0 {
        true => low,
        false => low - (full - 1) * size - extra,
    };
    Some((
        u64::try_from(start).ok()?,
        u64::try_from(full * size + extra).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        alone, binary, called, closed, copied, each, loop_with, steps, walked_each,
    };
    use super::*;
    use crate::arch::startup::Event;

    /// What the loop with `stores` does, as "copy 0x20000000 32 from
    /// 0x8000400".
    fn piece(ram: &RamSections, stores: Vec<LoopStore>) -> Option<String> {
        // In the reset handler's own code.
        let home = (0, 0x800_0000, None);
        let piece = Piece::of_loop(ram, &loop_with(stores), home)?;
        let from =
            (piece.gap).map(|gap| format!(" from {:#x}", (piece.to as u32).wrapping_add(gap)));
        Some(format!(
            "{} {:#x} {}{}",
            piece.does.word(),
            piece.to,
            piece.size,
            from.unwrap_or_default()
        ))
    }

    #[test]
    fn a_loop_s_stores_cover_its_bytes_one_time_after_another() {
        let tiled = |stores: &[LoopStore]| tiled(&stores.iter().collect::<Vec<_>>());
        // A word each time, up, or down from the last word; or once.
        assert_eq!(
            tiled(&[each(0x2000_0000, 4, 4, 8, None)]),
            Some((0x2000_0000, 32))
        );
        assert_eq!(
            tiled(&[each(0x2000_001c, -4, 4, 8, None)]),
            Some((0x2000_0000, 32))
        );
        assert_eq!(
            tiled(&[each(0x2000_0000, 0, 4, 1, None)]),
            Some((0x2000_0000, 4))
        );
        // Two words each time, as `strd` stores them, or two stores.
        assert_eq!(
            tiled(&[each(0x2000_0000, 8, 8, 4, None)]),
            Some((0x2000_0000, 32))
        );
        let two = [
            each(0x2000_0004, 8, 4, 4, None),
            each(0x2000_0000, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&two), Some((0x2000_0000, 32)));
        // The last time, the loop leaves after the first store: the next
        // word up, or down.
        let up = [
            each(0x2000_0000, 8, 4, 4, None),
            each(0x2000_0004, 8, 4, 3, None),
        ];
        assert_eq!(tiled(&up), Some((0x2000_0000, 28)));
        let down = [
            each(0x2000_001c, -8, 4, 4, None),
            each(0x2000_0018, -8, 4, 3, None),
        ];
        assert_eq!(tiled(&down), Some((0x2000_0004, 28)));
        // Every other word; stores with a gap between them; and a last
        // time that leaves a gap.
        assert_eq!(tiled(&[each(0x2000_0000, 8, 4, 4, None)]), None);
        let gap = [
            each(0x2000_0000, 8, 4, 4, None),
            each(0x2000_0008, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&gap), None);
        let short = [
            each(0x2000_0000, 8, 4, 3, None),
            each(0x2000_0004, 8, 4, 4, None),
        ];
        assert_eq!(tiled(&short), None);
    }

    #[test]
    fn a_loop_copies_words_it_loads_as_far_on_and_zeroes_with_0() {
        let binary = binary();
        let ram = RamSections::new(&binary);
        // .data and .ramfunc from their load images, a word at a time or
        // 16 bytes, as `ldm` and `stm` copy them.
        assert_eq!(
            piece(&ram, vec![copied(0x2000_0000, 0x800_0400, 4, 4, 12)]).as_deref(),
            Some("copy 0x20000000 48 from 0x8000400")
        );
        assert_eq!(
            piece(&ram, vec![copied(0x2000_0000, 0x800_0400, 16, 16, 2)]).as_deref(),
            Some("copy 0x20000000 32 from 0x8000400")
        );
        // .bss from its last word down, beside a store to a peripheral
        // each time, which sets up nothing.
        let zero = vec![
            each(0x2000_006c, -4, 4, 16, Some(0)),
            each(0x4000_0000, 0, 4, 16, Some(1)),
        ];
        assert_eq!(piece(&ram, zero).as_deref(), Some("zero 0x20000030 64"));
        // Not of words from a table that moves otherwise, or as far on
        // for each store; nor of a 0 that the loop loaded, nor of words it
        // can't tell; nor of a loop that stores only elsewhere.
        let mut table = copied(0x2000_0000, 0x800_0400, 4, 4, 8);
        table.source = Some((0x800_0400, 8));
        assert_eq!(piece(&ram, vec![table]), None);
        let apart = vec![
            copied(0x2000_0000, 0x800_0400, 8, 4, 4),
            copied(0x2000_0004, 0x800_0500, 8, 4, 4),
        ];
        assert_eq!(piece(&ram, apart), None);
        let loaded = LoopStore {
            loaded: true,
            ..each(0x2000_0030, 4, 4, 16, Some(0))
        };
        assert_eq!(piece(&ram, vec![loaded]), None);
        assert_eq!(piece(&ram, vec![each(0x2000_0030, 4, 4, 16, None)]), None);
        assert_eq!(
            piece(&ram, vec![each(0x4000_0000, 4, 4, 16, Some(0))]),
            None
        );
    }

    #[test]
    fn stores_after_a_loop_extend_its_step_and_alone_copy_only_whole_sections() {
        let binary = binary();
        // A loop that copies .data 8 bytes at a time, and leaves the last
        // word to a store after it, as GCC unrolls a copy.
        let list = [
            closed(vec![copied(0x2000_0000, 0x800_0400, 8, 8, 3)], 0..0),
            alone(
                0x800_0030,
                Some(0x2000_0018),
                0x2000_0018,
                Some(0x800_0418),
                None,
            ),
            alone(
                0x800_0034,
                Some(0x2000_001c),
                0x2000_0018,
                Some(0x800_041c),
                None,
            ),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a loop: .data 32"]
        );
        // Stores alone, of all of .ramfunc from its load image; but not of
        // part of it, nor from elsewhere, which may be code that sets a
        // variable.
        let whole: Vec<Event> = (0..4)
            .map(|n| {
                let address = 0x2000_0020 + 4 * n;
                alone(
                    0x800_0030 + 4 * u64::from(n),
                    Some(address),
                    address,
                    Some(0x800_0400 + address - 0x2000_0000),
                    None,
                )
            })
            .collect();
        assert_eq!(
            steps(&binary, &whole),
            ["copy 0x20000020 16 by a store: .ramfunc 16"]
        );
        assert_eq!(steps(&binary, &whole[..3]), Vec::<String>::new());
        let elsewhere = [alone(
            0x800_0030,
            Some(0x2000_0000),
            0x2000_0000,
            Some(0x800_0300),
            None,
        )];
        assert_eq!(steps(&binary, &elsewhere), Vec::<String>::new());
    }

    #[test]
    fn a_store_through_a_pointer_after_a_loop_that_sets_up_part_of_a_section_makes_it_unread() {
        let binary = binary();
        // A loop that zeroes all of .bss but its last word, which a store
        // through a pointer to .bss at an offset the walk can't tell may
        // zero, or not.
        let list = [
            closed(vec![each(0x2000_0030, 4, 4, 15, Some(0))], 0..0),
            alone(0x800_0030, None, 0x2000_0030, None, Some(0)),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
        // When the loop zeroes all of .bss, the store changes nothing.
        let list = [
            closed(vec![each(0x2000_0030, 4, 4, 16, Some(0))], 0..0),
            alone(0x800_0030, None, 0x2000_0030, None, Some(0)),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["zero 0x20000030 64 by a loop: .bss 64"]
        );
    }

    #[test]
    fn the_bytes_that_one_store_stores_over_a_loop_s_times_join() {
        let binary = binary();
        // A word of .data each time, as a loop copies with a call.
        let mut list: Vec<Event> = (0..8)
            .map(|n| {
                let address = 0x2000_0000 + 4 * n;
                alone(
                    0x800_0030,
                    Some(address),
                    address,
                    Some(0x800_0400 + 4 * n),
                    None,
                )
            })
            .collect();
        list.push(walked_each(&(0..8).map(|n| n..n + 1).collect::<Vec<_>>()));
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a loop: .data 32"]
        );
        // The same two words each time, by two stores, as of a variable
        // that the function that copies each row sets: they don't join
        // into a zeroing, and as they set up part of .bss, the loop may
        // set up the rest.
        let twice = |at: u32| -> Vec<Event> {
            (0..2)
                .flat_map(|_| {
                    [
                        alone(0x800_0030, Some(at), at, None, Some(0)),
                        alone(0x800_0034, Some(at + 4), at, None, Some(0)),
                    ]
                })
                .collect()
        };
        let mut list = twice(0x2000_0030);
        list.push(walked_each(&[0..2, 2..4]));
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
        // In .data, which a call copied before: no step.
        let mut list = vec![called(0x800_0010, [0x2000_0000, 0x800_0400, 0x20])];
        list.extend(twice(0x2000_0000));
        list.push(walked_each(&[1..3, 3..5]));
        assert_eq!(
            steps(&binary, &list),
            ["copy 0x20000000 32 by a call: .data 32"]
        );
    }
}
