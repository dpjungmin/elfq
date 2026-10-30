# elfq

elfq ("ELF query") answers questions about an ELF binary that existing tools
can't, such as:

- Which functions are reached only through `process_frame`?
- How many bytes would go away if `process_frame` did?
- Why is `core::fmt::write` in my firmware at all?

It is meant for x86-64 Linux programs and Cortex-M firmware alike, and for
binaries as they are shipped: with or without DWARF, even stripped.

**elfq is fully vibe coded:** all of its code and documentation were written
by an AI (Claude).

```sh
elfq app.elf                            # what's in app.elf, and what to ask next
elfq app.elf top                        # what keeps the most bytes in app.elf
elfq app.elf top --tree                 # the same, drawn as a tree
elfq app.elf top --range 0x0+256K       # the same, counting only the first 256 KiB
elfq app.elf retained process_frame     # what removing process_frame would save
elfq app.elf why crc_table              # what keeps crc_table in the file
elfq app.elf inlined                    # which inlined functions take the most bytes
elfq app.elf inlined main               # the inlined code in main
elfq app.elf callers log_msg            # what calls or uses log_msg
elfq app.elf callees main               # what main calls or uses
elfq app.elf callees 0x0+256K           # what the first 256 KiB uses outside it
elfq app.elf reach                      # where the program starts, and the bytes each start reaches
elfq app.elf reach SysTick_Handler      # what SysTick_Handler runs and uses, and where
elfq app.elf browse                     # where the program starts, each start a page to read
elfq app.elf browse log_msg             # read log_msg's code, what it calls and reaches, its callers, and what keeps it
elfq old.elf diff new.elf               # what changed from old.elf to new.elf
elfq app.elf startup                    # whether the reset code copies and zeroes memory before it uses it
```

[docs/examples.md](docs/examples.md) asks real questions about the sample
binaries in `testdata/bin`, each with the command that answers it and what
elfq prints.

Every command but `browse` also prints JSON for scripts with `--json`:
every item, site and chain, one row per line, though the overview's JSON
leaves its table to `top`'s. [docs/design.md](docs/design.md#json) lists
the keys. Until 1.0 the JSON can change, and each release says how. The
overview, `top` and `retained` can count only the bytes in one range of
memory, such as a microcontroller's ITCM, with `--range START..END` or
`--range START+SIZE`; for firmware, the overview lists the ranges its
sections and their load images occupy.

`callers` and `callees` take ranges in place of a symbol, and then list
only what crosses their edge, such as code in ITCM that calls a function in
flash. elfq's exit status is then 1 if anything does or might, so a CI job
can check that time-critical code stays in fast memory, or that code run
from RAM never touches flash. `--from RANGE` lists only the callers in
RANGE, and `--to RANGE` only the callees, for ranges or a symbol:
`callers 0x20000000+256K --from 0x0+256K` lists what in the first 256 KiB
uses the 256 KiB at 0x20000000, and `callers crc32 --from 0x30000000+16M`
what at 0x30000000 calls crc32. Errors go to stderr, with exit status 2
and nothing on stdout.

`reach SYMBOL --to RANGE` lists only what SYMBOL reaches in RANGE, at any
depth, with the code that uses each item there, and exits 1 if it reaches
anything there or might. To check that an interrupt handler runs only from
TCM, give `--to` for every other block that the overview lists. A call
through a pointer that may go there counts too; to fail only on what elfq
is sure of, let exit status 1 through and check the JSON:

```sh
elfq app.elf reach SysTick_Handler --to 0x20240000+1M --to 0x30000000+16M \
  --json > reach.json || [ $? -eq 1 ]
jq -e 'all(.items[]; .in_to | not) and all(.unresolved[]; .target == null)' reach.json
```

`elfq old.elf diff new.elf` compares two builds of one program: its exit
status is 0 when both load the same bytes at the same addresses, as a build
and its stripped copy do, and 1 when they don't, with the blocks of memory
whose bytes differ, the sections that changed size in each, the functions
and data objects that moved to another block, are new or gone, changed
size, or are edited, which is to say they kept their size and block but
changed in more than the addresses of what moved, each with what keeps
it, the calls between blocks that are new or gone, and when no item
changed, the sections that differ. A CI job can check that a change
leaves the firmware's memory as it was, or with the JSON, only that no
call between blocks is new, such as from ITCM to flash:

```sh
elfq old.elf diff new.elf --json > diff.json || [ $? -eq 1 ]
jq -e '(.same_bytes or .items_compared) and ([.crossings[] | select(.status=="new" and (.how=="call" or .how=="tail call"))] | length == 0)' diff.json
```

The check counts calls and tail calls only. A use of data from another
block is new each time a relink moves the data to another block, and a
pointer is any word of data that holds an address, so a constant that
happens to equal a function's address reads as one. A call between
blocks is the same in both builds when the same function calls the same
one from and to the same blocks, so the check passes when a call only
becomes a tail call or stops being one, or gains or loses a veneer in
the caller's or the callee's block. It still fails when the code at
either end of a call between blocks moves to a third block, as when
code that calls flash moves from OCRAM into ITCM, or when a relink moves
it to another block, as when flash starts at 0x60000000 rather than
0x30000000: diff lists such a call as gone, and as new between the
blocks it is between now. As diff works out the blocks over both builds
together, a smaller relink, which leaves the two builds' flash
overlapping or less than 64 KiB apart, as a 32 KiB bootloader offset
does, keeps one block, and its calls stay the same. Both builds need a
symbol table: without one, diff compares no items, so the check fails
unless the memory is the same.

