# elfq design

elfq answers questions about an ELF binary's call graph and size that other
tools can't: what code exists only because of one function, how many bytes
that costs, and why a function is in the binary at all. It works on binaries
as they are shipped (x86-64 Linux and Cortex-M firmware, with or without
DWARF, even stripped) without rebuilding them.

[survey.md](survey.md) explains why these questions were chosen: it records
what existing tools do on the same sample binaries.

## Concepts

These words mean the same in elfq's output, in these docs and in the code.
Each ends with the type that holds it.

**Item.** A function or a data object in the file, with a name, an address
and a size in bytes. Most items come from the symbol table, but elfq also
finds those that no symbol names, such as string literals and the
functions of stripped files, and names them after where they are:
`fn_0x1690`, `.rodata@0x2004`. `elfq FILE debug items` lists every item and
where elfq found it. (`binary::Item`)

**Import.** A function or variable that the file takes from a shared
library, such as `write`. Its code isn't in the file, so it has a name but
no address or size. On x86-64, code calls an import through a **PLT stub**,
a few instructions in the procedure linkage table that jump to it; elfq
makes an item of each stub and names it after where it jumps, as in
`write@plt`. A shared library calls its own exported functions through
stubs too, such as `log_msg@plt`. (`graph::Node::Import`)

**Reference.** One item's direct use of another item or of an import, at a
**site**: the address of an instruction, or of a pointer stored in data.
Items and imports are the nodes of elfq's **reference graph**, and
references are its edges. (`graph::Reference`, `graph::Graph`) The HOW
column of `callers`, `callees` and `why` says what kind of reference each
is (`graph::ReferenceKind`):

| HOW | JSON `how` | Meaning |
|---|---|---|
| `call` | `call` | a direct call |
| `tail call` | `tail call` | a jump into another function, which then returns to the caller's caller |
| `uses` | `access` or `address taken` | code reads or writes data, or loads its address |
| `address taken` | `address taken` | code loads the address of a function or an import, as when passing a function pointer |
| `pointer` | `pointer` | data holds a pointer to it, as a table of functions does |

**Root.** A place where the program starts running: the entry point, the
functions that the C runtime runs at startup and exit, such as those in
`.init_array`, the PLT's header, which the dynamic loader uses,
personality routines, which the unwinder calls, notes that the system
reads, and on Cortex-M the vector table and each handler it lists. An item
that no chain of references from a root reaches is **unreachable**: dead
code, or used in a way elfq can't see. The functions that a shared library
exports aren't roots yet, so `top`, `retained` and `why` don't answer for
libraries. (`graph::Root`)

**Exclusive and retained size.** An item is **exclusive** to X when every
chain of references from the roots to it passes through X, so that it is
in the file only because X is: X **dominates** it. In the **dominator
tree**, each item hangs under the closest item that dominates it, and the
roots, with whatever several roots reach, hang from the top. An item's
**own size** is its own bytes, and its **retained size** is its own size
plus the own sizes of everything below it in the tree: the bytes that would
go away if it did. (`graph::DominatorTree`)

Every kind of reference counts. elfq can't tell which call through a
pointer reaches a function, so a function that only pointers lead to counts
for the code or data that holds its address, which is the same rule that
the linker's `--gc-sections` follows. Trees mark such a function "(through a
pointer)", since nothing calls it directly.

**Unresolved call.** A call or jump that elfq finds in code but can't turn
into a reference: one through a pointer set at run time, as in a call
through a function pointer or a C++ virtual call, or one to an address
where elfq knows no item, such as code in ROM. Where they could change an
answer, elfq says so. (`graph::Unresolved`)

**Estimated size.** When the file doesn't record an item's size, elfq
estimates it, mostly as the distance to the next item, and marks it `~`,
as in `~12`, or `size_estimated` in JSON. A function with nothing after it
in its section can't be estimated: its size shows as `?`, and elfq can't
read its code. (`binary::Item::size_estimated`)

