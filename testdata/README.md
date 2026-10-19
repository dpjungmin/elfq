# Sample binaries

Small programs built for x86-64 Linux and for Cortex-M microcontrollers,
used by elfq's tests and docs. Each function in them exists to create one
shape in the call graph or one DWARF pattern, so tests can check elfq against
known answers.

```
testdata/
├── build.sh     Rebuilds everything in bin/ from the sources.
├── bin/         The built files (committed).
├── c/           C samples: app.c plus a platform file per target, deep.c, plt.c and far.c.
├── cpp/         C++ sample: the kinds of names C++ compilers make.
└── rust/        Rust sample: the same program, plus Rust-only patterns.
```

## Files in bin/

| File | Language | Target |
|---|---|---|
| `c-x86_64*.elf` | C | x86-64 Linux, PIE, dynamically linked against glibc |
| `c-x86_64.so` | C | x86-64 Linux, the same code as a shared library |
| `c-plt.so` | C | x86-64 Linux shared library built without Intel CET, whose code calls its own functions through each kind of PLT stub |
| `c-cortex-m4*.elf` | C | Cortex-M4 bare metal, newlib-nano, `--gc-sections` |
| `c-cortex-m4-cmsis*.elf` | C | The same, laid out as CMSIS's linker script template does |
| `c-cortex-m7*.elf` | C | Cortex-M7 bare metal, with code and data in TCM, OCRAM and flash |
| `c-deep.elf` | C | x86-64 Linux, PIE, with calls and inlined code nested 20 deep |
| `far-*.elf` | C | Cortex-M0 and M4 bare metal, with calls too far for one branch instruction, linked by GNU ld or LLD |
| `cpp-x86_64*.elf` | C++ | x86-64 Linux, PIE, dynamically linked against glibc and libstdc++ |
| `cpp-x86_64.so` | C++ | x86-64 Linux, the same code as a shared library |
| `rust-x86_64*.elf` | Rust | x86-64 Linux, std |
| `rust-cortex-m4*.elf` | Rust | Cortex-M4 bare metal, `no_std` |

Each comes in up to three variants, matching what people usually have
(`c-deep.elf`, `c-x86_64.so`, `cpp-x86_64.so` and `c-plt.so` only in the
first, `c-cortex-m4-cmsis` and `far-*` only in the last two):

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

### Shared library sample

The C sample for x86-64 Linux again, built with `-shared -fPIC` as a
library. Another library loaded first may replace any function this one
exports, so its code calls its own exported functions through PLT stubs,
as it calls imports: `process_frame` calls `log_msg@plt`, which jumps to
`log_msg`. Only `static` functions, such as `crc32`, are called directly.

| Item | Expected answer |
|---|---|
| `log_msg` | The same callers as in `c-x86_64.elf`, all through `log_msg@plt`. |
| `log_msg@plt` | A 16-byte stub that jumps to `log_msg`. |
| `main` | Calls `app_tick`, `dispatch_event`, `process_frame`, `register_handler` and `report`, each through its stub. |

`cpp-x86_64.so` is the C++ sample built the same way, for the names of
these stubs in C++: `main` calls `app::log(unsigned int)@plt`, which jumps
to `app::log(unsigned int)`.

### PLT sample ([c/plt.c](c/plt.c))

A shared library built with `-fcf-protection=none`. Without Intel CET,
GNU ld puts no `endbr64` in PLT stubs, so there is no `.plt.sec`: `.plt`
holds a header and 16-byte stubs, and `.plt.got`, for functions whose
address the code also takes, holds 8-byte stubs.

| Item | Expected answer |
|---|---|
| `.plt` | A 16-byte header, then stubs for `twice`, `pick`, `square` and `sum_to`. |
| `.plt.got` | 8-byte stubs for `negate`, `__cxa_finalize` and `thrice`, in that order. |
| `run` | Calls `twice`, `thrice`, `negate`, `sum_to` and `pick`, each through its stub. |
| `pick` | Takes the addresses of `thrice` and `negate`. |
| `sum_to` | An ifunc. Its symbol shares an address with `sum_to_resolver`, which picks `sum_fast` or `sum_plain` when the library is loaded, and its stub's pointer has an `IRELATIVE` relocation to that address. |
| `square` | An ifunc that nothing calls. `pick_square` takes its address, which is the address of its stub. |

### Cortex-M7 sample ([c/platform_cortexm7.c](c/platform_cortexm7.c))

The C sample again, with a platform file and linker script
([c/cortex-m7.ld](c/cortex-m7.ld)) that spread it over several kinds of
memory, for looking at one range of memory at a time. The addresses are
those of the M7 core in an NXP i.MX RT1176, a common chip with tightly
coupled memory (TCM), but elfq knows nothing about the chip itself:

| Memory | Address | Holds |
|---|---|---|
| ITCM | `0x00000000` | `.itcm`: `SysTick_Handler`, `crc32`, `filter_tap`, `apply_filter` and the linker's veneers `__app_tick_veneer` and `__memcpy_veneer`, copied from flash at startup |
| DTCM | `0x20000000` | `.data`: `crc_table` and `stats`, copied from flash; `.bss`: the other variables |
| OCRAM | `0x20240000` | `.ocram_bss`: `tx_buffer`, the UART's 4 KiB buffer |
| Flash | `0x30000000` | `.isr_vector`, `.text`, `.rodata`, and the load images of `.itcm` and `.data` |

