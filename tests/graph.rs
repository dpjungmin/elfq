//! The reference graph, checked against the answers in testdata/README.md.

use std::path::Path;

use elfq::binary::{Binary, ItemKind};
use elfq::graph::{self, Graph, Node, ReferenceKind};

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
    let node = graph::find(binary, name).unwrap();
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
    let Node::Item(item) = graph::find(binary, name).unwrap() else {
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

fn pairs(expected: &[(&str, ReferenceKind)]) -> Vec<(String, ReferenceKind)> {
    expected.iter().map(|&(n, k)| (n.to_string(), k)).collect()
}

#[test]
fn a_shared_helper_has_every_caller() {
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        callers(&binary, &graph, "log_msg"),
        pairs(&[
            ("app_tick", Call),
            ("debug_dump", Call),
            ("on_button", TailCall),
            ("process_frame", Call),
            ("report", Call),
        ])
    );
}

#[test]
fn a_function_uses_its_helpers_and_variables() {
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        callees(&binary, &graph, "process_frame"),
        pairs(&[
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
}

#[test]
fn functions_called_through_pointers_have_their_address_taken() {
    let (binary, graph) = load("c-x86_64.elf");
    for handler in ["on_timer", "on_button", "on_data"] {
        assert_eq!(
            callers(&binary, &graph, handler),
            pairs(&[("main", Address)]),
            "{handler}"
        );
    }
    // The call through the table is a jump whose target elfq can't know.
    let Node::Item(dispatch) = graph::find(&binary, "dispatch_event").unwrap() else {
        unreachable!()
    };
    let unresolved = graph.unresolved_in(dispatch);
    assert_eq!(unresolved.len(), 1);
    assert_eq!((unresolved[0].is_call, unresolved[0].target), (false, None));
    assert_eq!(
        callees(&binary, &graph, "dispatch_event"),
        pairs(&[("handlers", Address)])
    );
}

#[test]
fn recursion_and_cycles_are_references_too() {
    let (binary, graph) = load("c-x86_64.elf");
    assert!(callers(&binary, &graph, "count_nodes").contains(&("count_nodes".to_string(), Call)));
    assert!(
        callees(&binary, &graph, "parse_value").contains(&("parse_list".to_string(), TailCall))
    );
    assert!(callees(&binary, &graph, "parse_list").contains(&("parse_value".to_string(), Call)));
}

#[test]
fn pointers_stored_in_data_are_references() {
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        callees(&binary, &graph, "root"),
        pairs(&[("inner", Pointer), ("leaf_a", Pointer)])
    );
    assert_eq!(
        callees(&binary, &graph, "inner"),
        pairs(&[("leaf_b", Pointer), ("leaf_c", Pointer)])
    );
    assert_eq!(
        callers(&binary, &graph, "frame_dummy"),
        pairs(&[(".init_array", Pointer)])
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
fn calls_through_trait_objects_are_indirect() {
    let (binary, graph) = load("rust-x86_64-nodebug.elf");
    let process_frame = binary
        .items
        .iter()
        .position(|i| i.name.ends_with("11elfq_sample13process_frame"))
        .unwrap();
    let unresolved = graph.unresolved_in(process_frame);
    assert_eq!(unresolved.len(), 2);
    assert!(unresolved.iter().all(|u| u.is_call && u.target.is_none()));
}

#[test]
fn find_takes_names_addresses_and_import_names() {
    let (binary, _) = load("c-x86_64.elf");
    let name = |query| {
        graph::find(&binary, query)
            .unwrap()
            .name(&binary)
            .to_string()
    };
    assert_eq!(name("process_frame"), "process_frame");
    assert_eq!(name("0x1700"), "process_frame");
    assert_eq!(name("0x1705"), "process_frame");
    // An imported function means its stub, which is what code calls.
    assert_eq!(name("write"), "write@plt");
    assert_eq!(name("__libc_start_main"), "__libc_start_main");

    let error = |query| graph::find(&binary, query).unwrap_err().to_string();
    assert_eq!(
        error("proces_frame"),
        "no function or data object is named `proces_frame`; did you mean process_frame?"
    );
    assert_eq!(
        error("0x10"),
        "no function or data object contains address 0x10"
    );
}
