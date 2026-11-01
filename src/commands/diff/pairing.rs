//! Which item of the new build each item of the old build is: the two are
//! then "paired".
//!
//! The rules run in order, each a function below. Each looks only at the
//! items that are still unpaired, and pairs two items by a key, such as a
//! name, only when exactly one item on each side has it, and both are
//! functions or both data. A key that two items on one side share pairs
//! neither, and so does an item whose keys lead to two items of the other
//! side. A wrong pair never changes a total or the exit status, which come
//! from the bytes in each block, only what the rows call an item.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;

use serde::Serialize;

use crate::binary::{Binary, Item, ItemKind, Source, names};
use crate::graph::{Graph, Node, ReferenceKind};

/// The rule that paired two items, as JSON's `paired_by` says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum PairedBy {
    /// Their name, another of their names, or their name without the
    /// suffix of a compiler's copy (rules 1 to 3).
    Name,
    /// Their bytes (rules 4 to 6).
    Bytes,
    /// What uses them (rule 7).
    Uses,
    /// Where they are (rule 8).
    Place,
}

/// Which item of the new build each item of the old build is.
pub(super) struct Pairing {
    /// For each item of the old build, by its index, its partner in the new
    /// build and the rule that paired them, or `None` when it is gone.
    pub old_to_new: Vec<Option<(usize, PairedBy)>>,
    /// For each item of the new build, its partner in the old build, or
    /// `None` when it is new.
    pub new_to_old: Vec<Option<usize>>,
}

/// A build, as the rules see it.
#[derive(Clone, Copy)]
pub(super) struct Side<'a> {
    pub binary: &'a Binary,
    pub graph: &'a Graph,
}

/// Pairs the items of `old` with those of `new`, by the rules in order.
pub(super) fn pair(old: Side, new: Side) -> Pairing {
    let mut pairing = Pairing {
        old_to_new: vec![None; old.binary.items.len()],
        new_to_old: vec![None; new.binary.items.len()],
    };
    by_name(&mut pairing, old.binary, new.binary);
    by_other_name(&mut pairing, old.binary, new.binary);
    by_copy_name(&mut pairing, old.binary, new.binary);
    by_bytes(&mut pairing, old.binary, new.binary);
    by_first_string(&mut pairing, old.binary, new.binary);
    by_short_contents(&mut pairing, old.binary, new.binary);
    by_uses(&mut pairing, old, new);
    by_place(&mut pairing, old.binary, new.binary);
    pairing
}

/// Rule 1: the same name.
fn by_name(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Name, |binary, index| {
        let item = &binary.items[index];
        pairs_by_name(item, &item.name).then_some(item.name.as_str())
    });
}

/// Rule 2: any of their names, so that an item pairs by an alias, as a
/// weak alias or a mangled symbol is.
fn by_other_name(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Name, |binary, index| {
        let item = &binary.items[index];
        item.names()
            .filter(|name| pairs_by_name(item, name))
            .collect::<Vec<_>>()
    });
}

/// Rule 3: the name without the suffix that a compiler adds to a copy of
/// a function, as `dev_read.constprop.0` for `dev_read`, or LLVM's
/// `.llvm.123`, which changes with every build.
fn by_copy_name(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Name, |binary, index| {
        let item = &binary.items[index];
        pairs_by_name(item, &item.name).then(|| names::without_copy_suffixes(&item.name))
    });
}

/// Rule 4: the same bytes, at least 4 of them, the padding at the end left
/// out. A data object whose size the file gives must keep its size.
fn by_bytes(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Bytes, |binary, index| {
        let item = &binary.items[index];
        let bytes = trimmed_bytes(binary, item)?;
        let size = (item.kind == ItemKind::Data && !item.size_estimated).then_some(item.size);
        (bytes.len() >= 4).then_some((bytes, size))
    });
}

/// Rule 5: data whose bytes start with the same string, as a message's do
/// when the text after it changed.
fn by_first_string(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Bytes, |binary, index| {
        let item = &binary.items[index];
        if item.kind != ItemKind::Data {
            return None;
        }
        let bytes = binary.bytes(item.address, item.size)?;
        let end = bytes.iter().position(|&byte| byte == 0)?;
        let text = |byte: &u8| matches!(byte, b' '..=b'~' | b'\t' | b'\n' | b'\r');
        (end > 0 && bytes[..end].iter().all(text)).then_some(&bytes[..end])
    });
}

