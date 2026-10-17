# elfq design

elfq answers questions about an ELF binary's call graph and size that other
tools can't: what code exists only because of one function, how many bytes
that costs, and why a function is in the binary at all. It works on binaries
as they are shipped (x86-64 Linux and Cortex-M firmware, with or without
DWARF, even stripped) without rebuilding them.

[survey.md](survey.md) explains why these questions were chosen: it records
what existing tools do on the same sample binaries.

## Non-goals

- **Re-implementing readelf, objdump, nm or bloaty.** elfq does not print
  headers, section tables, disassembly listings or flat size breakdowns. When
  one of those tools already answers a question well, the docs point to it.
- **A reverse-engineering framework.** No decompiler, no interactive session.
- **Runtime profiling.** elfq reads the file; it never runs the program.

## Commands

Every command has the form `elfq <FILE> <command> [ARGS]`: the file first,
then the question, so that a command sits next to the symbol it asks about
(`elfq app.elf callers log_msg`). `elfq <FILE>` alone prints an overview.
`retained` and `why` are the reason elfq exists; the rest support them,
except `inlined`, which reads DWARF instead of machine code.

| Command | Answers |
|---|---|
| `elfq FILE` | What is this file, what did elfq find in it, which items keep the most bytes, and what can I ask next? |
| `elfq FILE retained SYMBOL` | What does SYMBOL keep alive, as a tree, and how many bytes is that? |
| `elfq FILE why SYMBOL` | Which chains of calls and references from the roots keep SYMBOL in the binary? |
| `elfq FILE top` | Which items keep the most bytes, as a tree? |
| `elfq FILE callers SYMBOL` | What calls or uses SYMBOL directly, and how (call, tail call, uses, address taken, pointer in data)? |
| `elfq FILE callees SYMBOL` | What does SYMBOL call or use directly? |
| `elfq FILE callees RANGE...` | What do the code and data in these ranges of memory use outside them, such as code in ITCM that calls a function in flash? `callers RANGE...` asks the reverse. |
| `elfq FILE inlined [SYMBOL]` | Which inlined functions cost the most bytes across the binary, or inside SYMBOL? Needs DWARF. |

SYMBOL is a function or data object's name, an import (`write` finds the
`write@plt` stub that code calls), or an address such as `0x1700`. Names are
shown demangled (`app::process_frame`, not `_RNvCs…13process_frame`), and
the end of a name is enough: `process_frame`, or `Gain::run` for the trait
method `<app::Gain as app::Stage>::run`. C++ names are shown with their
parameters, as `c++filt` shows them, but can be typed without:
`process_frame` finds `app::process_frame(app::Frame const&, app::Stage&)`,
`log(unsigned int)` picks one of two overloads of `app::log`, and
`vtable for Gain` finds `vtable for app::Gain`. A name that matches exactly
wins over one that only ends the same way. When a name could mean several items,
the error lists the shortest name that picks each one, and a misspelled name
gets suggestions. Typing the command first, as in
`elfq callers app.elf log_msg`, gets a hint that the file comes first, and
`elfq help top` prints the same help as `elfq app.elf top --help`.

