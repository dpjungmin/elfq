//! x86-64 code, decoded with the `iced-x86` crate.

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind};

use super::{CodeReference, CodeReferenceKind, Operands};

/// The references in the x86-64 code `code`, which starts at `address`, in
/// the order of their instructions.
pub(super) fn scan(code: &[u8], address: u64) -> Vec<CodeReference> {
    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    let mut references = Vec::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        if let Some(kind) = reference(&instruction) {
            references.push(CodeReference {
                site: instruction.ip(),
                kind,
            });
        }
    }
    references
}

/// The bits of the x86-64 code `code`, which starts at `address`, that say
/// where something is (see `arch::operands`): the distance of a call or jump
/// to its target, and of an operand relative to the instruction pointer,
/// each from the end of its instruction, when it leads outside the code,
/// and where each leads.
pub(super) fn operands(code: &[u8], address: u64) -> Operands {
    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    let mut relative = vec![0; code.len()];
    let mut targets = Vec::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        // Where the distance leads, when it leads outside the code.
        let (target, branch) = if let Some(target) = branch_target(&instruction) {
            (target, true)
        } else if instruction.is_ip_rel_memory_operand() {
            (instruction.ip_rel_memory_address(), false)
        } else {
            continue;
        };
        if target.wrapping_sub(address) < code.len() as u64 {
            continue;
        }
        // Where the distance is in the instruction: a branch's is its
        // immediate.
        let constants = decoder.get_constant_offsets(&instruction);
        let (offset, size) = if branch {
            (constants.immediate_offset(), constants.immediate_size())
        } else {
            (
                constants.displacement_offset(),
                constants.displacement_size(),
            )
        };
        let site = (instruction.ip() - address) as usize;
        if let Some(bits) = relative.get_mut(site + offset..site + offset + size) {
            bits.fill(0xff);
        }
        targets.push((site, target));
    }
    Operands {
        relative,
        targets,
        split: Vec::new(),
    }
}

/// The address of the last instruction in the x86-64 code `code`, which
/// starts at `address`, when the processor can run on past the end of
/// `code` after it: unless it always jumps, returns or traps, as a
/// conditional jump or a call doesn't, or when `code` ends inside it. The
/// padding that compilers put between functions, `nop`s and `int3`s,
/// doesn't count.
pub(super) fn runs_on(code: &[u8], address: u64) -> Option<u64> {
    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    let mut last = None;
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        if !matches!(instruction.mnemonic(), Mnemonic::Nop | Mnemonic::Int3) {
            last = Some(instruction);
        }
    }
    let last = last?;
    let stops = !last.is_invalid()
        && matches!(
            last.flow_control(),
            FlowControl::UnconditionalBranch
                | FlowControl::IndirectBranch
                | FlowControl::Return
                | FlowControl::Exception
        );
    (!stops).then(|| last.ip())
}

fn reference(instruction: &Instruction) -> Option<CodeReferenceKind> {
    // Addresses in x86-64 code are relative to the instruction pointer:
    // `call 0x1700`, `lea rdi, [rip + 0x2f3e]`.
    let memory = instruction
        .is_ip_rel_memory_operand()
        .then(|| instruction.ip_rel_memory_address());
    let kind = match instruction.flow_control() {
        FlowControl::Call => CodeReferenceKind::Call(branch_target(instruction)?),
        FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch => {
            CodeReferenceKind::Jump(branch_target(instruction)?)
        }
        FlowControl::IndirectCall => match memory {
            Some(pointer) => CodeReferenceKind::CallThrough(pointer),
            None => CodeReferenceKind::IndirectCall,
        },
        FlowControl::IndirectBranch => match memory {
            Some(pointer) => CodeReferenceKind::JumpThrough(pointer),
            None => CodeReferenceKind::IndirectJump,
        },
        _ if instruction.mnemonic() == Mnemonic::Lea => CodeReferenceKind::Address(memory?),
        _ => CodeReferenceKind::Memory(memory?),
    };
    Some(kind)
}

/// The target of a direct call or jump.
fn branch_target(instruction: &Instruction) -> Option<u64> {
    matches!(
        instruction.op0_kind(),
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
    )
    .then(|| instruction.near_branch_target())
}

#[cfg(test)]
mod tests {
    use super::*;
    use CodeReferenceKind::*;

    fn kinds(code: &[u8]) -> Vec<CodeReferenceKind> {
        scan(code, 0x1000).into_iter().map(|r| r.kind).collect()
    }