**Range and memory block.** A range is the addresses from START up to END,
written `START..END` or `START+SIZE`. With `--range`, the overview, `top`
and `retained` count only the bytes inside one range, and `callers` and
`callees` take ranges in place of SYMBOL. A memory block is a run of
sections less than 64 KiB apart, which stands for one memory, such as flash
or a RAM; the overview lists the blocks when there are several.
(`binary::AddressRange`, `binary::MemoryBlock`)

**Boundary and crossing.** For `callers` and `callees` with ranges, an item
is inside when any of its bytes is in one of the ranges, and a reference
**crosses** the boundary when one end is inside and the other isn't. The
help calls the boundary the ranges' edge. (`graph::Boundary`,
`graph::Crossing`)

**Thumb.** The instruction set of Cortex-M processors, with 16- and 32-bit
instructions. Arm sets bit 0 of a Thumb function's address wherever the
address is stored: in symbols, the entry point, pointers and the vector
table. elfq clears it, so the addresses it prints are even. elfq reads
Thumb code, but not the older 32-bit Arm (A32) code. (`binary::Binary::thumb`)

**Vector table.** On Cortex-M, the list of addresses that the processor
reads at reset and on each interrupt or exception: the initial stack
pointer, then the reset handler, which is the entry point, then the other
handlers. elfq finds it by its shape rather than its section's name.
(`binary::VectorTable`)

**Veneer.** A few instructions that a linker adds when a call's target is
too far away for the call instruction, as between flash and ITCM: the code
calls the veneer, which jumps on to the target. The range forms of
`callers` and `callees` list a call through a veneer as a call to its
target, "via veneer"; elsewhere a veneer is a function that tail-calls its
target. elfq knows only the veneers that GNU ld adds for Thumb-2 so far.
(`arch::is_veneer`)

