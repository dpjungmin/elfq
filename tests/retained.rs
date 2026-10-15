//! Roots, dominators and what is exclusive to what, checked against the
//! answers in testdata/README.md.

use std::path::Path;

use elfq::binary::{Binary, ItemKind, Source};
use elfq::commands::symbol;
use elfq::graph::{DominatorTree, Graph, Node, RootKind};

fn load(name: &str) -> (Binary, Graph, DominatorTree) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/bin")
        .join(name);
    let binary = Binary::load(&path).unwrap();
    let graph = Graph::build(&binary);
    let tree = DominatorTree::build(&binary, &graph, None);
    (binary, graph, tree)
}

fn node(binary: &Binary, name: &str) -> Node {
    symbol::find(binary, name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// An item's name, or for a string literal without a symbol, the string
/// in quotes, which doesn't depend on where the compiler put it.
fn describe(binary: &Binary, node: Node) -> String {
    let Node::Item(index) = node else {
        return node.name(binary).to_string();
    };
    let item = &binary.items[index];
    if item.source == Source::Reference
        && let Some(bytes) = binary.bytes(item.address, item.size)
        && let Some(end) = bytes.iter().position(|&byte| byte == 0)
        && let Ok(text) = std::str::from_utf8(&bytes[..end])
    {
        return format!("{text:?}");
    }
    item.name.clone()
}

/// The items exclusive to `name`, described and sorted.
fn exclusive(binary: &Binary, tree: &DominatorTree, name: &str) -> Vec<String> {
    let mut exclusive: Vec<String> = tree
        .exclusive(node(binary, name))
        .into_iter()
        .filter(|node| matches!(node, Node::Item(_)))
        .map(|node| describe(binary, node))
        .collect();
    exclusive.sort();
    exclusive
}

fn parent(binary: &Binary, tree: &DominatorTree, name: &str) -> Option<String> {
    tree.parent(node(binary, name))
        .map(|parent| parent.name(binary).to_string())
}

const C_SAMPLES: [&str; 3] = ["c-x86_64.elf", "c-x86_64-nodebug.elf", "c-cortex-m4.elf"];

#[test]
fn process_frame_keeps_its_helpers_tables_and_messages() {
    for sample in C_SAMPLES {
        let (binary, _, tree) = load(sample);
        assert_eq!(
            exclusive(&binary, &tree, "process_frame"),
            [
                "\"bad crc\\n\"",
                "\"bad header\\n\"",
                "apply_filter",
                "crc32",
                "crc_table",
                "decode_header",
                "filter_tap",
                "fir_coeffs",
            ],
            "{sample}"
        );
        // Its retained size is its own plus theirs.
        let process_frame = node(&binary, "process_frame");
        let size = |node| match node {
            Node::Item(index) => binary.items[index].size,
            Node::Import(_) => 0,
        };
        let below: u64 = tree.exclusive(process_frame).into_iter().map(size).sum();
        assert_eq!(
            tree.retained(process_frame),
            size(process_frame) + below,
            "{sample}"
        );
    }
}

#[test]
fn each_item_hangs_under_what_alone_leads_to_it() {
    for sample in C_SAMPLES {
        let (binary, _, tree) = load(sample);
        let parent = |name| parent(&binary, &tree, name);
        assert_eq!(parent("crc_table").as_deref(), Some("crc32"), "{sample}");
        assert_eq!(parent("format_u32").as_deref(), Some("report"), "{sample}");
        // Shared by several functions, so only `main` leads to it. On
        // Cortex-M the SysTick interrupt also calls it, through `app_tick`,
        // so nothing does.
        let expected = (!sample.contains("cortex-m")).then_some("main");
        assert_eq!(parent("log_msg").as_deref(), expected, "{sample}");
        // Reached through the `handlers` table, which `main` fills in.
        assert_eq!(parent("on_timer").as_deref(), Some("main"), "{sample}");
    }
}

#[test]
fn dead_code_is_unreachable() {
    let (binary, _, tree) = load("c-x86_64.elf");
    assert!(!tree.is_reachable(node(&binary, "debug_dump")));
    // Its message string, which nothing else uses.
    let mut unreachable: Vec<String> = (0..binary.items.len())
        .map(Node::Item)
        .filter(|&node| !tree.is_reachable(node))
        .map(|node| describe(&binary, node))
        .collect();
    unreachable.sort();
    assert_eq!(unreachable, ["\"debug: \"", "_IO_stdin_used", "debug_dump"]);
}

#[test]
fn firmware_starts_from_its_vector_table() {
    let (binary, graph, tree) = load("c-cortex-m4.elf");
    let roots: Vec<(&str, RootKind)> = graph
        .roots
        .iter()
        .map(|root| (root.node.name(&binary), root.kind))
        .collect();
    assert_eq!(
        roots,
        [
            ("vector_table", RootKind::VectorTable),
            ("Default_Handler", RootKind::Handler),
            ("Reset_Handler", RootKind::Entry),
            ("SysTick_Handler", RootKind::Handler),
        ]
    );
    // The processor starts each handler, so nothing else leads to them.
    assert_eq!(parent(&binary, &tree, "SysTick_Handler"), None);
    // `main` and the SysTick interrupt both call `app_tick`, so it isn't
    // exclusive to either.
    assert_eq!(parent(&binary, &tree, "app_tick"), None);
}

#[test]
fn everything_in_the_samples_is_reachable_but_dead_code() {
    for sample in [
        "c-cortex-m4.elf",
        "rust-cortex-m4.elf",
        "rust-x86_64-nodebug.elf",
    ] {
        let (binary, _, tree) = load(sample);
        let unreachable: Vec<&str> = (0..binary.items.len())
            .filter(|&index| !tree.is_reachable(Node::Item(index)))
            .map(|index| binary.items[index].name.as_str())
            .collect();
        assert_eq!(unreachable, [] as [&str; 0], "{sample}");
    }
}

#[test]
fn the_unwinder_and_the_dynamic_loader_are_starting_points() {
    let (binary, graph, _) = load("rust-x86_64-nodebug.elf");
    let roots: Vec<(&str, String)> = graph
        .roots
        .iter()
        .map(|root| {
            let kind = match root.kind {
                RootKind::InSection(section) => format!("in {}", binary.sections[section].name),
                RootKind::ListedIn(section) => {
                    format!("listed in {}", binary.sections[section].name)
                }
                kind => format!("{kind:?}"),
            };
            (root.node.name(&binary), kind)
        })
        .collect();
    for root in [
        ("_start", "Entry"),
        ("rust_eh_personality", "Personality"),
        (".plt", "PltHeader"),
        ("_init", "in .init"),
        ("_fini", "in .fini"),
        ("frame_dummy", "listed in .init_array"),
        ("__do_global_dtors_aux", "listed in .fini_array"),
    ] {
        let root = (root.0, root.1.to_string());
        assert!(roots.contains(&root), "{root:?} in {roots:?}");
    }
}

#[test]
fn merged_variables_are_all_used_through_the_block_address() {
    // LLVM merged these four statics into one block, which code reaches
    // through the address of the first.
    let (binary, _, tree) = load("rust-cortex-m4.elf");
    for name in ["FRAMES_BAD", "SAMPLE_COUNT", "PEAK", "FRAMES_OK"] {
        assert!(tree.is_reachable(node(&binary, name)), "{name}");
    }
}

#[test]
fn trait_implementations_belong_to_the_code_that_makes_the_trait_objects() {
    // `app_main` takes the vtables' addresses; each implementation hangs
    // under its vtable, which hangs under `app_main`.
    for sample in ["rust-x86_64-nodebug.elf", "rust-cortex-m4.elf"] {
        let (binary, _, tree) = load(sample);
        for implementation in ["Gain::run", "DcBlock::run"] {
            let vtable = tree.parent(node(&binary, implementation)).unwrap();
            assert_eq!(
                tree.parent(vtable).map(|n| n.name(&binary).to_string()),
                Some("elfq_sample::app_main".to_string()),
                "{sample}: {implementation}"
            );
        }
    }
}

#[test]
fn stripping_keeps_the_exclusive_functions() {
    // Names differ, so compare the addresses of the functions exclusive to
    // `process_frame`.
    let functions = |binary: &Binary, tree: &DominatorTree, address: u64| {
        let node = Node::Item(binary.item_at(address).unwrap());
        let mut functions: Vec<u64> = tree
            .exclusive(node)
            .into_iter()
            .filter_map(|node| match node {
                Node::Item(index) if binary.items[index].kind == ItemKind::Function => {
                    Some(binary.items[index].address)
                }
                _ => None,
            })
            .collect();
        functions.sort();
        functions
    };
    for sample in [
        "c-x86_64",
        "rust-x86_64",
        "c-cortex-m4",
        "c-cortex-m4-cmsis",
        "c-cortex-m7",
        "rust-cortex-m4",
    ] {
        let (named, _, named_tree) = load(&format!("{sample}-nodebug.elf"));
        let (stripped, _, stripped_tree) = load(&format!("{sample}-stripped.elf"));
        let Node::Item(process_frame) = node(&named, "process_frame") else {
            unreachable!()
        };
        let address = named.items[process_frame].address;
        let expected = functions(&named, &named_tree, address);
        assert!(expected.len() >= 4, "{sample}");
        assert_eq!(
            functions(&stripped, &stripped_tree, address),
            expected,
            "{sample}"
        );
    }
}

#[test]
fn cpp_virtual_functions_and_constructors_have_their_starting_points() {
    let (binary, graph, tree) = load("cpp-x86_64.elf");
    // `main` makes the objects, so it takes the vtables' addresses; each
    // override hangs under its class's vtable, as in Rust.
    for class in ["Gain", "DcBlock"] {
        assert_eq!(
            parent(&binary, &tree, &format!("{class}::run")),
            Some(format!("vtable for app::{class}"))
        );
        assert_eq!(
            parent(&binary, &tree, &format!("vtable for {class}")),
            Some("main".to_string())
        );
    }
    // A global object's constructor runs from a function that GCC lists in
    // .init_array.
    let constructor = tree.parent(node(&binary, "load_defaults")).unwrap();
    let root = graph
        .roots
        .iter()
        .find(|root| root.node == constructor)
        .unwrap();
    let RootKind::ListedIn(section) = root.kind else {
        panic!("{root:?}");
    };
    assert_eq!(binary.sections[section].name, ".init_array");
    assert_eq!(
        exclusive(&binary, &tree, "process_frame"),
        [
            "\"bad crc\\n\"",
            "\"bad header\\n\"",
            "app::(anonymous namespace)::crc32(unsigned char const*, unsigned long)",
            "app::(anonymous namespace)::crc_table",
            "app::frames_bad"
        ]
    );
}
