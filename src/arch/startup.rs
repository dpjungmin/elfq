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
//! the walk ends. See [`Code::walk`] for what it follows and where it
//! stops.
//!
//! `commands::startup` asks for the walk and reads what it finds, and the
//! walk through Thumb code is in `arch::thumb::startup`. docs/design.md's
//! "Startup, end to end" follows one run through all of them.

use std::ops::Range;

use object::Endianness;

use super::thumb;

/// What [`Code::walk`] reads of a program, which the caller works out from
/// the file.
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
    fn function_at(&self, address: u64) -> Option<FunctionBounds>;
}

/// Where a function's code starts and ends, as [`Program::function_at`]
/// gives it. `arch` is below `binary`, so it has this small struct of its
/// own rather than `binary::AddressRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionBounds {
    /// The address of its first byte.
    pub start: u64,
    /// The address just past its last byte.
    pub end: u64,
}

impl FunctionBounds {
    /// Whether `address` is in the function.
    pub fn contains(self, address: u64) -> bool {
        (self.start..self.end).contains(&address)
    }
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
    /// What the startup code does, in the order it runs, which says how
    /// the events nest:
    ///
    /// - A call that the walk follows comes before the events of the code
    ///   it calls, which are one deeper (see [`Event::depth`]) and end at
    ///   the first event after the call that is no deeper, but for a
    ///   backtrack. When the walk of that code stops short, an
    ///   [`Event::Stop`] at the callee's depth comes last.
    /// - A tail call's code goes on at the depth of the function that
    ///   jumps, so its events end where that function's would: at the
    ///   first event that is less deep.
    /// - A loop comes after the events of its code, which [`Loop::events`]
    ///   says, and those of each iteration that the walk went through one
    ///   by one ([`Iteration::events`]).
    /// - A backtrack drops the events that the walk found after the branch
    ///   it goes back to, and comes in their place.
    pub events: Vec<Event>,
    /// How the walk ended, in the reset handler or what it called on its
    /// way to `main`.
    pub end: End,
}

/// One thing the startup code does, at a depth of calls from the reset
/// handler (see [`Event::depth`]). [`Walk::events`] says how they nest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A call, or a tail call.
    Call(Call),
    /// A store outside the stack.
    Store(Store),
    /// A loop, after the events of its code (see [`Loop::events`]). When
    /// the walk can tell how many times it runs ([`Loop::runs`]), it goes
    /// on from where the loop leaves with what the loop leaves in the
    /// registers and memory; otherwise with what the loop's code writes
    /// unknown.
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

impl Event {
    /// How many calls deep from the reset handler the event is: 0 in the
    /// reset handler's own code, 1 in a function it calls, and so on. A
    /// tail call takes the place of the function that makes it, so what
    /// the function it jumps to does is at that function's depth. A
    /// backtrack is at 0, since the walk backtracks only in the reset
    /// handler's own code and in what that tail-calls.
    pub fn depth(&self) -> u32 {
        match self {
            Event::Call(call) => call.depth,
            Event::Store(store) => store.depth,
            Event::Loop(l) => l.depth,
            Event::Stop { depth, .. } => *depth,
            Event::Backtrack { .. } => 0,
        }
    }
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
    /// Where each iteration starts.
    pub head: u64,
    /// The branch back to `head`, or out of a loop body that the compiler
    /// put after the function's other code.
    pub back: u64,
    pub depth: u32,
    /// The events that the walk found in the loop's code, which come just
    /// before this loop's own: those of each iteration in turn, when the
    /// walk went through each ([`Runs::EachIteration`]), and otherwise
    /// those of the one time it went through the code.
    pub events: Range<usize>,
    /// How many times the loop runs, as far as the walk can tell.
    pub runs: Runs,
}

/// How many times a loop runs, as far as the walk can tell, and what the
/// walk knows of it then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runs {
    /// The walk can't tell. It goes on from the loop with what the loop's
    /// code writes unknown. `stopped` is where the walk of the loop's
    /// code stopped before it got to `back`, if it did: then the events
    /// are only some of the loop's, and when the loop's code stores, the
    /// walk goes on knowing no word in memory either.
    Uncounted { stopped: Option<End> },
    /// It runs `iterations` times from its head, the last to where it
    /// leaves (see [`Code::walk`]). `stores` says what each store of the
    /// loop's own code does over all its iterations, in the order of the
    /// first, when the walk can tell what each store does each iteration.
    Counted {
        iterations: u32,
        stores: Option<Vec<LoopStore>>,
    },
    /// The walk went through the loop's code each time it runs, with the
    /// calls it makes and the loops inside, as for a loop over a table of
    /// sections: these are its iterations, the last to where the loop
    /// leaves, and the loop's events are theirs, in order.
    EachIteration(Vec<Iteration>),
}

impl Loop {
    /// How many times the loop runs, when the walk can tell.
    pub fn iterations(&self) -> Option<u32> {
        match &self.runs {
            Runs::Uncounted { .. } => None,
            Runs::Counted { iterations, .. } => Some(*iterations),
            Runs::EachIteration(iterations) => Some(iterations.len() as u32),
        }
    }

