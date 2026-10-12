# Existing tools, and where elfq fits

elfq should not reimplement tools people already have. Before writing any
analysis code we ran the common ELF inspection tools against the sample
binaries in [`testdata/`](../testdata/README.md) and asked each the questions
elfq is meant to answer. This page records what we found.

All results below come from running the tool on the files in `testdata/bin`,
or on rebuilds of the same sources where a section says so. Tools marked "not
run" are described from their documentation. Versions: binutils 2.42, bloaty 1.1 (built from
source at f1a83e6), cargo-bloat 0.12.1, ddbug 0.6.1, twiggy 0.8.0,
puncover 0.8.0, radare2 5.5.0, GCC 13.3.

## The questions

1. **Direct callers and callees.** Who calls `log_msg`? What does
   `process_frame` call?
2. **Exclusive code.** Which functions are reached only through
   `process_frame`, so they would disappear with it? For the C sample the
   answer is `decode_header`, `crc32`, `apply_filter`, `filter_tap`, plus the
   `crc_table` and `fir_coeffs` data they read.
3. **Retained size.** How many bytes does the binary carry because of
   `process_frame`, counting everything only it keeps alive?
4. **Why is this here?** Which chains of calls and references from the entry
   point keep a function in the binary?
5. **Inlined code.** Which inlined functions make `process_frame` itself big,
   and which inlined functions cost the most bytes across the whole binary?

Each question also has to work on the binaries people actually have: built
without `-g`, stripped, for x86-64 or for a Cortex-M microcontroller.

## Summary

| Tool | Q1 callers/callees | Q2 exclusive | Q3 retained | Q4 why | Q5 inlined | Without DWARF | Stripped | Cortex-M |
|---|---|---|---|---|---|---|---|---|
| nm, readelf, size | no | no | no | no | no | yes | no | yes |
| objdump -d | by reading the listing | no | no | no | no | yes | listing only | yes |
| addr2line -i, llvm-symbolizer | no | no | no | no | one address at a time | no | no | yes |
| bloaty | no | no | no | no | per source line | yes | sections only | yes |
| cargo-bloat | no | no | no | no | no | yes | no | yes (Rust only) |
| ddbug | from DWARF call sites (misses some) | no | no | no | one function | no | no | yes |
| twiggy | callers via `paths`, only with `--emit-relocs` | yes, only with `--emit-relocs` | yes, only with `--emit-relocs` | yes, only with `--emit-relocs` | no | yes | no | partly |
| puncover | direct only | no | no | no | no | yes | no | ARM only |
| radare2 | yes | no | no | no | no | yes | yes | yes |
| gcc -fcallgraph-info | per translation unit | no | no | no | no | n/a (compile time) | n/a | yes |

The gap is questions 2 to 4 on ordinary binaries. The only tool that answers
them, twiggy, needs the binary to be relinked with `--emit-relocs` and still
gives wrong answers on real programs (details below). Question 1 is well
covered by radare2 and partly by others. Question 5 is half covered: ddbug
answers it for one function, and nothing ranks inlined functions across a
whole binary.

## Tool by tool

### nm, readelf, size, objdump

`nm -S --size-sort` and `readelf -s` give flat per-symbol sizes, `size -A`
gives per-section totals. `objdump -d` shows every call instruction, so you
can answer question 1 by searching the listing, but you have to know that
`jmp` can be a call too: in `c-x86_64.elf`, `on_button` reaches `log_msg`
only through `jmp 1610 <log_msg>`, and `app_tick` reaches `dispatch_event`
the same way. On a stripped file, `nm` prints `no symbols` and the listing has
no function boundaries.

### bloaty

`bloaty -d symbols` and `-d compileunits` give flat size breakdowns, and
`bloaty diff` compares two builds; these are excellent and elfq should not
redo them. `-d symbols,inlines --source-filter='^process_frame$'` splits
`process_frame` by source line (`c/app.c:189 95 bytes`, `c/app.c:225 62
bytes`, ...), which tells you where its bytes come from, but by line rather
than by inlined function, so you map lines back to functions yourself. On the
whole Rust Cortex-M binary, `-d inlines` spreads the code over 285 source
lines. There is no call graph, so no answer to questions 1 to 4. On a stripped
file it falls back to sections.

### cargo-bloat

Flat per-function and per-crate sizes for Rust projects, built through cargo.
On the Rust x86-64 sample it shows that 97% of `.text` is `std`, led by the
backtrace symbolizer (`gimli`, `addr2line`, `miniz_oxide`), but it cannot say
what pulls that code in or how much would go away without it. It says itself:
"numbers above are a result of guesswork".

### ddbug

Reads DWARF only. For one function it lists the inlined functions and their
sizes, which answers the first half of question 5 well:

```
$ ddbug -c function -f name=process_frame -p function-calls c-x86_64.elf
fn process_frame
	size: 532
	inlined functions:
		[0]	load_le32
		[74]	unpack_samples
		[164]	update_stats
```

`ddbug --bloat` lists each function with its callers, taken from the
compiler's DWARF call-site records. That misses edges the call graph needs:
on `c-x86_64.elf` it reports no caller for `parse_list`, `platform_write`,
`on_button`, `on_timer` or `on_data`, and does not list `app_tick` as a caller
of `dispatch_event` or `on_button` as a caller of `log_msg`. The three
handlers are reached only through function pointers, and every other missing
edge is a tail call. It does not rank inlined functions across the binary, and it has
nothing to work with when DWARF is absent.

