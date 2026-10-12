# Contributing to elfq

## Build and test

```sh
cargo build
cargo test
cargo run -- debug items testdata/bin/c-x86_64.elf
```

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
| `src/commands/` | One file per subcommand. Commands connect a question to output; they do no analysis. |
| `src/binary/` | Reading an ELF file and finding its functions and data objects ("items"). |
| `src/render/` | Text output. |
| `tests/` | Integration tests that run on the sample binaries. |
| `testdata/` | The sample programs and the ELF files built from them. |

Analysis flows one way: `binary`, then (from step 2b) `arch` and `graph`, each
importing only from the ones before it. `render` knows nothing about ELF
files, and `commands` connect the two. A command does no analysis of its own
and the analysis modules never print.

## Adding a subcommand

1. Add its arguments to `src/cli.rs`.
2. Add `src/commands/<name>.rs` with a `run` function, and call it from
   `src/commands/mod.rs`.
3. Add tests under `tests/` that run it on files in `testdata/bin`.

## Sample binaries

[testdata/README.md](testdata/README.md) lists what each sample contains and
the answers tests should expect. If you change a sample's source, rebuild with
`testdata/build.sh` and commit the new files with the source change.

## Pull requests

Keep them small and focused, ideally one step of the delivery plan in
[docs/design.md](docs/design.md) at a time.
