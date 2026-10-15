#!/bin/sh
# Rebuilds the sample ELF files in testdata/bin from testdata/c, testdata/cpp
# and testdata/rust.
#
# The outputs are committed so that tests and docs do not need cross
# toolchains, and so that their bytes stay stable across machines. Only run
# this when a sample's source changes, and commit the results together.
#
# Needs: gcc and g++, arm-none-eabi-gcc with newlib, cargo with the
# thumbv7em-none-eabihf target (rustup target add thumbv7em-none-eabihf).
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

echo "C, Cortex-M4"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16 \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/cortex-m4.ld -Wl,--gc-sections \
    -o "$out/c-cortex-m4.elf" c/app.c c/platform_cortexm.c
variants arm-none-eabi-strip c-cortex-m4

echo "C, Cortex-M7 with TCMs"
arm-none-eabi-gcc $cflags \
    -mcpu=cortex-m7 -mthumb -mfloat-abi=hard -mfpu=fpv5-d16 \
    -ffunction-sections -fdata-sections \
    -nostartfiles --specs=nano.specs --specs=nosys.specs \
    -T c/cortex-m7.ld -Wl,--gc-sections \
    -o "$out/c-cortex-m7.elf" c/app.c c/platform_cortexm7.c
variants arm-none-eabi-strip c-cortex-m7

# Only the full build: this sample is for looking at trees deeper than elfq
# draws in one piece.
echo "C, nested 20 deep, x86-64 Linux"
gcc $cflags -o "$out/c-deep.elf" c/deep.c

echo "C++, x86-64 Linux"
g++ $cflags -o "$out/cpp-x86_64.elf" cpp/app.cpp
variants strip cpp-x86_64

rustflags="--remap-path-prefix=$here=."

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
