//! Tests of the Thumb text that function pages show, on the Cortex-M
//! samples: against objdump's text for the same code, which
//! testdata/thumb-text.sh writes to testdata/tests/thumb, so that the
//! tests don't need objdump; and in each stripped sample, which has no `$d`
//! symbols, against its twin with symbols, which says where data is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::item::listing;
use crate::arch::Line;
use crate::binary::{Binary, ItemKind};

/// The lines of each function of `binary`, as its page prints them, and
/// where the function ends.
fn functions(binary: &Binary) -> Vec<(Vec<Line>, u64)> {
    let mut functions = Vec::new();
    for item in &binary.items {
        if item.kind != ItemKind::Function {
            continue;
        }
        if let Some(bytes) = binary.bytes(item.address, item.size)
            && let Some(lines) = listing(binary, item, bytes)
        {
            functions.push((lines, item.address + item.size));
        }
    }
    functions
}

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// The Cortex-M samples' files in testdata/bin that end with `suffix`,
/// such as `-nodebug.elf`, by the name before it.
fn cortex_m_samples(suffix: &str) -> Vec<(String, Binary)> {
    let mut samples = Vec::new();
    for entry in std::fs::read_dir(testdata().join("bin")).unwrap() {
        let path = entry.unwrap().path();
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(name) = file.strip_suffix(suffix) else {
            continue;
        };
        let binary = Binary::load(&path).unwrap();
        if binary.thumb {
            samples.push((name.to_string(), binary));
        }
    }
    samples.sort_by(|a, b| a.0.cmp(&b.0));
    samples
}

