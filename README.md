# elfq

elfq ("ELF query") answers questions about an ELF binary that existing tools
can't, such as:

- Which functions are reached only through `process_frame`?
- How many bytes would go away if `process_frame` did?
- Why is `core::fmt` in my firmware at all?

It is meant for x86-64 Linux programs and Cortex-M firmware alike, and for
binaries as they are shipped: with or without DWARF, even stripped.

**elfq is fully vibe coded:** all of its code and documentation were written
by an AI (Claude).

```sh
elfq app.elf                            # what's in app.elf, and what to ask next
elfq app.elf top                        # what keeps the most bytes in app.elf
elfq app.elf retained process_frame     # what process_frame keeps in the file
elfq app.elf exclusive process_frame    # what only process_frame leads to
elfq app.elf why crc_table              # what keeps crc_table in the file
elfq app.elf inlined                    # which inlined functions take the most bytes
elfq app.elf callers log_msg            # what calls or uses log_msg
elfq app.elf callees main               # what main calls or uses
```

Every command also prints JSON for scripts with `--format json`.

**Status:** early development. The overview, `top`, `retained`, `exclusive`,
`why`, `callers` and `callees` work on x86-64 and Cortex-M executables, with
readable Rust and C++ names, and `inlined` works on any file with DWARF debug
info.
See [docs/design.md](docs/design.md) for the plan,
[docs/survey.md](docs/survey.md) for how existing tools compare, and
[CONTRIBUTING.md](CONTRIBUTING.md) to build it.
