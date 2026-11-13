//! The docs, checked against elfq: each code block whose first line is
//! `$ elfq ...` must show what elfq prints for it, given its arguments as a
//! shell passes them, without their quotes; design.md's JSON section must
//! name every key that elfq's JSON has; every path, module path and type
//! path that CONTRIBUTING.md and design.md cite must be there; and
//! `scripts/same-output.sh`, to which CONTRIBUTING.md says to add each
//! command, must run every command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use clap::CommandFactory;
use serde_json::Value;

use elfq::cli::Cli;

use crate::support::{
    Exit, SAMPLE, TCM_INLINED_SAMPLE, TCM_MOVED_SAMPLE, TCM_SAMPLE, assert_output, run, shell_words,
};

/// The docs that can hold examples, relative to the crate's root.
const DOCS: [&str; 3] = ["README.md", "docs/design.md", "docs/examples.md"];

#[test]
fn the_examples_in_the_docs_show_what_elfq_prints() {
    let mut examples = 0;
    for doc in DOCS {
        let path = format!("{}/{doc}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        // Every other piece between fences is a code block, which starts
        // with the rest of its opening fence's line, such as `json`.
        for block in text.split("```").skip(1).step_by(2) {
            let (_, block) = block.split_once('\n').unwrap();
            let Some(example) = block.strip_prefix("$ elfq ") else {
                continue;
            };
            let (command, expected) = example.split_once('\n').unwrap();
            // Shown with the output of a test that fails.
            println!("{doc}: $ elfq {command}");
            let args = shell_words(command);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            assert_output(&run(&args).printed(Exit::ZeroOrOne), expected);
            examples += 1;
        }
    }
    // Well under the number of examples in the docs, so that a change in
    // how blocks are written can't make this test check nothing.
    assert!(examples >= 10, "found {examples} examples");
}

#[test]
fn a_command_line_splits_as_a_shell_splits_it() {
    assert_eq!(
        shell_words("  retained 'uart_*'\t\"log(char const*)\" a\\ b '' "),
        ["retained", "uart_*", "log(char const*)", "a b", ""]
    );
}

/// Answers that between them have every key that elfq's JSON has. A key
/// shows only in an object that is there, so for each key that holds a
/// list, one of them has an entry in it, which the test checks.
const JSON_ANSWERS: [&[&str]; 29] = [
    &[TCM_SAMPLE],
    &[SAMPLE, "top"],
    &[SAMPLE, "retained", "process_frame"],
    &[SAMPLE, "retained", "process_frame", "report", "_init"],
    &[TCM_SAMPLE, "retained", "on_*"],
    &[SAMPLE, "why", "log_msg"],
    &[SAMPLE, "why", "parse_value"],
    &[SAMPLE, "inlined"],
    &[SAMPLE, "inlined", "process_frame"],
    &[SAMPLE, "callers", "log_msg"],
    &[TCM_SAMPLE, "callers", "Default_Handler"],
    &[SAMPLE, "callees", "dispatch_event"],
    &[SAMPLE, "callers", "0x1000+4K"],
    &[SAMPLE, "callees", "0x1000+4K"],
    &[TCM_SAMPLE, "callers", "0x0+256K"],
    &[TCM_SAMPLE, "callees", "0x0+256K"],
    &[
        TCM_SAMPLE,
        "callers",
        "0x20000000+256K",
        "--from",
        "0x0+256K",
    ],
    &[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x30000000+16M"],
    // Sets: a pattern's `matches`, and several SYMBOLs' `symbols`.
    &[TCM_SAMPLE, "callers", "on_*"],
    &[TCM_SAMPLE, "callees", "on_timer", "on_data"],
    &[TCM_SAMPLE, "reach", "SysTick_Handler"],
    &[SAMPLE, "reach"],
    &[TCM_SAMPLE, "reach"],
    &[
        TCM_SAMPLE,
        "reach",
        "SysTick_Handler",
        "--to",
        "0x30000000+16M",
    ],
    &[TCM_SAMPLE, "diff", TCM_MOVED_SAMPLE],
    &[TCM_SAMPLE, "diff", TCM_INLINED_SAMPLE],
    &["testdata/bin/c-cortex-m7-sysinit.elf", "startup"],
    &["testdata/bin/c-cortex-m7-tables.elf", "startup"],
    &["testdata/bin/c-cortex-m7-rom.elf", "startup"],
];

#[test]
fn design_md_names_every_json_key() {
    let path = format!("{}/docs/design.md", env!("CARGO_MANIFEST_DIR"));
    let design = std::fs::read_to_string(&path).unwrap();
    let (_, section) = design.split_once("\n### JSON\n").unwrap();
    let (section, _) = section.split_once("\n### ").unwrap();
    // Whether each list had an entry in any answer.
    let mut lists = BTreeMap::new();
    for args in JSON_ANSWERS {
        // Range checks exit with 1 when something crosses the ranges,
        // diff when the memory differs, and startup when it finds a
        // problem.
        let answer = run(&[args, &["--json"]].concat()).json(Exit::ZeroOrOne);
        let mut keys = BTreeSet::new();
        add_keys(&answer, &mut keys, &mut lists);
        for key in keys {
            assert!(
                section.contains(&format!("`{key}`")),
                "design.md's JSON section doesn't name `{key}`, from elfq {}",
                args.join(" ")
            );
        }
    }
    for (key, had_entries) in lists {
        assert!(had_entries, "no answer has an entry in `{key}`");
    }
}

/// Adds the keys of every object in `value` to `keys`, and notes in `lists`
/// whether each list it holds has entries.
fn add_keys(value: &Value, keys: &mut BTreeSet<String>, lists: &mut BTreeMap<String, bool>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                keys.insert(key.clone());
                if let Value::Array(values) = value {
                    *lists.entry(key.clone()).or_default() |= !values.is_empty();
                }
                add_keys(value, keys, lists);
            }
        }
        Value::Array(values) => values.iter().for_each(|value| add_keys(value, keys, lists)),
        _ => {}
    }
}

