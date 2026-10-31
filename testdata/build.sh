#!/bin/sh
# Rebuilds the sample ELF files in testdata/bin from testdata/c, testdata/cpp
# and testdata/rust.
#
# The outputs are committed so that tests and docs do not need cross
# toolchains, and so that their bytes stay stable across machines. Only run
# this when a sample's source changes, and commit the results together.
#
# Needs: gcc and g++, arm-none-eabi-gcc with newlib, ld.lld, and rustup,
# which installs the Rust version that rust/rust-toolchain.toml names.
set -eu

cd "$(dirname "$0")"
here=$(pwd)
out=bin
mkdir -p "$out"

# Writes NAME.elf (symbols + DWARF), NAME-nodebug.elf (symbols only) and
# NAME-stripped.elf (no symbols) from one full build.
variants() {
    strip_tool=$1
    name=$2
    "$strip_tool" --strip-debug -o "$out/$name-nodebug.elf" "$out/$name.elf"
    "$strip_tool" --strip-all -o "$out/$name-stripped.elf" "$out/$name.elf"
}

# Paths inside DWARF are made relative so the files do not depend on where
# the repository is checked out.
cflags="-O2 -g -Wall -Wextra -ffile-prefix-map=$here=."

echo "C, x86-64 Linux"
gcc $cflags -o "$out/c-x86_64.elf" c/app.c c/platform_host.c
variants strip c-x86_64

# Only the full build: one variant is enough to cover how calls go through
# the PLT to the library's own functions.
echo "C, x86-64 Linux shared library"
gcc $cflags -shared -fPIC -o "$out/c-x86_64.so" c/app.c c/platform_host.c

# For diff to compare with c-x86_64.elf and c-x86_64.so: with -DDIFF_MOVED,
# crc32 logs an empty frame (see c/platform.h), as in c-cortex-m7-moved.elf.
echo "C, x86-64 Linux, crc32 logs an empty frame"
gcc $cflags -DDIFF_MOVED -o "$out/c-x86_64-moved.elf" c/app.c c/platform_host.c
variants strip c-x86_64-moved
gcc $cflags -DDIFF_MOVED -shared -fPIC -o "$out/c-x86_64-moved.so" c/app.c c/platform_host.c

# Without Intel CET, GNU ld lays out the PLT the older way, with 8-byte
# entries in .plt.got.
echo "C, x86-64 Linux shared library without CET"
gcc $cflags -shared -fPIC -fcf-protection=none -o "$out/c-plt.so" c/plt.c

echo "C, Cortex-M4"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16 \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/cortex-m4.ld -Wl,--gc-sections \
    -o "$out/c-cortex-m4.elf" c/app.c c/platform_cortexm.c
variants arm-none-eabi-strip c-cortex-m4

# Only the variants without DWARF: this sample is for finding the
# functions of stripped firmware whose constants are in .text.
echo "C, Cortex-M4, laid out as CMSIS does"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16 \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/cortex-m4-cmsis.ld -Wl,--gc-sections \
    -o "$out/c-cortex-m4-cmsis.elf" c/app.c c/platform_cortexm.c
variants arm-none-eabi-strip c-cortex-m4-cmsis
rm "$out/c-cortex-m4-cmsis.elf"

# cortex_m7_from NAME SCRIPT SOURCES FLAGS... builds SOURCES, a list split
# by spaces, for the Cortex-M7 as NAME.elf, linked with SCRIPT and compiled
# with FLAGS too.
cortex_m7_from() {
    name=$1
    script=$2
    sources=$3
    shift 3
    arm-none-eabi-gcc $cflags "$@" \
        -mcpu=cortex-m7 -mthumb -mfloat-abi=hard -mfpu=fpv5-d16 \
        -ffunction-sections -fdata-sections \
        -nostartfiles --specs=nano.specs --specs=nosys.specs \
        -T "$script" -Wl,--gc-sections \
        -o "$out/$name.elf" $sources
}

# cortex_m7 NAME SCRIPT FLAGS... builds the C sample for the Cortex-M7 as
# NAME.elf, linked with SCRIPT and compiled with FLAGS too.
cortex_m7() {
    name=$1
    script=$2
    shift 2
    cortex_m7_from "$name" "$script" "c/app.c c/platform_cortexm7.c" "$@"
}

echo "C, Cortex-M7 with TCMs"
cortex_m7 c-cortex-m7 c/cortex-m7.ld
variants arm-none-eabi-strip c-cortex-m7

