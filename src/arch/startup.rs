//! Following the code that a Cortex-M processor runs from reset, before
//! `main`: the startup code that sets the stack and copies and zeroes
//! memory, in the order it runs.
//!
//! The walk goes one way through the code, as the processor would, keeping
//! the numbers that it can work out in registers, on the stack and in the
//! memory that the code writes, and the condition flags, so that it takes
//! a branch the way the processor would when it can tell. It records what
//! a check of the startup code needs: each call, with the numbers in its
//! argument registers, each store outside the stack, each loop, and how
//! the walk ends. See [`walk`] for what it follows and where it stops.

use object::{Architecture, Endianness};

use super::thumb;

/// What [`walk`] reads of a program, which the caller works out from the
/// file.
pub trait Program {
    /// The bytes of code that the file holds from `address` to the end of
    /// the section of code that holds it, as they are once startup code
    /// has copied the section into place, if it has a load image. `None`
    /// where the file holds no code.
    fn code(&self, address: u64) -> Option<&[u8]>;

    /// The word in memory at `address` when the processor starts, before
    /// any code runs: what the file stores where it isn't copied into
    /// place, as in flash. `None` where memory holds nothing that the file
    /// says, as in RAM, where startup code copies load images to.
    fn word_at_reset(&self, address: u64) -> Option<u32>;

    /// Where the function whose code holds `address` starts and ends.
    fn function_at(&self, address: u64) -> Option<(u64, u64)>;
}

/// Where a walk starts, and what it knows of the program before it does.
#[derive(Debug, Clone, Copy)]
pub struct Start {
    /// Where the processor starts running code: the reset handler.
    pub address: u64,
    /// The stack pointer at reset, from the first word of the vector
    /// table. Stores between the lowest value the stack pointer gets and
    /// this one are to the stack. `None` without a vector table.
    pub stack_pointer: Option<u32>,
    /// Where `main` starts. The walk ends at a call to it, or when it gets
    /// there, as where a compiler inlined `main` into the reset handler.
    pub main: Option<u64>,
    /// How many calls deep the walk follows, at most [`DEPTH`]: 0 walks
    /// only the code at `address`, and what it jumps to.
    pub follow: u32,
}

/// How many calls deep a walk follows at most.
pub const DEPTH: u32 = 3;

/// What a walk found, in the order the code runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    pub events: Vec<Event>,
    /// How the walk ended, in the reset handler or what it called on its
    /// way to `main`.
    pub end: End,
}

/// One thing the startup code does, at `depth` calls from the reset
/// handler, where the reset handler's own code is at depth 0. A tail call
/// takes the place of the function that makes it, so what the function it
/// jumps to does is at that function's depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A call, or a tail call.
    Call(Call),
    /// A store outside the stack.
    Store(Store),
    /// A loop. The walk goes through its code once, and then knows nothing
    /// of the registers and memory that the code changes.
    Loop(Loop),
    /// The walk went back to a branch whose condition it couldn't tell, and
    /// took it, since the way it had gone ended where it couldn't go on
    /// (see [`End::is_dead`]), or spun or returned before `main`. It does so
    /// only in the reset handler's own code and in what that tail-calls.
    /// What it found that way is dropped.
    Backtrack {
        /// The branch.
        site: u64,
        /// Where it goes.
        target: u64,
        /// How the other way ended.
        because: End,
    },
    /// A function that the startup code calls at `site` goes where the walk
    /// can't follow, or on past as many instructions as the walk goes
    /// through in a function: `end` says where, and is dead (see
    /// [`End::is_dead`]) or [`End::Budget`]. `depth` is the function's.
    Stop { site: u64, depth: u32, end: End },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The call instruction.
    pub site: u64,
    pub depth: u32,
    /// What it calls, when the walk can tell.
    pub target: Option<u64>,
    pub how: How,
    /// The numbers in r0 to r3, which pass the first four arguments, when
    /// the walk can tell.
    pub arguments: [Option<u32>; 4],
    /// Of r0 to r3, as bits, those that hold what an earlier call left
    /// there, which code hasn't written since: those that the code it ran
    /// wrote, and r0 when it read it, as a function such as `memset`
    /// returns its first argument there.
    pub left: u8,
    /// Whether it runs only when condition flags that the walk can't tell
    /// are set: a call in an `it` block, or a branch to another function.
    /// The walk doesn't follow it.
    pub conditional: bool,
    /// Whether the walk went through the code it calls, or found it outside
    /// the file: not for a call through a register it can't tell, a
    /// conditional one, or one [`DEPTH`] calls deep.
    pub followed: bool,
}

