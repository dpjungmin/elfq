//! x86-64 code, decoded with the `iced-x86` crate.

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind};

use super::{CodeReference, CodeReferenceKind};

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
    fn other_instructions_are_ignored() {
        // push rbp; mov rbp, rsp; ret
        assert_eq!(kinds(&[0x55, 0x48, 0x89, 0xe5, 0xc3]), []);
    }
}
