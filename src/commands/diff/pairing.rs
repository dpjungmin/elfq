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

use crate::binary::{Binary, Item, ItemKind, Source, without_copy_suffixes};
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
        pairs_by_name(item, &item.name).then(|| without_copy_suffixes(&item.name))
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
            Lead::To(at) if at == index => Lead::To(index),
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
        Source::EhFrame | Source::EntryPoint | Source::Reached | Source::Reference => true,
        // `.text@0x8000000`, for a vector table that is only part of its
        // section, has an address; `.init_array` doesn't.
        Source::Section => name.contains("@0x"),
        Source::SymbolTable | Source::DynamicSymbolTable | Source::Plt => {
            name.starts_with(".L")
                || name == ".str"
                || name.starts_with(".str.")
                || is_anonymous(name)
        }
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
    let (before, number) = without_copy_suffixes(name).rsplit_once('.')?;
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
        while let Some(at) = name[from..].find(before).map(|at| at + from) {
            let digits = at + before.len();
            let end = digits
                + name[digits..]
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
            if end > digits && name[end..].starts_with(after) {
                name.replace_range(at..end + after.len(), "");
                from = at;
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
mod tests {
    use object::SectionKind;

    use super::*;
    use crate::binary::Section;
    use crate::graph::Reference;

    /// An item for [`binary`]: a function in `.text` when `kind` is one,
    /// else data, in `.rodata` with `bytes`, or with `bss`, in `.bss`, of
    /// `bytes`' size.
    struct Spec {
        name: &'static str,
        aliases: &'static [&'static str],
        kind: ItemKind,
        source: Source,
        bss: bool,
        bytes: &'static [u8],
    }

    fn function(name: &'static str, bytes: &'static [u8]) -> Spec {
        Spec {
            name,
            aliases: &[],
            kind: ItemKind::Function,
            source: Source::SymbolTable,
            bss: false,
            bytes,
        }
    }

    fn data(name: &'static str, bytes: &'static [u8]) -> Spec {
        Spec {
            kind: ItemKind::Data,
            ..function(name, bytes)
        }
    }

    /// Data whose name elfq made from its address, as for a string.
    fn unnamed(name: &'static str, bytes: &'static [u8]) -> Spec {
        Spec {
            source: Source::Reference,
            ..data(name, bytes)
        }
    }

    /// A file whose `.text` at 0x1000, `.rodata` at 0x2000 and `.bss` at
    /// 0x3000 hold the items, one after another.
    fn binary(specs: &[Spec]) -> Binary {
        let section = |name: &str, address, kind| Section {
            name: name.to_string(),
            address,
            size: 0,
            kind,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        let mut sections = vec![
            section("", 0, SectionKind::Unknown),
            section(".text", 0x1000, SectionKind::Text),
            section(".rodata", 0x2000, SectionKind::ReadOnlyData),
            section(".bss", 0x3000, SectionKind::UninitializedData),
        ];
        let mut contents: [Vec<u8>; 3] = Default::default();
        let mut items = Vec::new();
        for spec in specs {
            let at = match (spec.kind, spec.bss) {
                (ItemKind::Function, _) => 1,
                (_, false) => 2,
                (_, true) => 3,
            };
            items.push(Item {
                name: spec.name.to_string(),
                aliases: spec.aliases.iter().map(|alias| alias.to_string()).collect(),
                kind: spec.kind,
                address: sections[at].address + contents[at - 1].len() as u64,
                size: spec.bytes.len() as u64,
                size_estimated: false,
                section: at,
                source: spec.source,
            });
            contents[at - 1].extend_from_slice(spec.bytes);
        }
        let mut file = Vec::new();
        for (at, bytes) in contents.into_iter().enumerate() {
            let section = &mut sections[at + 1];
            section.size = bytes.len() as u64;
            if at < 2 {
                section.file_range = Some((file.len() as u64, bytes.len() as u64));
                file.extend(bytes);
            }
        }
        items.sort_by_key(|item| item.address);
        Binary::for_tests(sections, items, file)
    }

    /// Each item of `old` by name, with the name of its partner in `new`
    /// and the rule that paired them, using `references` in both.
    fn pairs(
        old: &[Spec],
        new: &[Spec],
        references: &[(usize, usize)],
    ) -> Vec<(String, Option<(String, PairedBy)>)> {
        let graph = |binary: &Binary| Graph {
            references: references
                .iter()
                .map(|&(from, to)| Reference {
                    from,
                    to: Node::Item(to),
                    kind: ReferenceKind::Address,
                    site: binary.items[from].address,
                })
                .collect(),
            ..Graph::default()
        };
        let (old, new) = (binary(old), binary(new));
        let (old_graph, new_graph) = (graph(&old), graph(&new));
        let side = |binary, graph| Side { binary, graph };
        let pairing = pair(side(&old, &old_graph), side(&new, &new_graph));
        old.items
            .iter()
            .zip(pairing.old_to_new)
            .map(|(item, partner)| {
                let partner = partner.map(|(index, rule)| (new.items[index].name.clone(), rule));
                (item.name.clone(), partner)
            })
            .collect()
    }

    fn paired(old: &str, new: &str, rule: PairedBy) -> (String, Option<(String, PairedBy)>) {
        (old.to_string(), Some((new.to_string(), rule)))
    }

    fn unpaired(old: &str) -> (String, Option<(String, PairedBy)>) {
        (old.to_string(), None)
    }

    #[test]
    fn rule_1_pairs_by_name_only_one_item_on_each_side_and_of_one_kind() {
        // Two static functions named `twice` pair with neither, and a
        // function never pairs with data.
        assert_eq!(
            pairs(
                &[
                    function("crc32", b"\x01\x02\x03\x04\x05"),
                    function("twice", b"\x11\x12\x13\x14\x15"),
                    function("twice", b"\x21\x22\x23\x24\x25"),
                    function("table", b"\x31\x32\x33\x34\x35"),
                ],
                &[
                    function("crc32", b"\x41\x42\x43\x44\x45\x46"),
                    function("twice", b"\x51\x52\x53\x54\x55"),
                    data("table", b"\x61\x62\x63\x64\x65"),
                ],
                &[],
            ),
            [
                paired("crc32", "crc32", PairedBy::Name),
                unpaired("twice"),
                unpaired("twice"),
                unpaired("table"),
            ]
        );
    }

    #[test]
    fn rule_2_pairs_by_another_of_their_names() {
        // A weak alias that became the name, as when the default handler
        // is replaced.
        assert_eq!(
            pairs(
                &[Spec {
                    aliases: &["SysTick_Handler"],
                    ..function("Default_Handler", b"\xfe\xe7")
                }],
                &[Spec {
                    aliases: &["Default_Handler"],
                    ..function("SysTick_Handler", b"\x70\x47")
                }],
                &[],
            ),
            [paired("Default_Handler", "SysTick_Handler", PairedBy::Name)]
        );
    }

    #[test]
    fn rule_2_pairs_neither_when_the_names_of_one_item_lead_to_two() {
        // Identical code folding gave a stub the names of the old stub and
        // of a function that does the same: neither pairs by name, whichever
        // build is the old one, and the stubs pair by their bytes.
        let stub = Spec {
            aliases: &["Error::cause"],
            ..function("Stub::cause", b"\x31\xc0\xc3")
        };
        let parse = function("ParseError::cause", b"\x01\x02\x03\x04\x05");
        let folded = || Spec {
            aliases: &["Error::cause", "ParseError::cause"],
            ..function("Box::cause", b"\x31\xc0\xc3")
        };
        assert_eq!(
            pairs(&[stub, parse], &[folded()], &[]),
            [
                paired("Stub::cause", "Box::cause", PairedBy::Bytes),
                unpaired("ParseError::cause")
            ]
        );
        let stub = Spec {
            aliases: &["Error::cause"],
            ..function("Stub::cause", b"\x31\xc0\xc3")
        };
        let parse = function("ParseError::cause", b"\x01\x02\x03\x04\x05");
        assert_eq!(
            pairs(&[folded()], &[stub, parse], &[]),
            [paired("Box::cause", "Stub::cause", PairedBy::Bytes)]
        );
    }

    #[test]
    fn rule_3_pairs_a_compiler_copy_of_a_function_with_it() {
        assert_eq!(
            pairs(
                &[
                    function("dev_read", b"\x01\x02\x03\x04\x05"),
                    function("scale.llvm.123", b"\x11\x12\x13\x14\x15"),
                    function("std::sort(int*) [clone .isra.0]", b"\x21\x22\x23\x24\x25"),
                ],
                &[
                    function("dev_read.constprop.0", b"\x41\x42\x43\x44"),
                    function("scale.llvm.456", b"\x51\x52\x53\x54"),
                    function("std::sort(int*)", b"\x61\x62\x63\x64"),
                ],
                &[],
            ),
            [
                paired("dev_read", "dev_read.constprop.0", PairedBy::Name),
                paired("scale.llvm.123", "scale.llvm.456", PairedBy::Name),
                paired(
                    "std::sort(int*) [clone .isra.0]",
                    "std::sort(int*)",
                    PairedBy::Name
                ),
            ]
        );
    }

    #[test]
    fn rule_4_pairs_by_bytes_but_a_made_name_pairs_nothing() {
        // The code is the same once the padding after it is left out.
        // `.rodata@0x2000` is in both, but the name is elfq's, from its
        // address, and the bytes differ. So is `.str.1051`, which LLVM
        // numbered, as libLLVM 20 and 22 show.
        assert_eq!(
            pairs(
                &[
                    Spec {
                        source: Source::EhFrame,
                        ..function("fn_0x1000", b"\x01\x02\x03\x04\x05\x90\x90")
                    },
                    unnamed(".rodata@0x2000", b"\x11\x12\x13\x14\x15"),
                    data(".str.1051.llvm.27", b"N1UnitV1\0"),
                ],
                &[
                    Spec {
                        source: Source::EhFrame,
                        ..function("fn_0x1000", b"\x01\x02\x03\x04\x05\x00\x00")
                    },
                    unnamed(".rodata@0x2000", b"\x21\x22\x23\x24\x25"),
                    data(".str.1051.llvm.84", b"M3UnitFST\0"),
                ],
                &[],
            ),
            [
                paired("fn_0x1000", "fn_0x1000", PairedBy::Bytes),
                unpaired(".rodata@0x2000"),
                unpaired(".str.1051.llvm.27"),
            ]
        );
    }

    #[test]
    fn rule_5_pairs_data_by_its_first_string() {
        assert_eq!(
            pairs(
                &[unnamed(".rodata@0x2000", b"usage: %s\0old\0")],
                &[unnamed(".rodata@0x2000", b"usage: %s\0newer\0")],
                &[],
            ),
            [paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Bytes)]
        );
    }

    #[test]
    fn rule_6_pairs_a_few_bytes_of_one_size() {
        // Too few bytes for rule 4, and no string.
        assert_eq!(
            pairs(
                &[unnamed(".rodata@0x2000", b"\x01\x00\x00\x00")],
                &[unnamed(".rodata@0x2000", b"\x01\x00\x00\x00")],
                &[],
            ),
            [paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Bytes)]
        );
    }

    #[test]
    fn rule_7_pairs_data_by_what_uses_it() {
        // main, paired by name, is the only user of each.
        assert_eq!(
            pairs(
                &[
                    function("main", b"\x01\x02\x03\x04\x05"),
                    unnamed(".rodata@0x2000", b"\x11\x12\x13\x14\x15"),
                ],
                &[
                    function("main", b"\x01\x02\x03\x04\x05\x06"),
                    unnamed(".rodata@0x2000", b"\x21\x22\x23\x24\x25"),
                ],
                &[(0, 1)],
            ),
            [
                paired("main", "main", PairedBy::Name),
                paired(".rodata@0x2000", ".rodata@0x2000", PairedBy::Uses),
            ]
        );
    }

    #[test]
    fn rules_1_to_3_leave_out_a_name_that_gcc_numbered() {
        // A function with a switch table of its own, added before f, moved
        // the number of f's.
        assert_eq!(
            pairs(
                &[data("CSWTCH.3", b"\x01\x02\x03\x04\x05")],
                &[
                    data("CSWTCH.3", b"\x11\x12\x13\x14\x15\x16"),
                    data("CSWTCH.5", b"\x01\x02\x03\x04\x05"),
                ],
                &[],
            ),
            [paired("CSWTCH.3", "CSWTCH.5", PairedBy::Bytes)]
        );
    }

    #[test]
    fn rule_7_keeps_the_numbered_variables_of_a_function_apart_by_name() {
        // Two static variables of main, which only main uses, numbered
        // otherwise in the new build.
        let bss = |name, bytes| Spec {
            bss: true,
            ..data(name, bytes)
        };
        assert_eq!(
            pairs(
                &[
                    function("main", b"\x01\x02\x03\x04\x05"),
                    bss("buf.1", &[0; 16]),
                    bss("count.2", &[0; 4]),
                ],
                &[
                    function("main", b"\x01\x02\x03\x04\x05\x06"),
                    bss("buf.3", &[0; 32]),
                    bss("count.4", &[0; 4]),
                ],
                &[(0, 1), (0, 2)],
            ),
            [
                paired("main", "main", PairedBy::Name),
                paired("buf.1", "buf.3", PairedBy::Uses),
                paired("count.2", "count.4", PairedBy::Uses),
            ]
        );
    }

    #[test]
    fn rule_7_pairs_numbered_names_that_one_function_uses_alike_by_their_whole_names() {
        // GCC split a structure that only main uses into its fields, which
        // keep their numbers from build to build.
        let bss = |name, bytes| Spec {
            bss: true,
            ..data(name, bytes)
        };
        assert_eq!(
            pairs(
                &[
                    function("main", b"\x01\x02\x03\x04\x05"),
                    bss("state.0", &[0; 8]),
                    bss("state.1", &[0; 8]),
                ],
                &[
                    function("main", b"\x01\x02\x03\x04\x05\x06"),
                    bss("state.0", &[0; 8]),
                    bss("state.1", &[0; 8]),
                ],
                &[(0, 1), (0, 2)],
            ),
            [
                paired("main", "main", PairedBy::Name),
                paired("state.0", "state.0", PairedBy::Uses),
                paired("state.1", "state.1", PairedBy::Uses),
            ]
        );
    }

    #[test]
    fn rule_7_pairs_a_numbered_name_with_the_name_before_the_number() {
        // A static variable of _sbrk, in place of a global of one name.
        let bss = |name, bytes| Spec {
            bss: true,
            ..data(name, bytes)
        };
        assert_eq!(
            pairs(
                &[
                    function("_sbrk", b"\x01\x02\x03\x04\x05"),
                    bss("heap_end", &[0; 4])
                ],
                &[
                    function("_sbrk", b"\x01\x02\x03\x04\x05\x06"),
                    bss("heap_end.0", &[0; 4]),
                ],
                &[(0, 1)],
            ),
            [
                paired("_sbrk", "_sbrk", PairedBy::Name),
                paired("heap_end", "heap_end.0", PairedBy::Uses),
            ]
        );
    }

    #[test]
    fn names_can_differ_only_in_numbers_that_change_with_every_build() {
        let same = |old: Spec, new: Spec| {
            let (old, new) = (binary(&[old]), binary(&[new]));
            same_but_numbers(&old.items[0], &new.items[0])
        };
        for (old, new) in [
            ("scale.llvm.123.cold", "scale.llvm.456.cold"),
            ("assoc_def.llvm.123.cold", "assoc_def.cold"),
            (
                "Table [clone .llvm.123]",
                "Table [clone .llvm] [clone .456]",
            ),
        ] {
            assert!(same(function(old, b""), function(new, b"")), "{old}");
        }
        assert!(same(data("buf.1", b""), data("buf.3", b"")));
        // A copy is another function; and only a C variable's name has
        // GCC's number.
        assert!(!same(
            function("dev_read", b""),
            function("dev_read.constprop.0", b"")
        ));
        assert!(!same(function("f.1", b""), function("f.2", b"")));
    }

    #[test]
    fn rule_8_pairs_the_only_unnamed_object_in_a_section_without_bytes() {
        assert_eq!(
            pairs(
                &[Spec {
                    bss: true,
                    ..unnamed(".bss@0x3000", &[0; 8])
                }],
                &[Spec {
                    bss: true,
                    ..unnamed(".bss@0x3000", &[0; 16])
                }],
                &[],
            ),
            [paired(".bss@0x3000", ".bss@0x3000", PairedBy::Place)]
        );
    }
}
