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
column of `callers`, `callees`, `why` and `reach` says what kind of
reference each is (`graph::ReferenceKind`):

| HOW | JSON `how` | Meaning |
|---|---|---|
| `call` | `call` | a direct call, or a call through a register that the code loaded with the target's address, as a long call does |
| `tail call` | `tail call` | a jump into another function, which then returns to the caller's caller |
| `uses` | `access` or `address taken` | code reads or writes data, or loads its address |
| `address taken` | `address taken` | code loads the address of a function or an import, as when passing a function pointer |
| `pointer` | `pointer` | data holds a pointer to it, as a table of functions does |

elfq tells `access` from `address taken` only in x86-64 code: Thumb code
loads an address before it reads or writes data there, so in Thumb code,
JSON says `address taken`. `callers` and `callees` give each way that one
item uses another a row of its own. `why` joins them with commas, as in
`call, address taken`, and the first row of each chain is a root, whose HOW
says why the program starts there instead, such as `entry point` or `vector
table`.

**Root.** A place where the program starts running, such as the entry
point, a function that `.init_array` lists, or on Cortex-M a handler that
the vector table lists. Step 5 of [How it works](#how-it-works) lists every
kind. An item that no chain of references from a root reaches is
**unreachable**: the program never uses it, or uses it in a way elfq
can't see. The functions that a shared library exports aren't roots yet,
so `top`, `retained` and `why` don't answer for libraries.
(`graph::Root`)

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

**Reach.** What an item **reaches** is everything it calls or uses, what
those call or use, and so on: every item that a chain of references from it
leads to. Where its retained size counts only what no other code uses, what
it reaches counts everything that it needs to run, shared or not, and
doesn't depend on the roots. `reach` keeps the shortest chain to each item,
so that the items form a tree under SYMBOL, each under the item that uses
it on its chain. (`graph::ShortestPaths::from`)

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
and `retained` count only the bytes inside one range, `callers` and
`callees` take ranges in place of SYMBOL and after `--from` and `--to`, and
`reach` takes them after `--to`. A memory block is a run of sections and load images less than 64 KiB apart,
which stands for one memory, such as flash or a RAM; the overview lists the
blocks when there are several, and `reach` adds up what SYMBOL reaches in
each. (`binary::AddressRange`, `binary::MemoryBlock`)

**Load image.** The bytes of a section that the file stores in one place,
such as flash, for the program to copy to where it uses them, such as RAM:
firmware stores the starting values of `.data` this way, and code that
runs from RAM or ITCM, and its startup code copies them. The program
headers say where each load image is. The overview's memory blocks list
them, as in `load image of .data`, and `--range` counts them where they
are stored. (`binary::Section::load_address`)

**Boundary and crossing.** For `callers` and `callees` with ranges, `--from`
or `--to`, and for `reach --to`, an item is inside when any of its bytes is
in one of the ranges, where an item whose size elfq doesn't know counts as
its first byte, and a reference **crosses** the boundary when one end is
inside and the other isn't. The help calls the boundary the ranges' edge.
(`graph::Boundary`, `graph::Crossing`)

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
`callers` and `callees`, and the rows of `reach`, list a call through a
veneer as a call to its target, "via veneer", and a call through a PLT stub to one of the file's
own functions, as a shared library makes, the same way, "via PLT";
elsewhere a veneer is a function that tail-calls its target. elfq knows the
veneers that GNU ld and LLD add for Cortex-M. (`arch::is_veneer`,
`graph::Veneers`)

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
| `elfq FILE reach SYMBOL` | What does SYMBOL call or use at any depth, and how many bytes of code and data does that take in each block of memory, such as ITCM, DTCM and flash? |
| `elfq FILE reach` | Where does the program start, and how many bytes of code and data does each start reach in each block of memory? |
| `elfq FILE reach SYMBOL --to RANGE` | What does SYMBOL reach in one range of memory, such as flash, and which code uses each item there? |
| `elfq FILE callees RANGE...` | What do the code and data in these ranges of memory use outside them, such as code in ITCM that calls a function in flash? `callers RANGE...` asks the reverse. |
| `elfq FILE callers RANGE... --from RANGE` | What in one range of memory uses these ranges, such as code in ITCM that uses data in DTCM? `callees RANGE... --to RANGE` asks what these ranges use in that one, and either takes SYMBOL in place of the ranges. |
| `elfq FILE inlined [SYMBOL]` | Which inlined functions cost the most bytes across the binary, or inside SYMBOL? Needs DWARF. |
| `elfq FILE browse [SYMBOL]` | What does SYMBOL's code do, line by line, where does each call and use lead, and what calls it? Opens in the terminal, follows the links that the other commands' answers name, and shows the answers of `callees`, `callers`, `reach`, `why` and `retained SYMBOL` in tabs, each with the command that prints it, and the retained bytes in each block of memory; a page's header says what `retained` and `why` say about the item, and `u` opens the item that keeps it; `/` finds text in a page's rows. A data object's page lists its contents as far as elfq reads them: a vector table's entries, named by exception, the pointers it holds, as `callees` lists them, with what the dynamic loader writes in a shared library, or the strings of an object that holds only strings, with the `objdump -s` command that prints its bytes. Without SYMBOL, it opens on where the program starts, as `reach` lists it, each start a link to its page. |
| `elfq FILE diff NEW` | Do two builds of one program load the same bytes at the same addresses? If not, in which blocks of memory do the bytes differ, which sections changed size there, and which functions and data objects moved to another block, are new or gone, or changed size, each with what keeps it? |

SYMBOL is a function or data object's name, an import (`write` finds the
`write@plt` stub that code calls), or an address such as `0x1700`. Names are
shown demangled (`app::process_frame`, not `_RNvCs…13process_frame`), and
the end of a name is enough: `process_frame`, or `Gain::run` for the trait
method `<app::Gain as app::Stage>::run`. C++ names are shown with their
parameters, as `c++filt` shows them, but can be typed without:
`process_frame` finds `app::process_frame(app::Frame const&, app::Stage&)`,
`log(unsigned int)` picks one of two overloads of `app::log`, and `vtable
for Gain` finds `vtable for app::Gain`. A name that matches exactly wins
over one that only ends the same way. When SYMBOL is another of an item's
names, such as the weak alias `NMI_Handler` of `Default_Handler`, or an
address past its start, the answer's first line says so: `Default_Handler
(also named NMI_Handler)`, or `process_frame (which holds 0x1705)`. When a
name could mean several items, the error lists the shortest name that picks
each one, or its address when that name is longer than 40 characters, and a
misspelled name gets suggestions from every name an item has. A name that no
item has says why, when elfq can tell, before any suggestions: the file has
no symbol table, and elfq names its items by address, or the compiler
inlined every copy of the function, which `inlined` lists in the functions
that hold them. Typing the command first, as in `elfq callers app.elf
log_msg`, gets a hint that the file comes first, and `elfq help top` prints
the same help as `elfq app.elf top --help`.

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
wrote, with "via veneer" after how it calls, and a call through a PLT stub
to one of the file's own functions with "via PLT". elfq knows the veneers
that GNU ld and LLD add for Cortex-M (`arch::is_veneer`). `callers` also
lists the calls and jumps to an address in the ranges that no item holds,
as to a routine in a chip's ROM, and then the ranges need hold nothing
else. elfq's exit status is then 1 when anything crosses, or might:
through a call elfq can't follow, through a pointer to a function in the
ranges, or in a function in them whose end elfq doesn't know. A CI job can
then check that code in ITCM stays there, or that code run from RAM never
touches flash. As with diff(1), the exit status is 0 otherwise, and 2 for
any error, with or without ranges. When the reader stops early, as `head`
does, elfq still finishes and exits with the answer's status, and an error
that nothing reads still exits with 2.

`--from RANGE` lists only the callers in RANGE, and `--to RANGE` only the
callees, so that `callers 0x20000000+256K --from 0x0+256K` lists what in
ITCM uses DTCM, and the exit status is 1 only for that. Each takes one
range and can be given again. With SYMBOL, they ask about the range that
SYMBOL occupies, as `callers crc32 --from 0x30000000+16M` does, so the
answer looks through veneers as the range forms do, where `callers SYMBOL`
alone lists the veneer. `callees A --to B` and `callers B --from A` list
the same uses, each row from its own side. A call through a veneer or PLT
stub is listed at the call, "via veneer", unless the edge falls at the
veneer: when B holds the veneer but not its target, it is listed as a call
to the veneer, and when A holds the veneer but not the code that calls it,
at the veneer's jump. What elfq can't follow stays when it may be in the
other ranges: `callees` keeps a call to a known address in them, and a call
through a pointer when they hold a function whose address is taken or
stored other than in the vector table; `callers` keeps its note on calls
through a pointer when code in the `--from` ranges calls through one, or is
a function whose end elfq doesn't know, which it names, as `callees` does,
since that code may use the ranges in any way. Code that no item holds,
such as a routine in a chip's ROM, counts only where the file calls it, not
as a caller. Each range of `--from` or `--to` must hold an item outside the
ranges asked about, or for `--to`, such code outside those ranges, so that
a mistyped range is an error and not a quiet exit status 0. An item can be
in both only when everything in the ranges asked about is, as SYMBOL is in
`callees process_frame --to 0x30000000+16M`; otherwise it would be on both
sides of the edge, and elfq says so.

All commands print a table for people by default, with one row per line. A
table puts its short cells, such as sizes, HOW and AT, first and the name
last, so that a long name that the terminal wraps moves no column. Where
an answer is a tree, as in the overview, `top`, `retained`, `why`, `reach
SYMBOL` and `inlined SYMBOL`, each row then names the row it hangs under
by a short name, as in `crc_table, kept by crc32`, so that a row that grep
finds still says where it is; in `why`, only where chains branch. `--tree`
draws such answers as trees instead. AT shows where an item is used: every
site when there are one or two, else the first and how many more. `callers
SYMBOL` and `callees SYMBOL` list the 30 items with the largest own sizes,
callers in order of name and callees in the order SYMBOL first uses them,
and a last row such as `70 more` counts the rest; with ranges, `--from` or
`--to`, they list every crossing. `reach` lists the 30 largest items, and
a row such as `9 more data objects in 0x20000000..0x20000450` sums up the
rest of a block's; with `--to`, it lists every item in its ranges. Without
SYMBOL, it lists 30 starts, and a row such as `650 more` counts the rest
of a kind. `--all` lifts every such cut in an answer but those of AT and
of `reach`'s WHY, cells before the name that stay short: it shows every
item in a table or tree, every chain in `why`, and every name and address
in a sentence, where a long list otherwise ends with "and 48 more".

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

`reach` answers what SYMBOL needs: everything it calls or uses, at any
depth, and the bytes of code and data that takes in each memory block, with
the sections that hold them. It follows the same references as `retained`,
but counts what other code uses too, and starts at SYMBOL instead of the
roots, so it works on shared libraries. Each row names the item that uses
it on a shortest chain from SYMBOL, as in `log_msg, used by app_tick`; a
call through a veneer is listed as a call from the code that calls the
veneer, "via veneer", and the veneer has a row of its own. The rows come by
block, functions then data objects, largest first, then the calls to code
outside any function, as to a chip's ROM, and the imports. Each item counts
once, where the program uses it, not where a load image stores it, so
`crc32`, which runs from ITCM, uses its table in DTCM and nothing in
flash:

```
$ elfq testdata/bin/c-cortex-m7.elf reach crc32
crc32 and the data object it reaches take 48 bytes of code and 1024 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    48     0  .itcm
0x20000000..0x20000450     0  1024  .data
0x20240000..0x20241000     0     0
0x30000000..0x30000edc     0     0