/// Rule 6: the same size and the same few bytes, fewer than 4 once the
/// padding at the end is left out, such as a variable that starts as zero.
fn by_short_contents(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Bytes, |binary, index| {
        let item = &binary.items[index];
        let bytes = trimmed_bytes(binary, item)?;
        (bytes.len() < 4).then_some((bytes, item.size))
    });
}

/// Rule 7: data in a section of the same name whose users are all paired,
/// each with its partner, and use it in the same ways. Where several are
/// alike, as the static variables of one function, those of one name pair
/// too, a name that GCC numbered counting as itself and as the name before
/// the number, as `buf` of `buf.3`.
fn by_uses(pairing: &mut Pairing, old: Side, new: Side) {
    let old_users = users(old, |index| pairing.old_to_new[index].is_none());
    let new_users = users(new, |index| pairing.new_to_old[index].is_none());
    let old_key = |index: usize| {
        let Some(users) = old_users.get(&index) else {
            return Vec::new();
        };
        // Each user as its partner, so that the keys of both sides name
        // the new build's items.
        let Some(mut partners) = users
            .iter()
            .map(|&(user, kind)| Some((pairing.old_to_new[user]?.0, kind)))
            .collect::<Option<Vec<_>>>()
        else {
            return Vec::new();
        };
        partners.sort_unstable();
        partners.dedup();
        uses_keys(old.binary, index, partners)
    };
    let new_key = |index: usize| {
        let Some(users) = new_users.get(&index) else {
            return Vec::new();
        };
        if users
            .iter()
            .any(|&(user, _)| pairing.new_to_old[user].is_none())
        {
            return Vec::new();
        }
        let mut users = users.clone();
        users.sort_unstable();
        users.dedup();
        uses_keys(new.binary, index, users)
    };
    let old_keys = keys_of(old.binary, pairing, true, old_key);
    let new_keys = keys_of(new.binary, pairing, false, new_key);
    pairing.pair_keys(old.binary, new.binary, PairedBy::Uses, old_keys, new_keys);
}

/// Rule 8: the only object without a name in a section of that name that
/// the file doesn't store, such as `.bss`, whose zeros have nothing to pair
/// by.
fn by_place(pairing: &mut Pairing, old: &Binary, new: &Binary) {
    pairing.pair_unique(old, new, PairedBy::Place, |binary, index| {
        let item = &binary.items[index];
        let section = &binary.sections[item.section];
        (item.kind == ItemKind::Data
            && is_made_name(item, &item.name)
            && section.file_range.is_none())
        .then_some(section.name.as_str())
    });
}

impl Pairing {
    /// Pairs, of the items still unpaired, each item of `old` with the item
    /// of `new` that has one of its keys, when only that item of each side
    /// has the key, and both are of one kind. `key` gives the keys of an
    /// item of either build, by its index.
    fn pair_unique<'a, K, I>(
        &mut self,
        old: &'a Binary,
        new: &'a Binary,
        rule: PairedBy,
        key: impl Fn(&'a Binary, usize) -> I,
    ) where
        K: Hash + Eq + Send,
        I: IntoIterator<Item = K>,
    {
        let old_keys = keys_of(old, self, true, |index| key(old, index));
        let new_keys = keys_of(new, self, false, |index| key(new, index));
        self.pair_keys(old, new, rule, old_keys, new_keys);
    }

    /// Pairs by the keys of the unpaired items of each side, as
    /// [`Pairing::pair_unique`] does: two items pair when every key that
    /// only one item of each side has leads from each to the other. Where
    /// identical code folding gave one function the names of two, it pairs
    /// with neither, whichever build is the old one.
    fn pair_keys<K: Hash + Eq + Send>(
        &mut self,
        old: &Binary,
        new: &Binary,
        rule: PairedBy,
        old_keys: Vec<(usize, K)>,
        new_keys: Vec<(usize, K)>,
    ) {
        // Keys are kept apart by the kind of item that has them. Each side's
        // are hashed on a thread of its own.
        let (old_holders, new_holders) =
            super::on_both((old, old_keys), (new, new_keys), |(binary, keys)| {
                holders(
                    keys.into_iter()
                        .map(|(index, key)| (index, (binary.items[index].kind, key))),
                )
            });
        // Where each item's keys lead on the other side.
        let mut old_leads = vec![Lead::Nowhere; old.items.len()];
        let mut new_leads = vec![Lead::Nowhere; new.items.len()];
        for (key, holder) in &old_holders {
            if let (Some(holder), Some(&Some(partner))) = (*holder, new_holders.get(key)) {
                old_leads[holder].add(partner);
                new_leads[partner].add(holder);
            }
        }
        for (index, lead) in old_leads.into_iter().enumerate() {
            if let Lead::To(partner) = lead
                && new_leads[partner] == Lead::To(index)
            {
                self.old_to_new[index] = Some((partner, rule));
                self.new_to_old[partner] = Some(index);
            }
        }
    }
}

