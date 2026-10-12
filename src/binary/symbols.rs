//! Items from the symbol tables: `.symtab`, and `.dynsym`, which survives
//! stripping because the dynamic linker needs it.

use std::cmp::Reverse;

use object::{Object, ObjectSymbol, SymbolKind, SymbolSection};

use super::names::demangle;
use super::{Item, ItemKind, Section, Source, code_address};

/// A symbol that can become an item, before aliases are merged.
struct Candidate {
    /// The readable name: the symbol, demangled if it is mangled.
    name: String,
    /// The symbol as the file has it.
    symbol: String,
    kind: ItemKind,
    address: u64,
    size: u64,
    section: usize,
    binding: Binding,
    source: Source,
}

/// How strongly a symbol claims its name. When several symbols share an
/// address, the strongest one names the item and the rest become aliases.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Binding {
    Local,
    Weak,
    Global,
}

/// Returns one item per address that has function or data symbols.
pub(super) fn items(file: &object::File, sections: &[Section]) -> Vec<Item> {
    let mut candidates: Vec<Candidate> = file
        .symbols()
        .filter_map(|symbol| candidate(file, &symbol, Source::SymbolTable, sections))
        .collect();
    candidates.extend(
        file.dynamic_symbols()
            .filter_map(|symbol| candidate(file, &symbol, Source::DynamicSymbolTable, sections)),
    );
    merge_aliases(candidates)
}

fn candidate(
    file: &object::File,
    symbol: &object::Symbol,
    source: Source,
    sections: &[Section],
) -> Option<Candidate> {
    // Other kinds are section and file markers, untyped labels, and
    // thread-local variables, whose "address" is an offset.
    let kind = match symbol.kind() {
        SymbolKind::Text => ItemKind::Function,
        SymbolKind::Data => ItemKind::Data,
        _ => return None,
    };
    // Imports and absolute symbols have no bytes in this file.
    let SymbolSection::Section(index) = symbol.section() else {
        return None;
    };
    if !sections.get(index.0)?.allocated {
        return None;
    }
    // Zero-size data symbols mark positions, like `_edata`; they hold nothing.
    if kind == ItemKind::Data && symbol.size() == 0 {
        return None;
    }
    let symbol_name = String::from_utf8_lossy(symbol.name_bytes().ok()?).into_owned();
    if symbol_name.is_empty() {
        return None;
    }
    let address = match kind {
        ItemKind::Function => code_address(file.architecture(), symbol.address()),
        ItemKind::Data => symbol.address(),
    };
    let binding = if symbol.is_weak() {
        Binding::Weak
    } else if symbol.is_global() {
        Binding::Global
    } else {
        Binding::Local
    };
    Some(Candidate {
        name: demangle(&symbol_name).unwrap_or_else(|| symbol_name.clone()),
        symbol: symbol_name,
        kind,
        address,
        size: symbol.size(),
        section: index.0,
        binding,
        source,
    })
}

/// Turns symbols into items, one per kind and address. Symbols that share an
/// address are aliases of one item, like a weak `NMI_Handler` that points at
/// `Default_Handler`, or the same symbol listed in both tables. A mangled
/// symbol is an alias of its readable name.
fn merge_aliases(mut candidates: Vec<Candidate>) -> Vec<Item> {
    drop_containers(&mut candidates);
    // Within an address, the candidate that best names the item comes first:
    // the strongest binding, then a real name over an assembler-local `.L` one.
    candidates.sort_by(|a, b| {
        let key = |c: &Candidate| {
            (
                c.kind,
                c.address,
                Reverse(c.binding),
                c.name.starts_with(".L"),
            )
        };
        key(a)
            .cmp(&key(b))
            .then(a.source.cmp(&b.source))
            .then(a.name.cmp(&b.name))
    });
    let mut items: Vec<Item> = Vec::new();
    for candidate in candidates {
        if let Some(item) = items.last_mut()
            && item.kind == candidate.kind
            && item.address == candidate.address
        {
            item.size = item.size.max(candidate.size);
            add_alias(item, candidate.name);
            add_alias(item, candidate.symbol);
            continue;
        }
        let mut item = Item {
            name: candidate.name,
            aliases: Vec::new(),
            kind: candidate.kind,
            address: candidate.address,
            size: candidate.size,
            size_estimated: false,
            section: candidate.section,
            source: candidate.source,
        };
        add_alias(&mut item, candidate.symbol);
        items.push(item);
    }
    items
}

fn add_alias(item: &mut Item, name: String) {
    if name != item.name && !item.aliases.contains(&name) {
        item.aliases.push(name);
    }
}

/// Drops data symbols that only enclose other data symbols, such as LLVM's
/// `.L_MergedGlobals`, which covers several variables placed side by side.
/// The variables inside are the items.
fn drop_containers(candidates: &mut Vec<Candidate>) {
    candidates.sort_by_key(|c| (c.kind, c.address, Reverse(c.size)));
    let shape = |c: &Candidate| (c.kind, c.address, c.size);
    let keep: Vec<bool> = (0..candidates.len())
        .map(|i| {
            let candidate = &candidates[i];
            let end = candidate.address + candidate.size;
            // The next symbol that is not an exact alias of this one.
            let next = candidates[i + 1..]
                .iter()
                .find(|next| shape(next) != shape(candidate));
            !matches!(next, Some(next) if candidate.kind == ItemKind::Data
                && next.kind == ItemKind::Data
                && next.address < end
                && next.address + next.size <= end)
        })
        .collect();
    let mut keep = keep.into_iter();
    candidates.retain(|_| keep.next().unwrap_or(true));
}
