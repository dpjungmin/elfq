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
`retained`, `exclusive` and `why` are the reason elfq exists; the rest
support them, except `inlined`, which reads DWARF instead of machine code.

| Command | Answers |
|---|---|
| `elfq FILE` | What is this file, what did elfq find in it, which items keep the most bytes, and what can I ask next? |
| `elfq FILE retained SYMBOL` | What does SYMBOL keep alive, as a tree, and how many bytes is that? |
| `elfq FILE exclusive SYMBOL` | Which functions and data are reachable only through SYMBOL? (flat list) |
| `elfq FILE why SYMBOL` | Which chains of calls and references from the roots keep SYMBOL in the binary? |
| `elfq FILE top` | Which items keep the most bytes, as a tree? |
| `elfq FILE callers SYMBOL` | What calls or uses SYMBOL directly, and how (call, tail call, address taken, access, pointer in data)? |
| `elfq FILE callees SYMBOL` | What does SYMBOL call or use directly? |
| `elfq FILE inlined [--in SYMBOL]` | Which inlined functions cost the most bytes across the binary, or inside SYMBOL? Needs DWARF. |

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
`elfq callers app.elf log_msg`, gets a hint that the file comes first.

All commands print a table or tree for people by default. `--format json`
prints the same answer for scripts. The JSON has every item, where the text
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
list of steps, each an item and how the one before uses it. The overview's
JSON says what the file is and what elfq found in it; the largest items are
in `top`'s.

For example:

```
$ elfq testdata/bin/c-x86_64.elf retained process_frame
process_frame keeps 2068 bytes in the file: its own 532, and 1536 in 8 items that only it leads to.

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

Also uses, but shares with other code: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
```

A tree shows its 30 largest items and sums up the rest under their parents
(`└─ 6 more`); `exclusive` lists every item, largest first.

`top` shows the same kind of tree for the whole file, starting from the
roots, and the overview starts with its five largest items. On the Rust
x86-64 sample it shows that the standard library's backtrace printer keeps
228,391 of the file's 282,384 bytes.

`why` answers from the other end: each item that uses SYMBOL keeps it in the
file, and the tree shows the shortest chain to each of them from where the
program starts. Users that nothing reaches, or that are there only because
of SYMBOL, don't keep it and are left out of the tree:

```
$ elfq testdata/bin/c-x86_64.elf why log_msg
log_msg is in the file because 4 functions use it. The shortest chain through each, from where the program starts:

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

Every chain to log_msg goes through main, so it counts toward main's retained size.
```

`inlined` ranks the functions that the compiler copied into their callers by
the bytes all their copies take, which ddbug shows only one function at a
time. Each copy's bytes leave out the copies inlined into it, so the rows add
up to the file's inlined code. On the Rust x86-64 sample, built with DWARF,
`EndianSlice::read_slice` tops the list: 302 copies in 6,388 bytes. With
`--in SYMBOL` it shows the copies in one function as a tree, merging the
copies of one function in one place:

```
$ elfq testdata/bin/c-x86_64.elf inlined --in report
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
```

With `--range START..END` or `--range START+SIZE`, the overview and `top`
count only the bytes inside one range of memory, such as a microcontroller's
ITCM or DTCM. An item outside the range still shows when it alone keeps
bytes inside it, with none of its own: on the Cortex-M7 sample, which
follows an i.MX RT1176's memory map, `process_frame` in flash keeps 200 of
ITCM's 224 bytes. When the sections sit in several blocks of addresses, as
firmware's flash and RAMs do, the overview lists the blocks, each with a
`--range` command to try, so the range needn't come from the chip's
reference manual. Sizes can take a unit, in any case: `0x20000000+256K`.
Units are powers of 1024, as memory sizes usually are.

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
and jumps it can't follow. `retained` and `exclusive` note when SYMBOL's code
calls through pointers, since what those calls reach may count for other
items. In JSON, `callers` and `callees` have `complete`, and `retained` and
`exclusive` have `indirect_calls`.

## How it works

Data moves through five modules in order: `arch` finds the addresses that
machine code uses, `binary` finds the items, `graph` connects them by those
references and computes dominators and paths, `commands` run queries, and
`render` prints text or JSON. `debuginfo` reads DWARF beside the graph, for
`inlined`.

1. **Load** the file with the `object` crate: sections, symbols, entry point,
   dynamic relocations.
2. **Discover items**, each with an address, size and a record of how it was
   found. Sources, best first: the symbol table; the dynamic symbol table;
   PLT stubs, named after the import they jump to (`write@plt`), and
   arrays of startup functions such as `.init_array`, named after their
   section; `.eh_frame` entries (on x86-64 they cover almost every
   function, even in stripped files); the entry point; targets of calls
   found during scanning; and data that code or a stored pointer refers to
   but no symbol names, such as vtables, string literals and, in stripped
   files, variables. Such data starts at each address something refers to
   and runs to the next one, the next item or the end of its section; elfq
   never guesses which function it belongs to. Items without a symbol get
   names like `fn_0x1690`, `.rodata@0x2004` (section and address) or
   `entry`. Rust and C++ symbols are demangled, and the mangled symbol
   stays as an alias. `elfq FILE debug items` lists what was found, for checking this
   step.
