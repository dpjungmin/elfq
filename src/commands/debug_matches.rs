//! `elfq FILE debug matches PATTERN...`: lists the items that each pattern,
//! such as `uart_*`, matches, one JSON object a line, with how each one
//! matched. It exists to check patterns while developing elfq (see
//! `symbol::Pattern`).

use std::io::Write;

use anyhow::{Result, anyhow};

use super::symbol::{Names, Pattern};
use super::{Outcome, json};
use crate::binary::Binary;

/// Answers `elfq FILE debug matches PATTERN...`: for each pattern, in the
/// order given, a line for each item it matches, in address order.
pub(super) fn run(binary: &Binary, typed: &[String], out: &mut dyn Write) -> Result<Outcome> {
    let patterns = typed
        .iter()
        .map(|typed| {
            Pattern::parse(typed).ok_or_else(|| {
                anyhow!("`{typed}` isn't a pattern: it has no * that stands for any text")
            })
        })
        .collect::<Result<Vec<Pattern>>>()?;
    let names = Names::new(binary);
    for pattern in &patterns {
        for found in names.matching(pattern) {
            json::write(&json::Match::new(binary, pattern, &found), out)?;
        }
    }
    Ok(Outcome::Answered)
}
