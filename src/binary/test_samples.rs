//! The samples in testdata/bin, for the unit tests of `binary` and the
//! layers above it.
//!
//! A unit test names a sample by its path from the crate's root, such as
//! `testdata/bin/c-x86_64.elf`, as an integration test does. The
//! integration tests have functions of the same names in tests/it/support,
//! `sample_path`, `load_sample` and `samples`, since they can't use this
//! module: it is built only with `cfg(test)`, into the unit tests. It is in
//! `binary`, the lowest layer that loads a file, so that the tests of
//! `graph` and `commands` can use it too. `Binary::index_of`, also only for
//! tests, gives the index of a loaded sample's item from its name.

use std::path::{Path, PathBuf};

use super::Binary;

/// Where the file at `path` is, from wherever the test runs. `path` is
/// from the crate's root, as for support's `sample_path`: a sample, such
/// as `testdata/bin/c-x86_64.elf`, or another file in testdata.
pub(crate) fn sample_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// Loads the sample at `path`, from the crate's root, and panics naming it
/// if it can't.
pub(crate) fn load_sample(path: &str) -> Binary {
    Binary::load(&sample_path(path)).unwrap_or_else(|err| panic!("{path}: {err:#}"))
}

/// Every sample: each `.elf` and `.so` file in testdata/bin, sorted.
///
/// It gives each as a full path, which a unit test opens with
/// `Binary::load` from wherever it runs. The `samples` in tests/it/support
/// gives paths from the crate's root instead, since an integration test
/// gives them to elfq as FILE.
pub(crate) fn samples() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(sample_path("testdata/bin"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "elf" || e == "so"))
        .collect();
    paths.sort();
    assert!(paths.len() > 10, "{paths:?}");
    paths
}