    /// What each store of the loop's own code does over all its
    /// iterations, when the walk can tell (see [`Runs::Counted`]).
    pub fn stores(&self) -> Option<&[LoopStore]> {
        match &self.runs {
            Runs::Counted { stores, .. } => stores.as_deref(),
            _ => None,
        }
    }

    /// The iterations that the walk went through one by one (see
    /// [`Runs::EachIteration`]); none for a loop whose code it went
    /// through once.
    pub fn walked_iterations(&self) -> &[Iteration] {
        match &self.runs {
            Runs::EachIteration(iterations) => iterations,
            _ => &[],
        }
    }

    /// Where the walk of the loop's code stopped before it got to `back`,
    /// if it did (see [`Runs::Uncounted`]).
    pub fn stopped(&self) -> Option<End> {
        match self.runs {
            Runs::Uncounted { stopped } => stopped,
            _ => None,
        }
    }
}

/// The head of the last loop in `events` whose branch back is at `back`,
/// as where a walk that ends at [`End::Loops`] loops for good.
pub fn loop_head(events: &[Event], back: u64) -> Option<u64> {
    events.iter().rev().find_map(|event| match event {
        Event::Loop(l) if l.back == back => Some(l.head),
        _ => None,
    })
}

/// One iteration of a loop that the walk went through each time it runs
/// (see [`Runs::EachIteration`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iteration {
    /// The events that the walk found in it.
    pub events: Range<usize>,
    /// Where this iteration reads a row of a table from, as a loop over
    /// `__data_section_table` does: the lowest address in the file,
    /// outside the stack, that the code of the loop's function reads a
    /// word from in it, other than the words that the iterations so far
    /// copy. A word that a register brings from the iteration before
    /// counts as read from where it was loaded.
    ///
    /// `None` for every iteration unless the walk can tell where each row
    /// starts: in each, the lowest address that its loads point at is the
    /// row's start. A load points at the register that it adds an offset
    /// to, when that is lower than its word, and else at its word; a word
    /// brought from the iteration before points nowhere. The rows must
    /// also be as far apart each iteration, and each iteration read the
    /// same words of its row, but the last, which may read fewer.
    pub row: Option<u32>,
}

/// A store in a loop's code, over all the iterations of the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopStore {
    /// The store instruction.
    pub site: u64,
    /// Where it stores in the first iteration.
    pub address: u32,
    /// How far that moves each iteration.
    pub step: i32,
    /// How many bytes it stores each iteration.
    pub size: u32,
    /// How many times it stores: one fewer than the loop runs when it
    /// doesn't store in the last iteration, as when the loop leaves first.
    pub times: u32,
    /// The word it stores, when it is the same each iteration.
    pub value: Option<u32>,
    /// Where it loads the first word it stores from in the first
    /// iteration, and how far that moves each iteration, when the walk can
    /// tell.
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

/// A program's code as the walk reads it: Thumb code, the only kind that
/// has a walk, as [`InstructionSet::startup_code`] hands it out.
///
/// [`InstructionSet::startup_code`]: super::InstructionSet::startup_code
#[derive(Clone, Copy)]
pub struct Code<'p> {
    program: &'p dyn Program,
    /// The order of the bytes in the words that the code loads.
    endianness: Endianness,
}