/// `scripts/same-output.sh` runs every command, as step 11 of
/// CONTRIBUTING.md's 'Adding a command' asks: `check_help` compares the
/// help of each, and `check_file` runs each `debug` command, whose help
/// `check_help` compares too.
#[test]
fn same_output_sh_runs_every_command() {
    let script = std::fs::read_to_string(crate_path("scripts/same-output.sh")).unwrap();
    // A function's body, without its comments, and with each line that
    // ends with `\` joined to the next.
    let body = |function: &str| {
        let (_, body) = script.split_once(&format!("\n{function}() {{\n")).unwrap();
        let (body, _) = body.split_once("\n}\n").unwrap();
        let is_code = |line: &&str| !line.trim_start().starts_with('#');
        let lines: Vec<&str> = body.lines().filter(is_code).collect();
        lines.join("\n").replace("\\\n", " ")
    };
    let (check_file, check_help) = (body("check_file"), body("check_help"));
    let (_, commands) = check_help.split_once("for command in ").unwrap();
    let (commands, _) = commands.split_once("; do").unwrap();
    let commands: Vec<&str> = commands.split_whitespace().collect();
    for command in elfq::cli::command_names() {
        let compared = commands.contains(&command.as_str());
        assert!(compared, "check_help doesn't compare `{command} --help`");
    }
    let cli = Cli::command();
    let debug = cli.find_subcommand("debug").unwrap();
    for command in debug.get_subcommands() {
        let command = format!("debug {}", command.get_name());
        let listed = check_file.contains(&format!("\"$file\" {command}"));
        assert!(listed, "check_file doesn't run `{command}`");
        let compared = check_help.contains(&format!(" {command} --help"));
        assert!(compared, "check_help doesn't compare `{command} --help`");
    }
}

/// The docs that say where the code is, whose paths
/// [`the_paths_the_docs_cite_are_there`] checks.
const MAPS: [&str; 2] = ["CONTRIBUTING.md", "docs/design.md"];

/// The folders, in the crate's root, whose paths the docs cite.
const ROOTS: [&str; 5] = ["src", "tests", "testdata", "scripts", "docs"];

