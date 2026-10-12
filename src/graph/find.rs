//! Finding what a user means by a symbol on the command line.

use anyhow::{Context, Result, bail};

use super::Node;
use crate::binary::Binary;

/// Finds the item or import that `query` names: a name, an alias, or an
/// address such as `0x1700`, meaning the item that contains it.
///
/// `write` also finds `write@plt`, the stub that code calls to reach an
/// imported `write`. When nothing matches, the error suggests similar names.
pub fn find(binary: &Binary, query: &str) -> Result<Node> {
    if let Some(hex) = query.strip_prefix("0x") {
        let address = u64::from_str_radix(hex, 16)
            .with_context(|| format!("`{query}` is not a hexadecimal address"))?;
        return match binary.item_at(address) {
            Some(index) => Ok(Node::Item(index)),
            None => bail!("no function or data object contains address {address:#x}"),
        };
    }

    let named: Vec<usize> = (0..binary.items.len())
        .filter(|&index| {
            let item = &binary.items[index];
            item.name == query || item.aliases.iter().any(|alias| alias == query)
        })
        .collect();
    match named[..] {
        [index] => return Ok(Node::Item(index)),
        [] => {}
        _ => {
            let addresses: Vec<String> = named
                .iter()
                .map(|&index| format!("{:#x}", binary.items[index].address))
                .collect();
            bail!(
                "{} items are named `{query}`; choose one by its address: {}",
                named.len(),
                addresses.join(", ")
            );
        }
    }

    let stub = format!("{query}@plt");
    if let Some(index) = binary.items.iter().position(|item| item.name == stub) {
        return Ok(Node::Item(index));
    }
    if let Some(index) = binary.imports.iter().position(|name| name == query) {
        return Ok(Node::Import(index));
    }

    let similar = similar_names(binary, query);
    if similar.is_empty() {
        bail!("no function or data object is named `{query}`");
    }
    bail!(
        "no function or data object is named `{query}`; did you mean {}?",
        similar.join(", ")
    );
}

/// Up to five names that contain `query`, ignoring case, or that differ
/// from it by a typo or two.
fn similar_names<'a>(binary: &'a Binary, query: &str) -> Vec<&'a str> {
    let query = query.to_lowercase();
    let typos = (query.chars().count() / 3).min(2);
    let names = binary
        .items
        .iter()
        .flat_map(|item| std::iter::once(&item.name).chain(&item.aliases))
        .chain(&binary.imports);
    let mut similar: Vec<(usize, &str)> = names
        .filter_map(|name| {
            let lowercase = name.to_lowercase();
            if lowercase.contains(&query) {
                return Some((0, name.as_str()));
            }
            let distance = edit_distance(&lowercase, &query);
            (distance <= typos).then_some((distance, name.as_str()))
        })
        .collect();
    similar.sort();
    similar.dedup();
    similar.into_iter().take(5).map(|(_, name)| name).collect()
}

/// The number of characters to insert, delete or replace to turn `a` into
/// `b` (Levenshtein distance).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, a_char) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &b_char) in b.iter().enumerate() {
            let replace = diagonal + usize::from(a_char != b_char);
            diagonal = row[j + 1];
            row[j + 1] = replace.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_typos() {
        assert_eq!(edit_distance("process_frame", "process_frame"), 0);
        assert_eq!(edit_distance("process_frame", "proces_frame"), 1);
        assert_eq!(edit_distance("log_msg", "log_mgs"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
