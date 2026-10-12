# Sample binaries

Small programs built for x86-64 Linux and for Cortex-M microcontrollers,
used by elfq's tests and docs. Each function in them exists to create one
shape in the call graph or one DWARF pattern, so tests can check elfq against
known answers.

```
testdata/
├── build.sh     Rebuilds everything in bin/ from the sources.
├── bin/         The built files (committed).
├── c/           C sample: app.c plus a platform file per target.
├── cpp/         C++ sample: the kinds of names C++ compilers make.
└── rust/        Rust sample: the same program, plus Rust-only patterns.
```

## Files in bin/

| File | Language | Target |
|---|---|---|
| `c-x86_64*.elf` | C | x86-64 Linux, PIE, dynamically linked against glibc |
| `c-cortex-m4*.elf` | C | Cortex-M4 bare metal, newlib-nano, `--gc-sections` |
| `c-cortex-m7.elf` | C | Cortex-M7 bare metal, with code and data in TCM, OCRAM and flash |
| `cpp-x86_64*.elf` | C++ | x86-64 Linux, PIE, dynamically linked against glibc and libstdc++ |
| `rust-x86_64*.elf` | Rust | x86-64 Linux, std |
| `rust-cortex-m4*.elf` | Rust | Cortex-M4 bare metal, `no_std` |

Each comes in up to three variants, matching what people usually have
(`c-cortex-m7.elf` only in the first):

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
| `process_frame` | The function under investigation. | Exclusive: `decode_header`, `crc32`, `crc_table`, `apply_filter`, `filter_tap`, `fir_coeffs`, and the strings `"bad header\n"` and `"bad crc\n"`, which have no symbols. Inlined into it: `load_le32`, `unpack_samples`, `update_stats`. On x86-64 the compiler merged `load_le32` into the code around it, so DWARF gives its copy no bytes. |
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

### Cortex-M7 sample ([c/platform_cortexm7.c](c/platform_cortexm7.c))

The C sample again, with a platform file and linker script
([c/cortex-m7.ld](c/cortex-m7.ld)) that spread it over several kinds of
memory, for looking at one range of memory at a time. The addresses are
those of the M7 core in an NXP i.MX RT1176, a common chip with tightly
coupled memory (TCM), but elfq knows nothing about the chip itself:

| Memory | Address | Holds |
|---|---|---|
| ITCM | `0x00000000` | `.itcm`: `SysTick_Handler`, `crc32`, `filter_tap`, `apply_filter`, copied from flash at startup |
| DTCM | `0x20000000` | `.data`: `crc_table` and `stats`, copied from flash; `.bss`: the other variables |
| OCRAM | `0x20240000` | `.ocram_bss`: `tx_buffer`, the UART's 4 KiB buffer |
| Flash | `0x30000000` | `.isr_vector`, `.text`, `.rodata`, and the load images of `.itcm` and `.data` |

Calls between flash and ITCM are too far for one branch instruction, so the
linker adds veneers such as `__crc32_veneer`, which `process_frame` calls
and which jumps to `crc32`. `Reset_Handler` loads where `.itcm`, `.data`
and `.bss` start and end, but doesn't use `SysTick_Handler`, `crc_table` or
`config_sum`, which start at the same addresses.

### C++ sample ([cpp/app.cpp](cpp/app.cpp))

A smaller program in `namespace app`, built for x86-64 only, since C++
names are the same on every target. Each part makes one kind of C++
symbol:

| Item | Shape | Expected answer |
|---|---|---|
| `app::log(char const*)`, `app::log(unsigned int)` | Two overloads of one name. | `log` alone is ambiguous; `log(unsigned int)` picks one. |
| `app::Gain::run`, `app::DcBlock::run` | Virtual functions, called through a `Stage&` in `process_frame`. | Each hangs under its class's vtable (`vtable for app::Gain`), which hangs under `main`, where the objects are made. The vtables point to `typeinfo for app::Gain`, which points to an import from libstdc++. |
| `app::(anonymous namespace)::crc32` | An anonymous namespace, holding `crc32` and `crc_table` as in the C sample. | Exclusive to `process_frame`, with `crc_table`, `frames_bad` and the strings `"bad header\n"` and `"bad crc\n"`. |
| `app::checksum<unsigned char>` and two more | One template, three machine-code copies. | `checksum` alone is ambiguous. |
| `app::operator<(app::Reading const&, app::Reading const&)` | An operator, whose name has an unpaired `<`. Called by `sort_readings`. | `operator<` finds it. |
| `app::setup_handlers()::{lambda(unsigned int)#1}::_FUN(unsigned int)` and `#2` | Lambdas stored as function pointers. | Reached only through `setup_handlers`, which takes their addresses. Inlined into those functions, their bodies have only the name `operator()` in DWARF, so `inlined` counts both on one row. |
| `app::Config::load_defaults()` | Called from the constructor of the global `app::config`, which GCC puts in `_GLOBAL__sub_I__ZN3app3logEPKc`, listed in `.init_array`. | Hangs under that function, a root. |
| `app::Frame::payload_length() const`, `app::Gain::Gain(short)` | Inlined into `process_frame` and `main`. | Shown by `inlined`. |

### Rust sample ([rust/src/main.rs](rust/src/main.rs))

It mirrors the C sample (`process_frame`, `crc32`, `apply_filter`,
`filter_tap`, `log_msg`, the handler table, `app_tick`, `count_nodes`,
`report`, `format_u32`) and adds:

| Item | Shape |
|---|---|
| `checksum::<u8>`, `checksum::<u16>`, `checksum::<u32>` | One generic function, three machine-code copies. |
| `<Gain as Stage>::run`, `<DcBlock as Stage>::run` | Called through trait objects from `process_frame`. Their vtables are referenced from `app_main`. On x86-64 the vtables have no symbol (`objdump` labels one `HANDLERS+0x18`), so elfq names them like `.data.rel.ro@0x52da8`; on Cortex-M they keep local `.Lanon` symbols. |
| Panic paths | Slicing and indexing can panic, which pulls in `core::panicking` and parts of `core::fmt`. |
| `<u32 as Display>::fmt`, `<usize as Display>::fmt` | On Cortex-M both symbols name the same address. |
| `Reset`, `EXCEPTIONS` | Entry point and vector table, as in the C sample. |

On x86-64, most of the file is the standard library, led by the backtrace
symbolizer; that is a good case for `top`, `why` and `retained`.

## Rebuilding

```sh
./testdata/build.sh
```

This needs `gcc` and `g++`, `arm-none-eabi-gcc` with newlib, and Rust with the
`thumbv7em-none-eabihf` target (`rustup target add thumbv7em-none-eabihf`).
The committed files were built with GCC 13.3.0 (Ubuntu 24.04),
arm-none-eabi-gcc 13.2.1 with newlib 4.4.0, and rustc 1.97.0.

Builds are reproducible: the same toolchains give byte-identical files from
any checkout path. Other toolchain versions produce different addresses and
sizes, so commit rebuilt files together with any test expectations they
change, and only rebuild when a sample's source changes.