/// The first names of the module paths that the docs cite but that aren't
/// elfq's: other crates', as `object::elf::SHT_NOBITS`, a type's, as
/// `u32::MAX`, and the C++ sample's, as `app::process_frame`. Add a crate
/// here when a doc cites a path into it.
///
/// Browse's `src/commands/browse/app.rs` is also named `app`, so paths into
/// it aren't checked; the docs name its items after its types, as
/// `App::pending`.
const OTHER_FIRST_NAMES: [&str; 7] = ["std", "core", "alloc", "object", "serde_json", "u32", "app"];

/// Every path, module path and type path that the [`MAPS`] cite is there.
///
/// A path that starts with one of the [`ROOTS`] must be a file or folder
/// there. A shorter path to a Rust file or a folder, such as
/// `views/item.rs` or `support/`, must end a path in them. A link's target
/// must be there, from the doc's own folder. A module path, such as
/// `symbol::find`, must name modules of `src/` or `tests/it/` up to its
/// last module, and the name after that must be defined in the module's
/// file, as `fn find` is, or imported there with `use`. A type path, such
/// as `Binary::load`, must name a function, constant, associated type,
/// field or variant in the type's definition, in `src/` or `tests/it/`, or
/// in an `impl` of it. The module paths and type paths in a code block are
/// checked too, as in design.md's map of the layers, unless it holds a
/// `$ elfq` line, whose output names the samples' items; there a module
/// path is checked only if it starts with a module of elfq's, since a Rust
/// snippet may name another crate's.
///
/// A type that elfq defines is read as elfq's. So a doc cites another
/// crate's type of the same name from the crate's name, as
/// `serde_json::Value::as_str`, with the crate among the
/// [`OTHER_FIRST_NAMES`], and a method that a derive or a trait's default
/// body gives a type after its trait, as `Parser::try_parse_from`.
///
/// Not checked: a path with a placeholder or a pattern in it, as
/// `src/arch/<name>.rs`; a path through a `target` folder, which a build
/// makes; a path to a file or folder in a code block; a module path that
/// starts with one of the [`OTHER_FIRST_NAMES`]; a type path whose type no
/// file of `src/` or `tests/it/` defines as a struct, enum or trait, as
/// another crate's `EndianSlice` or a type alias; and a name cited alone,
/// as `check_code_is_readable`. So a doc that names a file to add, or one
/// that is gone, writes it with a placeholder or in a code block.
#[test]
fn the_paths_the_docs_cite_are_there() {
    let tree = Tree::read();
    let mut checked = BTreeSet::new();
    let mut type_paths = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for doc in MAPS {
        let text = std::fs::read_to_string(crate_path(doc)).unwrap();
        for (cited, is_there) in tree.citations(doc, &text) {
            if !is_there {
                missing.insert(format!("{doc} cites `{cited}`, which isn't there"));
            }
            // A type path is the only citation that starts with a capital
            // and holds `::`.
            if cited.starts_with(|c: char| c.is_ascii_uppercase()) && cited.contains("::") {
                type_paths.insert(format!("{doc}: {cited}"));
            }
            checked.insert(format!("{doc}: {cited}"));
        }
    }
    let missing: Vec<String> = missing.into_iter().collect();
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    // Well under the numbers the docs cite, so that a change in how they
    // are written, or in how elfq defines its types, can't make this test
    // check nothing.
    assert!(checked.len() >= 100, "checked {} citations", checked.len());
    let count = type_paths.len();
    assert!(count >= 50, "checked {count} type paths");
}

/// The path of `path`, from the crate's root.
fn crate_path(path: &str) -> String {
    format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))
}

/// The files and folders of the [`ROOTS`], and the modules among them.
struct Tree {
    /// Each file and folder, from the crate's root, with a `/` after each
    /// folder.
    paths: BTreeSet<String>,
    /// The modules of `src/` and `tests/it/`, other than the crates' roots.
    modules: Vec<Module>,
    /// The text of each Rust file of `src/` and `tests/it/`, the crates'
    /// roots among them.
    sources: Vec<String>,
}

