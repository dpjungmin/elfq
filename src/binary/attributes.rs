//! The build attributes of 32-bit Arm files, in `.ARM.attributes`, which
//! say which version of the Arm architecture the code is built for.
//!
//! The section holds a list of attributes for each vendor, and the `aeabi`
//! vendor's are those that Arm's ABI defines ("Addenda to, and Errata in,
//! the ABI for the Arm Architecture"). Each attribute is a tag and a value,
//! but the section doesn't say whether a value is a number or a string:
//! the reader has to know each tag's type, as [`read_value`] does.

use object::elf::{FileHeader32, Tag_File};
use object::read::elf::{AttributeReader, AttributesSection};
use object::{Endianness, Object, ObjectSection};

/// Tag_CPU_arch, the version of the architecture.
const CPU_ARCH: u64 = 6;
/// Tag_CPU_arch_profile: `A` for applications, `R` for real time, `M` for
/// microcontrollers, as on Armv7, which has all three.
const CPU_ARCH_PROFILE: u64 = 7;

/// The version of the Arm architecture that a file's code is built for, as
/// its build attributes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmArchitecture {
    /// Tag_CPU_arch: 10 for Armv7, 13 for Armv7E-M, 17 for Armv8-M
    /// Mainline, and so on (see [`ArmArchitecture::name`]).
    pub version: u64,
    /// Tag_CPU_arch_profile, such as `b'M'`, when the file gives one.
    pub profile: Option<u8>,
}

impl ArmArchitecture {
    /// Its name, as Arm writes it: `Armv7E-M`.
    pub fn name(&self) -> String {
        let name = match self.version {
            0 => "an Arm architecture before Armv4",
            1 => "Armv4",
            2 => "Armv4T",
            3 => "Armv5T",
            4 => "Armv5TE",
            5 => "Armv5TEJ",
            6 => "Armv6",
            7 => "Armv6KZ",
            8 => "Armv6T2",
            9 => "Armv6K",
            10 => match self.profile {
                Some(b'A') => "Armv7-A",
                Some(b'R') => "Armv7-R",
                Some(b'M') => "Armv7-M",
                _ => "Armv7",
            },
            11 => "Armv6-M",
            12 => "Armv6S-M",
            13 => "Armv7E-M",
            14 => "Armv8-A",
            15 => "Armv8-R",
            16 => "Armv8-M Baseline",
            17 => "Armv8-M Mainline",
            18 => "Armv8.1-A",
            19 => "Armv8.2-A",
            20 => "Armv8.3-A",
            21 => "Armv8.1-M Mainline",
            22 => "Armv9-A",
            version => return format!("Arm architecture {version}"),
        };
        name.to_string()
    }

    /// Whether it is the architecture of a Cortex-M core up to Armv7E-M:
    /// Armv6-M (Cortex-M0 and M0+, M1), Armv6S-M, Armv7-M (Cortex-M3) or
    /// Armv7E-M (Cortex-M4 and M7). Armv8-M cores, such as the Cortex-M33,
    /// add instructions that take the place of others.
    pub fn is_cortex_m_up_to_v7(&self) -> bool {
        matches!(self.version, 11..=13) || (self.version == 10 && self.profile == Some(b'M'))
    }
}

/// The architecture that the build attributes of `file` name, or `None`
/// when it has no `.ARM.attributes`, or they don't name one.
pub(super) fn arm_architecture(file: &object::File) -> Option<ArmArchitecture> {
    let data = file.section_by_name(".ARM.attributes")?.data().ok()?;
    let endian = if file.is_little_endian() {
        Endianness::Little
    } else {
        Endianness::Big
    };
    read_architecture(endian, data)
}

/// The architecture that the attributes section `data` names.
fn read_architecture(endian: Endianness, data: &[u8]) -> Option<ArmArchitecture> {
    let section = AttributesSection::<FileHeader32<Endianness>>::new(endian, data).ok()?;
    let mut version = None;
    let mut profile = None;
    for subsection in section.subsections().ok()? {
        let subsection = subsection.ok()?;
        if subsection.vendor() != b"aeabi" {
            continue;
        }
        for subsubsection in subsection.subsubsections() {
            let subsubsection = subsubsection.ok()?;
            // The attributes of the whole file, not of some sections or
            // symbols in it.
            if subsubsection.tag() != Tag_File {
                continue;
            }
            let mut attributes = subsubsection.attributes();
            while let Some(tag) = attributes.read_tag().ok()? {
                let value = read_value(&mut attributes, tag)?;
                match tag {
                    CPU_ARCH => version = value,
                    CPU_ARCH_PROFILE => profile = value.and_then(|v| u8::try_from(v).ok()),
                    _ => {}
                }
            }
        }
    }
    Some(ArmArchitecture {
        version: version?,
        // 0 means none.
        profile: profile.filter(|&profile| profile != 0),
    })
}

