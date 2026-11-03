use object::{Architecture, Endianness, SectionKind};

use super::*;
use crate::binary::FileKind;

/// The functions found in firmware that runs `text` from its start at
/// 0x1000 and has `rodata` at 0x2000, as (address, size).
fn functions(text: &[u8], rodata: &[u8]) -> Vec<(u64, u64)> {
    functions_with(text, (".rodata", SectionKind::ReadOnlyData, rodata))
}

/// The same, with another section at 0x2000 than `.rodata`: its name,
/// kind and bytes.
fn functions_with(text: &[u8], (name, kind, data): (&str, SectionKind, &[u8])) -> Vec<(u64, u64)> {
    let section = |name: &str, address, kind, offset, bytes: &[u8]| Section {
        name: name.to_string(),
        address,
        size: bytes.len() as u64,
        kind,
        allocated: true,
        file_range: Some((offset, bytes.len() as u64)),
        entry_size: 0,
        load_address: None,
    };
    let binary = Binary {
        architecture: Architecture::Arm,
        pointer_size: 4,
        kind: FileKind::Executable,
        entry: 0x1000,
        thumb: true,
        endianness: Endianness::Little,
        has_symbols: false,
        has_dwarf: false,
        sections: vec![
            section(".text", 0x1000, SectionKind::Text, 0, text),
            section(name, 0x2000, kind, text.len() as u64, data),
        ],
        items: Vec::new(),
        imports: Vec::new(),
        relocations: Vec::new(),
        used_by_unwinder: Vec::new(),
        merged_variables: Vec::new(),
        vector_table: None,
        arm_architecture: None,
        data_in_code: std::sync::OnceLock::new(),
        warnings: Vec::new(),
        code_references: std::collections::HashMap::new(),
        data: [text, data].concat(),
    };
    let mut items = vec![Item {
        name: made_names::ENTRY_NAME.to_string(),
        aliases: Vec::new(),
        kind: ItemKind::Function,
        address: 0x1000,
        size: 0,
        size_estimated: false,
        section: 0,
        source: Source::EntryPoint,
    }];
    add(&binary, &mut items);
    let mut found: Vec<(u64, u64)> = items.iter().map(|item| (item.address, item.size)).collect();
    found.sort_unstable();
    found
}

#[test]
fn a_b_w_that_stays_in_the_function_starts_nothing() {
    // push {r4, lr}; nop; b.w 0x1002, back into the function; pop.
    let looping = [0x10, 0xb5, 0x00, 0xbf, 0xff, 0xf7, 0xfd, 0xbf, 0x10, 0xbd];
    assert_eq!(functions(&looping, &[]), [(0x1000, 10)]);
    // cmp r0, #0; beq 0x100c; b.w 0x100a, which the beq reaches past, as
    // in a long function; movs r0, #1; movs r0, #2; bx lr.
    let long = [
        0x00, 0x28, 0x03, 0xd0, 0x00, 0xf0, 0x01, 0xb8, 0x01, 0x20, 0x02, 0x20, 0x70, 0x47,
    ];
    assert_eq!(functions(&long, &[]), [(0x1000, 14)]);
    // b.w 0x1006, past the function's end: a tail call. Then bx lr, and
    // the function it calls: movs r0, #1; bx lr.
    let tail_call = [0x00, 0xf0, 0x01, 0xb8, 0x70, 0x47, 0x01, 0x20, 0x70, 0x47];
    assert_eq!(functions(&tail_call, &[]), [(0x1000, 6), (0x1006, 4)]);
}

#[test]
fn startup_arrays_are_followed() {
    // movs r0, #1; bx lr; movs r0, #2; bx lr. Only `.init_array`
    // points to 0x1004.
    let code = [0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47];
    let init_array = (".init_array", SectionKind::Unknown, &[0x05, 0x10, 0, 0][..]);
    assert_eq!(
        functions_with(&code, init_array),
        [(0x1000, 4), (0x1004, 4)]
    );
}