/// A module of `src/` or `tests/it/`.
struct Module {
    /// Its path from `src/` or `tests/it/`, with `/` between its names, as
    /// `commands/symbol`.
    path: String,
    /// Whether it is in `src/`, the library.
    in_src: bool,
    /// Its file, from the crate's root.
    file: String,
}

impl Tree {
    /// Reads the files and folders of the [`ROOTS`].
    fn read() -> Tree {
        let mut paths = BTreeSet::new();
        for root in ROOTS {
            add_paths(&format!("{root}/"), &mut paths);
        }
        let modules = paths.iter().filter_map(|file| Module::of(file)).collect();
        let sources = (paths.iter())
            .filter(|path| path.starts_with("src/") || path.starts_with("tests/it/"))
            .filter(|path| path.ends_with(".rs"))
            .map(|file| std::fs::read_to_string(crate_path(file)).unwrap())
            .collect();
        Tree {
            paths,
            modules,
            sources,
        }
    }

    /// What `text`, the text of `doc`, cites, each with whether it is
    /// there: what its prose cites, and what its code blocks do.
    fn citations<'a>(&self, doc: &str, text: &'a str) -> Vec<(&'a str, bool)> {
        // Every other piece between fences is a code block.
        let pieces = text.split("```").enumerate();
        let citations = pieces.flat_map(|(at, piece)| match at % 2 {
            0 => self.prose_citations(doc, piece),
            _ => self.block_citations(piece),
        });
        citations.collect()
    }

    /// What `prose`, a part of `doc` outside its code blocks, cites, each
    /// with whether it is there: the first word of each code span that is a
    /// path, a module path or a type path, each link's target, and each
    /// other path that starts with one of the [`ROOTS`].
    fn prose_citations<'a>(&self, doc: &str, prose: &'a str) -> Vec<(&'a str, bool)> {
        let spans = code_spans(prose).filter_map(|word| {
            let is_there = (self.has_path(word))
                .or_else(|| self.has_module_path(word))
                .or_else(|| self.has_type_path(word))?;
            Some((word, is_there))
        });
        let links = link_targets(prose).map(|target| (target, link_is_there(doc, target)));
        let rooted = rooted_paths(prose)
            .into_iter()
            .filter_map(|path| Some((path, self.has_path(path)?)));
        spans.chain(links).chain(rooted).collect()
    }

    /// What `block`, a code block, cites, each with whether it is there:
    /// each word that is a module path that starts with a module of elfq's,
    /// or a type path. Another module path may be another crate's, in a
    /// Rust snippet, or a sample's, in output. Nothing if the block holds a
    /// `$ elfq` line, whose output names the samples' items.
    fn block_citations<'a>(&self, block: &'a str) -> Vec<(&'a str, bool)> {
        if block.lines().any(|line| line.starts_with("$ elfq ")) {
            return Vec::new();
        }
        let words = block.split(|c: char| c.is_whitespace() || c == ',');
        let citations = words.filter_map(|word| {
            let module_path = self.has_module_path(word);
            let is_there = (module_path.filter(|_| self.starts_with_module(word)))
                .or_else(|| self.has_type_path(word))?;
            Some((word, is_there))
        });
        citations.collect()
    }

    /// Whether `path`, from the crate's root or the end of a path in it, is
    /// there, or `None` if it is neither, if it holds a placeholder or a
    /// pattern (a `<`, `*` or `{`, as in `src/arch/<name>.rs`), or if it
    /// goes through a `target` folder, which a build makes.
    fn has_path(&self, path: &str) -> Option<bool> {
        // A placeholder, a pattern, or a build's files.
        if path.contains(['<', '*', '{']) || path.split('/').any(|name| name == "target") {
            return None;
        }
        if ROOTS
            .iter()
            .any(|root| path.starts_with(&format!("{root}/")))
        {
            let folder = format!("{}/", path.trim_end_matches('/'));
            return Some(self.paths.contains(path) || self.paths.contains(&folder));
        }
        let names = path
            .strip_suffix(".rs")
            .or_else(|| path.strip_suffix('/'))?;
        if !names.split('/').all(is_module_name) {
            return None;
        }
        let end = format!("/{path}");
        Some(self.paths.iter().any(|known| known.ends_with(&end)))
    }

    /// Whether the module path `path`, such as `symbol::find`, is there, or
    /// `None` if it isn't a module path of elfq's: if it starts with a type,
    /// as `Binary::load`, which [`Tree::has_type_path`] checks, or with one
    /// of the [`OTHER_FIRST_NAMES`].
    ///
    /// A path that starts with `crate::` or `elfq::` names its modules from
    /// `src/`. Any other may leave out the modules before its first, as
    /// `symbol::find` leaves out `commands::`, so its modules may end the
    /// path of any module of `src/` or `tests/it/`.
    fn has_module_path(&self, path: &str) -> Option<bool> {
        let path = path.split('(').next().unwrap().trim_end_matches("::");
        let names: Vec<&str> = path.split("::").collect();
        if names.len() < 2 || !names.iter().all(|name| is_identifier(name)) {
            return None;
        }
        let first = names[0];
        if first.starts_with(|c: char| c.is_ascii_uppercase()) || OTHER_FIRST_NAMES.contains(&first)
        {
            return None;
        }
        let (names, from_src) = match first {
            "crate" | "elfq" => (&names[1..], true),
            _ => (&names[..], false),
        };
        // How many of the names, from the first, are modules.
        let modules = (1..=names.len())
            .take_while(|&count| !self.files_of(&names[..count], from_src).is_empty())
            .count();
        if modules == 0 {
            return Some(false);
        }
        let Some(item) = names.get(modules) else {
            return Some(true);
        };
        let files = self.files_of(&names[..modules], from_src);
        Some(files.iter().any(|file| {
            let text = std::fs::read_to_string(crate_path(file)).unwrap();
            defines(&text, item) || imports(&text, item)
        }))
    }

    /// Whether `path` starts with a module of elfq's: whether its first
    /// name, after any `crate::` or `elfq::`, is a module of `src/` or
    /// `tests/it/`, as `symbol` is in `symbol::find`.
    fn starts_with_module(&self, path: &str) -> bool {
        let in_src = (path.strip_prefix("crate::")).or_else(|| path.strip_prefix("elfq::"));
        let first = in_src.unwrap_or(path).split("::").next().unwrap();
        !self.files_of(&[first], in_src.is_some()).is_empty()
    }

    /// Whether the type path `path`, such as `Binary::load`, is there: the
    /// name after the type must be a function, constant, associated type,
    /// field or variant in the type's definition, as a struct, enum or
    /// trait, or in an `impl` of it. `None` if `path` isn't a type and a
    /// name, or if no file of `src/` or `tests/it/` defines the type as a
    /// struct, enum or trait, as for another crate's `EndianSlice`, the
    /// Rust sample's `Gain`, or a type alias, such as the startup walk's
    /// `Memory`, which has no members of its own.
    fn has_type_path(&self, path: &str) -> Option<bool> {
        let path = path.split('(').next().unwrap();
        let (type_name, name) = path.split_once("::")?;
        let is_type = type_name.starts_with(|c: char| c.is_ascii_uppercase());
        if !is_type || !is_identifier(type_name) || !is_identifier(name) {
            return None;
        }
        // Only the files that name the type, which are quick to find.
        let sources = || (self.sources.iter()).filter(|text| text.contains(type_name));
        if !sources().any(|text| code_lines(text).any(|line| defines_type(line, type_name))) {
            return None;
        }
        let mut lines = sources().flat_map(|text| member_lines(text, type_name));
        Some(lines.any(|line| defines_member(line, name)))
    }

    /// The files of the modules whose paths are `names`, from `src/` when
    /// `from_src`, and otherwise end with `names`, in `src/` or `tests/it/`.
    fn files_of(&self, names: &[&str], from_src: bool) -> Vec<&str> {
        let wanted = names.join("/");
        let end = format!("/{wanted}");
        let matches = |module: &Module| match from_src {
            true => module.in_src && module.path == wanted,
            false => module.path == wanted || module.path.ends_with(&end),
        };
        let modules = self.modules.iter().filter(|module| matches(module));
        modules.map(|module| module.file.as_str()).collect()
    }
}

