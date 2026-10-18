//! Finding references in machine code.
//!
//! Each instruction set has a module with a function `scan`: given a
//! function's bytes and address, it reports every address the code calls,
//! jumps to or otherwise uses. x86-64 needs nothing else about the
//! instructions; Thumb also needs what each one does with the registers and
//! the stack (`thumb/decode.rs`), to follow an address into a branch
//! through a register. The graph module then works out which item each
//! address belongs to.
//!
//! In stripped Cortex-M firmware, elfq also has to work out where functions
//! end, so the Thumb module also tells the padding between functions apart
//! and how a stretch of code ends.

mod thumb;
mod x86_64;

use object::{Architecture, Endianness};

/// One use of an address by one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeReference {
    /// The instruction's address.
    pub site: u64,
    /// What the instruction does with the address.
    pub kind: CodeReferenceKind,
}

/// What one instruction does with an address. `Graph::build` turns most into
/// a reference to the item there, or into a call or jump it can't follow.
/// It drops a jump that stays inside its function, as most do, a call into
/// its function's middle, and an address where no item is, unless the
/// loader stores a pointer there, as in a global offset table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeReferenceKind {
    /// A direct call: `call target`, or on Thumb, a call through a
    /// register that the code put `target` in, as a long call does.
    Call(u64),
    /// A direct jump, conditional or not: `jmp target`, or to a case of a
    /// jump table. Usually to another place in the same function; otherwise
    /// a tail call.
    Jump(u64),
    /// A direct jump of the kind that compilers and linkers use to carry on
    /// in another function. Only Thumb tells these apart, and only as a
    /// hint: an unconditional `b.w`, which long functions also use inside
    /// themselves, the jump of a linker's veneer, and a jump through a
    /// register that the code put the target in. The graph treats it as a
    /// `Jump`.
    TailJump(u64),
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
    /// on Arm also before reading or writing a variable. On Thumb, an
    /// address that the code only branches to is a call or jump instead.
    Address(u64),
    /// Reading or writing memory at an address, as when using a variable.
    Memory(u64),
}

/// How a stretch of code ends, which tells whether another function can
/// start right after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The processor can run on past the end: the code ends inside an
    /// instruction or an `it` block, or with an instruction that runs on
    /// into the next.
    RunsOn,
    /// With a call, which returns unless what it calls never does, as with
    /// a panic.
    Call,
    /// With a jump, a return or data, such as a literal pool.
    Stops,
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
/// `endianness` is the file's, for reading the constants in Thumb code.
/// Returns nothing for instruction sets other than x86-64 and 32-bit Arm,
/// and reads any 32-bit Arm code as Thumb, so check [`is_supported`] first.
pub fn scan(
    architecture: Architecture,
    endianness: Endianness,
    code: &[u8],
    address: u64,
) -> Vec<CodeReference> {
    match architecture {
        Architecture::X86_64 => x86_64::scan(code, address),
        Architecture::Arm => thumb::scan(code, address, endianness),
        _ => Vec::new(),
    }
}

/// The references that each instruction in one function's code makes on
/// its own: like [`scan`], but without following an address from the
/// instruction that puts it in a register to one that branches through
/// it, so such a branch is always through a pointer. Code split in two
/// finds the same references this way as the code together, unless an
/// instruction or the constants it loads are cut in two.
pub fn scan_instructions(
    architecture: Architecture,
    endianness: Endianness,
    code: &[u8],
    address: u64,
) -> Vec<CodeReference> {
    match architecture {
        Architecture::X86_64 => x86_64::scan(code, address),
        Architecture::Arm => thumb::scan_instructions(code, address, endianness),
        _ => Vec::new(),
    }
}

/// How many bytes at the start of `code` a function's instructions and
/// constants take, when `code` may run on past its end to the next
/// function. Only Thumb tells the padding between functions apart; for
/// other instruction sets, this is all of `code`.
pub fn code_size(
    architecture: Architecture,
    endianness: Endianness,
    code: &[u8],
    address: u64,
) -> u64 {
    let size = match architecture {
        Architecture::Arm => thumb::code_size(code, address, endianness),
        _ => code.len(),
    };
    size as u64
}

/// Whether `code` is all of a veneer: a stub that a linker puts near a call
/// whose target is too far away for the call instruction, and that only
/// jumps on to the target. Only Thumb has them: those that GNU ld and LLD
/// add for Cortex-M. `name` is the code's symbol, when it has one: code
/// that a compiler may also make is a veneer only under a linker's name.
pub fn is_veneer(architecture: Architecture, code: &[u8], name: Option<&str>) -> bool {
    match architecture {
        Architecture::Arm => thumb::is_veneer(code, name),
        _ => false,
    }
}

/// How many bytes of padding between functions `code` starts with. Only
/// Thumb tells padding apart; for other instruction sets, this is 0.
pub fn leading_padding(architecture: Architecture, code: &[u8]) -> u64 {
    let size = match architecture {
        Architecture::Arm => thumb::leading_padding(code),
        _ => 0,
    };
    size as u64
}

/// How the code that starts at `address` ends. Only Thumb is read; for other
/// instruction sets, the code may always run on.
pub fn ending(
    architecture: Architecture,
    endianness: Endianness,
    code: &[u8],
    address: u64,
) -> Ending {
    match architecture {
        Architecture::Arm => thumb::ending(code, address, endianness),
        _ => Ending::RunsOn,
    }
}
