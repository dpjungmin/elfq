//! The names that elfq makes for items that no symbol names, from where they
//! are: `fn_0x8000040` for a function, `.rodata@0x2004` for data, after its
//! section, and `entry` for the entry point. Such a *made name* says nothing
//! of which function or data the item is: `fn_0x…` and `.rodata@0x…` change
//! from build to build, and `entry` names only where execution starts. So
//! `diff` never pairs two builds' items by one ([`Item::has_made_name`]).
//! `startup` doesn't add the address to a name that already says it
//! ([`says_address`]), and leaves data named after where it is out of the
//! objects it names in a section ([`says_data_address`]). The names are made
//! here and told apart here, so that the two stay in step.

use super::{Item, Section, Source};

/// The name of a function that no symbol names, from its address:
/// `fn_0x8000040`.
pub(super) fn function_name_at(address: u64) -> String {
    format!("fn_{address:#x}")
}

/// The name of data that no symbol names, from its section and address:
/// `.rodata@0x2004`.
pub(super) fn data_name_at(section: &Section, address: u64) -> String {
    format!("{}@{address:#x}", section.name)
}

/// Whether `name` has the shape of `data_name_at`'s names, which say where
/// their data is: an `@` in it, as in `.rodata@0x2004`. A symbol whose name
/// has an `@` counts too.
pub fn says_data_address(name: &str) -> bool {
    name.contains('@')
}

/// The name of the entry point's function, when nothing else names it.
pub(super) const ENTRY_NAME: &str = "entry";

/// Whether `name` already says where its item is, as the made names of
/// functions and data do: it has the shape of a data name
/// ([`says_data_address`]), or starts with `fn_0x`. A symbol whose name
/// has an `@` counts too.
pub fn says_address(name: &str) -> bool {
    says_data_address(name) || name.starts_with("fn_0x")
}

impl Item {
    /// Whether elfq made the item's name from where it is: an item from
    /// the unwind table, the entry point, the code that elfq follows or a
    /// reference to data, or one named after its section and its address,
    /// as the vector table is when it is only part of its section. An item
    /// named after its whole section, such as `.init_array`, is named
    /// after what it is.
    pub fn has_made_name(&self) -> bool {
        match self.source {
            Source::EhFrame | Source::EntryPoint | Source::Reached | Source::Reference => true,
            Source::Section => self.name.contains("@0x"),
            Source::SymbolTable | Source::DynamicSymbolTable | Source::Plt => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::*;
    use crate::binary::ItemKind;

    fn item(name: &str, source: Source) -> Item {
        Item {
            name: name.to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Data,
            address: 0x2004,
            size: 4,
            size_estimated: false,
            section: 0,
            source,
        }
    }

    /// A section of read-only data at 0x2000.
    fn rodata() -> Section {
        Section {
            name: ".rodata".to_string(),
            address: 0x2000,
            size: 0x100,
            kind: SectionKind::ReadOnlyData,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        }
    }

    #[test]
    fn function_and_data_names_say_where_their_items_are() {
        let function = function_name_at(0x800_0040);
        let data = data_name_at(&rodata(), 0x2004);
        assert_eq!(function, "fn_0x8000040");
        assert_eq!(data, ".rodata@0x2004");
        assert!(says_address(&function));
        assert!(says_address(&data));
        assert!(says_data_address(&data));
        assert!(!says_data_address(&function));
        // The entry point's made name doesn't say where it is.
        assert!(!says_address(ENTRY_NAME));
        assert!(!says_address("main"));
    }

    #[test]
    fn has_made_name_follows_the_source() {
        let function = function_name_at(0x800_0040);
        let data = data_name_at(&rodata(), 0x2004);
        assert!(item(&function, Source::Reached).has_made_name());
        assert!(item(&function, Source::EhFrame).has_made_name());
        assert!(item(ENTRY_NAME, Source::EntryPoint).has_made_name());
        assert!(item(&data, Source::Reference).has_made_name());
        // The vector table, as part of its section or as all of it.
        assert!(item(&data, Source::Section).has_made_name());
        assert!(!item(".isr_vector", Source::Section).has_made_name());
        // A symbol's name is its own, whatever it looks like.
        assert!(!item(&function, Source::SymbolTable).has_made_name());
        assert!(!item("write@plt", Source::Plt).has_made_name());
    }
}