# Builds for diff to compare with c-cortex-m7.elf, each with one change.
# Their linker scripts are cortex-m7.ld with one line changed. "moved"
# links filter_tap in flash rather than in ITCM, and with -DDIFF_MOVED,
# crc32 logs an empty frame (see c/platform.h).
echo "C, Cortex-M7 with TCMs, filter_tap in flash"
sed 's/ \.text\.filter_tap)/)/' c/cortex-m7.ld >"$out/cortex-m7-moved.ld"
cortex_m7 c-cortex-m7-moved "$out/cortex-m7-moved.ld" -DDIFF_MOVED
rm "$out/cortex-m7-moved.ld"
variants arm-none-eabi-strip c-cortex-m7-moved

# Only the full build: with -DDIFF_INLINED, the compiler inlines
# decode_header into process_frame, as its DWARF says.
echo "C, Cortex-M7 with TCMs, decode_header inlined"
cortex_m7 c-cortex-m7-inlined c/cortex-m7.ld -DDIFF_INLINED

# Only the full build: with -DDIFF_EDITED, the constant that decode_header
# compares with and one of the filter's coefficients change, and no item
# changes size.
echo "C, Cortex-M7 with TCMs, two constants changed"
cortex_m7 c-cortex-m7-edited c/cortex-m7.ld -DDIFF_EDITED

# Only the full build: DTCM at 0x80000000, so that .data and .bss move to
# another block of memory.
echo "C, Cortex-M7 with TCMs, DTCM at 0x80000000"
sed 's/^\( *DTCM .*ORIGIN = \)0x20000000/\10x80000000/' c/cortex-m7.ld >"$out/cortex-m7-sdram.ld"
cortex_m7 c-cortex-m7-sdram "$out/cortex-m7-sdram.ld"
rm "$out/cortex-m7-sdram.ld"

# ENTRY(main), so that the entry point isn't the reset handler, and elfq
# finds the vector table by how it begins.
echo "C, Cortex-M7 with TCMs, main as the entry point"
sed 's/^ENTRY(Reset_Handler)/ENTRY(main)/' c/cortex-m7.ld >"$out/cortex-m7-entry-main.ld"
cortex_m7 c-cortex-m7-entry-main "$out/cortex-m7-entry-main.ld"
rm "$out/cortex-m7-entry-main.ld"
variants arm-none-eabi-strip c-cortex-m7-entry-main

# Builds for startup, each with one mistake in its linker script. "trap"
# links memcpy into ITCM, where Reset_Handler calls it to copy .itcm before
# .itcm holds it. "reset-itcm" links Reset_Handler itself into ITCM, where
# the processor starts it before anything is copied there.
echo "C, Cortex-M7 with TCMs, memcpy in ITCM"
sed 's/^\( *\)\*(\.itcm_text\*)/&\n\1*_nano.a:libc_a-memcpy.o(.text .text.*)/' c/cortex-m7.ld \
    >"$out/cortex-m7-trap.ld"
cortex_m7 c-cortex-m7-trap "$out/cortex-m7-trap.ld"
rm "$out/cortex-m7-trap.ld"
variants arm-none-eabi-strip c-cortex-m7-trap

echo "C, Cortex-M7 with TCMs, Reset_Handler in ITCM"
sed 's/^\( *\)\*(\.itcm_text\*)/&\n\1*(.text.Reset_Handler)/' c/cortex-m7.ld \
    >"$out/cortex-m7-reset-itcm.ld"
cortex_m7 c-cortex-m7-reset-itcm "$out/cortex-m7-reset-itcm.ld"
rm "$out/cortex-m7-reset-itcm.ld"
variants arm-none-eabi-strip c-cortex-m7-reset-itcm

# A startup with a mistake in its code rather than in its linker script:
# Reset_Handler calls SystemInit, which calls code in ITCM, before it
# copies .itcm (see c/sysinit.c).
echo "C, Cortex-M7 with TCMs, SystemInit calls code in ITCM"
cortex_m7_from c-cortex-m7-sysinit c/cortex-m7.ld c/sysinit.c
variants arm-none-eabi-strip c-cortex-m7-sysinit

# Only the full build: a startup with more steps than startup's table shows
# without --all, and a call through a pointer (see c/steps.c).
echo "C, Cortex-M7 with TCMs, startup with many steps"
cortex_m7_from c-cortex-m7-steps c/cortex-m7.ld c/steps.c

