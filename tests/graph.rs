//! The reference graph, checked against the answers in testdata/README.md.

use std::path::Path;

use elfq::binary::{Binary, ItemKind, Source};
use elfq::commands::symbol;
use elfq::graph::{Graph, Node, ReferenceKind};

use ReferenceKind::*;

fn load(name: &str) -> (Binary, Graph) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/bin")
        .join(name);
    let binary = Binary::load(&path).unwrap();
    let graph = Graph::build(&binary);
    (binary, graph)
}

/// What uses `name`, and how: one entry per caller and kind, sorted.
fn callers(binary: &Binary, graph: &Graph, name: &str) -> Vec<(String, ReferenceKind)> {
    let node = symbol::find(binary, name).unwrap();
    let mut callers: Vec<(String, ReferenceKind)> = graph
        .references_to(node)
        .map(|r| (binary.items[r.from].name.clone(), r.kind))
        .collect();
    callers.sort();
    callers.dedup();
    callers
}

/// What `name` uses, and how: one entry per callee and kind, sorted.
fn callees(binary: &Binary, graph: &Graph, name: &str) -> Vec<(String, ReferenceKind)> {
    let Node::Item(item) = symbol::find(binary, name).unwrap() else {
        panic!("{name} is an import");
    };
    let mut callees: Vec<(String, ReferenceKind)> = graph
        .references_from(item)
        .iter()
        .map(|r| (r.to.name(binary).to_string(), r.kind))
        .collect();
    callees.sort();
    callees.dedup();
    callees
}

/// The text of a C string: the bytes of the item named `name`, up to the
/// first NUL.
fn text(binary: &Binary, name: &str) -> String {
    let Node::Item(index) = symbol::find(binary, name).unwrap() else {
        panic!("{name} is an import");
    };
    let item = &binary.items[index];
    let bytes = binary.bytes(item.address, item.size).unwrap();
    let end = bytes.iter().position(|&byte| byte == 0).unwrap();
    String::from_utf8(bytes[..end].to_vec()).unwrap()
}

fn pairs(expected: &[(&str, ReferenceKind)]) -> Vec<(String, ReferenceKind)> {
    expected.iter().map(|&(n, k)| (n.to_string(), k)).collect()
}

/// The C sample, built for x86-64 Linux and for Cortex-M.
const C_SAMPLES: [&str; 2] = ["c-x86_64.elf", "c-cortex-m4.elf"];

#[test]
fn a_shared_helper_has_every_caller() {
    for sample in C_SAMPLES {
        let (binary, graph) = load(sample);
        let mut expected = vec![
            ("app_tick", Call),
            ("on_button", TailCall),
            ("process_frame", Call),
            ("report", Call),
        ];
        // The Cortex-M linker removes debug_dump, which nothing calls.
        if sample == "c-x86_64.elf" {
            expected.push(("debug_dump", Call));
            expected.sort();
        }
        assert_eq!(
            callers(&binary, &graph, "log_msg"),
            pairs(&expected),
            "{sample}"
        );
    }
}

#[test]
fn a_function_uses_its_helpers_and_variables() {
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        callees(&binary, &graph, "process_frame"),
        pairs(&[
            // Two log messages; see string_literals_are_unnamed_data.
            (".rodata@0x2012", Address),
            (".rodata@0x201e", Address),
            ("__stack_chk_fail@plt", Call),
            ("apply_filter", Call),
            ("crc32", Call),
            ("decode_header", Call),
            ("dispatch_event", Call),
            ("frames_bad", Access),
            ("frames_ok", Access),
            ("log_msg", Call),
            ("stats", Access),
        ])
    );
    assert_eq!(
        callees(&binary, &graph, "crc32"),
        pairs(&[("crc_table", Address)])
    );

    // Arm code loads a variable's address before using it, so variables
    // have their address taken rather than being accessed.
    let (binary, graph) = load("c-cortex-m4.elf");
    assert_eq!(
        callees(&binary, &graph, "process_frame"),
        pairs(&[
            (".rodata@0x8000900", Address),
            (".rodata@0x800090c", Address),
            ("apply_filter", Call),
            ("crc32", Call),
            ("decode_header", Call),
            ("dispatch_event", Call),
            ("frames_bad", Address),
            ("frames_ok", Address),
            ("log_msg", Call),
            ("stats", Address),
        ])
    );
}

#[test]
fn string_literals_are_unnamed_data() {
    // String literals have no symbols, so elfq names them by section and
    // address. `process_frame` alone uses its two log messages.
    for sample in C_SAMPLES {
        let (binary, graph) = load(sample);
        let mut strings = Vec::new();
        for (name, kind) in callees(&binary, &graph, "process_frame") {
            if name.starts_with(".rodata@") {
                assert_eq!(kind, Address, "{sample}: {name}");
                assert_eq!(
                    callers(&binary, &graph, &name),
                    pairs(&[("process_frame", Address)]),
                    "{sample}: {name}"
                );
                strings.push(text(&binary, &name));
            }
        }
        assert_eq!(strings, ["bad header\n", "bad crc\n"], "{sample}");
    }
}