impl<'p> Code<'p> {
    /// The Thumb code in `program`, whose words are in `endianness`.
    pub(super) fn thumb(program: &'p dyn Program, endianness: Endianness) -> Code<'p> {
        Code {
            program,
            endianness,
        }
    }

    /// Walks the code from `start`, as [`Start`] says.
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
    /// - The condition flags that compares, `adds` and `subs` set, so that
    ///   it takes a branch, and runs an instruction in an `it` block, as the
    ///   processor would. Where it can't tell, a branch is taken later, if
    ///   the way on doesn't go on (see [`Event::Backtrack`]); an instruction
    ///   in an `it` block leaves what it writes unknown.
    /// - Calls, up to [`Start::follow`] deep, never into `main`. A called
    ///   function starts with the caller's registers and memory. Where it
    ///   returns, the caller goes on with what the function left in the
    ///   registers, in memory and in the flags, as a result in r0 and r1 or
    ///   what code built with GCC's `-fipa-ra` counts on staying in r2 and
    ///   r3, but with its own stack pointer and lr unknown. If the function
    ///   got to the return past a branch that the walk couldn't tell, all it
    ///   changed is unknown. A call that the walk doesn't follow, or that
    ///   doesn't get to a return, leaves unknown the flags and those of r0
    ///   to r3, ip and lr that the function writes (all of them if it calls
    ///   on, or the walk can't tell where its code is), and, for one it
    ///   followed, the words it changed in memory. It doesn't follow a call
    ///   that it can't tell runs: one in an `it` block, or a branch to
    ///   another function (see [`Call::conditional`]).
    /// - Code that runs on past the end of a function: after a call, the
    ///   call can't return (see [`End::RunsOff`]); otherwise, the code goes
    ///   on into the next function.
    ///
    /// It goes through a loop's code, and then counts how many times the
    /// loop runs as the processor would, when it can: it goes through the
    /// code again for the first three iterations, and then a compare in it
    /// tells when a number that moves by the same amount each iteration gets
    /// to where the loop leaves, which it checks by going through the
    /// iteration before and that one, from where each number would be; or
    /// when none does, it goes on iteration by iteration, up to 16 (see
    /// [`Runs::Counted`]). It goes on from where the loop leaves, with what
    /// the loop leaves in the registers and memory. To count, it doesn't
    /// follow calls or count loops inside, so it can't tell what the stores
    /// of such a loop do. When such a loop runs at most 16 times, as one
    /// over a table of sections that copies each with a function or a loop,
    /// it goes through its code each iteration, with its calls and the loops
    /// inside (see [`Runs::EachIteration`]), but not when it calls through a
    /// register another function each iteration, as one over a table of
    /// functions does, such as the constructors that `__libc_init_array`
    /// calls, or one it can't tell. A loop it can't count, as one whose
    /// bound it can't tell, leaves what its code writes unknown
    /// ([`Runs::Uncounted`]).
    ///
    /// Each of its limits is one constant:
    ///
    /// - It stops after 20,000 instructions in all ([`BUDGET`]) and 600 in
    ///   the reset handler's own code ([`RESET_LIMIT`]).
    /// - It goes through 300 in each function it calls ([`CALL_LIMIT`]), and
    ///   60 in a loop's code each iteration ([`LOOP_LIMIT`]), 8 loops deep
    ///   ([`NESTING`]), and then goes on after the call or the loop (see
    ///   [`Event::Stop`] and [`Runs::Uncounted`]). It follows calls
    ///   [`DEPTH`] deep.
    /// - To count loops, it goes through 50,000 more instructions in all
    ///   ([`COUNT_BUDGET`]). It counts a loop that runs at most 2^24 times
    ///   ([`MOST_ITERATIONS`]), tries at most 12 of the times that its
    ///   compares say it may leave ([`CANDIDATES`]), and goes through its
    ///   code at most 16 times when they say none
    ///   ([`MOST_WALKED_ITERATIONS`]).
    /// - It goes through the code of a loop each iteration when the loop
    ///   runs at most 16 times ([`MOST_WALKED_ITERATIONS`]), and its
    ///   iterations take at most 5,000 instructions ([`WALK_EACH_BUDGET`])
    ///   and 10,000 to count the loops inside them
    ///   ([`WALK_EACH_COUNT_BUDGET`]), in place of the two budgets above;
    ///   all such loops take at most 10,000 and 20,000 ([`WALK_EACH_TOTAL`]
    ///   and [`WALK_EACH_COUNT_TOTAL`]). Past these, it goes through the
    ///   loop's code once, as through others.
    ///
    /// docs/design.md tells the walk in prose where it describes `elfq FILE
    /// startup`, with the loops it counts and the tables of sections it goes
    /// through, and its "Startup, end to end" says what runs before and
    /// after the walk.
    ///
    /// [`BUDGET`]: super::thumb::startup::BUDGET
    /// [`RESET_LIMIT`]: super::thumb::startup::RESET_LIMIT
    /// [`CALL_LIMIT`]: super::thumb::startup::CALL_LIMIT
    /// [`LOOP_LIMIT`]: super::thumb::startup::LOOP_LIMIT
    /// [`NESTING`]: super::thumb::startup::NESTING
    /// [`COUNT_BUDGET`]: super::thumb::startup::COUNT_BUDGET
    /// [`MOST_ITERATIONS`]: super::thumb::startup::MOST_ITERATIONS
    /// [`CANDIDATES`]: super::thumb::startup::CANDIDATES
    /// [`MOST_WALKED_ITERATIONS`]: super::thumb::startup::MOST_WALKED_ITERATIONS
    /// [`WALK_EACH_BUDGET`]: super::thumb::startup::WALK_EACH_BUDGET
    /// [`WALK_EACH_COUNT_BUDGET`]: super::thumb::startup::WALK_EACH_COUNT_BUDGET
    /// [`WALK_EACH_TOTAL`]: super::thumb::startup::WALK_EACH_TOTAL
    /// [`WALK_EACH_COUNT_TOTAL`]: super::thumb::startup::WALK_EACH_COUNT_TOTAL
    pub fn walk(&self, start: &Start) -> Walk {
        thumb::startup::walk(self.endianness, self.program, start)
    }

    /// Whether the code after the call at `first` and before the call at
    /// `second` only copies registers, or loads words from a literal pool,
    /// and leaves in r0 what `first` returns there, so that `second` gets
    /// it, as in `exit(main())`: `bl main; mov r3, r0; mov r0, r3; bl
    /// exit`. False when the calls are more than 16 bytes apart.
    pub fn passes_result(&self, first: u64, second: u64) -> bool {
        thumb::startup::passes_result(self.program, first, second)
    }

    /// Whether a branch in the code from `start` up to the call at `call`
    /// goes to the instruction after the call: then the call doesn't
    /// return, and the compiler put other code after it, such as a loop's.
    pub fn branches_past(&self, start: u64, call: u64) -> bool {
        thumb::startup::branches_past(self.program, start, call)
    }
}
