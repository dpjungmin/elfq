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
//! What elfq reads of code goes through [`InstructionSet`], which has
//! only the instruction sets elfq can read.
//!
//! In stripped Cortex-M firmware, elfq also has to work out where functions
//! end, so the Thumb module also tells the padding between functions apart
//! and how a stretch of code ends.
//!
//! `diff` needs to know which bits of a function's code say where something
//! is, since those change whenever code or data moves (see
//! [`InstructionSet::operands`]).
//!
//! `listing.rs` prints a function's code as text, for `elfq FILE browse`.
//!
//! `startup` follows the code that a Cortex-M processor runs from reset to
//! `main`, keeping the numbers in its registers and memory, to tell what
//! the startup code copies and zeroes.

#[cfg(feature = "tui")]
mod listing;
pub mod startup;
mod thumb;
mod x86_64;

#[cfg(feature = "tui")]
pub use listing::{Line, Refers};

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
    /// register that the code put the target in. On any instruction set,
    /// `Binary::code_references` also gives one from the last instruction
    /// of an entry point that runs on into the next function (see
    /// [`InstructionSet::runs_on`]). The graph treats it as a `Jump`.
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

/// The bits of one function's code that say where something is, as
/// [`InstructionSet::operands`] finds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operands {
    /// For each byte of the code, the bits of it that hold the distance from
    /// an instruction to an address outside the code: the target of a call
    /// or jump, or the address of a constant that a load reads or of
    /// something whose address the code takes. They change whenever the
    /// function or what it uses moves. A distance to a place inside the
    /// code, as a loop's jump, doesn't change when the function moves, and
    /// isn't here.
    pub relative: Vec<u8>,
    /// Where each of those distances leads, as (the offset of its
    /// instruction in the code, the address), in the order of the
    /// instructions.
    pub targets: Vec<(usize, u64)>,
    /// The numbers that two instructions put together, as an address mostly.
    pub split: Vec<SplitNumber>,
}

/// A 32-bit number that a pair of instructions puts together in a
/// register, as Thumb's `movw` and `movt` do, the first with the low 16
/// bits and the second with the high 16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitNumber {
    pub value: u64,
    /// The bits of the code that hold it, as (offset into the code, bits of
    /// the byte there).
    pub bits: Vec<(usize, u8)>,
}

/// An instruction set that elfq can read: x86-64, or Thumb, the
/// instruction set that Cortex-M runs. Code in the older Arm (A32)
/// instruction set isn't read. What elfq reads of code goes through these
/// methods, so code that elfq can't read never reaches a scanner, and a new
/// instruction set needs an arm in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionSet {
    X86_64,
    /// With the file's byte order, for reading the constants in the code:
    /// in big-endian Arm firmware (BE8), the instructions are still
    /// little-endian.
    Thumb {
        endianness: Endianness,
    },
}

impl InstructionSet {
    /// The instruction set of a file's code, or `None` when elfq can't read
    /// it yet. `thumb` says whether 32-bit Arm code is Thumb (see
    /// `Binary::thumb`).
    pub fn of(architecture: Architecture, thumb: bool, endianness: Endianness) -> Option<Self> {
        match architecture {
            Architecture::X86_64 => Some(Self::X86_64),
            Architecture::Arm if thumb => Some(Self::Thumb { endianness }),
            _ => None,
        }
    }

    /// The references in one function's code, which starts at `address`.
    pub fn scan(self, code: &[u8], address: u64) -> Vec<CodeReference> {
        match self {
            Self::X86_64 => x86_64::scan(code, address),
            Self::Thumb { endianness } => thumb::scan(code, address, endianness),
        }
    }

