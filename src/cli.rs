//! Command-line arguments. Each command has a variant here and a module in
//! `commands/`.
//!
//! The file comes first and the question after it, so that a command sits
//! next to the symbol it asks about: `elfq app.elf callers log_msg`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::binary::AddressRange;

// The whole command line. No `///` comment here: clap would show one of more
// than one paragraph at the top of `elfq --help`, in place of the
// description that `about` takes from Cargo.toml.
#[derive(Debug, Parser)]
#[command(
    name = "elfq",
    version,
    about,
    arg_required_else_help = true,
    disable_help_subcommand = true,
    after_help = EXAMPLES
)]
pub struct Cli {
    /// The ELF file: an executable, shared library or firmware image
    pub file: PathBuf,
    /// What to ask about FILE. Without one, elfq prints an overview.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Print the answer as JSON, for scripts
    #[arg(long, global = true)]
    pub json: bool,
    /// Draw the overview, top, retained, why, reach SYMBOL and inlined
    /// SYMBOL as trees
    #[arg(long, global = true)]
    pub tree: bool,
    /// Count only the bytes in one range of memory, such as ITCM: START..END
    /// or START+SIZE, like 0x20000000+256K. When the file's sections and load
    /// images occupy several ranges, as in firmware, the overview lists them.
    /// For the overview, top and retained.
    #[arg(long, global = true, value_name = "RANGE", value_parser = one_range)]
    pub range: Option<AddressRange>,
}

/// Reads `--range`, which takes one range: given several, separated by
/// commas, it says so, with the first as the example.
fn one_range(text: &str) -> Result<AddressRange, String> {
    if let Some((first, _)) = text.split_once(',')
        && first.parse::<AddressRange>().is_ok()
    {
        return Err(format!("--range takes one range, such as {}", first.trim()));
    }
    text.parse()
}

/// Reads one `--from`.
fn from_range(text: &str) -> Result<AddressRange, String> {
    one_range_each("--from", text)
}

/// Reads one `--to`.
fn to_range(text: &str) -> Result<AddressRange, String> {
    one_range_each("--to", text)
}

/// Reads one range for `option`, `--from` or `--to`, which take one each:
/// given several, separated by commas, it shows them given apart.
fn one_range_each(option: &str, text: &str) -> Result<AddressRange, String> {
    if !text.contains(',') {
        return text.parse();
    }
    let parts: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    for part in &parts {
        part.parse::<AddressRange>()?;
    }
    match parts[..] {
        [] => text.parse(),
        [_] => Err(format!("{option} takes one range; leave out the comma")),
        _ => {
            let apart: Vec<String> = parts
                .iter()
                .map(|part| format!("{option} {part}"))
                .collect();
            Err(format!(
                "{option} takes one range; give it again for each: {}",
                apart.join(" ")
            ))
        }
    }
}

impl Cli {
    /// How to print the answer. `--json` and `--tree` don't go together,
    /// which `commands::run` checks.
    pub fn format(&self) -> Format {
        if self.json {
            Format::Json
        } else if self.tree {
            Format::Tree
        } else {
            Format::Text
        }
    }
}

/// How a command prints its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Tables of one row per line, for people and for tools such as grep
    Text,
    /// The same, but with trees drawn with branches, with `--tree`
    Tree,
    /// JSON, for scripts, with `--json`
    Json,
}

const EXAMPLES: &str = "\
Examples:
  elfq app.elf                            Overview of app.elf
  elfq app.elf top                        What keeps the most bytes in app.elf
  elfq app.elf top --tree                 The same, drawn as a tree
  elfq app.elf top --range 0x0+256K       The same, counting only the first 256 KiB
  elfq app.elf retained process_frame     What removing process_frame would save
  elfq app.elf why crc_table              What keeps crc_table in the file
  elfq app.elf inlined                    Which inlined functions take the most bytes
  elfq app.elf inlined main               The inlined code in main
  elfq app.elf callers log_msg            What calls or uses log_msg
  elfq app.elf callees main               What main calls or uses
  elfq app.elf callees 0x0+256K           What the first 256 KiB uses outside it
  elfq app.elf reach                      Where the program starts, and the bytes each start reaches
  elfq app.elf reach SysTick_Handler      What SysTick_Handler runs and uses, and where
  elfq app.elf browse                     Where the program starts, each start a page to read
  elfq app.elf browse log_msg             Read log_msg's code, what it calls and reaches, its callers, and what keeps it
  elfq old.elf diff new.elf               What changed from old.elf to new.elf