impl Module {
    /// The module that `file` holds, if it is a module of `src/` or
    /// `tests/it/` other than the crate's root.
    fn of(file: &str) -> Option<Module> {
        let (path, in_src) = match file.strip_prefix("src/") {
            Some(path) => (path, true),
            None => (file.strip_prefix("tests/it/")?, false),
        };
        let path = path.strip_suffix(".rs")?;
        let path = path.strip_suffix("/mod").unwrap_or(path);
        let module = Module {
            path: path.to_string(),
            in_src,
            file: file.to_string(),
        };
        (!matches!(path, "main" | "lib")).then_some(module)
    }
}

/// Adds `folder`, a path from the crate's root that ends with `/`, and the
/// files and folders in it to `paths`, but for build folders, `target`.
fn add_paths(folder: &str, paths: &mut BTreeSet<String>) {
    paths.insert(folder.to_string());
    for entry in std::fs::read_dir(crate_path(folder)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        if !entry.file_type().unwrap().is_dir() {
            paths.insert(format!("{folder}{name}"));
        } else if name != "target" {
            add_paths(&format!("{folder}{name}/"), paths);
        }
    }
}

/// The first word of each code span in `prose`.
fn code_spans(prose: &str) -> impl Iterator<Item = &str> {
    // Every other piece between backticks is a code span.
    let spans = prose.split('`').skip(1).step_by(2);
    spans.map(|span| span.split_whitespace().next().unwrap_or_default())
}