**TCM, ITCM and DTCM.** Tightly coupled memory: small, fast RAM wired to a
core such as the Cortex-M7, ITCM for instructions and DTCM for data. elfq
knows nothing about any chip, but TCM is what `--range` and the range forms
of `callers` and `callees` are for: counting what sits in one memory, and
checking that code in ITCM doesn't call into slow flash.

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
| `elfq FILE callers SYMBOL` | What calls or uses SYMBOL directly, and how (call, tail call, uses, address taken, pointer)? |
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
list of steps, each an item and how the one before uses it, in the words of
the JSON column in [Concepts](#concepts). The overview's JSON says what the
file is and what elfq found in it; the largest items are in `top`'s.

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
x86-64 sample (`rust-x86_64-nodebug.elf`) it shows that the standard
library's backtrace printer keeps 228,391 of the file's 282,384 bytes of
code and data.

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
up to the file's inlined code. On the Rust x86-64 sample with DWARF, which
`testdata/build.sh` builds but which is too large to commit,
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

### Calls elfq can't follow

Calls through pointers leave some answers incomplete, and elfq says so
where they do. `callers` notes that it leaves out calls through a pointer to
a function whose address is taken or stored, and `callees` lists the calls
and jumps it can't follow. `retained` notes when SYMBOL or the items it
keeps call through pointers, since what those calls reach may count for
other items.
In JSON, `callers` and `callees` have `complete`, and `retained` has
`indirect_calls`. For ranges, `complete` is `false` when `callees` finds a
call it can't follow in them, or a function whose end it doesn't know, or
when `callers` finds a function in them whose address is taken or stored,
other than in the vector table, which only the processor calls through.
`callers` can't see calls from code whose end elfq doesn't know, such as
assembly without a `.size`, but C runtimes have such code (`_init`,
`_fini`) in almost every file, so it doesn't make the answer incomplete.

## How it works

elfq reads the file, then answers one question. `binary` reads the file and
finds its items; `graph` has `arch` read each function's code, connects the
items by the references it finds, and works out the roots, the dominator
tree and the shortest chains; and the command picks what to show and writes
it, as a table that `render` lays out, or as JSON. `debuginfo` reads DWARF
beside the graph, for `inlined`. [Life of a command](#life-of-a-command)
follows one command through the functions that do this.

1. **Load** the file with the `object` crate: sections, symbols, entry point,
   dynamic relocations.
2. **Discover items**, each with an address, size and a record of how it was
   found. Sources, best first: the symbol table; the dynamic symbol table;
   PLT stubs, named after the import they jump to (`write@plt`) or the
   file's own function (`log_msg@plt`), and arrays of startup functions
   such as `.init_array`, named after their section;
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
   the functions that shared libraries export, and `--root SYMBOL`.
   Non-allocated sections and unwind tables
   (`.eh_frame`, `.ARM.exidx`) are never sources of references; otherwise
   everything would look reachable. One more source is only on Arm: LLVM
   merges small variables into one block (`.L_MergedGlobals`) and code uses
   them through the block's address, so that address counts as a use of each
   variable in the block.
6. **Analyze**: compute the dominator tree once from a virtual node that
   points at every root. `retained` and `top` read it. `why` searches from
   the roots for the shortest chain to each item that uses SYMBOL, going
   around SYMBOL itself, and prefers calls among equally short chains.
   `callers` and `callees` read edges. `callers` counts a call to the stub
   for one of the file's own functions, such as `log_msg@plt` in a shared
   library, as a call to the function, and says so.
7. **Read DWARF** when it is present (`gimli`), in `debuginfo`: each
   inlined-subroutine entry is a copy of one function in another, with the
   address ranges of its code. `inlined` adds them up. Copies whose code the
   linker dropped still have entries, so elfq skips those that don't start
   inside an item. Nothing above depends on DWARF; source locations could
   make output easier to follow later.

## Life of a command

These are the functions that run for `elfq app.elf retained process_frame`,
in order. Its output is the first example under [Commands](#commands).

1. **Arguments.** `main`, in main.rs, runs `run` on a thread with a large
   stack, since elfq builds its trees recursively. `run` reads
   `elfq help top` as `elfq top --help`, then clap parses the arguments
   into a `cli::Cli`: the file, `--json`, `--range`, and here
   `Command::Retained` with its SYMBOL.
2. **Dispatch.** `commands::run` checks that the arguments go together
   (`check_arguments`), before it reads the file, then loads the file,
   prints any warnings about it, and calls `retained::run`.
3. **Loading.** `binary::Binary::load` reads the file with the `object`
   crate: its sections, the dynamic relocations, which name the imports and
   the pointers the loader stores, and in Thumb files the vector table
   (`vector_table::find`). Then it finds the items: `symbols::items`,
   `plt::stubs`, the startup arrays, `eh_frame::read`, the entry point, the
   vector table, in stripped firmware `unnamed_functions::add`, and last
   `unnamed_data::items`, which has `arch::scan` read the code for the
   addresses of data that no symbol names.
4. **SYMBOL.** `retained::run` calls `SymbolInTree::find`, which checks
   that elfq can read the file's code (`check_code_is_readable`) and turns
   `process_frame` into an item with `symbol::find`: by address, by full
   name, or by the end of a name. `readable_item` refuses an import, and a
   function whose end elfq doesn't know.
5. **The graph.** `RetainedTree::build` has `graph::Graph::build` run
   `arch::scan` on each function's code and turn what it finds into
   references, or into unresolved calls. `Graph::build` then adds the
   pointers stored in data, from the dynamic relocations or, in a file that
   runs at a fixed address, from the words that hold an item's address, and
   finds the roots with `roots::find`.
6. **The dominator tree.** `graph::DominatorTree::build` has `petgraph`
   find each node's closest dominator, starting from one node above every
   root, and adds up the retained sizes from the bottom of the tree up.
   `RetainedTree` also notes which functions something calls directly, for
   the "(through a pointer)" mark.
7. **The answer.** `retained::run` writes the lead line, then
   `RetainedTree::largest` picks the 30 largest items under
   `process_frame`, and the items between them and it, and
   `RetainedTree::tree_row` makes a `render::TreeRow` of each.
   `render::Table` draws them as a tree in its last column (`Table::tree`)
   and writes the table (`Table::write`). Then `write_shared` names what
   `process_frame` uses that other code uses too, and `write_indirect_note`
   notes any calls through pointers in `process_frame` or the items it
   keeps. With `--json`, `retained::write_json` writes the whole tree
   instead, through `json::write`.
8. **Exit.** `run`, in main.rs, flushes the output and turns the answer into
   the exit status: 0 for an answer, 1 when `callers` or `callees` finds
   something that crosses its ranges, or may, and 2 for an error, which it
   prints after `error:`.

Each module uses only the ones below it. Beside each are the steps above
that run in it:

```
main.rs      1 main, run                                  8 exit status
commands/    2 commands::run   4 retained::run, symbol::find
             7 RetainedTree::largest, tree_row, write_shared
cli.rs       1 Cli, Command
render/      7 Table::tree, Table::write
debuginfo/   (only for inlined)
graph/       5 Graph::build, roots::find   6 DominatorTree::build
binary/      3 Binary::load, unnamed_data::items
arch/        3, 5 arch::scan
```

[CONTRIBUTING.md](../CONTRIBUTING.md#where-things-live) says what each
file holds, the rules that keep these layers apart, how elfq is tested, and
how to add a command or an instruction set.

## Dependencies

Pure Rust only, so building elfq compiles no C code. Ten crates:
`object` (ELF), `gimli` (DWARF), `iced-x86` (x86-64 decoding), `petgraph`
(the dominator tree), `clap` (arguments), `rustc-demangle` (Rust names),
`cpp_demangle` (C++ names), `serde` and `serde_json` (`--json`) and
`anyhow` (errors). Thumb has its own small decoder in
`arch/thumb.rs`: the code scan needs about a dozen instructions, which are
easier to read in one file, with the samples' disassembly as the reference,
than through a general disassembler's API.

## Status and roadmap

The overview, `top`, `retained`, `why`, `callers` and `callees` work on
x86-64 and Cortex-M executables, with or without symbols and DWARF, and
`inlined` works on any file with DWARF. The overview, `callers` and
`callees` also work on x86-64 shared libraries. The pull requests and the
git log record how elfq got here.

Next:

1. **Long calls**: calls that load the target's address into a register
   and branch through it, as in `ldr r3, =f; bx r3`, and the veneers that
   work that way, so that the range checks see them (see
   [Known limitations](#known-limitations), 3).
2. **Output fixes** that a review of elfq for scripts and AI agents turned
   up.

Ideas for later, not yet planned: what one function can reach in each
memory block, a check of what startup code copies into RAM, the retained
size of a set of items, comparing two builds, the functions that shared
libraries export as roots, `--root SYMBOL`, worst-case stack depth (the call
graph plus each function's stack frame), and a query language.

## Non-goals

- **Re-implementing readelf, objdump, nm or bloaty.** elfq does not print
  headers, section tables, disassembly listings or flat size breakdowns. When
  one of those tools already answers a question well, the docs point to it.
- **A reverse-engineering framework.** No decompiler, no interactive session.
- **Runtime profiling.** elfq reads the file; it never runs the program.

## Known limitations

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
   reads a table of them as CMSIS does, adds rows to `callers`. Long
   calls, next on the roadmap, cover the veneers.
4. **Shared libraries.** The functions that a library exports aren't roots
   yet, so `top`, `retained` and `why` refuse libraries, and also programs
   linked with `-static-pie`, which elfq takes for libraries.
5. **Compressed DWARF.** `inlined` can't read debug info that the linker
   compressed (`--compress-debug-sections`).