# Startup code that copies and zeroes with loops of its own rather than
# memcpy and memset (see c/loops.c, c/loops-O0.c and c/tables.c). "loops"
# moves .text before .itcm, so that .data's load image follows .itcm's.
echo "C, Cortex-M7 with TCMs, startup loops in assembly"
sed -e '/^    \.text : /d' -e '/^    \.isr_vector : /a\    .text : { *(.text*) } > FLASH' \
    c/cortex-m7.ld >"$out/cortex-m7-loops.ld"
cortex_m7_from c-cortex-m7-loops "$out/cortex-m7-loops.ld" c/loops.c
rm "$out/cortex-m7-loops.ld"
variants arm-none-eabi-strip c-cortex-m7-loops

echo "C, Cortex-M7 with TCMs, startup loops in C without optimization"
cortex_m7_from c-cortex-m7-loops-O0 c/cortex-m7.ld c/loops-O0.c -O0
variants arm-none-eabi-strip c-cortex-m7-loops-O0

# "tables" adds the tables of sections to copy and zero before .text.
echo "C, Cortex-M7 with TCMs, startup tables"
sed 's/^    \.text : /    .copy.table : {\
        __copy_table_start__ = .;\
        LONG(LOADADDR(.itcm)) LONG(ADDR(.itcm)) LONG(SIZEOF(.itcm) \/ 4)\
        LONG(LOADADDR(.data)) LONG(ADDR(.data)) LONG(SIZEOF(.data) \/ 4)\
        __copy_table_end__ = .;\
    } > FLASH\
    .zero.table : {\
        __zero_table_start__ = .;\
        LONG(ADDR(.bss)) LONG(SIZEOF(.bss) \/ 4)\
        __zero_table_end__ = .;\
    } > FLASH\
&/' c/cortex-m7.ld >"$out/cortex-m7-tables.ld"
cortex_m7_from c-cortex-m7-tables "$out/cortex-m7-tables.ld" c/tables.c
rm "$out/cortex-m7-tables.ld"
variants arm-none-eabi-strip c-cortex-m7-tables

# "sections" adds tables laid out as MCUXpresso IDE's, whose rows ResetISR
# passes to data_init and bss_init (see c/sections.c), before .text.
echo "C, Cortex-M7 with TCMs, startup tables with a function for each row"
sed -e 's/^ENTRY(Reset_Handler)/ENTRY(ResetISR)/' -e 's/^    \.text : /    .data_section_table : {\
        __data_section_table = .;\
        LONG(LOADADDR(.itcm)) LONG(ADDR(.itcm)) LONG(SIZEOF(.itcm))\
        LONG(LOADADDR(.data)) LONG(ADDR(.data)) LONG(SIZEOF(.data))\
        __data_section_table_end = .;\
        __bss_section_table = .;\
        LONG(ADDR(.bss)) LONG(SIZEOF(.bss))\
        LONG(ADDR(.ocram_bss)) LONG(SIZEOF(.ocram_bss))\
        __bss_section_table_end = .;\
    } > FLASH\
&/' c/cortex-m7.ld >"$out/cortex-m7-sections.ld"
cortex_m7_from c-cortex-m7-sections "$out/cortex-m7-sections.ld" c/sections.c
rm "$out/cortex-m7-sections.ld"
variants arm-none-eabi-strip c-cortex-m7-sections

# Only the full build: a startup that copies .data with a function from the
# boot ROM, which isn't in the file (see c/rom.c).
echo "C, Cortex-M7 with TCMs, .data copied by the boot ROM"
cortex_m7_from c-cortex-m7-rom c/cortex-m7.ld c/rom.c

# A startup laid out as the Pico SDK's for the RP2040, whose table of
# sections to copy ends with a row of zeros (see c/pico.c and c/pico.ld).
echo "C, Cortex-M0+, laid out as the Pico SDK does"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m0plus -mthumb \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/pico.ld -Wl,--gc-sections \
    -o "$out/c-cortex-m0-pico.elf" c/pico.c
variants arm-none-eabi-strip c-cortex-m0-pico

# Only the full build: a startup laid out as Zephyr's, whose reset handler
# calls a function that sets up the memory and doesn't return (see
# c/noreturn.c).
echo "C, Cortex-M7 with TCMs, setup in a call that doesn't return"
cortex_m7_from c-cortex-m7-noreturn c/cortex-m7.ld c/noreturn.c

# Only the variant with symbols and no DWARF: this sample is for libgcc's
# routines for doubles, which have no DWARF of their own.
echo "C, Cortex-M4 without its FPU"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=soft \
    -ffunction-sections -fdata-sections -nostdlib \
    -T c/cortex-m4.ld -Wl,--gc-sections \
    -o "$out/c-softfloat.elf" c/softfloat.c -lgcc