#[test]
fn an_address_in_data_starts_a_function_where_the_code_before_ends() {
    // movs r0, #1; bx lr; movs r0, #2; bx lr. Only data points to 0x1004.
    let code = [0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47];
    assert_eq!(
        functions(&code, &[0x05, 0x10, 0, 0]),
        [(0x1000, 4), (0x1004, 4)]
    );
    // movs r0, #1 runs on into 0x1002.
    assert_eq!(functions(&code, &[0x03, 0x10, 0, 0]), [(0x1000, 8)]);
    // movw r0, #0x1234 is one instruction of 4 bytes; bx lr.
    let movw = [0x41, 0xf2, 0x34, 0x20, 0x70, 0x47];
    assert_eq!(functions(&movw, &[0x03, 0x10, 0, 0]), [(0x1000, 6)]);
    // bx lr; nop, which pads; movs r0, #1; bx lr. A function starts
    // after the padding, not in it.
    let padded = [0x70, 0x47, 0x00, 0xbf, 0x01, 0x20, 0x70, 0x47];
    assert_eq!(functions(&padded, &[0x03, 0x10, 0, 0]), [(0x1000, 8)]);
    assert_eq!(
        functions(&padded, &[0x05, 0x10, 0, 0]),
        [(0x1000, 2), (0x1004, 4)]
    );
    // bl 0x1008; bx lr; nop; then the function it calls, bx lr. Data
    // points to the nop, which pads.
    let padded = [0x00, 0xf0, 0x02, 0xf8, 0x70, 0x47, 0x00, 0xbf, 0x70, 0x47];
    assert_eq!(
        functions(&padded, &[0x07, 0x10, 0, 0]),
        [(0x1000, 6), (0x1008, 2)]
    );
    // bl 0x1008, which may never return; movs r0, #1; bx lr; b 0x1008.
    let after_a_call = [0x00, 0xf0, 0x02, 0xf8, 0x01, 0x20, 0x70, 0x47, 0xfe, 0xe7];
    assert_eq!(
        functions(&after_a_call, &[0x05, 0x10, 0, 0]),
        [(0x1000, 4), (0x1004, 4), (0x1008, 2)]
    );
    // ldr r0, [pc, #4]; bx lr; movs r0, #2; bx lr; the word it loads,
    // which the code before 0x1004 no longer finds when cut there.
    let loads = [
        0x01, 0x48, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47, 0x78, 0x56, 0x34, 0x12,
    ];
    assert_eq!(functions(&loads, &[0x05, 0x10, 0, 0]), [(0x1000, 12)]);
}

#[test]
fn an_address_in_data_that_a_jump_crosses_starts_nothing() {
    // cmp r0, #0; beq 0x100a; bx lr; movs r0, #1; bx lr; movs r0, #2;
    // bx lr. The beq jumps past 0x1006.
    let code = [
        0x00, 0x28, 0x02, 0xd0, 0x70, 0x47, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
    ];
    assert_eq!(functions(&code, &[0x07, 0x10, 0, 0]), [(0x1000, 14)]);
    // movs r0, #1; bx lr; subs r0, #1; bne 0x1002; bx lr. The bne
    // jumps back past 0x1004.
    let back = [0x01, 0x20, 0x70, 0x47, 0x01, 0x38, 0xfc, 0xd1, 0x70, 0x47];
    assert_eq!(functions(&back, &[0x05, 0x10, 0, 0]), [(0x1000, 10)]);
}

#[test]
fn an_address_in_data_that_a_jump_lands_on_starts_nothing() {
    // ldr r0, [pc, #0]; b 0x1008, over the word it loads; the word;
    // adds r0, #1; bx lr.
    let island = [
        0x00, 0x48, 0x01, 0xe0, 0x00, 0x00, 0x00, 0x20, 0x01, 0x30, 0x70, 0x47,
    ];
    assert_eq!(functions(&island, &[0x09, 0x10, 0, 0]), [(0x1000, 12)]);
    // cmp r0, #0; beq 0x1008; movs r0, #1; bx lr; movs r0, #2; bx lr.
    let branch = [
        0x00, 0x28, 0x01, 0xd0, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
    ];
    assert_eq!(functions(&branch, &[0x09, 0x10, 0, 0]), [(0x1000, 12)]);
}