    /// The bits of one function's code, which starts at `address`, that say
    /// where something is, and where each distance leads, for `diff` to tell
    /// apart the bytes of two builds of a function that differ because
    /// things moved from those that differ because the code changed: on
    /// x86-64, the distance in a call or jump and in an operand relative to
    /// the instruction pointer, as in `lea rdi, [rip + 0x2f3e]`; on Thumb,
    /// the distance in `bl`, `b`, `b<cond>`, `cbz`, `cbnz`, `adr` and a load
    /// from a literal pool, each in its 16- and 32-bit forms, and the
    /// numbers that `movw` and `movt` put together. Thumb code is swept as
    /// [`InstructionSet::scan`] sweeps it, so the constants and jump tables
    /// it reads hold no distances.
    pub fn operands(self, code: &[u8], address: u64) -> Operands {
        match self {
            Self::X86_64 => x86_64::operands(code, address),
            Self::Thumb { endianness } => thumb::operands(code, address, endianness),
        }
    }

    /// Where the last instruction of the code that starts at `address` is,
    /// when the processor can run on past the end of the code after it:
    /// unless that instruction always jumps, returns or traps. A
    /// conditional jump runs on when it isn't taken, and a call counts as
    /// running on, since elfq can't tell whether what it calls returns.
    /// `None` when the code stops before its end. Padding after the last
    /// instruction doesn't count.
    pub fn runs_on(self, code: &[u8], address: u64) -> Option<u64> {
        match self {
            Self::X86_64 => x86_64::runs_on(code, address),
            Self::Thumb { endianness } => thumb::runs_on(code, address, endianness),
        }
    }

    /// Whether `code` is all of a veneer: a stub that a linker puts near a
    /// call whose target is too far away for the call instruction, and that
    /// only jumps on to the target. Only Thumb has them: those that GNU ld
    /// and LLD add for Cortex-M. `name` is the code's symbol, when it has
    /// one: code that a compiler may also make is a veneer only under a
    /// linker's name.
    pub fn is_veneer(self, code: &[u8], name: Option<&str>) -> bool {
        match self {
            Self::X86_64 => false,
            Self::Thumb { .. } => thumb::is_veneer(code, name),
        }
    }

    // The next four matter only for finding the functions in stripped
    // firmware, which elfq does only for Thumb. The x86-64 arms of the last
    // three answer as for code with no padding between functions that may
    // always run on.

    /// The references that each instruction in one function's code makes on
    /// its own: like [`InstructionSet::scan`], but without following an
    /// address from the instruction that puts it in a register to one that
    /// branches through it, so such a branch is always through a pointer.
    /// Code split in two finds the same references this way as the code
    /// together, unless an instruction or the constants it loads are cut in
    /// two. x86-64 follows no address through a register, so there this is
    /// [`InstructionSet::scan`].
    pub fn scan_instructions(self, code: &[u8], address: u64) -> Vec<CodeReference> {
        match self {
            Self::X86_64 => x86_64::scan(code, address),
            Self::Thumb { endianness } => thumb::scan_instructions(code, address, endianness),
        }
    }

    /// How many bytes at the start of `code` a function's instructions and
    /// constants take, when `code` may run on past its end to the next
    /// function: without the padding that aligns the next function.
    pub fn code_size(self, code: &[u8], address: u64) -> u64 {
        let size = match self {
            Self::X86_64 => code.len(),
            Self::Thumb { endianness } => thumb::code_size(code, address, endianness),
        };
        size as u64
    }

    /// How many bytes of padding between functions `code` starts with.
    pub fn leading_padding(self, code: &[u8]) -> u64 {
        let size = match self {
            Self::X86_64 => 0,
            Self::Thumb { .. } => thumb::leading_padding(code),
        };
        size as u64
    }

    /// How the code that starts at `address` ends.
    pub fn ending(self, code: &[u8], address: u64) -> Ending {
        match self {
            Self::X86_64 => Ending::RunsOn,
            Self::Thumb { endianness } => thumb::ending(code, address, endianness),
        }
    }

    /// What the startup walk reads `program`'s code through, to follow it
    /// as a Cortex-M processor runs it from reset: `None` for x86-64, which
    /// has no such walk.
    pub fn startup_code(self, program: &dyn startup::Program) -> Option<startup::Code<'_>> {
        match self {
            Self::X86_64 => None,
            Self::Thumb { endianness } => Some(startup::Code::thumb(program, endianness)),
        }
    }
}
