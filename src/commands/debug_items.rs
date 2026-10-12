//! `elfq debug items FILE`: lists the items elfq found and where it found
//! each one. It exists to check item discovery while developing elfq; for
//! browsing symbols, `nm` and `readelf -s` are better tools.

use std::io::Write;

use anyhow::Result;
use object::{Architecture, ObjectKind};

use crate::binary::{Binary, Item, ItemKind};
use crate::cli::ItemsArgs;
use crate::render::{Align, Table};

pub fn run(args: &ItemsArgs, out: &mut dyn Write) -> Result<()> {
    let binary = Binary::load(&args.file)?;
    for warning in &binary.warnings {
        eprintln!("warning: {warning}");
    }

    writeln!(
        out,
        "{}, {}, entry {:#x}: {}, {}",
        architecture_name(binary.architecture),
        kind_name(binary.kind),
        binary.entry,
        count(&binary, ItemKind::Function, "function", "functions"),
        count(&binary, ItemKind::Data, "data object", "data objects"),
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
            size_text(item),
            item.kind.to_string(),
            item.source.to_string(),
            name_text(item),
        ]);
    }
    table.write(out)?;
    Ok(())
}

/// "1 function", "2 functions".
fn count(binary: &Binary, kind: ItemKind, one: &str, many: &str) -> String {
    let n = binary.items.iter().filter(|item| item.kind == kind).count();
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The size, `~N` when it is an estimate, or `?` when it is unknown.
fn size_text(item: &Item) -> String {
    match (item.size, item.size_estimated) {
        (0, _) => "?".to_string(),
        (size, true) => format!("~{size}"),
        (size, false) => size.to_string(),
    }
}

fn name_text(item: &Item) -> String {
    if item.aliases.is_empty() {
        item.name.clone()
    } else {
        format!("{} (also {})", item.name, item.aliases.join(", "))
    }
}

fn architecture_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => "x86-64".to_string(),
        Architecture::Arm => "32-bit Arm".to_string(),
        other => format!("{other:?}"),
    }
}

fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Executable => "executable",
        ObjectKind::Dynamic => "shared library or position-independent executable",
        _ => "unsupported file type",
    }
}
