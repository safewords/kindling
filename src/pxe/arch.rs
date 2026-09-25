//! DHCP option 93 — the Client System Architecture ID.
//!
//! This one number decides which binary a machine can execute. Hand a UEFI
//! machine `undionly.kpxe` and the firmware loads it, fails to run it and
//! falls through to the next boot device with no message anybody sees; hand a
//! BIOS machine `ipxe.efi` and the same happens the other way round. So the
//! registry is spelled out here in full rather than approximated with "is it 7
//! or is it 0", and an architecture this server has never heard of is carried
//! through as its number instead of being flattened to a default.
//!
//! The values are IANA's "Processor Architecture Types" registry (RFC 4578 and
//! its successors).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What kind of firmware is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    /// A PC BIOS, which loads a flat binary at 0x7C00.
    Bios,
    /// UEFI, which loads a PE/COFF application.
    Uefi,
    /// U-Boot, which mostly appears on ARM boards.
    UBoot,
    /// Open Firmware / ePAPR / OPAL, on POWER.
    OpenFirmware,
    /// Something this server does not have a name for.
    Unknown,
}

impl Firmware {
    pub fn as_str(&self) -> &'static str {
        match self {
            Firmware::Bios => "bios",
            Firmware::Uefi => "uefi",
            Firmware::UBoot => "uboot",
            Firmware::OpenFirmware => "open-firmware",
            Firmware::Unknown => "unknown",
        }
    }
}

/// The instruction set, as far as a boot loader cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    X86,
    X64,
    Ia64,
    Alpha,
    Arm32,
    Arm64,
    Powerpc,
    RiscV32,
    RiscV64,
    RiscV128,
    S390,
    Mips32,
    Mips64,
    Sunway32,
    Sunway64,
    LoongArch32,
    LoongArch64,
    Ebc,
    Unknown,
}

impl Family {
    pub fn as_str(&self) -> &'static str {
        match self {
            Family::X86 => "x86",
            Family::X64 => "x64",
            Family::Ia64 => "ia64",
            Family::Alpha => "alpha",
            Family::Arm32 => "arm32",
            Family::Arm64 => "arm64",
            Family::Powerpc => "powerpc",
            Family::RiscV32 => "riscv32",
            Family::RiscV64 => "riscv64",
            Family::RiscV128 => "riscv128",
            Family::S390 => "s390",
            Family::Mips32 => "mips32",
            Family::Mips64 => "mips64",
            Family::Sunway32 => "sunway32",
            Family::Sunway64 => "sunway64",
            Family::LoongArch32 => "loongarch32",
            Family::LoongArch64 => "loongarch64",
            Family::Ebc => "ebc",
            Family::Unknown => "unknown",
        }
    }
}

/// One row of the registry.
struct Entry {
    code: u16,
    label: &'static str,
    description: &'static str,
    family: Family,
    firmware: Firmware,
    /// Whether the firmware fetches its boot file over HTTP rather than TFTP.
    /// A machine in this column wants a URL in option 67 and a vendor class of
    /// `HTTPClient`; give it a bare filename and it will not boot.
    http: bool,
}