`elfq app.elf startup` reads what the reset code of Cortex-M firmware
does before `main`, in order: each copy of a load image, such as `.data`
or code that runs from ITCM, from where the file stores it, and each
zeroing of a section such as `.bss`. Its exit status is 1 when code runs
from a load image before its copy, as when `memcpy` itself is linked into
ITCM, when a load image isn't copied in full, or `.bss` isn't zeroed in
full, and when elfq can't follow code that may set one up, such as a
loop of the startup code's own, which it can't follow yet. It reads only
the file, so it can't know what a boot ROM or a debugger loads, how the
chip's memory is set up, or what interrupts and other cores do. To fail
only on what elfq is sure of, let exit status 1 through:

```sh
elfq app.elf startup --json > startup.json || [ $? -eq 1 ]
jq -e '.problems_certain == 0' startup.json
```

**For AI agents:** paste this into a project's AGENTS.md or CLAUDE.md to
tell its agents about elfq:

```markdown
## elfq

elfq answers questions about a built ELF file: what keeps the most bytes,
what removing a function would save, why something is in it, and what
calls what. Run `elfq FILE.elf` first: its table is the start of `top`'s,
and `elfq --help` lists the commands.

- SYMBOL is a name, the end of a name, or an address. `top`, `retained`,
  `callers`, `callees` and `reach` show the 30 largest items, `inlined` the
  30 largest functions, and `why` the chains through the 10 nearest users;
  `--all` shows every row, and `--json` has every row.
- Each table row is one line, with the name last. A name after "kept by"
  or "used by" works as SYMBOL; rows with ranges, `--to` or `--from` give
  whole names, which several items can share, so use an address from AT.
  A `why` row without "used by" is used by the row above, or starts a
  chain. Read `why`'s chains with `--json`.
- `elfq FILE.elf callees 0x0+256K` lists what the code and data in that
  range use outside it, and `callers 0x0+256K` what uses them from
  outside; both exit 1 if anything does or might. `callees --to RANGE`
  and `callers --from RANGE` keep only what is in RANGE, with ranges or
  SYMBOL. With ranges, `--to` or `--from`, both show every row.
- `elfq FILE.elf reach SYMBOL` lists everything SYMBOL calls or uses, at
  any depth, with the bytes of code and data it takes in each block of
  memory, such as ITCM and flash: a hot function's whole footprint, and
  what of it isn't where it should be. `--to RANGE` keeps only what it
  reaches in RANGE, with every row, and exits 1 if there is any, or might
  be. Without SYMBOL, `reach` lists where the program starts, such as
  `main` and each interrupt handler, with the bytes each start reaches in
  each block.
- `elfq OLD.elf diff NEW.elf` exits 0 when both builds load the same bytes
  at the same addresses, and 1 when they don't, with the blocks of memory
  whose bytes differ, the sections that changed size in each, the
  functions and data objects that moved to another block, are new or gone,
  changed size, or are edited (changed in place), each with what keeps
  it, the calls between blocks that are new or gone, and when no item
  changed, the sections that differ; another build ID alone makes them
  differ.
- `elfq FILE.elf startup` lists what the reset code of Cortex-M firmware
  copies and zeroes before `main`, in order, and exits 1 if code runs
  from a load image, such as code in ITCM, before it is copied, if a load
  image isn't copied or `.bss` isn't zeroed in full, or if elfq can't
  follow code that may set one up; the JSON's `problems_certain` counts
  the problems it is sure of.
- elfq reads the built file, not the source: rebuild before asking again.
```

**Install** with Rust 1.88 or later:

```sh
cargo install --locked --git https://github.com/dpjungmin/elfq elfq
```

The `elfq` at the end is needed: the repository holds a second program, the
Rust sample in `testdata`. The `elfq` crate on crates.io is only a
placeholder for now.

**Status:** early development. The overview, `top`, `retained`, `why`,
`callers`, `callees` and `reach` work on x86-64 and Cortex-M executables,
with readable Rust and C++ names, and `inlined` works on any file with DWARF
debug info that isn't compressed. The overview, `callers`, `callees` and
`reach` also work on x86-64 shared libraries. `browse` prints x86-64 code
and the Thumb code of Cortex-M cores up to Armv7E-M (M0, M0+, M3, M4, M7);
for Armv8-M cores, such as the M33, its pages list only the lines that use
another item. A data object's page lists the pointers it holds, a vector
table's entries, or the strings of an object that holds only strings, and
the `objdump -s` command that prints its bytes. `diff` compares any two
builds of one program that have section headers, by block of memory.
`startup` reads Cortex-M reset code that copies and zeroes memory through
calls, such as to `memcpy` and `memset`, but not yet in loops of its own.
See [docs/design.md](docs/design.md) for how elfq works and what comes next,
[docs/survey.md](docs/survey.md) for how existing tools compare, and
[CONTRIBUTING.md](CONTRIBUTING.md) to build it.
