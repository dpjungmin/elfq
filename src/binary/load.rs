//! Loading a file ([`Binary::load`]). `read_all_but_items` reads the
//! sections, relocations and vector table, then [`Binary::from_vec`] finds
//! the items in the steps that the module doc of `binary` lists, marked with
//! its numbers, through the file of each step that has one. The steps, or
//! parts of them, that have no file of their own are here: the startup
//! arrays, the functions for the unwind table's ranges, which `eh_frame.rs`
//! reads, and the entry point. So is settling the sizes of functions before
//! `scan_functions` reads each one's code once.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use object::{Architecture, Endianness, Object, ObjectSymbol, SymbolKind};

use super::eh_frame::UnwindFunction;
use super::sections::{fitting_size, parse_program, read_sections};
use super::{
    ArmArchitecture, Binary, FileKind, Item, ItemKind, Section, Source, attributes, code_address,
    eh_frame, plt, relocations, symbols, unnamed_data, unnamed_functions, vector_table,
};
use crate::arch::{CodeReference, CodeReferenceKind, InstructionSet};

impl Binary {
    /// Reads and parses the ELF file at `path`.
    pub fn load(path: &Path) -> Result<Binary> {
        let data =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        Binary::from_vec(data).with_context(|| format!("cannot load {}", path.display()))
    }

    /// Parses an ELF file that is already in memory.
    pub fn parse(data: &[u8]) -> Result<Binary> {
        Binary::from_vec(data.to_vec())
    }

    /// Parses an ELF file, keeping `data` as the file's bytes rather than a
    /// copy of them, since a large file takes hundreds of megabytes. It
    /// reads everything but the items, then finds them in the steps that
    /// the module doc lists.
    pub(crate) fn from_vec(data: Vec<u8>) -> Result<Binary> {
        let mut binary = read_all_but_items(data)?;
        // The bytes have moved into `binary`, so read them there. This
        // reads only the headers again.
        let file = object::File::parse(binary.data.as_slice()).context("malformed ELF file")?;

        // `binary.items` stays empty until every function is found, so
        // `item_at` finds nothing until then: each step sees what the steps
        // before it found through `items`.
        // 1. The symbol tables.
        let tables = symbols::read(&file, &binary.sections);
        let mut items = tables.items;
        binary.merged_variables = tables.merged_variables;
        // 2. PLT stubs.
        let stubs = plt::stubs(&binary, &items);
        add_new_items(&mut items, stubs);
        // 3. The arrays of startup and shutdown functions.
        add_new_items(&mut items, startup_array_items(&binary.sections));
        // 4. The unwind table.
        match eh_frame::read(&file) {
            Ok(table) => {
                add_unwind_items(&binary, &mut items, &table.functions);
                binary.used_by_unwinder = eh_frame::used_by_unwinder(&binary, &table.personalities);
            }
            Err(err) => binary
                .warnings
                .push(format!("cannot read .eh_frame: {err}")),
        }
        // 5. The entry point, and the vector table.
        if !binary.entry_in_vector_table() {
            add_entry_item(&binary, &mut items);
        }
        items.extend(vector_table::item(&binary, &items));
        // 6. The code itself, in firmware that nothing lists functions for.
        let listed = items
            .iter()
            .any(|item| item.kind == ItemKind::Function && item.source.lists_functions());
        if binary.thumb && binary.kind == FileKind::Executable && !listed {
            unnamed_functions::add(&binary, &mut items);
        }

        // Every function is found. Their sizes settle before the scan,
        // which reads each function's code by its address and size: entry
        // points end where the next starts, and then those of unknown size
        // reach the next item, which needs the items in address order.
        let run_on = end_where_functions_start(&mut items);
        sort_items(&mut items);
        estimate_missing_sizes(&mut items, &binary.sections);
        binary.items = items;
        binary.code_references = scan_functions(&binary, &run_on);
        // 7. Data that nothing names. Last, since finding it needs all the
        // other items and the references in their code.
        let unnamed = unnamed_data::items(&binary);
        binary.items.extend(unnamed);
        sort_items(&mut binary.items);
        Ok(binary)
    }
}