/// IANA's registry, in full.
///
/// Note that 7 and 9 are *both* x86-64 UEFI. 7 is "EFI Byte Code" on paper and
/// x86-64 in practice — nearly every PC UEFI sends it — and 9 is the honest
/// one. They share a label deliberately: a rule saying `arch = ["x64-uefi"]`
/// means the machine, not the number its firmware happens to send.
const REGISTRY: &[Entry] = &[
    Entry { code: 0, label: "bios", description: "Intel x86PC (BIOS)", family: Family::X86, firmware: Firmware::Bios, http: false },
    Entry { code: 1, label: "nec-pc98", description: "NEC/PC98", family: Family::X86, firmware: Firmware::Bios, http: false },
    Entry { code: 2, label: "ia64", description: "EFI Itanium", family: Family::Ia64, firmware: Firmware::Uefi, http: false },
    Entry { code: 3, label: "alpha", description: "DEC Alpha", family: Family::Alpha, firmware: Firmware::Unknown, http: false },
    Entry { code: 4, label: "arc-x86", description: "Arc x86", family: Family::X86, firmware: Firmware::Unknown, http: false },
    Entry { code: 5, label: "lean-client", description: "Intel Lean Client", family: Family::X86, firmware: Firmware::Unknown, http: false },
    Entry { code: 6, label: "x86-uefi", description: "EFI IA32", family: Family::X86, firmware: Firmware::Uefi, http: false },
    Entry { code: 7, label: "x64-uefi", description: "EFI BC (x86-64 in practice)", family: Family::X64, firmware: Firmware::Uefi, http: false },
    Entry { code: 8, label: "xscale-uefi", description: "EFI Xscale", family: Family::Arm32, firmware: Firmware::Uefi, http: false },
    Entry { code: 9, label: "x64-uefi", description: "EFI x86-64", family: Family::X64, firmware: Firmware::Uefi, http: false },
    Entry { code: 10, label: "arm32-uefi", description: "EFI ARM32", family: Family::Arm32, firmware: Firmware::Uefi, http: false },
    Entry { code: 11, label: "arm64-uefi", description: "EFI ARM64", family: Family::Arm64, firmware: Firmware::Uefi, http: false },
    Entry { code: 12, label: "powerpc-ofw", description: "PowerPC Open Firmware", family: Family::Powerpc, firmware: Firmware::OpenFirmware, http: false },
    Entry { code: 13, label: "powerpc-epapr", description: "PowerPC ePAPR", family: Family::Powerpc, firmware: Firmware::OpenFirmware, http: false },
    Entry { code: 14, label: "power-opal", description: "POWER OPAL v3", family: Family::Powerpc, firmware: Firmware::OpenFirmware, http: false },
    Entry { code: 15, label: "x86-uefi-http", description: "EFI IA32 over HTTP", family: Family::X86, firmware: Firmware::Uefi, http: true },
    Entry { code: 16, label: "x64-uefi-http", description: "EFI x86-64 over HTTP", family: Family::X64, firmware: Firmware::Uefi, http: true },
    Entry { code: 17, label: "ebc-http", description: "EFI Byte Code over HTTP", family: Family::Ebc, firmware: Firmware::Uefi, http: true },
    Entry { code: 18, label: "arm32-uefi-http", description: "EFI ARM32 over HTTP", family: Family::Arm32, firmware: Firmware::Uefi, http: true },
    Entry { code: 19, label: "arm64-uefi-http", description: "EFI ARM64 over HTTP", family: Family::Arm64, firmware: Firmware::Uefi, http: true },
    Entry { code: 20, label: "bios-http", description: "PC/AT BIOS over HTTP", family: Family::X86, firmware: Firmware::Bios, http: true },
    Entry { code: 21, label: "arm32-uboot", description: "ARM 32-bit U-Boot", family: Family::Arm32, firmware: Firmware::UBoot, http: false },
    Entry { code: 22, label: "arm64-uboot", description: "ARM 64-bit U-Boot", family: Family::Arm64, firmware: Firmware::UBoot, http: false },
    Entry { code: 23, label: "arm32-uboot-http", description: "ARM 32-bit U-Boot over HTTP", family: Family::Arm32, firmware: Firmware::UBoot, http: true },
    Entry { code: 24, label: "arm64-uboot-http", description: "ARM 64-bit U-Boot over HTTP", family: Family::Arm64, firmware: Firmware::UBoot, http: true },
    Entry { code: 25, label: "riscv32-uefi", description: "RISC-V 32-bit UEFI", family: Family::RiscV32, firmware: Firmware::Uefi, http: false },
    Entry { code: 26, label: "riscv32-uefi-http", description: "RISC-V 32-bit UEFI over HTTP", family: Family::RiscV32, firmware: Firmware::Uefi, http: true },
    Entry { code: 27, label: "riscv64-uefi", description: "RISC-V 64-bit UEFI", family: Family::RiscV64, firmware: Firmware::Uefi, http: false },
    Entry { code: 28, label: "riscv64-uefi-http", description: "RISC-V 64-bit UEFI over HTTP", family: Family::RiscV64, firmware: Firmware::Uefi, http: true },
    Entry { code: 29, label: "riscv128-uefi", description: "RISC-V 128-bit UEFI", family: Family::RiscV128, firmware: Firmware::Uefi, http: false },
    Entry { code: 30, label: "riscv128-uefi-http", description: "RISC-V 128-bit UEFI over HTTP", family: Family::RiscV128, firmware: Firmware::Uefi, http: true },
    Entry { code: 31, label: "s390-basic", description: "s390 Basic", family: Family::S390, firmware: Firmware::Unknown, http: false },
    Entry { code: 32, label: "s390-extended", description: "s390 Extended", family: Family::S390, firmware: Firmware::Unknown, http: false },
    Entry { code: 33, label: "mips32-uefi", description: "MIPS 32-bit UEFI", family: Family::Mips32, firmware: Firmware::Uefi, http: false },
    Entry { code: 34, label: "mips64-uefi", description: "MIPS 64-bit UEFI", family: Family::Mips64, firmware: Firmware::Uefi, http: false },
    Entry { code: 35, label: "sunway32-uefi", description: "Sunway 32-bit UEFI", family: Family::Sunway32, firmware: Firmware::Uefi, http: false },
    Entry { code: 36, label: "sunway64-uefi", description: "Sunway 64-bit UEFI", family: Family::Sunway64, firmware: Firmware::Uefi, http: false },
    Entry { code: 37, label: "loongarch32-uefi", description: "LoongArch32 UEFI", family: Family::LoongArch32, firmware: Firmware::Uefi, http: false },
    Entry { code: 38, label: "loongarch32-uefi-http", description: "LoongArch32 UEFI over HTTP", family: Family::LoongArch32, firmware: Firmware::Uefi, http: true },
    Entry { code: 39, label: "loongarch64-uefi", description: "LoongArch64 UEFI", family: Family::LoongArch64, firmware: Firmware::Uefi, http: false },
    Entry { code: 40, label: "loongarch64-uefi-http", description: "LoongArch64 UEFI over HTTP", family: Family::LoongArch64, firmware: Firmware::Uefi, http: true },
];

