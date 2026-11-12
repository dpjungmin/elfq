//! The line that says which arguments given in place of SYMBOL are files
//! in the current folder, which the shell may have put there in place of a
//! pattern.
//!
//! Every shell puts the names of the files that an unquoted `*` matches in
//! its place before elfq runs, so `uart_*`, typed without quotes in a
//! folder that holds `uart_driver.c` and `uart_hal.c`, reaches elfq as
//! those two names. elfq can't know what was typed, but when the names it
//! can't use are files here, the error ends with a line that says so, and
//! shows the pattern to quote, which the files' names start with
//! ([`FilesHere::hint`]): "uart_driver.c and uart_hal.c are files here: the
//! shell may have put them in place of a pattern. Put a pattern in single
//! quotes, as in 'uart_*', so that the shell passes it as typed."
//!
//! Only names in the current folder count, not paths such as
//! `drivers/uart_hal.c`, and only files, or symlinks to files
//! ([`is_file_here`]): a folder's name, such as `build`, is more often
//! typed as is. The commands that take one SYMBOL at most get the line
//! under clap's error for the arguments after it, which clap refuses
//! before the file is read; there, the argument that clap refuses may be a
//! folder too ([`is_here`]), since a `*` matches folders as well.
//!
//! `Names::find` adds the line to its error for a name that finds nothing
//! or a pattern that matches nothing, `subject` to its error for the
//! argument it stopped at, when that one is a file here, and `retained` to
//! its list of the arguments that it can't use ([`with_shell_hint`]), each
//! after the error is worded, so that no error is worded otherwise.
//! `arguments::unexpected_argument_hints` words the lines under clap's
//! error.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::anyhow;

use crate::commands::text::{self, Detail};

/// The arguments given in place of SYMBOL that are files in the current
/// folder (see [`is_file_here`]), which the shell may have put there in
/// place of a pattern.
pub(in crate::commands) struct FilesHere<'a> {
    /// Their names, in the order given, each once.
    names: Vec<&'a str>,
}

