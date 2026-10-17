//! The examples in the docs, run as they are written: each code block whose
//! first line is `$ elfq ...` must show what elfq prints for it.

use crate::support::{assert_output, elfq};

/// The docs that can hold examples, relative to the crate's root.
const DOCS: [&str; 2] = ["README.md", "docs/design.md"];

#[test]
fn the_examples_in_the_docs_show_what_elfq_prints() {
    let mut examples = 0;
    for doc in DOCS {
        let path = format!("{}/{doc}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        // Every other piece between fences is a code block, which starts
        // with the rest of its opening fence's line, such as `json`.
        for block in text.split("```").skip(1).step_by(2) {
            let (_, block) = block.split_once('\n').unwrap();
            let Some(example) = block.strip_prefix("$ elfq ") else {
                continue;
            };
            let (command, expected) = example.split_once('\n').unwrap();
            // Shown with the output of a test that fails.
            println!("{doc}: $ elfq {command}");
            let args: Vec<&str> = command.split_whitespace().collect();
            let output = elfq(&args);
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_output(&String::from_utf8(output.stdout).unwrap(), expected);
            examples += 1;
        }
    }
    // The examples in design.md, so that a change in how blocks are
    // written can't make this test check nothing.
    assert!(examples >= 3, "found {examples} examples");
}
