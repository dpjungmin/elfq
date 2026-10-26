//! One module per command, and what they share. This file runs the command
//! that the arguments name, and holds the checks that commands make before
//! they answer, and the rows and the table of `callers` and `callees`. How
//! elfq words things is in `text.rs`, and the shapes of its JSON in `json.rs`.

#[cfg(feature = "tui")]
pub mod browse;
mod callees;
mod callers;
mod debug_items;
mod inlined;
mod json;
mod overview;
mod ranges;
mod reach;
mod retained;
mod retained_tree;
pub mod symbol;
mod text;
mod top;
mod why;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use crate::arch;
use crate::binary::{AddressRange, Binary, Item, ItemKind, Source, memory_blocks};
use crate::cli::{BrowseArgs, Cli, Command, DebugCommand, Format, ReachArgs, UsesArgs};
use crate::graph::{Graph, Node, Reference};
use crate::render::{Align, Table, TreeRow, TreeShape};

/// The error for `--range` with a command that doesn't take it.
const RANGE_REFUSED: &str = "only the overview, top and retained take --range";

/// What an answer means for elfq's exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exit status 0.
    Answered,
    /// Something crosses the boundary of the ranges that `callers` or
    /// `callees` asked about, or may, or `reach --to` finds something in
    /// its ranges, or may: exit status 1, so that a script can check that
    /// code in a range stays in it.
    Crossed,
}

/// Whether the process ends once the command has answered, as the elfq
/// program does. Tests run many commands in one process, and leave it off.
static ENDS_AFTER_ANSWER: AtomicBool = AtomicBool::new(false);

/// Says that the process ends once the command has answered, so that what
/// the command has built needn't be freed.
pub fn end_after_answer() {
    ENDS_AFTER_ANSWER.store(true, Ordering::Relaxed);
}

/// A value that a command keeps until it has answered, such as the loaded
/// file or its graph. When the process ends after the answer, the value is
/// left for the system to take back with the rest of the process's memory,
/// at once, rather than freed a piece at a time, which takes a while in a
/// large file. A run of a command whose answer is thrown away, as
/// `range_error` runs `callers` to see whether it answers, goes in
/// [`freeing`], so that what it keeps is freed for the runs after it.
pub(crate) struct Kept<T>(Option<T>);

impl<T> Kept<T> {
    pub(crate) fn new(value: T) -> Kept<T> {
        Kept(Some(value))
    }

    /// The value, which is then freed as any other.
    pub(crate) fn into_inner(mut self) -> T {
        self.0.take().expect("only dropping takes a kept value")
    }
}

impl<T> Deref for Kept<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.0.as_ref().expect("only dropping takes a kept value")
    }
}

impl<T> DerefMut for Kept<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("only dropping takes a kept value")
    }
}

impl<T> Drop for Kept<T> {
    fn drop(&mut self) {
        if ENDS_AFTER_ANSWER.load(Ordering::Relaxed) {
            std::mem::forget(self.0.take());
        }
    }
}

/// Runs `trial`, whose answer is thrown away, freeing what it keeps in a
/// [`Kept`] once it is done with it.
fn freeing<R>(trial: impl FnOnce() -> R) -> R {
    let ends = ENDS_AFTER_ANSWER.swap(false, Ordering::Relaxed);
    let result = trial();
    ENDS_AFTER_ANSWER.store(ends, Ordering::Relaxed);
    result
}

