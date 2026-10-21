# Contributing to elfq

[docs/design.md](docs/design.md) explains what elfq answers and how: start
with its Concepts and its Life of a command. This file says where the code
is, the rules it keeps, how it is tested, and how to add a command or an
instruction set.

## Build and test

elfq needs Rust 1.88 or later.

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
cargo clippy --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
cargo test --locked
cargo +1.88 check --all-targets --locked
```

The last line checks that elfq still compiles with Rust 1.88, the oldest
version it supports (`rust-version` in `Cargo.toml`); `rustup toolchain
install 1.88` gets it.

## Where things live

| Path | What it holds |
|---|---|
| `src/main.rs` | The program: parses arguments, runs one command on a thread with a large stack, prints errors and sets the exit status. It also turns `elfq help COMMAND` into `--help`, and suggests the right order when the command comes before the file. |
| `src/lib.rs` | Declares the modules below. elfq is a library plus that small program; the library exists for the program and its tests. |
| `src/cli.rs` | Command-line arguments, with clap: a `Command` variant for each command, the structs that hold their arguments, and the text of `--help`. |
| `src/commands/` | One file per command, which looks up SYMBOL, asks `graph` or `debuginfo` and decides what the answer shows. `mod.rs` runs the command the arguments name, after checking arguments that don't go together. The rest are shared: `symbol.rs` works out what a user means by SYMBOL, `retained_tree.rs` builds the trees of retained sizes that the overview, `top` and `retained` show, `ranges.rs` answers `callers` and `callees` for ranges of memory, `text.rs` holds the wording that commands share and `json.rs` the JSON shapes. |
| `src/arch/` | Finding the addresses that machine code calls or uses, one module per instruction set (x86-64 and Thumb). Thumb's has three parts: `thumb/mod.rs` reads each instruction on its own, `thumb/decode.rs` says what an instruction does with the registers and the stack, and `thumb/values/` follows the addresses that code puts in registers to where a branch through one goes, as for long calls and veneers. |
| `src/binary/` | Reading an ELF file: its functions and data objects ("items") and where they were found, their names, imports, stored pointers and memory blocks. Unnamed data, such as string literals, is found from what code and pointers refer to. Each function's code is scanned once, as the file loads (`Binary::code_references`). |
| `src/graph/` | The reference graph that connects items, the roots where the program starts, the dominator tree that says what is exclusive to what, the shortest paths that `why` shows, and the references that cross the boundary of ranges of memory. |
| `src/debuginfo/` | Reading DWARF, when the file has it: the copies of functions that the compiler inlined, for `inlined`, and for the error when SYMBOL names a function that the compiler inlined everywhere. |
| `src/render/` | Text output: tables, and the trees in their last column (`tree.rs`), which every command shows the same way: as rows that name the row they hang under (in `why`, only where chains branch), or drawn with `--tree`. |
| `tests/it/` | Integration tests, built as one test program (`main.rs` lists its modules). See [Tests](#tests). |
| `testdata/` | The sample programs and the ELF files built from them. See [Sample binaries](#sample-binaries). |
| `docs/` | `design.md`, how elfq works and what comes next; `examples.md`, real questions about the sample binaries, each with the command that answers it and what elfq prints; and `survey.md`, how existing tools compare, written before elfq existed. |

## Rules that keep it approachable

Each module uses only the ones below it in this list:

```
main.rs      the program
commands/    one module per command
cli.rs       arguments
render/      tables and trees
debuginfo/   DWARF
graph/       references, roots, the dominator tree, paths, crossings
binary/      items
arch/        machine code
```

So `arch` knows nothing about items, `binary` nothing about the graph, and
`render` nothing about ELF files. No tool checks this; a `use crate::` line
that reaches up the list is a sign that the code belongs lower down.

- **Analysis never prints.** `arch`, `binary`, `graph` and `debuginfo`
  return what they find. Problems found while loading a file go into
  `Binary::warnings`, which `commands::run` prints. Only the commands and
  `main.rs` write output.
- **Commands decide what an answer shows.** A command looks up SYMBOL, asks
  `graph` or `debuginfo` for the facts, and picks, groups, sorts and words
  what to show. Scanning code, dominators, shortest paths and crossings
  live in the modules below. A small query that only one command needs can
  stay in its file, as `retained`'s list of what SYMBOL shares does; one
  that another command could use belongs in `graph`.
- **Library code returns errors; it doesn't panic or exit on bad input.**
  Add context where the error happens, with anyhow's `context`, as
  `Binary::load` adds "cannot load FILE". `main.rs` prints `error: ` and
  the whole chain, and exits with status 2. `unwrap`, `expect` and `panic!`
  are for tests; an assertion outside them, such as the one in
  `Table::row`, catches only elfq's own mistakes. `unsafe` code is
  forbidden (`Cargo.toml`).
- **A damaged file is read as far as it makes sense.** As elfq loads a
  file, it ignores an allocated section that would end past the last
  address, with a warning, and reads a symbol whose size would take it
  past the last address as if it had no size. One whose last byte is the
  very last address is read one byte shorter. So the rest of the code can
  add a size to an address without checking. Stored pointers are read only
  where the file holds bytes, however large a section claims to be, and
  sums of sizes stop at the largest number rather than overflow.

## Tests

`cargo test` runs two kinds of tests:

- **Unit tests** sit beside the code, in a `tests` module at the end of a
  file. They check one function: a decoder on bytes from a sample's
  disassembly, or an algorithm on a small graph built by hand
  (`graph/dominators.rs`, `graph/paths.rs`).
- **Integration tests** in `tests/it/` use the samples in `testdata/bin`.
  `cli.rs`, `json.rs` and `docs.rs` run the elfq program, the way a user
  would. The others call the library, as `Binary::load` and
  `Graph::build`. `damaged.rs` does both, and so does one test in
  `json.rs`. The answers they expect are
  those in [testdata/README.md](testdata/README.md), which says, for
  example, what calls `log_msg` in each sample.

| File in `tests/it/` | What it tests |
|---|---|
| `support.rs` | No tests: the samples' paths (`SAMPLE` and others), `elfq`, which runs the program, and `assert_output` |
| `cli.rs` | Each command's whole text output, errors, exit statuses and help |
| `json.rs` | `--json` output |
| `items.rs` | Finding items: symbols, PLT stubs, unwind tables, functions in stripped firmware, unnamed data |
| `graph.rs` | The reference graph: callers, callees, pointers in data, veneers |
| `retained.rs` | Roots, and what is exclusive to what |
| `why.rs` | The shortest chains |
| `symbol.rs` | Looking up SYMBOL |
| `inlined.rs` | Inlined copies, from DWARF |
| `damaged.rs` | Damaged and unusual files, made by changing a sample's bytes |
| `docs.rs` | The `$ elfq` examples in README.md, docs/design.md and docs/examples.md, and that design.md names every JSON key |

Two tests in `cli.rs` run the overview and every command it suggests on
each `.elf` and `.so` file in `testdata/bin`, and `top` on each `.elf`
file, so a new sample is tested as soon as it is committed.

To run one test, one file, or the unit tests of one module:

```sh
cargo test --test it cli::top_shows_what_keeps_the_most_bytes
cargo test --test it damaged::
cargo test --lib arch::thumb
```

**When output changes.** Most tests in `cli.rs` check a command's whole
output with `assert_output`. When you change what elfq prints on purpose,
the test that fails shows the first line that differs, then the new output
in full, after "The whole output:". Check it, then paste it over the
expected text, which starts at the beginning of the line after `"\`. Some
expected texts are `format!` templates, with placeholders such as `{path}`;
put those back. A test that checks several outputs stops at the first that
differs, so run it again until it passes.