3. **Scan code** with an architecture backend. For each function it decodes
   the instructions and reports direct calls, tail jumps to other items,
   indirect call sites (target unknown), and PC-relative references to items
   (`lea rip+...` on x86-64; `adr`, literal-pool `ldr` and `movw`/`movt` on
   Thumb). It does not need a full disassembler, only these instructions.
   On Thumb it also skips the literal pools and jump tables inside
   functions, which would otherwise decode as instructions. The entry
   point's code is a special case: startup code copies or clears whole
   sections such as `.data` and `.bss`, loading where each starts and ends
   through symbols that the linker script defines, such as `_sdata` and
   `_edata`. When the entry point loads both ends of a section, those
   addresses aren't uses of the items that start there.
4. **Scan data** for references from data items to other items. When the file
   is position-independent (most Linux executables are), its dynamic
   relocations give every stored pointer exactly. Otherwise, as in firmware,
   elfq reads data items word by word and takes a word that equals the start
   of an item (plus 1 for a Thumb function) for a pointer to it. A number
   that happens to equal an address is mistaken for a pointer, which is rare
   because addresses are large numbers.
5. **Find roots**, where the program starts running: the ELF entry point;
   the functions in `.preinit_array`, `.init_array` and `.fini_array`, and
   the code in `.init` and `.fini`; the PLT header, which the dynamic
   loader uses; personality routines that `.eh_frame` names, which the
   unwinder calls; notes such as `__abi_tag`; and the Cortex-M vector table,
   recognized by its shape rather than its section name: a word holding the
   entry point's Thumb address, then handlers until an entry is neither 0
   nor a handler. Not yet: exported symbols of shared libraries, which need
   a shared-library sample, and `--root SYMBOL`. Non-allocated sections and
   unwind tables (`.eh_frame`, `.ARM.exidx`) are never sources of
   references; otherwise everything would look reachable. One more source
   is only on Arm: LLVM merges small variables into one block
   (`.L_MergedGlobals`) and code uses them through the block's address, so
   that address counts as a use of each variable in the block.
6. **Analyze**: compute the dominator tree once from a virtual node that
   points at every root. `retained`, `exclusive` and `top` read it. `why`
   searches from the roots for the shortest chain to each item that uses
   SYMBOL, going around SYMBOL itself, and prefers calls among equally short
   chains. `callers` and `callees` read edges.
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
├── commands/      One file per subcommand: find the symbol (symbol.rs), run a query, render (tree.rs draws retained sizes, json.rs has shared JSON shapes).
├── binary/        ELF loading, item discovery, names and address ranges.
├── arch/          One file per instruction set (x86_64.rs, thumb.rs), each with one `scan` function.
├── graph/         The reference graph, its roots, dominator tree and shortest paths.
├── debuginfo/     DWARF, when the file has it: what the compiler inlined.
└── render/        Table, tree and JSON output.
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

Pure Rust only, so `cargo install elfq` needs no C toolchain:
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
  expected answers, for example that `exclusive process_frame` on any C build
  is `decode_header`, `crc32`, `crc_table`, `apply_filter`, `filter_tap`,
  `fir_coeffs` and two message strings, with or without DWARF.
- **Snapshot tests** pin the human-readable output so format changes are
  reviewed on purpose.
- **CI** runs `cargo fmt --check`, `cargo clippy -- -D warnings` and
  `cargo test`. The sample binaries are committed, so CI needs no cross
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
| 5. Memory ranges | 5a: a Cortex-M7 sample with code and data in ITCM, DTCM, OCRAM and flash, at the addresses an i.MX RT1176 uses; startup code no longer seems to use the first item of each section it copies. 5b: `--range START..END` or `--range START+SIZE` for the overview and `top`, and the overview lists the address blocks that the file's sections occupy. 5c: `--range` for `retained`. |

Later: a query language, and worst-case stack depth (call graph plus each
function's stack frame).

## Open questions

1. **Names.** `exclusive` or `only-from`?
2. **Pointer-reached functions in `exclusive`.** Listed with a marker (the
   current plan) or in a separate section?
3. **Stripped Cortex-M files** have no symbols and no `.eh_frame`, so elfq
   finds only the entry point in them today. Finding functions would mean
   following calls from the entry point and the vector table.
4. **Where unnamed data ends.** Unnamed data starts wherever something
   refers to it, so a struct in a stripped file whose fields code uses one
   at a time becomes one item per field, and the end address of an array,
   which loops compare against, can start an item of its own. The bytes
   still all count, split across more items than the source has. Worth
   fixing only if it makes real answers confusing.
