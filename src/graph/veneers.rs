//! Veneers: code that a linker adds where a call is too far for the call
//! instruction. The call goes to the veneer, which jumps on to the target,
//! as GNU ld's `__app_tick_veneer` does on Cortex-M. The target is a
//! function, or code outside every item, such as a routine in a
//! microcontroller's ROM, which elfq doesn't read.
//!
//! A PLT stub that passes calls on to one of the file's own functions, as
//! `log_msg@plt` does in a shared library, counts as a veneer for that
//! function. A stub may also jump to the header that lazy binding uses,
//! but [`Veneers::target`] gives only the function.

use std::collections::HashMap;

use crate::arch;
use crate::binary::{Binary, ItemKind, Source};

use super::{Graph, Node, ReferenceKind, Unresolved};

/// Where a veneer jumps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VeneerTarget {
    /// A function or import.
    Node(Node),
    /// Code outside every item, at this address, such as a routine in a
    /// microcontroller's ROM.
    Outside(u64),
}

/// The veneers in a file, and where each jumps to.
pub struct Veneers<'a> {
    binary: &'a Binary,
    graph: &'a Graph,
    /// The PLT stubs for the file's own functions, each with its function.
    stubs: HashMap<usize, usize>,
}

impl<'a> Veneers<'a> {
    /// Finds the PLT stubs for `binary`'s own functions. `graph` is
    /// `binary`'s graph.
    pub fn new(binary: &'a Binary, graph: &'a Graph) -> Veneers<'a> {
        Veneers {
            binary,
            graph,
            stubs: binary.own_function_stubs(),
        }
    }

    /// Where `item` jumps to, when it is a linker's veneer: its code is all
    /// a veneer (see `arch::is_veneer`), and jumps to one function, or to
    /// code outside every item. A PLT stub for one of the file's own
    /// functions passes calls on to it.
    pub fn target(&self, item: usize) -> Option<VeneerTarget> {
        if let Some(&function) = self.stubs.get(&item) {
            return Some(VeneerTarget::Node(Node::Item(function)));
        }
        let function = &self.binary.items[item];
        if function.kind != ItemKind::Function {
            return None;
        }
        let code = self.binary.item_bytes(function)?;
        let symbol = matches!(
            function.source,
            Source::SymbolTable | Source::DynamicSymbolTable
        )
        .then_some(function.name.as_str());
        if !arch::is_veneer(self.binary.architecture, code, symbol) {
            return None;
        }
        match (
            self.graph.references_from(item),
            self.graph.unresolved_in(item),
        ) {
            ([reference], []) if reference.kind == ReferenceKind::TailCall => {
                Some(VeneerTarget::Node(reference.to))
            }
            (
                [],
                [
                    Unresolved {
                        is_call: false,
                        target: Some(target),
                        ..
                    },
                ],
            ) => Some(VeneerTarget::Outside(*target)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn load(name: &str) -> (Binary, Graph) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/bin")
            .join(name);
        let binary = Binary::load(&path).unwrap();
        let graph = Graph::build(&binary);
        (binary, graph)
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    #[test]
    fn a_veneer_jumps_to_its_target() {
        // The veneers GNU ld added between ITCM and flash.
        let (binary, graph) = load("c-cortex-m7.elf");
        let veneers = Veneers::new(&binary, &graph);
        let target = |name| veneers.target(index(&binary, name));
        let function = |name| Some(VeneerTarget::Node(Node::Item(index(&binary, name))));
        assert_eq!(target("__app_tick_veneer"), function("app_tick"));
        assert_eq!(target("__memcpy_veneer"), function("memcpy"));
        assert_eq!(target("SysTick_Handler"), None);
        assert_eq!(target("fir_coeffs"), None);
        // One to a routine in the chip's ROM, outside every item.
        let (binary, graph) = load("far-gnu-m4-nodebug.elf");
        let veneers = Veneers::new(&binary, &graph);
        assert_eq!(
            veneers.target(index(&binary, "__rom_routine_veneer")),
            Some(VeneerTarget::Outside(0x1fff_0000))
        );
    }

    #[test]
    fn a_plt_stub_for_the_files_own_function_is_a_veneer() {
        let (binary, graph) = load("c-plt.so");
        let veneers = Veneers::new(&binary, &graph);
        assert_eq!(
            veneers.target(index(&binary, "negate@plt")),
            Some(VeneerTarget::Node(Node::Item(index(&binary, "negate"))))
        );
        assert_eq!(veneers.target(index(&binary, "negate")), None);
    }

    #[test]
    fn a_veneer_only_jumps_to_one_function() {
        let (binary, mut graph) = load("c-cortex-m7.elf");
        // Calling on is not what a veneer does.
        let veneer = index(&binary, "__memcpy_veneer");
        let start = graph.references.partition_point(|r| r.from < veneer);
        graph.references[start].kind = ReferenceKind::Call;
        assert_eq!(Veneers::new(&binary, &graph).target(veneer), None);
        // Nor is a function a veneer when it only makes one tail call, but
        // its code isn't a veneer's.
        let handler = index(&binary, "SysTick_Handler");
        graph
            .references
            .retain(|r| r.from != handler || r.kind == ReferenceKind::TailCall);
        assert_eq!(graph.references_from(handler).len(), 1);
        assert_eq!(Veneers::new(&binary, &graph).target(handler), None);
    }
}