#[test]
fn an_address_in_data_at_a_case_of_a_jump_table_starts_nothing() {
    // cmp r0, #1; bhi 0x100a; tbb [pc, r0] with cases at 0x100e and
    // 0x1012. Then for other values, movs r0, #0; bx lr; and the cases,
    // movs r0, #n; bx lr.
    let code = [
        0x01, 0x28, 0x02, 0xd8, 0xdf, 0xe8, 0x00, 0xf0, 0x03, 0x05, 0x00, 0x20, 0x70, 0x47, 0x01,
        0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
    ];
    assert_eq!(functions(&code, &[0x0f, 0x10, 0, 0]), [(0x1000, 22)]);
    assert_eq!(functions(&code, &[0x13, 0x10, 0, 0]), [(0x1000, 22)]);
}

#[test]
fn an_adr_to_a_function_starts_it() {
    // As GNU as assembles `adr r0, function` to a Thumb function, at
    // the function's address + 1: push {r4, lr}; adr.w r0, 0x100d;
    // bl 0x101a; pop {r4, pc}. Then 0x100c: push {r4, lr}; movs r1, #2;
    // bl 0x1016; pop {r4, pc}. 0x1016: movs r0, #1; bx lr. 0x101a: bx
    // lr.
    let code = [
        0x10, 0xb5, 0x0f, 0xf2, 0x09, 0x00, 0x00, 0xf0, 0x08, 0xf8, 0x10, 0xbd, 0x10, 0xb5, 0x02,
        0x21, 0x00, 0xf0, 0x01, 0xf8, 0x10, 0xbd, 0x01, 0x20, 0x70, 0x47, 0x70, 0x47,
    ];
    assert_eq!(
        functions(&code, &[]),
        [(0x1000, 12), (0x100c, 10), (0x1016, 4), (0x101a, 2)]
    );
}

#[test]
fn constants_among_the_code_end_the_function_before_them() {
    // push {r4, lr}; bl 0x1010; ldr r0, =0x1014, the constants; pop
    // {r4, pc}; nop; the word it loads. Then the function it calls,
    // movs r0, #1; bx lr; and the constants.
    let code = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x14, 0x10, 0x00,
        0x00, 0x01, 0x20, 0x70, 0x47, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x00,
    ];
    assert_eq!(functions(&code, &[]), [(0x1000, 16), (0x1010, 4)]);
    // The same, but the constants decode as bl 0x101a, which the code
    // followed again no longer finds.
    let code = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x14, 0x10, 0x00,
        0x00, 0x01, 0x20, 0x70, 0x47, 0x00, 0xf0, 0x01, 0xf8, 0x45, 0x46, 0x47, 0x00,
    ];
    assert_eq!(functions(&code, &[]), [(0x1000, 16), (0x1010, 4)]);
    // bl 0x1010; bx lr; 2 bytes of padding; constants that start with
    // zeros; then the function it calls: ldr r0, =0x1008; bx lr; the
    // word it loads.
    let zeros = [
        0x00, 0xf0, 0x06, 0xf8, 0x70, 0x47, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x48, 0x70, 0x47, 0x08, 0x10, 0x00, 0x00,
    ];
    assert_eq!(functions(&zeros, &[]), [(0x1000, 6), (0x1010, 8)]);
}