    #[test]
    fn finds_calls_jumps_and_addresses() {
        assert_eq!(kinds(&[0xe8, 0xfb, 0x06, 0x00, 0x00]), [Call(0x1700)]); // call 0x1700
        assert_eq!(kinds(&[0xe9, 0xfb, 0x06, 0x00, 0x00]), [Jump(0x1700)]); // jmp 0x1700
        assert_eq!(kinds(&[0x74, 0x10]), [Jump(0x1012)]); // je 0x1012
        // lea rdi, [rip + 0x100]
        assert_eq!(
            kinds(&[0x48, 0x8d, 0x3d, 0x00, 0x01, 0x00, 0x00]),
            [Address(0x1107)]
        );
        // mov eax, [rip + 0x100]: reading a variable.
        assert_eq!(
            kinds(&[0x8b, 0x05, 0x00, 0x01, 0x00, 0x00]),
            [Memory(0x1106)]
        );
    }

    #[test]
    fn calls_through_pointers_name_the_pointer() {
        // call [rip + 0x100]: a pointer in the global offset table, say.
        assert_eq!(
            kinds(&[0xff, 0x15, 0x00, 0x01, 0x00, 0x00]),
            [CallThrough(0x1106)]
        );
        // jmp [rip + 0x100]
        assert_eq!(
            kinds(&[0xff, 0x25, 0x00, 0x01, 0x00, 0x00]),
            [JumpThrough(0x1106)]
        );
        // call rax; jmp rax; call [rax + 8]
        assert_eq!(
            kinds(&[0xff, 0xd0, 0xff, 0xe0, 0xff, 0x50, 0x08]),
            [IndirectCall, IndirectJump, IndirectCall]
        );
    }

    #[test]
    fn code_runs_on_unless_it_jumps_returns_or_traps() {
        // mov eax, 1; then nop and int3 padding.
        assert_eq!(
            runs_on(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0x90, 0xcc], 0x1000),
            Some(0x1000)
        );
        // call 0x1700 returns to run on.
        assert_eq!(
            runs_on(&[0xe8, 0xfb, 0x06, 0x00, 0x00], 0x1000),
            Some(0x1000)
        );
        // xor eax, eax; the first byte of mov eax, 1: the code ends inside it.
        assert_eq!(runs_on(&[0x31, 0xc0, 0xb8], 0x1000), Some(0x1002));
        // ret; jmp 0x1700; jmp rax; ud2; then padding.
        assert_eq!(runs_on(&[0xc3, 0x90], 0x1000), None);
        assert_eq!(runs_on(&[0xe9, 0xfb, 0x06, 0x00, 0x00], 0x1000), None);
        assert_eq!(runs_on(&[0xff, 0xe0, 0xcc], 0x1000), None);
        assert_eq!(runs_on(&[0x0f, 0x0b], 0x1000), None);
        // Padding alone.
        assert_eq!(runs_on(&[0x90, 0xcc], 0x1000), None);
    }

    fn relative_bits(code: &[u8]) -> Vec<u8> {
        operands(code, 0x1000).relative
    }

    #[test]
    fn a_call_holds_its_distance_in_its_rel32() {
        // call 0x1700
        let code = [0xe8, 0xfb, 0x06, 0x00, 0x00];
        assert_eq!(relative_bits(&code), [0, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(operands(&code, 0x1000).targets, [(0, 0x1700)]);
    }

    #[test]
    fn an_operand_relative_to_rip_holds_its_distance_in_its_disp32() {
        // lea rdi, [rip + 0x100]
        assert_eq!(
            relative_bits(&[0x48, 0x8d, 0x3d, 0x00, 0x01, 0x00, 0x00]),
            [0, 0, 0, 0xff, 0xff, 0xff, 0xff]
        );
        // mov dword ptr [rip + 0x100], 5: the immediate is a number.
        assert_eq!(
            relative_bits(&[0xc7, 0x05, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00]),
            [0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]
        );
    }

    #[test]
    fn a_jump_inside_the_code_is_left_out() {
        // jmp 0x1002; ret
        assert_eq!(relative_bits(&[0xeb, 0x00, 0xc3]), [0, 0, 0]);
    }

    #[test]
    fn other_instructions_are_ignored() {
        // push rbp; mov rbp, rsp; ret
        assert_eq!(kinds(&[0x55, 0x48, 0x89, 0xe5, 0xc3]), []);
    }
}