impl Call {
    /// The numbers it passes, as far as elfq takes them to be: `arguments`,
    /// but not those that an earlier call left, which a function that
    /// takes fewer arguments finds there too, as memset leaves the start
    /// of what it zeroed in r0. A number that code set before an earlier
    /// call that didn't write it counts, as code built with GCC's
    /// `-fipa-ra` keeps one there for the next call.
    pub fn passes(&self) -> [Option<u32>; 4] {
        let mut numbers = self.arguments;
        for (n, number) in numbers.iter_mut().enumerate() {
            if self.left & 1 << n != 0 {
                *number = None;
            }
        }
        numbers
    }
}

/// How code calls a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A call to an address in the instruction: `bl`.
    Direct,
    /// A call to the address in a register: `blx` and the number of the
    /// register.
    Register(u16),
    /// A tail call: a jump to another function, which returns to the
    /// caller's caller, as `b` to one does, and a linker's veneer's jump.
    Jump,
    /// A supervisor call, `svc`, which runs the handler of SVCall.
    Supervisor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// The store instruction.
    pub site: u64,
    pub depth: u32,
    /// Where it stores, when the walk can tell.
    pub address: Option<u32>,
    /// The address in its base register, when the walk can tell: where
    /// it stores from on, when `address` is `None` because the walk
    /// doesn't know what it adds to the base, as with an offset in a
    /// register, or a store of several registers. One that stores below
    /// it, as `stmdb` does, stores that far off from it.
    pub base: Option<u32>,
    /// How many bytes it stores, when one instruction stores a known
    /// number of them; 0 when it doesn't say, as for the floating-point
    /// `vstm`.
    pub size: u32,
    /// The word it stores, when it stores one from a register and the walk
    /// can tell what that holds, or stores several that all hold it.
    pub value: Option<u32>,
    /// Where the code loaded the first word it stores from, when it loaded
    /// it from an address the walk can tell and hasn't changed the
    /// register since: for a copy, where it copies from.
    pub source: Option<Source>,
    /// Whether it stores only when condition flags that the walk can't
    /// tell are set, in an `it` block.
    pub conditional: bool,
}

/// Where a word in a register came from: the load instruction, and the
/// address it loaded the word from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub site: u64,
    pub address: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// Where each pass starts.
    pub head: u64,
    /// The branch back to `head`, or out of a loop body that the compiler
    /// put after the function's other code.
    pub back: u64,
    pub depth: u32,
    /// The first of the events that the walk found in the loop's code: the
    /// events from it up to this loop's are the loop's own.
    pub first_event: usize,
    /// Where the walk of the loop's code stopped before it got to `back`,
    /// if it did: then the events are only some of the loop's, and the
    /// walk goes on knowing nothing of the registers and memory but the
    /// stack pointer.
    pub end: Option<End>,
    /// How many times the loop's code runs from its head, the last time to
    /// where it leaves, when the walk can tell (see [`walk`]).
    pub passes: Option<u32>,
    /// What each store of the loop's own code does over all the times it
    /// runs, in the order of the first time, when the walk can tell how
    /// many times the loop runs and what each store does each time.
    pub stores: Option<Vec<LoopStore>>,
    /// When the walk went through the loop's code each time it runs, with
    /// the calls it makes and the loops inside, as for a loop over a table
    /// of sections: each time, the last to where the loop leaves. Then the
    /// loop's events are those of every time, in order, and `stores` is
    /// `None`. Empty otherwise.
    pub times: Vec<Time>,
}