#[test]
fn functions_called_through_pointers_have_their_address_taken() {
    for sample in C_SAMPLES {
        let (binary, graph) = load(sample);
        for handler in ["on_timer", "on_button", "on_data"] {
            assert_eq!(
                callers(&binary, &graph, handler),
                pairs(&[("main", Address)]),
                "{sample}: {handler}"
            );
        }
        // The call through the table is a tail jump whose target elfq
        // can't know: `jmp rax` on x86-64, `bx r3` on Cortex-M.
        let Node::Item(dispatch) = symbol::find(&binary, "dispatch_event").unwrap() else {
            unreachable!()
        };
        let unresolved = graph.unresolved_in(dispatch);
        assert_eq!(unresolved.len(), 1, "{sample}");
        assert_eq!(
            (unresolved[0].is_call, unresolved[0].target),
            (false, None),
            "{sample}"
        );
        assert_eq!(
            callees(&binary, &graph, "dispatch_event"),
            pairs(&[("handlers", Address)]),
            "{sample}"
        );
    }
}

#[test]
fn recursion_and_cycles_are_references_too() {
    for sample in C_SAMPLES {
        let (binary, graph) = load(sample);
        let calls =
            |from, to: &str| callees(&binary, &graph, from).contains(&(to.to_string(), Call));
        assert!(calls("count_nodes", "count_nodes"), "{sample}");
        assert!(calls("parse_list", "parse_value"), "{sample}");
        assert!(
            callees(&binary, &graph, "parse_value").contains(&("parse_list".to_string(), TailCall)),
            "{sample}"
        );
    }
}

#[test]
fn pointers_stored_in_data_are_references() {
    // On x86-64 the dynamic relocations list them; the Cortex-M file has
    // none, so elfq reads the data instead.
    for sample in C_SAMPLES {
        let (binary, graph) = load(sample);
        assert_eq!(
            callees(&binary, &graph, "root"),
            pairs(&[("inner", Pointer), ("leaf_a", Pointer)]),
            "{sample}"
        );
        assert_eq!(
            callees(&binary, &graph, "inner"),
            pairs(&[("leaf_b", Pointer), ("leaf_c", Pointer)]),
            "{sample}"
        );
    }
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        callers(&binary, &graph, "frame_dummy"),
        pairs(&[(".init_array", Pointer)])
    );
}

#[test]
fn interrupt_handlers_are_reached_through_the_vector_table() {
    let (binary, graph) = load("c-cortex-m4.elf");
    for handler in ["Reset_Handler", "SysTick_Handler", "Default_Handler"] {
        assert_eq!(
            callers(&binary, &graph, handler),
            pairs(&[("vector_table", Pointer)]),
            "{handler}"
        );
    }
    assert_eq!(
        callees(&binary, &graph, "SysTick_Handler"),
        pairs(&[("app_tick", TailCall), ("systick_count", Address)])
    );
    // The compiler turned Reset_Handler's copy and clear loops into calls.
    assert_eq!(
        callers(&binary, &graph, "memcpy"),
        pairs(&[("Reset_Handler", Call), ("apply_filter", Call)])
    );
    assert_eq!(
        callers(&binary, &graph, "memset"),
        pairs(&[("Reset_Handler", Call)])
    );
}

#[test]
fn startup_code_copies_whole_sections_without_using_their_first_items() {
    // Reset_Handler loads where .data and .bss start and end, to copy and
    // clear them. The first variable in each starts at the same address.
    let (binary, graph) = load("c-cortex-m4.elf");
    assert_eq!(
        callees(&binary, &graph, "Reset_Handler"),
        pairs(&[("main", Call), ("memcpy", Call), ("memset", Call)])
    );
    // report loads the same addresses, to read stats and config_sum.
    let report = callees(&binary, &graph, "report");
    assert!(report.contains(&("stats".to_string(), Address)));
    assert!(report.contains(&("config_sum".to_string(), Address)));

    let (binary, graph) = load("rust-cortex-m4.elf");
    assert_eq!(
        callees(&binary, &graph, "Reset"),
        pairs(&[("elfq_sample::app_main", Call)])
    );

    // The same for .itcm, whose first function is SysTick_Handler.
    let (binary, graph) = load("c-cortex-m7.elf");
    assert_eq!(
        callees(&binary, &graph, "Reset_Handler"),
        pairs(&[("main", Call), ("memcpy", Call), ("memset", Call)])
    );
}

#[test]
fn calls_between_flash_and_itcm_go_through_veneers() {
    // ITCM at 0 is too far from flash at 0x30000000 for one branch
    // instruction, so the linker adds a stub that jumps to the target.
    let (binary, graph) = load("c-cortex-m7.elf");
    assert_eq!(
        callers(&binary, &graph, "__crc32_veneer"),
        pairs(&[("process_frame", Call)])
    );
    assert_eq!(
        callers(&binary, &graph, "crc32"),
        pairs(&[("__crc32_veneer", TailCall)])
    );
    // And back, from SysTick_Handler in ITCM to app_tick in flash.
    assert_eq!(
        callees(&binary, &graph, "SysTick_Handler"),
        pairs(&[("__app_tick_veneer", TailCall), ("systick_count", Address)])
    );
    assert_eq!(
        callees(&binary, &graph, "__app_tick_veneer"),
        pairs(&[("app_tick", TailCall)])
    );
}