/// Reads the value of the attribute `tag`, which is a number for most tags
/// but a string for some: Tag_CPU_raw_name (4), Tag_CPU_name (5),
/// Tag_conformance (67) and every odd tag above 32, which the ABI keeps for
/// strings. Tag_compatibility (32) has a number and then a string. Returns
/// the number, or `Some(None)` for a string; `None` when the value is cut
/// short.
fn read_value(attributes: &mut AttributeReader, tag: u64) -> Option<Option<u64>> {
    match tag {
        4 | 5 | 67 => {}
        32 => {
            let number = attributes.read_integer().ok()?;
            attributes.read_string().ok()?;
            return Some(Some(number));
        }
        _ if tag > 32 && tag % 2 == 1 => {}
        _ => return Some(Some(attributes.read_integer().ok()?)),
    }
    attributes.read_string().ok()?;
    Some(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attributes of a file, as a `.ARM.attributes` section holds
    /// them: version `A`, then one subsection of the `aeabi` vendor with a
    /// sub-subsection of the whole file's attributes.
    fn section(attributes: &[u8]) -> Vec<u8> {
        let file = [
            &[1][..],
            &(attributes.len() as u32 + 5).to_le_bytes(),
            attributes,
        ]
        .concat();
        let vendor = [
            &(file.len() as u32 + 10).to_le_bytes()[..],
            b"aeabi\0",
            &file,
        ]
        .concat();
        [&b"A"[..], &vendor].concat()
    }

    fn read(attributes: &[u8]) -> Option<ArmArchitecture> {
        read_architecture(Endianness::Little, &section(attributes))
    }

    #[test]
    fn strings_and_numbers_are_told_apart() {
        // As GCC writes them: Tag_CPU_name "7E-M", Tag_CPU_arch 13,
        // Tag_CPU_arch_profile 'M', Tag_compatibility 1 "gnu",
        // Tag_also_compatible_with (65) "\x06\x11" and Tag_conformance
        // "2.09".
        let attributes = [
            &[5][..],
            b"7E-M\0",
            &[6, 13, 7, b'M', 32, 1],
            b"gnu\0",
            &[65, 6, 17, 0, 67],
            b"2.09\0",
        ]
        .concat();
        assert_eq!(
            read(&attributes),
            Some(ArmArchitecture {
                version: 13,
                profile: Some(b'M')
            })
        );
        // A string tag's value, or the string after Tag_compatibility's
        // number, taken for a number would leave 6, 17 to be read as
        // Tag_CPU_arch 17, and then a tag with no value.
        for tag in [4, 5, 65, 67, 69] {
            let attributes = [6, 13, tag, b'x', 6, 17, 0];
            assert_eq!(read(&attributes).map(|a| a.version), Some(13), "tag {tag}");
        }
        assert_eq!(read(&[6, 13, 32, 1, 6, 17, 0]).map(|a| a.version), Some(13));
        // A string cut short.
        assert_eq!(read(&[5, b'7']), None);
    }

    #[test]
    fn names_and_the_cores_that_get_text() {
        let architecture = |version, profile| ArmArchitecture { version, profile };
        assert_eq!(architecture(13, Some(b'M')).name(), "Armv7E-M");
        assert_eq!(architecture(10, Some(b'M')).name(), "Armv7-M");
        assert_eq!(architecture(17, Some(b'M')).name(), "Armv8-M Mainline");
        assert_eq!(architecture(40, None).name(), "Arm architecture 40");
        let cortex_m = [(11, None), (12, None), (13, Some(b'M')), (10, Some(b'M'))];
        for (version, profile) in cortex_m {
            assert!(architecture(version, profile).is_cortex_m_up_to_v7());
        }
        let others = [
            (10, Some(b'A')),
            (10, None),
            (16, Some(b'M')),
            (17, Some(b'M')),
        ];
        for (version, profile) in others {
            assert!(!architecture(version, profile).is_cortex_m_up_to_v7());
        }
    }

    /// Every sample names its core as `readelf -A` does: Armv6S-M for the
    /// Cortex-M0 builds, Armv7E-M for the M4 and M7 builds, stripped or
    /// not, and none for x86-64.
    #[test]
    fn every_sample_names_its_core() {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin");
        let mut arm = 0;
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let binary = crate::binary::Binary::load(&path).unwrap();
            let expected = if binary.architecture != object::Architecture::Arm {
                None
            } else if name.contains("-m0") {
                Some((12, "Armv6S-M"))
            } else {
                Some((13, "Armv7E-M"))
            };
            let found = binary.arm_architecture;
            assert_eq!(found.map(|a| a.version), expected.map(|e| e.0), "{name}");
            if let Some(found) = found {
                arm += 1;
                assert_eq!(found.name(), expected.unwrap().1, "{name}");
                assert_eq!(found.profile, Some(b'M'), "{name}");
                assert!(found.is_cortex_m_up_to_v7(), "{name}");
            }
        }
        assert!(arm >= 20, "{arm}");
    }
}