**The examples in the docs.** A code block in README.md, docs/design.md or
docs/examples.md whose first line starts with `$ elfq ` is run by
`docs.rs`, which checks that elfq prints the rest of the block and nothing
on stderr. The command is split at spaces, with no shell quoting, and its
exit status isn't checked. The text around an example often repeats its
numbers and names, as docs/examples.md does, so when an example's output
changes, check that the text around it still matches the new output.

**Damaged files.** `damaged.rs` makes its files from a sample's bytes, so
that the hangs and crashes they once caused stay fixed. Tests run in a
debug build, so arithmetic that overflows fails them too. A test that uses
`parse_quickly` fails if loading the file takes more than 10 seconds: add
one when you make elfq quick to load a file that took far too long.

## Adding a command

Commands take the form `elfq FILE <command> [ARGS]`. `src/commands/top.rs`
is a short one to copy.

1. **Arguments, in `src/cli.rs`.** Add a variant to `Command`, before the
   hidden `Debug`. Its name, in lowercase with `-` between words (`SizeOf`
   is typed `size-of`), is the command's name, and its `///` line is what
   `elfq --help` lists. Give it an `after_help` with a longer explanation,
   as the others have, and a line in `EXAMPLES`. Its
   arguments go in a struct of their own, or in `SymbolArgs` if SYMBOL is
   all it takes. `--json`, `--tree` and `--range` are global, so don't
   declare them again. `elfq help COMMAND` and the hint for a command typed before the
   file find the new command by themselves.
