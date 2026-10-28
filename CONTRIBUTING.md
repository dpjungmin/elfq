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

Debug builds are optimized a little (`opt-level = 1` in Cargo.toml), so that
`cargo run` on a large file and `cargo test` take seconds. A clean build
takes about twice as long as it would unoptimized, but a build after a
change takes about as long.

`elfq FILE debug items` lists every function and data object elfq found and
where it found it. It is hidden from `--help` because it is only for
developing elfq.

The tests read the prebuilt sample binaries in `testdata/bin`, so you need no
cross compilers. Before opening a pull request, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --no-default-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
cargo test --locked
cargo +1.88 check --all-targets --locked
cargo +1.88 check --all-targets --locked --no-default-features
```

The third line and the last check elfq without the `tui` feature, which
`browse` needs. The last two lines check that elfq still compiles with Rust
1.88, the oldest version it supports (`rust-version` in `Cargo.toml`);
`rustup toolchain install 1.88` gets it. On Linux, CI also runs `browse` in
a terminal that `script` makes, and checks that q quits it and that the
terminal is as it was; `.github/workflows/ci.yml` has the command.

## Where things live

| Path | What it holds |
|---|---|
| `src/main.rs` | The program: parses arguments, runs one command on a thread with a large stack, prints errors and sets the exit status. It also turns `elfq help COMMAND` into `--help`, and suggests the right order when the command comes before the file. |
| `src/lib.rs` | Declares the modules below. elfq is a library plus that small program; the library exists for the program and its tests. |
| `src/cli.rs` | Command-line arguments, with clap: a `Command` variant for each command, the structs that hold their arguments, and the text of `--help`. |
| `src/commands/` | One file per command, which looks up SYMBOL, asks `graph` or `debuginfo` and decides what the answer shows. `mod.rs` runs the command the arguments name, after checking arguments that don't go together. The rest are shared: `symbol.rs` works out what a user means by SYMBOL, `retained_tree.rs` builds the trees of retained sizes that the overview, `top` and `retained` show, `ranges.rs` answers `callers` and `callees` for ranges of memory or with `--from` or `--to`, `text.rs` holds the wording that commands share and `json.rs` the JSON shapes. `mod.rs` also has `Kept`, which leaves what a command keeps until it has answered for the system to take back when elfq ends (`end_after_answer`). |
| `src/commands/browse/` | `elfq FILE browse`, with the `tui` feature: `mod.rs` takes over the terminal and runs the event loop, `session.rs` holds the loaded file and asks the commands' code for answers, `keys.rs` is the table of keys, `app.rs` the history of pages, the go-to box, find's prompt and the screen's layout, and `views/` the pages, the go-to box and the tabs of an item's page other than a function's code, one file each (a data object's contents, its tab 1, are in `item/contents.rs`), `printed.rs`, which draws a command's answer for the pages and tabs that show one, and `names.rs`, which shortens long C++ and Rust names to fit a column. |
| `src/commands/diff/` | `elfq FILE diff NEW`: `mod.rs` reads both files and gives the exit status, `blocks.rs` works out the blocks of memory that both builds share and what changed in each, `pairing.rs` pairs the builds' items, `rows.rs` works out what changed and the rows of MOVED and CHANGE, `edited.rs` which items of the same size are edited, `crossings.rs` the uses between blocks that are new or gone, and `text.rs` and `json.rs` write the answer. |
| `src/arch/` | Finding the addresses that machine code calls or uses, one module per instruction set (x86-64 and Thumb). Thumb's has three parts: `thumb/mod.rs` reads each instruction on its own, `thumb/decode.rs` says what an instruction does with the registers and the stack, and `thumb/values/` follows the addresses that code puts in registers to where a branch through one goes, as for long calls and veneers. `listing.rs` prints a function's code as text, for `browse`: x86-64 with iced-x86's formatter, and Thumb with thumb-asm, in the lines that `thumb::layout` cuts the code into with the same sweep as `scan`. `operands` finds the bits of a function's code that say where something is, such as the distance in a call, for `diff` to tell code that changed from code that only moved. |
| `src/binary/` | Reading an ELF file: its functions and data objects ("items") and where they were found, their names, imports, stored pointers and memory blocks, and in Arm files the core the code is for (`attributes.rs`) and the data that `$d` symbols mark among code. `memory.rs` reads only the headers and the bytes that the sections put in memory, which `diff` compares. Unnamed data, such as string literals, is found from what code and pointers refer to. Each function's code is scanned once, as the file loads (`Binary::code_references`). |
| `src/graph/` | The reference graph that connects items, the roots where the program starts, the dominator tree that says what is exclusive to what, the shortest paths that `why` and `reach` show, the references that cross the boundary of ranges of memory, and where a linker's veneers jump to. |
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
graph/       references, roots, the dominator tree, paths, crossings, veneers
binary/      items
arch/        machine code
```

So `arch` knows nothing about items, `binary` nothing about the graph, and
`render` nothing about ELF files. No tool checks this; a `use crate::` line
that reaches up the list is a sign that the code belongs lower down.