/// The [`Binary`] of the file in `data` with everything but its items: its
/// sections, imports and relocations, and in Arm files the core the code
/// is for and the stretches that `$d` symbols mark as data, and in Thumb
/// files the vector table.
fn read_all_but_items(data: Vec<u8>) -> Result<Binary> {
    let (file, kind) = parse_program(&data)?;
    let architecture = file.architecture();
    let arm_architecture = if architecture == Architecture::Arm {
        attributes::arm_architecture(&file)
    } else {
        None
    };
    let mut warnings = Vec::new();
    let sections = read_sections(&file, data.len() as u64, &mut warnings);
    let dynamic = relocations::read(&file, architecture);
    let data_in_code = if architecture == Architecture::Arm {
        symbols::data_in_code(&file, &sections)
    } else {
        Vec::new()
    };
    let mut binary = Binary {
        architecture,
        pointer_size: if file.is_64() { 8 } else { 4 },
        kind,
        entry: code_address(architecture, file.entry()),
        // Only a damaged header puts 32-bit Arm code in a 64-bit file.
        thumb: architecture == Architecture::Arm
            && !file.is_64()
            && is_thumb(&file, arm_architecture),
        endianness: if file.is_little_endian() {
            Endianness::Little
        } else {
            Endianness::Big
        },
        has_symbols: file.symbol_table().is_some(),
        has_dwarf: file.section_by_name(".debug_info").is_some(),
        sections,
        items: Vec::new(),
        imports: dynamic.imports,
        relocations: dynamic.relocations,
        used_by_unwinder: Vec::new(),
        merged_variables: Vec::new(),
        vector_table: None,
        arm_architecture,
        data_in_code,
        warnings,
        code_references: HashMap::new(),
        data,
    };
    // Finding the vector table reads the bytes, which are now in `binary`.
    if binary.thumb {
        binary.vector_table = vector_table::find(&binary);
    }
    Ok(binary)
}

/// Whether a 32-bit Arm file's code is Thumb: its entry point or any
/// function symbol has bit 0 set, or its build attributes name a Cortex-M
/// architecture, which runs only Thumb code, as stripped firmware whose
/// linker script names no entry point does.
fn is_thumb(file: &object::File, architecture: Option<ArmArchitecture>) -> bool {
    architecture.is_some_and(|architecture| architecture.is_cortex_m())
        || file.entry() & 1 == 1
        || file
            .symbols()
            .any(|symbol| symbol.kind() == SymbolKind::Text && symbol.address() & 1 == 1)
}

/// Adds each of `new` unless an item of its kind already starts at its
/// address: symbols always win.
fn add_new_items(items: &mut Vec<Item>, new: Vec<Item>) {
    let known: HashSet<(ItemKind, u64)> =
        items.iter().map(|item| (item.kind, item.address)).collect();
    items.extend(
        new.into_iter()
            .filter(|item| !known.contains(&(item.kind, item.address))),
    );
}

/// Items for the arrays of functions that run at startup and exit, which
/// have no symbols of their own.
fn startup_array_items(sections: &[Section]) -> Vec<Item> {
    sections
        .iter()
        .enumerate()
        .filter(|(_, section)| section.is_startup_array() && section.size > 0)
        .map(|(index, section)| Item {
            name: section.name.clone(),
            aliases: Vec::new(),
            kind: ItemKind::Data,
            address: section.address,
            size: section.size,
            size_estimated: false,
            section: index,
            source: Source::Section,
        })
        .collect()
}