/// A client architecture, known or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientArch(u16);

impl ClientArch {
    /// x86 BIOS — what a machine that sends no option 93 at all is taken to
    /// be, because option 93 postdates PXE and only BIOS clients omit it.
    pub const BIOS: ClientArch = ClientArch(0);
    pub const X86_UEFI: ClientArch = ClientArch(6);
    pub const X64_UEFI: ClientArch = ClientArch(7);
    pub const ARM64_UEFI: ClientArch = ClientArch(11);
    pub const X64_UEFI_HTTP: ClientArch = ClientArch(16);

    pub const fn from_code(code: u16) -> Self {
        Self(code)
    }

    pub const fn code(&self) -> u16 {
        self.0
    }

    fn entry(&self) -> Option<&'static Entry> {
        REGISTRY.iter().find(|entry| entry.code == self.0)
    }

    /// The name a rule file uses. An unregistered architecture gets
    /// `arch-<code>`, which is still matchable — nothing is silently lost.
    pub fn label(&self) -> String {
        self.entry().map_or_else(|| format!("arch-{}", self.0), |entry| entry.label.to_string())
    }

    pub fn description(&self) -> String {
        self.entry().map_or_else(
            || format!("unregistered architecture {}", self.0),
            |entry| entry.description.to_string(),
        )
    }

    pub fn family(&self) -> Family {
        self.entry().map_or(Family::Unknown, |entry| entry.family)
    }

    pub fn firmware(&self) -> Firmware {
        self.entry().map_or(Firmware::Unknown, |entry| entry.firmware)
    }

    /// Whether this firmware fetches its boot file over HTTP.
    pub fn is_http_boot(&self) -> bool {
        self.entry().is_some_and(|entry| entry.http)
    }

    pub fn is_uefi(&self) -> bool {
        self.firmware() == Firmware::Uefi
    }

    pub fn is_known(&self) -> bool {
        self.entry().is_some()
    }

    /// Resolve the name a rule file — or a human at a terminal — wrote.
    ///
    /// A label, a family, a firmware or a bare number: all four are things
    /// people write, and refusing three of them would only mean a rule that
    /// silently never fires.
    pub fn matches_name(&self, name: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return false;
        }
        if self.label() == name {
            return true;
        }
        if let Ok(code) = name.parse::<u16>() {
            return self.0 == code;
        }
        match name.as_str() {
            "x86_64" | "amd64" => self.family() == Family::X64,
            "aarch64" => self.family() == Family::Arm64,
            "i386" | "ia32" => self.family() == Family::X86,
            "http" => self.is_http_boot(),
            "tftp" => !self.is_http_boot(),
            other => other == self.family().as_str() || other == self.firmware().as_str(),
        }
    }

    /// Every label in the registry, for `--help` output and error messages.
    pub fn known_labels() -> Vec<&'static str> {
        let mut labels: Vec<&'static str> = REGISTRY.iter().map(|entry| entry.label).collect();
        labels.dedup();
        labels
    }
}

