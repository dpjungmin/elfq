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
elfq app.elf                    # what's in app.elf, and what to ask next
elfq app.elf callers log_msg    # what calls or uses log_msg
elfq app.elf callees main       # what main calls or uses
```

**Status:** early development. The overview, `callers` and `callees` work on
x86-64; the questions above come next. See [docs/design.md](docs/design.md)
for the plan, [docs/survey.md](docs/survey.md) for how existing tools
compare, and [CONTRIBUTING.md](CONTRIBUTING.md) to build it.