`callers` and `callees` also take one or more ranges of memory in place of
SYMBOL, written as `--range` takes them: `0x0+256K`, or `0x0..0x40000`.
Names don't start with a digit, so an argument that does and has `..` or
`+` in it is a range. Then they list only the references that cross the
edge of the ranges, one row per pair of items, grouped by the item outside,
largest first. A call that goes through a linker's veneer is listed as a
call to the veneer's target, at the call to the veneer, which is in code
someone wrote, with "via veneer" after how it calls. For now, elfq only
recognizes the veneers GNU ld adds for Thumb-2 (`ldr.w pc, [pc]` and the
target's address); other veneers are listed as functions of their own.
elfq's exit status is then 1 when anything crosses, or might: through a
call elfq can't follow, through a pointer to a function in the ranges, or
in a function in them whose end elfq doesn't know. A CI job can then check
that code in ITCM stays there, or that code run from RAM never touches
flash. As with `diff`, the exit status is 0 otherwise, and 2 for any
error, with or without ranges. When the reader stops early, as `head`
does, elfq still finishes and exits with the answer's status.

All commands print a table or tree for people by default. `--json` prints
the same answer for scripts. The JSON has every item, where the text
shows only the largest; every key is always there, `null` when it has no
value; addresses are hex strings, as elfq prints them and SYMBOL takes them,
and sizes are numbers. An item looks like this:

```json
{
  "name": "process_frame",
  "kind": "function",
  "address": "0x1700",
  "size": 532,
  "size_estimated": false
}
```

`kind` is `function`, `data` or `import`; an import has no address or size
in the file. In the trees of `top` and `retained`, each item also has its
`retained` size and `keeps`, the items under it. `why` gives each chain as a
list of steps, each an item and how the one before uses it. Where the HOW
column says `uses` for data, JSON keeps `access` (code reads or writes it)
apart from `address taken` (code loads its address). The overview's
JSON says what the file is and what elfq found in it; the largest items are
in `top`'s.

For example:

```
$ elfq testdata/bin/c-x86_64.elf retained process_frame
Removing process_frame would save 2068 bytes:

RETAINED   OWN  ITEM
    2068   532  process_frame
    1083    59  ├─ crc32
    1024  1024  │  └─ crc_table
     378   271  ├─ apply_filter
     107    75  │  └─ filter_tap
      32    32  │     └─ fir_coeffs
      54    54  ├─ decode_header
      12   ~12  ├─ .rodata@0x2012
       9    ~9  └─ .rodata@0x201e

Shared with other code, so not counted: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
```

A tree shows its 30 largest items and sums up the rest under their parents:
`└─ 6 more` stands for six items, and its RETAINED column adds up their
bytes. A line never stands for a single item, since the item's own line
takes no more room, and lists of names and addresses never end in "and 1
more" either. `--all` shows every item instead, in `top`, `retained` and
`inlined`.

Every tree, in every command, is drawn at most 9 levels below its top
row, so names never start more than 27 columns into the tree and long names
don't push it off the screen. Where a tree goes deeper, the row 8 levels
down ends with `(continued below)`, and the rows under it come after the
rest of the table, under a row `NAME (continued)`, by the same rule. Rows
with nothing under them stay in place, 9 levels down. `retained chain1`
on `testdata/bin/c-deep.elf`, a chain of calls 20 deep, shows it.

`top` shows the same kind of tree for the whole file, starting from the
roots, and the overview starts with its five largest items. On the Rust
x86-64 sample it shows that the standard library's backtrace printer keeps
228,391 of the file's 282,384 bytes of code and data.

`why` answers from the other end: each item that uses SYMBOL keeps it in the
file, and the tree shows the shortest chain to each of them from where the
program starts. Users that nothing reaches, or that are there only because
of SYMBOL, don't keep it and are left out of the tree:

```
$ elfq testdata/bin/c-x86_64.elf why log_msg
log_msg is used by 4 functions, through these chains:

HOW            ITEM
entry point    _start
address taken  └─ main
call              ├─ app_tick
call              │  └─ log_msg
address taken     ├─ on_button
tail call         │  └─ log_msg
call              ├─ process_frame
call              │  └─ log_msg
call              └─ report
call                 └─ log_msg

debug_dump uses log_msg too, but nothing reaches debug_dump.

Removing main would remove log_msg too.
```

`inlined` ranks the functions that the compiler copied into their callers by
the bytes all their copies take, which ddbug shows only one function at a
time. Each copy's bytes leave out the copies inlined into it, so the rows add
up to the file's inlined code. On the Rust x86-64 sample, built with DWARF,
`EndianSlice::read_slice` tops the list: 302 copies in 6,388 bytes. With
SYMBOL it shows the copies in one function as a tree, merging the copies of
one function in one place:

```
$ elfq testdata/bin/c-x86_64.elf inlined report
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
```

With `--range START..END` or `--range START+SIZE`, the overview, `top` and
`retained` count only the bytes inside one range of memory, such as a
microcontroller's ITCM or DTCM. An item outside the range still shows when
it alone keeps bytes inside it, with none of its own: on the Cortex-M7
sample, which follows an i.MX RT1176's memory map, `process_frame` in flash
keeps 200 of ITCM's 224 bytes. When the sections sit in several blocks of
addresses, as firmware's flash and RAMs do, the overview lists the blocks,
and a `--range` command to try for each block that elfq counts bytes in, so
the range needn't come from the chip's reference manual. Sizes can take a
unit, in any case: `0x20000000+256K`. Units are powers of 1024, as memory
sizes usually are.

### What "exclusive" and "retained" mean

elfq builds a graph whose nodes are **items** (functions and data objects) and
whose edges are **references**: a call, a tail call (a jump into another
function), or an address being taken, for example a function pointer stored in
a table or loaded into a register. An item is **exclusive** to X when every
path from the binary's roots to it passes through X, which is to say X
dominates it. **Retained size** is the size of X plus everything exclusive to
X: the bytes that would disappear if X did.

For direct calls this is exactly "only called from X". For functions reached
through pointers, static analysis cannot know which indirect call reaches
them, so elfq attributes them to the code that takes their address (and the
table that stores it). That is the same rule the linker's `--gc-sections`
uses, so retained sizes match what removing code would save. Output marks such
functions "(through a pointer)" so the reader knows.

Calls through pointers also leave some answers incomplete, and elfq says so
where they do. `callers` notes that it leaves out calls through a pointer to
a function whose address is taken or stored, and `callees` lists the calls
and jumps it can't follow. `retained` notes when SYMBOL's code calls
through pointers, since what those calls reach may count for other items.
In JSON, `callers` and `callees` have `complete`, and `retained` has
`indirect_calls`. For ranges, `complete` is `false` when `callees` finds a
call it can't follow in them, or a function whose end it doesn't know, or
when `callers` finds a function in them whose address is taken or stored,
other than in the vector table, which only the processor calls through.
`callers` can't see calls from code whose end elfq doesn't know, such as
assembly without a `.size`, but C runtimes have such code (`_init`,
`_fini`) in almost every file, so it doesn't make the answer incomplete.

## How it works

Data moves through five modules in order: `arch` finds the addresses that
machine code uses, `binary` finds the items, `graph` connects them by those
references and computes dominators and paths, `commands` run queries, and
`render` prints text. `debuginfo` reads DWARF beside the graph, for
`inlined`.

1. **Load** the file with the `object` crate: sections, symbols, entry point,
   dynamic relocations.
2. **Discover items**, each with an address, size and a record of how it was
   found. Sources, best first: the symbol table; the dynamic symbol table;
   PLT stubs, named after the import they jump to (`write@plt`), and arrays
   of startup functions such as `.init_array`, named after their section;
   `.eh_frame` entries (on x86-64 they cover almost every function, even in
   stripped files); the entry point, and the Cortex-M vector table when no
   item covers any of it, named after its section, or after the section and
   address (`.text@0x8000000`) when it shares it; in firmware that nothing
   lists functions for, the functions that the code leads to from the entry
   point, the vector table's handlers and the startup arrays, and those
   whose Thumb addresses code loads or data holds, where the code before
   them ends (see `unnamed_functions.rs`); and data that code or a stored
   pointer refers to but no symbol names, such as vtables, string literals
   and, in stripped files, variables and the constants that some linker
   scripts put in `.text`. Such data starts at each address something refers
   to and runs to the next one, the next item or the end of its section;
   elfq never guesses which function it belongs to. Items without a symbol
   get names like `fn_0x1690`, `.rodata@0x2004` (section and address) or
   `entry`. Rust and C++ symbols are demangled, and the mangled symbol stays
   as an alias. `elfq FILE debug items` lists what was found, for checking
   this step.
3. **Scan code** with an architecture backend. For each function it decodes
   the instructions and reports direct calls, tail jumps to other items,
   indirect call sites (target unknown), and PC-relative references to items
   (`lea rip+...` on x86-64; `adr`, literal-pool `ldr` and `movw`/`movt` on
   Thumb). It does not need a full disassembler, only these instructions. On
   Thumb it also skips the literal pools, jump tables and constants that
   `adr` points at inside functions, which would otherwise decode as
   instructions. The entry point's code is a special case: startup code
   copies or clears whole sections such as `.data` and `.bss`, loading where
   each starts and ends through symbols that the linker script defines, such
   as `_sdata` and `_edata`. When the entry point loads both ends of a
   section, those addresses aren't uses of the items that start there.
4. **Scan data** for references from data items to other items. When the file
   is position-independent (most Linux executables are), its dynamic
   relocations give every stored pointer exactly. Otherwise, as in firmware,
   elfq reads data items word by word and takes a word that equals the start
   of an item (plus 1 for a Thumb function) for a pointer to it. A number
   that happens to equal an address is mistaken for a pointer, which is rare
   because addresses are large numbers.
5. **Find roots**, where the program starts running: the ELF entry point;
   the functions in `.preinit_array`, `.init_array` and `.fini_array`, and
   the code in `.init` and `.fini`; the PLT header, which the dynamic loader
   uses; personality routines that `.eh_frame` names, which the unwinder
   calls; notes such as `__abi_tag`; and the Cortex-M vector table,
   recognized by its shape rather than its section name: a word in data
   holding the entry point's Thumb address, or the second word of a code
   section, where a linker script may put the table; then handlers until an
   entry other than a reserved one is neither 0 nor the address of Thumb
   code, or the table's section ends, less the reserved entries at its end
   that hold other words, which are code after a short table. Not yet:
   exported symbols of shared libraries, which need a shared-library sample,
   and `--root SYMBOL`. Non-allocated sections and unwind tables
   (`.eh_frame`, `.ARM.exidx`) are never sources of references; otherwise
   everything would look reachable. One more source is only on Arm: LLVM
   merges small variables into one block (`.L_MergedGlobals`) and code uses
   them through the block's address, so that address counts as a use of each
   variable in the block.
6. **Analyze**: compute the dominator tree once from a virtual node that
   points at every root. `retained` and `top` read it. `why` searches from
   the roots for the shortest chain to each item that uses SYMBOL, going
   around SYMBOL itself, and prefers calls among equally short chains.
   `callers` and `callees` read edges.
7. **Read DWARF** when it is present (`gimli`), in `debuginfo`: each
   inlined-subroutine entry is a copy of one function in another, with the
   address ranges of its code. `inlined` adds them up. Copies whose code the
   linker dropped still have entries, so elfq skips those that don't start
   inside an item. Nothing above depends on DWARF; source locations could
   make output easier to follow later.

## Code layout

```
src/
├── main.rs        Parses arguments, runs one command, prints errors. Nothing else.
├── lib.rs         Declares the modules below. elfq is a library plus a thin binary.
├── cli.rs         clap definitions, one struct per subcommand.
├── commands/      One file per subcommand: find the symbol (symbol.rs), run a query, render (tree.rs builds the rows of retained sizes, json.rs has shared JSON shapes, edge.rs is callers and callees for ranges).
├── binary/        ELF loading, item discovery, names and address ranges.
├── arch/          One file per instruction set (x86_64.rs, thumb.rs), each with a `scan` function.
├── graph/         The reference graph, its roots, dominator tree and shortest paths, and what crosses the edge of ranges of memory.
├── debuginfo/     DWARF, when the file has it: what the compiler inlined.
└── render/        Tables, and trees drawn in a table's last column (tree.rs).
```

Rules that keep it approachable:

- **Analysis flows one way**: arch, then binary, then graph, each importing
  only from the ones before it. debuginfo imports only binary. `render` knows nothing about ELF files, and
  `commands` connect the two.
- **Analysis never prints and commands never analyze.** A command is a short
  function that wires a query to a renderer.
- **Adding an architecture** means writing one function ("given a
  function's bytes and address, report its references") in a new file under
  `arch/`, plus sample binaries.
- **Adding a command** means one clap struct in `cli.rs`, one file in
  `commands/`, and tests that run it on `testdata/bin`.
- **Library code returns errors, it does not exit or panic** on bad input;
  `main.rs` adds context and prints them.

## Dependencies

Pure Rust only, so installing elfq needs no C toolchain:
`object` (ELF), `gimli` (DWARF), `iced-x86` (x86-64 decoding), `petgraph`
(graph and dominators), `clap` (arguments), `rustc-demangle` (Rust names)
and `cpp_demangle` (C++ names). Thumb has its own small decoder in
`arch/thumb.rs`: the code scan needs about a dozen instructions, which are
easier to read in one file, with the samples' disassembly as the reference,
than through a general disassembler's API.

## Testing

- **Unit tests** beside the code for algorithms, using hand-built graphs.
- **Integration tests** run each command on the committed files in
  `testdata/bin`. [testdata/README.md](../testdata/README.md) lists the
  expected answers, for example that what only `process_frame` leads to in
  the C sample is `decode_header`, `crc32`, `crc_table`, `apply_filter`,
  `filter_tap`, `fir_coeffs` and two message strings, with or without DWARF,
  plus the linker's veneers on Cortex-M7.
- **Snapshot tests** pin the human-readable output so format changes are
  reviewed on purpose.
- **CI** runs the checks that [CONTRIBUTING.md](../CONTRIBUTING.md) lists,
  and checks that elfq builds with Rust 1.88, the oldest version it
  supports. The sample binaries are committed, so CI needs no cross
  toolchains.

## Delivery

Each step lands as one or more small pull requests, merged before the next
starts.

| Step | Pull requests |
|---|---|
| 1. Survey and samples | This document, survey.md, testdata/ |
| 2. Core engine | 2a: crate skeleton, CLI, CI, ELF loading and item discovery. 2b: x86-64 scanning, PLT stub names, the reference graph, `callers`, `callees` and the overview. 2c: Rust demangling and short names for SYMBOL. 2d: Cortex-M Thumb scanning, and pointers in firmware data such as the vector table. 2e: items for unnamed data, such as vtables and string literals. |
| 3. Headline queries | 3a: roots, dominators, `retained` and `exclusive`. 3b: `why`. 3c: `top`. 3d: `inlined`. |
| 4. Polish | 4a: C++ demangling, with a C++ sample. 4b: JSON output for `callers`, `callees`, `exclusive` and `inlined`. 4c: JSON for `top`, `retained`, `why` and the overview. 4d: warnings where indirect calls make answers uncertain. |
| 5. Memory ranges | 5a: a Cortex-M7 sample with code and data in ITCM, DTCM, OCRAM and flash, at the addresses an i.MX RT1176 uses; startup code no longer seems to use the first item of each section it copies. 5b: `--range START..END` or `--range START+SIZE` for the overview and `top`, and the overview lists the address blocks that the file's sections occupy. 5c: `--range` for `retained` and `exclusive`. |
| 6. Clear output | 6a: trees deeper than 9 levels continue below them. 6b: shorter lead lines and notes for the overview, `top` and `retained`, which say what a size means. 6c: no line sums up a single item, and "1 byte". 6d: shorter lead lines and notes for `why`, `callers`, `callees` and `inlined`, and "uses" for data on every architecture. 6e: `exclusive` folds into `retained`, and `--all` shows every item; `inlined SYMBOL` instead of `inlined --in SYMBOL`; `--json` instead of `--format json`; `elfq help top` works. 6f: fixes for what step 6 turned up: no `--range` suggestions that fail on stripped firmware, no empty tree for a range that nothing reaches, and no crash on an argument that isn't UTF-8. |
| 7. Stripped firmware | 7a: stripped and `-nodebug` copies of the Cortex-M7 sample. 7b: the vector table is found while loading the file. 7c: the Thumb decoder tells apart the jumps that can carry on in another function. 7d: functions in stripped firmware, found by following the code from the entry point and the vector table. 7e: functions that only data points to, such as Rust trait methods in vtables. 7f: an item for the vector table of stripped firmware, so that its bytes count. 7g: those functions leave out the padding after them, so their sizes match the symbols'. 7h: fixes for what a review and 91 other builds turned up: the startup arrays are followed too, a `b.w` that stays in its function starts nothing, and an address that code loads or data holds starts a function only where the code before it ends, so far fewer numbers that look like addresses start one. 7i: firmware whose linker script puts the vector table and the constants in `.text`, as CMSIS's template does, with a sample laid out that way. 7j: fixes for what a second review turned up: a jump that lands where a number points keeps it from starting a function, the jump tables of `tbb` and `tbh` end where the `cmp` before them says and jump to their cases, a short vector table in `.text` ends at its last handler, an `adr` to Thumb code marks no data, and a test for each rule. |
| 8. Range checks | 8a: `callers` and `callees` take ranges of memory in place of SYMBOL and list what crosses their edge, at the call that goes through a linker's veneer; the exit status is 1 when something crosses or might, and 2 for errors; the constants in big-endian (BE8) firmware are read the right way round. |

Later: a query language, and worst-case stack depth (call graph plus each
function's stack frame).

## Open questions

1. **Where unnamed data ends.** Unnamed data starts wherever something
   refers to it, so a struct in a stripped file whose fields code uses one
   at a time becomes one item per field, and the end address of an array,
   which loops compare against, can start an item of its own. The bytes
   still all count, split across more items than the source has. Worth
   fixing only if it makes real answers confusing.
2. **Functions that following the code misses.** In stripped firmware,
   code that nothing calls or points to, a function that code reaches only
   with a 16-bit `b`, and constants that only the function before them
   loads stay part of that function. A `bl` to a place inside its own
   function, as Cortex-M0 code uses for far jumps, a forward `b.w` in a
   long unoptimized function, and a number that equals the address right
   after a return, such as a CRC polynomial in firmware whose code starts
   at address 0, start a function where there is none. The bytes still
   all count, under a neighbouring function. Worth improving if it makes
   answers on real firmware confusing.
3. **What range checks don't see yet.** LLD's veneers, GNU ld's for
   Cortex-M0 and M23, and long calls such as `ldr r3, =f; bx r3` load the
   target's address and jump through a register, so the veneer or the
   function that calls is listed as taking that address, and the jump as
   one elfq can't follow; the exit status is still right. In stripped
   firmware, elfq doesn't find the functions that only such veneers lead
   to, so their calls are missing too, and a range that holds only them
   is an error. GNU ld's veneers
   with `--pic-veneer` add an offset to the program counter instead,
   which elfq reads as an address: `callees` names the wrong function,
   and `callers` misses the call and can exit 0. A veneer to code outside
   every item, such as a routine in ROM, is listed under the veneer's
   name. Startup code that copies sections outside the entry function, or
   reads a table of them as CMSIS does, adds rows to `callers`. The next
   step, which follows long calls, covers the veneers.