#[test]
fn imports_are_reached_through_stubs_and_the_global_offset_table() {
    let (binary, graph) = load("c-x86_64.elf");
    // Through the PLT: `platform_write` ends with a jump to `write@plt`.
    assert_eq!(
        callees(&binary, &graph, "platform_write"),
        pairs(&[("write@plt", TailCall)])
    );
    assert_eq!(
        callees(&binary, &graph, "write@plt"),
        pairs(&[("write", TailCall)])
    );
    // Through the GOT: `call [rip + __libc_start_main@GOT]`.
    assert!(callees(&binary, &graph, "_start").contains(&("__libc_start_main".to_string(), Call)));
}

#[test]
fn stripping_keeps_the_calls_between_functions() {
    // Names differ, so compare by address. Stripped files lose their data
    // symbols, which changes how code reaches data, and the few functions
    // that have no unwind information. So compare calls and tail calls
    // between the functions that both files have.
    let shape = |binary: &Binary, graph: &Graph, functions: &[u64]| {
        let function = |index: usize| {
            let item = &binary.items[index];
            (item.kind == ItemKind::Function && functions.contains(&item.address))
                .then_some(item.address)
        };
        let mut shape: Vec<(u64, String, ReferenceKind, u64)> = graph
            .references
            .iter()
            .filter(|r| matches!(r.kind, Call | TailCall))
            .filter_map(|r| {
                let to = match r.to {
                    Node::Item(to) => format!("{:#x}", function(to)?),
                    Node::Import(import) => binary.imports[import].clone(),
                };
                Some((function(r.from)?, to, r.kind, r.site))
            })
            .collect();
        shape.sort();
        shape
    };
    for sample in ["c-x86_64", "rust-x86_64"] {
        let (named, named_graph) = load(&format!("{sample}-nodebug.elf"));
        let (stripped, stripped_graph) = load(&format!("{sample}-stripped.elf"));
        let functions: Vec<u64> = stripped
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Function)
            .map(|item| item.address)
            .collect();
        let named_shape = shape(&named, &named_graph, &functions);
        let stripped_shape = shape(&stripped, &stripped_graph, &functions);
        assert!(named_shape.len() > 50, "{sample}");
        assert_eq!(named_shape, stripped_shape, "{sample}");
    }
}

#[test]
fn stripped_files_still_show_which_variables_code_uses() {
    // Without symbols, variables are unnamed data, found where code uses
    // them. Names differ, so compare by address.
    let (named, named_graph) = load("c-x86_64-nodebug.elf");
    let (stripped, stripped_graph) = load("c-x86_64-stripped.elf");
    let users = |binary: &Binary, graph: &Graph, variable: usize| {
        let mut users: Vec<(u64, ReferenceKind)> = graph
            .references_to(Node::Item(variable))
            .map(|r| (binary.items[r.from].address, r.kind))
            .collect();
        users.sort();
        users
    };
    for name in ["crc_table", "fir_coeffs", "frames_ok", "handlers", "inner"] {
        let Node::Item(variable) = symbol::find(&named, name).unwrap() else {
            unreachable!()
        };
        let address = named.items[variable].address;
        let unnamed = stripped.item_at(address).unwrap();
        let item = &stripped.items[unnamed];
        assert_eq!((item.address, item.source), (address, Source::Reference));
        assert_eq!(
            users(&stripped, &stripped_graph, unnamed),
            users(&named, &named_graph, variable),
            "{name}"
        );
    }
}

#[test]
fn calls_through_trait_objects_are_indirect() {
    for sample in ["rust-x86_64-nodebug.elf", "rust-cortex-m4.elf"] {
        let (binary, graph) = load(sample);
        let Node::Item(process_frame) = symbol::find(&binary, "process_frame").unwrap() else {
            panic!("process_frame is an import");
        };
        let unresolved = graph.unresolved_in(process_frame);
        assert_eq!(unresolved.len(), 2, "{sample}");
        assert!(
            unresolved.iter().all(|u| u.is_call && u.target.is_none()),
            "{sample}"
        );
    }

    // Each implementation is reached from its vtable, which `app_main`
    // uses. On x86-64 the vtables have no symbols, so they are unnamed data
    // such as `.data.rel.ro@0x52da8`; on Cortex-M they have local symbols.
    for sample in ["rust-x86_64-nodebug.elf", "rust-cortex-m4.elf"] {
        let (binary, graph) = load(sample);
        for implementation in ["Gain::run", "DcBlock::run"] {
            let [(vtable, Pointer)] = &callers(&binary, &graph, implementation)[..] else {
                panic!("{sample}: {implementation}");
            };
            assert_eq!(
                callers(&binary, &graph, vtable),
                pairs(&[("elfq_sample::app_main", Address)]),
                "{sample}: {vtable}"
            );
        }
    }
}
