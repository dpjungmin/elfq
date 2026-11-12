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

Debug builds are optimized a little (`opt-level = 1` in Cargo.toml), so
that `cargo run` on a large file and `cargo test` take seconds. A clean
build takes about twice as long as it would unoptimized, but a build after
a change takes about as long.

`elfq FILE debug items` lists every function and data object elfq found
and where it found it, and `elfq FILE debug walk` lists what the code that
a Cortex-M processor runs from reset does before `main`, as elfq follows
it: calls with their arguments, stores, loops and how it ends. It starts
and ends where `startup` does, so it lists the walk that `startup` reads.
`elfq FILE debug matches PATTERN...` prints a line of JSON for each item
that a pattern such as `'uart_*'` matches, with how it matched. They are
hidden from `--help` because they are only for developing elfq, and
`debug matches` from `debug --help` too, so that adding it changed no
help text.

The tests read the prebuilt sample binaries in `testdata/bin`, so you need
no cross compilers. Before opening a pull request, run what CI runs:

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
`browse` needs. The last two lines check that elfq still compiles with
Rust 1.88, the oldest version it supports (`rust-version` in
`Cargo.toml`); `rustup toolchain install 1.88` gets it. On Linux, CI also
runs `browse` in a terminal that `script` makes, and checks that q quits
it and that the terminal is as it was; `.github/workflows/ci.yml` has the
command.

## Where things live

