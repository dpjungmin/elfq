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
where elfq found it. When hand-written code gives a function several entry
points, each with a symbol that runs to the end they share, as libgcc's
`__aeabi_dsub` runs on into `__adddf3`, each entry ends where the next
starts, so that their bytes count once, and runs on into it as a tail
call. (`binary::Item`)

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
| `call` | `call` | a direct call, or a call through a register that the code loaded with the target's address, as a long call does |
| `tail call` | `tail call` | a jump into another function, which then returns to the caller's caller |
| `uses` | `access` or `address taken` | code reads or writes data, or loads its address |
| `address taken` | `address taken` | code loads the address of a function or an import, as when passing a function pointer |
| `pointer` | `pointer` | data holds a pointer to it, as a table of functions does |

`callers` and `callees` give each way that one item uses another a row of
its own. `why` joins them with commas, as in `call, address taken`, and the
first row of each chain is a root, whose HOW says why the program starts
there instead, such as `entry point` or `vector table`.

**Root.** A place where the program starts running, such as the entry
point, a function that `.init_array` lists, or on Cortex-M a handler that
the vector table lists. Step 5 of [How it works](#how-it-works) lists every
kind. An item that no chain of references from a root reaches is
**unreachable**: dead
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
the linker's `--gc-sections` follows. Rows mark such a function "(through
a pointer)", since nothing calls it directly.

**Unresolved call.** A call or jump that elfq finds in code but can't turn
into a reference: one through a pointer set at run time, as in a call
through a function pointer or a C++ virtual call, or one to an address
where elfq knows no item, such as code in ROM. Where they could change an
answer, elfq says so. (`graph::Unresolved`)

**Estimated size.** When the file doesn't record an item's size, elfq
estimates it, mostly as the distance to the next item, and marks it `~`,
as in `~12`, or `size_estimated` in JSON. A function with nothing after it
in its section can't be estimated, unless elfq found it by following the
code of stripped firmware: then it runs to the section's end. Otherwise
its size shows as `?`, and elfq can't read its code.
(`binary::Item::size_estimated`)

**Range and memory block.** A range is the addresses from START up to END,
written `START..END` or `START+SIZE`. With `--range`, the overview, `top`
and `retained` count only the bytes inside one range, and `callers` and
`callees` take ranges in place of SYMBOL. A memory block is a run of
sections and load images less than 64 KiB apart, which stands for one
memory, such as flash or a RAM; the overview lists the blocks when there
are several. (`binary::AddressRange`, `binary::MemoryBlock`)

**Load image.** The bytes of a section that the file stores in one place,
such as flash, for the program to copy to where it uses them, such as RAM:
firmware stores the starting values of `.data` this way, and code that
runs from RAM or ITCM, and its startup code copies them. The program
headers say where each load image is. The overview's memory blocks list
them, as in `load image of .data`, and `--range` counts them where they
are stored. (`binary::Section::load_address`)

**Boundary and crossing.** For `callers` and `callees` with ranges, an item
is inside when any of its bytes is in one of the ranges, and a reference
**crosses** the boundary when one end is inside and the other isn't. The
help calls the boundary the ranges' edge. (`graph::Boundary`,
`graph::Crossing`)

**Thumb.** The instruction set of Cortex-M processors, with 16- and 32-bit
instructions. Arm sets bit 0 of a Thumb function's address wherever the
address is stored: in symbols, the entry point, pointers and the vector
table. elfq clears it, so the addresses it prints for functions are even.
elfq reads
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
target. elfq knows the veneers that GNU ld and LLD add for Cortex-M.
(`arch::is_veneer`)

**Long call.** A call that puts the target's address in a register and
branches through it, as `ldr r3, =f; blx r3`, so that it can reach any
address. Compilers make one for a function declared `long_call`, and most
veneers do the same. elfq follows the address to the branch and
lists a call to `f`. (`arch::thumb::values`)

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
| `elfq FILE retained SYMBOL` | What does SYMBOL keep alive, and how many bytes is that? |
| `elfq FILE why SYMBOL` | Which chains of calls and references from the roots keep SYMBOL in the binary? |
| `elfq FILE top` | Which items keep the most bytes? |
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
edge of the ranges, one row for each pair of items and way of use, the
largest item outside first. Each row names the item outside, then the one
inside, as `memcpy, used by apply_filter` for `callees` or `process_frame,
uses crc32` for `callers`, so that it stands alone when `grep` picks it
out. A call that goes through a linker's veneer is listed as a call to the
veneer's target, at the call to the veneer, which is in code someone
wrote, with "via veneer" after how it calls. elfq knows the veneers that
GNU ld and LLD add for Cortex-M
(`arch::is_veneer`). elfq's exit status is then 1 when anything crosses,
or might: through a call elfq can't follow, through a pointer to a
function in the ranges, or in a function in them whose end elfq doesn't
know. A CI job can then check
that code in ITCM stays there, or that code run from RAM never touches
flash. As with `diff`, the exit status is 0 otherwise, and 2 for any
error, with or without ranges. When the reader stops early, as `head`
does, elfq still finishes and exits with the answer's status.