/// The targets of the links in `prose` to files of the crate.
fn link_targets(prose: &str) -> impl Iterator<Item = &str> {
    let targets = prose
        .match_indices("](")
        .map(|(at, _)| prose[at + 2..].split([')', ' ']).next().unwrap());
    targets.filter(|target| !target.starts_with('#') && !target.contains("://"))
}

/// Whether the file that `doc` links to as `target` is there, from the
/// doc's folder.
fn link_is_there(doc: &str, target: &str) -> bool {
    let file = target.split('#').next().unwrap();
    let folder = Path::new(doc).parent().unwrap();
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(folder)
        .join(file)
        .exists()
}

/// The paths in `prose` that start with one of the [`ROOTS`], other than
/// those that start a code span or end another path, each without a `.`
/// that ends a sentence.
fn rooted_paths(prose: &str) -> Vec<&str> {
    let in_path = |c: char| c.is_ascii_alphanumeric() || "_./-".contains(c);
    let mut paths = Vec::new();
    for root in ROOTS {
        for (at, _) in prose.match_indices(&format!("{root}/")) {
            // Not the end of another path, nor the start of a code span.
            if prose[..at].ends_with(|c| in_path(c) || c == '`') {
                continue;
            }
            let rest = &prose[at..];
            let end = rest.find(|c| !in_path(c)).unwrap_or(rest.len());
            paths.push(rest[..end].trim_end_matches('.'));
        }
    }
    paths
}

/// Whether `text`, a Rust file, defines `name` on a line that isn't a
/// comment, as a function, type, constant, static, trait, module or macro.
fn defines(text: &str, name: &str) -> bool {
    let kinds = "fn struct enum const static type trait mod macro_rules!".split(' ');
    let definitions: Vec<String> = kinds.map(|kind| format!("{kind} {name}")).collect();
    code_lines(text).any(|line| (definitions.iter()).any(|definition| holds_word(line, definition)))
}

/// Whether `line`, of a Rust file, defines the type `name`, as a struct,
/// enum or trait.
fn defines_type(line: &str, name: &str) -> bool {
    let kinds = ["struct", "enum", "trait"];
    line.contains(name)
        && kinds
            .iter()
            .any(|kind| holds_word(line, &format!("{kind} {name}")))
}