| Path | What it holds |
|---|---|
| `src/main.rs` | The program: parses arguments, runs one command on a thread with a large stack, prints errors and sets the exit status. It also turns `elfq help COMMAND` into `--help`, suggests the right order when the command comes before the file, and puts lines under the first line of clap's error for an argument that `why`, `reach`, `inlined`, `browse` or `top` doesn't take (`with_hints`), which `arguments::unexpected_argument_hints` words: for `top`, the command that takes a pattern given to it, and when the arguments are files here, which the shell may have put in place of a pattern, the line that says so. `commands/symbol/shell_hint.rs` words that line, which the commands' own errors for SYMBOL end with too. |
| `src/lib.rs` | Declares the modules below. elfq is a library plus that small program; the library exists for the program and its tests. |
| `src/cli/` | Command-line arguments, with clap: in `mod.rs`, a `Command` variant for each command and the structs that hold their arguments, and in `help.rs`, the text of `--help`. |
| `src/commands/` | One file per command, or a folder for the larger ones below, which looks up SYMBOL, asks `graph` or `debuginfo` and decides what the answer shows. `mod.rs` runs the command the arguments name. The rest are shared: `arguments.rs` checks which arguments go together, `checks.rs` holds the checks a command makes before it answers, such as `check_code_is_readable` and `check_holds_items`, which `range_check/` and `retained` make of a range, `symbol/` works out what a user means by SYMBOL (`mod.rs` tells ranges from SYMBOL and reads what `callers` and `callees` ask about into a `Subject`, `lookup.rs` finds what a name means, or in the commands that take one item, the one item that a pattern matches (`Names::find`), and names items for the user in return, `not_found.rs` and `similar.rs` word the error when it finds several or none, or a pattern matches none, in words that `retained` adds its hint to, and the error when a pattern matches several where one item is needed, and for a name that finds nothing offer first the pattern for what is in a module of that name, where the command can take what it matches (`Takes`), `pattern.rs` finds the items that a pattern such as `'uart_*'` matches, with its unit tests in `pattern/tests.rs`, `matched.rs` words what patterns matched, for an answer's first line and its notes, and gives the one item that a pattern matched (`PatternMatches::only_item`), and `set.rs` puts what several SYMBOLs, patterns and ranges pick together in a `Set`, each item once, and words how an answer's first line names it, part by part; unit tests build a file of functions with `fixtures.rs`), `retained_tree.rs` builds the trees of retained sizes that the overview, `top` and `retained` show, `uses.rs` holds the table of `callers` and `callees` for one SYMBOL, `range_check/` answers them for ranges of memory, for a set, or with `--from` or `--to` (its `mod.rs` says what a check asks about, an `Inside`, which holds the ranges, one SYMBOL or a set, and works out the items inside once, for the rest, `validate.rs` checks the ranges, `crossing.rs` holds the answers and `rows.rs` their rows, which `reach` and `diff` print too), `flat_answer.rs` holds the answers whose table has one row to a line and `tree_answer.rs` those whose table holds a tree, `text.rs` the wording that commands share and `json.rs` the JSON shapes. `kept.rs` has `Kept`, which leaves what a command keeps until it has answered for the system to take back when elfq ends (`end_after_answer`). |
| `src/commands/browse/` | `elfq FILE browse`, with the `tui` feature: `mod.rs` takes over the terminal and runs the event loop, `session.rs` holds the loaded file and asks the commands' code for answers, `keys.rs` is the table of keys, `app.rs` the history of pages, the go-to box, find's prompt and the screen's layout, and `views/` the pages, the go-to box and the tabs of an item's page other than its tab 1, one file each (`item.rs` is an item's page, and `item/` holds its parts: the header, tab 1's lines, a function's code, a data object's contents and the notes on their lines), `printed.rs`, which draws a command's answer for the pages and tabs that show one, and `names.rs`, which shortens long C++ and Rust names to fit a column. |
| `src/commands/reach/` | `elfq FILE reach [SYMBOL]`: `mod.rs` holds what the search from SYMBOL finds, with the words the code uses for its parts, and `Text`, which words it for people, `find.rs` the search, `rows.rs` writes the answer by block of memory, which browse shows too, `to.rs` the answer with `--to`, `tree.rs` the tree of `--tree` and `json.rs` the JSON, and `starts.rs` answers without SYMBOL, as browse's start page does. |
| `src/commands/diff/` | `elfq FILE diff NEW`: `mod.rs` reads both files, builds the answer from their bytes alone or from their items too, and gives the exit status, `blocks.rs` works out the blocks of memory that both builds share and what changed in each, `pairing.rs` pairs the builds' items, `changes.rs` works out what changed, `edited.rs` which items of the same size are edited, `crossings.rs` the uses between blocks that are new or gone, `rows.rs` picks the rows of MOVED and CHANGE, and `text.rs` and `json.rs` write the answer. The unit tests of `pairing.rs` and `edited.rs` are in `pairing/tests.rs` and `edited/tests.rs`. |
| `src/commands/startup/` | `elfq FILE startup`: `mod.rs` refuses files that aren't Cortex-M firmware from their headers, and runs the rest, `start.rs` finds the reset handler and `main`, `ram_sections.rs` lists the load images and sections without contents that startup code sets up, and matches what a call or a loop copies or zeroes to them, `events.rs` says what the steps read of each of the walk's events, `steps.rs` reads the walk's calls and loops as the steps that copy and zero sections, with `pieces.rs`, which joins the bytes that loops and stores copy or zero into steps, and `unread.rs`, which works out what code that elfq can't follow may set up, `problems.rs` works out the state of each section when `main` starts, the problems and the notes, and `text.rs` and `json.rs` write the answer. `debug.rs` lists the events of the walk that the steps read, for `elfq FILE debug walk`. The unit tests build their walks with `fixtures.rs`; those of `problems.rs` and `steps.rs` are in `problems/tests.rs` and `steps/tests.rs`. |
| `src/commands/retained/` | `elfq FILE retained SYMBOL...`: `symbols.rs` finds the SYMBOLs, and the items of the ranges and patterns given in their place, puts them together in a `Set`, which `symbol/set.rs` holds with the words of the answer's first line, leaves out the functions whose end elfq doesn't know and words the error that lists every argument that retained can't use, `mod.rs` puts the notes on how the patterns matched before the others, and answers about one item as text, as browse's tab 6 shows it too, `several.rs` answers about several at once as text, with the rows that several keep together and the notes about the SYMBOLs and the bytes they save only together, `json.rs` writes the JSON, in one shape for one SYMBOL or several, and `parts.rs` works out what the answers take from what removing the SYMBOLs loses: the items under the removal's tops, what they share with other code and the calls through pointers among them, with the notes that name those, the bytes that go only when several go, and the SYMBOLs that each row kept together needs. |
| `src/arch/` | Finding the addresses that machine code calls or uses, one module per instruction set (x86-64 and Thumb). The rest of elfq reads code through `InstructionSet`, in `mod.rs`, which a file whose code elfq can't read doesn't have. Thumb's has three parts: `thumb/sweep.rs` reads each instruction on its own, `thumb/decode.rs` says what an instruction does with the registers and the stack, and `thumb/values/` follows the addresses that code puts in registers to where a branch through one goes, as for long calls and veneers; `thumb/mod.rs` begins with a map of its files. `listing.rs` prints a function's code as text, for `browse`: x86-64 with iced-x86's formatter, and Thumb with thumb-asm, in the lines that `thumb::layout` cuts the code into with the same sweep as `scan`. `operands` finds the bits of a function's code that say where something is, such as the distance in a call, for `diff` to tell code that changed from code that only moved. `startup.rs` and `thumb/startup/` follow the code that a Cortex-M processor runs from reset, with the numbers in its registers and memory, for `startup`; `thumb/startup/mod.rs` begins with a map of its files, and design.md's [Startup, end to end](docs/design.md#startup-end-to-end) shows how the four `startup` modules hand off to each other. |
| `src/binary/` | Reading an ELF file: its functions and data objects ("items") and where they were found, their names, imports, stored pointers and memory blocks, and in Arm files the core the code is for (`attributes.rs`) and the data that `$d` symbols mark among code. `mod.rs` holds the types and the queries on them, and begins with the steps of loading a file and a map of the files; `load.rs` takes those steps, and `sections.rs` parses the file, refusing one that isn't a linked program or library, and reads the section headers, for `load.rs` and `memory.rs`. `memory.rs` reads only the headers and the bytes that the sections put in memory, which `diff` compares, and `header.rs` only the ELF header and program headers, for `startup` to refuse a file for another processor before loading it. Unnamed data, such as string literals, is found from what code and pointers refer to. Each function's code is scanned once, as the file loads (`Binary::code_references`). `names.rs` makes mangled names readable and works out the short names that people type for long ones, and the texts of a name that patterns match; its module doc says how a short name is found quickly, and how most names are ruled out for a pattern. `made_names.rs` makes the names of items that no symbol names, such as `fn_0x8000040`, and tells them apart. `startup.rs` gives the startup walk (`arch::startup::Program`) the file's code, the words that memory holds at reset, as in flash or a load image, and where its functions are, and `listing.rs`, with the `tui` feature, gives browse a function's code as `arch` prints it, with the data that `$d` symbols mark among it. The unit tests of `names.rs`, `vector_table.rs` and `unnamed_functions.rs` are in `names/tests.rs`, `vector_table/tests.rs` and `unnamed_functions/tests.rs`, and those of `listing.rs`, which check the Thumb text against objdump's, in `listing/thumb_text.rs`. |
| `src/graph/` | The reference graph that connects items, the roots where the program starts, the dominator tree that says what is exclusive to what, the shortest paths that `why` and `reach` show, the bytes that a function reaches (`reached.rs`), what removing several items at once would remove (`removal.rs`), the items in ranges of memory (`items_in_ranges.rs`) and the references that cross the ranges' boundary, narrowed with `--from` or `--to` (`boundary.rs`), and where a linker's veneers jump to. |
| `src/debuginfo/` | Reading DWARF, when the file has it: the copies of functions that the compiler inlined, for `inlined`, for the error when SYMBOL names a function that the compiler inlined everywhere, and for `startup`, to find a `main` that the compiler inlined. |
| `src/render/` | Text output: tables, and the trees in their last column (`tree.rs`), which every command shows the same way: as rows that name the row they hang under (in `why`, only where chains branch), or drawn with `--tree`. |
| `tests/it/` | Integration tests, built as one test program (`main.rs` lists its modules). See [Tests](#tests). |
| `testdata/` | The sample programs and the ELF files built from them. See [Sample binaries](#sample-binaries). |
| `scripts/` | `same-output.sh`, which checks that two builds of elfq print the same answers, or with `--time` times them; and `sample-pairs.txt`, the pairs of samples for its `diff` runs, which the command in [Checking that output didn't change](#checking-that-output-didnt-change) gives it with `--pairs`. |
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
`render` nothing about ELF files: a tree's row carries a `graph::Node`
only so that browse can open the item the row is about. No tool checks
this; a `use crate::` line that reaches up the list is a sign that the
code belongs lower down.

In `commands/`, a command calls only the shared modules beside it, which
`commands/mod.rs` lists: `arguments.rs`, `checks.rs`, `symbol/`,
`retained_tree.rs`, `uses.rs`, `range_check/`, `flat_answer.rs`,
`tree_answer.rs`, `text.rs`, `json.rs` and `kept.rs`. Code that two
commands need goes in one of those, or in a module lower down. Code
outside a command calls it only to ask whether it would answer, for a
Try line, which ends an answer with a command to run next, or for a
suggestion in an error:

- The overview and `diff` ask `startup::reads` whether to suggest
  `startup`.
- When `diff` answers from the builds' bytes alone, it asks
  `startup::check_header` first, so that it parses the new build only
  when its headers allow it.
- The error for a function that the compiler inlined everywhere asks
  `inlined::shows` whether the `inlined` command it suggests needs
  `--all`.
- `arguments.rs` runs `callers` and `callees` to find which form of them
  answers in place of `--range`.

Tests may run any command, as the unit tests of `checks.rs` run the
overview and `top`.

`commands/browse/` sits at the top of `commands/`, and nothing else uses
it. It asks the six commands it shows for their answers, the overview,
`reach`, `callers`, `callees`, `why` and `retained`, as `callers::answer`
for "used by N", so that a page says what the commands print. It is a
child of `commands`, so it reaches their `pub(super)` helpers without
making them public. It, `arch/listing.rs` and `binary/listing.rs` build
only with the `tui` feature.

- **Each rule has one home.** When several places need the same rule,
  one function holds it and the others ask it, as decode's
  `Effect::sets_flags` says when an instruction sets the flags, for both
  Thumb readers. Some rules are still written more than once (see
  [below](#where-the-code-doesnt-follow-these-yet)).
- **Analysis never prints.** `arch`, `binary`, `graph` and `debuginfo`
  return what they find. Problems found while loading a file go into
  `Binary::warnings`, which `commands::run` prints. Only the commands and
  `main.rs` write output.
- **Commands decide what an answer shows.** A command looks up SYMBOL,
  asks `graph` or `debuginfo` for the facts, and picks, groups, sorts and
  words what to show. Scanning code, dominators, shortest paths and
  crossings live in the modules below. A small query that only one
  command needs can stay in its file, as `retained`'s list of what
  SYMBOL shares does; one that another command could use belongs in
  `graph`.
- **Work out the facts, then word them.** The types that analysis and
  searches return hold facts only. A command builds one answer from
  them, then writes it as text or as JSON, as `top.rs`, `startup/` and
  `diff/` do (see [Adding a command](#adding-a-command)), and browse
  shows the command's answer rather than wording its own. The overview's
  text and its JSON say different things, so each has a path of its own,
  and its text is written as it is worked out. Some code doesn't do this
  yet (see [below](#where-the-code-doesnt-follow-these-yet)).
- **Library code returns errors; it doesn't panic or exit on bad input.**
  Add context where the error happens, with anyhow's `context`, as
  `Binary::load` adds "cannot load FILE". `main.rs` prints `error: ` and
  the whole chain, and exits with status 2. `unwrap`, `expect` and
  `panic!` are for tests; an assertion outside them, such as the one in
  `Table::row`, catches only elfq's own mistakes. `unsafe` code is
  forbidden (`Cargo.toml`).
- **A damaged file is read as far as it makes sense.** As elfq loads a
  file, it ignores an allocated section that would end past the last
  address, with a warning, and reads a symbol whose size would take it
  past the last address as if it had no size. One whose last byte is the
  very last address is read one byte shorter. So the rest of the code
  can add a size to an address without checking. Stored pointers are
  read only where the file holds bytes, however large a section claims
  to be, and sums of sizes stop at the largest number rather than
  overflow.
- **A file does one job.** A file that does several becomes a folder,
  whose list of files is its map, as `commands/diff/` and
  `arch/thumb/startup/` are. A long file can still do one job, as
  `arch/thumb/decode.rs` does, and `commands/browse/views/names.rs`,
  whose module doc numbers the steps by which it fits a name to a
  column. Unit tests longer than about 300 lines move to a `tests.rs` of
  their own (see [Tests](#tests)).
- **A long function reads as named steps.** A function of more than 100
  lines of code, not counting comments and blank lines, is the usual
  sign that it does several things: give each a function of its own,
  with a doc line. clippy's `too_many_lines`, set in `Cargo.toml` and an
  error in CI's clippy, is the check. One decoding match may be longer,
  and allows the lint with that reason, as `decode::narrow` does; so may
  tests, which list cases or pin outputs: `tests/it/main.rs` allows it
  for the integration tests, and a `tests.rs` of unit tests that needs
  it allows it at its top, as `arch/thumb/decode/tests.rs` does. A
  function that would need clippy's `too_many_arguments` allowed takes a
  struct instead; none does. Some long functions still wait to be split
  (see [below](#where-the-code-doesnt-follow-these-yet)).
- **Let the compiler hold the rules.** Where fields must agree, an enum
  holds the cases that make sense, as diff's `Change` does, rather than
  Options that are `None` together, bools with rules between them, or a
  number such as 0 or `u32::MAX` that means something else. A match
  where a new variant must decide names every variant, with no `_` arm
  that returns a likely answer, as `InstructionSet`'s methods do. Some
  code doesn't do this yet (see
  [below](#where-the-code-doesnt-follow-these-yet)).
- **Name what crosses a function boundary.** A tuple that a function
  returns or a struct keeps gets a struct with named fields, as the
  bytes of code and data that `graph::Reached` counts are a
  `binary::CodeAndData`, and a bool argument gets an enum, so that a
  call says what it means. A sort key used in one place may stay a
  tuple. Much of the code doesn't do this yet (see
  [below](#where-the-code-doesnt-follow-these-yet)).
- **One word, one meaning.** A word means one thing across elfq,
  plumbing takes none of the words that name elfq's things, and indexes
  and addresses are named apart. Check a new name against
  [Words](#words), and add the words you bring in.
- **A test runs elfq through `support/`.** A test of the program runs
  it with `run`, or with `command` when it needs more, such as a pipe
  for its input; it changes a sample's bytes with `ElfBytes`, in
  `support/elf.rs`; and a test of one command goes in
  `tests/it/commands/<name>.rs`. See [Tests](#tests). Two tests don't
  do this yet (see [below](#where-the-code-doesnt-follow-these-yet)).
- **Visibility says how far a declaration's uses reach.** A function,
  type, constant or field is private unless code outside its module uses
  it. It is `pub(super)` when only its parent module and the parent's
  other children use it, `pub(in crate::commands)`,
  `pub(in crate::commands::browse)` or `pub(in crate::arch)` when its
  uses lie further apart inside one of those, and `pub(crate)` when they
  cross top-level modules. `pub` is only for what `src/main.rs` and
  `tests/it` use, such as `commands::run`, `Binary::load` and browse's
  `App`, and for what those show, as `Graph::roots` shows `Root`. The
  helpers in `tests/it/support/` are `pub(crate)`. A declaration that
  only browse uses is behind `#[cfg(feature = "tui")]`, as
  `callers::answer` is, since CI's clippy also builds without `tui` and
  refuses dead code. A doc link counts as a use, as `arch/startup.rs`
  links to the startup walk's limits in `arch/thumb/startup/mod.rs`, and
  a `pub` declaration's doc may link to a `pub(crate)` one, which
  `Cargo.toml` allows. The `unreachable_pub` lint, set in `Cargo.toml`
  and an error in CI's clippy, finds a `pub` declaration that nothing
  outside the crate can reach, but never looks at fields. The rest is
  kept by hand: a `pub` field of a struct that isn't `pub`, a `pub`
  declaration that outside code could reach but doesn't use, as in a
  module that `lib.rs` makes public, and a `pub(crate)` or `pub(super)`
  that could be narrower.

### Where the code doesn't follow these yet

Later cleanups will take these on. Until then, they are no model for new
code.

- Each rule has one home:
  - Which rows of an answer are the largest is written three times: by
    size and then by name in `uses.rs` (`largest_rows`), for `callers`
    and `callees`, and in `reach/rows.rs` (`Text::largest`), for
    `reach`'s rows by block and its `--tree`; and by size and then by
    number in `tree_answer::largest`, for the trees of retained sizes
    and for `inlined`.
- Work out the facts, then word them:
  - The hidden `debug` commands write as they go.
- A long function reads as named steps:
  - The functions that wait for a later cleanup are marked with
    `#[expect(clippy::too_many_lines, reason = "waits to be split; see CONTRIBUTING")]`,
    so a search for "waits to be split" lists them. Once one is split,
    the lint no longer fires there and the unmet `expect` fails CI's
    clippy, so the change that splits it drops the attribute too.
- Let the compiler hold the rules:
  - The Thumb readers match `Flow` and `Operation` with `_` arms in
    places, as `Function::step` in `arch/thumb/values/mod.rs` and
    `Walker::flags` in `arch/thumb/startup/execute.rs` do.
  - The startup walk gives a word that no register points to the
    pointer `NOWHERE`, the highest address, so that it sorts last
    (`arch/thumb/startup/each_iteration.rs`).
- Name what crosses a function boundary:
  - About sixty functions, most of them private, return tuples, as
    `thumb::layout` returns (offset, size, piece) and `Binary::words`
    (address, value) pairs.
  - `Operands::targets` holds (offset, address) pairs, and the startup
    walk keeps its ways out of a loop as pairs of addresses (`pending`).
  - About fifty arguments are bools, as in `Block::parts` in
    `diff/blocks.rs`, `Boundary::unfollowed_from` in
    `graph/boundary.rs` and the startup walk's `Walker::execute`.
- A test runs elfq through `support/`:
  - In `tests/it/commands/browse.rs`,
    `the_command_under_the_callers_runs_in_a_shell` starts elfq through
    `sh`, and `in_a_terminal` through `timeout` and `script`.

## Words

A word means one thing across elfq, so that a name read in one module
means the same in the next. Check a new file, type or field name against
this list, and add to it when you bring in a word. Plumbing, such as a
helper type, doesn't take a word of the list; where code still does, as
`Kept` and the private `Run`s below, the entry says so.

- **item**: a function or data object in the file (`binary::Item`). Its
  place in `Binary::items` is an item index, named `index` or `item`.
- **index**, **address**: an index is a place in a list, and an address
  a place in memory, and their names keep them apart. An index is named
  `index`, or after what it is the place of, as `item`, `section`,
  `event` or `step`; an address is named `address`, or after what is
  there, as `site` for an instruction's and `target` for where a call or
  jump goes, and never `item` or `index`. Where both are about, the
  names say which, as in diff's `edited.rs`: `old_item` is an index, and
  `old_address` an address.
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
  doesn't hold. **node**: an item or an import, as the graph connects them
  (`graph::Node`). **root**: a node where the program starts, such as the
  entry point or a handler that the vector table lists (`graph::Root`).
- **use**: a reference from one node to another, such as a call, a jump
  or a stored pointer: what `callers` and `callees` list.
- **inside**: in `callers` and `callees` of ranges or a set, or with
  `--from` or `--to`, the side of the boundary that the answer is about:
  the ranges given in place of SYMBOL, SYMBOL's item, or a set's items,
  wherever they are (`range_check::Inside`), whose items are an
  `ItemsInRanges`. A row's `inside` (`CrossingRow`) is its item there.
  The ranges of `--from` and `--to` are on the far side (`far_side`).
  The `inside` key of `startup`'s JSON, the function that a step is in,
  is another thing.
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
- **pattern**: a name with a `*` in it that stands for any text, such as
  `'uart_*'` (`symbol::Pattern`). `retained`, `callers` and `callees`
  take every item that it matches, and the other commands that take
  SYMBOL the one item that it matches; browse lists them in its go-to
  box when it matches several. An argument is one only when it finds
  nothing as a name, so that a name with a `*` in it still finds its item
  (`Names::look_up_or_match`). It matches an item when it matches all of
  one of the **texts** of the item's names: each name, the paths that
  mean the same and their ends (`names::for_each_pattern_text`). These
  texts are unrelated to text, the format of an answer.
- **module**: in the error for a name that finds nothing, a module, type
  or class of the program that elfq reads, which the name may mean: the
  error offers the pattern for what is in it, as `'core::fmt::*'` for
  `core::fmt` (`Names::module_pattern`), when the command can take what
  that matches. `Takes` says what the command takes: a set, whose error
  offers it whenever it matches an item, or one item, whose error offers
  it only when it matches one. elfq's own modules, as in the map above,
  are other things.
- **end**: in short names and patterns, a path from one of its parts on,
  as `Gain::run` and `run` are ends of `app::Gain::run`. A text that a
  pattern matches is a whole path or the end of a longer one
  (`names::TextKind`), under the parts before it, as `Gain::run` is under
  `app`, and the `matches` of `retained`, `callers` and `callees` with
  `--json`, and `debug matches`, say `end` in `matched_by` for an item
  that a pattern matches only by the end of a longer path. The startup
  walk's `End`, and where a function ends, which elfq may not know
  (`Item::end_unknown`), are other things.
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
- **lost**, **top**, **matters**, **kept by others**, **kept together**:
  what removing several nodes at once does (`graph::Removal`). A node is
  lost when something reaches it and a search from the roots that never
  enters a removed node doesn't, so what each removed node keeps is lost
  with it. The tops are the lost nodes whose parent in the tree isn't
  lost, much as the tree's top (`DominatorTree::top`), which the `top`
  command lists, holds the nodes with no parent at all. A removed node
  matters when the search stopped at it: it is a root, or a node that the
  search reaches uses it. One that something reaches and that doesn't
  matter is kept by others (`Removal::is_kept_by_others`, and
  `kept_by_others` in `retained`'s JSON): every chain from a root to it
  passes another removed node, so the others keep it, alone or together,
  and it adds nothing. Unlike kept by, which names the one item that keeps
  another, kept by others may take several of them at once. A top that
  isn't removed is kept together by its keepers (`graph::KeptTogether`):
  the removed nodes that matter and lead to it through lost nodes that
  don't matter. Removing just them loses it, and removing all of them but
  one keeps it. `lost` in `arch/thumb/values`, where a value goes when
  elfq stops following it, is unrelated.
- **unusable**: in `retained`, a SYMBOL, or a pattern or range in its
  place, that it can't use, such as a name that no item has, a pattern
  that matches none or a range that holds no item, which its error lists
  with why (`Unusable`, in `retained/symbols.rs`). **left out**: in
  `retained`, a function given as SYMBOL, or that a pattern or range
  given in its place picks, whose end elfq doesn't know
  (`Item::end_unknown`), such as glibc's `_init`: elfq can't read its
  code, so the answer doesn't count its bytes, though the first line
  counts it among a pattern's items, and names it in a note and in
  `left_out` (`Symbols`, in the same file). The words' other uses, as for
  the rows that a table leaves out without `--all`, have their everyday
  sense.
- **lead**: an answer's first line, which names what the answer is about,
  as `Symbol::lead_name` names an item there, and `Set::lead_name` a set,
  as a `LeadName`, which takes a capital where elfq's own words start a
  sentence, but never in a name (`LeadName::at_start`). When patterns are
  among what `retained`, `callers` or `callees` was given, or a range that
  holds only code, which `callers` takes, its lead names each part of what
  was given in turn, a `Named`: a SYMBOL, the ranges or the patterns.
  `Parts`, in `retained/parts.rs`, is unrelated: it holds what removing
  the SYMBOLs loses.
- **files here**: the arguments given in place of SYMBOL that are files
  in the current folder, which the shell may have put there in place of
  a pattern (`symbol::FilesHere`); the line under an error names them.
- **set**: the items that several SYMBOLs, patterns and ranges given in
  place of SYMBOL pick together, each once, as `retained`, `callers` and
  `callees` take them (`symbol::Set`). What one of them picks is a
  `Pick`, and an item of the set a `Found`. `callers` and `callees` take
  any arguments but one SYMBOL alone, or ranges alone, as a set, and
  answer for it as for ranges (`Subject::Set`, `Inside::Set`). An
  instruction set (`arch::InstructionSet`), Rust's `HashSet`, and set as
  a verb, as in "sets the flags" or "sets up a section", are other
  things.
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
  code, in `arch/thumb/values/` (`State`). What the startup walk knows at
  one point, of the registers, the flags and the words that code stored,
  is `Known`, in `arch/thumb/startup/walk_state.rs`. A section's state
  when `main` starts, in `startup`, is its status (`problems::Status`).
- **row**: a line of a table that elfq prints, or of its JSON, where each
  element of a list in the answer's object is a row (`json::write`,
  `json::Rows`); a loop over a table of sections also reads one row of
  that table each time it runs.
- **shape**: how a table shows a tree: as rows that name the row they
  hang under, or drawn with `--tree` (`TreeShape`). A command's
  **format** is text, tree or JSON (`Format`).
- **brief**: what a command shows without `--all` (`Detail::Brief`, in
  `commands/text.rs`; a `Detail` is named `detail`): the first rows of a
  long table or tree, as the command ranks them, such as `top`'s 30
  largest items or `why`'s chains through the 10 nearest, with a row such
  as "6 more" that sums up the rest, and in `diff`, one row for several
  changes; and the first names or addresses of a long list in a
  sentence, then "and 5 more". With `--all`, it shows all of it
  (`Detail::All`). Each command says how many a brief answer has room
  for, and `Detail::room` gives that many, or every one. Browse shows a
  tab's notes brief when they don't fit in full, and briefest, naming
  only the first item of each list, when they don't fit even so
  (`Printed`, in `views/printed.rs`).
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
- **event**: one thing that the startup walk records, such as a call, a
  store or a loop (`arch::startup::Event`); its place in the walk's list
  is an event index, named `event`, or `loop_event` for a loop's.
  Browse's event loop, which reads the terminal's keys (crossterm's
  `Event`), is unrelated. **iteration**: one time through a loop's code,
  from its head; a loop that runs 5 times has 5 iterations, and the last
  goes only as far as where it leaves (`Runs::Counted`). Types and fields
  name it so, as `Iteration` and `Runs::EachIteration` do.
  **walked loop**: a loop that the walk went through one iteration at a
  time, as a loop over a table of sections, so that each iteration's calls
  and stores are events of their own (`Loop::walked_iterations`).
  **call tree**: in `startup`, the functions that run when a function
  runs, each with the function that calls it on a shortest chain from the
  first (`CallTree`). **piece**: bytes that a loop or stores copy or zero
  one after another, which join into a step (`startup`'s `pieces.rs`).
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

Some code still uses one of these words for something else. The Thumb
walk's `Step` is what an instruction leads to; `reach`'s `Step` is
SYMBOL, or an item or import that it reaches, at its place in the tree,
whose place in the search is a `StepId`; and a loop's `LoopStore::step`
is how far a store moves each iteration, which `pieces.rs` reads beside
`Piece::step`, a step index. The functions named `answer`, as
`callers::answer`, `callees::answer` and `top`'s, return the text of an
answer, a `FlatAnswer` or a `TreeAnswer`, rather than what the command
worked out. Those are names to fix, not meanings to copy.

## Tests

`cargo test` runs two kinds of tests:

- **Unit tests** sit beside the code, in a `tests` module at the end of a
  file, or in a `tests.rs` of their own when they are long: a folder's,
  when they would make up most of its `mod.rs`, as in
  `arch/thumb/startup/`, or in a folder named after the file, as
  `arch/thumb/decode/tests.rs` holds `decode.rs`'s. They check one
  function: a decoder on bytes from a sample's disassembly, or an
  algorithm on a small graph built by hand (`graph/dominators.rs`,
  `graph/paths.rs`, `graph/removal.rs`).
- **Integration tests** in `tests/it/` use the samples in `testdata/bin`.
  Most run the elfq program, the way a user would: each command's tests,
  in `commands/`; the tests of what the commands share, in
  `across_commands/`; and `docs.rs`, which runs the examples in the docs,
  checks the paths that CONTRIBUTING.md and design.md cite, and checks
  that `scripts/same-output.sh` runs every command. Those in `library/`
  call the library, as `Binary::load` and `Graph::build`.
  `damaged.rs` and `commands/browse.rs` do both, and so do one test in
  `commands/callers_and_callees.rs`, the test in
  `across_commands/errors_and_help.rs` that takes the commands' names from
  `cli::command_names`, and the checks at the end of `commands/reach.rs`.
  Those checks load samples to list the items they check, and run commands
  in the test's own process, with `run_in_process`, which is quicker for
  the hundreds they run. The answers that integration tests expect are
  those in [testdata/README.md](testdata/README.md), which says, for
  example, what calls `log_msg` in each sample.

| File in `tests/it/` | What it tests |
|---|---|
| `support/` | No tests: in `mod.rs`, the samples' paths (`SAMPLE` and others), the lists that several modules check (`C_SAMPLES`, `FAR_SAMPLES`), `samples()`, which lists every sample, and `samples_for`, which lists those for one architecture, `sample_path` and `load_sample`, `node`, which finds SYMBOL in a loaded sample, `command`, the program set to run in the crate's root, `run`, which runs it and returns a `Run` to check, with `Exit`, its shortcuts `stdout`, `stderr`, `crossing`, `elfq_json` and `text_rows`, `names`, which reads the names in a list of elfq's JSON, `shell_words`, which splits a command line as a shell does, `run_in_process`, `assert_output`, and `TempFolder`, a folder of a test's own, with the files it names, to run elfq in; in `elf.rs`, `ElfBytes`, which changes a sample's bytes field by field and saves the file, and the changed files that several modules use, such as `without_section_headers` |
| `commands/browse.rs` | `browse`'s screens, driven with keys on ratatui's `TestBackend`, a screen in memory, with what a pattern opens and what the go-to box lists for one; its errors; and on Linux, the program in a terminal that `script` makes. The checks that need its private types are unit tests; see below |
| `commands/callers_and_callees.rs` | `callers` and `callees`, which share their table and their ranges: their text and JSON for SYMBOL, for ranges of memory, for patterns and several SYMBOLs, and with `--from` and `--to`, and the errors about those ranges and sets |
| `commands/debug.rs` | `debug items`, `debug walk` and `debug matches`; the tests that `debug walk` lists the walk that `startup` reads are in `commands/startup.rs` |
| `commands/diff.rs` | `diff` on pairs of builds: its text, its JSON, its exit status and the builds it refuses, and that `scripts/sample-pairs.txt` lists its pairs of samples; and at the end, on builds made by changing a sample's bytes: a warning about the new build, bytes or blocks that alone differ, and a separate debug file and a file that puts nothing in memory, which it refuses |
| `commands/inlined.rs` | `inlined`: its ranking and its tree, as text and JSON, for a pattern that matches one function, and its errors for a file without DWARF and for a pattern that matches several items |
| `commands/overview_top_and_retained.rs` | The overview, `top` and `retained`, which share the tree of retained sizes and `--range`: their text and JSON, for the whole file and for a range, `retained` of several SYMBOLs, of ranges and of patterns, and the errors about ranges and patterns, with the line under clap's error for a pattern given to `top` |
| `commands/reach.rs` | `reach`'s text and JSON, with and without SYMBOL, and for a pattern that matches one item; and, run in the test's own process, `reach` on every item of the Cortex-M samples, against `callees`, `retained` and its own `--tree`, and with `--to` against `reach` without it and `callees --to`, and on every function of each stripped one against its twin with symbols; and without SYMBOL, each start of every sample against `reach START` |
| `commands/startup.rs` | `startup` on the samples whose reset code has a mistake and some without, the exit status of every Cortex-M sample, and the files it refuses; and that `debug walk` lists the walk that `startup` reads |
| `commands/why.rs` | `why`: its chains as text and JSON, where they start, what it says when nothing reaches SYMBOL, and for a pattern, the one item it matches and the errors for several or none |
| `across_commands/json.rs` | What the commands' JSON shares: the keys of every item, each row on a line of its own, and the other names of SYMBOL's item |
| `across_commands/tables.rs` | What the commands' tables share: the tree of `--tree`, at most nine levels deep, the lines that sum up the rest of a long list, and `--all` |
| `across_commands/names.rs` | The names that the commands print and take: what SYMBOL gave when it isn't the item's name, as a pattern that matches one item, readable Rust and C++ names and their short forms, and the names of data that no symbol names |
| `across_commands/errors_and_help.rs` | What the commands share in their errors and help: the arguments and files they refuse, errors on stderr with exit status 2 and what to try instead, the line that says that the arguments are files here, which the shell may have put in place of a pattern, in a `TempFolder`, under the commands' own errors and clap's, in its colours, the exit status when what reads the answer stops early, and help |
| `library/items.rs` | Finding items: symbols, PLT stubs, unwind tables, functions in stripped firmware, unnamed data |
| `library/graph.rs` | The reference graph: callers, callees, pointers in data, veneers |
| `library/retained.rs` | Roots, and what is exclusive to what |
| `library/removal.rs` | What removing several items at once removes, on every sample: for one item, what is exclusive to it; for sets of up to 10 nodes, what two plain searches from the roots find, each top's keepers, and the bytes in a range; and the example in `graph/removal.rs`'s module doc |
| `library/why.rs` | The shortest chains |
| `library/symbol.rs` | Looking up SYMBOL |
| `library/inlined.rs` | Inlined copies, from DWARF |
| `damaged.rs` | Damaged and unusual files, made by changing a sample's bytes |
| `docs.rs` | The `$ elfq` examples in README.md, docs/design.md and docs/examples.md; that design.md names every JSON key; that the paths, module paths and type paths that CONTRIBUTING.md and design.md cite are there; and that `scripts/same-output.sh` runs every command |

**Where a test goes.** A test of one command goes in that command's file
in `commands/`, its text and its JSON together. A test that runs several
commands to check what they share goes in `across_commands/`, in
`json.rs`, `tables.rs`, `names.rs` or `errors_and_help.rs`, and a test of
a part of the library, called as the commands call it, goes in `library/`.
A test whose point is that elfq reads a damaged file without hanging,
crashing or misreading it goes in `damaged.rs`, whatever command it runs;
a test of one command's wording on such a file goes in that command's
file.

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
- `views/picker.rs`: that no two rows of the go-to box read the same;
  that a box opened with the error for SYMBOL over its rows doesn't work
  out that error again; and on every sample, that a pattern lists the
  largest of the items it matches, and the one item it matches alone as
  SYMBOL's answer names it.
- `views/names.rs`: each step of shortening a name.
- `app.rs`: that tabs 2 to 6 keep their table's header and selected row
  on a small screen, that tabs 4, 5 and 6 are worked out once the screen
  says so, that what keeps each item is worked out once, after the first
  page is drawn, and that the start page under the go-to box is worked
  out only once the box closes.

The Thumb text that a function's page shows is tested beside the code that
gives it, `binary/listing.rs`, in `binary/listing/thumb_text.rs`: against
objdump's text for each Cortex-M sample, in `testdata/tests/thumb`, and in
each stripped sample, against the `$d` symbols of its twin.

A test that goes through `samples()`, or `samples_for`, which picks from
it, reads every `.elf` and `.so` file in `testdata/bin`, so it tests a new
sample as soon as it is committed, wherever the test is. Such tests are in
`commands/overview_top_and_retained.rs`, `commands/reach.rs`,
`commands/diff.rs`, `commands/startup.rs`, `across_commands/tables.rs`,
`library/removal.rs` and `library/symbol.rs`; browse's unit tests go
through a `samples()` of their own, in `src/commands/browse/views/mod.rs`,
and other unit tests list the files in `testdata/bin` themselves:
`every_sample_names_its_core`, in `binary/attributes.rs`, and the tests of
the Thumb text, in `binary/listing/thumb_text.rs`. Two of the tests in
`tests/it/` run the overview and every command it suggests on every
sample, and `top` on each `.elf` file:
`every_command_the_overview_suggests_works`, in
`commands/overview_top_and_retained.rs`, and
`no_line_sums_up_a_single_item`, in `across_commands/tables.rs`. Each
takes seconds. The test harness starts tests in the order of their paths,
a few at a time, so a test that slow belongs where its path sorts early,
as `across_commands::tables::no_line_sums_up_a_single_item` does: one that
starts last keeps the whole run waiting for it.

To run one test, one file, or the unit tests of one module:

```sh
cargo test --test it commands::overview_top_and_retained::top_shows_what_keeps_the_most_bytes
cargo test --test it damaged::
cargo test --lib arch::thumb
```

A filter picks every test whose path holds it, so `commands::` picks the
tests in `across_commands/` too; `cargo test --test it commands:: --
--skip across_commands` runs only those in `commands/`.

**Samples.** A test names a sample by its path from the crate's root,
such as `testdata/bin/c-x86_64.elf`, which is what elfq takes as FILE:
`SAMPLE` and the other constants in `support/` hold that path, `samples()`
lists every sample that way, and `load_sample` and `run_in_process` take
it. Only `screen` and `burst`, in `commands/browse.rs`, take the file's
name, as the screen's first line shows it. The lists that several modules
check are in `support/` too: `C_SAMPLES`, the C sample for each
instruction set, and `FAR_SAMPLES`, the builds of `c/far.c` with symbols,
whose calls between RAM, flash and ROM go through veneers. So is
`samples_for`, which lists the samples for one architecture, from the
machine in their ELF headers, beside `samples()` and `sample_path`. Any
other constant or list that only one module uses stays in it.

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

**When output changes.** Most tests in `commands/` check a command's whole
output with `assert_output`. When you change what elfq prints on purpose,
the test that fails shows the first line that differs, then the new output
in full, after "The whole output:". Check it, then paste it over the
expected text, which starts at the beginning of the line after `"\`. Some
expected texts are `format!` templates, with placeholders such as
`{path}`; put those back. A test that checks several outputs stops at the
first that differs, so run it again until it passes.

**The examples in the docs.** A code block in README.md, docs/design.md or
docs/examples.md whose first line starts with `$ elfq ` is run by
`docs.rs`, which checks that elfq prints the rest of the block and nothing
on stderr. The command is split at spaces, with no shell quoting, and its
exit status may be 0 or 1. The text around an example often repeats its
numbers and names, as docs/examples.md does, so when an example's output
changes, check that the text around it still matches the new output.

**Paths in the docs.** `the_paths_the_docs_cite_are_there`, in `docs.rs`,
checks what CONTRIBUTING.md and docs/design.md cite. Outside their code
blocks, that is each path in `src/`, `tests/`, `testdata/`, `scripts/` or
`docs/`, each shorter path to a Rust file or a folder, such as
`views/item.rs`, each link's target, each module path, such as
`symbol::find`: its modules must be there, and the name after them must be
defined in the last one's file or imported there with `use`; and each type
path, such as `Symbol::lead_name`: the name after the type must be a
function, constant, associated type, field or variant in the type's
definition or in an `impl` of it. In a code block that holds no `$ elfq`
line, as design.md's map of the layers, it is each module path that
starts with a module of elfq's, and each type path. When you move or
rename a file, or a function cited with its module path or its type, the
test names every citation to change. A name cited alone, such as
`check_code_is_readable`, isn't checked, nor is a type path whose type
elfq doesn't define as a struct, enum or trait, such as gimli's
`EndianSlice::read_slice` or a type alias's, nor a path with a
placeholder, such as `src/arch/<name>.rs`, or one through a `target`
folder: write a file to add, or one that is gone, that way or in a code
block. Write a path into another crate from the crate's name, as
`object::elf::SHT_NOBITS`, and add the crate to the test's
`OTHER_FIRST_NAMES`, since a short `elf::` would be read as a path into
elfq's own module of that name. In the same way, a type that elfq
defines is read as elfq's, so write another crate's type of the same name
from the crate's name, as `serde_json::Value::as_str`. The test doesn't
find a method that a derive or a trait's default body gives a type, so
cite it after its trait, as `Parser::try_parse_from`, which isn't
checked.

**Damaged files.** `damaged.rs` makes its files from a sample's bytes, so
that the hangs and crashes they once caused stay fixed. It and the other
tests that need such a file change the bytes with `ElfBytes`, in
`support/elf.rs`, which sets a field of the ELF header, a section header
or a program header by its name, such as `HeaderField::EntryPoint` or
`SectionField::Size`, to a number or to one of `object::elf`'s constants,
and changes a symbol's name or size, or a section's bytes. Its `save`
writes the file and returns the path to give `run`. Tests run in a debug
build, so arithmetic that overflows fails them too. A test that uses
`parse_quickly` fails if loading the file takes more than 10 seconds: add
one when you make elfq quick to load a file that took far too long.

## Adding a command

Commands take the form `elfq FILE <command> [ARGS]`. `src/commands/top.rs`
is a short one to copy.

1. **Arguments, in `src/cli/`.** Add a variant to `Command` in `mod.rs`,
   before the hidden `Debug`. Its name, in lowercase with `-` between
   words (`SizeOf` is typed `size-of`), is the command's name, and its
   `///` line is what `elfq --help` lists. Give it an `after_help` with a
   longer explanation, a constant in `help.rs` in `Command`'s order, as
   the others have, and a line in `EXAMPLES` there. Its arguments go in a
   struct of their own, as `RetainedArgs` holds SYMBOL and `--all`.
   `--json`, `--tree` and `--range` are global, so don't declare them
   again. `elfq help COMMAND` and the hint for a command typed before the
   file find the new command by themselves.
2. **The command, in `src/commands/<name>.rs`.** Start with a `//!` line
   that shows how to type it, then a `pub(super) fn run`, which
   `commands/mod.rs` calls. It takes what the command needs: the `Binary`
   (and the file's path, if the answer names the file, as `overview`
   does), the arguments it reads, the `Format` and the output. It returns
   `Result<Outcome>`: `Outcome::Answered`, or for exit status 1 another
   variant, as `callers` and `callees` return for ranges.
3. **Wiring, in `src/commands/mod.rs` and `arguments.rs`.** Add
   `mod <name>;` and an arm in `run`. In `arguments.rs`, `check_tree`,
   `check_range` and `check_json` each have an arm for every command, and
   the compiler asks for the new one in each: put it with the commands
   that take `--tree`, `--range` or `--json`, or with those that refuse
   it. `Cli::all`, in `cli/mod.rs`, has an arm for every command too: a
   command that takes `--all` reads it there, and gets `detail` from its
   arm in `run`, and one that doesn't goes with `browse` and `debug`. A
   command that takes `--range` gets `range` from its arm in `run`;
   name it in `RANGE_REFUSED`, the error that lists the commands that
   take `--range`, in the tests that check that error, which a search of
   `tests/it/` for its text finds, and in the `--range` help in
   `cli/mod.rs`, README.md and docs/design.md. A command that shows a
   tree goes likewise in `check_tree`'s error, its tests and the
   `--tree` help. A command that takes one SYMBOL at most goes in
   `TAKE_ONE_SYMBOL_AT_MOST`, the list of commands whose clap error for
   the arguments after it gets the line that says that they are files
   here, which the shell may have put in place of a pattern; one that
   takes several ends its own error with that line, with
   `symbol::with_shell_hint`, as `retained` does.
4. **SYMBOL.** If the command reads code, call `check_code_is_readable`
   first, so that a file whose code elfq can't read says so even when
   SYMBOL names nothing. Make the other checks that need no graph, such
   as `readable_item` below, or `check_has_bytes` for `--range`, before
   building the graph (step 8), as `top` and `reach` do, so that a
   mistyped argument fails at once; a check that needs the graph, such
   as `reach`'s check of `--to`, comes after it. `Names`, in
   `commands/symbol/lookup.rs`, finds what a name means and names items
   for the user. A command whose answer names items with it keeps the
   one that finds SYMBOL, so that the names are worked out once, as
   `why`, `retained` and `reach` do:
   `let names = Kept::new(Names::new(binary));`, then
   `names.find(symbol)?`, which gives a `Symbol`: what a name or an
   address finds, or the one item that a pattern matches, with an error
   that lists them when it matches several. `retained`, which
   words its errors its own way to list every argument it can't use,
   calls `names.look_up_or_match` instead, whose `LookupOrMatch` says
   what the name finds without wording it: a `Lookup`, or when the name
   finds nothing and is a pattern, the items that the pattern matches.
   It puts what its arguments pick together in a `symbol::Set`, which
   counts each item once and names them in the first line. It words the
   error for a name that finds nothing with `nothing_named`, and passes
   `Takes::Set`, since it takes a set, so that the error offers the
   pattern for what is in a module of that name whenever it matches an
   item; `find` takes one item, so its error offers that pattern only
   when it matches one.
   One whose answer needs no `Names` calls
   `symbol::find`, which frees its `Names` before the answer, as
   `inlined` does. `callers` and `callees`, which take ranges of memory,
   patterns and several SYMBOLs in place of SYMBOL too, call
   `symbol::subject`, whose `Subject` holds one `Symbol`, the ranges, or
   a `Set`, and which stops at the first argument it can't read;
   `retained`, which takes ranges too, reads each argument with
   `symbol::argument`, as `subject` does. Both word the error for a name
   that could mean several with `Names::choices`, which ends it with the
   pattern for all of them, where `find` words it with `could_mean`. A
   `Symbol`'s `node` may be an import.
   `item_in_file` or `readable_item`, in `checks.rs`, gives its item, or
   an error that says why there is none, and the command's own checks
   come next, as in `retained::find_symbols` and `why::refusal`; a
   command that needs the roots refuses shared libraries, as `top`,
   `retained` and `why` do. The answer's first line names SYMBOL with
   `Symbol::lead_name`, which says when SYMBOL gave another of its names,
   an address inside it, or a pattern that matches only it.
5. **The answer.** Work out the facts once, then write them as text or
   as JSON. Every error, such as a refusal, comes before the first line
   is written, so that elfq prints a whole answer or only an error.
   `top.rs` shows this in one file: its facts are a `RetainedTree`,
   which `run` writes as a `TopJson`, or through `answer` as a
   `TreeAnswer`. A command with many facts gets a folder, and an `Answer`
   that holds them, which its own `text.rs` and `json.rs` write, as
   `startup/` and `diff/` do.
6. **Text.** The text of an answer is a `FlatAnswer` for a table of one
   row to a line, as `callers` builds one, or a `TreeAnswer` for a table
   that holds a tree of `TreeRow`s, as `top` builds one; browse shows
   either on a page. The wording that commands share is in `text.rs`. A
   tree cut down to its largest rows uses the functions at the end of
   `tree_answer.rs`, as `top` does through `RetainedTree::largest`.
   `startup/` and `diff/`, which browse doesn't show, build their tables
   with `render::Table` themselves.
7. **JSON.** The JSON is a struct that derives `Serialize`, made of the
   shapes in `json.rs`, such as `json::Symbol` for SYMBOL's item, which
   adds its `aliases`, and `json::Item` for every other item, and
   written with `json::write`. Every key is always there, `null` when it
   has no value. `json::write` puts each element of a list that the
   answer's object holds on a line of its own, so give the rows their
   own list there, as a flat list rather than a tree. A tree's rows go
   in the text's order, each with an `id` counting from 0 and the
   `parent` id of the row it hangs under, or `null`, as
   `RetainedTree::to_json` makes them. A list whose rows would take much
   memory held all at once, as the chains of `why`, the items and calls
   of `reach` and the items of `diff` would, is a `json::Rows`, made
   from an iterator that makes each row as `json::write` writes it, so
   that the command never holds them all. Making a row then mustn't
   fail, since every error comes before the first line. Other lists stay
   `Vec`s, which read more plainly, even those that grow with the
   program, as `top`'s items do.
8. **Large values.** Keep what the command builds that is large, such as
   the graph, `Names` or a `RetainedTree`, in a `Kept`, from `kept.rs`
   (`let graph = Kept::new(Graph::build(binary));`), so that elfq ends
   without freeing it a piece at a time. A `Kept` is dropped when it goes
   out of scope, so the compiler checks that what its value borrows
   outlives it: declare it after what it borrows, and give a struct that
   borrows kept values a lifetime for them apart from the file's, as
   `why`'s `Chains` has. A value kept only until an error return and
   then moved on, as `retained::run` keeps its `Names`, is taken back out
   with `into_inner`.
9. **Tests.** Add `tests/it/commands/<name>.rs`, and list it in
   `tests/it/commands/mod.rs`. `tests/it/commands/inlined.rs` is a short
   one to copy: a test of its text for each kind of answer, pinned with
   `assert_output`, one of its JSON, and one of its error. A command
   whose exit status means something checks it on every sample it reads,
   as `every_arm_sample_gets_an_answer_and_the_exit_status_it_means` in
   `startup.rs` does, and one that refuses some files tests those, as
   `startup_reads_only_cortex_m_firmware` does. Add the command to the
   tests in `tests/it/across_commands/errors_and_help.rs` that go through
   commands one by one:
   `arguments_that_dont_go_together_are_refused_before_the_file_is_read`,
   `a_file_whose_code_elfq_cant_read_says_so` if it reads code, and
   `commands_that_need_roots_refuse_a_shared_library` if it needs the
   roots.
10. **Docs.** List it in README.md and in the Commands table in
    docs/design.md, and its JSON keys in design.md's JSON section. Add
    answers of it to `JSON_ANSWERS` in `tests/it/docs.rs` with an entry
    in each of its lists: the test checks that the section names every
    key.
11. **The output check.** Add it to `check_file` in
    `scripts/same-output.sh`, with the arguments it takes, and to the
    commands whose help `check_help` compares, so that a later change
    that shouldn't change its output is checked (see [Checking that output
    didn't change](#checking-that-output-didnt-change)).
    `same_output_sh_runs_every_command`, in `tests/it/docs.rs`, checks
    that `check_help` names every command, and that the script runs each
    `debug` command and its help.

## Adding a view

`browse`'s pages are views, in `src/commands/browse/views/`. Copy
`help.rs` for a page of its own, and `callees.rs` for a tab of an
item's page. An item's page, `item.rs` with its parts in `item/`, is
the biggest view, and not the one to start from.

1. **The page, in `views/<name>.rs`.** A struct with a
   `pub(super) fn draw`, which draws it in the area it is given, and a
   `pub(super) fn handle`, which does what an `Action` asks; `View`, in
   `views/mod.rs`, calls both. A page that lists rows keeps a `Cursor`,
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
   A tab of an item's page, such as `callees.rs`, is a `Printed` too,
   which its `pub(super) fn tab` makes, but `ItemPage`, in `item.rs`,
   holds it in a `Pane` of its own: add it to `Tab` and `Tab::ALL`, with
   its number and the name of the command that prints the same answer in
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
4. **Tests.** Pin its screens in `tests/it/commands/browse.rs` at 80x24
   and 100x30, and drive it with keys there, as the other tests do.

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

1. **The scanner, in `src/arch/<name>.rs`.** Its `pub(super) fn scan`,
   which `InstructionSet::scan` calls, takes one function's bytes and
   address and returns a `CodeReference` for each address the code calls,
   jumps to or uses; `CodeReferenceKind` in `src/arch/mod.rs` lists the
   kinds. It must not panic on any bytes: a truncated instruction ends the
   scan. Unit tests beside it check it on bytes from a sample's
   disassembly.
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

[testdata/README.md](testdata/README.md) lists what each sample contains
and the answers tests should expect. If you change a sample's source,
rebuild with `testdata/build.sh` and commit the new files with the source
change. For a Cortex-M sample, also run `testdata/thumb-text.sh`, which
writes objdump's text for its code to `testdata/tests/thumb`, where the
tests of `browse`'s Thumb text read it, and commit that too. Then read
docs/examples.md again: it says what the samples' source does, down to
line numbers in testdata/rust/src/main.rs, and no test checks that. A new
file in `testdata/bin` is tested as soon as it is committed, by every test
that goes through `samples()`, wherever the test is (see [Tests](#tests)).

## Checking that output didn't change

A change that shouldn't change what elfq prints, such as a cleanup or a
speed-up, needs more than the tests: run `scripts/same-output.sh` with a
release build of `main` and one of your branch, on as many ELF files as
you can find.

```sh
git worktree add ../elfq-main main
(cd ../elfq-main && cargo build --release)
cargo build --release
scripts/same-output.sh ../elfq-main/target/release/elfq target/release/elfq \
    testdata/bin /usr/bin --pairs scripts/sample-pairs.txt
```

It runs every command, with and without `--json`, `--all` and `--tree`, on
each file, the SYMBOL commands on the first three items that `top` lists,
and the range commands on the first two blocks of memory that the overview
lists, with `reach SYMBOL --to RANGE` on the first item and each block.
`retained` and `callers` also take the first two items as a set,
`retained`, `callees` and `debug matches` a pattern made from the first
item's name, and `why` a name that isn't there. It also runs every
`--help`, and `diff` on each pair of files listed after `--pairs`, one
pair a line: `scripts/sample-pairs.txt` lists the pairs of samples in
`DIFF_PAIRS`, in `tests/it/commands/diff.rs`, by their paths from the
crate's root, and another `--pairs` adds pairs of your own. It prints
each command whose stdout, stderr or exit status differs, with the first
lines that differ, and exits with status 1 if one does. On the samples
alone, with their pairs, it runs about 6,800 commands, in about 15
seconds on four processors.

The samples are small and similar, so add other files too: programs from
`/usr/bin`, a Rust build of elfq itself, and firmware, which
`testdata/build.sh` and the SDKs of chip makers can build. A pull request
that says its output didn't change should say what files it was checked
on.

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
prints the median times side by side, in milliseconds. Compare the peak
memory too (`/usr/bin/time -v` on Linux, `-l` on macOS), and the output,
which should not change unless you mean it to.

## Pull requests

Keep them small and focused: one change at a time, each merged before the
next. [docs/design.md](docs/design.md#status-and-roadmap) says what comes
next.