arm-none-eabi-strip --strip-debug -o "$out/c-softfloat-nodebug.elf" "$out/c-softfloat.elf"
rm "$out/c-softfloat.elf"

# Conditional VFP instructions in IT blocks, as `vmovgt.f32`, for browse's
# Thumb text.
echo "C, Cortex-M4 with its FPU"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16 \
    -ffunction-sections -fdata-sections -nostdlib \
    -T c/cortex-m4.ld -Wl,--gc-sections \
    -o "$out/c-vfp.elf" c/vfp.c
variants arm-none-eabi-strip c-vfp

# newlib's strtod, whose switch jumps through a table of addresses with
# `ldr.w pc`, for browse's Thumb text and the sweep that finds such tables.
echo "C, Cortex-M4, newlib's strtod"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16 \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/cortex-m4.ld -Wl,--gc-sections \
    -o "$out/c-strtod.elf" c/strtod.c
variants arm-none-eabi-strip c-strtod

# Calls too far for one branch instruction, through the veneers that GNU ld
# and LLD add for Cortex-M, all but two of LLD's (see README.md), and through a
# register (see c/far.c). Only the variants without DWARF, which has nothing to add here.
# far_gnu NAME FLAGS... links with GNU ld.
far_gnu() {
    name=far-$1
    shift
    echo "C, far calls, $name"
    arm-none-eabi-gcc $cflags -mthumb -ffunction-sections "$@" -nostdlib \
        -T c/far.ld -o "$out/$name.elf" c/far.c
    variants arm-none-eabi-strip "$name"
    rm "$out/$name.elf"
}

# far_lld NAME COMPILER-FLAGS LINKER-FLAGS links with LLD.
far_lld() {
    name=far-$1
    echo "C, far calls, $name"
    arm-none-eabi-gcc $cflags -mthumb -ffunction-sections $2 -c -o "$out/$name.o" c/far.c
    # LLD puts segments 64 KiB apart in the file by default, which would
    # make each file mostly zeros.
    ld.lld -T c/far.ld -z max-page-size=4 $3 -o "$out/$name.elf" "$out/$name.o"
    rm "$out/$name.o"
    variants arm-none-eabi-strip "$name"
    rm "$out/$name.elf"
}

far_gnu gnu-m4 -mcpu=cortex-m4
far_gnu gnu-m4-pure -mcpu=cortex-m4 -mpure-code
far_gnu gnu-m4-pic -mcpu=cortex-m4 -Wl,--pic-veneer
far_gnu gnu-m0 -mcpu=cortex-m0
far_lld lld-m4 -mcpu=cortex-m4 ""
far_lld lld-m4-pic -mcpu=cortex-m4 --pic-veneer
far_lld lld-m0 -mcpu=cortex-m0 ""
far_lld lld-m0-pure "-mcpu=cortex-m0 -mpure-code" ""

# Only the full build: this sample is for looking at trees deeper than elfq
# draws in one piece.
echo "C, nested 20 deep, x86-64 Linux"
gcc $cflags -o "$out/c-deep.elf" c/deep.c

echo "C++, x86-64 Linux"
g++ $cflags -o "$out/cpp-x86_64.elf" cpp/app.cpp
variants strip cpp-x86_64

# Only the full build, for the C++ names of the PLT stubs that the library
# calls its own functions through.
echo "C++, x86-64 Linux shared library"
g++ $cflags -shared -fPIC -o "$out/cpp-x86_64.so" cpp/app.cpp

rustflags="--remap-path-prefix=$here=."
# Build where the copies below look, even if CARGO_TARGET_DIR or a cargo
# config points elsewhere.
export CARGO_TARGET_DIR="$here/rust/target"

echo "Rust, x86-64 Linux"
(cd rust && RUSTFLAGS="$rustflags" cargo build --release --locked --quiet)
cp rust/target/release/elfq-sample "$out/rust-x86_64.elf"
variants strip rust-x86_64
# With full DWARF the std library makes this file over 4 MB, so only the
# smaller variants are committed (see README.md).
rm "$out/rust-x86_64.elf"

echo "Rust, Cortex-M4"
(cd rust && RUSTFLAGS="$rustflags" cargo build --release --locked --quiet --target thumbv7em-none-eabihf)
cp rust/target/thumbv7em-none-eabihf/release/elfq-sample "$out/rust-cortex-m4.elf"
variants arm-none-eabi-strip rust-cortex-m4

ls -l "$out"