2. **The command, in `src/commands/<name>.rs`.** Start with a `//!` line
   that shows how to type it, then a `run` function that takes the
   `Binary`, the arguments, the `Format` and the output. It returns
   `Result<()>`, or `Result<Outcome>` when the exit status can be 1, as
   `callers` and `callees` do for ranges.
3. **Wiring, in `src/commands/mod.rs`.** Add `mod <name>;`, an arm in `run`
   and an arm in `command_name`; the compiler asks for the last two. It
   won't ask about `check_arguments`, which refuses `--range` for every
   command but the overview, `top` and `retained`. A command that takes
   `--range` goes in its first arm, and its arm in `run` passes it `range`.
   Then name it in the error further down that lists the commands that take
   `--range`, in the tests in `tests/it/cli.rs` that check that error, and
   in the `--range` help in `cli.rs`, README.md and docs/design.md. A
   command that can't print JSON needs an arm like the one for the debug
   commands. `check_tree` likewise refuses `--tree` for every command that
   shows no tree: a command that shows one goes in its first arm, and in
   that error, its tests and the `--tree` help.
4. **SYMBOL.** If the command reads code, call `check_code_is_readable`
   first, so that a file whose code elfq can't read says so even when
   SYMBOL names nothing. `symbol::find` turns SYMBOL into a `Symbol`, whose
   `node` may be an import; `item_in_file` or `readable_item` gives its
   item, or an error that says why there is none. The answer's first line
   names it with `Symbol::lead_name`, which says when SYMBOL gave another
   of its names or an address inside it. A command that needs the roots
   refuses shared libraries, as `top`, `retained` and `why` do.
5. **The answer.** Work it out first, then write it. Text uses
   `render::Table`, `TreeRow` for trees, and the shared wording in
   `text.rs`. JSON is a struct that derives `Serialize`, made of the shapes
   in `json.rs`, such as `json::Symbol` for SYMBOL's item, which adds its
   `aliases`, and `json::Item` for every other item, and written with
   `json::write`. Every key is always there, `null` when it has no value.
   `json::write` puts each element of a list that the answer's object
   holds on a line of its own, so give the rows their own list there, as
   a flat list rather than a tree. A tree's rows go in the text's order,
   each with an `id` counting from 0 and the `parent` id of the row it
   hangs under, or `null`, as `RetainedTree::to_json` makes them.
6. **Tests.** Pin the text output with `assert_output` in
   `tests/it/cli.rs`, and test the JSON in `tests/it/json.rs`. Add the
   command to the tests in `cli.rs` that go through commands one by one:
   `arguments_that_dont_go_together_are_refused_before_the_file_is_read`,
   `a_file_whose_code_elfq_cant_read_says_so` if it reads code, and
   `commands_that_need_roots_refuse_a_shared_library` if it needs the roots.