The overview's table is the start of the one top shows; elfq help top
explains it.

Tables have one row per line: short cells such as sizes, HOW and AT come
first, and the name last. A name after \"kept by\" or \"used by\" works as
SYMBOL, and a why row without \"used by\" is used by the row above, or
starts a chain. The tables of top, retained, callers, callees and reach
show the 30 largest items, inlined the 30 largest functions, why the
chains through the 10 nearest users, and diff the 30 largest changes and
the 3 sections that changed most in each block; --all shows every row and
section, and makes sentences name everything they would cut short. With
ranges, --from or --to, callers, callees and reach show every row. --json
has every item, site and chain, one row per line, but the overview's JSON
leaves its table to top's. Scripts that read why's chains should use
--json.

Exit status: 0 for an answer, 1 when callers or callees with ranges, --from
or --to finds something that crosses, or might, reach --to finds something
in its ranges, or might, or diff finds that NEW's memory differs from
FILE's, and 2 for an error, which goes to stderr with nothing on stdout.";

// One variant per command. Each variant's `///` line is its line in
// `elfq --help`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the items that keep the most bytes in the file
    #[command(after_help = TOP)]
    Top(TopArgs),
    /// Show what removing SYMBOL would save
    #[command(after_help = RETAINED)]
    Retained(RetainedArgs),
    /// Show the chains of calls and references that keep SYMBOL in the file
    #[command(after_help = format!("{WHY}\n\n{}\n\n{HOW_NOTES}", how_table()))]
    Why(WhyArgs),
    /// List the functions the compiler inlined, by the bytes their copies take
    #[command(after_help = INLINED)]
    Inlined(InlinedArgs),
    /// List what calls or uses SYMBOL
    #[command(
        override_usage = "elfq <FILE> callers [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callers [OPTIONS] <RANGE>...",
        after_help = format!("{}\n\n{HOW_NOTES}\n\n{AT}\n\n{CALLERS}\n\n{CALLERS_RANGES}", how_table())
    )]
    Callers(CallersArgs),
    /// List what SYMBOL calls or uses
    #[command(
        override_usage = "elfq <FILE> callees [OPTIONS] <SYMBOL>\n       \
                          elfq <FILE> callees [OPTIONS] <RANGE>...",
        after_help = format!("{}\n\n{HOW_NOTES}\n\n{AT}\n\n{CALLEES}\n\n{CALLEES_RANGES}", how_table())
    )]
    Callees(CalleesArgs),
    /// List everything SYMBOL calls or uses, at any depth, by memory block
    #[command(after_help = format!("{REACH}\n\n{}\n\n{HOW_NOTES}", how_table()))]
    Reach(ReachArgs),
    /// Read a function's code in the terminal, and follow what it calls, reaches and is used by
    #[command(after_help = format!("{BROWSE}\n\n{}", how_table()))]
    Browse(BrowseArgs),
    /// Show what changed from FILE to NEW, by block of memory
    #[command(after_help = DIFF)]
    Diff(DiffArgs),
    /// Show elfq's own view of the file (for developing elfq)
    #[command(subcommand, hide = true)]
    Debug(DebugCommand),
}

/// The words of the HOW column, each with what it means, which `--help`
/// prints as a table, and browse's help page lists too.
pub const HOW_WORDS: [(&str, &str); 5] = [
    (
        "call",
        "a direct call, or a call through a register that the code loaded with \
         the target's address, as a long call does",
    ),
    (
        "tail call",
        "a jump to the start of a function, which then returns to the caller's caller",
    ),
    ("uses", "code reads or writes data, or loads its address"),
    (
        "address taken",
        "code loads a function's address, as when passing a function pointer, \
         or an import's",
    ),
    (
        "pointer",
        "data holds a pointer to it, as in a table of functions",
    ),
];

