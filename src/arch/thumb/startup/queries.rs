//! Two questions that `commands/startup` asks about the code around a
//! call, to tell which call is `main` in a stripped file:
//! [`passes_result`] and [`branches_past`].

use crate::arch::startup::Program;
use crate::arch::thumb::decode::{Flow, Operation};

use super::decode_at;

/// See `arch::startup::passes_result`.
pub(in crate::arch) fn passes_result(program: &dyn Program, first: u64, second: u64) -> bool {
    if !(first < second && second - first <= 16) {
        return false;
    }
    let Some(call) = decode_at(program, first) else {
        return false;
    };
    let mut address = call.next();
    // The registers that hold what the first call returns in r0.
    let mut result: u16 = 1;
    while address < second {
        let Some(instruction) = decode_at(program, address) else {
            return false;
        };
        let effect = instruction.effect;
        match effect.operation {
            Operation::Copy { register, source } if register < 16 && source < 16 => {
                match result & 1 << source {
                    0 => result &= !(1 << register),
                    _ => result |= 1 << register,
                }
            }
            Operation::Literal { register, .. } if register < 16 => result &= !(1 << register),
            _ => return false,
        }
        if effect.flow != Flow::Next {
            return false;
        }
        address = instruction.next();
    }
    address == second && result & 1 != 0
}

/// See `arch::startup::branches_past`.
pub(in crate::arch) fn branches_past(program: &dyn Program, start: u64, call: u64) -> bool {
    let Some(after) = decode_at(program, call).map(|call| call.next()) else {
        return false;
    };
    let mut address = start;
    while address < call {
        let Some(instruction) = decode_at(program, address) else {
            address += 2;
            continue;
        };
        if let Flow::Branch { target, .. } = instruction.effect.flow
            && target == after
        {
            return true;
        }
        address = instruction.next();
    }
    false
}