/// Runs the command in `cli`, writing its answer to `out`.
pub fn run(cli: &Cli, out: &mut dyn Write) -> Result<Outcome> {
    check_arguments(cli)?;
    // The browser takes over the terminal, so it loads the file itself,
    // and prints the warnings once it gives the terminal back.
    if let Some(Command::Browse(args)) = &cli.command {
        return run_browse(cli, args).map(|()| Outcome::Answered);
    }
    let binary = Kept::new(Binary::load(&cli.file)?);
    write_warnings(&binary);
    // `check_arguments` leaves `--range` with `callers` and `callees` until
    // elfq can tell which command to suggest.
    if let Some(range) = cli.range
        && let Some(command) = &cli.command
        && let Some(uses) = uses_args(command)
    {
        bail!("{}", range_error(cli, &binary, command, &uses, range)?);
    }
    let format = cli.format();
    let range = cli.range;
    let answered = |result: Result<()>| result.map(|()| Outcome::Answered);
    match &cli.command {
        None => answered(overview::run(&cli.file, &binary, range, format, out)),
        Some(Command::Top(args)) => answered(top::run(&binary, range, args.all, format, out)),
        Some(Command::Retained(args)) => answered(retained::run(
            &binary,
            &args.symbol,
            range,
            args.all,
            format,
            out,
        )),
        Some(Command::Why(args)) => {
            answered(why::run(&binary, &args.symbol, args.all, format, out))
        }
        Some(Command::Inlined(args)) => answered(inlined::run(
            &cli.file,
            &binary,
            args.symbol.as_deref(),
            args.all,
            format,
            out,
        )),
        Some(Command::Callers(args)) => callers::run(
            &binary,
            &args.uses.symbols,
            &args.from,
            args.uses.all,
            format,
            out,
        ),
        Some(Command::Callees(args)) => callees::run(
            &binary,
            &args.uses.symbols,
            &args.to,
            args.uses.all,
            format,
            out,
        ),
        Some(Command::Reach(args)) => match &args.symbol {
            Some(symbol) => reach::run(&binary, symbol, &args.to, args.all, format, out),
            None => reach::run_starts(&cli.file, &binary, &args.to, range, args.all, format, out),
        },
        Some(Command::Debug(DebugCommand::Items)) => answered(debug_items::run(&binary, out)),
        Some(Command::Browse(_)) => unreachable!("browse has run above"),
    }
}

/// Prints the file's warnings to stderr.
fn write_warnings(binary: &Binary) {
    for warning in &binary.warnings {
        // Not `eprintln!`, which panics when nothing reads standard error
        // any more: the answer still counts.
        let _ = writeln!(std::io::stderr(), "warning: {warning}");
    }
}

/// Runs `elfq FILE browse`, in a build with the browser.
#[cfg(feature = "tui")]
fn run_browse(cli: &Cli, args: &BrowseArgs) -> Result<()> {
    browse::run(&cli.file, args.symbol.as_deref())
}

/// Says that this build has no browser.
#[cfg(not(feature = "tui"))]
fn run_browse(_: &Cli, _: &BrowseArgs) -> Result<()> {
    bail!("this elfq was built without browse (the tui feature)")
}

/// Refuses arguments that don't go together, before the file is loaded, so
/// that a mistake is reported at once, even for a large file.
fn check_arguments(cli: &Cli) -> Result<()> {
    // browse shows its pages on the screen, and prints no answer for these
    // to change.
    if let Some(Command::Browse(_)) = &cli.command
        && (cli.json || cli.tree || cli.range.is_some())
    {
        bail!("browse takes no --json, --tree or --range");
    }
    if cli.tree {
        check_tree(cli)?;
    }
    if let Some(command) = &cli.command
        && let Some(uses) = uses_args(command)
    {
        let name = command_name(command);
        if let Some((misplaced, belongs_to)) = uses.misplaced {
            bail!(
                "{misplaced} goes with {belongs_to}; {name} takes {} to list only the {name} \
                 in a range",
                uses.option
            )
        }
        if cli.range.is_some() {
            let symbols = &uses.args.symbols;
            if symbols.len() > 1
                && let Some(query) = symbols.iter().find(|query| !symbol::is_range(query))
            {
                bail!("`{query}` isn't a range; give one SYMBOL, or ranges such as 0x0+256K")
            }
            // Which command can take its place depends on what the file
            // holds: see `range_error`.
            return Ok(());
        }
        if uses.args.symbols.is_empty() {
            bail!("{name} needs a SYMBOL, or ranges such as 0x0+256K")
        }
    }
    if let Some(Command::Reach(args)) = &cli.command {
        check_reach_arguments(cli, args)?;
    }
    match &cli.command {
        None | Some(Command::Top(_) | Command::Retained(_)) => Ok(()),
        Some(Command::Reach(args)) if args.symbol.is_none() => Ok(()),
        Some(_) if cli.range.is_some() => bail!("{RANGE_REFUSED}"),
        Some(Command::Debug(_)) if cli.format() == Format::Json => {
            bail!("the debug commands print only text")
        }
        Some(_) => Ok(()),
    }
}

