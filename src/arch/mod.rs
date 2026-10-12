//! Finding references in machine code.
//!
//! Each instruction set has a module with one function, `scan`: given a
//! function's bytes and address, it reports every address the code calls,
//! jumps to or otherwise uses. It doesn't need to understand anything else
//! about the instructions. The graph module then works out which item each
//! address belongs to.

mod thumb;
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
    /// An address that code computes or loads as a number, without touching
    /// memory there: `lea` on x86-64, or on Arm a constant from a literal
    /// pool or a `movw`/`movt` pair. Code does this to pass a pointer, and
    /// on Arm also before reading or writing a variable.
    Address(u64),
    /// Reading or writing memory at an address, as when using a variable.
    Memory(u64),
}

/// Whether elfq can read a file's code yet: x86-64, or 32-bit Arm when the
/// code is Thumb, which is what Cortex-M runs. Code in the older Arm (A32)
/// instruction set isn't read.
pub fn is_supported(architecture: Architecture, thumb: bool) -> bool {
    match architecture {
        Architecture::X86_64 => true,
        Architecture::Arm => thumb,
        _ => false,
    }
}

/// The references in one function's code, which starts at `address`.
/// Returns nothing for instruction sets elfq can't read yet; check
/// [`is_supported`] first.
pub fn scan(architecture: Architecture, code: &[u8], address: u64) -> Vec<CodeReference> {
    match architecture {
        Architecture::X86_64 => x86_64::scan(code, address),
        Architecture::Arm => thumb::scan(code, address),
        _ => Vec::new(),
    }
}