/// The lines one level inside each definition of the type `name` in
/// `text`, a Rust file, as a struct, enum or trait, and inside each `impl`
/// of it, as `impl Names {` or `impl<'a> fmt::Display for LeadName<'a> {`:
/// those that start its fields, variants, functions, constants and
/// associated types.
fn member_lines<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let starts = |line: &str| defines_type(line, name) || implemented(line) == Some(name);
    let mut lines = Vec::new();
    // The indent of the definition or `impl` that is open, if one is. Its
    // members are four spaces further in, and a `}` as far in as its first
    // line closes it, as rustfmt writes them.
    let mut open = None;
    for line in code_lines(text) {
        match open {
            Some(indent) if line.trim_end().strip_prefix(indent) == Some("}") => open = None,
            Some(indent) => {
                let member = (line.strip_prefix(indent)).and_then(|rest| rest.strip_prefix("    "));
                if member.is_some_and(|member| !member.starts_with(' ')) {
                    lines.push(line);
                }
            }
            None if starts(line) && !line.trim_end().ends_with([';', '}']) => {
                open = Some(&line[..line.len() - line.trim_start().len()]);
            }
            None => {}
        }
    }
    lines
}

/// The type that `line` starts an `impl` of, if it does.
fn implemented(line: &str) -> Option<&str> {
    let mut rest = line.trim_start().strip_prefix("impl")?;
    if rest.starts_with('<') {
        // Past the impl's own generics, which may nest.
        let mut depth = 0;
        let end = rest.find(|c| {
            depth += match c {
                '<' => 1,
                '>' => -1,
                _ => 0,
            };
            depth == 0
        })?;
        rest = &rest[end + 1..];
    } else if !rest.starts_with(' ') {
        return None;
    }
    let rest = rest.split_once(" for ").map_or(rest, |(_, after)| after);
    let rest = rest.trim_start();
    let in_path = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == ':';
    let path = &rest[..rest.find(|c| !in_path(c)).unwrap_or(rest.len())];
    path.rsplit("::").next()
}

/// Whether `line`, one level inside a type's definition or an `impl` of
/// it, defines `name`: as a function, a constant or an associated type, or
/// as a field, `name: Type`, or a variant, `Name`, `Name(..)` or
/// `Name { .. }`.
fn defines_member(line: &str, name: &str) -> bool {
    let defines = |kind| holds_word(line, &format!("{kind} {name}"));
    if ["fn", "const", "type"].into_iter().any(defines) {
        return true;
    }
    let Some(rest) = without_visibility(line.trim()).strip_prefix(name) else {
        return false;
    };
    match name.starts_with(|c: char| c.is_ascii_uppercase()) {
        true => rest.is_empty() || rest.starts_with([',', '(', '{', ' ']),
        false => rest.starts_with(": "),
    }
}

/// The lines of `text`, a Rust file, that aren't comments.
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    (text.lines()).filter(|line| !line.trim_start().starts_with("//"))
}

/// Whether `text`, a Rust file, names `name` in a `use` declaration, which
/// may take several lines.
fn imports(text: &str, name: &str) -> bool {
    let mut in_use = false;
    text.lines().map(str::trim).any(|line| {
        in_use |= without_visibility(line).starts_with("use ");
        let names_it = in_use && holds_word(line, name);
        in_use &= !line.ends_with(';');
        names_it
    })
}

/// `line` without the visibility it starts with, if any, as `pub(crate) `.
fn without_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else {
        return line;
    };
    let rest = match rest.strip_prefix('(') {
        Some(restricted) => restricted.split_once(')').map_or(rest, |(_, after)| after),
        None => rest,
    };
    rest.trim_start()
}

/// Whether `name` is a name in a module's path, in lowercase.
fn is_module_name(name: &str) -> bool {
    let in_name = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
    !name.is_empty() && name.chars().all(in_name)
}

/// Whether `name` is a Rust identifier.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let starts = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `text` holds `word` with no letter, digit or `_` on either side.
fn holds_word(text: &str, word: &str) -> bool {
    let in_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].ends_with(in_word) && !text[at + word.len()..].starts_with(in_word)
    })
}
