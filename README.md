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
elfq app.elf browse log_msg             # read log_msg's code, its calls and its callers
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
another item.
See [docs/design.md](docs/design.md) for how elfq works and what comes next,
[docs/survey.md](docs/survey.md) for how existing tools compare, and
[CONTRIBUTING.md](CONTRIBUTING.md) to build it.