/// What the HOW column's words mean, as a table: each meaning is wrapped
/// under the column it starts in.
fn how_table() -> String {
    const WIDTH: usize = 76;
    const INDENT: usize = 17;
    let mut text = "The HOW column says how one item uses the other:".to_string();
    for (word, meaning) in HOW_WORDS {
        let mut line = format!("  {word:<13}  ");
        for part in meaning.split(' ') {
            if line.len() > INDENT && line.len() + 1 + part.len() > WIDTH {
                text += "\n";
                text += line.trim_end();
                line = " ".repeat(INDENT);
            } else if line.len() > INDENT {
                line.push(' ');
            }
            line += part;
        }
        text += "\n";
        text += &line;
    }
    text
}

/// What the commands with a HOW column also say about it, after
/// [`how_table`].
const HOW_NOTES: &str = "\
In --json, the how key uses these words, but in place of uses, it says
access where code reads or writes data, and address taken where it loads
its address. elfq tells the two apart only in x86-64 code; in Thumb code,
it says address taken.

elfq can't tell where a call through a pointer set at run time goes; the
address taken and pointer rows show where such pointers come from.";

const AT: &str = "\
The AT column gives the sites: the address of each instruction or pointer,
or for more than two, the first and how many more; --json has every one.";

const CALLERS: &str = "\
The table lists the 30 largest callers by their own size, in order of name,
and a last row such as \"70 more\" counts the rest; --all lists every one.";

const CALLEES: &str = "\
The table lists the 30 largest callees by their own size, in the order
SYMBOL first uses them, and a last row such as \"70 more\" counts the rest;
--all lists every one.";

const CALLEES_RANGES: &str = "\
With ranges of memory in place of SYMBOL, such as 0x0+256K for an ITCM,
elfq lists only what the code and data in them use outside them. Each row
names the item outside first, as in \"memcpy, used by apply_filter\", and
ADDRESS and SIZE are that item's. Every crossing has a row, and --all makes
the sentences name everything they would cut short. Calls and jumps that
elfq can't follow come first, as \"(through a pointer)\", or as \"(outside any
function)\" when they go to an address no function holds. A call through a
veneer, which a linker adds when the target is too far away for the call
instruction, is listed where it calls the veneer, as \"call via veneer\",
and a call through a PLT stub to one of the file's own functions, as in a
shared library, as \"call via PLT\". In --json, the how key leaves out \"via
veneer\" and \"via PLT\", and the veneer key names the veneer or stub. elfq
knows the veneers that GNU ld and LLD add for Cortex-M.

--to RANGE lists only the callees in RANGE, and can be given again for
more: callees 0x0+256K --to 0x30000000+16M lists what an ITCM at 0x0 uses
in flash at 0x30000000. With SYMBOL, --to answers as for the range that
SYMBOL occupies: callees process_frame --to 0x0+256K lists what
process_frame uses in the ITCM. Calls and jumps that elfq can't follow stay
when they may go to the --to ranges: one to a known address, when the
address is in them, and one through a pointer, when the ranges hold a
function whose address is taken or stored other than in the vector table.
When the --to ranges hold a veneer or PLT stub but not its target, the call
is listed as a call to the veneer or stub, as in \"__crc32_veneer, used by
process_frame\".

Exit status, with ranges or --to: 1 if the answer lists a callee, or code
that elfq can't follow or read; 0 if not; and 2 for an error.";

const CALLERS_RANGES: &str = "\
With ranges of memory in place of SYMBOL, such as 0x0+256K for an ITCM,
elfq lists only the code and data outside them that use what is in them.
Each row names the item outside first, as in \"process_frame, uses crc32\",
and ADDRESS and SIZE are that item's. Every crossing has a row, and --all
makes the sentences name everything they would cut short. Calls and jumps
to an address in the ranges that no function holds, as in a ROM, come
first, as \"uses 0x1fff0000 (outside any function)\". A call through a
veneer, which a linker adds when the target is too far away for the call
instruction, is listed where it calls the veneer, as \"call via veneer\",
and a call through a PLT stub to one of the file's own functions, as in a
shared library, as \"call via PLT\". In --json, the how key leaves out \"via
veneer\" and \"via PLT\", and the veneer key names the veneer or stub. elfq
knows the veneers that GNU ld and LLD add for Cortex-M. A note below the
table names the functions in the ranges whose address is taken or stored,
other than in the vector table, since calls to them through a pointer
aren't listed.

