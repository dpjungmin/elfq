#!/bin/sh
# Writes tests/thumb/NAME.txt for each Cortex-M sample NAME-nodebug.elf in
# bin: objdump's text for its code, which the tests check browse's Thumb
# text against (src/commands/browse/views/thumb_text.rs), so that they don't
# need objdump.
#
# Each line is ADDRESS, MNEMONIC and OPERANDS, split by tabs, as
# `objdump -d -z` prints them, without its comments and with an address in
# place of `ADDRESS <symbol+offset>`. Two comments say more than the
# operands: `add r0, pc, #4 @ (adr r0, 8002530 ...)` becomes
# `adr r0, 0x8002530`, and `vmov.f32 s14, #96 @ 0x3f000000 0.5` becomes
# `vmov.f32 s14, #0.5`, as elfq prints them. The data objects in code, such
# as a vector table, are left out: objdump prints their bytes as text.
#
# Run it after build.sh, and commit the results with the samples. Needs
# arm-none-eabi-objdump and arm-none-eabi-readelf.
set -eu

cd "$(dirname "$0")"
mkdir -p tests/thumb
tab=$(printf '\t')
for file in bin/*-nodebug.elf; do
    arm-none-eabi-objdump -f "$file" 2>/dev/null | grep -q elf32-littlearm || continue
    name=$(basename "$file" -nodebug.elf)
    objects=$(arm-none-eabi-readelf -sW "$file" | awk '$4 == "OBJECT" { print $2 }')
    arm-none-eabi-objdump -d -z --no-show-raw-insn "$file" |
        awk -v objects="$objects" '
            BEGIN { n = split(objects, list); for (i = 1; i <= n; i++) object[list[i]] = 1 }
            /^[0-9a-f]+ <.*>:$/ { skip = ($1 in object) }
            !skip' |
        sed -n \
            -e "/^ *[0-9a-f]*:$tab/!d" \
            -e "s/^\\( *[0-9a-f]*:$tab\\)addw*$tab\\([a-z0-9]*\\), pc, #[0-9]*$tab@ (adr [a-z0-9]*, \\([0-9a-f]*\\) .*/\\1adr$tab\\2, 0x\\3/" \
            -e "s/^\\(.*${tab}vmov[^$tab]*$tab.*#\\)-*[0-9]*$tab@ 0x[0-9a-f]* *\\([^ ]*\\)\$/\\1\\2/" \
            -e "s/$tab$tab*@.*//" \
            -e "s/\\([0-9a-f][0-9a-f]*\\) <[^>]*>\$/0x\\1/" \
            -e "s/^ *\\([0-9a-f]*\\):$tab/\\1$tab/" \
            -e p \
            >"tests/thumb/$name.txt"
done
ls -l tests/thumb