impl<'a> FilesHere<'a> {
    /// Those of `arguments` that are files here, in the order given, each
    /// once.
    pub(in crate::commands) fn among(arguments: &'a [impl AsRef<str>]) -> FilesHere<'a> {
        let mut names: Vec<&str> = Vec::new();
        for argument in arguments {
            let argument = argument.as_ref();
            if is_file_here(argument) && !names.contains(&argument) {
                names.push(argument);
            }
        }
        FilesHere { names }
    }

    /// The line that says that the files are here, and how to give a
    /// pattern so that the shell leaves it as typed: "uart_driver.c and
    /// uart_hal.c are files here: the shell may have put them in place of a
    /// pattern. Put a pattern in single quotes, as in 'uart_*', so that the
    /// shell passes it as typed." It names up to 11 files, and past that
    /// the first 10, then how many more, even with `--all`, since what
    /// matters is that they are files. The example is the pattern that
    /// their names start with (see [`FilesHere::pattern`]), and there is
    /// none for one file, which "is a file here", or when their names start
    /// with nothing in common. `None` when there are no files.
    pub(in crate::commands) fn hint(&self) -> Option<String> {
        let (are, them) = match self.names[..] {
            [] => return None,
            [_] => ("is a file", "it"),
            _ => ("are files", "them"),
        };
        let quoted: Vec<String> = (self.names.iter())
            .map(|name| text::shell_quote(name))
            .collect();
        let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
        let example = match self.pattern() {
            Some(pattern) => format!(", as in {}", text::shell_quote(&pattern)),
            None => String::new(),
        };
        Some(format!(
            "{} {are} here: the shell may have put {them} in place of a pattern. Put a pattern \
             in single quotes{example}, so that the shell passes it as typed.",
            text::names_text(&quoted, Detail::Brief)
        ))
    }

    /// The pattern that the shell may have put the files in place of, as
    /// the hint shows it: what their names start with, and a `*`, as
    /// `uart_*` for `uart_driver.c` and `uart_hal.c`. `None` for fewer than
    /// two files, since one file's name says nothing of where the pattern's
    /// `*` was, or when their names start with nothing in common.
    pub(in crate::commands) fn pattern(&self) -> Option<String> {
        if self.names.len() < 2 {
            return None;
        }
        match common_start(&self.names) {
            "" => None,
            start => Some(format!("{start}*")),
        }
    }
}

/// `error`, with the line that says which of `arguments` are files here
/// under it (see [`FilesHere::hint`]), or as it is when none is.
pub(in crate::commands) fn with_shell_hint(
    error: anyhow::Error,
    arguments: &[impl AsRef<str>],
) -> anyhow::Error {
    match FilesHere::among(arguments).hint() {
        Some(hint) => anyhow!("{error:#}\n{hint}"),
        None => error,
    }
}

/// Whether `name` is a file in the current folder, or a symlink to one: a
/// name that the shell may have put in place of a pattern.
pub(super) fn is_file_here(name: &str) -> bool {
    in_this_folder(name) && std::fs::metadata(name).is_ok_and(|metadata| metadata.is_file())
}

/// Whether `name` is a file or a folder in the current folder, or a
/// symlink to one, as the shell may put either in place of a pattern.
pub(in crate::commands) fn is_here(name: &str) -> bool {
    in_this_folder(name)
        && std::fs::metadata(name).is_ok_and(|metadata| metadata.is_file() || metadata.is_dir())
}

/// Whether `name` names something in the current folder: it is a name, not
/// a path such as `drivers/uart_hal.c` or `./uart_hal.c`, nor `.` or `..`.
fn in_this_folder(name: &str) -> bool {
    Path::new(name).file_name() == Some(OsStr::new(name))
}

/// What all of `names` start with, which ends between two characters, never
/// inside one: "uart_" for `uart_driver.c` and `uart_hal.c`, and "" when
/// they start with nothing in common.
fn common_start<'n>(names: &[&'n str]) -> &'n str {
    let Some((&first, rest)) = names.split_first() else {
        return "";
    };
    let mut start = first;
    for name in rest {
        let shared: usize = (start.chars().zip(name.chars()))
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        start = &start[..shared];
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hint for files with these names, as if they were here.
    fn hint(names: &[&str]) -> Option<String> {
        FilesHere {
            names: names.to_vec(),
        }
        .hint()
    }

    #[test]
    fn the_hint_shows_the_pattern_that_the_files_names_start_with() {
        assert_eq!(
            hint(&["uart_driver.c", "uart_hal.c"]).unwrap(),
            "uart_driver.c and uart_hal.c are files here: the shell may have put them in place of \
             a pattern. Put a pattern in single quotes, as in 'uart_*', so that the shell passes \
             it as typed."
        );
        // One file says nothing of where the `*` was.
        assert_eq!(
            hint(&["on_button.c"]).unwrap(),
            "on_button.c is a file here: the shell may have put it in place of a pattern. Put a \
             pattern in single quotes, so that the shell passes it as typed."
        );
        // Nor do names that start with nothing in common.
        assert_eq!(
            hint(&["main.c", "uart.c"]).unwrap(),
            "main.c and uart.c are files here: the shell may have put them in place of a \
             pattern. Put a pattern in single quotes, so that the shell passes it as typed."
        );
        // A name that the shell would split is quoted, as is the pattern.
        assert_eq!(
            hint(&["my uart.c", "my hal.c"]).unwrap(),
            "'my uart.c' and 'my hal.c' are files here: the shell may have put them in place of \
             a pattern. Put a pattern in single quotes, as in 'my *', so that the shell passes \
             it as typed."
        );
        assert_eq!(hint(&[]), None);
    }

    #[test]
    fn the_hint_names_ten_files_then_how_many_more() {
        let names: Vec<String> = (1..=12).map(|number| format!("irq{number}.c")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            hint(&names).unwrap(),
            "irq1.c, irq2.c, irq3.c, irq4.c, irq5.c, irq6.c, irq7.c, irq8.c, irq9.c, irq10.c and \
             2 more are files here: the shell may have put them in place of a pattern. Put a \
             pattern in single quotes, as in 'irq*', so that the shell passes it as typed."
        );
    }

    #[test]
    fn names_start_alike_up_to_a_whole_character() {
        assert_eq!(common_start(&["uart_driver.c", "uart_hal.c"]), "uart_");
        assert_eq!(common_start(&["uart.c", "uart.c.orig", "uart.h"]), "uart.");
        assert_eq!(common_start(&["über_a.c", "über_b.c"]), "über_");
        // `é` and `è` share their first byte, but not the character.
        assert_eq!(common_start(&["é1.c", "è2.c"]), "");
        assert_eq!(common_start(&["main.c", "uart.c"]), "");
        assert_eq!(common_start(&["main.c"]), "main.c");
        assert_eq!(common_start(&[]), "");
    }

    #[test]
    fn only_a_name_in_this_folder_counts() {
        assert!(in_this_folder("uart_hal.c"));
        assert!(in_this_folder("build"));
        for path in [
            "drivers/uart_hal.c",
            "./uart_hal.c",
            "build/",
            ".",
            "..",
            "",
        ] {
            assert!(!in_this_folder(path), "{path}");
        }
    }
}
