# Sample binaries

Small programs built for x86-64 Linux and for a Cortex-M4 microcontroller,
used by elfq's tests and docs. Each function in them exists to create one
shape in the call graph or one DWARF pattern, so tests can check elfq against
known answers.

```
testdata/
├── build.sh     Rebuilds everything in bin/ from the sources.
├── bin/         The built files (committed).
├── c/           C sample: app.c plus a platform file per target.
└── rust/        Rust sample: the same program, plus Rust-only patterns.
```

## Files in bin/

| File | Language | Target |
|---|---|---|
| `c-x86_64*.elf` | C | x86-64 Linux, PIE, dynamically linked against glibc |
| `c-cortex-m4*.elf` | C | Cortex-M4 bare metal, newlib-nano, `--gc-sections` |
| `rust-x86_64*.elf` | Rust | x86-64 Linux, std |
| `rust-cortex-m4*.elf` | Rust | Cortex-M4 bare metal, `no_std` |

Each comes in up to three variants, matching what people usually have:

| Suffix | Symbols | DWARF | Typical of |
|---|---|---|---|
| `.elf` | yes | yes | a debug-info build |
| `-nodebug.elf` | yes | no | a default release build |
| `-stripped.elf` | no | no | a shipped product |

`rust-x86_64.elf` (with DWARF) is not committed: the standard library's debug
info makes it over 4 MB. `build.sh` creates it and then deletes it.

All files are built with `-O2` (C) or `--release` (Rust). The x86-64 C build
uses Ubuntu's GCC defaults, so it has stack-protector calls
(`__stack_chk_fail`) and `endbr64` instructions.

## What each function demonstrates

### C sample ([c/app.c](c/app.c))

| Function | Shape | Expected answer |
|---|---|---|
| `process_frame` | The function under investigation. | Exclusive: `decode_header`, `crc32`, `crc_table`, `apply_filter`, `filter_tap`, `fir_coeffs`. Inlined into it: `load_le32`, `unpack_samples`, `update_stats`. |
| `crc32` | Reads a 1 KiB table (`crc_table`) that nothing else reads. | `crc_table` is retained by `crc32`. |
| `apply_filter`, `filter_tap` | A chain reachable only through `process_frame`. | Both exclusive to `process_frame`. On Cortex-M, `apply_filter` also calls `memcpy`, which is shared with `Reset_Handler`. |
| `log_msg` | Shared helper. | Callers: `process_frame`, `report`, `app_tick`, `on_button` (tail call), and `debug_dump` on x86-64. Calls `strlen` (added by the compiler) and `platform_write` (tail call). |
| `on_timer`, `on_button`, `on_data` | Reached only through the `handlers` table. | Address taken by `main`; called indirectly from `dispatch_event`. |
| `dispatch_event` | Indirect call through a table. | One indirect call site, whose target is unknown. Both builds make it an indirect tail jump (`jmp rax`, `bx r3`). |
| `app_tick` | Ends in a tail call to `dispatch_event`. | On Cortex-M it is also reached from `SysTick_Handler`. |
| `parse_value`, `parse_list` | Mutual recursion: a cycle. | `parse_value` reaches `parse_list` by a tail call. |
| `count_nodes` | Direct recursion: a self-loop. | |
| `report`, `format_u32` | `format_u32` is used only by `report`. | `format_u32` is exclusive to `report`. `append` is inlined into `report`. |
| `debug_dump` | Never called. | Unreachable on x86-64. Removed by `--gc-sections` on Cortex-M. |

On Cortex-M ([c/platform_cortexm.c](c/platform_cortexm.c)), `Reset_Handler`
is the entry point and `SysTick_Handler` is reached only through
`vector_table`, which is data. The compiler turned `Reset_Handler`'s copy and
clear loops into calls to `memcpy` and `memset`.

### Rust sample ([rust/src/main.rs](rust/src/main.rs))

It mirrors the C sample (`process_frame`, `crc32`, `apply_filter`,
`filter_tap`, `log_msg`, the handler table, `app_tick`, `count_nodes`,
`report`, `format_u32`) and adds:

| Item | Shape |
|---|---|
| `checksum::<u8>`, `checksum::<u16>`, `checksum::<u32>` | One generic function, three machine-code copies. |
| `<Gain as Stage>::run`, `<DcBlock as Stage>::run` | Called through trait objects from `process_frame`. Their vtables are data with no symbol, referenced from `app_main` (`objdump` labels one `HANDLERS+0x18`). |
| Panic paths | Slicing and indexing can panic, which pulls in `core::panicking` and parts of `core::fmt`. |
| `<u32 as Display>::fmt`, `<usize as Display>::fmt` | On Cortex-M both symbols name the same address. |
| `Reset`, `EXCEPTIONS` | Entry point and vector table, as in the C sample. |

On x86-64, most of the file is the standard library, led by the backtrace
symbolizer; that is a good case for `why` and `retained`.

## Rebuilding

```sh
./testdata/build.sh
```

This needs `gcc`, `arm-none-eabi-gcc` with newlib, and Rust with the
`thumbv7em-none-eabihf` target (`rustup target add thumbv7em-none-eabihf`).
The committed files were built with GCC 13.3.0 (Ubuntu 24.04),
arm-none-eabi-gcc 13.2.1 with newlib 4.4.0, and rustc 1.97.0.

Builds are reproducible: the same toolchains give byte-identical files from
any checkout path. Other toolchain versions produce different addresses and
sizes, so commit rebuilt files together with any test expectations they
change, and only rebuild when a sample's source changes.