ADDRESS     SIZE  HOW   ITEM
0x10          48        crc32
0x20000000  1024  uses  crc_table, used by crc32
```

`reach --tree` draws the same items as a tree, in the order of the JSON's
rows: each under the item that uses it, in the order that item first uses
them, with each veneer and PLT stub as a step of its own between the call
and its target, and each call or jump to code outside any function under
the item it is in. Like the other trees, it shows the 30 largest items,
with the items on their chains from SYMBOL, and sums up the rest under the
items that use them.

`reach SYMBOL --to RANGE` lists only what SYMBOL reaches in the ranges of
`--to`, SYMBOL included, with every row, and exits with 1 when it lists
anything, as the range checks do, so that a CI job can check that an
interrupt handler in ITCM never runs or uses anything in flash. Each row
has an AT column, and the row of an item that code outside the ranges uses
is the one that `callees USER --to RANGE` lists for that code, so that it
names the code to change, as `app_tick, used by SysTick_Handler` at the
call to its veneer. So the rows name items as `callees` does, by their
whole names, with no "(through a pointer)" mark, since HOW, "address
taken" or "pointer", shows that something takes or stores the function's
address. After SYMBOL's row, what elfq can't follow comes first and stays
by `callees --to`'s rule. The two differ in three ways. `callees --to`
stops at the first crossing, and `reach --to` follows every chain:
`callees app_tick --to 0x20240000+1M` uses nothing in OCRAM, but
`app_tick` reaches `tx_buffer` there through `log_msg` and
`platform_write`. `reach --to` lists SYMBOL itself when it is in the
ranges, so a range that holds only SYMBOL, which `callees` refuses, is an
answer. And a veneer in the ranges has a row of its own even when its
target has one too, since its bytes are there, where `callees` lists only
the call through it. Code that elfq can't read may reach the ranges, so it
too makes the exit status 1. With `--tree`, only the chains from SYMBOL to
what is in the ranges, and to the calls and jumps that elfq can't follow
that may go there, are drawn, and the JSON has the items on those chains.

`reach` without SYMBOL lists where the program starts: `main`, but not a
shared library's; then each root that is a function whose code elfq can
read, but for the PLT's header, which only jumps to the dynamic loader:
the entry point; the handlers in the vector table, in the order of their
first entries; the functions that `.preinit_array`, `.init_array` and
`.fini_array` list, in that order and each in its array's; then the
others, such as personality routines, by kind and by address. Each row has
the bytes of code and data that its start reaches, as `reach START`
counts them: with several memory blocks, both together in a column for
each block, headed by its start, and with one, in CODE and DATA. It lists
`main`, the entry point and the handlers, and of the other starts, those
that reach the most bytes, 30 rows in all, and a row such as `650 more`
after the starts of a kind counts those it leaves out, as in a large C++
library, whose `.init_array` lists hundreds of functions. WHY says why the
program starts there, naming a handler's exceptions as Arm's manuals do
for the file's architecture, but not the reserved entries that list it,
since the processor takes none of them: the first two and how many more.
A start marked "(may reach more)" reaches calls or jumps that elfq can't
follow, or code that it can't read, which `reach START` names, and the
roots whose end elfq doesn't know, such as `_init`, are named below the
table. A large program has many starts, so for each, one search through
the graph only adds up the bytes, without the chains that `reach START`
keeps:

```
$ elfq testdata/bin/c-cortex-m7.elf reach
Where testdata/bin/c-cortex-m7.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2071  main function              main (may reach more)
       200        1100        4096        2345  Reset (entry point)        Reset_Handler (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  Default_Handler
        24          24        4096         430  SysTick                    SysTick_Handler (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
```

A start's own bytes count, so `Default_Handler`, which only loops, has the
2 bytes of its one instruction.

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
the range needn't come from the chip's reference manual. It also suggests
`reach` for the handler in the vector table that reaches the most code, to
show which blocks its code and data are in. It picks from the handlers of
SVCall, PendSV, SysTick and the chip's interrupts, which run in normal
operation, and leaves out those that reach nothing else, such as a default
handler that only loops. Sizes can take a
unit, in any case: `0x20000000+256K`. Units are powers of 1024, as memory
sizes usually are. A range typed as other tools write them, `0x0-0x40000`
or `0x0:0x40000`, is refused with the form to type, `0x0..0x40000`, and
`--range` takes one range, not a list.

`elfq FILE diff NEW` compares two builds of one program, FILE the old and
NEW the new, by the memory they load: each allocated section at its
address, with a section that the file doesn't store, such as `.bss`, as
zeros, and each load image at its load address. Symbols, debug info, where
the file keeps its bytes, and the bytes of loadable segments outside any
section, such as the ELF header, don't count, so a build and its stripped
copy load the same memory. Notes that the program loads, such as
`.note.gnu.build-id`, do count: the linker makes the build ID from the
whole file, debug info included, so two builds that differ only in debug
info, as when they were built in other folders, can differ there, and the
last line, below, names the note. `.tbss` isn't in memory at its
addresses, which other sections share, so its name, address and size are
compared apart. How the bytes are split into sections doesn't count
either, only which byte is at which address, even where sections overlap;
bytes that a damaged header puts past the end of the file are the same
only as bytes missing in the same place. diff first reads only the
headers and the sections' bytes (`binary::LoadedMemory`), not the items,
so it answers at once when the memory is the same, even for a large file.
The exit status is 0 when both load the
same bytes at the same addresses, and diff then says only that; 1 when
they don't; and 2 for an error, as for two files for different
architectures, a file without section headers, which diff needs to find
its memory, or a file that puts nothing in memory. A separate debug file,
as `objcopy --only-keep-debug` makes, keeps the headers of the sections
that hold code, but not their bytes, so diff refuses it too, rather than
compare its code as zeros. Warnings about a file start with its name,
since there are two.

When the memory differs, the blocks are the overview's, worked out once
over both files' sections and load images together, so that both have the
same edges: data that moved from DTCM to SDRAM leaves a block marked
"(gone)" and one marked "(new)". The MEMORY table gives each build's bytes
in each block, from its first section or load image there to the end of
its last, gaps included, as the overview counts a block, and NEW less OLD.
So CHANGE can be more than the changes in SECTIONS add up to: data linked
within 64 KiB of another memory shares its block, gap and all, as when
`.data` and `.bss` move from DTCM to just below OCRAM, and OCRAM's block
grows by 65536 bytes, 1104 of them theirs. SECTIONS has the sections and
load images whose size changed, with the change: the 3 that changed the
most, in address order, and "and 2 more", unless `--all`. A section that
only one build has in the block counts with all its bytes. When no
section changed size, but bytes did, SECTIONS says "bytes differ", and a
block that only one build has names what is in it, as the overview's
table does. A pair of files with one block, as most programs for an
operating system are, gets a line on its size in place of the table.

When both files have a symbol table, diff then loads both, each with its
graph and, unless either is a shared library, its dominator tree, on two
threads, and pairs their items
(`commands/diff/pairing.rs`). The rules run in order, each on the items
still unpaired: the same name; any of their names, as a weak alias; the
name without a compiler's copy suffix, as `dev_read.constprop.0`,
`scale.llvm.123` or `[clone .isra.0]`; the same bytes, at least 4, with
the padding at the end left out, a data object keeping its size; data
whose bytes start with the same string; the same few bytes and size; data
in a section of one name whose users are all paired, each with its
partner, and use it in the same ways, and where several are alike, of
one name, a name that GCC numbered counting as itself or as the name
before the number; and the only object without a name
in a section of one name that the file doesn't store, such as `.bss`. A
key pairs two items only when exactly one item on each side has it, only
a function with a function, or data with data, and only when no other key
of either item leads to a third item, as when one of an item's names is
another item's name in the other build. A name that elfq or a
compiler made from an address, a counter or a hash, such as
`.rodata@0x2004`, `fn_0x8000040`, a `.L` label, also as ThinLTO keeps a
string's, `.str.12.llvm.345`, or Rust's `anon.` objects, pairs nothing by
name, and a data object's name that GCC numbered, as a switch table's
`CSWTCH.12` or a function's static variable `buf.3`, pairs by name only
with what uses it: GCC counts the file's names, so the number moves when
a function is added before it.
A wrong pair never changes a total or the exit status, which come
from the bytes in each block, only what a row calls an item.

A paired item that is in another block moved; one in the same block
grew or shrank, or is the same. The first line counts them, with the new
and gone items. MOVED has a row for each item that moved, with its sizes
and where it starts in each build, largest first, but one row for all the
items of whole sections that moved by one offset between the same two
blocks, as "all 12 items of .data and .bss", unless `--all`; it is never
cut. CHANGE has the rest, grouped by block in MEMORY's order, largest
change first. A new item and the new items in its block that only it
keeps, below it in the new build's dominator tree, are one row, as
"strtod (new, with 114 new items it keeps)", whose NEW is their bytes;
likewise a gone item and the gone items in its block that only it kept,
in the old build's tree, as "(gone, with 3 gone items it kept)". Data
objects without a
name whose estimated size changed, as it does when what follows them
moves, fold into one row per block, and so do new and gone items of one
name, each with their sum. The table has 30 rows, unless `--all`, which
also leaves out the subtrees and folds: every block that changed shows at
least its largest row, and a row for the rest of each, as "24 more in
0x08000000", counts the items in them and gives their sum, but where one
row is left, the block shows it, as it takes no more room. So a block's
rows add up to the change of its items that stayed in it; MOVED has the
rest. A row ends with what keeps the item, as `top`'s rows do: ",
kept by X", its parent in the dominator tree, in the old build for a
gone item, nothing when several of the places where the program starts
keep it, or ", which nothing reaches". "(was NAME)" gives the old name of
an item whose name changed, as a compiler's copy's does, when neither
name is made, the two share no name, as an item and its alias do, and the
names differ in more than the numbers that change with every build,
ThinLTO's, as in `f.llvm.123`, and GCC's. When the new build's DWARF says that the compiler inlined every copy
of a gone function, and its name finds nothing in the new build, as for
`why`, its row says where, as "(gone, inlined into process_frame)", and
then doesn't say that process_frame kept it too. elfq can't yet tell what
keeps items in a shared library, so when either build is one, a line
above the tables says so and rows don't, and the same when elfq can't
read the code. A line counts the items that are the same, and the Try
line asks `why` of the new item that heads the subtree of new items with
the most new bytes, when something keeps it, rather than it being a
place where the program starts, and it has a name of its own.

When no row says why the memory differs, as when no item changed
because only a build ID or `.tbss` differs, or when either file has no
symbol table, the last line names every section whose address, size,
load image or bytes differ, or that only one build has, in address order,
as "Sections that differ: .note.gnu.build-id.", and "load image of .data"
for a section whose load image alone moved, so that exit status 1 always
has a reason in what diff prints. When rows say why, the line is left
out: the sections it would add are mostly tables of pointers, such as a
vector table or `.ARM.exidx`, whose bytes changed only because what they
point to moved, which the rows already say.

### JSON

`--json` prints the same answer for scripts, as one object, for every
command but `browse`, which is for reading in a terminal and refuses
`--json`, `--tree` and `--range`; for scripts, `callers` and `callees` print
its links, and `objdump --start-address` the instructions. The JSON has
every item, site and chain, where the text shows only the largest items, the
first sites and the nearest chains; every key is always there, `null` when
it has no value; addresses are hex strings, as elfq prints them and SYMBOL
takes them, and sizes are numbers. Each row of a list in the answer, such as
an item of `top` or a chain of `why`, is a line of its own, so that `grep`
and `head` work on the JSON as on the text. Until 1.0 the JSON can change,
and each release says how. An error is never JSON: it goes to stderr, with
exit status 2 and nothing on stdout.

An item looks like this, spread over lines here:

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

| Key | Value |
|---|---|
| `name` | its name, as the text shows it |
| `kind` | `function`, `data` or `import` |
| `section` | the section that holds it, such as `.data` or `.bss`, or `null` for an import |
| `address` | where it starts, or `null` for an import, whose code is in another file |
| `size` | its size in bytes, or `null` for an import, or when elfq doesn't know it |
| `size_estimated` | whether elfq estimated the size, which the text marks `~` |

SYMBOL's item, `symbol` in most answers and `function` in
`inlined SYMBOL`'s, also has `aliases`: its other names, such as the
mangled symbol of a demangled name, or weak aliases at its address. A
range has a `start` and an `end`, which isn't in it.

Each answer has these keys, in this order.

**The overview**, `elfq FILE --json`, says what the file is and what elfq
found in it, and leaves its table of the largest items to `top --json`:

| Key | Value |
|---|---|
| `file` | the file's path, as given |
| `architecture` | as the text names it, such as `x86-64` or `32-bit Arm` |
| `kind` | `executable`, `position-independent executable` or `shared library` |
| `has_symbols`, `has_dwarf` | whether the file has a symbol table, and DWARF debug info |
| `memory` | the memory blocks, in address order, each a range with its `size`, its `sections`, and `load_images`, the sections whose load images it holds |
| `range` | the range that `--range` picked, or `null` |
| `functions`, `data_objects` | how many of each elfq found, or with `--range`, how many have bytes in it |
| `bytes` | the bytes of all of them, or those in the range |
| `imports` | how many imports the file has, or `null` with `--range` |
| `references` | how many references elfq found, or `null` when it can't read the file's code, or with `--range` |
| `indirect_calls` | how many calls and jumps go through pointers set at run time, or `null` when `references` is |

**`top`**:

| Key | Value |
|---|---|
| `range` | the range that `--range` picked, or `null` |
| `bytes` | the bytes of every item, or of those in the range: the sum of every `own` in `items` and `unreachable` |
| `items` | the rows of the tree, below, for the items that something reaches |
| `unreachable` | the items that nothing reaches, largest first, each with `own` and `load_image` too |

**`retained SYMBOL`**:

| Key | Value |
|---|---|
| `symbol` | SYMBOL's item |
| `reachable` | whether anything reaches SYMBOL from the roots; when nothing does, `items` and `shares` are empty |
| `range` | the range that `--range` picked, or `null` |
| `own` | SYMBOL's own bytes, or with `--range`, those in the range |
| `load_image` | how many of `own` are its load image's |
| `retained` | `own` and the `own` of each row of `items`: what removing SYMBOL would save |
| `items` | the rows of the tree, below, for the items that only SYMBOL leads to |
| `shares` | what SYMBOL and those items use that other code uses too, and so isn't counted, sorted by name; with `--range`, only items with bytes in it |
| `indirect_calls` | how many calls and jumps in SYMBOL and those items go through pointers set at run time |

The `items` of `top` and `retained` are the rows of their trees, in the
order of the text's rows, each an item with these keys too:

| Key | Value |
|---|---|
| `id` | the row's number, counting from 0 |
| `own` | its own bytes, as in the OWN column: all of them, or with `--range`, those in the range; 0 where OWN shows `?` |
| `load_image` | how many of `own` are its load image's, for a row marked "(load image)" or "(with its load image)" |
| `retained` | its retained size |
| `through_pointer` | whether the row is marked "(through a pointer)" |
| `parent` | the `id` of the row it hangs under, or `null` for a row at the top of the tree: in `top`, a root or what several roots reach; in `retained`, what SYMBOL keeps itself |

So a script that adds up `retained` takes only the rows whose `parent` is
`null`. The first rows of `retained process_frame`, shown above:

```json
{"id":0,"name":"crc32","kind":"function","section":".text","address":"0x1280","size":59,"size_estimated":false,"own":59,"load_image":0,"retained":1083,"through_pointer":false,"parent":null},
{"id":1,"name":"crc_table","kind":"data","section":".rodata","address":"0x2120","size":1024,"size_estimated":false,"own":1024,"load_image":0,"retained":1024,"through_pointer":false,"parent":0},
```

**`why SYMBOL`**:

| Key | Value |
|---|---|
| `symbol` | SYMBOL's item |
| `reachable` | whether anything reaches SYMBOL from the roots |
| `root` | why SYMBOL is itself a root, in the words of a chain's first step, or `null`; when it is one, `chains`, `unreachable_users` and `users_it_keeps` are empty |
| `chains` | for each item that keeps SYMBOL, the shortest chain from the roots through it to SYMBOL, the shortest first; each is a list of steps with an `item` and a `how` |
| `unreachable_users` | the items that use SYMBOL but that nothing reaches |
| `users_it_keeps` | the items that use SYMBOL but that only SYMBOL leads to, so they don't keep it either |
| `on_every_chain` | the closest item that every chain goes through, or `null`: SYMBOL counts toward that item's retained size |

The first step of a chain is a root, and its `how` says why the program
starts there: `entry point`; `vector table` for the table itself; `listed
in the vector table` for a handler, which the HOW column shortens to
`vector table`; `used by the dynamic loader`; `used by the unwinder`; or
`in` or `listed in` and a section, as in `listed in .init_array`. In the
other steps, it says how the item before uses this one, in the words of the
JSON column in [Concepts](#concepts), joined with commas when there are
several.

**`inlined`**:

| Key | Value |
|---|---|
| `inlined_bytes` | the bytes of code that inlined copies take in all |
| `code_bytes` | the bytes of all the functions in the file |
| `functions` | each function that the compiler inlined, the most bytes first, with its `name`, the `bytes` its copies take, leaving out the functions inlined into them, and how many `copies` there are |

**`inlined SYMBOL`**:

| Key | Value |
|---|---|
| `function` | SYMBOL's item |
| `inlined_bytes` | the bytes of its code that inlined copies take |
| `inlined` | the rows of the tree, in the order of the text's rows, each the copies of one function in one place, with `id`, `name`, `bytes`, which count the copies inlined into them, `copies`, and `parent`, the `id` of the row of the copies they were inlined into, or `null` for those inlined into SYMBOL's function itself |

**`callers SYMBOL`** and **`callees SYMBOL`**:

| Key | Value |
|---|---|
| `symbol` | SYMBOL's item |
| `callers` or `callees` | a row for each item on the other end and way it is used, in the text's order: callers by name, callees as SYMBOL first uses them. Each has the `item`, the `how` and the `sites`; `how` is in the words of the JSON column in [Concepts](#concepts), so a text row that says `uses` can be two rows here |
| `unresolved` | for `callees` only, the calls and jumps that elfq can't follow, each with its `site`, `how`, which is `call` or `jump`, and `target`, where it goes when the code says so but no item is there, else `null` |
| `complete` | whether the list is complete, as far as elfq can tell, as [below](#calls-elfq-cant-follow) |

**`callees RANGE...`** and **`callers RANGE...`**, and with `--to` or
`--from`, **`callees SYMBOL`** and **`callers SYMBOL`**:

| Key | Value |
|---|---|
| `ranges` | the ranges, or the range that SYMBOL occupies: its first byte when elfq doesn't know its size |
| `to` | for `callees` only, the ranges of `--to`, or `null` without it |
| `from` | for `callers` only, the ranges of `--from`, or `null` without it |
| `callees` | for `callees`, the references that cross, a row for each pair of items and way of use, in the text's order, the largest item outside first: the `item` or import outside; the `caller` in the ranges that uses it; `how`, as in `callees SYMBOL`; `veneer`, the linker's veneer or PLT stub that a call goes through, or `null`; and the `sites`. With `--to`, only those into its ranges, where a call through a veneer in them that goes on elsewhere uses the veneer, as its `item` |
| `callers` | for `callers`, the same, but with the `callee` in the ranges that `item` uses, in place of `caller` |
| `unresolved` | for `callees`, as in `callees SYMBOL`, with the `caller` it is in and the `veneer` too; with `--to`, only those that may go to its ranges. For `callers`, the calls and jumps from outside to an address in the ranges that no item holds, as to a chip's ROM, in the same shape |
| `unreadable` | the functions whose end elfq doesn't know, so it can't read their code: for `callees`, those in the ranges; for `callers`, those in the ranges of `--from`, outside the ranges, and none without it |
| `through_pointer` | for `callers` only, the functions in the ranges whose address is taken or stored, other than in the vector table; with `--from`, only when code in its ranges calls through a pointer, or is code that elfq can't read |
| `complete` | whether the lists have every crossing, as far as elfq can tell, as [below](#calls-elfq-cant-follow) |

**`reach SYMBOL`**:

| Key | Value |
|---|---|
| `symbol` | SYMBOL's item |
| `to` | the ranges of `--to`, or `null` without it |
| `code_bytes`, `data_bytes` | the bytes of the functions, and of the data objects, among SYMBOL and what it reaches, or with `--to`, among those in its ranges |
| `blocks` | every memory block that the overview's `memory` lists, in address order, each a range with the `code_bytes` and `data_bytes` in it, counted the same way, and the `sections` there that hold what they count |
| `items` | SYMBOL, then what it reaches, as the rows of a tree: each comes after its parent, and the items its parent uses come in the order it first uses them. Each is an item with an `id`, counting from 0; the `block` that holds it, as the block's `start`, or `null` for an import, or for an item in no block, as a damaged file can have; `how` its parent uses it, in the words of the JSON column in [Concepts](#concepts); the `sites` where it does; `through_pointer`, whether it is a function there only because something takes or stores its address, which the text marks "(through a pointer)", but in the rows of `--to`; `in_to`, whether it is in the ranges of `--to`, or `null` without it; and `parent`, the `id` of the item that uses it on its shortest chain from SYMBOL. `how` and `parent` are `null` for SYMBOL. A veneer has a row of its own, between the call to it and its target. With `--to`, only the items that `--tree` draws, numbered again: SYMBOL, the items in its ranges, those with calls and jumps in `unresolved`, and the items on their chains |
| `unresolved` | the calls and jumps in SYMBOL and what it reaches that elfq can't follow, each with the `caller` it is in, and as in `callees SYMBOL`, its `site`, `how` and `target`; not the jump in a PLT's header to the dynamic loader. With `--to`, only those that may go to its ranges, as `callees --to` keeps them |
| `unreadable` | the functions that SYMBOL reaches whose end elfq doesn't know, so it can't read their code |
| `complete` | whether `items` has everything SYMBOL reaches, or with `--to`, in its ranges, as far as elfq can tell, as [below](#calls-elfq-cant-follow) |

**`diff`**:

| Key | Value |
|---|---|
| `old`, `new` | the files' paths, as given |
| `same_bytes` | whether both load the same bytes at the same addresses, which exit status 0 says |
| `items_compared` | whether diff compared the functions and data objects: not when the memory is the same, nor when either file has no symbol table |
| `old_bytes`, `new_bytes`, `unchanged` | the bytes of the functions and data objects in each file, as the overview's `bytes`, and how many paired items have the same size, in the same block; `null` while `items_compared` is `false` |
| `sections_that_differ` | the names of the sections whose address, size, load image or bytes differ, or that only one file has, in address order, `.tbss` among them, as the text's last line names them when no row says why, but every one and always; empty when `same_bytes` is `true` |
| `blocks` | the memory blocks, worked out over both files, in address order, each a range with: `old_size` and `new_size`, each file's bytes in it, from its first section or load image there to the end of its last, gaps included, or `null` when it has none there; `change`, `new_size` less `old_size`, counting `null` as 0; `same_bytes`, whether both put the same bytes at the same addresses in it; `item_bytes`, `section_bytes` and `padding_bytes`, how much of `change` the items that start in it, the sections and load images that hold none, and the bytes between them make up, which add up to `change`, unless an item lies inside another, whose bytes `item_bytes` then counts twice, `null` while `items_compared` is `false`; `item_bytes` is also what the `items` change it by, a moved item taking its `old_size` from its `old_block` and adding its `new_size` to its `new_block`; and `sections`, the sections and load images in it, in address order, but not `.tbss`, each with its `name`, `load_image`, whether it is the section's load image, `holds_items`, whether items are in it in either file, `null` while `items_compared` is `false`, and `old_address`, `old_size`, `new_address` and `new_size`, `null` for a file that doesn't have it in the block |
| `items` | every item that changed, largest change first, and of the same change, by name, each with: `name`, the new file's, or for a gone item, the old file's; `was`, the old name when the new file names it otherwise, or `null`; `kind`; `status`, `moved`, `new`, `gone`, `grew` or `shrank`; `paired_by`, the rule that paired it, `name`, `bytes`, `uses` or `place`, or `null` for a new or gone item; `change`, `new_size` less `old_size`, counting `null` as 0; in each file, `null` for a file that doesn't have it, `old_address` and `new_address`, `old_size` and `new_size`, `old_section` and `new_section`, `old_block` and `new_block`, the `start` of its block, `old_kept_by` and `new_kept_by`, the `name` and `address` of the item that keeps it, its parent in the dominator tree, or `null` when several of the places where the program starts keep it, when nothing reaches it, when either file is a shared library, or when elfq can't read the code, and `old_retained` and `new_retained`, its retained bytes, as `top` gives them, or `null` when nothing reaches it or there is no tree; `size_estimated`, whether elfq estimated its size in either file; and `inlined_into`, for a gone function that the new file's DWARF says the compiler inlined everywhere, and whose name finds nothing in the new file, the functions it inlined it into, or `null`. `old_kept_by` and `new_kept_by` are the only keys that name an item's parent by name and address rather than by an `id`, since the rows aren't a tree; `[]` when `items_compared` is `false` |

**`reach`**, without SYMBOL:

| Key | Value |
|---|---|
| `starts` | where the program starts, every one, in the text's order, each an item with: `root`, why it is a root, as `why`'s `root` says it, or `null` for `main`; `exceptions`, the exceptions that it handles, as WHY names them but every one, such as `["NMI","HardFault"]`, and `[]` for a function that isn't a handler; `code_bytes` and `data_bytes`, the bytes of the functions and of the data objects among the start and what it reaches; `blocks`, every memory block that the overview's `memory` lists, each a range with the `code_bytes` and `data_bytes` in it; and `complete`. Each is what `reach START --json` has for the start |
| `unreadable` | the roots that are functions whose end elfq doesn't know, such as `_init`, so it can't tell what they reach |

### Calls elfq can't follow

Calls through pointers leave some answers incomplete, and elfq says so
where they do. `callers` notes that it leaves out calls through a pointer to
a function whose address is taken or stored, and `callees` lists the calls
and jumps it can't follow. `retained` notes when SYMBOL or the items it
keeps call through pointers, since what those calls reach may count for
other items.
`reach` names the code in what SYMBOL reaches that calls or jumps through a
pointer, since SYMBOL may reach more through it, or with `--to`, lists
those calls and jumps as `callees` does, and names the functions whose end
elfq doesn't know, since it can't read what they use.
Without SYMBOL, `reach` marks a start "(may reach more)" when its answer
would name such code.
In JSON, `callers`, `callees` and `reach` have `complete`, and `retained`
has `indirect_calls`. Without `--to`, `reach`'s is `false` when
`unresolved` or `unreadable` isn't empty, and without SYMBOL, each
start's is `reach START`'s. For ranges, and with `--from` or `--to`,
`complete` is `false` when `callees` or `reach` lists a call it can't
follow, other than, with `--to`, one to a known address in its
ranges, or a function whose end it doesn't know, or when `callers` notes
calls through a pointer to a function in the ranges, whose address is
taken or stored other than in the vector table, which only the processor
calls through, or names code in the `--from` ranges whose end it doesn't
know. A call from outside to code in
the ranges that no item holds, as to a chip's ROM, goes where `callers`
says, so it leaves the answer complete.
`callers` can't see calls from code whose end elfq doesn't know, such as
assembly without a `.size`, but C runtimes have such code (`_init`,
`_fini`) in almost every file, so it doesn't make the answer incomplete,
except in the `--from` ranges, where `callers` names it and keeps the note
on calls through a pointer, since it may make them.

## How it works

elfq reads the file, then answers one question. `binary` reads the file,
finds its items and has `arch` read each function's code once; `graph`
connects the items by the references found there, and works out the roots,
the dominator tree and the shortest chains; and the command picks what to
show and writes it, as a table that `render` lays out, or as JSON.
`debuginfo` reads DWARF beside the graph, for `inlined`, and to say when
SYMBOL names a function that the compiler inlined. [Life of a
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
   Thumb). On x86-64 it needs only these instructions. On Thumb it also
   skips the literal pools, jump tables, tables of addresses that
   `ldr.w pc` jumps through and constants that `adr` points at inside
   functions, which would otherwise decode as instructions. A Thumb branch
   through a register goes where the code put
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
   around SYMBOL itself. Of equally short chains it takes the one with the
   most calls, and then the one that the breadth-first search found first,
   so the answer is always the same. `reach` searches from SYMBOL the same
   way, for the shortest chain to every item. Without SYMBOL, it only adds
   up the bytes that each start reaches, in one search from each, with no
   chains (`reach::Totals`), which the overview also uses to pick the
   handler that reaches the most code.
   `callers` and `callees` read edges. `callers` counts a call to the stub
   for one of the file's own functions, such as `log_msg@plt` in a shared
   library, as a call to the function, and says so.
7. **Read DWARF** when it is present (`gimli`), in `debuginfo`: each
   inlined-subroutine entry is a copy of one function in another, with the
   address ranges of its code. `inlined` adds them up. Copies whose code the
   linker dropped still have entries, so elfq skips those that don't start
   inside an item. A SYMBOL that no item has is looked for among them too.
   Nothing above depends on DWARF; source locations could make output
   easier to follow later.

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
4. **SYMBOL.** `retained::run` checks that elfq can read the file's code
   (`check_code_is_readable`) and turns `process_frame` into an item with
   `Names::find`: by address, by full name, or by the end of a name.
   Working out the ends of every name takes a while in a large file, so
   `Names` files each item under the last part of the paths that its
   names' ends can have (`short_name_keys`), and works out the ends of
   only the items filed under `process_frame`. `retained::refusal` refuses
   an import, and a function whose end elfq doesn't know
   (`readable_item`), then a shared library, before `run` builds the
   graph.
5. **The graph.** `retained::run` has `graph::Graph::build` turn what
   `arch::scan` found in each function's code into references, or into
   unresolved calls. `Graph::build` then adds the pointers stored in data,
   from the dynamic relocations or, in a file that runs at a fixed
   address, from the words that hold an item's address, and finds the
   roots with `roots::find`.
6. **The dominator tree.** `RetainedTree::new` borrows the graph, and
   `graph::DominatorTree::build` has `petgraph` find each node's closest
   dominator, starting from one node above every root, and adds up the
   retained sizes from the bottom of the tree up.
   `RetainedTree` also notes which functions something calls directly, for
   the "(through a pointer)" mark.
7. **The answer.** `retained::answer` words the lead line, then
   `RetainedTree::largest` picks the 30 largest items under
   `process_frame`, and the items between them and it, and
   `RetainedTree::tree_row` makes a `render::TreeRow` of each, with the
   short name that the rows under it call it by (`Names::short_name`).
   `shared_note` names what `process_frame` uses that other code uses too,
   and `indirect_note` notes any calls through pointers in `process_frame`
   or the items it keeps. `TreeAnswer::write` writes the lead line, then
   `render::Table` puts the rows in its last column, as rows that each name
   the row they hang under or, with `--tree`, drawn as a tree
   (`Table::tree`), and writes the table (`Table::write`), then the notes.
   With `--json`, `retained::write_json` lists every row of the tree
   instead (`RetainedTree::to_json`), and writes them through `json::write`.
8. **Exit.** `run`, in main.rs, flushes the output and turns the answer into
   the exit status: 0 for an answer, 1 when `callers` or `callees` with
   ranges, `--from` or `--to` finds something that crosses, or may,
   `reach --to` finds something in its ranges, or may, or `diff` finds
   that the builds load different memory, and 2 for an error,
   which it prints after `error:`. What the command keeps until it has
   answered, such as the loaded file, its graph and `Names`, isn't freed:
   the commands keep it in a `Kept`, which leaves it for the system to
   take back when elfq ends, at once, since freeing it a piece at a time
   takes a while in a large file. `run` turns that on with
   `end_after_answer`; tests, which run many commands in one process,
   leave it off.

Each module uses only the ones below it. Beside each are the steps above
that run in it:

```
main.rs      1 main, run                                  8 exit status
commands/    2 commands::run   4 retained::run, symbol::find
             7 retained::answer, RetainedTree::largest, tree_row
cli.rs       1 Cli, Command
render/      7 Table::tree, Table::write
debuginfo/   (for inlined, and SYMBOL that the compiler inlined)
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

`browse` adds `ratatui` with its `crossterm` backend (the terminal),
`rustix` and `signal-hook` (Ctrl-Z, on Unix) and iced-x86's Intel
formatter (printing x86-64 code), behind the `tui` feature, which is on by
default. They take elfq from 40 crates to 91, elfq included, as `cargo
tree -e normal` lists them on Linux, each once; its release build from
4.1 MB to 5.0 MB on x86-64 Linux; and a clean release build from about
33 s to about 46 s on 4 CPUs. `cargo build --no-default-features` builds
elfq without them, and then `browse` says so.

`browse` prints Thumb code with `thumb-asm`, a disassembler with no
dependencies of its own, behind the same feature: elfq's own decoder says
what an instruction does, not how to write it. Only `arch/listing.rs`
uses it, and only to print the lines that elfq's sweep cuts the code into
(`arch::thumb::layout`), so the lines start where the references that
elfq finds are. It is pinned to one version, since another may write some
instructions differently, and the tests check its text, against
objdump's for the samples' code, in `testdata/tests/thumb`. It takes
elfq to 92 crates and its release build from 5.0 MB to 5.3 MB on x86-64
Linux, and adds no time that we could measure to a clean release build,
about 47 s on 4 CPUs.

## Status and roadmap

The overview, `top`, `retained`, `why`, `callers`, `callees` and `reach`
work on x86-64 and Cortex-M executables, with or without symbols and DWARF,
and `inlined` works on any file with DWARF that isn't compressed. The
overview, `callers`, `callees` and `reach` also work on x86-64 shared
libraries. `browse` prints x86-64 code and the Thumb code of Cortex-M
cores up to Armv7E-M; for Armv8-M cores, its pages list only the lines
that use another item. `diff` compares any two builds of one program that
have section headers, by block of memory, and their functions and data
objects when both have a symbol table. The pull requests and the git
log record how elfq got here.

On a 4-CPU x86-64 machine, a release build gives the overview of elfq's
own debug build (90 MB) in under half a second, of cargo (43 MB) in under
a second, and of librustc_driver (158 MB) and libLLVM (199 MB) in 3 to 4
seconds, most of it reading the file: decoding its code and demangling
its names. `browse` shows its first screen in about the same time. Each
key typed into go to then takes under 0.1 s. The first takes up to 0.2 s
in those libraries, and the first whose text names nothing takes about
half a second, as it works out what to suggest.

Next, `diff` lists the calls between blocks that are new or gone.

Ideas for later, not yet planned: output that scripts and AI agents can
read more easily, a check of what startup code copies into RAM, the
retained size of a set of items, the functions that shared libraries
export as roots, `--root SYMBOL`, worst-case stack depth (the call graph
plus each function's stack frame), and a query language.

## Non-goals

- **Re-implementing readelf, objdump, nm or bloaty.** elfq does not print
  headers, section tables, whole-section disassembly listings, hex dumps or
  flat size breakdowns. When one of those tools already answers a question
  well, the docs point to it. `browse` shows one item's code at a time, as
  the context for elfq's links: what each line calls or uses, in the words
  of the other commands. A data object's page lists only the words that
  hold pointers, a vector table's entries and the strings of an object that
  holds only strings, and gives the `objdump -s` command that prints its
  bytes.
- **A reverse-engineering framework.** `browse` reads code and follows it,
  but it has no patching, renaming, decompiler or graph canvas.
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
5. **Thumb text.** `browse` never shows wrong text where elfq knows code
   from data; where it can't tell, it prints `.word` or `.inst`. It prints
   the text of Thumb instructions only where elfq's sweep decodes an
   instruction, so a literal pool, a jump table, a table of addresses and
   the bytes that the file's `$d` symbols mark print as `.word`, `.short`
   or `.byte`, and an instruction that thumb-asm can't read prints as
   `.inst`. Without symbols, the sweep misses some data: in stripped
   Cortex-M0 code built with `-Os`, the tables of `__gnu_thumb1_case_uhi`
   and its kind, which follow the call, and a literal pool that only the
   function before it loads, still print as instructions. On the 105
   stripped Cortex-M files of the samples and a firmware corpus, 646 of
   the 247,778 bytes that the same files with symbols mark as data inside
   functions print as instructions, 32 of them as `.inst`, all in five
   Cortex-M0 files. Text is shown only for the cores up to Armv7E-M, as
   the file's build attributes name them: for Armv8-M and Armv8.1-M cores,
   such as the Cortex-M33 and M55, and files that don't say, a page lists
   only the lines that use another item.
6. **Compressed DWARF.** `inlined` can't read debug info that the linker
   compressed (`--compress-debug-sections`).