/// One time through the code of a loop that the walk went through each
/// time it runs (see [`Loop::times`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    /// The first of its events: those up to the next time's, or the
    /// loop's, are its own.
    pub first_event: usize,
    /// Where it reads a row of a table from: the lowest address in the
    /// file, outside the stack, that the code of the loop's function loads
    /// a word from that time, or that a register holds from the time
    /// before that the time reads, but for the words that the times so far
    /// copy; when the register of a load that time points there and none
    /// points lower, the rows of the times that load one follow each other,
    /// as far apart each time, and each time reads the same words of its
    /// row, but the last, which may read fewer.
    pub row: Option<u32>,
}

/// A store in a loop's code, over all the times the loop runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopStore {
    /// The store instruction.
    pub site: u64,
    /// Where it stores the first time.
    pub address: u32,
    /// How far that moves each time.
    pub step: i32,
    /// How many bytes it stores each time.
    pub size: u32,
    /// How many times it stores: one fewer than the loop runs when it
    /// doesn't store the last time, as when the loop leaves first.
    pub times: u32,
    /// The word it stores, when it is the same each time.
    pub value: Option<u32>,
    /// Where it loads the first word it stores from, the first time, and
    /// how far that moves each time, when the walk can tell.
    pub source: Option<(u32, i32)>,
    /// Whether it stores a word that the loop's code loaded.
    pub loaded: bool,
}

/// Where a walk stops: the instruction it stopped at, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// It reached `main`: a call to it, or its code.
    Main(u64),
    /// A branch to itself, where the processor waits for good.
    Spins(u64),
    /// A return.
    Returns(u64),
    /// The branch back of a loop that nothing leaves that the walk knows
    /// of.
    Loops(u64),
    /// Code that the walk already went through, other than a loop's head:
    /// a loop that it can't make out, as one whose code runs on into its
    /// test at the end, which GCC's `-Og` makes of CMSIS's
    /// `__cmsis_start`.
    Rejoins(u64),
    /// Where code runs on to after a call that can't return: the end of its
    /// function, or straight after the call, data, such as a literal pool,
    /// or `udf`, which compilers put after such a call.
    RunsOff(u64),
    /// Data, or an instruction elfq can't decode, or that traps, or no
    /// code at all, as at a reset handler in RAM.
    Undecoded(u64),
    /// A branch through a register, through a word loaded from memory or
    /// to a case of a jump table, which the walk can't tell.
    Branches { site: u64, through: Through },
    /// A branch or call to `target`, which is not in the code the file
    /// holds, as in a boot ROM.
    LeavesFile { site: u64, target: u64 },
    /// The walk went through as many instructions as it may, or for a
    /// loop's code, as many loops inside loops.
    Budget(u64),
}

/// What a branch that the walk can't follow goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Through {
    Register(u16),
    Memory,
    Table,
}

impl End {
    /// The instruction where the walk stopped.
    pub fn site(self) -> u64 {
        match self {
            End::Main(site)
            | End::Spins(site)
            | End::Returns(site)
            | End::Loops(site)
            | End::Rejoins(site)
            | End::RunsOff(site)
            | End::Undecoded(site)
            | End::Branches { site, .. }
            | End::LeavesFile { site, .. }
            | End::Budget(site) => site,
        }
    }

    /// Whether the walk ended where the code goes on in a way it can't
    /// follow: code it can't decode, or a branch it can't tell, or that
    /// leaves the code the file holds. Other ends are where the code
    /// doesn't go on, or a limit of the walk's.
    pub fn is_dead(self) -> bool {
        matches!(
            self,
            End::Undecoded(_) | End::Branches { .. } | End::LeavesFile { .. }
        )
    }

    /// Whether code goes on past where the walk ended that it didn't go
    /// through: where it can't follow the code, at its budget, or in a
    /// loop it can't make out.
    pub fn stops_short(self) -> bool {
        self.is_dead() || matches!(self, End::Budget(_) | End::Rejoins(_))
    }
}