/// Where an item's keys lead on the other side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lead {
    Nowhere,
    /// To this one item, as an index into its build's items.
    To(usize),
    /// To several items.
    Several,
}

impl Lead {
    /// Adds a key that leads to item `index`.
    fn add(&mut self, index: usize) {
        *self = match *self {
            Lead::Nowhere => Lead::To(index),
            Lead::To(earlier) if earlier == index => Lead::To(index),
            _ => Lead::Several,
        };
    }
}

/// The keys of the unpaired items of `binary`, the old build's when `old`,
/// with the index of the item that has each.
fn keys_of<K, I>(
    binary: &Binary,
    pairing: &Pairing,
    old: bool,
    key: impl Fn(usize) -> I,
) -> Vec<(usize, K)>
where
    I: IntoIterator<Item = K>,
{
    let unpaired = |index: &usize| match old {
        true => pairing.old_to_new[*index].is_none(),
        false => pairing.new_to_old[*index].is_none(),
    };
    (0..binary.items.len())
        .filter(unpaired)
        .flat_map(|index| key(index).into_iter().map(move |key| (index, key)))
        .collect()
}

/// For each key, the item that has it, or `None` when several do.
fn holders<K: Hash + Eq>(keys: impl Iterator<Item = (usize, K)>) -> HashMap<K, Option<usize>> {
    let mut holders = HashMap::new();
    for (index, key) in keys {
        match holders.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(Some(index));
            }
            Entry::Occupied(mut entry) => {
                if *entry.get() != Some(index) {
                    entry.insert(None);
                }
            }
        }
    }
    holders
}

/// The items that use each unpaired data object of `side`, with how, by the
/// object's index: those for which `unpaired` is true. An item's use of
/// itself doesn't count.
fn users(
    side: Side,
    unpaired: impl Fn(usize) -> bool,
) -> HashMap<usize, Vec<(usize, ReferenceKind)>> {
    let mut users: HashMap<usize, Vec<(usize, ReferenceKind)>> = HashMap::new();
    for reference in &side.graph.references {
        if let Node::Item(index) = reference.to
            && index != reference.from
            && side.binary.items[index].kind == ItemKind::Data
            && unpaired(index)
        {
            users
                .entry(index)
                .or_default()
                .push((reference.from, reference.kind));
        }
    }
    users
}

/// The name of the section that holds item `index`.
pub(super) fn section_name(binary: &Binary, index: usize) -> &str {
    &binary.sections[binary.items[index].section].name
}

/// Rule 7's keys of data object `index`, which `users` use: the name of its
/// section with its users, and those with its name too, when it has one of
/// its own, and with the name before the number when GCC numbered it.
fn uses_keys<U: Clone>(binary: &Binary, index: usize, users: U) -> Vec<((&str, Option<&str>), U)> {
    let item = &binary.items[index];
    let section = section_name(binary, index);
    let mut keys = Vec::with_capacity(2);
    if !is_made_name(item, &item.name) {
        for name in [Some(item.name.as_str()), numbered_name(item, &item.name)]
            .into_iter()
            .flatten()
        {
            keys.push(((section, Some(name)), users.clone()));
        }
    }
    keys.push(((section, None), users));
    keys
}

