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
elfq app.elf retained crc32 report      # what removing both at once would save
elfq app.elf retained 0x0+256K          # what removing everything in the first 256 KiB would save
elfq app.elf retained 'uart_*'          # what removing every item named uart_* would save
elfq app.elf why crc_table              # what keeps crc_table in the file
elfq app.elf inlined                    # which inlined functions take the most bytes
elfq app.elf inlined main               # the inlined code in main
elfq app.elf callers log_msg            # what calls or uses log_msg
elfq app.elf callees main               # what main calls or uses
elfq app.elf callees 0x0+256K           # what the first 256 KiB uses outside it
elfq app.elf callers 'uart_*'           # what uses the items named uart_* from outside them
elfq app.elf reach                      # where the program starts, and the bytes each start reaches
elfq app.elf reach SysTick_Handler      # what SysTick_Handler runs and uses, and where
elfq app.elf browse                     # where the program starts, each start a page to read
elfq app.elf browse log_msg             # read log_msg's code, what it calls and reaches, its callers, and what keeps it
elfq app.elf browse 'uart_*'            # the items named uart_*, to pick one to read
elfq old.elf diff new.elf               # what changed from old.elf to new.elf
elfq app.elf startup                    # whether the reset code copies and zeroes memory before it uses it
```

[docs/examples.md](docs/examples.md) asks real questions about the sample
binaries in `testdata/bin`, each with the command that answers it and what
elfq prints.

Every command but `browse` also prints JSON for scripts with `--json`, and
[docs/design.md](docs/design.md#json) lists the keys. `elfq FILE COMMAND
--help` says what a command takes: ranges of memory, patterns such as
`'uart_*'`, several symbols, `--from` or `--to`. `callers` and `callees` of
ranges exit with status 1 when something crosses the edge of the ranges, or
might, `reach SYMBOL --to RANGE` when SYMBOL reaches something in RANGE, or
might, `diff` when the two builds load different memory, and `startup` when
it finds a problem in the reset code, or might, so a CI job can run them as
checks. [docs/examples.md](docs/examples.md) shows each check, and how to
read the JSON with `jq`.

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
  `--all` shows every row, and `--json` has every row. `why`, `reach` and
  `inlined` also take a quoted pattern, such as `'process_f*'`, where `*`
  stands for any text, that matches one item, and answer for that item,
  with the JSON of its name; for a pattern that matches several, the
  error lists what to type for each.
- Each table row is one line, with the name last. Each name after "kept
  by" or "used by" works as SYMBOL; rows with ranges, `--to` or `--from`
  give whole names, which several items can share, so use an address from
  AT. A `why` row without "used by" is used by the row above, or starts a
  chain. Read `why`'s chains with `--json`.
- `elfq FILE.elf retained A B C` says what removing all of them at once
  would save, with the rows that only several of them keep, as in "stats,
  kept by A and B together", which their retained sizes added up would
  miss. In `--json`, `kept_together` counts those bytes, such a row's
  `kept_together_by` gives the addresses of the SYMBOLs it needs, and
  `symbols` lists the SYMBOLs, one or several, each with `row`, the `id`
  of its own row. `left_out` lists the functions whose end elfq doesn't
  know, such as `_init`, which it leaves out. One error lists every
  argument that fails. A range in place of SYMBOL stands for every item
  with a byte in it, so `elfq FILE.elf retained 0x0+256K` says what
  removing everything in ITCM would save; `--range` still picks which
  bytes count. A quoted pattern in place of SYMBOL, such as
  `'core::fmt::*'`, where `*` stands for any text, stands for every item
  it matches, by its name, the end of a longer path or another of its
  names; the notes say how many matched only by the end of a longer path,
  and where, and which matched only through another name, and in
  `--json`, `matches` says how each one matched.
- `elfq FILE.elf callees 0x0+256K` lists what the code and data in that
  range use outside it, and `callers 0x0+256K` what uses them from
  outside; both exit 1 if anything does or might. Patterns and several
  SYMBOLs, alone or with ranges, answer the same way, for the items they
  pick, even a pattern that matches one item; in `--json`, `symbols`
  lists the SYMBOLs given, `matches` says how each pattern matched, and
  `ranges` has only the ranges given. `callees --to RANGE` and
  `callers --from RANGE` keep only what is in RANGE, with any of these or
  one SYMBOL. With patterns, ranges, several SYMBOLs, `--to` or `--from`,
  both show every row.
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
  the problems it is sure of, and its `keep_out` lists, for each load
  image of code, the functions that must stay out of it, as they run
  before its copy.
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
calls, such as to `memcpy` and `memset`, in loops of its own, and through
tables of sections, as the startup code of CMSIS, MCUXpresso IDE and the
Pico SDK does; it was checked on what GCC builds, not Arm Compiler 6 or
IAR.
See [docs/design.md](docs/design.md) for how elfq works and what comes next,
[docs/survey.md](docs/survey.md) for how existing tools compare, and
[CONTRIBUTING.md](CONTRIBUTING.md) to build it.
