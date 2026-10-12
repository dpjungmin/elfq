# Contributing to elfq

## Build and test

```sh
cargo build
cargo test
cargo run -- testdata/bin/c-x86_64.elf
cargo run -- testdata/bin/c-x86_64.elf callers log_msg
```

`elfq FILE debug items` lists every function and data object elfq found and
where it found it. It is hidden from `--help` because it is only for
developing elfq.

The tests read the prebuilt sample binaries in `testdata/bin`, so you need no
cross compilers. Before opening a pull request, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Where things live

[docs/design.md](docs/design.md) explains how elfq works and why. In short:

| Path | What it holds |
|---|---|
| `src/main.rs` | The program: parses arguments, runs one command, prints errors. |
| `src/cli.rs` | Command-line arguments, one struct per subcommand. |
| `src/commands/` | One file per subcommand, plus `symbol.rs`, which works out what a user means by SYMBOL. Commands connect a question to output; they do no analysis. |
| `src/arch/` | Finding the addresses that machine code calls or uses, one file per instruction set (x86-64 and Thumb). |
| `src/binary/` | Reading an ELF file: its functions and data objects ("items"), their names, imports and stored pointers. Unnamed data, such as string literals, is found from what code and pointers refer to. |
| `src/graph/` | The reference graph that connects items, the roots where the program starts, the dominator tree that says what is exclusive to what, and the shortest paths that `why` shows. |
| `src/render/` | Text output. |
| `tests/` | Integration tests that run on the sample binaries. |
| `testdata/` | The sample programs and the ELF files built from them. |

Analysis flows one way: `arch`, then `binary`, then `graph`, each importing
only from the ones before it. `render` knows nothing about ELF files, and
`commands` connect the two. A command does no analysis of its own and the
analysis modules never print.

## Adding a subcommand

Commands take the form `elfq FILE <command> [ARGS]`.

1. Add its arguments to `src/cli.rs`.
2. Add `src/commands/<name>.rs` with a `run` function, and call it from
   `src/commands/mod.rs`. A command that takes a SYMBOL turns it into an
   item with `symbol::find`.
3. Add tests under `tests/` that run it on files in `testdata/bin`.

## Adding an instruction set

1. Add `src/arch/<name>.rs` with a `scan` function that reports the calls,
   jumps and addresses in one function's code, and call it from
   `src/arch/mod.rs`.
2. Add sample binaries for it, and tests that check `callers` and `callees`
   against the answers in [testdata/README.md](testdata/README.md).

## Sample binaries

[testdata/README.md](testdata/README.md) lists what each sample contains and
the answers tests should expect. If you change a sample's source, rebuild with
`testdata/build.sh` and commit the new files with the source change.

## Pull requests

Keep them small and focused, ideally one step of the delivery plan in
[docs/design.md](docs/design.md) at a time.