/// The bytes of `item`, without the padding at the end: zeros after data,
/// and after code, the pairs of bytes that pad it, zeros or a no-op
/// instruction. `None` when the file doesn't hold them, as for `.bss`.
fn trimmed_bytes<'b>(binary: &'b Binary, item: &Item) -> Option<&'b [u8]> {
    if item.size == 0 {
        return None;
    }
    let mut bytes = binary.bytes(item.address, item.size)?;
    match item.kind {
        ItemKind::Function => {
            // Thumb's no-op is 00 bf, and x86-64's 90.
            while let [rest @ .., a, b] = bytes
                && matches!([*a, *b], [0, 0] | [0, 0xbf] | [0x90, 0x90])
            {
                bytes = rest;
            }
        }
        ItemKind::Data => {
            while let [rest @ .., 0] = bytes {
                bytes = rest;
            }
        }
    }
    Some(bytes)
}

/// Whether `name`, one of `item`'s names, is one that elfq or a compiler
/// made from an address, a counter or a hash, which changes from build to
/// build and says nothing of what the item is: elfq's `fn_0x8000040`,
/// `.rodata@0x2004` and `entry`, and a compiler's `.L` labels, also a
/// string's `.L.str.12` that ThinLTO kept as `.str.12.llvm.345`, and Rust's
/// `anon.` objects. Such names never pair items.
pub(super) fn is_made_name(item: &Item, name: &str) -> bool {
    match item.source {
        Source::SymbolTable | Source::DynamicSymbolTable | Source::Plt => {
            name.starts_with(".L")
                || name == ".str"
                || name.starts_with(".str.")
                || is_anonymous(name)
        }
        // An item that elfq names has no aliases, so `name` is that name.
        Source::EhFrame
        | Source::EntryPoint
        | Source::Reached
        | Source::Reference
        | Source::Section => item.has_made_name(),
    }
}

/// Whether `name`, one of `item`'s names, pairs it by name: not a name
/// that elfq or a compiler made, nor one that GCC numbered.
fn pairs_by_name(item: &Item, name: &str) -> bool {
    !is_made_name(item, name) && numbered_name(item, name).is_none()
}

/// For a data object's `name` that GCC numbered, the name before the
/// number: GCC numbers each switch table it makes, as `CSWTCH.12`, and each
/// static variable of a function, as `buf.3` or `__func__.4`, counting the
/// file's names, so the numbers move when a function is added before them.
/// A C name has no dot, so it is one followed by a dot and a number, once
/// the suffixes of a copy are left out.
pub(super) fn numbered_name<'n>(item: &Item, name: &'n str) -> Option<&'n str> {
    if item.kind != ItemKind::Data {
        return None;
    }
    let (before, number) = names::without_copy_suffixes(name).rsplit_once('.')?;
    let is_number = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
    let mut c_name = before.bytes();
    let is_c_name = c_name
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && c_name.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    (is_number && is_c_name).then_some(before)
}

/// Whether two items, paired, have names that are the same but for the
/// numbers that change from build to build: ThinLTO's, however a name
/// writes it, as `f.llvm.123` or `f [clone .llvm.123]`, and the number
/// that GCC gives a name (see [`numbered_name`]).
pub(super) fn same_but_numbers(old: &Item, new: &Item) -> bool {
    let without = |item: &Item| {
        let name = numbered_name(item, &item.name).unwrap_or(&item.name);
        without_llvm_numbers(name)
    };
    without(old) == without(new)
}

/// `name` without the numbers that ThinLTO adds to it, in each of the ways
/// that names write them: `f.llvm.123`, `f [clone .llvm.123]` and `f [clone
/// .llvm] [clone .123]`.
fn without_llvm_numbers(name: &str) -> String {
    let mut name = name.to_string();
    for (before, after) in [
        (" [clone .llvm] [clone .", "]"),
        (" [clone .llvm.", "]"),
        (".llvm.", ""),
    ] {
        let mut from = 0;
        while let Some(offset) = name[from..].find(before).map(|offset| offset + from) {
            let digits = offset + before.len();
            let end = digits
                + name[digits..]
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
            if end > digits && name[end..].starts_with(after) {
                name.replace_range(offset..end + after.len(), "");
                from = offset;
            } else {
                from = digits;
            }
        }
    }
    name
}

/// Whether `name` is the name that LLVM gives an object without one in a
/// Rust program, `anon.` then 32 hex digits and a number.
fn is_anonymous(name: &str) -> bool {
    name.strip_prefix("anon.").is_some_and(|rest| {
        rest.len() > 33
            && rest.as_bytes()[..32].iter().all(u8::is_ascii_hexdigit)
            && rest.as_bytes()[32] == b'.'
    })
}

#[cfg(test)]
mod tests;
