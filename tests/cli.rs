//! Runs the elfq program the way a user would.

use std::process::{Command, Output};

fn elfq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfq"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap()
}

#[test]
fn debug_items_lists_each_item() {
    let output = elfq(&["debug", "items", "testdata/bin/c-x86_64.elf"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("x86-64, "), "{stdout}");
    let line = stdout
        .lines()
        .find(|line| line.ends_with(" process_frame"))
        .unwrap();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields,
        ["0x1700", "532", "function", "symtab", "process_frame"]
    );
}

#[test]
fn errors_go_to_stderr_with_a_failing_exit_code() {
    let output = elfq(&["debug", "items", "Cargo.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "error: cannot load Cargo.toml: not an ELF file\n");
}