--from RANGE lists only the callers in RANGE, and can be given again for
more: callers 0x20000000+256K --from 0x0+256K lists what in an ITCM at 0x0
uses a DTCM at 0x20000000. With SYMBOL, --from answers as for the range
that SYMBOL occupies: callers crc32 --from 0x30000000+16M lists what in
flash at 0x30000000 calls or uses crc32. The note on calls through a pointer
stays only when code in the --from ranges calls through one, or is a
function whose end elfq doesn't know, which a line then names. When the
--from ranges hold a veneer or PLT stub but not the code that calls it, the
veneer or stub is listed as the caller, at its jump.

Exit status, with ranges or --from: 1 if the answer lists a caller, or
notes calls through a pointer or code that elfq can't read; 0 if not; and 2
for an error.";

const REACH: &str = "\
SYMBOL reaches what it calls or uses, what those call or use, and so on:
calls, tail calls, data that code reads or writes, anything whose address
code takes, and pointers stored in data, as in a table of functions. These
are what retained follows, and what a linker's --gc-sections keeps. reach
needs no entry point, so it works on shared libraries too.

Each row names the item that uses it on a shortest chain from SYMBOL, as in
\"log_msg, used by app_tick\"; of chains equally short, elfq takes the one
with the most calls, then the first it finds, so the answer is always the
same. A call through a veneer, which a linker adds when the target is too
far away for the call instruction, is listed as a call from the code that
calls the veneer, as \"tail call via veneer\", and the veneer has a row of
its own; so is a call through a PLT stub to one of the file's own
functions, as in a shared library, as \"call via PLT\". A function marked
\"(through a pointer)\" is there only because something takes or stores
its address.

The MEMORY table, for a file with several blocks of memory, has the bytes
of code and data that SYMBOL and what it reaches take in each block, as
the overview lists them, and the sections that hold them. Each item counts
once, where the program uses it, not where a load image stores it, and a
heap or stack that code uses, as _sbrk does, counts as data.

The table of items lists SYMBOL, then what it reaches by block, functions
then data objects, largest first, then the calls and jumps to code outside
any function, as in a chip's ROM, and the imports. Sizes marked ~ are
estimates, and ? marks a function whose end elfq doesn't know. It shows
the 30 largest items, and rows such as \"9 more data objects in
0x20000000..0x20000450\" sum up the rest of a block's; --all shows every
row. Calls through pointers, which elfq can't follow, are named below it,
since SYMBOL may reach more through them.

--tree draws the items as a tree instead: each under the item that uses
it, in the order that item first uses them, with veneers and PLT stubs as
steps of their own. It shows the 30 largest items and the items on their
chains from SYMBOL, and rows such as \"4 more\" sum up the rest under an
item. --json has the whole tree.

--to RANGE lists only what SYMBOL reaches in RANGE, SYMBOL included, and
can be given again for more: reach SysTick_Handler --to 0x30000000+16M
lists what the handler runs and uses in flash at 0x30000000. Every row is
shown, with an AT column for the sites, and the row of an item that code
outside the ranges uses is the one that callees lists for that code with
--to, so that it names the code to change; only a veneer whose target is
in the ranges too has a row that callees doesn't list. SYMBOL's row, if it
is in the ranges, comes first, then the calls and jumps that elfq can't
follow, kept as callees --to keeps them: one to a known address, when the
address is in the ranges, and one through a pointer, when the ranges hold
a function whose address is taken or stored other than in the vector
table. --tree draws the chains from SYMBOL to what is in the ranges and to
those calls and jumps, and --json has the items on them, with in_to
saying which are in the ranges.