#[test]
fn thumb_text_says_what_objdump_says() {
    let mut lines = 0;
    let mut differences = Vec::new();
    for (name, binary) in cortex_m_samples("-nodebug.elf") {
        let path = testdata().join(format!("tests/thumb/{name}.txt"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("no {}: run testdata/thumb-text.sh", path.display()));
        // objdump's lines, by address.
        let objdump: HashMap<u64, (&str, &str)> = text
            .lines()
            .enumerate()
            .map(|(number, line)| {
                let mut fields = line.splitn(3, '\t');
                let address = fields.next().and_then(|a| u64::from_str_radix(a, 16).ok());
                match (address, fields.next()) {
                    (Some(address), Some(mnemonic)) => {
                        (address, (mnemonic, fields.next().unwrap_or("")))
                    }
                    _ => panic!(
                        "{}:{}: {line:?} isn't an address, a mnemonic and operands, split by tabs",
                        path.display(),
                        number + 1
                    ),
                }
            })
            .collect();
        for line in functions(&binary).iter().flat_map(|(lines, _)| lines) {
            lines += 1;
            let elfq = format!("{} {}", line.mnemonic, line.operands);
            match objdump.get(&line.address) {
                Some(&(mnemonic, operands))
                    if canonical(mnemonic, operands)
                        == canonical(&line.mnemonic, &line.operands) => {}
                Some(&(mnemonic, operands)) => differences.push(format!(
                    "{name} {:#x}: elfq {elfq:?}, objdump \"{mnemonic} {operands}\"",
                    line.address
                )),
                None => differences.push(format!(
                    "{name} {:#x}: elfq {elfq:?}, and no line of objdump's",
                    line.address
                )),
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} of {lines} lines differ:\n{}",
        differences.len(),
        differences[..differences.len().min(30)].join("\n")
    );
    assert!(lines > 10_000, "{lines}");
}

/// In a stripped sample, elfq prints no instruction where the same file
/// with symbols has a `$d` symbol, which marks data, not even one that
/// thumb-asm can't read (`.inst`). The sweep finds literal pools, jump
/// tables and tables of addresses on its own.
#[test]
fn stripped_samples_print_data_where_their_twins_mark_it() {
    let mut stretches = 0;
    let mut wrong = Vec::new();
    for (name, stripped) in cortex_m_samples("-stripped.elf") {
        let twin = Binary::load(&testdata().join(format!("bin/{name}-nodebug.elf"))).unwrap();
        let data: Vec<(u64, u64)> = twin.data_in_code(0, u64::MAX).collect();
        stretches += data.len();
        for (lines, end) in functions(&stripped) {
            let ends = lines.iter().skip(1).map(|line| line.address).chain([end]);
            for (line, line_end) in lines.iter().zip(ends) {
                let marked = data
                    .iter()
                    .any(|&(start, data_end)| start < line_end && line.address < data_end);
                let instruction =
                    !line.mnemonic.starts_with('.') || line.mnemonic.starts_with(".inst");
                if marked && instruction {
                    wrong.push(format!(
                        "{name} {:#x} {} {}",
                        line.address, line.mnemonic, line.operands
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(stretches > 100, "{stretches}");
}

/// The spelling of `mnemonic` and `operands` that both objdump's and elfq's
/// text of an instruction come to: objdump writes `bcs.n`, `#100`,
/// `ldmia.w sp!, {r4, r5, r6, r7}` and `ip` where elfq writes `bhs`,
/// `#0x64`, `pop.w {r4-r7}` and `r12`. Some of these rules apply to both
/// texts at once, as `bics` becoming `bihs`; that keeps them equal.
fn canonical(mnemonic: &str, operands: &str) -> (String, String) {
    let mnemonic = mnemonic.to_lowercase();
    let mnemonic = mnemonic
        .strip_suffix(".w")
        .or_else(|| mnemonic.strip_suffix(".n"))
        .unwrap_or(&mnemonic);
    // The name and the data types after it: `vmovgt` and `f32`.
    let (name, types) = match mnemonic.split_once('.') {
        Some((name, types)) => (name, format!(".{types}")),
        None => (mnemonic, String::new()),
    };
    // Conditions: `cs` is also `hs`, and `cc` is also `lo`.
    let name = match name.len().checked_sub(2).map(|at| name.split_at(at)) {
        Some((base, "cs")) => format!("{base}hs"),
        Some((base, "cc")) => format!("{base}lo"),
        _ => name.to_string(),
    };
    // Multiple loads and stores: `ldmia` is `ldm`, and `stmfd` is `stmdb`.
    let name = [
        ("ldmia", "ldm"),
        ("ldmfd", "ldm"),
        ("stmia", "stm"),
        ("stmea", "stm"),
    ]
    .iter()
    .chain(&[("stmfd", "stmdb")])
    .find_map(|(long, short)| Some(format!("{short}{}", name.strip_prefix(long)?)))
    .unwrap_or(name);
    let mut operands = canonical_operands(&operands.to_lowercase());
    if name.starts_with("it") {
        operands = match operands.as_str() {
            "cs" => "hs".to_string(),
            "cc" => "lo".to_string(),
            _ => operands,
        };
    }
    let (name, operands) = alias(name, operands);
    (name + &types, operands)
}

/// Operands with registers by number (`r12` for `ip`), register lists
/// written out (`{r4, r5}` for `{r4-r5}`), `[r0]` for `[r0, #0]`, and every
/// immediate in decimal: `#100` for `#0x64`, `#0.5` for `#0.500`.
fn canonical_operands(operands: &str) -> String {
    let operands = operands.split_whitespace().collect::<Vec<_>>().join(" ");
    let operands = operands.replace(", #0]", "]");
    let mut out = String::new();
    let mut rest = operands.as_str();
    while let Some(c) = rest.chars().next() {
        if c == '{' {
            let end = rest.find('}').unwrap_or(rest.len() - 1);
            out += &register_list(&rest[1..end]);
            rest = &rest[end + 1..];
        } else if c == '#' {
            let end = rest[1..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-+.".contains(c)))
                .map_or(rest.len(), |end| end + 1);
            out += &format!("#{}", number(&rest[1..end]));
            rest = &rest[end..];
        } else if c.is_ascii_alphabetic() {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            out += register(&rest[..end]);
            rest = &rest[end..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The register `word` names, by number, or else `word`.
fn register(word: &str) -> &str {
    match word {
        "sb" => "r9",
        "sl" => "r10",
        "fp" => "r11",
        "ip" => "r12",
        // An M-profile core has only the APSR, which objdump calls CPSR, as
        // on other cores: `msr cpsr_f` is `msr apsr_nzcvq`.
        "cpsr" => "apsr",
        "cpsr_f" => "apsr_nzcvq",
        _ => word,
    }
}

/// The registers of a list, between the braces, written out.
fn register_list(list: &str) -> String {
    let mut registers = Vec::new();
    for part in list.split(',').map(str::trim) {
        let range = part.split_once('-').and_then(|(first, last)| {
            let first: u32 = first.strip_prefix('r')?.parse().ok()?;
            let last: u32 = last.strip_prefix('r')?.parse().ok()?;
            Some(first..=last)
        });
        match range {
            Some(range) => registers.extend(range.map(|r| format!("r{r}"))),
            None => registers.push(register(part).to_string()),
        }
    }
    format!("{{{}}}", registers.join(", "))
}

/// An immediate in decimal, or `text` itself if it is no number.
fn number(text: &str) -> String {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(digits) => ("-", digits),
        None => ("", text),
    };
    if let Some(hex) = digits.strip_prefix("0x") {
        if let Ok(value) = u64::from_str_radix(hex, 16) {
            return format!("{sign}{value}");
        }
    } else if let Ok(value) = digits.parse::<u64>() {
        return format!("{sign}{value}");
    } else if let Ok(value) = text.parse::<f64>() {
        return format!("{value:?}");
    }
    text.to_string()
}

/// The condition after `base` in `name`, if `name` is `base` with a
/// condition or none: `Some("gt")` for `movgt` and `mov`.
fn condition<'a>(name: &'a str, base: &str) -> Option<&'a str> {
    let condition = name.strip_prefix(base)?;
    const CONDITIONS: [&str; 15] = [
        "", "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le",
    ];
    CONDITIONS.contains(&condition).then_some(condition)
}

/// The instruction that objdump writes as `name` and `operands`, in the
/// words that elfq uses, where the two differ: `pop` for `ldm sp!`, `lsls`
/// for `movs` with a shift, `rsbs ..., #0` for `negs`, and so on.
fn alias(name: String, operands: String) -> (String, String) {
    let registers: Vec<&str> = operands.split(", ").collect();
    let is_register = |text: &str| {
        matches!(text, "sp" | "lr" | "pc")
            || text
                .strip_prefix('r')
                .is_some_and(|n| n.parse::<u8>().is_ok())
    };
    let all_registers = registers.iter().all(|r| is_register(r));
    if let Some(list) = operands.strip_prefix("sp!, ") {
        if let Some(c) = condition(&name, "ldm") {
            return (format!("pop{c}"), list.to_string());
        }
        if let Some(c) = condition(&name, "stmdb") {
            return (format!("push{c}"), list.to_string());
        }
    }
    if let [register, "[sp", "#-4]!"] = registers[..]
        && name == "str"
    {
        return ("push".to_string(), format!("{{{register}}}"));
    }
    if let [register, "[sp]", "#4"] = registers[..]
        && name == "ldr"
    {
        return ("pop".to_string(), format!("{{{register}}}"));
    }
    for (base, s) in [("mov", ""), ("movs", "s")] {
        let Some(c) = condition(&name, base) else {
            continue;
        };
        match registers[..] {
            [rd, rm, "rrx"] => {
                return (format!("rrx{s}{c}"), format!("{rd}, {rm}"));
            }
            [rd, rm, shift] => {
                if let Some((kind, amount)) = shift.split_once(' ')
                    && ["lsl", "lsr", "asr", "ror"].contains(&kind)
                {
                    return (format!("{kind}{s}{c}"), format!("{rd}, {rm}, {amount}"));
                }
            }
            ["r8", "r8"] if name == "mov" => return ("nop".to_string(), String::new()),
            _ => {}
        }
    }
    for (base, rsb) in [("neg", "rsb"), ("negs", "rsbs")] {
        if let Some(c) = condition(&name, base)
            && registers.len() == 2
            && all_registers
        {
            return (format!("{rsb}{c}"), format!("{operands}, #0"));
        }
    }
    // Three registers, the first twice, of an instruction that doesn't
    // care about their order: `muls r0, r1, r0` is `muls r0, r1`, and
    // `add r3, sp, r3` is `add r3, sp`.
    if let [rd, rn, rm] = registers[..]
        && all_registers
        && (condition(&name, "muls").is_some() || condition(&name, "add").is_some())
    {
        if rd == rm {
            return (name, format!("{rd}, {rn}"));
        }
        if rd == rn && condition(&name, "add").is_some() {
            return (name, format!("{rd}, {rm}"));
        }
    }
    (name, operands)
}

#[test]
fn spellings_that_mean_the_same_compare_equal() {
    let same = [
        (("bcs.n", "0x8000022"), ("bhs", "0x8000022")),
        (
            ("ldmia.w", "sp!, {r4, r5, r6, r7, r8, r9, sl, fp, pc}"),
            ("pop.w", "{r4-r11, pc}"),
        ),
        (("stmdb", "sp!, {r4, lr}"), ("push.w", "{r4, lr}")),
        (("sub", "sp, #100"), ("sub", "sp, #0x64")),
        (("ldr", "r0, [r3, #0]"), ("ldr", "r0, [r3]")),
        (("movs", "r3, r3, lsl #2"), ("lsls", "r3, r3, #2")),
        (("negs", "r0, r1"), ("rsbs", "r0, r1, #0")),
        (("add", "r3, sp"), ("add", "r3, sp, r3")),
        (
            ("vmov.f32", "s12, #0.7187500"),
            ("vmov.f32", "s12, #0.71875"),
        ),
        (("it", "cc"), ("it", "lo")),
        (("mrs", "r4, CPSR"), ("mrs", "r4, apsr")),
        (("str.w", "ip, [sp, #-4]!"), ("push", "{r12}")),
    ];
    for ((mnemonic, operands), (other, its_operands)) in same {
        assert_eq!(
            canonical(mnemonic, operands),
            canonical(other, its_operands),
            "{mnemonic} {operands}"
        );
    }
    let different = [
        (("add", "r3, sp, r2"), ("add", "r3, sp")),
        (("sub", "sp, #100"), ("sub", "sp, #0x60")),
        (("bcs.n", "0x8000022"), ("bls", "0x8000022")),
    ];
    for ((mnemonic, operands), (other, its_operands)) in different {
        assert_ne!(
            canonical(mnemonic, operands),
            canonical(other, its_operands),
            "{mnemonic} {operands}"
        );
    }
}
