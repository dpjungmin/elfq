//! Finding references in machine code.
//!
//! Each instruction set has a module with one function, `scan`: given a
//! function's bytes and address, it reports every address the code calls,
//! jumps to or otherwise uses. It doesn't need to understand anything else
//! about the instructions. The graph module then works out which item each
//! address belongs to.

mod x86_64;

use object::Architecture;

/// One use of an address by one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeReference {
    /// The instruction's address.
    pub site: u64,
    pub kind: CodeReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeReferenceKind {
    /// A direct call: `call target`.
    Call(u64),
    /// A direct jump, conditional or not: `jmp target`. Usually to another
    /// place in the same function; otherwise a tail call.
    Jump(u64),
    /// A call through the pointer stored at an address: `call [pointer]`.
    CallThrough(u64),
    /// A jump through the pointer stored at an address: `jmp [pointer]`.
    JumpThrough(u64),
    /// A call to an address computed at run time, such as `call rax`.
    IndirectCall,
    /// A jump to an address computed at run time, such as `jmp rax`.
    IndirectJump,
    /// Computing an address without touching memory there (`lea`), as when
    /// passing a pointer to a function or a variable.
    Address(u64),
    /// Reading or writing memory at an address, as when using a variable.
    Memory(u64),
}

/// Whether elfq can read code for this instruction set yet.
pub fn is_supported(architecture: Architecture) -> bool {
    architecture == Architecture::X86_64
}

/// The references in one function's code, which starts at `address`.
/// Returns nothing for instruction sets elfq can't read yet.
pub fn scan(architecture: Architecture, code: &[u8], address: u64) -> Vec<CodeReference> {
    match architecture {
        Architecture::X86_64 => x86_64::scan(code, address),
        _ => Vec::new(),
    }
}
