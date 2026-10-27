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

# cortex_m7 NAME SCRIPT FLAGS... builds the C sample for the Cortex-M7 as
# NAME.elf, linked with SCRIPT and compiled with FLAGS too.
cortex_m7() {
    name=$1
    script=$2
    shift 2
    arm-none-eabi-gcc $cflags "$@" \
        -mcpu=cortex-m7 -mthumb -mfloat-abi=hard -mfpu=fpv5-d16 \
        -ffunction-sections -fdata-sections \
        -nostartfiles --specs=nano.specs --specs=nosys.specs \
        -T "$script" -Wl,--gc-sections \
        -o "$out/$name.elf" c/app.c c/platform_cortexm7.c
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

# Only the full build: DTCM at 0x80000000, so that .data and .bss move to
# another block of memory.
echo "C, Cortex-M7 with TCMs, DTCM at 0x80000000"
sed 's/^\( *DTCM .*ORIGIN = \)0x20000000/\10x80000000/' c/cortex-m7.ld >"$out/cortex-m7-sdram.ld"
cortex_m7 c-cortex-m7-sdram "$out/cortex-m7-sdram.ld"
rm "$out/cortex-m7-sdram.ld"

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