/// Refuses what `reach` doesn't take: `--from`, ranges in place of SYMBOL,
/// and `--range`, each with what to type instead. Without SYMBOL, `--to`,
/// `--range` and `--tree` wait for the file to load, so that the command to
/// type instead can name a function in it (see `reach::run_starts`).
fn check_reach_arguments(cli: &Cli, args: &ReachArgs) -> Result<()> {
    if !args.misplaced_from.is_empty() {
        bail!("--from goes with callers; reach takes --to to list only what it reaches in a range")
    }
    let Some(query) = &args.symbol else {
        return Ok(());
    };
    let file = text::shell_quote(&cli.file.display().to_string());
    let json = if cli.json { " --json" } else { "" };
    let all = if args.all { " --all" } else { "" };
    let to: String = args
        .to
        .iter()
        .map(|&range| format!(" --to {}", text::range_argument(range)))
        .collect();
    if symbol::is_range(query) {
        // What a range uses outside it, or in other ranges, is a range
        // check, which callees has.
        let callees = match query.parse::<AddressRange>() {
            Ok(range) => {
                let what = if to.is_empty() {
                    "what a range uses outside it"
                } else {
                    "what a range uses in the --to ranges"
                };
                format!(
                    "; for {what}: elfq {file} callees {}{to}{json}",
                    text::range_argument(range)
                )
            }
            Err(_) => String::new(),
        };
        bail!("reach takes one SYMBOL, not ranges{callees}")
    }
    if let Some(range) = cli.range {
        let tree = if cli.tree { " --tree" } else { "" };
        bail!(
            "reach takes --to, not --range, to list only what it reaches in a range: elfq {file} \
             reach {}{to} --to {}{all}{tree}{json}",
            text::shell_quote(query),
            text::range_argument(range)
        )
    }
    Ok(())
}

/// Refuses `--tree` where there is no tree to draw: with `--json`, and for
/// commands that print a list.
fn check_tree(cli: &Cli) -> Result<()> {
    if cli.json {
        bail!("--tree draws trees in the text, so it doesn't go with --json");
    }
    match &cli.command {
        None
        | Some(Command::Top(_) | Command::Retained(_) | Command::Why(_) | Command::Reach(_)) => {
            Ok(())
        }
        Some(Command::Inlined(args)) if args.symbol.is_some() => Ok(()),
        Some(_) => {
            bail!(
                "only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree"
            )
        }
    }
}

/// The error for `--range` with `callers` or `callees` (see
/// [`range_hint`]), suggesting only the commands that answer for `binary`:
/// elfq runs them to find out, which costs time only on this mistake. When
/// none does, and the command fails without `--range` too, that error comes
/// first, since no form of the command answers until it is fixed.
fn range_error(
    cli: &Cli,
    binary: &Binary,
    command: &Command,
    uses: &Uses,
    range: AddressRange,
) -> Result<String> {
    check_code_is_readable(binary)?;
    let symbols = &uses.args.symbols;
    let ranges = symbols.iter().all(|query| symbol::is_range(query));
    if !ranges {
        // A mistyped name is the first thing to fix.
        freeing(|| symbol::subject(binary, symbols))?;
    }
    let answer = |symbols: &[String], other: &[AddressRange]| {
        let sink = &mut std::io::sink();
        freeing(|| match command {
            Command::Callers(_) => callers::run(binary, symbols, other, false, Format::Json, sink),
            _ => callees::run(binary, symbols, other, false, Format::Json, sink),
        })
    };
    let added =
        ranges && answer(&[&symbols[..], &[range.to_string()]].concat(), uses.other).is_ok();
    let limited = !symbols.is_empty() && answer(symbols, &[uses.other, &[range]].concat()).is_ok();
    if !added && !limited {
        if !symbols.is_empty() {
            answer(symbols, uses.other)?;
        }
        return Ok(RANGE_REFUSED.to_string());
    }
    Ok(range_hint(
        cli,
        command_name(command),
        uses,
        range,
        added,
        limited,
    ))
}