Without SYMBOL, reach lists where the program starts, and what each start
reaches, as reach START counts it: main, the entry point, the handlers in
the vector table, in the order of their first entries, the functions that
.preinit_array, .init_array and .fini_array list, in that order and each
in its array's, then the other roots, such as personality routines, but
not the PLT's header. It shows main, the entry point, the handlers, and of
the other starts, those that reach the most bytes, 30 rows in all, and
rows such as \"650 more\" count the rest of a kind; --all shows every row.
Starts whose end elfq doesn't know, such as _init, are named below the
table. A shared library's main isn't a start, nor yet are the functions it
exports. With several blocks of memory, each block has a column, headed by
its start, with the bytes of code and data together; otherwise CODE and
DATA have them, and --json has both for each block. WHY names the
exceptions that a handler handles, but not the entries that the
architecture reserves, the first two and how many more, even with --all;
--json has every one. A start marked \"(may reach more)\" reaches calls or
jumps that elfq can't follow, or code that it can't read, which reach START
names. --to and --tree need SYMBOL.

Exit status, with --to: 1 if the answer lists an item, or code that elfq
can't follow or read; 0 if not; and 2 for an error.";

const BROWSE: &str = "\
browse opens a page for SYMBOL, or without it, the start page: what the
file is and its blocks of memory, as the overview says them, then where the
program starts, as reach lists it without SYMBOL, with every start and that
command under them; Enter opens a start's page. When SYMBOL could mean
several items, or none, the go-to box opens over the start page with SYMBOL
typed in.

A function's page shows its code, one line per instruction, and a line
that calls, jumps to or uses another item ends with a note that names it,
in the words of the HOW column. Enter follows the selected line's link to
that item's page, Esc goes back, and ] goes forward again; g goes to any
function, data object or import, and ? lists every key. elfq prints x86-64
code, and the Thumb code of Cortex-M cores up to Armv7E-M (M0, M0+, M3, M4,
M7); for other Arm cores, a page lists only the lines that use another item.

A data object's page shows its contents as far as elfq reads them: a
vector table's entries, named by exception, each word that holds a pointer,
as callees SYMBOL lists them, with a note that links to what it points to,
or, for an object that holds only strings, each string, one a line; the
status line shows in full a string too long for its line. A pointer's
value is what the program finds there: in a shared library, the address
that the dynamic loader writes, which the file may not hold. Under them is
the objdump -s command that prints its bytes, since elfq prints no hex
dump, unless its section, such as .bss, takes no room in the file.

The other tabs show what callees SYMBOL, callers SYMBOL, reach SYMBOL, why
SYMBOL and retained SYMBOL print about the item, but with every row, and
under them that command, ready to paste: 2 what the item calls or uses, 3
what calls or uses it, 4 everything it calls or uses at any depth, by block
of memory, 5 the chains from where the program starts that keep it, and 6
what removing it would save, with, in a file with several blocks of memory,
the bytes in each, as retained SYMBOL --range BLOCK counts them; 1 shows
the code or contents again. Enter on a row opens that item's page on the
same tab, so that Enter after Enter walks down the callees or up the
callers, and from the callers its code is already at the call. The page of
a data object whose contents have no rows opens on its callers, as an
import's does, and a tab that an item can't have says why, as its command
does; an import that has a PLT stub opens as the stub's page, since its
name finds the stub as SYMBOL.

An item's header also says what retained SYMBOL and why SYMBOL say about
it: what removing it would save, the item that keeps it, after \"kept by\",
and where the screen has room, the shortest chain to it from where the
program starts, which leaves out names at its start to fit. u opens the
page of the item that keeps it, on the same tab. In a large program,
browse works these out once, after the first page is drawn, and the status
line says so meanwhile. They aren't there in a shared library, where those
commands don't answer yet, and nor are tabs 5 and 6.

/ finds text in the page's rows, ignoring case: Enter selects the first
line that has it, from the selected one on, and n and N the next and the
one before, going round the end; the status line says which of how many it
is. Esc closes the prompt.

Long C++ and Rust names are shortened to fit the screen: paths inside
brackets keep their last part, and brackets become <…> or (…), but rows
that would then read the same show where they differ. The line under the
page gives the selected row's full name, cut at the edge of the screen.

browse needs a terminal, and takes no --json, --tree or --range. For
scripts, callers and callees print the same links, with --json, and
objdump --start-address prints the instructions.";