/// Adds a function for each of the unwind table's `functions` that the
/// items found so far, from symbols and PLT stubs, missed, and fills in
/// sizes that the symbol table left as 0.
fn add_unwind_items(binary: &Binary, items: &mut Vec<Item>, functions: &[UnwindFunction]) {
    // The functions found so far, as (address, size, index into items).
    let mut known: Vec<(u64, u64, usize)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.kind == ItemKind::Function)
        .map(|(index, item)| (item.address, item.size, index))
        .collect();
    known.sort_unstable();

    for &UnwindFunction { start, length } in functions {
        if start == 0 || length == 0 {
            continue;
        }
        // A function that would end past the last address is damaged.
        let Some(length) = fitting_size(start, length) else {
            continue;
        };
        let before = known.partition_point(|&(address, _, _)| address <= start);
        if let Some(&(address, size, index)) = before.checked_sub(1).map(|i| &known[i]) {
            if address == start {
                if size == 0 {
                    items[index].size = length;
                }
                continue;
            }
            if start < address + size {
                // Part of a function the symbols already describe.
                continue;
            }
        }
        let Some(section) = binary.code_section_at(start) else {
            continue;
        };
        items.push(Item {
            name: format!("fn_{start:#x}"),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: start,
            size: length,
            size_estimated: false,
            section,
            source: Source::EhFrame,
        });
    }
}

/// Adds the entry point as a function when nothing else covers it, as in
/// stripped firmware.
fn add_entry_item(binary: &Binary, items: &mut Vec<Item>) {
    let entry = binary.entry;
    let covered = items
        .iter()
        .any(|item| item.kind == ItemKind::Function && item.contains(entry));
    if covered {
        return;
    }
    if let Some(section) = binary.code_section_at(entry) {
        items.push(Item {
            name: "entry".to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: entry,
            size: 0,
            size_estimated: false,
            section,
            source: Source::EntryPoint,
        });
    }
}

/// Ends each function where the next one starts, when that is inside it
/// and the functions that start inside it run to its end. Hand-written
/// code can give a function several entry points that share its end, each
/// with a symbol whose size runs to that end: libgcc's `__aeabi_dsub` flips
/// the sign of its second argument and runs on into `__adddf3`. Ending each
/// entry where the next starts counts each byte once. A function whose
/// code goes on after the functions inside it stays whole. Returns the
/// functions it ended, by address and size, as the code of each may run on
/// into the next.
fn end_where_functions_start(items: &mut [Item]) -> HashSet<(u64, u64)> {
    // Each function's section, start and end, sorted.
    let mut functions: Vec<(usize, u64, u64)> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| (item.section, item.address, item.address + item.size))
        .collect();
    functions.sort_unstable();
    let mut ended = HashSet::new();
    for item in items
        .iter_mut()
        .filter(|item| item.kind == ItemKind::Function)
    {
        let end = item.address + item.size;
        let after = functions.partition_point(|&(section, address, _)| {
            (section, address) <= (item.section, item.address)
        });
        let mut inside = functions[after..]
            .iter()
            .take_while(|&&(section, address, _)| section == item.section && address < end)
            .peekable();
        let Some(&&(_, next, _)) = inside.peek() else {
            continue;
        };
        if inside.all(|&(_, _, inner_end)| inner_end < end) {
            continue;
        }
        item.size = next - item.address;
        ended.insert((item.address, item.size));
    }
    ended
}

/// Sorts items by address. Of several items at one address, the largest
/// comes first.
fn sort_items(items: &mut [Item]) {
    items.sort_by(|a, b| (a.address, b.size, &a.name).cmp(&(b.address, a.size, &b.name)));
}

/// Gives each function of unknown size the distance to the next item in its
/// section. Functions with nothing after them keep size 0 (unknown), since
/// the end of the section would usually be a wild overestimate.
fn estimate_missing_sizes(items: &mut [Item], sections: &[Section]) {
    for i in 0..items.len() {
        let item = &items[i];
        if item.kind != ItemKind::Function || item.size != 0 {
            continue;
        }
        let section = &sections[item.section];
        let next = items[i + 1..]
            .iter()
            .map(|next| next.address)
            .find(|&address| address > item.address && section.contains(address));
        if let Some(next) = next {
            items[i].size = next - items[i].address;
            items[i].size_estimated = true;
        }
    }
}