/// The error for `--range` with `callers` or `callees`, which don't take
/// it, with the commands to type: with ranges in place of SYMBOL, or none,
/// the one that adds `range` to them, when `added`, and the one that lists
/// only the uses in `range`, with `--from` or `--to`, when `limited`. Keeps
/// what `uses` has of those. One of them must be given.
fn range_hint(
    cli: &Cli,
    name: &str,
    uses: &Uses,
    range: AddressRange,
    added: bool,
    limited: bool,
) -> String {
    let option = uses.option;
    let file = text::shell_quote(&cli.file.display().to_string());
    let command = |args: Vec<String>| {
        let mut words = vec!["elfq".to_string(), file.clone(), name.to_string()];
        words.extend(args);
        if cli.json {
            words.push("--json".to_string());
        }
        words.join(" ")
    };
    let symbols = &uses.args.symbols;
    let given: Vec<String> = symbols
        .iter()
        .map(|query| text::shell_quote(query))
        .collect();
    let kept: Vec<String> = uses
        .other
        .iter()
        .flat_map(|&other| [option.to_string(), text::range_argument(other)])
        .collect();
    let range_text = text::range_argument(range);
    let limited_command =
        command([&given[..], &kept, &[option.to_string(), range_text.clone()]].concat());
    if !added || !symbols.iter().all(|query| symbol::is_range(query)) {
        return format!(
            "{name} takes {option}, not --range, to list only the {name} in a range: \
             {limited_command}"
        );
    }
    let added_command = command([&given[..], std::slice::from_ref(&range_text), &kept].concat());
    let mut error =
        format!("{name} takes ranges in place of SYMBOL, not with --range: {added_command}");
    // With `--from` or `--to` given, the command that adds `range` to them
    // lists the uses in theirs too.
    if limited && !given.is_empty() && kept.is_empty() {
        error += &format!("; to list only the {name} in {range_text}: {limited_command}");
    }
    error
}

/// What `callers` or `callees` was given.
struct Uses<'a> {
    args: &'a UsesArgs,
    /// The option that lists only the uses whose other end is in some
    /// ranges: `--from` or `--to`.
    option: &'static str,
    /// Its ranges.
    other: &'a [AddressRange],
    /// The other command's option, when it was given by mistake, and that
    /// command: `--to` and `callees` for `callers`.
    misplaced: Option<(&'static str, &'static str)>,
}

/// What `command` was given, when it is `callers` or `callees`.
fn uses_args(command: &Command) -> Option<Uses<'_>> {
    match command {
        Command::Callers(args) => Some(Uses {
            args: &args.uses,
            option: "--from",
            other: &args.from,
            misplaced: (!args.misplaced_to.is_empty()).then_some(("--to", "callees")),
        }),
        Command::Callees(args) => Some(Uses {
            args: &args.uses,
            option: "--to",
            other: &args.to,
            misplaced: (!args.misplaced_from.is_empty()).then_some(("--from", "callers")),
        }),
        _ => None,
    }
}

/// The name of `command` as typed: `callers` for [`Command::Callers`].
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Top(_) => "top",
        Command::Retained(_) => "retained",
        Command::Why(_) => "why",
        Command::Inlined(_) => "inlined",
        Command::Callers(_) => "callers",
        Command::Callees(_) => "callees",
        Command::Reach(_) => "reach",
        Command::Browse(_) => "browse",
        Command::Debug(_) => "debug",
    }
}

/// Builds the reference graph, or explains why elfq can't yet.
fn reference_graph(binary: &Binary) -> Result<Graph> {
    check_code_is_readable(binary)?;
    Ok(Graph::build(binary))
}

/// Explains why elfq can't build the reference graph yet, if it can't.
fn check_code_is_readable(binary: &Binary) -> Result<()> {
    if !arch::is_supported(binary.architecture, binary.thumb) {
        bail!("{}", text::cannot_read_code(binary));
    }
    Ok(())
}

/// The index of `node`'s item, or an error that says it is an import,
/// whose code is in another file.
fn item_in_file(binary: &Binary, node: Node) -> Result<usize> {
    match node {
        Node::Item(index) => Ok(index),
        Node::Import(_) => bail!(
            "{} is imported from a shared library, so its code isn't in this file",
            node.name(binary)
        ),
    }
}

