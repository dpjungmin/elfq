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
where it found it, and `elfq FILE debug walk` lists what the code that a
Cortex-M processor runs from reset does before `main`, as elfq follows it:
calls with their arguments, stores, loops and how it ends. It starts and
ends where `startup` does, so it lists the walk that `startup` reads. They
are hidden from `--help` because they are only for developing elfq.

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
| `src/cli/` | Command-line arguments, with clap: in `mod.rs`, a `Command` variant for each command and the structs that hold their arguments, and in `help.rs`, the text of `--help`. |
| `src/commands/` | One file per command, which looks up SYMBOL, asks `graph` or `debuginfo` and decides what the answer shows. `mod.rs` runs the command the arguments name. The rest are shared: `arguments.rs` checks which arguments go together, `checks.rs` holds the checks a command makes before it answers, such as `check_code_is_readable`, `symbol/` works out what a user means by SYMBOL (`lookup.rs` finds what a name means and names items for the user in return, and `not_found.rs` and `similar.rs` word the error when it finds several or none), `retained_tree.rs` builds the trees of retained sizes that the overview, `top` and `retained` show, `uses.rs` holds the table of `callers` and `callees` for SYMBOL, `range_check/` answers them for ranges of memory or with `--from` or `--to` (its `validate.rs` checks the ranges, `crossing.rs` holds the answers and `rows.rs` their rows, which `reach` and `diff` print too), `flat_answer.rs` holds the answers whose table has one row to a line and `tree_answer.rs` those whose table holds a tree, `text.rs` the wording that commands share and `json.rs` the JSON shapes. `kept.rs` has `Kept`, which leaves what a command keeps until it has answered for the system to take back when elfq ends (`end_after_answer`). |
| `src/commands/browse/` | `elfq FILE browse`, with the `tui` feature: `mod.rs` takes over the terminal and runs the event loop, `session.rs` holds the loaded file and asks the commands' code for answers, `keys.rs` is the table of keys, `app.rs` the history of pages, the go-to box, find's prompt and the screen's layout, and `views/` the pages, the go-to box and the tabs of an item's page other than its tab 1, one file each (`item.rs` is an item's page, and `item/` holds its parts: the header, tab 1's lines, a function's code, a data object's contents and the notes on their lines), `printed.rs`, which draws a command's answer for the pages and tabs that show one, and `names.rs`, which shortens long C++ and Rust names to fit a column. |
| `src/commands/reach/` | `elfq FILE reach [SYMBOL]`: `mod.rs` holds what the search from SYMBOL finds, with the words the code uses for its parts, and `Text`, which words it for people, `find.rs` the search, `rows.rs` writes the answer by block of memory, which browse shows too, `to.rs` the answer with `--to`, `tree.rs` the tree of `--tree` and `json.rs` the JSON, and `starts.rs` answers without SYMBOL, as browse's start page does. |
| `src/commands/diff/` | `elfq FILE diff NEW`: `mod.rs` reads both files, builds the answer from their bytes alone or from their items too, and gives the exit status, `blocks.rs` works out the blocks of memory that both builds share and what changed in each, `pairing.rs` pairs the builds' items, `changes.rs` works out what changed, `edited.rs` which items of the same size are edited, `crossings.rs` the uses between blocks that are new or gone, `rows.rs` picks the rows of MOVED and CHANGE, and `text.rs` and `json.rs` write the answer. The unit tests of `pairing.rs` and `edited.rs` are in `pairing/tests.rs` and `edited/tests.rs`. |
| `src/commands/startup/` | `elfq FILE startup`: `mod.rs` refuses files that aren't Cortex-M firmware from their headers, and runs the rest, `start.rs` finds the reset handler and `main`, `ram_sections.rs` lists the load images and sections without contents that startup code sets up, and matches what a call or a loop copies or zeroes to them, `events.rs` says what the steps read of each of the walk's events, `steps.rs` reads the walk's calls and loops as the steps that copy and zero sections, with `pieces.rs`, which joins the bytes that loops and stores copy or zero into steps, and `unread.rs`, which works out what code that elfq can't follow may set up, `problems.rs` works out the state of each section when `main` starts, the problems and the notes, and `text.rs` and `json.rs` write the answer. `debug.rs` lists the events of the walk that the steps read, for `elfq FILE debug walk`. The unit tests build their walks with `fixtures.rs`; those of `problems.rs` and `steps.rs` are in `problems/tests.rs` and `steps/tests.rs`. |
| `src/arch/` | Finding the addresses that machine code calls or uses, one module per instruction set (x86-64 and Thumb). The rest of elfq reads code through `InstructionSet`, in `mod.rs`, which a file whose code elfq can't read doesn't have. Thumb's has three parts: `thumb/sweep.rs` reads each instruction on its own, `thumb/decode.rs` says what an instruction does with the registers and the stack, and `thumb/values/` follows the addresses that code puts in registers to where a branch through one goes, as for long calls and veneers; `thumb/mod.rs` begins with a map of its files. `listing.rs` prints a function's code as text, for `browse`: x86-64 with iced-x86's formatter, and Thumb with thumb-asm, in the lines that `thumb::layout` cuts the code into with the same sweep as `scan`. `operands` finds the bits of a function's code that say where something is, such as the distance in a call, for `diff` to tell code that changed from code that only moved. `startup.rs` and `thumb/startup/` follow the code that a Cortex-M processor runs from reset, with the numbers in its registers and memory, for `startup`; `thumb/startup/mod.rs` begins with a map of its files, and design.md's [Startup, end to end](docs/design.md#startup-end-to-end) shows how the four `startup` modules hand off to each other. |
| `src/binary/` | Reading an ELF file: its functions and data objects ("items") and where they were found, their names, imports, stored pointers and memory blocks, and in Arm files the core the code is for (`attributes.rs`) and the data that `$d` symbols mark among code. `mod.rs` holds the types and the queries on them, and begins with the steps of loading a file and a map of the files; `load.rs` takes those steps, and `sections.rs` parses the file, refusing one that isn't a linked program or library, and reads the section headers, for `load.rs` and `memory.rs`. `memory.rs` reads only the headers and the bytes that the sections put in memory, which `diff` compares, and `header.rs` only the ELF header and program headers, for `startup` to refuse a file for another processor before loading it. Unnamed data, such as string literals, is found from what code and pointers refer to. Each function's code is scanned once, as the file loads (`Binary::code_references`). `names.rs` makes mangled names readable and works out the short names that people type for long ones; its module doc says how a short name is found quickly. `made_names.rs` makes the names of items that no symbol names, such as `fn_0x8000040`, and tells them apart. `startup.rs` gives the startup walk (`arch::startup::Program`) the file's code, the words that memory holds at reset, as in flash or a load image, and where its functions are, and `listing.rs`, with the `tui` feature, gives browse a function's code as `arch` prints it, with the data that `$d` symbols mark among it. The unit tests of `names.rs` and `vector_table.rs` are in `names/tests.rs` and `vector_table/tests.rs`, and those of `listing.rs`, which check the Thumb text against objdump's, in `listing/thumb_text.rs`. |
| `src/graph/` | The reference graph that connects items, the roots where the program starts, the dominator tree that says what is exclusive to what, the shortest paths that `why` and `reach` show, the bytes that a function reaches (`reached.rs`), the items in ranges of memory (`items_in_ranges.rs`) and the references that cross the ranges' boundary, narrowed with `--from` or `--to` (`boundary.rs`), and where a linker's veneers jump to. |
| `src/debuginfo/` | Reading DWARF, when the file has it: the copies of functions that the compiler inlined, for `inlined`, for the error when SYMBOL names a function that the compiler inlined everywhere, and for `startup`, to find a `main` that the compiler inlined. |
| `src/render/` | Text output: tables, and the trees in their last column (`tree.rs`), which every command shows the same way: as rows that name the row they hang under (in `why`, only where chains branch), or drawn with `--tree`. |
| `tests/it/` | Integration tests, built as one test program (`main.rs` lists its modules). See [Tests](#tests). |
| `testdata/` | The sample programs and the ELF files built from them. See [Sample binaries](#sample-binaries). |
| `scripts/` | `same-output.sh`, which checks that two builds of elfq print the same answers, and times them. See [Checking that output didn't change](#checking-that-output-didnt-change). |
| `docs/` | `design.md`, how elfq works and what comes next; `examples.md`, real questions about the sample binaries, each with the command that answers it and what elfq prints; and `survey.md`, how existing tools compare, written before elfq existed. |

## Rules that keep it approachable

Each module uses only the ones below it in this list:

```
main.rs      the program
commands/    one module per command
cli/         arguments
render/      tables and trees
debuginfo/   DWARF
graph/       references, roots, the dominator tree, paths, crossings, veneers
binary/      items
arch/        machine code
```

So `arch` knows nothing about items, `binary` nothing about the graph, and
`render` nothing about ELF files: a tree's row carries a `graph::Node` only
so that browse can open the item the row is about. No tool checks this; a
`use crate::` line that reaches up the list is a sign that the code belongs
lower down.

`commands/browse/` is a command too, at the top of `commands/`: it asks the
other commands' code for its answers, as `callers::answer` for "used by
N", so that a page says what the commands print, and nothing else uses it.
It is a child of `commands`, so it reaches their `pub(super)` helpers
without making them public. It, `arch/listing.rs` and `binary/listing.rs`
build only with the `tui` feature.

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
- **A long function reads as named steps.** A function of more than
  about 100 lines that isn't one decoding match is the usual sign that it
  does several things. Some still wait to be split, such as
  `Names::similar` in `commands/symbol/similar.rs` and `Function::report`
  in `arch/thumb/values/mod.rs`.

## Words

A word means one thing across elfq, so that a name read in one module
means the same in the next. Check a new file, type or field name against
this list, and add to it when you bring in a word.

- **item**: a function or data object in the file (`binary::Item`). Its
  place in `Binary::items` is an item index, named `index` or `item`.
- **range**: the addresses from a start up to, but not including, an end
  (`binary::AddressRange`), as `--range` reads them. An item keeps an
  address and a size instead, and `Item::range` gives the range they
  make. A section's or segment's `file_range`, named after the `object`
  crate's, is an offset and a size in the file. **bounds**: where a
  function starts and ends, in `arch`, which can't use `AddressRange`
  (`arch::startup::FunctionBounds`).
- **load**: elfq loads a file once, before a command answers: it reads
  the sections, symbols and relocations and finds the items
  (`Binary::load`, in `load.rs`). A section's load image, below, the
  loadable segments that program headers list (`PT_LOAD`),
  `LoadedMemory`, what a file puts in memory when it runs, and a Thumb
  load, an instruction that reads memory, are other things.
- **import**: a function from a shared library, which the file calls but
  doesn't hold. **node**: an item or an import, as the graph connects
  them (`graph::Node`). **root**: a node where the program starts, such as
  the entry point or a handler that the vector table lists (`graph::Root`).
- **use**: a reference from one node to another, such as a call, a jump
  or a stored pointer: what `callers` and `callees` list.
- **handler**: code that an entry of the Cortex-M vector table names,
  which the processor runs for that entry's **exception**. An exception
  is numbered by its entry, counting the stack pointer's as 0.
  `binary::Handler` holds the two: the exception, and where the code
  that handles it starts. The exceptions and panics that the unwinder
  handles, in `eh_frame.rs`, are unrelated.
- **short name**: a shorter name that also means a long one, as
  `process_frame` means `app::process_frame` (`binary::names`). Items
  are filed under their short names' keys, the last parts of their
  paths, so that finding one needs only the short names of a few
  (`names::short_name_key`). A name's **words**, in `names`, are its
  runs of letters, digits, `_` and characters outside ASCII, unrelated
  to the words of memory that `Binary::words` reads.
- **made name**: a name made from where an item is, or from a counter or
  a hash, so that it says nothing of which function or data the item
  is: elfq's names for items that no symbol names, such as
  `fn_0x8000040` and `.rodata@0x2004`, which change from build to
  build, and `entry`, which names only where execution starts
  (`binary::made_names`, `Item::has_made_name`); and in diff's pairing,
  a compiler's, such as a `.L` label (`pairing::is_made_name`).
- **keeps**, **kept by**: retained's words. An item keeps another when
  every chain from a root to the other goes through it, so removing it
  frees the other; the closest such item is what the other is kept by.
  `Kept`, in `kept.rs`, is unrelated: it leaves a value for the system to
  free when elfq ends.
- **answer**: what a command has worked out, before it is written as text
  or JSON. `FlatAnswer` is the text of an answer whose table has one row
  to a line, as `callers`, `callees` and `reach` write it, and
  `TreeAnswer` the text of one whose table holds a tree, as `why` and
  `retained` write it. Browse shows both; `TabAnswer` is a `TreeAnswer`
  as a tab of browse shows it.
- **listing**: an item's lines as tab 1 of its page in browse shows them
  (`Listing`, in `views/item/listing.rs`). For a function, that is its
  code, one line to an instruction, as `InstructionSet::listing` prints
  it, with the data that the file marks among it printed as data
  (`Binary::listing` finds those); for a data object, its contents, one
  row to a value. **line note**: the note at the end of a line of
  browse's listing: what the line does with another item, in the words
  of the CLI's HOW column, as `call` or `uses frames+0x8`, or where a
  jump inside the function goes (`LineNotes`, in
  `views/item/line_notes.rs`). The notes under a command's table, the
  note that SYMBOL adds to an item's name (`Symbol::note`) and startup's
  notes (`problems::Note`) are other things.
- **pane**: in browse, what one tab of an item's page holds (`Pane`, in
  `views/item.rs`): its listing, with the cursor over its lines
  (`ListingPane`), or a command's answer, why the item can't have the
  tab, or for a tab worked out after the page opens, what working it out
  takes (`Later`).
- **work**: in browse, what the screen has yet to work out, which can
  take a while, so the status line says it first: the start page, what
  keeps each item, or tab 4, 5 or 6 of an item's page (`Work`, in
  `app.rs`). `App::pending` gives the next, in their order. Tab 2 is
  worked out at once, the first time it is shown, so it isn't work.
- **header**: in browse, the lines above the tabs of an item's page
  (`Header`, in `views/item/header.rs`). The ELF header, which
  `binary/header.rs` reads, and the header line of a table are other
  things.
- **row name**, **lead name**, **query**: three of the names that browse
  gives an item, with its full name, its short name and its page's
  header name. The module doc of `browse/session.rs` says where each
  goes. An item's query, in browse, is what the command under a tab
  gives as SYMBOL (`Names::query_for`). The queries on a type, as in
  `binary/mod.rs`, and the text that the user types as SYMBOL, which
  `commands/symbol/` calls a query, are other things.
- **build**: in `diff`, one of the two files it compares, the old one or
  the new one (`Build`, which holds the file, its graph and its
  dominator tree). `old` and `new` name what is each build's; what a
  build loads (`LoadedMemory`) is `old_memory` or `new_memory` in diff's
  `Answer` and in the functions of `diff/mod.rs` that make one, and
  `old` or `new` elsewhere.
- **partners**: in `diff`, an item of the old build and the item of the
  new build that it is paired with, by the rule that `PairedBy` names
  (`pairing.rs`). **change**: an item that changed from one build to the
  other (`Change`, in `diff/changes.rs`): partners that moved to another
  block, grew, shrank or are edited (`Change::Partners`), or an item
  that only one build has (`Change::New`, `Change::Gone`); its status
  (`changes::Status`, unrelated to `startup`'s) says which. Its place in
  `Items::changes` is named `at`. **delta**: how many bytes something
  grew by from one build to the other, less than 0 when it shrank: a
  change's item, a block, a section or a row of CHANGE (`Items::delta`,
  `Block::delta`). The CHANGE column and the JSON's `change` keys print
  it.
- **section**: an ELF section. Its place in `Binary::sections` is a
  section index, named `section`. **block**: a run of addresses that
  sections occupy, as the overview's MEMORY table lists
  (`MemoryBlock`). **load image**: where the file stores the bytes of a
  section such as `.data`, for startup code to copy to its address; the
  load image's address is the section's **load address**
  (`Section::load_address`).
  **RAM sections**: in `startup`, the sections that startup code sets
  up, which are all in RAM: those with a load image, and those without
  contents, such as `.bss` (`RamSections`, in `ram_sections.rs`, named
  `ram`). **early run**: code that runs from a load image before startup
  code copies it there (`EarlyRun`, which `EarlyRunFinder` finds, and the
  problem `runs before copy`), and its functions (`EarlyFunction`). A
  **hit** is a function in a load image that the reset handler or a call
  runs, with the chain of functions that gets to it: for one that a
  call's call tree finds, from the function that the call goes to down
  to it; otherwise the function alone (`Hit`).
- **state**: what the registers and the stack hold at one point in the
  code, in `arch/thumb/values/` (`State`). A section's state when `main`
  starts, in `startup`, is its status (`problems::Status`).
- **shape**: how a table shows a tree: as rows that name the row they
  hang under, or drawn with `--tree` (`TreeShape`). A command's
  **format** is text, tree or JSON (`Format`).
- **step**: one thing that startup code does, such as copying a section
  (`startup::steps::Step`). Its place in the steps is a step index;
  where elfq writes a step out, it gives its number, from 1
  (`steps::number`). **region**: what a copy or zeroing copies or
  zeroes: how many bytes, where, and for a copy, where from, with the
  sections that start in them (`ram_sections::Region`). **setup**: the
  first step that sets up a section (`problems::Setup`); `Setups` holds
  each section's, with the unread steps that may set up the others.
  **facts**: in `startup`, what each stage reads once the walk is done,
  from the file to how the walk ended (`Facts`). **ending**: where
  startup ends: at `main`, at the reset handler's endless loop, or where
  the walk ended before either (`Ending`); the walk's own end is an
  `End`.
  **row**: a line of a table that elfq prints; a loop over a table of
  sections also reads one row of that table each time it runs.
- **event**: one thing that the startup walk records, such as a call, a
  store or a loop (`arch::startup::Event`); its place in the walk's list
  is an event index, named `event`, or `loop_event` for a loop's.
  Browse's event loop, which reads the terminal's keys (crossterm's
  `Event`), is unrelated. **walked loop**: a loop that the walk went
  through one iteration at a time, as a loop over a table of sections,
  so that each iteration's calls and stores are events of their own
  (`Loop::walked_iterations`). **call tree**: in `startup`, the
  functions that run when a function runs, each with the function that
  calls it on a shortest chain from the first (`CallTree`). **piece**:
  bytes that a loop or stores copy or zero one after another, which
  join into a step (`startup`'s `pieces.rs`).
  **unread**: code that elfq can't follow, such as a call through a
  register, which may set up any section not set up yet
  (`Does::Unread`, and `unread.rs`). Where the JSON says `unknown` for a
  section's status, `can't follow` for the problem and `can't tell` for
  the note, the code keeps those words (`Status::Unknown`,
  `Problem::CantFollow`, `Note::CantTell`).
- **run**: in the tests, one run of the elfq program, with its exit
  status and what it printed on stdout and stderr (`Run`, in
  `tests/it/support/mod.rs`), which `run` gives; its `printed` is what
  elfq printed on stdout alone. `run_in_process` runs a command inside
  the test instead, and gives no `Run`. An early run, in `startup`, is
  unrelated, and so are the private `Run`s of the startup walk and of
  `binary/memory.rs`.

Some code still uses one of these words for something else, as the Thumb
walk's `Step` (what an instruction leads to) and `reach`'s `Step` (a row
of its tree) do. Those are names to fix, not meanings to copy.

## Tests

`cargo test` runs two kinds of tests:

- **Unit tests** sit beside the code, in a `tests` module at the end of a
  file, or in a `tests.rs` of their own when they are long: a folder's,
  when they would make up most of its `mod.rs`, as in
  `arch/thumb/startup/`, or in a folder named after the file, as
  `arch/thumb/decode/tests.rs` holds `decode.rs`'s. They check one
  function: a decoder on bytes from a sample's disassembly, or an
  algorithm on a small graph built by hand (`graph/dominators.rs`,
  `graph/paths.rs`).
- **Integration tests** in `tests/it/` use the samples in `testdata/bin`.
  `cli.rs`, `json.rs`, `startup.rs` and `docs.rs` run the elfq program,
  the way a user would. Those in `library/` call the library, as
  `Binary::load` and `Graph::build`. `damaged.rs` and `browse.rs` do
  both, and so does one test in `json.rs`; `reach.rs` runs commands in
  its own process, with `run_in_process`, which is quicker for the
  hundreds it runs. The answers they expect are those in
  [testdata/README.md](testdata/README.md), which says, for example, what
  calls `log_msg` in each sample.

| File in `tests/it/` | What it tests |
|---|---|
| `support/` | No tests: in `mod.rs`, the samples' paths (`SAMPLE` and others), the lists that several modules check (`C_SAMPLES`, `FAR_SAMPLES`), `samples_for`, which lists the samples for one architecture, `run`, which runs the program and returns a `Run` to check, its shortcuts `stdout`, `stderr`, `crossing`, `elfq_json` and `text_rows`, `names`, which reads the names in a list of elfq's JSON, `run_in_process`, and `assert_output`; in `elf.rs`, `ElfBytes`, which changes a sample's bytes field by field and saves the file |
| `cli.rs` | Each command's whole text output, errors, exit statuses and help |
| `json.rs` | `--json` output |
| `reach.rs` | `reach` on every item of the Cortex-M samples, against `callees`, `retained` and its own `--tree`, and with `--to` against `reach` without it and `callees --to`, and on every function of each stripped one against its twin with symbols; and without SYMBOL, each start of every sample against `reach START` |
| `startup.rs` | `startup` on the samples whose reset code has a mistake and some without, the exit status of every Cortex-M sample, and the files it refuses; and that `debug walk` lists the walk that `startup` reads |
| `browse.rs` | `browse`'s screens, driven with keys on ratatui's `TestBackend`, a screen in memory; its errors; and on Linux, the program in a terminal that `script` makes. The checks that need its private types are unit tests; see below |
| `library/items.rs` | Finding items: symbols, PLT stubs, unwind tables, functions in stripped firmware, unnamed data |
| `library/graph.rs` | The reference graph: callers, callees, pointers in data, veneers |
| `library/retained.rs` | Roots, and what is exclusive to what |
| `library/why.rs` | The shortest chains |
| `library/symbol.rs` | Looking up SYMBOL |
| `library/inlined.rs` | Inlined copies, from DWARF |
| `damaged.rs` | Damaged and unusual files, made by changing a sample's bytes |
| `docs.rs` | The `$ elfq` examples in README.md, docs/design.md and docs/examples.md, and that design.md names every JSON key |

The checks of `browse` that need its private types are unit tests in
`src/commands/browse/`:

- `views/item.rs`: that the help names every tab, with its key.
- `views/item/code.rs`: every line's link and note, on every sample.
- `views/item/listing.rs`: what the status line says that Enter opens,
  on every line that links, in every function of every sample.
- `views/item/header.rs`: the header of every item and import, against
  what `retained --json` and `why --json` say about it, with the item
  that keeps it named as the rows of `retained` name it.
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

The Thumb text that a function's page shows is tested beside the code
that gives it, `binary/listing.rs`, in `binary/listing/thumb_text.rs`:
against objdump's text for each Cortex-M sample, in `testdata/tests/thumb`,
and in each stripped sample, against the `$d` symbols of its twin.

Two tests in `cli.rs` run the overview and every command it suggests on
each `.elf` and `.so` file in `testdata/bin`, and `top` on each `.elf`
file, so a new sample is tested as soon as it is committed.

To run one test, one file, or the unit tests of one module:

```sh
cargo test --test it cli::top_shows_what_keeps_the_most_bytes
cargo test --test it damaged::
cargo test --lib arch::thumb
```

**Samples.** A test names a sample by its path from the crate's root,
such as `testdata/bin/c-x86_64.elf`, which is what elfq takes as FILE:
`SAMPLE` and the other constants in `support/` hold that path, `samples()`
lists every sample that way, and `load_sample` and `run_in_process` take
it. Only `screen` and `burst`, in `browse.rs`, take the file's name, as the
screen's first line shows it. The lists that several modules check are in
`support/` too: `C_SAMPLES`, the C sample for each instruction set, and
`FAR_SAMPLES`, the builds of `c/far.c` with symbols, whose calls between
RAM, flash and ROM go through veneers. So is `samples_for`, which lists
the samples for one architecture, from the machine in their ELF headers,
beside `samples()` and `sample_path`. Any other constant or list that
only one module uses stays in it.

**Running elfq.** A test runs the program with `run(ARGS)`, from
`support/`, and says what it expects of the `Run` it gets back:
`printed(STATUS)` gives what elfq printed on stdout, `json(STATUS)` gives
that parsed as JSON, and `error()` gives the error, which comes with exit
status 2. Each checks the exit status, and that elfq printed nothing on
the other stream, so that a warning the test doesn't expect fails it.
The shortcuts in `support/` each run elfq and read the `Run` one way:
`stdout(ARGS)` is `run(ARGS).printed(0)`, `stderr(ARGS)` is
`run(ARGS).error()`, `crossing(ARGS)` is `run(ARGS).printed(1)`, for an
answer that finds uses crossing the edge of its ranges, `elfq_json(ARGS)`
is `run(ARGS --json).json(0)`, and `text_rows(ARGS)` is the names in the
rows of `run(ARGS --all).printed(0)`. `names(LIST)` gives the names of the
items in a list of elfq's JSON. Where the status may be 0 or 1, as for a
command run on every sample, `Exit::ZeroOrOne` stands for either. A test
that expects a warning with the answer calls `exited(STATUS)`, which
checks the status alone, and then reads the `Run`'s `stdout` and `stderr`
itself.
`run_in_process(SAMPLE, ARGS)` runs a command inside the test instead: it
gives no `Run`, only what elfq printed on stdout and the outcome that sets
its exit status, and an error panics.

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
exit status may be 0 or 1. The text around an example often repeats its
numbers and names, as docs/examples.md does, so when an example's output
changes, check that the text around it still matches the new output.

**Damaged files.** `damaged.rs` makes its files from a sample's bytes, so
that the hangs and crashes they once caused stay fixed. It and the other
tests that need such a file change the bytes with `ElfBytes`, in
`support/elf.rs`, which sets a field of the ELF header, a section header
or a program header by its name, such as `HeaderField::EntryPoint` or
`SectionField::Size`, to a number or to one of `object::elf`'s constants.
Its `save` writes the file and returns the path to give `run`. Tests run
in a debug build, so arithmetic that overflows fails them too. A test that
uses `parse_quickly` fails if loading the file takes more than 10 seconds:
add one when you make elfq quick to load a file that took far too long.

## Adding a command

Commands take the form `elfq FILE <command> [ARGS]`. `src/commands/top.rs`
is a short one to copy.

1. **Arguments, in `src/cli/`.** Add a variant to `Command` in `mod.rs`,
   before the hidden `Debug`. Its name, in lowercase with `-` between
   words (`SizeOf` is typed `size-of`), is the command's name, and its
   `///` line is what `elfq --help` lists. Give it an `after_help` with a
   longer explanation, a constant in `help.rs` in `Command`'s order, as the
   others have, and a line in `EXAMPLES` there. Its arguments go in a
   struct of their own, as `RetainedArgs` holds SYMBOL and `--all`.
   `--json`, `--tree` and `--range` are global, so don't declare them
   again. `elfq help COMMAND` and the hint for a command typed before the
   file find the new command by themselves.
2. **The command, in `src/commands/<name>.rs`.** Start with a `//!` line
   that shows how to type it, then a `run` function that takes what it
   needs: the `Binary` (and the file's path, if the answer names the file,
   as `overview` does), the arguments it reads, the `Format` and the
   output. It returns
   `Result<Outcome>`: `Outcome::Answered`, or for exit status 1 another
   variant, as `callers` and `callees` return for ranges.
3. **Wiring, in `src/commands/mod.rs` and `arguments.rs`.** Add
   `mod <name>;` and an arm in `run`. In `arguments.rs`, `check_tree`,
   `check_range` and `check_json` each have an arm for every command, and
   the compiler asks for the new one in each: put it with the commands that
   take `--tree`, `--range` or `--json`, or with those that refuse it. A
   command that takes `--range` gets `range` from its arm in `run`; name
   it in `RANGE_REFUSED`, the error that lists the commands that take
   `--range`, in the tests in `tests/it/cli.rs` that check that error, and
   in the `--range` help in `cli/mod.rs`, README.md and docs/design.md. A
   command that shows a tree goes likewise in `check_tree`'s error, its
   tests and the `--tree` help.
4. **SYMBOL.** If the command reads code, call `check_code_is_readable`
   first, so that a file whose code elfq can't read says so even when
   SYMBOL names nothing. `symbol::find` turns SYMBOL into a `Symbol`, whose
   `node` may be an import; an answer that names other items too keeps
   one `Names` and calls `Names::find`, as `retained` and `why` do, so
   that the names are worked out once. `item_in_file` or `readable_item`
   gives its item, or an error that says why there is none. These checks
   are in `checks.rs`. The answer's first line names it with `Symbol::lead_name`,
   which says when SYMBOL gave another of its names or an address inside
   it. A command that needs the roots refuses shared libraries, as `top`,
   `retained` and `why` do.
5. **The answer.** Work it out first, then write it. Text is a
   `FlatAnswer` for a table of one row to a line, or a `TreeAnswer` for a
   table that holds a tree of `TreeRow`s, as `top` builds one, with the
   shared wording in `text.rs`. A tree cut down to its largest rows uses
   the functions at the end of `tree_answer.rs`, as `top` does through
   `RetainedTree::largest`. JSON is a struct that derives `Serialize`, made of the shapes
   in `json.rs`, such as `json::Symbol` for SYMBOL's item, which adds its
   `aliases`, and `json::Item` for every other item, and written with
   `json::write`. Every key is always there, `null` when it has no value.
   `json::write` puts each element of a list that the answer's object
   holds on a line of its own, so give the rows their own list there, as
   a flat list rather than a tree. A tree's rows go in the text's order,
   each with an `id` counting from 0 and the `parent` id of the row it
   hangs under, or `null`, as `RetainedTree::to_json` makes them.
   Keep what the command builds that is large, such as the graph,
   `Names` or a `RetainedTree`, in a `Kept`, from `kept.rs` (`let graph =
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

`browse`'s pages are views, in `src/commands/browse/views/`. Copy
`help.rs` for a page of its own, and `callees.rs` for a tab of an
item's page. An item's page, `item.rs` with its parts in `item/`, is
the biggest view, and not the one to start from.

1. **The page, in `views/<name>.rs`.** A struct with a `draw` function
   that draws it in the area it is given, and a `handle` function that
   does what an `Action` asks. A page that lists rows keeps a `Cursor`,
   which scrolls them and draws only those on the screen. A `handle` whose
   actions can lead off the page returns an `Option<Go>`: following a link
   returns `Go::Open` with the page to open, and `Go::Say` puts a line on
   the status line. One whose actions only move within the page returns
   nothing, as `Help::handle` does, and its arm in `View::handle` gives
   `None`. Pages ask `Session` and the commands' code for their answers,
   rather than working them out again. A column of names shortens the
   names on the screen with a `FittedRows`, which keeps rows that would
   read the same apart, and fits them again only when other rows show or
   the column's width changes, since long C++ names take a while;
   `preview` names the selected one in full.
2. **Wiring, in `views/mod.rs`.** Add `mod <name>;`, a variant to `View`,
   and its arm in each of `View`'s functions; the compiler asks for them.
   `title` names the page in the title line's trail, `preview` says what
   the selected row's link opens, `back` goes back within the page, as
   from a jump inside a function, when it can, `command` gives the
   command that prints what the page shows, which goes under it, and
   `cursor`, `rows_with` and `row_noun` give find the page's rows and
   what to call them.

   A page or tab that shows a command's answer, as `callers.rs` does, is
   a `Printed` (`printed.rs`), made from the command's own answer: the
   lines before its table, the rows, each with the item it opens, and
   the notes; it draws them and answers the keys. The start page
   (`StartPage`, in `start.rs`) holds one once it is worked out.
   A page that takes a while to work out says so first: it waits as a
   variant of `Work`, in `app.rs`, which `App::pending` puts in order with
   the rest and `Work::message` words for the status line, and
   `App::work` works it out after a draw that says so, through the
   page's own `work_out`, which `View::work_out` calls.
   A tab of an item's page, such as `callees.rs`, is drawn and handles
   actions in the same way, but `ItemPage`, in `item.rs`, holds it in a
   `Pane` of its own: add it to `Tab` and `Tab::ALL`, with its number and
   the name of the command that prints the same answer in
   `Tab::number_and_name`, and its pane to those that `ItemPage::new`
   lists. Where some items can't have the tab, the pane says why in the
   command's own words, from its check, such as `why::refusal`. A tab
   worked out the first time it is shown, as tab 2 is, waits as a variant
   of `Later`, and one that can take seconds, as tabs 4 to 6 can, as a
   `SlowTab`, until the screen says that it is working it out, in the
   words of `Work::message`; `Later::work_out` works out both. The
   compiler asks for their arms, but not for `Tab::ALL`. Its key goes in
   the "1 2 3 4 5 6" row of `BINDINGS`, with its help text (step 3), which
   a test in `item.rs` checks against `Tab::ALL`. Its rows come from that
   command's own code, as `callees::answer`, so that they can't drift from
   what it prints.
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
2. **Wiring, in `src/arch/mod.rs`.** Add `mod <name>;`, a variant of
   `InstructionSet`, and an arm for it in `InstructionSet::of`: until `of`
   names it, elfq never reads the code and says it can't read it. The
   compiler then asks for an arm in each of `InstructionSet`'s methods.
   `scan` and `operands` call the new scanner; without a real `operands`,
   `diff` calls a function edited whenever the distance in one of its
   calls changes, as when it or what it calls moves. `is_veneer` lets
   `callers` and `callees` of ranges, and `reach`, see through a linker's
   veneers. `runs_on` lets an entry point that shares its code with the
   next function, as libgcc's `__aeabi_dsub` does with `__adddf3`, run on
   into it as a tail call; when it answers `None`, such entry points use
   nothing. `code_size`, `leading_padding`, `ending` and
   `scan_instructions` matter only for finding the functions in stripped
   firmware, which elfq does only for Thumb, and `startup_code` only for
   `startup`, which walks only Thumb code: the x86-64 arms show what to
   answer there.
3. **Its name.** Add it to `text::architecture_name`, which the overview
   prints.
4. **Linux programs.** Dynamic relocations (`binary/relocations.rs`) and
   PLT stubs (`binary/plt.rs`) are read only for x86-64. Without them, a
   position-independent program or a shared library has no imports and no
   pointers stored in data.
5. **Samples.** Build the C sample for it in `testdata/build.sh`, with
   `variants` for the copies without DWARF and without symbols, and say
   what it holds in testdata/README.md. Add it to `C_SAMPLES` in
   `tests/it/support/mod.rs`; `library/graph.rs` and
   `library/retained.rs` check its answers against testdata/README.md.
6. **Tests that need code elfq can't read.**
   `ElfBytes::mark_code_unreadable`, in `tests/it/support/elf.rs`, marks a
   file as AArch64 for them. When AArch64 is the one you add, pick another
   machine there, and change the tests' text that names AArch64.
7. **Text for `browse`.** The arm in `InstructionSet::listing`, in
   `src/arch/listing.rs`, prints the code, one line per instruction,
   starting where `scan`'s references are.
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

## Checking that output didn't change

A change that shouldn't change what elfq prints, such as a cleanup or a
speed-up, needs more than the tests: run `scripts/same-output.sh` with a
release build of `main` and one of your branch, on as many ELF files as you
can find.

```sh
git worktree add ../elfq-main main
(cd ../elfq-main && cargo build --release)
cargo build --release
scripts/same-output.sh ../elfq-main/target/release/elfq target/release/elfq \
    testdata/bin /usr/bin --pairs pairs.txt
```

It runs every command, with and without `--json`, `--all` and `--tree`, on
each file, the SYMBOL commands on the first three items that `top` lists,
and the range commands on the first two blocks of memory that the overview
lists, with `reach SYMBOL --to RANGE` on the first item and each block. It
also runs every `--help`, and `diff` on each pair of files listed in
`pairs.txt`, one pair a line. It prints each command whose stdout, stderr
or exit status differs, with the first lines that differ, and exits with
status 1 if one does. On the samples alone it runs about 6,200
commands, in about 15 seconds on four processors.

The samples are small and similar, so add other files too: programs from
`/usr/bin`, a Rust build of elfq itself, and firmware, which `testdata/build.sh`
and the SDKs of chip makers can build. A pull request that says its output
didn't change should say what files it was checked on.

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

`scripts/same-output.sh --time OLD NEW FILE...` runs the overview, `top`,
`inlined`, `reach`, `startup` and the SYMBOL commands on each file 9 times
with each build, taking turns so that a busy moment slows both alike, and
prints the median times side by side, in milliseconds. Compare the peak memory too (`/usr/bin/time -v` on Linux,
`-l` on macOS), and the output, which should not change unless you mean
it to.

## Pull requests

Keep them small and focused: one change at a time, each merged before the
next. [docs/design.md](docs/design.md#status-and-roadmap) says what comes
next.
