//! `elfq FILE debug items`: lists the items elfq found and where it found
//! each one. It exists to check item discovery while developing elfq; for
//! browsing symbols, `nm` and `readelf -s` are better tools.

use std::io::Write;

use anyhow::Result;

use super::text;
use crate::binary::{Binary, Item, ItemKind};
use crate::render::{Align, Table};

pub fn run(binary: &Binary, out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "{} {}, entry {:#x}: {}, {}",
        text::architecture_name(binary.architecture),
        binary.kind,
        binary.entry,
        count(binary, ItemKind::Function, "function", "functions"),
        count(binary, ItemKind::Data, "data object", "data objects"),
    )?;
    writeln!(out)?;

    let mut table = Table::new(&[
        ("ADDRESS", Align::Right),
        ("SIZE", Align::Right),
        ("KIND", Align::Left),
        ("SOURCE", Align::Left),
        ("NAME", Align::Left),
    ]);
    for item in &binary.items {
        table.row(vec![
            format!("{:#x}", item.address),
            text::size_text(item),
            item.kind.to_string(),
            item.source.to_string(),
            name_text(item),
        ]);
    }
    table.write(out)?;
    Ok(())
}

fn count(binary: &Binary, kind: ItemKind, one: &str, many: &str) -> String {
    let n = binary.items.iter().filter(|item| item.kind == kind).count();
    text::plural(n, one, many)
}

fn name_text(item: &Item) -> String {
    if item.aliases.is_empty() {
        item.name.clone()
    } else {
        format!("{} (also {})", item.name, item.aliases.join(", "))
    }
}