const DIFF: &str = "\
diff compares the memory that FILE and NEW load: each section that the
program has in memory, at its address, with a section such as .bss, which
the file doesn't store, as zeros, and each load image, the bytes that the
file stores for startup code to copy to a section, at its address. Symbols,
debug info and where the file keeps its bytes don't count, so a build
holds the same memory as its stripped copy. Notes that the program loads,
such as .note.gnu.build-id, do count, and the linker makes the build ID
from the whole file, debug info included. .tbss, which each thread has a
copy of, counts by its name, address and size.

When both load the same bytes at the same addresses, diff says so, and
nothing else. Otherwise the MEMORY table lists the blocks of memory, as
the overview does, but worked out over both files, so that both have the
same blocks. OLD and NEW are each file's bytes in the block, from its
first section or load image there to the end of its last, gaps included,
and CHANGE is NEW less OLD. SECTIONS names the 3 sections or load images
whose size changed the most, in address order, each with its change, or
says \"bytes differ\" when none changed size but bytes did. A block that
only one file has is marked (new) or (gone), and SECTIONS names what is
in it. For files with one block, a line says how its size changed instead.

When both files have a symbol table, diff pairs their functions and data
objects, by name, by bytes, by what uses them or by where they are, and
the first line counts those that moved to another block, are new or gone,
grew or shrank, or are edited: an item of the same size, in the same
block, whose bytes changed other than where they say where something is,
such as the distance in a call to a function that moved, or that now uses
different items than before. MOVED lists those that moved, but all the
items of whole sections that moved together are one row. CHANGE lists the
rest by block, largest first, edited items last: a new item and the new
items in its block that only it keeps are one row, as are a gone item and
the gone items in its block that only it kept; data objects without a
name whose estimated size changed fold into one row per block, and so do
new and gone items of one name. It shows 30 rows, the largest of each
block among them, and a row that counts the items in the rest of each
block; --all shows every item. A row ends with what keeps the item, as
top's rows do, and says where the compiler inlined a gone function when
NEW's DWARF says. With several blocks, a table lists every call between
blocks that is new or gone, as callees BLOCK lists it in the file that has
it, and a line counts the other uses between blocks that are new or gone,
of data and of functions' addresses: an item that moved to another block
has its uses between blocks before gone, and those after new, but a call
stays the same when it only becomes a tail call or stops being one, or
gains or loses a veneer or PLT stub in the caller's or the callee's block.
When no item changed, or a file has no symbol table, so that no row says
why the memory differs, the last line names every section whose address,
size, load image or bytes differ, or that only one file has, such as one
with a new build ID: 10 of them, or with --all, every one.

--json has every block, with each section and load image in it and the
bytes of its items, of its sections without items and of its padding,
which add up to its change; every section that differs; every item that
changed; and every use between blocks that is new or gone.

Exit status: 0 when FILE and NEW load the same bytes at the same
addresses, 1 when they don't, and 2 for an error, as when a file is a
separate debug file, which doesn't store the program's code. For the JSON
whatever the answer, as in CI:

  elfq old.elf diff new.elf --json > diff.json || [ $? -eq 1 ]";

const WHY: &str = "\
The chains start where the program does, as HOW says: at the entry point, a
function the C runtime runs at startup, or a handler in the vector table.
Every other row is used by the row above it, or where chains branch, by the
item it names, as in \"process_frame, used by main\", and the chains end at
SYMBOL. Only the shortest chain through each item that uses SYMBOL is
shown, for the 10 nearest, or for every one with --all, and --tree draws
the chains as a tree. Items that nothing reaches, or that only SYMBOL leads
to, don't keep it, and are named below the chains.

The last line names the closest item that every chain goes through, if
there is one: SYMBOL counts toward that item's retained size.";

const TOP: &str = "\
An item's retained size is its own size plus the sizes of the items that
only it leads to, from where the program starts: what removing it would
save. A row such as \"crc_table, kept by crc32\" names the closest item that
every path to it goes through. The rows that name none are where the
program starts, or are reached from more than one of those places. The
table shows the 30 largest items, or every item with --all; rows such as
\"6 more, kept by main\" sum up the rest, and --tree draws the rows as a
tree.

Nothing calls a function marked \"(through a pointer)\" directly, so it counts
for the code or data that holds its address. With --range, a row marked
\"(load image)\" counts the copy of an item that the file stores in the
range for startup code to copy where the program uses it, and one marked
\"(with its load image)\" counts both copies. Sizes marked ~ are estimates.";