/// The references in the code of each function in `binary.items`, by the
/// function's address and size; see [`Binary::code_references`]. Each of
/// `run_on`, the functions that `end_where_functions_start` ended, also
/// jumps to the function after it if its code runs on into it, as a tail
/// call does.
fn scan_functions(
    binary: &Binary,
    run_on: &HashSet<(u64, u64)>,
) -> HashMap<(u64, u64), Vec<CodeReference>> {
    let Some(instruction_set) = binary.instruction_set() else {
        return HashMap::new();
    };
    let functions = binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function);
    functions
        .filter_map(|function| {
            let key = (function.address, function.size);
            let code = binary.bytes(function.address, function.size)?;
            let mut references = instruction_set.scan(code, function.address);
            if run_on.contains(&key) {
                references.extend(run_on_jump(instruction_set, function, code, &references));
            }
            Some((key, references))
        })
        .collect()
}

/// The jump from the last instruction of `function`, whose `code` makes
/// `references`, to the function that starts where it ends, when the code
/// runs on into it (see `InstructionSet::runs_on`). `None` when it
/// doesn't, or when that instruction is a conditional jump there, which
/// `references` already has.
fn run_on_jump(
    instruction_set: InstructionSet,
    function: &Item,
    code: &[u8],
    references: &[CodeReference],
) -> Option<CodeReference> {
    let next = function.address + function.size;
    let site = instruction_set.runs_on(code, function.address)?;
    let jumps_there = references.iter().any(|reference| {
        reference.site == site
            && matches!(
                reference.kind,
                CodeReferenceKind::Jump(to) | CodeReferenceKind::TailJump(to) if to == next
            )
    });
    (!jumps_there).then_some(CodeReference {
        site,
        kind: CodeReferenceKind::TailJump(next),
    })
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::super::sections::null_section;
    use super::*;

    fn text_section() -> Section {
        Section {
            name: ".text".to_string(),
            address: 0x1000,
            size: 0x1000,
            kind: SectionKind::Text,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        }
    }

    fn function(name: &str, address: u64, size: u64) -> Item {
        Item {
            name: name.to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address,
            size,
            size_estimated: false,
            section: 0,
            source: Source::SymbolTable,
        }
    }

    #[test]
    fn unwind_table_fills_gaps_but_never_overrides_symbols() {
        let binary = Binary::for_tests(vec![text_section()], Vec::new(), Vec::new());
        let mut items = vec![
            function("sized", 0x1000, 0x20),
            function("unsized", 0x1100, 0),
        ];
        let described = |start, length| UnwindFunction { start, length };
        let functions = [
            described(0x1000, 0x30),     // same start as a symbol: the symbol's size wins
            described(0x1010, 0x08),     // inside a symbol, such as a split-off cold part
            described(0x1100, 0x40),     // the symbol has no size: take the table's
            described(0x1200, 0x10),     // no symbol at all: a new, unnamed function
            described(0x9000, 0x10),     // outside any code section: ignored
            described(0x1300, u64::MAX), // would end past the last address: ignored
        ];
        add_unwind_items(&binary, &mut items, &functions);
        let found: Vec<(&str, u64, u64, Source)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.address, i.size, i.source))
            .collect();
        assert_eq!(
            found,
            [
                ("sized", 0x1000, 0x20, Source::SymbolTable),
                ("unsized", 0x1100, 0x40, Source::SymbolTable),
                ("fn_0x1200", 0x1200, 0x10, Source::EhFrame),
            ]
        );
    }

    #[test]
    fn entry_points_end_where_the_next_starts() {
        // As libgcc's __gedf2, __ledf2 and __cmpdf2: each symbol runs to
        // the end they share.
        let mut items = vec![
            function("__cmpdf2", 0x1010, 0x70),
            function("__gedf2", 0x1000, 0x80),
            function("__ledf2", 0x1008, 0x78),
            // A function of unknown size, a function at the same address,
            // and data inside a function, change nothing.
            function("unsized", 0x2000, 0),
            function("whole", 0x3000, 0x20),
            function("alias", 0x3000, 0x10),
            Item {
                kind: ItemKind::Data,
                ..function("table", 0x3004, 4)
            },
            // Nor does a function in another section, at an address inside
            // one.
            Item {
                section: 1,
                ..function("elsewhere", 0x3010, 4)
            },
            // A function whose code goes on after the one inside it stays
            // whole, as does one with a function of unknown size inside.
            function("outer", 0x4000, 0x20),
            function("inner", 0x4008, 8),
            function("around", 0x5000, 0x20),
            function("unknown", 0x5010, 0),
        ];
        let ended = end_where_functions_start(&mut items);
        let sizes: Vec<(&str, u64)> = items
            .iter()
            .map(|item| (item.name.as_str(), item.size))
            .collect();
        assert_eq!(
            sizes,
            [
                ("__cmpdf2", 0x70),
                ("__gedf2", 8),
                ("__ledf2", 8),
                ("unsized", 0),
                ("whole", 0x20),
                ("alias", 0x10),
                ("table", 4),
                ("elsewhere", 4),
                ("outer", 0x20),
                ("inner", 8),
                ("around", 0x20),
                ("unknown", 0),
            ]
        );
        assert_eq!(ended, HashSet::from([(0x1000, 8), (0x1008, 8)]));
    }

    /// An x86-64 file whose `.text` at 0x1000 is `code`, holding `items`.
    fn x86_64_binary(code: Vec<u8>, items: Vec<Item>) -> Binary {
        let text = Section {
            size: code.len() as u64,
            file_range: Some((0, code.len() as u64)),
            ..text_section()
        };
        let items = items
            .into_iter()
            .map(|item| Item { section: 1, ..item })
            .collect();
        Binary::for_tests(vec![null_section(), text], items, code)
    }

    #[test]
    fn an_entry_point_that_runs_on_tail_calls_the_next() {
        // neg esi, which runs on into add's lea eax, [rdi + rsi]; ret.
        let mut items = vec![function("subtract", 0x1000, 6), function("add", 0x1002, 4)];
        let ended = end_where_functions_start(&mut items);
        let binary = x86_64_binary(vec![0xf7, 0xde, 0x8d, 0x04, 0x37, 0xc3], items);
        let references = scan_functions(&binary, &ended);
        let run_on = CodeReference {
            site: 0x1000,
            kind: CodeReferenceKind::TailJump(0x1002),
        };
        assert_eq!(references[&(0x1000, 2)], [run_on]);
        assert_eq!(references[&(0x1002, 4)], []);

        // test esi, esi; jne 0x1004, a conditional jump to the next
        // entry, is one reference to it, not two; and ret runs on nowhere.
        let mut items = vec![
            function("skip", 0x1000, 9),
            function("add", 0x1004, 5),
            function("done", 0x1008, 1),
        ];
        let ended = end_where_functions_start(&mut items);
        let code = vec![0x85, 0xf6, 0x75, 0x00, 0x8d, 0x04, 0x37, 0xc3, 0xc3];
        let binary = x86_64_binary(code, items);
        let references = scan_functions(&binary, &ended);
        let jump = CodeReference {
            site: 0x1002,
            kind: CodeReferenceKind::Jump(0x1004),
        };
        assert_eq!(references[&(0x1000, 4)], [jump]);
        assert_eq!(references[&(0x1004, 4)], []);
    }

    #[test]
    fn missing_sizes_are_estimated_from_the_next_item() {
        let sections = [text_section()];
        let mut items = vec![
            function("a", 0x1000, 0),
            function("b", 0x1030, 0x10),
            function("last", 0x1100, 0),
        ];
        estimate_missing_sizes(&mut items, &sections);
        assert_eq!((items[0].size, items[0].size_estimated), (0x30, true));
        // Nothing follows `last`, so its size stays unknown.
        assert_eq!((items[2].size, items[2].size_estimated), (0, false));
    }
}