`commands/browse/` is a command too, at the top of `commands/`: it asks the
other commands' code for its answers, as `callers::answer` for "used by
N", so that a page says what the commands print, and nothing else uses it.
It is a child of `commands`, so it reaches their `pub(super)` helpers
without making them public. It and `arch/listing.rs` build only with the
`tui` feature.

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
  `Graph::build`. `damaged.rs` and `browse.rs` do both, and so does one
  test in `json.rs`; `reach.rs` runs commands in its own process, through
  `commands::run`, which is quicker for the hundreds it runs. The answers they expect are
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
| `reach.rs` | `reach` on every item of the Cortex-M samples, against `callees`, `retained` and its own `--tree`, and with `--to` against `reach` without it and `callees --to`, and on every function of each stripped one against its twin with symbols; and without SYMBOL, each start of every sample against `reach START` |
| `symbol.rs` | Looking up SYMBOL |
| `inlined.rs` | Inlined copies, from DWARF |
| `damaged.rs` | Damaged and unusual files, made by changing a sample's bytes |
| `browse.rs` | `browse`'s screens, driven with keys on ratatui's `TestBackend`, a screen in memory; its errors; and on Linux, the program in a terminal that `script` makes. The checks that need its private types are unit tests; see below |
| `docs.rs` | The `$ elfq` examples in README.md, docs/design.md and docs/examples.md, and that design.md names every JSON key |

The checks of `browse` that need its private types are unit tests in
`src/commands/browse/`:

- `views/item.rs`: every line's link and note, on every sample, and the
  header of every item and import, against what `retained --json` and
  `why --json` say about it, with the item that keeps it named as the
  rows of `retained` name it.
- `views/item/contents.rs`: tab 1 of every data object of every sample:
  its rows that link are the sites and items that `callees --json` lists,
  each string is the text at its address, and the objdump command under
  them names the object's section, start, end and file.
- `views/callers.rs`: the callers tab of every page of every sample,
  against what the command under it prints, and that no two callers on
  the screen read the same once their names are shortened.
- `views/callees.rs` and `views/reach.rs`: the callees and reach tabs of
  every page of every sample, against what the command under each prints.
- `views/why.rs` and `views/retained.rs`: the why and retained tabs of
  every page of every sample, against what the command under each prints
  with `--all` and without; the retained tab's bytes by block, against
  `retained --range BLOCK`; and that the retained tab keeps the first
  line of its answer on a small screen.
- `views/start.rs`: the start page of every sample, against what the
  overview and the command under it print, and of a file with more starts
  than that command lists.
- `views/printed.rs`: how a command's answer gives way on a small screen,
  which form of its notes it shows, and how the name after "kept by" is
  shortened.
- `views/picker.rs`: that no two rows of the go-to box read the same.
- `views/names.rs`: each step of shortening a name.
- `app.rs`: that tabs 2 to 6 keep their table's header and selected row
  on a small screen, that tabs 4, 5 and 6 are worked out once the screen
  says so, that what keeps each item is worked out once, after the first
  page is drawn, and that the start page under the go-to box is worked
  out only once the box closes.
- `views/thumb_text.rs`: the Thumb text, against objdump's text for each
  Cortex-M sample, in `testdata/tests/thumb`, and in each stripped
  sample, against the `$d` symbols of its twin.

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
   Then name it in `RANGE_REFUSED`, the error that lists the commands that
   take `--range`, in the tests in `tests/it/cli.rs` that check that error,
   and in the `--range` help in `cli.rs`, README.md and docs/design.md. A
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
   Keep what the command builds that is large, such as the graph,
   `Names` or a `RetainedTree`, in a `commands::Kept` (`let graph =
   Kept::new(Graph::build(binary));`), so that elfq ends without freeing
   it a piece at a time. A `Kept` is dropped when it goes out of scope,
   so the compiler checks that what its value borrows outlives it:
   declare it after what it borrows, and give a struct that borrows
   kept values a lifetime for them apart from the file's, as `why`'s
   `Chains` has. A value kept only until an error return and then moved
   on, as `retained::run` keeps its `Names`, is taken back out with
   `into_inner`.
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

## Adding a view

`browse`'s pages are views, in `src/commands/browse/views/`. An item's
page, `item.rs`, is the one to copy; `help.rs` is a short one.

1. **The page, in `views/<name>.rs`.** A struct with a `draw` function
   that draws it in the area it is given, and a `handle` function that
   does what an `Action` asks. A page that lists rows keeps a `Cursor`,
   which scrolls them and draws only those on the screen. Following a
   link returns `Go::Open` with the page to open, and `Go::Say` puts a
   line on the status line. Pages ask `Session` and the commands' code for
   their answers, rather than working them out again. A column of names
   shortens the names on the screen with a `FittedRows`, which keeps rows
   that would read the same apart, and fits them again only when other
   rows show or the column's width changes, since long C++ names take a
   while; `preview` names the selected one in full.