All commands print a table for people by default, with one row per line. A
table puts its short cells, such as sizes, HOW and AT, first and the name
last, so that a long name that the terminal wraps moves no column. Where an
answer is a tree, as in the overview, `top`, `retained`, `why` and `inlined
SYMBOL`, each row then names the row it hangs under by a short name, as in
`crc_table, kept by crc32`, so that a row that grep finds still says where
it is; in `why`, only where chains branch. `--tree` draws such answers as
trees instead. AT shows where an item is used: every site when there are
one or two, else the first and how many more. `callers SYMBOL` and `callees
SYMBOL` list the 30 items with the largest own sizes, callers in order of
name and callees in the order SYMBOL first uses them, and a last row such
as `70 more` counts the rest; with ranges, they list every crossing.
`--all` lifts every such cut in an answer but AT's: it shows every item in
a table or tree, every chain in `why`, and every name and address in a
sentence, where a long list otherwise ends with "and 48 more".

For example:

```
$ elfq testdata/bin/c-x86_64.elf retained process_frame
Removing process_frame would save 2068 bytes:

RETAINED   OWN  ITEM
    2068   532  process_frame
    1083    59  crc32, kept by process_frame
    1024  1024  crc_table, kept by crc32
     378   271  apply_filter, kept by process_frame
     107    75  filter_tap, kept by apply_filter
      32    32  fir_coeffs, kept by filter_tap
      54    54  decode_header, kept by process_frame
      12   ~12  .rodata@0x2012, kept by process_frame
       9    ~9  .rodata@0x201e, kept by process_frame

Shared with other code, so not counted: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
```

Each row names the item that keeps it by the shortest form of its name that
finds that item as SYMBOL, or by its whole name when only an address would.
A tree shows its 30 largest items and sums up the rest
under the items that keep them: `6 more, kept by main` stands for six
items, and its RETAINED column adds up their bytes. A row never stands for
a single item, since the item's own row takes no more room, and lists of
names and addresses never end in "and 1 more" either. `--all` shows every
item instead.

With `--tree`, every tree, in every command, is drawn at most 9 levels
below its top row, so names never start more than 27 columns into the tree
and long names don't push it off the screen. Where a tree goes deeper, the
row 8 levels down ends with `(continued below)`, and the rows under it come
after the rest of the table, under a row `NAME (continued)`, by the same
rule. Rows with nothing under them stay in place, 9 levels down.
`retained chain1 --tree` on `testdata/bin/c-deep.elf`, a chain of calls 20
deep, shows it.

`top` shows the same kind of tree for the whole file, starting from the
roots, and the overview starts with its five largest items. On the Rust
x86-64 sample (`rust-x86_64-nodebug.elf`) it shows that the standard
library's backtrace printer keeps 228,391 of the file's 282,384 bytes of
code and data.

`why` answers from the other end: each item that uses SYMBOL keeps it in the
file, and the rows show the shortest chain to each of them from where the
program starts, merged into a tree. Each chain starts at a row whose HOW
says why the program starts there, such as `entry point`. Below that, a row
names its user only where chains branch, since HOW says how the row above
uses it. Users that nothing reaches, or that are there only because of
SYMBOL, don't keep it and are left out of the chains:

```
$ elfq testdata/bin/c-x86_64.elf why log_msg
log_msg is used by 4 functions, through these chains:

HOW            ITEM
entry point    _start
address taken  main
call           app_tick
call           log_msg
address taken  on_button, used by main
tail call      log_msg
call           process_frame, used by main
call           log_msg
call           report, used by main
call           log_msg

debug_dump uses log_msg too, but nothing reaches debug_dump.

Removing main would remove log_msg too.
```

