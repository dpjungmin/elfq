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

Every command has the form `elfq <command> <FILE> [ARGS]`. The first three are
the reason elfq exists; the rest support them.

| Command | Answers |
|---|---|
| `elfq retained FILE SYMBOL` | What does SYMBOL keep alive, as a tree, and how many bytes is that? |
| `elfq exclusive FILE SYMBOL` | Which functions and data are reachable only through SYMBOL? (flat list) |
| `elfq why FILE SYMBOL` | Which chains of calls and references from the roots keep SYMBOL in the binary? |
| `elfq top FILE` | Which items retain the most bytes? |
| `elfq callers FILE SYMBOL` | What calls or references SYMBOL directly, and how (call, tail call, address taken)? |
| `elfq callees FILE SYMBOL` | What does SYMBOL call or reference directly? |
| `elfq inlined FILE [--in SYMBOL]` | Which inlined functions cost the most bytes across the binary, or inside SYMBOL? Needs DWARF. |

All commands print a table or tree for people by default and take
`--format json` for scripts.

Illustrative output (format to be settled when the command is built). The
sizes are the real ones for `testdata/bin/c-x86_64.elf`:

```
$ elfq retained testdata/bin/c-x86_64.elf process_frame
retained  own   item
  2047    532   process_frame
  1083     59   ├─ crc32
  1024   1024   │  └─ crc_table (data)
   378    271   ├─ apply_filter
   107     75   │  └─ filter_tap
    32     32   │     └─ fir_coeffs (data)
    54     54   └─ decode_header

Shared with other code, not counted: log_msg, dispatch_event, __stack_chk_fail
```

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
items so the reader knows they were reached through a pointer.

## How it works

Data moves through five modules in order: `binary` finds the items, `arch`
finds the references between them, `graph` computes dominators and paths,
`commands` run queries, and `render` prints text or JSON.

1. **Load** the file with the `object` crate: sections, symbols, entry point,
   dynamic relocations.
2. **Discover items**, each with an address, size and a record of how it was
   found. Sources, best first: the symbol table; the dynamic symbol table;
   `.eh_frame` entries (on x86-64 they cover almost every function, even in
   stripped files); the entry point; targets of calls found during scanning.
   Items without a symbol get names like `fn_0x1690`, or `entry`.
   `elfq debug items FILE` lists what was found, for checking this step.
3. **Scan code** with an architecture backend. For each function it decodes
   the instructions and reports direct calls, tail jumps to other items,
   indirect call sites (target unknown), and PC-relative references to items
   (`lea rip+...` on x86-64; `adr`, literal-pool `ldr` and `movw`/`movt` on
   Thumb). It does not need a full disassembler, only these instructions.
4. **Scan data** for references from data items to other items. When the file
   has dynamic relocations (most Linux executables are position-independent)
   they give every stored pointer exactly. Otherwise, as in firmware, elfq
   scans aligned words for values equal to an item's address (with the Thumb
   bit cleared) and marks those edges as heuristic.
5. **Find roots**: the ELF entry point, `.preinit_array`, `.init_array` and
   `.fini_array` entries, exported symbols of shared libraries, and the
   Cortex-M vector table (recognized by section name or by its shape: a RAM
   address followed by the entry point). `--root SYMBOL` adds more.
   Non-allocated sections and unwind tables (`.eh_frame`, `.ARM.exidx`) are
   never sources of references; otherwise everything would look reachable.
6. **Analyze**: compute the dominator tree once from a virtual node that
   points at every root. `retained`, `exclusive` and `top` read it; `why` is a
   shortest-path search back to the roots; `callers` and `callees` read edges.
7. **Enrich with DWARF** when it is present (`gimli`): inlined-subroutine
   ranges attribute each function's bytes to the functions inlined into it,
   and source locations make output easier to follow. Nothing above depends on
   DWARF.

## Code layout

```
src/
├── main.rs        Parses arguments, runs one command, prints errors. Nothing else.
├── lib.rs         Declares the modules below. elfq is a library plus a thin binary.
├── cli.rs         clap definitions, one struct per subcommand.
├── commands/      One file per subcommand: find the symbol, run a query, render.
├── binary/        ELF loading and item discovery.
├── arch/          One file per architecture (x86_64.rs, arm.rs) behind one trait.
├── graph/         The reference graph, roots, dominators, paths.
├── debuginfo/     Optional DWARF enrichment.
└── render/        Table, tree and JSON output.
```

Rules that keep it approachable:

- **Analysis flows one way**: binary, then arch, then graph, each importing
  only from the ones before it. `render` knows nothing about ELF files, and
  `commands` connect the two.
- **Analysis never prints and commands never analyze.** A command is a short
  function that wires a query to a renderer.
- **Adding an architecture** means implementing one trait ("given a
  function's bytes and address, report its references") in a new file under
  `arch/`, plus sample binaries.
- **Adding a command** means one clap struct in `cli.rs`, one file in
  `commands/`, and tests that run it on `testdata/bin`.
- **Library code returns errors, it does not exit or panic** on bad input;
  `main.rs` adds context and prints them.

## Dependencies

Pure Rust only, so `cargo install elfq` needs no C toolchain:
`object` (ELF), `gimli` (DWARF), `iced-x86` (x86-64 decoding), `petgraph`
(graph and dominators), `clap` (arguments), and later `rustc-demangle` and
`cpp_demangle`. Thumb decoding is chosen in step 2 between `yaxpeax-arm` and a
small decoder for the handful of instructions the code scan needs.

## Testing

- **Unit tests** beside the code for algorithms, using hand-built graphs.
- **Integration tests** run each command on the committed files in
  `testdata/bin`. [testdata/README.md](../testdata/README.md) lists the
  expected answers, for example that `exclusive process_frame` on any C build
  is `decode_header`, `crc32`, `crc_table`, `apply_filter`, `filter_tap` and
  `fir_coeffs`, with or without DWARF.
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
| 2. Core engine | 2a: crate skeleton, CLI, CI, ELF loading and item discovery. 2b: x86-64 scanning, the reference graph, `callers` and `callees`. 2c: Cortex-M Thumb scanning and vector-table roots. |
| 3. Headline queries | 3a: dominators, `retained` and `exclusive`. 3b: `why`. 3c: `top`. 3d: `inlined`. |
| 4. Polish | Demangling, fuzzy symbol matching, JSON output, warnings where indirect calls make answers uncertain. |

Later: a query language, and worst-case stack depth (call graph plus each
function's stack frame).

## Open questions

1. **Names.** `why` or `paths`? `exclusive` or `only-from`?
2. **Thumb decoder.** An existing crate or our own small decoder (step 2c).
3. **Pointer-reached functions in `exclusive`.** Listed with a marker (the
   current plan) or in a separate section?
4. **Stripped Cortex-M files** have no `.eh_frame`, so item discovery relies
   on the vector table and call targets. How well that works is unknown until
   step 2c.
5. **Anonymous data** such as vtables and merged string literals has no
   symbol. elfq still has to make items for it, or references through it are
   lost: in the Rust sample, `app_main` reaches `<Gain as Stage>::run` only
   through an unnamed vtable. The plan is to name such items by section and
   address and never guess an owning function.
