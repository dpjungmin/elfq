# elfq

elfq ("ELF query") answers questions about an ELF binary that existing tools
can't, such as:

- Which functions are reached only through `process_frame`?
- How many bytes would go away if `process_frame` did?
- Why is `core::fmt` in my firmware at all?

It is meant for x86-64 Linux programs and Cortex-M firmware alike, and for
binaries as they are shipped: with or without DWARF, even stripped.

**Status:** design stage, nothing to run yet.

- [docs/design.md](docs/design.md): commands, how the analysis works, code
  layout and the delivery plan.
- [docs/survey.md](docs/survey.md): what readelf, bloaty, ddbug, twiggy,
  radare2 and others already do, tested on the same binaries, and the gaps
  elfq fills.
- [testdata/](testdata/README.md): the sample programs and prebuilt ELF files
  the tests use.