/// Walks the code from `start`, as [`Start`] says, in a program in Thumb
/// code. `None` for other architectures; it reads any 32-bit Arm code as
/// Thumb, so check [`super::is_supported`] first.
///
/// What it follows:
///
/// - Numbers in registers that code puts there: constants, loaded from a
///   literal pool or built with `adr`, `movw` and `movt`, and what
///   adding, subtracting, shifting and masking makes of them.
/// - Words that code stores at an address the walk can tell, as on the
///   stack, and loads back, a word or several at once, and where each
///   word in a register was loaded from (see [`Store::source`]). A load
///   from anywhere else reads what is there at reset (see
///   [`Program::word_at_reset`]).
/// - The condition flags that compares, `adds` and `subs` set, so that it
///   takes a branch, and runs an instruction in an `it` block, as the
///   processor would. Where it can't tell, a branch is taken later, if the
///   way on doesn't go on (see [`Event::Backtrack`]); an instruction in an
///   `it` block leaves what it writes unknown.
/// - Calls, up to [`Start::follow`] deep, never into `main`. A called
///   function starts with the caller's registers and memory. Where it
///   returns, the caller goes on with what the function left in the
///   registers, in memory and in the flags, as a result in r0 and r1 or
///   what code built with GCC's `-fipa-ra` counts on staying in r2 and r3,
///   but with its own stack pointer and lr unknown. If the function got to
///   the return past a branch that the walk couldn't tell, all it changed
///   is unknown. A call that the walk doesn't follow, or that doesn't get
///   to a return, leaves unknown the flags and those of r0 to r3, ip and
///   lr that the function writes (all of them if it calls on, or the walk
///   can't tell where its code is), and, for one it followed, the words it
///   changed in memory. It doesn't follow a call that it can't tell runs:
///   one in an `it` block, or a branch to another function (see
///   [`Call::conditional`]).
/// - Code that runs on past the end of a function: after a call, the call
///   can't return (see [`End::RunsOff`]); otherwise, the code goes on into
///   the next function.
///
/// It goes through a loop's code, and then counts how many times the loop
/// runs as the processor would, when it can: it goes through the code
/// again, the first three times, and then a compare in it tells when a
/// number that moves by the same amount each time gets to where the loop
/// leaves, which it checks by going through the code the time before and
/// that time, from where each number would be; or when none does, it goes
/// on through the code time by time, up to 16 times (see
/// [`Loop::passes`]). The walk then goes on from where the loop leaves,
/// and the loop's events are those of the first time. To count, it
/// doesn't follow calls or count loops inside, so it can't tell what the
/// stores of such a loop do (see [`Loop::stores`]); when such a loop runs
/// at most 16 times, as one over a table of sections that copies each
/// with a function or a loop, it goes through its code each time, with
/// its calls and the loops inside, and the loop's events are those of
/// every time (see [`Loop::times`]), but not when it calls through a
/// register another function each time, as one over a table of functions
/// does, such as the constructors that `__libc_init_array` calls, or one
/// it can't tell, and not past 5,000 instructions in all in such times
/// and 10,000 more to count the loops inside them. A loop it can't
/// count, as one whose bound it can't tell, leaves what its code writes
/// unknown. It stops
/// after 20,000 instructions in all and 600 in the reset handler's own
/// code. It goes through 300 in each function it calls, and 60 in a
/// loop's code, 8 loops deep, and then goes on after the call or the loop
/// (see [`Event::Stop`] and [`Loop::end`]).
pub fn walk(
    architecture: Architecture,
    endianness: Endianness,
    program: &dyn Program,
    start: &Start,
) -> Option<Walk> {
    match architecture {
        Architecture::Arm => Some(thumb::startup::walk(endianness, program, start)),
        _ => None,
    }
}

/// Whether the code after the call at `first` and before the call at
/// `second` only copies registers, or loads words from a literal pool, and
/// leaves in r0 what `first` returns there, so that `second` gets it, as in
/// `exit(main())`: `bl main; mov r3, r0; mov r0, r3; bl exit`. False when
/// the calls are more than 16 bytes apart.
pub fn passes_result(
    architecture: Architecture,
    program: &dyn Program,
    first: u64,
    second: u64,
) -> bool {
    match architecture {
        Architecture::Arm => thumb::startup::passes_result(program, first, second),
        _ => false,
    }
}

/// Whether a branch in the code from `start` up to the call at `call` goes
/// to the instruction after the call: then the call doesn't return, and
/// the compiler put other code after it, such as a loop's.
pub fn branches_past(
    architecture: Architecture,
    program: &dyn Program,
    start: u64,
    call: u64,
) -> bool {
    match architecture {
        Architecture::Arm => thumb::startup::branches_past(program, start, call),
        _ => false,
    }
}