`inlined` ranks the functions that the compiler copied into their callers
by the bytes all their copies take, which ddbug shows only one function at
a time. Each copy's bytes leave out the copies inlined into it, so the rows
add up to the file's inlined code. On the Rust x86-64 sample with DWARF,
`EndianSlice::read_slice` tops the list: 302 copies in 6,388 bytes. That
file is too large to commit; `testdata/build.sh` leaves it at
`testdata/rust/target/release/elfq-sample`. With SYMBOL it shows the copies
in one function, merging the copies of one function in one place:

```
$ elfq testdata/bin/c-x86_64.elf inlined report
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
```

A row under another names the copy it was inlined into by the shortest name
that no other function there has. Where that function is inlined in several
places, the name adds the nearest place above it that is inlined in one
place only, or SYMBOL: `inlined process_frame` on
`testdata/bin/rust-cortex-m4.elf` has rows that end in
`inlined into fetch_add in update_stats`. Where that still leaves two
copies with rows under them called the same, as when `spec_next` is
inlined into both `next` and `step` in `padding`, each adds what the rows
call the copy it sits in instead: `spec_next in next in padding`. So no
two rows that others hang under are called the same, unless the debug info
nests copies of a function in copies of it more than 16 deep.

With `--range START..END` or `--range START+SIZE`, the overview, `top` and
`retained` count only the bytes inside one range of memory, such as a
microcontroller's ITCM or DTCM. An item outside the range still shows when
it alone keeps bytes inside it, with none of its own: on the Cortex-M7
sample, which follows an i.MX RT1176's memory map, `process_frame` in flash
keeps 200 of ITCM's 224 bytes. A range counts the load images in it too,
where the file stores them, and the row of an item whose bytes it counts
there says "(load image)": in flash, `process_frame` keeps 1712 bytes, not
488, since flash also holds the code it alone uses in ITCM and the data it
alone uses in DTCM, for the startup code to copy. A range that holds both
copies of an item counts both, and its row says "(with its load image)".
The range forms of `callers` and `callees` look only where the program
uses its code and data. When the sections sit in several blocks of
addresses, as firmware's flash and RAMs do, the overview lists the blocks,
and a `--range` command to try for each block that elfq counts bytes in, so
the range needn't come from the chip's reference manual. Sizes can take a
unit, in any case: `0x20000000+256K`. Units are powers of 1024, as memory
sizes usually are.

### JSON

`--json` prints the same answer for scripts. The JSON has every item and
every site, where the text shows only the largest items and the first
sites; every key is always there, `null` when it has no value; addresses
are hex strings, as elfq prints them and SYMBOL takes them, and sizes are
numbers. Each row of a list in the answer, such as an item of `top` or a
chain of `why`, is a line of its own, so that `grep` and `head` work on
the JSON as on the text. An item looks like this, spread over lines here:

```json
{
  "name": "process_frame",
  "kind": "function",
  "section": ".text",
  "address": "0x1700",
  "size": 532,
  "size_estimated": false
}
```

`kind` is `function`, `data` or `import`, and `section` names the section
that holds the item, such as `.data` or `.bss`; an import has no section,
address or size in the file. `top` and `retained` list the items of their
trees as rows, in the order of the text's rows. Each also has an `id`,
which counts the rows from 0; `own`, its own bytes as in the OWN column, or
with `--range` those in the range, and 0 where OWN shows `?`;
`load_image`, how many of `own` are its load image's, for a row marked
"(load image)" or "(with its load image)"; its `retained` size;
`through_pointer`, for a function marked "(through a pointer)"; and
`parent`, the `id` of the row it hangs under, or `null` at the top, which
in `retained` holds what SYMBOL keeps itself. So a script that adds up
`retained` takes only the rows whose `parent` is `null`. The answer of
`retained` also gives SYMBOL's own `own` and `load_image`, and `top`'s
`unreachable` items have them too; `top`'s `bytes` is the sum of every
`own`. The first rows of `retained process_frame`, shown above:

```json
{"id":0,"name":"crc32","kind":"function","section":".text","address":"0x1280","size":59,"size_estimated":false,"own":59,"load_image":0,"retained":1083,"through_pointer":false,"parent":null},
{"id":1,"name":"crc_table","kind":"data","section":".rodata","address":"0x2120","size":1024,"size_estimated":false,"own":1024,"load_image":0,"retained":1024,"through_pointer":false,"parent":0},
```