/// The index of `node`'s item, or why elfq can't read its code: it is an
/// import, or a function whose end elfq doesn't know.
fn readable_item(binary: &Binary, node: Node) -> Result<usize> {
    let index = item_in_file(binary, node)?;
    let item = &binary.items[index];
    if item.kind == ItemKind::Function && item.size == 0 {
        bail!(
            "elfq doesn't know where {} ends, so it can't read its code",
            item.name
        );
    }
    Ok(index)
}

/// Explains why there are no bytes to count in the file, or with a range,
/// in it, if there aren't: a range that no section or load image is in is
/// most likely mistyped, in some stripped files elfq can't find the
/// functions yet, and hand-written assembly often leaves out the sizes.
fn check_has_bytes(binary: &Binary, range: Option<AddressRange>) -> Result<()> {
    if total(binary.items.iter().map(|item| binary.bytes_in(item, range))) > 0 {
        return Ok(());
    }
    let place = empty_place(binary, range)?;
    let starts_in_place = |item: &&Item| {
        range.is_none_or(|range| {
            [Some(item.address), binary.load_address(item)]
                .into_iter()
                .flatten()
                .any(|start| range.contains(start))
        })
    };
    if let Some(item) = binary.items.iter().find(starts_in_place) {
        bail!(
            "elfq doesn't know where {} ends, so it can't count the bytes in {place}",
            item.name
        );
    }
    bail!("elfq found no functions or data in {place}")
}

/// Where an answer that found nothing looked, for its error: `range`, such
/// as `0x00000000..0x000000e0 (.itcm)`, or without one, "the file". Fails
/// instead when no section or load image is in `range`, which is then most
/// likely mistyped, saying where to look: the file's one block of
/// addresses, or the overview, which lists them when there are several. It
/// also fails when elfq can't find the file's functions yet.
fn empty_place(binary: &Binary, range: Option<AddressRange>) -> Result<String> {
    let place = match range {
        Some(range)
            if text::sections_in(binary, range).is_empty()
                && text::load_images_in(binary, range).is_empty() =>
        {
            let range = text::range_argument(range);
            match &memory_blocks(&binary.sections)[..] {
                [] => bail!("the file has nothing in {range}"),
                [block] => bail!(
                    "the file has nothing in {range}, only in {}",
                    text::range_argument(block.range)
                ),
                _ => bail!("the file has nothing in {range}; the overview lists its ranges"),
            }
        }
        Some(range) => text::range_text(binary, range, false),
        None => "the file".to_string(),
    };
    if !found_functions(binary) {
        bail!("{}", text::NO_FUNCTIONS);
    }
    Ok(place)
}

/// Whether elfq found the file's functions: a symbol table or an unwind
/// table lists them, or in firmware elfq followed the code (see
/// `unnamed_functions.rs`). Otherwise it has only the entry point, and
/// doesn't know its size.
fn found_functions(binary: &Binary) -> bool {
    binary.items.iter().any(|item| match item.source {
        Source::EntryPoint | Source::Reached => item.size > 0,
        source => source.lists_functions(),
    })
}

/// One line of a callers or callees table: the references between one pair
/// of nodes that are worded the same way.
struct Row {
    /// The node on the other end: the caller, or the callee.
    node: Node,
    /// How the node is used, in the words that [`rows`] was given: the HOW
    /// column's, or JSON's.
    how: &'static str,
    sites: Vec<u64>,
}

/// Groups references into rows, in the order each row first appears.
/// `other_end` picks the node that the table lists for each reference, and
/// `how` says how it is used: references with the same words share a row.
fn rows<'a>(
    references: impl IntoIterator<Item = &'a Reference>,
    other_end: impl Fn(&Reference) -> Node,
    how: impl Fn(&Reference) -> &'static str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Where each row is in `rows`, since a function can have thousands.
    let mut index: HashMap<(Node, &'static str), usize> = HashMap::new();
    for reference in references {
        let node = other_end(reference);
        let how = how(reference);
        let row = *index.entry((node, how)).or_insert_with(|| {
            rows.push(Row {
                node,
                how,
                sites: Vec::new(),
            });
            rows.len() - 1
        });
        rows[row].sites.push(reference.site);
    }
    rows
}