Calls between flash and ITCM are too far for one branch instruction, so the
linker adds veneers such as `__crc32_veneer`, which `process_frame` calls
and which jumps to `crc32`. `Reset_Handler` loads where `.itcm`, `.data`
and `.bss` start and end, but doesn't use `SysTick_Handler`, `crc_table` or
`config_sum`, which start at the same addresses.

Three things leave ITCM and DTCM, so `callees 0x0+256K 0x20000000+256K`
lists them: `apply_filter` calls `memcpy` and `SysTick_Handler` jumps to
`app_tick`, each through a veneer, and `filter_tap` reads `fir_coeffs`,
all in flash.

### CMSIS layout sample ([c/cortex-m4-cmsis.ld](c/cortex-m4-cmsis.ld))

The Cortex-M4 C sample again, linked the way CMSIS's `gcc_arm.ld` template
and the linker scripts based on it do: the vector table and the constants
are in `.text`, with the code between them. In the stripped file, elfq
finds the vector table at the start of `.text` and the constants after the
last function, and names the table `.text@0x8000000`.

### Far-call samples ([c/far.c](c/far.c))

Code in RAM at 0x0, as in a Cortex-M7's ITCM, calls code in flash at
0x30000000 and a routine in the chip's ROM at 0x1fff0000, which only the
linker script ([c/far.ld](c/far.ld)) names, and the other way. These are
further apart than one `bl` reaches, so the linker adds a veneer next to
each such call, except for `flash_far`, which GCC calls through a register
since it is declared `long_call`. The files differ in linker, core and
flags, which give these veneers. Two of LLD's have only unit tests: the one
for Cortex-M0 `-pie` files, and a lone `b.w` where the veneer is in reach
of the target:

| File | Linker and core | Veneer |
|---|---|---|
| `far-gnu-m4` | GNU ld, Cortex-M4 | `ldr.w pc, [pc]` and the target |
| `far-gnu-m4-pure` | GNU ld, Cortex-M4, `-mpure-code` | `movw ip`, `movt ip`, `bx ip` |
| `far-gnu-m4-pic` | GNU ld, Cortex-M4, `--pic-veneer` | adds pc to an offset in ip |
| `far-gnu-m0` | GNU ld, Cortex-M0 | loads the target into ip, `bx ip` |
| `far-lld-m4` | LLD, Cortex-M4 | `movw ip`, `movt ip`, `bx ip` |
| `far-lld-m4-pic` | LLD, Cortex-M4, `--pic-veneer` | adds pc to `movw`/`movt` in ip, as for `-pie` |
| `far-lld-m0` | LLD, Cortex-M0 | stores the target on the stack and pops it into pc |
| `far-lld-m0-pure` | LLD, Cortex-M0, `-mpure-code` | the same, with the target made a byte at a time |

GCC compiles all of them; LLD only links four. In each,
`callees 0x0+64K` lists the same calls out of RAM, each from the function
that makes it:

| Function in RAM | Expected answer |
|---|---|
| `ram_calls_flash` | Calls `flash_work`, via a veneer. |
| `ram_tails_flash` | Tail-calls `flash_tail`, via a veneer; calls it on Cortex-M0, which has no tail call this far. |
| `ram_calls_rom` | Calls 0x1fff0000, via a veneer: a call elfq can't follow, since the file has nothing there. |
| `ram_tails_rom` | Jumps to 0x1fff0000, via a veneer, since elfq can't tell whether a function starts there; calls it on Cortex-M0. |
| `ram_long_calls_flash` | Calls `flash_far`, through a register, with no veneer. |
| all of them | Use `counter`, except in `far-lld-m0-pure`, whose code makes its address a byte at a time, which elfq doesn't follow. |

`callers 0x0+64K` lists `Reset_Handler` and `flash_calls_ram`, which call
into RAM through veneers. The stripped files give the same rows, under
names like `fn_0x10`.

### Deep sample ([c/deep.c](c/deep.c))

Calls and inlined code nested deeper than `--tree` draws a tree in one
piece, which is 9 levels:

| Item | Shape | Expected answer |
|---|---|---|
| `chain1` to `chain20` | Each calls the next; `chain20` uses `chain_table`, which `run_steps` reads too. | `retained chain1 --tree` continues below twice, after `chain9` and `chain17`, and shares `chain_table`. `why chain_table` starts from `_start`, so it continues after `chain7` and `chain15`. |
| `stage1` to `stage20` | The same, but `stage1` to `stage19` also call a `check` function that only they use, so the tree branches at every level. | `retained stage1 --tree` continues below after `stage9` and `stage17`. `check17` moves below with `stage18`, since rows under a cut row move together. |
| `step1` to `step20` | Inlined into each other, and all into `run_steps`. | `inlined run_steps` continues below after `step9` and `step17`. |

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

This needs `gcc` and `g++`, `arm-none-eabi-gcc` with newlib, `ld.lld`, and
`rustup`, which installs the Rust version that `rust/rust-toolchain.toml`
names, with the `thumbv7em-none-eabihf` target. The committed files were
built with GCC 13.3.0 (Ubuntu 24.04), arm-none-eabi-gcc 13.2.1 with newlib
4.4.0, LLD 18.1.3, and rustc 1.97.0.

Builds are reproducible: the same toolchains give byte-identical files from
any checkout path. Other toolchain versions produce different addresses and
sizes, so commit rebuilt files together with any test expectations they
change, and only rebuild when a sample's source changes.