#[test]
fn constants_end_a_function_only_where_its_code_ends_there() {
    // push {r4, lr}; bl 0x1010; ldr r0, =0x1016, the constants; pop
    // {r4, pc}; nop; the word it loads. Then the function it calls,
    // movs r0, #1; bl 0x1000, which ends it with a call; and the
    // constants.
    let after_a_call = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x16, 0x10, 0x00,
        0x00, 0x01, 0x20, 0xff, 0xf7, 0xf5, 0xff, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x00,
    ];
    assert_eq!(functions(&after_a_call, &[]), [(0x1000, 16), (0x1010, 14)]);
    // The same up to 0x1010, with ldr r0, =0x1014. Then ldr r0, [pc,
    // #4]; bx lr; the constants; and the word it loads, past them,
    // which the code before them no longer finds when cut there.
    let loads_past = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x14, 0x10, 0x00,
        0x00, 0x01, 0x48, 0x70, 0x47, 0x41, 0x42, 0x43, 0x44, 0x78, 0x56, 0x34, 0x12,
    ];
    assert_eq!(functions(&loads_past, &[]), [(0x1000, 16), (0x1010, 12)]);
    // The same up to 0x1010, with ldr r0, =0x101c. Then adr r1, 0x1018;
    // bx lr; nop; nop; b 0x1010; bx lr; and the constants. Scanned on,
    // the 8 bytes at 0x1018 that adr points to are data. Cut at the
    // constants, they don't fit, so the code before them finds a jump
    // that it doesn't find scanned on.
    let finds_a_jump_alone = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x1c, 0x10, 0x00,
        0x00, 0x01, 0xa1, 0x70, 0x47, 0x00, 0xbf, 0x00, 0xbf, 0xfa, 0xe7, 0x70, 0x47, 0x41, 0x42,
        0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    ];
    assert_eq!(
        functions(&finds_a_jump_alone, &[]),
        [(0x1000, 16), (0x1010, 20)]
    );
}

#[test]
fn tables_among_constants_start_functions() {
    // push {r4, lr}; bl 0x1010; ldr r0, =0x1018; pop {r4, pc}; nop; the
    // word it loads. Then 0x1010: movs r0, #1; bx lr. 0x1014: movs r0,
    // #2; bx lr, which only the table at 0x1018 points to.
    let code = [
        0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x18, 0x10, 0x00,
        0x00, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47, 0x15, 0x10, 0x00, 0x00,
    ];
    assert_eq!(
        functions(&code, &[]),
        [(0x1000, 16), (0x1010, 4), (0x1014, 4)]
    );
}

#[test]
fn what_a_function_points_to_in_itself_ends_nothing() {
    // adr r0, 0x1008; ldr r0, [r0]; bx lr; nop; the word it points to.
    let code = [
        0x01, 0xa0, 0x00, 0x68, 0x70, 0x47, 0x00, 0xbf, 0x78, 0x56, 0x34, 0x12,
    ];
    assert_eq!(functions(&code, &[]), [(0x1000, 12)]);
}

#[test]
fn constants_at_a_case_of_a_jump_table_end_nothing() {
    // 0x1000: push {r4, lr}; bl 0x1028; cmp r0, #2; bls 0x100e; movs
    // r0, #0; pop {r4, pc}. 0x100e: tbb [pc, r0] with cases at 0x1016,
    // 0x101a and 0x101e: movs r0, #10; pop {r4, pc}; movs r0, #20; pop
    // {r4, pc}; bl 0x1024; pop {r4, pc}. 0x1024: movs r0, #1; bx lr.
    // 0x1028: ldr r0, =0x101a; bx lr; the word, which is the second
    // case's address.
    let code = [
        0x10, 0xb5, 0x00, 0xf0, 0x11, 0xf8, 0x02, 0x28, 0x01, 0xd9, 0x00, 0x20, 0x10, 0xbd, 0xdf,
        0xe8, 0x00, 0xf0, 0x02, 0x04, 0x06, 0x00, 0x0a, 0x20, 0x10, 0xbd, 0x14, 0x20, 0x10, 0xbd,
        0x00, 0xf0, 0x01, 0xf8, 0x10, 0xbd, 0x01, 0x20, 0x70, 0x47, 0x00, 0x48, 0x70, 0x47, 0x1a,
        0x10, 0x00, 0x00,
    ];
    assert_eq!(
        functions(&code, &[]),
        [(0x1000, 36), (0x1024, 4), (0x1028, 8)]
    );
}
