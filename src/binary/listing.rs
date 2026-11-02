//! A function's code as `arch` prints it, for `elfq FILE browse`.
//!
//! The data that the file marks among the code print as data. The unit
//! tests, in `listing/thumb_text.rs`, check the Thumb text against
//! objdump's.

use std::ops::Range;

use super::{Binary, Item};
use crate::arch::Line;

#[cfg(test)]
mod thumb_text;

impl Binary {
    /// The lines of `item`'s code, as `arch` prints them.
    ///
    /// `bytes` are the code's bytes. The data that the file's `$d` symbols
    /// mark there print as data (see [`Binary::data_in_code`]). It is `None`
    /// when elfq can't read the file's code (see
    /// [`Binary::instruction_set`]).
    pub(crate) fn listing(&self, item: &Item, bytes: &[u8]) -> Option<Vec<Line>> {
        // The marks, as offsets into `bytes`.
        let marked: Vec<Range<usize>> = self
            .data_in_code(item.address, item.size)
            .map(|stretch| {
                (stretch.start - item.address) as usize..(stretch.end - item.address) as usize
            })
            .collect();
        let instruction_set = self.instruction_set()?;
        Some(instruction_set.listing(bytes, item.address, &marked))
    }
}
