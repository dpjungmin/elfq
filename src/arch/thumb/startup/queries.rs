use crate::arch::startup::Program;
use crate::arch::thumb::decode::{Flow, Operation};
use crate::arch::thumb::is_wide;

use super::decode_at;

/// See `arch::startup::passes_result`.
pub(in crate::arch) fn passes_result(program: &dyn Program, first: u64, second: u64) -> bool {
    if !(first < second && second - first <= 16) {
        return false;
    }
    let Some((first_half, _, _)) = decode_at(program, first) else {
        return false;
    };
    let mut address = first + if is_wide(first_half) { 4 } else { 2 };
    // The registers that hold what the first call returns in r0.
    let mut result: u16 = 1;
    while address < second {
        let Some((half, _, effect)) = decode_at(program, address) else {
            return false;
        };
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
        address += if is_wide(half) { 4 } else { 2 };
    }
    address == second && result & 1 != 0
}

/// See `arch::startup::branches_past`.
pub(in crate::arch) fn branches_past(program: &dyn Program, start: u64, call: u64) -> bool {
    let Some((half, _, _)) = decode_at(program, call) else {
        return false;
    };
    let after = call + if is_wide(half) { 4 } else { 2 };
    let mut address = start;
    while address < call {
        let Some((half, _, effect)) = decode_at(program, address) else {
            address += 2;
            continue;
        };
        if let Flow::Branch { target, .. } = effect.flow
            && target == after
        {
            return true;
        }
        address += if is_wide(half) { 4 } else { 2 };
    }
    false
}