/// The distinct nodes in `rows`, in order.
fn distinct_nodes(rows: &[Row]) -> Vec<Node> {
    let mut seen = HashSet::new();
    rows.iter()
        .map(|row| row.node)
        .filter(|&node| seen.insert(node))
        .collect()
}

/// Writes the table of `callers` or `callees`: HOW, AT, and the caller or
/// callee under `column`. It lists the rows of the `count` largest nodes
/// (see [`largest_rows`]), and a last row such as "70 more" counts the
/// rest.
fn write_uses_table(
    binary: &Binary,
    rows: Vec<Row>,
    column: &'static str,
    count: usize,
    out: &mut dyn Write,
) -> Result<()> {
    let (rows, left_out) = largest_rows(binary, rows, count);
    let mut table = Table::new(&[
        ("HOW", Align::Left),
        ("AT", Align::Left),
        (column, Align::Left),
    ]);
    for row in &rows {
        table.row(vec![
            row.how.to_string(),
            text::at_column(&row.sites),
            row.node.name(binary).to_string(),
        ]);
    }
    if left_out > 0 {
        table.row(vec![
            String::new(),
            String::new(),
            format!("{left_out} more"),
        ]);
    }
    table.write(out)?;
    Ok(())
}

/// The rows of the `count` nodes in `rows` with the largest own sizes
/// ([`Node::size`]), in the order they come in, and how many nodes are left
/// out. A node that would be left out on its own is kept too, since its rows
/// take no more room than "1 more" would. Of nodes the same size, the name
/// that sorts first is kept.
fn largest_rows(binary: &Binary, rows: Vec<Row>, count: usize) -> (Vec<Row>, usize) {
    let mut nodes = distinct_nodes(&rows);
    let shown = text::shown_count(nodes.len(), count);
    let left_out = nodes.len() - shown;
    if left_out == 0 {
        return (rows, 0);
    }
    nodes.sort_by_key(|&node| (Reverse(node.size(binary)), node.name(binary), node));
    let kept: HashSet<Node> = nodes.into_iter().take(shown).collect();
    let rows = rows
        .into_iter()
        .filter(|row| kept.contains(&row.node))
        .collect();
    (rows, left_out)
}

/// The text of an answer whose table holds a tree, as `why` and `retained`
/// write it: its first lines, the table, and the notes under it, each
/// after an empty line.
struct TreeAnswer {
    /// The first lines, or what the answer says in place of a table.
    lead: Vec<String>,
    /// The table's columns, the last of which holds the tree.
    columns: Vec<(&'static str, Align)>,
    /// The rows at the top of the tree. Empty when the answer has no table.
    rows: Vec<TreeRow>,
    /// How the table shows the tree, which the rows were made for, since
    /// rows find the short names they show only when the tree isn't drawn.
    shape: TreeShape,
    notes: Vec<String>,
}

impl TreeAnswer {
    /// An answer of sentences, without a table, whose shape doesn't matter.
    fn sentences(lead: Vec<String>) -> TreeAnswer {
        TreeAnswer {
            lead,
            columns: Vec::new(),
            rows: Vec::new(),
            shape: TreeShape::Drawn,
            notes: Vec::new(),
        }
    }

    /// What [`TreeAnswer::write`] writes.
    #[cfg(feature = "tui")]
    fn written(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out).expect("writing to memory");
        out
    }

    fn write(&self, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if !self.rows.is_empty() {
            writeln!(out)?;
            let mut table = Table::new(&self.columns);
            table.tree(&self.rows, self.shape);
            table.write(out)?;
        }
        for note in &self.notes {
            writeln!(out)?;
            writeln!(out, "{note}")?;
        }
        Ok(())
    }
}

/// An answer of `why` or `retained` as a tab of browse shows it.
#[cfg(feature = "tui")]
struct TabAnswer {
    /// The answer with every row and name, as with `--all`.
    answer: TreeAnswer,
    /// Its notes as the command words them without `--all`, which name
    /// fewer items.
    brief: Vec<String>,
    /// Its notes naming only the first item of each list, shortened, for a
    /// screen with little room for them.
    briefest: Vec<String>,
    /// Whether the command needs `--all` to print all of `answer`.
    cut: bool,
}