impl fmt::Display for ClientArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl Serialize for ClientArch {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for ClientArch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        u16::deserialize(deserializer).map(ClientArch)
    }
}

/// Parse an architecture from something a human typed: a label or a number.
pub fn parse_arch(input: &str) -> Option<ClientArch> {
    let input = input.trim().to_ascii_lowercase();
    if let Ok(code) = input.parse::<u16>() {
        return Some(ClientArch::from_code(code));
    }
    REGISTRY.iter().find(|entry| entry.label == input).map(|entry| ClientArch(entry.code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_numbers_for_x86_64_uefi_are_one_architecture() {
        // 7 is what nearly every PC UEFI sends and 9 is what the registry
        // means. A rule written for one must fire for the other.
        assert_eq!(ClientArch::from_code(7).label(), "x64-uefi");
        assert_eq!(ClientArch::from_code(9).label(), "x64-uefi");
        assert!(ClientArch::from_code(9).matches_name("x64-uefi"));
    }

    #[test]
    fn bios_and_uefi_do_not_match_each_others_rules() {
        // The whole point of the option: the wrong binary for this answer is a
        // machine that silently falls through to its next boot device.
        assert!(!ClientArch::BIOS.matches_name("uefi"));
        assert!(!ClientArch::X64_UEFI.matches_name("bios"));
        assert!(ClientArch::BIOS.matches_name("bios"));
        assert!(ClientArch::X64_UEFI.matches_name("uefi"));
    }

    #[test]
    fn http_boot_is_distinguishable_from_tftp_boot() {
        // A machine in the HTTP column needs a URL in option 67, so a rule has
        // to be able to ask.
        assert!(ClientArch::from_code(16).is_http_boot());
        assert!(!ClientArch::from_code(7).is_http_boot());
        assert!(ClientArch::from_code(16).matches_name("http"));
        assert!(ClientArch::from_code(7).matches_name("tftp"));
    }

    #[test]
    fn an_architecture_nobody_has_registered_is_carried_rather_than_flattened() {
        let future = ClientArch::from_code(9000);
        assert!(!future.is_known());
        assert_eq!(future.label(), "arch-9000");
        assert!(future.matches_name("9000"), "a rule can still name it by number");
        assert!(!future.matches_name("bios"), "and it is not quietly treated as x86");
    }

    #[test]
    fn a_rule_can_match_by_family_or_by_label() {
        let arm = ClientArch::ARM64_UEFI;
        assert!(arm.matches_name("arm64-uefi"));
        assert!(arm.matches_name("arm64"));
        assert!(arm.matches_name("aarch64"));
        assert!(arm.matches_name("uefi"));
        assert!(!arm.matches_name("x64"));
    }

    #[test]
    fn names_a_human_types_resolve_both_ways() {
        assert_eq!(parse_arch("bios"), Some(ClientArch::BIOS));
        assert_eq!(parse_arch("X64-UEFI"), Some(ClientArch::from_code(7)));
        assert_eq!(parse_arch("11"), Some(ClientArch::ARM64_UEFI));
        assert_eq!(parse_arch("not-an-arch"), None);
    }

    #[test]
    fn every_registry_row_is_unique_by_code() {
        let mut codes: Vec<u16> = REGISTRY.iter().map(|entry| entry.code).collect();
        let before = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), before, "two rows claim the same architecture id");
    }
}