2. **Wiring, in `views/mod.rs`.** Add `mod <name>;`, a variant to `View`,
   and its arm in each of `View`'s functions; the compiler asks for them.
   `title` names the page in the title line's trail, `preview` says what
   the selected row's link opens, `back` goes back within the page, as
   from a jump inside a function, when it can, `command` gives the
   command that prints what the page shows, which goes under it, and
   `cursor`, `rows_with` and `row_noun` give find the page's rows and
   what to call them.

   A page or tab that shows a command's answer, as the start page and
   `callers.rs` do, is a `Printed` (`printed.rs`), made from the
   command's own answer: the lines before its table, the rows, each with
   the item it opens, and the notes; it draws them and answers the keys.
   A page that takes a while to work out says so first: `App::work` works
   it out after a draw that says what it is doing.
   A tab of an item's page, such as `callers.rs`, is drawn and handles
   actions in the same way, but `item.rs` holds it: add it to `Tab` and
   `Tab::ALL`, with its number and the name of the command that prints
   the same answer in `Tab::number_and_name`, and its arm in each of
   `ItemPage`'s functions that match on `Tab`, `why_not` for the items
   that can't have it, `show` for one worked out the first time it is
   shown, as tab 2 is, or `waiting` and `work` for one that can take
   seconds, as tabs 4 to 6 are, and `printed`; the compiler asks for
   those, but not for `Tab::ALL`. Its key goes in the "1 2 3 4 5 6" row
   of `BINDINGS`, with its help text (step 3). Its rows come from that
   command's own code, as `callers::answer`, so that they can't drift
   from what it prints.
3. **Keys.** A key goes in `BINDINGS` in `keys.rs`, with the new `Action`,
   what the help page says about it, and its hint, if the hint line names
   it. The help page and the hint line are printed from that table, and
   `no_key_does_two_things` checks it. `app.rs` handles an action that
   opens a page, as `Action::Help` does.
4. **Tests.** Pin its screens in `tests/it/browse.rs` at 80x24 and 100x30,
   and drive it with keys there, as the other tests do.

To see a page in a real terminal, run elfq in tmux and print the screen.
Keys sent before elfq draws its first screen can be lost, so the script
waits for the hint line first, and then for the page that the keys open:

```sh
cargo build
tmux new-session -d -s elfq -x 100 -y 30 'target/debug/elfq testdata/bin/c-x86_64.elf browse log_msg'
until tmux capture-pane -p -t elfq | grep -q 'q quit'; do sleep 0.1; done
tmux send-keys -t elfq Tab Enter
until tmux capture-pane -p -t elfq | grep -q '› strlen@plt'; do sleep 0.1; done
tmux capture-pane -p -t elfq
tmux send-keys -t elfq q
```

Tab selects log_msg's call to strlen@plt, and Enter opens strlen@plt's
page, which the capture prints.

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
   `callers` and `callees` of ranges, and `reach`, see through a linker's
   veneers. An arm in `runs_on` lets an entry point that shares its code
   with the next function, as libgcc's `__aeabi_dsub` does with
   `__adddf3`, run on into it as a tail call; without one, such entry
   points use nothing.
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
7. **Text for `browse`.** An arm in `listing` in `src/arch/listing.rs`
   prints the code, one line per instruction, starting where `scan`'s
   references are. Until then, a function's page lists only the lines that
   use another item.
8. **Docs.** README.md and docs/design.md say which instruction sets elfq
   reads.

## Sample binaries

[testdata/README.md](testdata/README.md) lists what each sample contains and
the answers tests should expect. If you change a sample's source, rebuild with
`testdata/build.sh` and commit the new files with the source change. For a
Cortex-M sample, also run `testdata/thumb-text.sh`, which writes objdump's
text for its code to `testdata/tests/thumb`, where the tests of `browse`'s
Thumb text read it, and commit that too. Then
read docs/examples.md again: it says what the samples' source does, down to
line numbers in testdata/rust/src/main.rs, and no test checks that.

## Checking performance

The samples are too small to show how fast elfq is. Before changing how it
loads a file, builds the graph, looks up a SYMBOL or groups rows, build a
release binary of `main` and of your branch, and run both on large ELF
files such as these, from a Linux machine (macOS programs are not ELF
files):

- a big Rust program, such as `cargo` (43 MB), for the overview and
  SYMBOL lookups;
- a big shared library, such as LLVM's (over 100 MB,
  `/usr/lib/*/libLLVM*.so.*` on Debian and Ubuntu), for `debug items`,
  which lists hundreds of thousands of items;
- a big Go program, such as `docker` (45 MB), for `callers` of a range,
  such as `callers 0x400000+1M`;
- a program of 200 MB, such as `pandoc`, for peak memory;
- `target/debug/elfq`, a 90 MB file that is mostly DWARF, for `inlined`;
- stripped Cortex-M firmware, for finding functions without symbols.

Compare the time, the peak memory (`/usr/bin/time -v` on Linux, `-l` on
macOS) and the output, which should not change unless you mean it to.

## Pull requests

Keep them small and focused: one change at a time, each merged before the
next. [docs/design.md](docs/design.md#status-and-roadmap) says what comes
next.
