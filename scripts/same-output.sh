#!/bin/sh
# Checks that two builds of elfq print the same answers: each command below,
# run on each file, must write the same stdout and stderr with both builds
# and exit with the same status. For changes that shouldn't change output,
# such as a cleanup. CONTRIBUTING.md says when and how to run it.
#
#   scripts/same-output.sh OLD NEW FILE_OR_FOLDER... [--pairs LIST]
#   scripts/same-output.sh --time OLD NEW FILE...
#
# OLD and NEW are elfq binaries, such as release builds of main and of your
# branch. A FOLDER stands for every file in it, at any depth. LIST is a file
# of diff pairs, one "OLD_FILE NEW_FILE" per line, with no spaces in the
# paths. Each difference is printed, and the script exits with status 1 if
# there is one. Files are checked as many at once as the machine has
# processors.
#
# With --time, it runs each command in `timed` below on each FILE 9 times
# with each build, the builds taking turns, and prints the median times in
# milliseconds, side by side. Use large files for it, as CONTRIBUTING.md says.
set -u

usage() {
    sed -n '7,8p' "$0" | sed 's/^# *//' >&2
    exit 2
}

# Both builds run as `elfq`, since clap puts the program's name in --help
# and in errors.
work=$(mktemp -d) || exit 2
trap 'rm -rf "$work"' EXIT
trap 'exit 2' HUP INT TERM
install_builds() {
    mkdir "$work/old" "$work/new"
    cp "$1" "$work/old/elfq" && cp "$2" "$work/new/elfq" || exit 2
    old=$work/old/elfq
    new=$work/new/elfq
}

# Runs `elfq ARGS...` with both builds, and prints what differs, if anything.
compare() {
    "$old" "$@" </dev/null >"$work/old.out" 2>"$work/old.err"
    old_status=$?
    "$new" "$@" </dev/null >"$work/new.out" 2>"$work/new.err"
    new_status=$?
    echo run
    if [ "$old_status" = "$new_status" ] &&
        cmp -s "$work/old.out" "$work/new.out" &&
        cmp -s "$work/old.err" "$work/new.err"; then
        return
    fi
    echo "differs: elfq $*"
    if [ "$old_status" != "$new_status" ]; then
        echo "  exit status $old_status, now $new_status"
    fi
    diff "$work/old.out" "$work/new.out" | head -20 | sed 's/^/  /'
    diff "$work/old.err" "$work/new.err" | head -20 | sed 's/^/  stderr: /'
}

# The commands run on one file. Those that take a SYMBOL take each of the
# first three items that `top` lists, and those that take a range take the
# first two blocks of memory that the overview lists, as old elfq prints
# them. A command that refuses the file is compared too.
check_file() {
    file=$1
    compare "$file"
    compare "$file" --json
    compare "$file" --tree
    compare "$file" top
    compare "$file" top --all
    compare "$file" top --tree
    compare "$file" top --json
    compare "$file" inlined
    compare "$file" inlined --all
    compare "$file" inlined --json
    compare "$file" reach
    compare "$file" reach --json
    compare "$file" startup
    compare "$file" startup --all
    compare "$file" startup --json
    compare "$file" debug items
    compare "$file" debug walk
    "$old" "$file" top --json 2>/dev/null |
        sed -n 's/^{"id":[0-9]*,"name":"\([^"\\]*\)".*/\1/p' |
        head -3 >"$work/symbols"
    while IFS= read -r symbol; do
        compare "$file" retained "$symbol"
        compare "$file" retained "$symbol" --tree
        compare "$file" retained "$symbol" --json
        compare "$file" why "$symbol"
        compare "$file" why "$symbol" --all
        compare "$file" why "$symbol" --json
        compare "$file" callers "$symbol"
        compare "$file" callers "$symbol" --json
        compare "$file" callees "$symbol"
        compare "$file" callees "$symbol" --json
        compare "$file" reach "$symbol"
        compare "$file" reach "$symbol" --tree
        compare "$file" reach "$symbol" --json
        compare "$file" inlined "$symbol"
    done <"$work/symbols"
    "$old" "$file" 2>/dev/null |
        sed -n 's/.* --range \([^ ]*\)$/\1/p' | head -2 >"$work/ranges"
    first=
    while IFS= read -r range; do
        compare "$file" --range "$range"
        compare "$file" top --range "$range" --json
        compare "$file" callers "$range"
        compare "$file" callees "$range" --json
        if [ -n "$first" ]; then
            compare "$file" callers "$range" --from "$first"
            compare "$file" callees "$first" --to "$range" --json
            compare "$file" reach --to "$range"
        fi
        first=$range
    done <"$work/ranges"
}