7. **Docs.** List it in README.md and in the Commands table in
   docs/design.md, and its JSON keys in design.md's JSON section. Add
   answers of it to `JSON_ANSWERS` in `tests/it/docs.rs` with an entry in
   each of its lists: the test checks that the section names every key.

## Adding an instruction set

1. **The scanner, in `src/arch/<name>.rs`.** Its `scan` function takes one
   function's bytes and address and returns a `CodeReference` for each
   address the code calls, jumps to or uses; `CodeReferenceKind` in
   `src/arch/mod.rs` lists the kinds. It must not panic on any bytes: a
   truncated instruction ends the scan. Unit tests beside it check it on
   bytes from a sample's disassembly.
2. **Wiring, in `src/arch/mod.rs`.** Add `mod <name>;`, an arm in `scan`
   and an arm in `is_supported`: until `is_supported` says yes, elfq never
   scans the code and says it can't read it. An arm in `is_veneer` lets
   `callers` and `callees` of ranges see through a linker's veneers. An arm
   in `runs_on` lets an entry point that shares its code with the next
   function, as libgcc's `__aeabi_dsub` does with `__adddf3`, run on into
   it as a tail call; without one, such entry points use nothing.
   `code_size`, `leading_padding` and `ending` matter only for finding the
   functions in stripped firmware, which elfq does only for Thumb.
3. **Its name.** Add it to `text::architecture_name`, which the overview
   prints.
4. **Linux programs.** Dynamic relocations (`binary/relocations.rs`) and
   PLT stubs (`binary/plt.rs`) are read only for x86-64. Without them, a
   position-independent program or a shared library has no imports and no
   pointers stored in data.
5. **Samples.** Build the C sample for it in `testdata/build.sh`, with
   `variants` for the copies without DWARF and without symbols, and say
   what it holds in testdata/README.md. Add it to `C_SAMPLES` in
   `tests/it/graph.rs` and `tests/it/retained.rs`, which check its answers
   against testdata/README.md.
6. **Tests that need code elfq can't read.**
   `a_file_whose_code_elfq_cant_read_says_so` in `tests/it/cli.rs` marks a
   file as AArch64; when that is the one you add, pick another machine.
7. **Docs.** README.md and docs/design.md say which instruction sets elfq
   reads.

## Sample binaries

[testdata/README.md](testdata/README.md) lists what each sample contains and
the answers tests should expect. If you change a sample's source, rebuild with
`testdata/build.sh` and commit the new files with the source change. Then
read docs/examples.md again: it says what the samples' source does, down to
line numbers in testdata/rust/src/main.rs, and no test checks that.

## Checking performance

The samples are too small to show how fast elfq is. Before changing how it
loads a file, builds the graph, looks up a SYMBOL or groups rows, build a
release binary of `main` and of your branch, and run both on large ELF
files such as these, from a Linux machine (macOS programs are not ELF
files):

- a big Rust program, such as `cargo` (40 MB), for the overview and
  SYMBOL lookups;
- a big shared library, such as LLVM's (over 100 MB,
  `/usr/lib/*/libLLVM*.so.*` on Debian and Ubuntu), for `debug items`,
  which lists hundreds of thousands of items;
- a big Go program, such as `docker` (45 MB), for `callers` of a range,
  such as `callers 0x400000+1M`;
- a program of 200 MB, such as `pandoc`, for peak memory;
- `target/debug/elfq`, a 45 MB file that is mostly DWARF, for `inlined`;
- stripped Cortex-M firmware, for finding functions without symbols.

Compare the time, the peak memory (`/usr/bin/time -v` on Linux, `-l` on
macOS) and the output, which should not change unless you mean it to.

## Pull requests

Keep them small and focused: one change at a time, each merged before the
next. [docs/design.md](docs/design.md#status-and-roadmap) says what comes
next.