const INLINED: &str = "\
The compiler inlines a function by putting a copy of its code into the
function that calls it. Only DWARF debug info says where it did that, so
this command needs a file built with debug info, such as with -g.

In the list, a function's bytes leave out the functions inlined into its
copies in turn, which have rows of their own. For SYMBOL, a row's bytes
include the functions inlined into it, whose rows name it, as in
\"atomic_add, inlined into fetch_add\", or where fetch_add is inlined in
several places, \"atomic_add, inlined into fetch_add in update_stats\";
--tree draws the rows as a tree. A copy of 0 bytes is one the compiler
merged into the code around it.";

const RETAINED: &str = "\
SYMBOL's retained size is its own size plus the sizes of the items that only
it leads to, from where the program starts: what removing it would save. A
row such as \"crc_table, kept by crc32\" names the closest item that every
path to it goes through. The table shows the 30 largest items, or every
item with --all; rows such as \"6 more, kept by crc32\" sum up the rest, and
--tree draws the rows as a tree.

Nothing calls a function marked \"(through a pointer)\" directly, so it counts
for the code or data that holds its address. With --range, a row marked
\"(load image)\" counts the copy of an item that the file stores in the
range for startup code to copy where the program uses it, and one marked
\"(with its load image)\" counts both copies. Sizes marked ~ are estimates.

reach SYMBOL lists everything SYMBOL calls or uses, including what other
items use too.";

#[derive(Debug, Args)]
pub struct TopArgs {
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct RetainedArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct WhyArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700
    pub symbol: String,
    /// Show the chains through every item that keeps SYMBOL, not only the
    /// 10 nearest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct UsesArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Or one or more ranges of memory, START..END or START+SIZE, to list
    /// only what crosses their edge
    // Not required here: commands::check_arguments says what is missing,
    // and with --range, what to type instead.
    #[arg(value_name = "SYMBOL|RANGE")]
    pub symbols: Vec<String>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct CallersArgs {
    #[command(flatten)]
    pub uses: UsesArgs,
    /// List only the callers in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = from_range)]
    pub from: Vec<AddressRange>,
    // Hidden, so that commands::check_arguments can say that --to goes with
    // callees, in place of clap's error.
    #[arg(long = "to", hide = true, value_name = "RANGE")]
    pub misplaced_to: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CalleesArgs {
    #[command(flatten)]
    pub uses: UsesArgs,
    /// List only the callees in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = to_range)]
    pub to: Vec<AddressRange>,
    // As `CallersArgs::misplaced_to`.
    #[arg(long = "from", hide = true, value_name = "RANGE")]
    pub misplaced_from: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ReachArgs {
    /// A function or data object: its name, the end of its name
    /// (process_frame for app::process_frame), or an address such as 0x1700.
    /// Without it, reach lists where the program starts, and what each
    /// start reaches
    pub symbol: Option<String>,
    /// List only what SYMBOL reaches in this range of memory, START..END or
    /// START+SIZE; give it again for more
    #[arg(long, value_name = "RANGE", value_parser = to_range)]
    pub to: Vec<AddressRange>,
    /// Show every item, not only the 30 largest
    #[arg(long)]
    pub all: bool,
    // As `CallersArgs::misplaced_to`.
    #[arg(long = "from", hide = true, value_name = "RANGE")]
    pub misplaced_from: Vec<String>,
}

#[derive(Debug, Args)]
pub struct BrowseArgs {
    /// The function, data object or import to open first: its name, the
    /// end of its name, or an address in it
    pub symbol: Option<String>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// The other build of the program, to compare with FILE
    #[arg(value_name = "NEW")]
    pub new: PathBuf,
    /// Show every item and section that changed, not only the largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct InlinedArgs {
    /// Show the inlined code in one function instead: its name, the end of
    /// its name, or an address in it
    pub symbol: Option<String>,
    /// Show every function, not only the 30 largest
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// List the functions and data objects elfq found, and where it found each
    Items,
    /// List what the code run from reset does before main: calls, stores and loops
    Walk,
}

/// The names of the commands, for reading `elfq help COMMAND` and for
/// spotting a command typed before the file.
pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .collect()
}