# What every build prints for help, whatever the file.
check_help() {
    compare
    compare --help
    for command in top retained why inlined callers callees reach browse \
        diff startup debug; do
        compare help "$command"
        compare file.elf "$command" --help
    done
    compare file.elf debug items --help
    compare file.elf debug walk --help
}

# Prints the median times, in milliseconds, that `elfq ARGS...` takes with
# the old build and with the new, over 9 runs of each. The runs take turns,
# so that a moment when the machine is busy slows both builds alike.
median_times() {
    i=0
    while [ "$i" -lt 9 ]; do
        echo "$(run_time "$old" "$@") $(run_time "$new" "$@")"
        i=$((i + 1))
    done >"$work/times"
    echo "$(median 1) $(median 2)"
}

# Prints how long `BUILD ARGS...` takes, in milliseconds. It needs GNU date,
# for %N.
run_time() {
    build=$1
    shift
    start=$(date +%s%N)
    "$build" "$@" </dev/null >/dev/null 2>&1
    end=$(date +%s%N)
    echo $(((end - start) / 1000000))
}

# Prints the median of column COLUMN of the times.
median() {
    cut -d ' ' -f "$1" "$work/times" | sort -n | sed -n 5p
}

# The commands that --time runs on each file.
timed() {
    file=$1
    symbol=$("$old" "$file" top --json 2>/dev/null |
        sed -n 's/^{"id":[0-9]*,"name":"\([^"\\ ]*\)".*/\1/p' | head -1)
    for command in "" top inlined reach startup "retained $symbol" \
        "why $symbol" "callers $symbol" "reach $symbol"; do
        # $command is split into words on purpose.
        # shellcheck disable=SC2086
        set -- "$file" $command
        echo "$(median_times "$@") elfq $*"
    done
}

if [ "${1-}" = --time ]; then
    [ $# -ge 4 ] || usage
    case $(date +%N) in
    *N*)
        echo "--time needs GNU date, as on Linux" >&2
        exit 2
        ;;
    esac
    install_builds "$2" "$3"
    shift 3
    echo "OLD ms  NEW ms  CHANGE  COMMAND"
    for file in "$@"; do
        timed "$file" | awk '{ change = $1 > 0 ? ($2 - $1) * 100 / $1 : 0
            change = int(change + (change < 0 ? -0.5 : 0.5))
            printf "%6d  %6d  %+5d%%  ", $1, $2, change
            $1 = ""; $2 = ""; sub(/^  /, ""); print }'
    done
    exit 0
fi

# The script checks each file in a process of its own, which runs it with
# --one and writes what it found to REPORT.
if [ "${1-}" = --one ]; then
    old=$2
    new=$3
    check_file "$5" >"$4"
    exit 0
fi

[ $# -ge 3 ] || usage
install_builds "$1" "$2"
shift 2
: >"$work/files"
: >"$work/pairs"
while [ $# -gt 0 ]; do
    case $1 in
    --pairs)
        [ $# -ge 2 ] || usage
        cat "$2" >>"$work/pairs" || exit 2
        shift 2
        ;;
    *)
        if [ -d "$1" ]; then
            find -L "$1" -type f | sort >>"$work/files"
        elif [ -e "$1" ]; then
            echo "$1" >>"$work/files"
        else
            echo "no such file or folder: $1" >&2
            exit 2
        fi
        shift
        ;;
    esac
done

mkdir "$work/reports"
processors=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)
i=0
while IFS= read -r file; do
    i=$((i + 1))
    printf '%s\000%s\000' "$work/reports/$i" "$file"
done <"$work/files" |
    xargs -0 -n 2 -P "$processors" sh "$0" --one "$old" "$new"
files=$(wc -l <"$work/files" | tr -d ' ')

{
    check_help
    i=1
    while [ "$i" -le "$files" ]; do
        cat "$work/reports/$i"
        i=$((i + 1))
    done
    while read -r old_file new_file; do
        [ -n "$old_file" ] || continue
        compare "$old_file" diff "$new_file"
        compare "$old_file" diff "$new_file" --all
        compare "$old_file" diff "$new_file" --json
    done <"$work/pairs"
} >"$work/report"

grep -v '^run$' "$work/report"
runs=$(grep -c '^run$' "$work/report")
differ=$(grep -c '^differs: ' "$work/report")
echo "$runs commands on $files files and $(grep -c . "$work/pairs") pairs;" \
    "$differ differ"
[ "$differ" -eq 0 ]