### twiggy

twiggy is a size profiler for WebAssembly with exactly the queries we want:
`top`, `dominators` (retained size), `paths` (why is this here), `garbage`
and `monos`. Version 0.8.0 can also read ELF, which makes it the closest prior
art. Its ELF support builds the call graph from relocations, so:

- On the binaries as built (`c-x86_64.elf`), it prints "Couldn't find any
  relocations ... Try recompiling the binary with --emit-relocs" and
  `dominators` reports 33 of 34 items as unreachable.
- Relinked with `-Wl,--emit-relocs`, calls to `static` functions in the same
  section still carry no relocation, so `process_frame` retains only its own
  532 bytes and `decode_header`, `crc32`, `apply_filter` and `filter_tap` are
  reported unreachable.
- With both `--emit-relocs` and `-ffunction-sections`, the x86-64 C result is
  right: `process_frame` retains 2047 bytes, including `crc_table` under
  `crc32`. On the Cortex-M C build with the same flags, `garbage` still
  reports 1273 bytes as unreachable, including `crc_table`, `fir_coeffs`, the
  vector table and `SysTick_Handler`, all of which the firmware uses.
- On the Rust x86-64 sample (relinked with `--emit-relocs`) it reports 65% of
  the file as unreachable "garbage", including the std backtrace symbolizer,
  even though the linker's own `--gc-sections` kept that code because it is
  referenced.
- For relocations against sections, its ELF reader handles a few types per
  architecture and stops with `todo!` on any other type (twiggy-parser 0.8.0
  source, `object_parse/mod.rs`).

Most binaries people want to inspect, such as distribution packages, vendor
firmware or a release build from CI, were not linked with `--emit-relocs`, so
in practice these queries are unavailable for ELF today.

### puncover

A web UI for ARM GCC firmware: per-function size, stack usage (from `.su`
files), and direct callers and callees parsed from `arm-none-eabi-objdump`.
On `c-cortex-m4.elf` it lists `on_button` among the callers of `log_msg`, so
it counts tail calls. It shows the summed size of direct callees ("Callees
(5, Σ=232)") but nothing transitive or exclusive. Pointed at the x86-64 build
with `--gcc-tools-base /usr/bin/`, it shows no callers or callees at all.

### radare2

A reverse-engineering framework. After `aaa`, `axt sym.log_msg` lists every
reference including tail jumps (`[CODE] jmp`) and address-taken uses
(`dbg.main ... [DATA] lea rsi, [dbg.on_button]`), and it discovers functions
in stripped files (21 in `c-x86_64-stripped.elf`, 18 in
`c-cortex-m4-stripped.elf`, with names like `fcn.000011a0`). It is the best answer
to question 1 today. It has no size-oriented queries; questions 2 to 4 need
your own scripts over its graph. Ghidra, IDA, Binary Ninja and angr (not run)
are in the same position: they can answer anything with scripting, but none
offers these questions as a command.

### gcc -fcallgraph-info

GCC 10+ writes an exact call graph with stack usage per translation unit
(`app.ci`, 45 edges for `app.c`), including tail calls and calls the
compiler added (`log_msg` calling `strlen`). It needs a rebuild with GCC, one
file per compilation unit to merge, and it cannot see what the linker did:
which functions `--gc-sections` removed, or the code inside libraries such as
newlib's `memcpy`, which were not built with the flag.

### Not run

- **cargo-call-stack** (Rust, nightly only): whole-program call graph and
  worst-case stack usage for embedded Rust, built from LLVM IR during the
  build. Overlaps with a future elfq stack-depth feature, not with sizes.
- **Chromium SuperSize**, **linker map viewers** (amap, MapViewer): size
  attribution from map files, tied to a build system.
- **perf**, **callgrind**: runtime profilers. They measure time, not what the
  binary contains, and need the program to run.

## What this means for elfq

1. **Build the call graph from machine code, not relocations or DWARF.** That
   is what makes questions 2 to 4 work on binaries as shipped. It also sees
   tail calls (`jmp`, `b.w`), which the DWARF call-site records that ddbug
   reads left out, and calls into library code built without debug info.
2. **Track references, not only calls.** A function whose address is stored
   in a table or vtable is kept alive by that reference. Treating it as an
   edge avoids twiggy's false "garbage", and is the same rule the linker's
   `--gc-sections` uses, so retained size matches what removing the code
   would actually save.
3. **Know the real roots.** Besides the ELF entry point: `.init_array`,
   exported symbols, and on Cortex-M the vector table. Otherwise interrupt
   handlers look dead.
4. **Lead with questions 2 to 4**: `exclusive`, `retained`, and `why`. These
   are the commands nothing else offers for ELF.
5. **Keep `callers` and `callees`** because the other commands need them and
   they cost little, but they are not the reason elfq exists. radare2 already
   does them well.
6. **Narrow `inlined`.** ddbug already breaks down one function. elfq's
   version should rank inlined functions by their total size across the whole
   binary, and keep the per-function view as a filter on that list.
7. **Degrade honestly.** Without DWARF, use symbols. Without symbols, recover
   function boundaries from `.eh_frame` and from call targets. In
   `c-x86_64-stripped.elf`, `.eh_frame` describes 24 code ranges where the
   unstripped file has 27 function symbols; for the Rust sample it is 531
   against 557. Say which answers rest on heuristics.