`inlined SYMBOL` lists the rows of its tree the same way, each the copies
of one function in one place, with `id`, `name`, `bytes`, `copies` and
`parent`, the `id` of the row of the copies they were inlined into, or
`null` for those inlined into SYMBOL's function itself.

`why` gives each chain as a list of steps, each an item and a `how`. The
first step is a root, and its `how` says why the program starts there,
such as `entry point` or `listed in .init_array`. In the others, it says
how the item before uses this one, in the words of the JSON column in
[Concepts](#concepts), joined with commas when there are several. The
items that use SYMBOL but don't keep it, which the text names below the
chains, are in `unreachable_users` when nothing reaches them, and in
`users_it_keeps` when only SYMBOL leads to them. The overview's JSON says
what the file is and what elfq found in it; the largest items are in
`top`'s.

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

elfq reads the file, then answers one question. `binary` reads the file,
finds its items and has `arch` read each function's code once; `graph`
connects the items by the references found there, and works out the roots,
the dominator tree and the shortest chains; and the command picks what to
show and writes it, as a table that `render` lays out, or as JSON.
`debuginfo` reads DWARF beside the graph, for `inlined`. [Life of a
command](#life-of-a-command) follows one command through the functions
that do this.

1. **Load** the file with the `object` crate: sections, with where the
   program headers store their load images, symbols, entry point, dynamic
   relocations.
2. **Discover items**, each with an address, size and a record of how it
   was found. Each source adds only what the ones before it missed:
   1. The symbol tables, `.symtab` and `.dynsym`: one item for each kind
      and address that has function or data symbols in an allocated
      section, named by the symbol with the strongest binding, with the
      others as aliases. Data symbols of size 0, which mark positions such
      as `_edata`, are left out, and so are those that only enclose other
      data symbols, such as `.L_MergedGlobals`.
   2. On x86-64, PLT stubs, named after the import they jump to
      (`write@plt`) or the file's own function (`log_msg@plt`), and the
      header before a table's first stub, named after its section.
   3. The startup and exit arrays, `.preinit_array`, `.init_array`,
      `.fini_array`, `.ctors` and `.dtors`, named after their section.
   4. `.eh_frame` entries, which on x86-64 cover almost every function,
      even in stripped files. One that starts where a function does gives
      it a size if it has none; one that starts inside a function, at 0 or
      is empty adds nothing; any other in code is a new function.
   5. The entry point, as a function named `entry`, when it is in code and
      no function covers it.
   6. The Cortex-M vector table, when no item covers any of it, named after
      its section, or after the section and address (`.text@0x8000000`)
      when it is only part of it.
   7. In Thumb executables where neither the symbol tables nor `.eh_frame`
      list a function, the functions that the code leads to from the entry
      point, the vector table's handlers and the startup arrays, and those
      whose Thumb addresses code loads or data holds, where the code before
      them ends (see `unnamed_functions.rs`).
   8. Data that code or a stored pointer refers to where there is no item,
      in a data section other than the global offset table or, in Thumb
      files, in code: vtables, string literals and, in stripped files,
      variables and the constants that some linker scripts put in `.text`.
      Such data starts at each address other than 0 that something refers
      to and runs to the next one, the next item or the end of its section;
      elfq never guesses which function it belongs to.

   Before 8, a function whose size runs past the start of another function
   in its section ends there when the functions that start inside it run
   to its end, as an entry point that shares its code with the next does;
   and each function still of unknown size runs to the next item in its
   section, as an estimate, while one with nothing after it there keeps an
   unknown size. Items without a symbol get names like `fn_0x1690`,
   `.rodata@0x2004` (section and address) or `entry`. Rust and C++ symbols
   are demangled, and the mangled symbol stays as an alias.
   `elfq FILE debug items` lists what was found, for checking this step.
3. **Scan code** with an architecture backend. For each function it decodes
   the instructions and reports direct calls, tail jumps to other items,
   indirect call sites (target unknown), and PC-relative references to items
   (`lea rip+...` on x86-64; `adr`, literal-pool `ldr` and `movw`/`movt` on
   Thumb). On x86-64 it needs only these instructions. On
   Thumb it also skips the literal pools, jump tables and constants that
   `adr` points at inside functions, which would otherwise decode as
   instructions. A Thumb branch through a register goes where the code put
   an address in the register, as for a long call: in each function that
   branches through a register, `arch/thumb/values/` follows what each
   register and each word of the function's stack may hold, along every
   path through its code, using what `decode.rs` says each instruction
   does. Each address the register may hold there is a call or a tail jump,
   except an even one for `bx` and `blx`, since Cortex-M cores fault on a
   branch to it, as to the address 0 of a weak function that the program
   doesn't have; `bxns` and `blxns` go to Non-secure code there, and `mov
   pc` ignores the low bit. After a call, r0 holds what the function
   returns, and r1 to r3 and ip may still hold what they held, as code
   built with GCC's `-fipa-ra` counts on, so a branch through one of them
   goes there or anywhere. An address that the code only branches to isn't
   also reported as used, unless it branches through r0 after a call: r0
   then holds what the call returned, but `-fipa-ra` code may have kept
   the address there, so it counts as taken. An entry point that step 2
   ended where the next function starts also tail-jumps to it from its
   last instruction, since its code runs on into it, unless that
   instruction always jumps, returns or traps. Code that only a jump elfq
   doesn't follow leads to, such as the cases of a switch that jumps
   through a table of addresses, keeps what reading each instruction on
   its own finds; when that code joins code elfq follows, the whole
   function does. elfq scans each function once, as it loads the file
   (`Binary::code_references`).
   The entry point's code is a special case: startup code copies or clears
   whole sections such as `.data` and `.bss`, loading where each starts and
   ends through symbols that the linker script defines, such as `_sdata`
   and `_edata`. When the entry point loads both ends of a section, those
   addresses aren't uses of the items that start there.
   LLVM may merge small variables into one block (`.L_MergedGlobals`),
   which code uses through the block's address; that address counts as a
   use of each variable in the block.
4. **Scan data** for references from data items to other items. When the file
   is position-independent (most Linux executables are), its dynamic
   relocations give every stored pointer exactly. Otherwise, as in firmware,
   elfq reads data items word by word and takes a word that equals the start
   of an item (plus 1 for a Thumb function) for a pointer to it. A number
   that happens to equal an address is mistaken for a pointer, which is rare
   because addresses are large numbers.
5. **Find roots**, where the program starts running. An item is a root
   when:
   1. it holds the ELF entry point;
   2. it is in `.init` or `.fini`, which the dynamic loader runs, in a
      startup or exit array, or in a note section, such as `__abi_tag`,
      which the system reads;
   3. a startup or exit array points to it, through the dynamic relocation
      stored there or, in a file linked to run at one address, by holding
      its address (plus 1 for a Thumb function);
   4. it is the PLT header, which the dynamic loader uses;
   5. it is a personality routine that `.eh_frame` names, which the
      unwinder calls, or holds a pointer to one that the table points to;
   6. it holds part of the Cortex-M vector table, or starts where a handler
      that the table lists does.

   Each root is listed once, for the first of these reasons. The vector
   table is found by its shape, not its section's name, as
   `binary/vector_table.rs` describes. Not yet: the functions that shared
   libraries export, and `--root SYMBOL`.
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
   into a `cli::Cli`: the file, `--json`, `--tree`, `--range`, and here
   `Command::Retained` with its SYMBOL.
2. **Dispatch.** `commands::run` checks that the arguments go together
   (`check_arguments`), before it reads the file, then loads the file,
   prints any warnings about it, and calls `retained::run`.
3. **Loading.** `binary::Binary::load` reads the file with the `object`
   crate: its sections, the dynamic relocations, which name the imports and
   the pointers the loader stores, and in Thumb files the vector table
   (`vector_table::find`). Then it finds the items: `symbols::items`,
   `plt::stubs`, the startup arrays, `eh_frame::read`, the entry point, the
   vector table, in stripped firmware `unnamed_functions::add`. Then it has
   `arch::scan` read each function's code once (`scan_functions`), and
   last `unnamed_data::items` finds there the addresses of data that no
   symbol names.
4. **SYMBOL.** `retained::run` calls `SymbolInTree::find`, which checks
   that elfq can read the file's code (`check_code_is_readable`) and turns
   `process_frame` into an item with `symbol::find`: by address, by full
   name, or by the end of a name. `readable_item` refuses an import, and a
   function whose end elfq doesn't know.
5. **The graph.** `RetainedTree::build` has `graph::Graph::build` turn
   what `arch::scan` found in each function's code into references, or
   into unresolved calls. `Graph::build` then adds the
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
   `RetainedTree::tree_row` makes a `render::TreeRow` of each, with the
   short name that the rows under it call it by (`Names::short_name`).
   `render::Table` puts them in its last column, as rows that each name the
   row they hang under or, with `--tree`, drawn as a tree (`Table::tree`),
   and writes the table (`Table::write`). Then `write_shared` names what
   `process_frame` uses that other code uses too, and `write_indirect_note`
   notes any calls through pointers in `process_frame` or the items it
   keeps. With `--json`, `retained::write_json` lists every row of the tree
   instead (`RetainedTree::to_json`), and writes them through `json::write`.
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
binary/      3 Binary::load, scan_functions, unnamed_data::items
arch/        3 arch::scan
```

[CONTRIBUTING.md](../CONTRIBUTING.md#where-things-live) says what each
file holds, the rules that keep these layers apart, how elfq is tested, and
how to add a command or an instruction set.

## Dependencies

Pure Rust only, so building elfq compiles no C code. Ten crates:
`object` (ELF), `gimli` (DWARF), `iced-x86` (x86-64 decoding), `petgraph`
(the dominator tree), `clap` (arguments), `rustc-demangle` (Rust names),
`cpp_demangle` (C++ names), `serde` and `serde_json` (`--json`) and
`anyhow` (errors). Thumb has its own decoder in `arch/thumb/`: elfq needs
only what each instruction does with the registers, the stack and where
the code goes next, which is easier to read in its own code, with the
samples' disassembly as the reference, than through a general
disassembler's API.

## Status and roadmap

The overview, `top`, `retained`, `why`, `callers` and `callees` work on
x86-64 and Cortex-M executables, with or without symbols and DWARF, and
`inlined` works on any file with DWARF that isn't compressed. The overview,
`callers` and
`callees` also work on x86-64 shared libraries. The pull requests and the
git log record how elfq got here.

Ideas for later, not yet planned: output that scripts and AI agents can
read more easily, what one function can reach in each memory block, a
check of what startup code copies into RAM, the retained size of a set of
items, comparing two builds, the functions that shared libraries export as
roots, `--root SYMBOL`, worst-case stack depth (the call graph plus each
function's stack frame), and a query language.

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
3. **Calls through a register that elfq doesn't follow.** A register that
   may hold more than 8 addresses at a branch, as when GCC merges many
   long calls into one `blx`, an address kept in memory other than the
   function's own stack, and one on a stack whose address the function
   passes to another, leave a call through a pointer; elfq lists each
   address as taken. Cortex-M0 code built with `-mpure-code` makes
   addresses a byte at a time, and elfq follows them only to a branch, so
   the variables such code uses are missing, and so are its long calls in
   a function that switches through a table of addresses, since such a
   function keeps the sweep's references. Code that a switch jumps to
   through a table of addresses, and that other code also branches to,
   gets what the registers hold only from those branches, so it misses
   what the switch put there. `callers SYMBOL` and `callees SYMBOL` name a
   veneer as the caller of its target, where the range forms name the
   function that calls the veneer. In a file stripped of local symbols
   only (`strip -x`), the veneers are in no item: `callees` with ranges
   lists the calls to them as calls elfq can't follow, and `callers` with
   ranges misses them. In a file without symbols, LLD's veneer that is a
   lone `b.w` looks like any tail call, so the range forms name it as the
   caller. Code built with GCC's `-fipa-ra` may keep an address in r0 to
   r3 across a call; when it then passes the address to a function or back
   to its caller without naming the register, or reads it from r0 other
   than to branch there, elfq doesn't list it as taken, since it takes r0
   to hold what the call returned. Calls through a register on x86-64
   aren't followed. A routine in ROM that only the linker script names
   shows as its address, since the symbol has no section. An assembly
   function whose symbol has no size, last in its section, counts as
   empty, so its calls are missing; the same file without symbols has
   them. Startup code that copies sections outside the entry function, or
   reads a table of them as CMSIS does, adds rows to `callers`.
4. **Shared libraries.** The functions that a library exports aren't roots
   yet, so `top`, `retained` and `why` refuse libraries, and also programs
   linked with `-static-pie`, which elfq takes for libraries. elfq finds
   the functions of stripped firmware by following its code only in files
   linked to run at one address, so in Arm firmware linked with `-pie` and
   stripped it finds only the entry point.
5. **Compressed DWARF.** `inlined` can't read debug info that the linker
   compressed (`--compress-debug-sections`).
