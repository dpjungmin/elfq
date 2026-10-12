//! The shortest chains from the roots that `why` shows, checked against the
//! samples' call graphs in testdata/README.md.

use std::path::Path;

use elfq::binary::Binary;
use elfq::commands::symbol;
use elfq::graph::{Graph, Node, ShortestPaths};

fn load(name: &str) -> (Binary, Graph) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/bin")
        .join(name);
    let binary = Binary::load(&path).unwrap();
    let graph = Graph::build(&binary);
    (binary, graph)
}

fn node(binary: &Binary, name: &str) -> Node {
    symbol::find(binary, name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// The names along the shortest path to `to` that doesn't pass through
/// `avoid`, from its root to `to`, or `None` if there is no such path.
fn chain(binary: &Binary, graph: &Graph, avoid: &str, to: &str) -> Option<Vec<String>> {
    let paths = ShortestPaths::avoiding(binary, graph, node(binary, avoid));
    let path = paths.path_to(node(binary, to))?;
    let mut names: Vec<String> = path
        .iter()
        .map(|reference| binary.items[reference.from].name.clone())
        .collect();
    names.push(to.to_string());
    Some(names)
}

#[test]
fn crc_table_is_kept_by_the_chain_through_process_frame() {
    for (sample, entry) in [
        ("c-x86_64.elf", "_start"),
        ("c-x86_64-nodebug.elf", "_start"),
        ("c-cortex-m4.elf", "Reset_Handler"),
    ] {
        let (binary, graph) = load(sample);
        assert_eq!(
            chain(&binary, &graph, "crc_table", "crc32").unwrap(),
            [entry, "main", "process_frame", "crc32"],
            "{sample}"
        );
    }
}

#[test]
fn an_interrupt_is_the_shortest_way_to_app_tick_on_cortex_m() {
    let (binary, graph) = load("c-cortex-m4.elf");
    assert_eq!(
        chain(&binary, &graph, "log_msg", "app_tick").unwrap(),
        ["SysTick_Handler", "app_tick"]
    );
}

#[test]
fn a_function_called_back_does_not_keep_its_caller() {
    // `parse_list` is there only because `parse_value` calls it, so it
    // doesn't keep `parse_value` in the file, though it calls it back.
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(chain(&binary, &graph, "parse_value", "parse_list"), None);
    assert_eq!(
        chain(&binary, &graph, "parse_value", "main").unwrap(),
        ["_start", "main"]
    );
}

#[test]
fn chains_prefer_calls_to_taking_addresses() {
    // `main` takes the address of `on_button`, which tail-calls `log_msg`,
    // and calls `process_frame`, which calls it: as short, but all calls.
    let (binary, graph) = load("c-x86_64.elf");
    assert_eq!(
        chain(&binary, &graph, "strlen", "log_msg").unwrap(),
        ["_start", "main", "process_frame", "log_msg"]
    );
}

#[test]
fn trait_implementations_are_reached_through_their_vtables() {
    let (binary, graph) = load("rust-cortex-m4.elf");
    let paths = ShortestPaths::avoiding(&binary, &graph, node(&binary, "process_frame"));
    let path = paths.path_to(node(&binary, "Gain::run")).unwrap();
    let names: Vec<&str> = path
        .iter()
        .map(|reference| binary.items[reference.from].name.as_str())
        .collect();
    assert_eq!(names[..2], ["Reset", "elfq_sample::app_main"]);
    assert!(names[2].starts_with(".Lanon."), "{names:?}");
}