#[cfg(feature = "tui")]
impl TabAnswer {
    /// An answer of sentences, the same with `--all` and without.
    fn sentences(lead: Vec<String>) -> TabAnswer {
        TabAnswer {
            answer: TreeAnswer::sentences(lead),
            brief: Vec::new(),
            briefest: Vec::new(),
            cut: false,
        }
    }
}

/// The sum of `sizes`, which stops at the largest `u64` rather than
/// overflow: in a damaged file, items or inlined copies can overlap, and
/// their sizes add up to more than a `u64` holds.
fn total(sizes: impl Iterator<Item = u64>) -> u64 {
    sizes.fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_file_whose_functions_elfq_cant_find_says_so() {
        // Such as stripped firmware for a processor whose code elfq can
        // read but not follow yet: it has only its entry point.
        let path = Path::new("testdata/bin/c-cortex-m4-stripped.elf");
        let mut binary = Binary::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        binary
            .items
            .retain(|item| item.source == Source::EntryPoint);
        binary.items[0].size = 0;
        let mut out = Vec::new();
        overview::run(path, &binary, None, Format::Text, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
1 function
elfq can't yet find functions without symbols or an unwind table.

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000e0c  3596  .isr_vector .text .rodata .ARM.exidx, load image of .data
0x20000000..0x20000048    72  .data .bss
"
        );
        let error = top::run(&binary, None, false, Format::Text, &mut Vec::new()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "elfq can't yet find functions without symbols or an unwind table"
        );
    }

    #[test]
    fn a_long_table_keeps_the_rows_of_its_largest_items() {
        let path = Path::new("testdata/bin/c-x86_64.elf");
        let mut binary = Binary::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        for (index, (name, size)) in [("d", 20), ("b", 10), ("a", 10), ("c", 5)]
            .into_iter()
            .enumerate()
        {
            binary.items[index].name = name.to_string();
            binary.items[index].size = size;
        }
        let row = |node, how| Row {
            node,
            how,
            sites: Vec::new(),
        };
        binary.imports.push("write".to_string());
        let import = Node::Import(binary.imports.len() - 1);
        let (d, b, a, c) = (Node::Item(0), Node::Item(1), Node::Item(2), Node::Item(3));
        let rows = || {
            vec![
                row(c, "call"),
                row(import, "call"),
                row(a, "call"),
                row(b, "call"),
                row(d, "call"),
                row(c, "pointer"),
            ]
        };
        let kept = |count| {
            let (rows, left_out) = largest_rows(&binary, rows(), count);
            let rows: Vec<(&str, &str)> = rows
                .iter()
                .map(|row| (row.node.name(&binary), row.how))
                .collect();
            (rows, left_out)
        };

        // d and the first name of the two of size 10, in their order. The
        // import counts as size 0, and c's two rows count as one item.
        assert_eq!(kept(2), (vec![("a", "call"), ("d", "call")], 3));
        assert_eq!(
            kept(3),
            (vec![("a", "call"), ("b", "call"), ("d", "call")], 2)
        );
        // With room for all but one, the last is kept too.
        assert_eq!(kept(4).0.len(), 6);
        assert_eq!(kept(4).1, 0);
    }

    #[test]
    fn a_kept_value_is_freed_unless_the_process_ends_after_the_answer() {
        use std::rc::Rc;

        let value = Rc::new(());
        let count = || Rc::strong_count(&value);
        // Tests leave end_after_answer off, so a kept value is freed.
        drop(Kept::new(Rc::clone(&value)));
        assert_eq!(count(), 1);
        // Turned on, only a value kept in a run that goes in freeing is.
        // Other tests that run meanwhile only free less.
        end_after_answer();
        freeing(|| drop(Kept::new(Rc::clone(&value))));
        let freed = count();
        drop(Kept::new(Rc::clone(&value)));
        let left = count();
        ENDS_AFTER_ANSWER.store(false, Ordering::Relaxed);
        assert_eq!((freed, left), (1, 2));
    }
}
