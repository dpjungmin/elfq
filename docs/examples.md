# elfq by example

Questions about the sample binaries in [testdata/bin](../testdata/bin), each
with the command that answers it and what elfq prints for it. A test runs
every `$ elfq` example here and checks that elfq still prints exactly that.
[testdata/README.md](../testdata/README.md) describes the samples,
`elfq --help` the commands, and [design.md](design.md) how elfq works.

1. [Why is formatting code in Rust firmware?](#why-is-formatting-code-in-rust-firmware)
2. [Does the code in fast memory use slow memory?](#does-the-code-in-fast-memory-use-slow-memory)
3. [Where does the firmware start, and where does each start run?](#where-does-the-firmware-start-and-where-does-each-start-run)
4. [What does SysTick_Handler run and use, and where?](#what-does-systick_handler-run-and-use-and-where)
5. [Through which calls does SysTick_Handler reach the RAM at 0x20240000?](#through-which-calls-does-systick_handler-reach-the-ram-at-0x20240000)
6. [Does SysTick_Handler run or use anything outside TCM?](#does-systick_handler-run-or-use-anything-outside-tcm)
7. [What is in stripped firmware?](#what-is-in-stripped-firmware)
8. [Who calls a function, and how?](#who-calls-a-function-and-how)
9. [What would removing a function save?](#what-would-removing-a-function-save)
10. [Which inlined functions take the most bytes?](#which-inlined-functions-take-the-most-bytes)
11. [What changed between two builds?](#what-changed-between-two-builds)
12. [How do scripts read the answers?](#how-do-scripts-read-the-answers)

## Why is formatting code in Rust firmware?

Given only a file, elfq prints an overview of it:

```
$ elfq testdata/bin/rust-cortex-m4.elf
testdata/bin/rust-cortex-m4.elf: 32-bit Arm executable, with symbols and DWARF debug info
34 functions and 38 data objects, 8897 bytes of code and data

MEMORY                  SIZE  SECTIONS
0x08000000..0x080022bc  8892  .vector_table .text .rodata
0x20000000..0x20000020    32  .bss

RETAINED  OWN  ITEM
    8505  134  Reset
    8371  262  elfq_sample::app_main, kept by Reset
    5840  736  elfq_sample::process_frame, kept by app_main
    2374  234  <u32 as core::fmt::Display>::fmt (through a pointer), kept by process_frame
    1940  614  <core::fmt::Formatter>::pad_integral, kept by fmt
    1326       2 more, kept by pad_integral
     200  200  .Lanon.93bf5a77af7a20bca9fbd99cda9bbf33.187, kept by fmt
    2730       22 more, kept by process_frame
    2269       25 more, kept by app_main
     392       17 more

Try:
  elfq testdata/bin/rust-cortex-m4.elf --range 0x08000000..0x080022bc
  elfq testdata/bin/rust-cortex-m4.elf --range 0x20000000..0x20000020
  elfq testdata/bin/rust-cortex-m4.elf top
  elfq testdata/bin/rust-cortex-m4.elf top --tree
  elfq testdata/bin/rust-cortex-m4.elf retained Reset
  elfq testdata/bin/rust-cortex-m4.elf reach sys_tick
  elfq testdata/bin/rust-cortex-m4.elf why do_count_chars
  elfq testdata/bin/rust-cortex-m4.elf inlined
```

MEMORY lists the ranges of memory that the file's sections occupy: flash at
0x08000000 and RAM at 0x20000000. In the table, each item is a function or
data object. OWN counts its own bytes, and RETAINED adds the bytes of
everything that only it leads to: what removing it would save. "Kept by"
names the closest item that every path to it goes through.

`<u32 as core::fmt::Display>::fmt`, which turns a number into text, keeps
2374 bytes, over a quarter of the firmware, though the sample turns its
numbers into text with code of its own. "(through a pointer)" means that
nothing calls it directly, so it counts for the code that holds its address.
`why` shows the chains that lead to it from where the program starts:

```
$ elfq testdata/bin/rust-cortex-m4.elf why fmt
<u32 as core::fmt::Display>::fmt is used by 2 functions, through these chains:

HOW            ITEM
entry point    Reset
call           elfq_sample::app_main
call           elfq_sample::process_frame
call           core::slice::index::slice_index_fail
address taken  <u32 as core::fmt::Display>::fmt
call           elfq_sample::apply_filter, used by process_frame
call           elfq_sample::filter_tap
call           core::panicking::panic_bounds_check
address taken  <u32 as core::fmt::Display>::fmt

Removing elfq_sample::process_frame would remove <u32 as core::fmt::Display>::fmt too.
```

The end of a name is enough, as long as it picks one item. A row without
"used by" is used by the row above it, so the first chain runs from `Reset`
through `slice_index_fail` to `fmt`. The row ending "used by process_frame"
starts a second chain, which branches off the first at `process_frame` and
runs through `panic_bounds_check`. Both chains reach `fmt` through a panic:
`slice_index_fail` and `panic_bounds_check` take `fmt`'s address to put
numbers into their panic messages, but the sample's [panic
handler](../testdata/rust/src/main.rs) never prints the message.

`callers` lists what calls or uses an item, and where:

```
$ elfq testdata/bin/rust-cortex-m4.elf callers slice_index_fail
core::slice::index::slice_index_fail is used by 1 function:

HOW   AT                    CALLER
call  0x80008e4, 0x80008f4  elfq_sample::process_frame
```

AT gives the address of each call. `addr2line -i` traces each one, through
the slicing code inlined from core, to lines 230 and 231 of
[main.rs](../testdata/rust/src/main.rs): two slices of a frame that panic
when the frame is shorter than its header says. `callers panic_bounds_check`
gives one address, which `addr2line -i` traces to `x[i - k]` in
`filter_tap`. Nothing else uses `fmt` or calls these two panics, so if the
compiler left out the bounds checks at all three places, `fmt` would go. The
compiler keeps the check in `filter_tap` although that check never fails:
`apply_filter` only passes an `i` below the length of `x`, but the compiler
doesn't know that inside `filter_tap`, which it doesn't inline. `retained`
shows what would go with `fmt`:

```
$ elfq testdata/bin/rust-cortex-m4.elf retained fmt
Removing <u32 as core::fmt::Display>::fmt would save 2374 bytes:

RETAINED   OWN  ITEM
    2374   234  <u32 as core::fmt::Display>::fmt (through a pointer)
    1940   614  <core::fmt::Formatter>::pad_integral, kept by fmt
    1258  1258  core::str::count::do_count_chars, kept by pad_integral
      68    68  <core::fmt::Formatter>::pad_integral::write_prefix, kept by pad_integral
     200   200  .Lanon.93bf5a77af7a20bca9fbd99cda9bbf33.187, kept by fmt

<core::fmt::Formatter>::pad_integral and <core::fmt::Formatter>::pad_integral::write_prefix also call and jump through pointers, so what they reach may count elsewhere.
```

The last line warns that two of these functions also call through pointers,
which elfq can't follow, so what those calls reach may be counted under
other items.

## Does the code in fast memory use slow memory?

The Cortex-M7 sample is laid out as on an NXP i.MX RT1176: ITCM for code at
0x0 and DTCM for data at 0x20000000, both fast memory next to the core, more
RAM at 0x20240000, and external flash at 0x30000000. Its startup code copies
the busiest functions from flash into ITCM:

```
$ elfq testdata/bin/c-cortex-m7.elf
testdata/bin/c-cortex-m7.elf: 32-bit Arm executable, with symbols and DWARF debug info
29 functions and 34 data objects, 7835 bytes of code and data

MEMORY                  SIZE  SECTIONS
0x00000000..0x000000e0   224  .itcm
0x20000000..0x20000450  1104  .data .bss
0x20240000..0x20241000  4096  .ocram_bss
0x30000000..0x30000edc  3804  .isr_vector .text .rodata .ARM.exidx, load images of .itcm .data

RETAINED   OWN  ITEM
    4442    26  log_msg
    4200    96  platform_write, kept by log_msg
    4096  4096  tx_buffer, kept by platform_write
       8        2 more, kept by platform_write
     216   216  strlen, kept by log_msg
    3195   112  Reset_Handler
    2613   132  main, kept by Reset_Handler
    2481        44 more, kept by main
     470        2 more, kept by Reset_Handler
     198        9 more

Try:
  elfq testdata/bin/c-cortex-m7.elf --range 0x00000000..0x000000e0
  elfq testdata/bin/c-cortex-m7.elf --range 0x20000000..0x20000450
  elfq testdata/bin/c-cortex-m7.elf --range 0x20240000..0x20241000
  elfq testdata/bin/c-cortex-m7.elf --range 0x30000000..0x30000edc
  elfq testdata/bin/c-cortex-m7.elf top
  elfq testdata/bin/c-cortex-m7.elf top --tree
  elfq testdata/bin/c-cortex-m7.elf retained main
  elfq testdata/bin/c-cortex-m7.elf reach SysTick_Handler
  elfq testdata/bin/c-cortex-m7.elf why tx_buffer
  elfq testdata/bin/c-cortex-m7.elf inlined
```

The last MEMORY row holds the load images of `.itcm` and `.data`: the copies
in flash that the startup code copies into ITCM and DTCM. The Try list has a
`--range` command for each row. `--range` counts only the bytes in one range
of memory, such as ITCM's 256 KiB from 0x0:

```
$ elfq testdata/bin/c-cortex-m7.elf top --range 0x0+256K
224 bytes of code in 0x00000000..0x00040000 (.itcm), by what keeps them:

RETAINED  OWN  ITEM
     200    0  Reset_Handler
     200    0  main, kept by Reset_Handler
     200    0  process_frame, kept by main
     152    0  __apply_filter_veneer, kept by process_frame
     152   92  apply_filter, kept by __apply_filter_veneer
      52   52  filter_tap, kept by apply_filter
       8    8  __memcpy_veneer, kept by apply_filter
      48    0  __crc32_veneer, kept by process_frame
      48   48  crc32, kept by __crc32_veneer
      24   16  SysTick_Handler
       8    8  __app_tick_veneer, kept by SysTick_Handler
```

`Reset_Handler`, `main` and `process_frame` are in flash, so they own none
of these bytes, but they keep 200 of the 224 through the functions they
call, and `SysTick_Handler` keeps the rest. The items whose names end in
`_veneer` are veneers, which the linker adds where a call's target is too
far away for the call instruction, as between flash and ITCM. Here each is
in the same memory as the code that calls it, so the two that
`process_frame` calls are in flash and own none of these bytes.

With ranges in place of a name, `callees` lists only what the code and data
in them use outside them. Given both ITCM and DTCM:

```
$ elfq testdata/bin/c-cortex-m7.elf callees 0x0+256K 0x20000000+256K
0x00000000..0x00040000 (.itcm) and 0x20000000..0x20040000 (.data and .bss) use 3 items outside them:

ADDRESS     SIZE  HOW                   AT    CALLEE
0x3000087c   308  call via veneer       0xbc  memcpy, used by apply_filter
0x300002e4    60  tail call via veneer  0x8   app_tick, used by SysTick_Handler
0x30000eb4    32  uses                  0x56  fir_coeffs, used by filter_tap
```

Each row names the item outside, with its ADDRESS and SIZE, then the item
inside that uses it. All three are in flash: `apply_filter` calls `memcpy`,
`filter_tap` reads its coefficients from `fir_coeffs`, and
`SysTick_Handler`, the timer's interrupt handler, ends with a jump to
`app_tick`. elfq exits with status 1, as here, when something leaves the
ranges or might, and 0 when nothing does. Once nothing does, a CI job can
run this command to keep it that way. `callers` asks the reverse:

```
$ elfq testdata/bin/c-cortex-m7.elf callers 0x0+256K
0x00000000..0x00040000 (.itcm) is used by 2 items outside it:

ADDRESS     SIZE  HOW              AT                      CALLER
0x30000320   376  call via veneer  0x3000034c              process_frame, uses crc32
0x30000320   376  call via veneer  0x30000386, 0x30000454  process_frame, uses apply_filter
0x30000000    64  pointer          0x3000003c              vector_table, uses SysTick_Handler
```

`process_frame`, in flash, calls `crc32` and `apply_filter` through veneers
from three places, and the vector table holds the address of
`SysTick_Handler`.

`--from RANGE` lists only the callers in RANGE, and `--to RANGE` only the
callees. DTCM is used by 11 functions, most of them in flash; this asks
which of them are in ITCM:

```
$ elfq testdata/bin/c-cortex-m7.elf callers 0x20000000+256K --from 0x0+256K
0x20000000..0x20040000 (.data and .bss) is used by 2 functions in 0x00000000..0x00040000 (.itcm):

ADDRESS  SIZE  HOW   AT    CALLER
0x10       48  uses  0x18  crc32, uses crc_table
0x0        16  uses  0x0   SysTick_Handler, uses systick_count
```

`crc32` reads its table from DTCM, and `SysTick_Handler` counts the timer's
ticks there. `callees 0x0+256K --to 0x20000000+256K` lists the same two
uses, with the data in DTCM first. Either command also takes a name in
place of the ranges, and then asks about the range that the item occupies:

```
$ elfq testdata/bin/c-cortex-m7.elf callees process_frame --to 0x0+256K
process_frame uses 2 functions in 0x00000000..0x00040000 (.itcm):

ADDRESS  SIZE  HOW              AT                      CALLEE
0x74       92  call via veneer  0x30000386, 0x30000454  apply_filter, used by process_frame
0x10       48  call via veneer  0x3000034c              crc32, used by process_frame
```

`process_frame`, in flash, calls two functions in ITCM, both through
veneers.

## Where does the firmware start, and where does each start run?

The processor starts the firmware at its reset handler, and runs a
handler for each exception and interrupt from the vector table. `reach`
without a symbol lists those starts, and `main`, with the bytes of code
and data that each reaches in each block of memory, as the overview lists
the blocks:

```
$ elfq testdata/bin/c-cortex-m7.elf reach
Where testdata/bin/c-cortex-m7.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2071  main function              main (may reach more)
       200        1100        4096        2345  Reset (entry point)        Reset_Handler (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  Default_Handler
        24          24        4096         430  SysTick                    SysTick_Handler (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
```

`Reset_Handler` calls `main`, so it reaches everything that `main` does,
and 274 bytes more in flash: its own 112, and the 162 of `memset`, which
it calls to clear `.bss`. `Default_Handler`, which only loops, handles
NMI, HardFault and six more exceptions, which `--json` names.
`SysTick_Handler`, the timer's interrupt handler, is the one to look at:
it starts in ITCM, but 430 of the bytes it reaches are in flash. With
`--json`, each block has its code and data apart.

## What does SysTick_Handler run and use, and where?

An interrupt handler should run from fast memory, and so should what it
calls and the data it uses. `reach` lists all of that, at any depth, and
adds up its bytes in each block of memory:

```
$ elfq testdata/bin/c-cortex-m7.elf reach SysTick_Handler
SysTick_Handler and the 12 items it reaches take 446 bytes of code and 4128 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    24     0  .itcm
0x20000000..0x20000450     0    24  .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc   422     8  .text .rodata

ADDRESS     SIZE  HOW                   ITEM
0x0           16                        SysTick_Handler
0xd0           8  tail call             __app_tick_veneer, used by SysTick_Handler
0x20000438    12  uses                  handlers, used by dispatch_event
0x20000444     4  uses                  systick_count, used by SysTick_Handler
0x2000044c     4  uses                  tx_head, used by platform_write
0x20000448     4  uses                  tx_tail, used by platform_write
0x20240000  4096  uses                  tx_buffer, used by platform_write
0x30000700   216  call                  strlen, used by log_msg
0x30000668    96  tail call             platform_write, used by log_msg
0x300002e4    60  tail call via veneer  app_tick, used by SysTick_Handler
0x30000288    26  call                  log_msg, used by app_tick
0x300002cc    24  tail call             dispatch_event, used by app_tick
0x30000de8    ~8  uses                  .rodata@0x30000de8, used by app_tick

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so SysTick_Handler may reach more.
```

`SysTick_Handler`, the timer's interrupt handler, is in ITCM, but 422 of
the 446 bytes of code it runs are in flash: it jumps to `app_tick` there
through a veneer, and `app_tick` logs through `log_msg`, `strlen` and
`platform_write`. Its data is in DTCM, but for `tx_buffer`, the 4 KiB that
`platform_write` fills in the RAM at 0x20240000, and the 8 bytes in flash
of the message that `app_tick` logs. Each row names the item that uses it,
on the shortest way there from `SysTick_Handler`.
`dispatch_event` jumps through `handlers`, a table that the program fills
in as it runs, so the handler may run more than elfq can see, as the last
line says.

## Through which calls does SysTick_Handler reach the RAM at 0x20240000?

`--tree` draws what `reach` lists as a tree, with each item under the item
that uses it, so every chain from the handler reads from top to bottom:

```
$ elfq testdata/bin/c-cortex-m7.elf reach SysTick_Handler --tree
SysTick_Handler and the 12 items it reaches take 446 bytes of code and 4128 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    24     0  .itcm
0x20000000..0x20000450     0    24  .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc   422     8  .text .rodata

ADDRESS     SIZE  HOW        ITEM
0x0           16             SysTick_Handler
0x20000444     4  uses       ├─ systick_count
0xd0           8  tail call  └─ __app_tick_veneer
0x300002e4    60  tail call     └─ app_tick
0x300002cc    24  tail call        ├─ dispatch_event
0x20000438    12  uses             │  └─ handlers
0x30000de8    ~8  uses             ├─ .rodata@0x30000de8
0x30000288    26  call             └─ log_msg
0x30000700   216  call                ├─ strlen
0x30000668    96  tail call           └─ platform_write
0x2000044c     4  uses                   ├─ tx_head
0x20240000  4096  uses                   ├─ tx_buffer
0x20000448     4  uses                   └─ tx_tail

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so SysTick_Handler may reach more.
```

The handler jumps through the veneer at 0xd0 to `app_tick` in flash, which
calls `log_msg`, which jumps to `platform_write`, which fills `tx_buffer`.
The veneer is a step of its own here, where the rows by block list
`app_tick` as a "tail call via veneer" from the handler.

## Does SysTick_Handler run or use anything outside TCM?

A CI job can check that a handler stays in fast memory. `--to` keeps only
what `reach` finds in the ranges it gives, here every block that the
overview lists other than ITCM and DTCM, and the exit status is 1 when it
finds anything there:

```
$ elfq testdata/bin/c-cortex-m7.elf reach SysTick_Handler --to 0x20240000+1M --to 0x30000000+16M
SysTick_Handler reaches 7 items in 0x20240000..0x20340000 (.ocram_bss) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which take 422 bytes of code and 4104 bytes of data; elfq can't follow 1 jump, which may go there:

ADDRESS     SIZE  HOW                   AT                      ITEM
                  jump                  0x300002da              (through a pointer), used by dispatch_event
0x20240000  4096  uses                  0x30000674, 0x30000696  tx_buffer, used by platform_write
0x30000700   216  call                  0x30000292              strlen, used by log_msg
0x30000668    96  tail call             0x3000029e              platform_write, used by log_msg
0x300002e4    60  tail call via veneer  0x8                     app_tick, used by SysTick_Handler
0x30000288    26  call                  0x30000304              log_msg, used by app_tick
0x300002cc    24  tail call             0x300002fe, 0x30000310  dispatch_event, used by app_tick
0x30000de8    ~8  uses                  0x30000302              .rodata@0x30000de8, used by app_tick
```

Each row names the code to change: for `app_tick`, the handler itself, at
0x8, where it jumps through the veneer to flash. `callees SysTick_Handler`
with the same ranges lists that row too, but stops there, where `reach`
follows `app_tick` on to `log_msg`, `platform_write` and `tx_buffer`. The
jump through a pointer in `dispatch_event` stays, since flash holds
functions whose addresses `main` takes. `crc32`, which runs from ITCM and
uses only DTCM, passes with exit status 0:

```
$ elfq testdata/bin/c-cortex-m7.elf reach crc32 --to 0x20240000+1M --to 0x30000000+16M
crc32 reaches nothing in 0x20240000..0x20340000 (.ocram_bss) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx).
```

## What is in stripped firmware?

Shipped firmware often has no symbols. elfq still finds its functions, from
the vector table, the calls between them and the addresses of functions that
code loads or data holds. It names each item it finds after where it is, as
`fn_0x8000518`, `.rodata@0x80009e4` or the vector table's `.isr_vector`,
except the entry point, `entry`:

```
$ elfq testdata/bin/c-cortex-m4-stripped.elf
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
25 functions and 27 data objects, 3586 bytes of code and data
Items without symbols are named by address, like fn_0x8000040 and .rodata@0x80008f0.

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000e0c  3596  .isr_vector .text .rodata .ARM.exidx, load image of .data
0x20000000..0x20000048    72  .data .bss

RETAINED    OWN  ITEM
    3120    ~76  entry
    2574   ~132  fn_0x8000518, kept by entry
    1684   ~376  fn_0x80002c8, kept by fn_0x8000518
    1076    ~52  fn_0x8000060, kept by fn_0x80002c8
    1024  ~1024  .rodata@0x80009e4, kept by fn_0x8000060
     232         6 more, kept by fn_0x80002c8
     758         28 more, kept by fn_0x8000518
     470         2 more, kept by entry
     466         11 more

Try:
  elfq testdata/bin/c-cortex-m4-stripped.elf --range 0x08000000..0x08000e0c
  elfq testdata/bin/c-cortex-m4-stripped.elf --range 0x20000000..0x20000048
  elfq testdata/bin/c-cortex-m4-stripped.elf top
  elfq testdata/bin/c-cortex-m4-stripped.elf top --tree
  elfq testdata/bin/c-cortex-m4-stripped.elf retained entry
  elfq testdata/bin/c-cortex-m4-stripped.elf reach fn_0x80005ec
  elfq testdata/bin/c-cortex-m4-stripped.elf why .rodata@0x80009e4
```

`entry` is the reset handler. Sizes marked ~ are estimates, since no symbol
records them. `fn_0x8000060` keeps 1 KiB of read-only data, and `why` shows
the chain that leads to it:

```
$ elfq testdata/bin/c-cortex-m4-stripped.elf why .rodata@0x80009e4
.rodata@0x80009e4 is used by fn_0x8000060, through this chain:

HOW          ITEM
entry point  entry
call         fn_0x8000518
call         fn_0x80002c8
call         fn_0x8000060
uses         .rodata@0x80009e4

Removing fn_0x8000060 would remove .rodata@0x80009e4 too.
```

These names work wherever a name does. In the same build with symbols,
`c-cortex-m4.elf`, this chain reads `Reset_Handler`, `main`,
`process_frame`, `crc32` and `crc_table`.

## Who calls a function, and how?

`callers` lists what calls or uses an item, and `callees` what it calls or
uses. In the C sample for x86-64:

```
$ elfq testdata/bin/c-x86_64.elf callers log_msg
log_msg is used by 5 functions:

HOW        AT              CALLER
call       0x16e7          app_tick
call       0x1aaf          debug_dump
tail call  0x1661          on_button
call       0x18ee, 0x1908  process_frame
call       0x1a72          report
```

HOW says how each caller uses it: `on_button` ends with a jump to `log_msg`,
a tail call, so `log_msg` returns straight to whatever called `on_button`.
AT has the address of each call: `process_frame` calls `log_msg` in two
places, for a bad header and for a bad CRC.

```
$ elfq testdata/bin/c-x86_64.elf callees dispatch_event
dispatch_event uses 1 data object:

HOW   AT      CALLEE
uses  0x169b  handlers

Also 1 jump through a pointer, at 0x16ad, which elfq can't follow.
```

`dispatch_event` ends by jumping to a handler that it reads from `handlers`,
and elfq can't tell which one: the line under the table says so. `callers`
shows where such pointers come from:

```
$ elfq testdata/bin/c-x86_64.elf callers on_timer
on_timer is used by 1 function:

HOW            AT      CALLER
address taken  0x10a8  main

Calls to on_timer through a pointer aren't listed, since elfq can't see them.
```

`main` takes the address of `on_timer` and passes it to `register_handler`,
which stores it in `handlers`. C++ names come with their namespaces and
parameters, but the end of a name is enough without them:

```
$ elfq testdata/bin/cpp-x86_64.elf callers crc32
app::(anonymous namespace)::crc32(unsigned char const*, unsigned long) is used by 1 function:

HOW   AT      CALLER
call  0x1608  app::process_frame(app::Frame const&, app::Stage&)
```

## What would removing a function save?

```
$ elfq testdata/bin/c-x86_64.elf retained process_frame --tree
Removing process_frame would save 2068 bytes:

RETAINED   OWN  ITEM
    2068   532  process_frame
    1083    59  ├─ crc32
    1024  1024  │  └─ crc_table
     378   271  ├─ apply_filter
     107    75  │  └─ filter_tap
      32    32  │     └─ fir_coeffs
      54    54  ├─ decode_header
      12   ~12  ├─ .rodata@0x2012
       9    ~9  └─ .rodata@0x201e

Shared with other code, so not counted: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
```

The rows are the items that only `process_frame` leads to, which would go
with it. `--tree` draws each under the item that keeps it; without it, each
row ends with ", kept by" and that item instead, which still reads when a
terminal wraps long lines, or when `grep` picks out one row. The last line
lists what `process_frame` uses that other code uses too, which would stay.

## Which inlined functions take the most bytes?

```
$ elfq testdata/bin/c-x86_64.elf inlined
Inlined copies of 5 functions take 420 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
  164       1  update_stats
  150       6  append
   74       1  unpack_samples
   32       1  saturate
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
```

`inlined` reads the DWARF debug info, so it needs a file built with it.
BYTES counts what all the copies of a function take, and COPIES how many
there are: `append` has 6 copies, all in `report`. `inlined SYMBOL` lists
the copies inside one function.

## What changed between two builds?

`diff` compares two builds of one program by the memory they load, and
their functions and data objects. In `c-cortex-m7-moved.elf`, `filter_tap`
is no longer in ITCM, and `crc32` logs an empty frame:

```
$ elfq testdata/bin/c-cortex-m7.elf diff testdata/bin/c-cortex-m7-moved.elf
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-moved.elf: 1 moved, 3 new, 1 grew.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   208     -16  .itcm -16
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000eec  3804  3820     +16  load image of .itcm -16, .rodata +16

OLD  NEW  FROM  TO          MOVED
 52   52  0x40  0x30000188  filter_tap (.itcm to .text), kept by __filter_tap_veneer

CHANGE  OLD  NEW  ADDRESS     ITEM
   +16   48   64  0x6c        crc32, kept by __crc32_veneer
    +8         8  0xb0        __filter_tap_veneer (new), kept by apply_filter
    +8         8  0xb8        __log_msg_veneer (new), kept by crc32
   +16       ~16  0x30000de0  .rodata@0x30000de0 (new), kept by crc32

Calls between blocks, new or gone:

STATUS  ADDRESS     SIZE  HOW              AT    CALLEE
new     0x30000188    52  call via veneer  0x36  filter_tap, used by apply_filter
new     0x300002bc    26  call via veneer  0x9a  log_msg, used by crc32

Other uses between blocks: 1 new and 1 gone; --json lists them.

61 other items are the same.

Try:
  elfq testdata/bin/c-cortex-m7-moved.elf why __filter_tap_veneer
```

The blocks are the overview's, worked out over both files, so that both
have the same blocks. ITCM is 16 bytes smaller: `filter_tap` moved to
flash, `crc32` grew, and the linker added veneers for the calls from ITCM
to `filter_tap` and `log_msg` in flash, each kept by its caller. Flash
holds ITCM's load image, which shrank with it, and the new message in
`.rodata`. Each row ends with what keeps the item, as `top`'s rows do, in
the new build, or for a gone item, in the old. Other sections' bytes
changed too, as the vector table points to code that moved, but the rows
say why. An item whose bytes changed only where they hold the address of
something that moved, or the distance to it, as in `apply_filter`'s call
to `filter_tap`, is the same. The exit status is 1.

The two calls from ITCM to flash are new, as `elfq
testdata/bin/c-cortex-m7-moved.elf callees 0x0..0xe0` lists them. So is
`crc32`'s use of its message in flash, and `filter_tap`'s use of
`fir_coeffs`, in flash too, is gone, as `filter_tap` is there now: the
JSON lists these uses of data. A CI job that should fail when code starts
calling across blocks checks the JSON with `jq`:

```sh
elfq old.elf diff new.elf --json > diff.json || [ $? -eq 1 ]
jq -e '(.same_bytes or .items_compared) and ([.crossings[] | select(.status=="new" and (.how=="call" or .how=="tail call"))] | length == 0)' diff.json
```

On this pair, `jq` prints `false` and exits 1, for the two new calls.
Uses of data don't count: the next pair moves data to another memory,
which makes every use of it from another block gone and new. Nor do
pointers: elfq reads any word of data that holds an address as one, so
a constant in a table that happens to equal a function's address is
listed as a pointer. A call between blocks that only changes how it is
made, as when it becomes a tail call or stops being one, or gains or
loses a veneer in the caller's or the callee's block, is neither new nor
gone. The check does fail on each call between blocks whose code at
either end moves to a third block, as when code that calls flash moves
from OCRAM into ITCM, or when a relink moves it to another block, as
when flash starts at 0x60000000 rather than 0x30000000, since diff lists
such a call as gone, and as new between the blocks it is between now. A
smaller relink, which leaves the two builds' flash overlapping or less
than 64 KiB apart, as a 32 KiB bootloader offset does, keeps one block,
and its calls stay the same. Both builds need a symbol table: without
one, diff compares no items, so the check fails unless the memory is the
same.

Moving `.data` and `.bss` from DTCM to RAM at 0x80000000 leaves a block
that only the old build has and one that only the new one has, and every
item in them moved, so they are one row. The code in ITCM and flash didn't
change size, but its bytes did, since it holds the addresses of the data:

```
$ elfq testdata/bin/c-cortex-m7.elf diff testdata/bin/c-cortex-m7-sdram.elf
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-sdram.elf: 12 moved, 1 edited.

MEMORY                          OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0          224   224       0  bytes differ
0x20000000..0x20000450 (gone)  1104     -   -1104  .data .bss
0x20240000..0x20241000         4096  4096       0
0x30000000..0x30000edc         3804  3804       0  bytes differ
0x80000000..0x80000450 (new)      -  1104   +1104  .data .bss

 OLD   NEW  FROM        TO          MOVED
1104  1104  0x20000000  0x80000000  all 12 items of .data and .bss

CHANGE  OLD  NEW  ADDRESS     ITEM
     0   64   64  0x30000000  vector_table (edited)

No call between blocks is new or gone.
Other uses between blocks: 17 new and 17 gone; --json lists them.

50 other items are the same.
```

`--all` lists each of the 12. The code holds the data's new addresses
where it held the old ones, so it is the same, but the vector table is
edited: its first word, the stack pointer that the processor starts
with, went from 0x20040000, the end of DTCM, to 0x80040000; neither is
the start or end of a section, so diff can't tell that the two
correspond. Every use of the data from the code now reaches the new
block at 0x80000000, so each is gone and new, but no call is, and the
`jq` check above prints `true` and exits 0. A function that the compiler
inlined everywhere is gone, and the new build's DWARF says where it
went:

```
$ elfq testdata/bin/c-cortex-m7.elf diff testdata/bin/c-cortex-m7-inlined.elf
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-inlined.elf: 1 gone, 1 edited.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   224       0  bytes differ
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000edc  3804  3740     -64  .text -64

CHANGE  OLD  NEW  ADDRESS     ITEM
   -40   40       0x30000160  decode_header (gone, inlined into process_frame)
     0  376  376  0x300002f8  process_frame (edited), kept by main

No call between blocks is new or gone.

61 other items are the same.
```

`process_frame` holds `decode_header`'s code now, at the same size, so
it is edited: an item of the same size, in the same block, whose bytes
changed other than where they say where something is, or that now uses
different items than before. A constant changed in place is an edit too,
as when the number that `decode_header` compares a frame with and the
last of `fir_coeffs` change:

```
$ elfq testdata/bin/c-cortex-m7.elf diff testdata/bin/c-cortex-m7-edited.elf
From testdata/bin/c-cortex-m7.elf to testdata/bin/c-cortex-m7-edited.elf: 2 edited.

MEMORY                   OLD   NEW  CHANGE  SECTIONS
0x00000000..0x000000e0   224   224       0
0x20000000..0x20000450  1104  1104       0
0x20240000..0x20241000  4096  4096       0
0x30000000..0x30000edc  3804  3804       0  bytes differ

CHANGE  OLD  NEW  ADDRESS     ITEM
     0   40   40  0x30000160  decode_header (edited), kept by process_frame
     0   32   32  0x30000eb4  fir_coeffs (edited), kept by filter_tap

No call between blocks is new or gone.

61 other items are the same.
```

Edited rows come last in their block, and `--json` gives them the status
`edited`.

Symbols, debug info and where the file keeps its bytes don't count, so a
build and its stripped copy load the same memory, and the exit status is 0:

```
$ elfq testdata/bin/c-cortex-m7.elf diff testdata/bin/c-cortex-m7-stripped.elf
testdata/bin/c-cortex-m7.elf and testdata/bin/c-cortex-m7-stripped.elf hold the same bytes at the same addresses.
```

A program for an operating system has one block, which gets a line instead
of a table:

```
$ elfq testdata/bin/c-x86_64.elf diff testdata/bin/c-x86_64-moved.elf
From testdata/bin/c-x86_64.elf to testdata/bin/c-x86_64-moved.elf: 1 new, 2 grew, 1 shrank, 1 edited.
The memory stays at 15728 bytes (.text +16, .eh_frame +8).

CHANGE  OLD  NEW  ADDRESS  ITEM
   +20   59   79  0x1610   crc32, kept by process_frame
   +13       ~13  0x2004   .rodata@0x2004 (new), kept by crc32
   -13  ~34  ~21  0x206b   .rodata@0x206b, kept by main
    +3   19   22  0x1660   on_button, kept by main
     0  532  532  0x1710   process_frame (edited), kept by main

64 other items are the same.
```

The strings that elfq finds only from the code that uses them have no
names, and estimated sizes, marked `~`, that end where the next one
starts: the new message took 13 bytes from the one after it.
`process_frame` kept its size, but the compiler keeps the frame it reads
in another register, so it is edited. Without a symbol table, diff
compares only the bytes, and no row says why the memory differs, so the
last line names every section that differs, as it does when no item
changed. The build ID in `.note.gnu.build-id` is among them: it is
loaded too, and the linker makes it from the whole file, so it changes
with any change to the file, even one only in debug info:

```
$ elfq testdata/bin/c-x86_64-stripped.elf diff testdata/bin/c-x86_64-moved-stripped.elf
From testdata/bin/c-x86_64-stripped.elf to testdata/bin/c-x86_64-moved-stripped.elf: the bytes differ.
testdata/bin/c-x86_64-stripped.elf has no symbol table, so diff compares only bytes, not functions and data.
The memory stays at 15728 bytes (.text +16, .eh_frame +8).
Sections that differ: .note.gnu.build-id, .text, .fini, .rodata, .eh_frame_hdr, .eh_frame and .dynamic.
```

## How do scripts read the answers?

Every command but `browse` prints its answer as JSON with `--json`, one row
per line:

```
$ elfq testdata/bin/c-x86_64.elf callers log_msg --json
{"symbol":{"name":"log_msg","kind":"function","section":".text","address":"0x1610","size":51,"size_estimated":false,"aliases":[]},"callers":[
{"item":{"name":"app_tick","kind":"function","section":".text","address":"0x16c0","size":51,"size_estimated":false},"how":"call","sites":["0x16e7"]},
{"item":{"name":"debug_dump","kind":"function","section":".text","address":"0x1aa0","size":29,"size_estimated":false},"how":"call","sites":["0x1aaf"]},
{"item":{"name":"on_button","kind":"function","section":".text","address":"0x1650","size":19,"size_estimated":false},"how":"tail call","sites":["0x1661"]},
{"item":{"name":"process_frame","kind":"function","section":".text","address":"0x1700","size":532,"size_estimated":false},"how":"call","sites":["0x18ee","0x1908"]},
{"item":{"name":"report","kind":"function","section":".text","address":"0x1920","size":370,"size_estimated":false},"how":"call","sites":["0x1a72"]}
],"complete":true}
```

It has every row and every site, even where the table would cut them short,
and each item's section, address and size. `complete` says whether the list
has every caller, as far as elfq can tell. [design.md](design.md#json) lists
every key. With `jq`, this prints the five names, one per line:

```sh
elfq testdata/bin/c-x86_64.elf callers log_msg --json | jq -r '.callers[].item.name'
```
